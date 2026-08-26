#!/usr/bin/env bash
# Decode correctness test — sqlcpu workstream, issue #18.
#
# sqlcpu/fixtures/decode_vectors.tsv is 52 hand-encoded RV32IM instructions
# (every dispatch id 0..27, both sentinel ids 254/255, FENCE-as-no-op, a
# reserved-funct3 case, and two deliberately-misaligned branch/jal targets),
# one per line:
#   word_addr  word  id  rd  rs1  rs2  imm  tgt  mk  sg  m_sg1  m_sg2  m_hi  d_sg  cmp_sel  neg  tgt_mis  note
# The first two columns are the raw ROM content to decode; the next fifteen
# are the hand-verified expected sqlcpu.decoded row (m_sg1/m_sg2/m_hi/d_sg
# added for issue #54's M-extension collapse -- non-zero only on the eight
# mul/mulh/mulhsu/mulhu/div/divu/rem/remu rows; cmp_sel/neg/tgt_mis added
# for issue #128/E4's branch pre-decode -- non-zero only on the six branch
# rows (cmp_sel/neg) or wherever a target lands on an odd half-word
# (tgt_mis, including the two rows added specifically to exercise it: every
# pre-existing branch/jal row's target happened to be 4-aligned already, so
# without those two rows tgt_mis=1 was never actually reached by this
# fixture -- per schema.sql's column-doc comment); `note` is a human label,
# not loaded anywhere. This is a correctness check for sqlcpu/decode.sql,
# not a substitute for riscv-tests (#21) — it validates decode's dispatch
# and field extraction against known encodings without needing execute
# (#19) to exist.
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

CH_CMD=()
if command -v clickhouse-client >/dev/null 2>&1; then
  CH_CMD=(clickhouse-client)
elif command -v clickhouse >/dev/null 2>&1; then
  CH_CMD=(clickhouse client)
else
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

FIXTURE="sqlcpu/fixtures/decode_vectors.tsv"
FIRST_WORD=$(head -1 "$FIXTURE" | cut -f1)
LAST_WORD=$(tail -1 "$FIXTURE" | cut -f1)
END_WORD=$((LAST_WORD + 1))

echo "# loading $(wc -l < "$FIXTURE" | tr -d ' ') decode vectors into ram [${FIRST_WORD}, ${END_WORD})..." >&2
cut -f1,2 "$FIXTURE" | awk -F'\t' '{print $1"\t"$2"\t0"}' \
  | ch --query "INSERT INTO ram (word_addr, value, version) FORMAT TSV"

echo "# running sqlcpu/decode.sql..." >&2
ch --param_text_start_word="$FIRST_WORD" --param_text_end_word="$END_WORD" --multiquery < sqlcpu/decode.sql

actual=$(ch --query "SELECT word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg, m_sg1, m_sg2, m_hi, d_sg, cmp_sel, neg, tgt_mis FROM decoded ORDER BY word_addr FORMAT TSV")
expected=$(cut -f1,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17 "$FIXTURE")

if [ "$actual" = "$expected" ]; then
  echo "decode.sql: all $(wc -l < "$FIXTURE" | tr -d ' ') vectors match"
  exit 0
fi

echo "::error::sqlcpu/decode.sql produced output that doesn't match sqlcpu/fixtures/decode_vectors.tsv" >&2
diff <(echo "$expected") <(echo "$actual") >&2 || true
exit 1
