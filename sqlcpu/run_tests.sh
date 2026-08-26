#!/usr/bin/env bash
# riscv-tests inside ClickHouse — sqlcpu workstream (SPEC §1, issue #21).
#
# CI's test-sqlcpu job (.github/workflows/ci.yml) invokes this script
# unconditionally the moment sqlcpu/schema.sql exists — the job's guard is on
# schema.sql, not on this file, so this script has to exist and exit 0 the
# same PR schema.sql lands, or every subsequent PR (any workstream) sees a
# broken job. That is why it lands with schema.sql rather than after it.
#
# What this script does today: connect, apply schema.sql, and confirm the
# tables it declares actually create and accept round-trip reads/writes. It
# does not run the real riscv-tests corpus yet — that needs decode (#18) and
# execute (#19, #20) landed first, so it deliberately reports zero
# instructions executed rather than a pass count it can't back up. Issue #21
# replaces run_riscv_tests() below with the real harness once those land;
# everything above that function (arg parsing, client discovery, schema
# apply) is meant to survive that change unmodified.
set -euo pipefail
cd "$(dirname "$0")/.."

HOST="localhost"
PORT="9000"
CH_USER="${CLICKHOUSE_USER:-default}"
PASSWORD="${CLICKHOUSE_PASSWORD:-}"
DATABASE="${CLICKHOUSE_DATABASE:-clickdoom}"

while [ $# -gt 0 ]; do
  case "$1" in
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) CH_USER="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    --database) DATABASE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

# --- locate a native-protocol client, installing one if the runner has none.
# CI's ubuntu-latest runner ships neither `clickhouse-client` nor `clickhouse`;
# a plain apt/brew package works locally but isn't guaranteed on the runner,
# so fall back to fetching the official single-file client binary into a
# scratch dir. Two steps (download, then run) rather than piping straight
# into a shell, so the fetch is inspectable before anything executes.
CH_CMD=()
if command -v clickhouse-client >/dev/null 2>&1; then
  CH_CMD=(clickhouse-client)
elif command -v clickhouse >/dev/null 2>&1; then
  CH_CMD=(clickhouse client)
else
  echo "# no local clickhouse client found; fetching the standalone binary..." >&2
  fetch_dir="$(mktemp -d)"
  curl -fsSL https://clickhouse.com/ -o "$fetch_dir/install.sh"
  (cd "$fetch_dir" && sh install.sh)
  CH_CMD=("$fetch_dir/clickhouse" client)
fi

ch() {
  local args=(--host "$HOST" --port "$PORT" --user "$CH_USER" --database "$DATABASE")
  [ -n "$PASSWORD" ] && args+=(--password "$PASSWORD")
  "${CH_CMD[@]}" "${args[@]}" "$@"
}

echo "# applying sqlcpu/schema.sql to ${HOST}:${PORT}/${DATABASE}..." >&2
ch --multiquery < sqlcpu/schema.sql

# Smoke-check every SPEC §5 / ADR-0002 table: each must exist with the
# expected engine and accept a round-trip row. This is real coverage of the
# schema itself (catches DDL typos, engine/ORDER BY mistakes, column drift
# from SPEC) even before any instruction ever executes.
check_table() {
  local table="$1" expect_engine="$2"
  local engine
  engine=$(ch --query "SELECT engine FROM system.tables WHERE database='${DATABASE}' AND name='${table}'")
  if [ "$engine" != "$expect_engine" ]; then
    echo "::error::sqlcpu/schema.sql: ${table} has engine '${engine}', expected '${expect_engine}'" >&2
    exit 1
  fi
  echo "  ${table}: ${engine} OK" >&2
}

echo "# checking table shapes..." >&2
check_table cpu_state    MergeTree
check_table ram          ReplacingMergeTree
check_table input_queue  MergeTree
check_table frames_out   MergeTree
check_table console_out  MergeTree
check_table decoded      MergeTree

# Round-trip: every table accepts a row and reads it back with spec_version
# defaulted, per SPEC §5 ("All tables carry spec_version String").
ch --query "INSERT INTO cpu_state (batch_id, icount, pc, regs, halted, halt_reason, exit_code) VALUES (0, 0, 2147483648, [], 0, '', 0)"
ch --query "INSERT INTO ram (word_addr, value, version) VALUES (0, 0, 0)"
ch --query "INSERT INTO input_queue (event_seq, key_event, consumed) VALUES (0, 0, 0)"
ch --query "INSERT INTO frames_out (frame_no, committed_icount, fb, palette) VALUES (0, 0, '', '')"
ch --query "INSERT INTO console_out (seq, byte) VALUES (0, 0)"
ch --query "INSERT INTO decoded (word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg) VALUES (0, 0, 0, 0, 0, 0, 0, 0, 0)"
for table in cpu_state ram input_queue frames_out console_out decoded; do
  version=$(ch --query "SELECT spec_version FROM ${table} LIMIT 1")
  if [ "$version" != "0.1.0" ]; then
    echo "::error::${table}.spec_version defaulted to '${version}', expected '0.1.0'" >&2
    exit 1
  fi
done
echo "# schema round-trip OK, spec_version defaults correctly on every table" >&2

decode_status="decode vectors: not landed yet (see #18)"
if [ -x sqlcpu/test_decode.sh ]; then
  echo "# running decode correctness vectors (issue #18)..." >&2
  vector_count=$(wc -l < sqlcpu/fixtures/decode_vectors.tsv | tr -d ' ')
  ./sqlcpu/test_decode.sh --host "$HOST" --port "$PORT" --user "$CH_USER" --password "$PASSWORD" --database "$DATABASE"
  decode_status="decode vectors: ${vector_count}/${vector_count} passed"
fi

execute_status="execute checks: not landed yet (see #19)"
if [ -f sqlcpu/test_execute.py ]; then
  echo "# running execute correctness checks (issue #19)..." >&2
  execute_out=$(python3 sqlcpu/test_execute.py --host "$HOST" --port "$PORT" --user "$CH_USER" \
    --password "$PASSWORD" --client "${CH_CMD[*]}")
  echo "$execute_out" >&2
  execute_status="$execute_out"
fi

echo "# running riscv-tests (issue #21)..." >&2
riscv_status=0
riscv_out=$(python3 sqlcpu/run_riscv_tests.py --host "$HOST" --port "$PORT" --user "$CH_USER" \
  --password "$PASSWORD" --database "$DATABASE" --client "${CH_CMD[*]}" 2>&1) || riscv_status=$?
echo "$riscv_out" >&2
# Pulled by pattern, not position: stdout/stderr interleaving under `2>&1`
# isn't guaranteed to preserve program order, and the script's own
# "::error::failed: ..." line (stderr) prints after its summary (stdout).
riscv_summary=$(echo "$riscv_out" | grep "^riscv-tests inside ClickHouse:")

checkpoint_status="checkpoint: not landed yet (see #22)"
if [ -f sqlcpu/test_checkpoint.py ]; then
  echo "# running checkpoint format checks (issue #22)..." >&2
  checkpoint_out=$(python3 sqlcpu/test_checkpoint.py --host "$HOST" --port "$PORT" --user "$CH_USER" \
    --password "$PASSWORD" --client "${CH_CMD[*]}")
  echo "$checkpoint_out" >&2
  checkpoint_status="$checkpoint_out"
fi

echo "${riscv_summary}. ${decode_status}. ${execute_status}. ${checkpoint_status}"
if [ "$riscv_status" -ne 0 ]; then
  exit 1
fi
