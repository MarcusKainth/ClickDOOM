#!/usr/bin/env bash
#
# Records native mode's parity and cost for each main commit not yet
# recorded, oldest first: one `clickdoom native diff --record` line per
# commit.
#
#   scripts/native-regression-walk.sh HISTORY OUT
#
# HISTORY holds the lines recorded so far and may be absent. The walk starts
# at the last commit it records, measured again so the commit after it has a
# parent measured on the same machine, and follows TIP's first parents from
# there. With no HISTORY, or when its last commit is not an ancestor of TIP,
# it measures TIP's first parent and TIP.
#
# Each commit is checked out in its own worktree, built, loaded into a fresh
# database and diffed against a probe trace generated from that commit's ROM
# and probe. The first diff runs SEARCH_TICS tics to find the first refused
# tic. When its line compared nothing, a second diff runs over the tics
# before the refusal, and the line kept is the second one carrying the
# first one's refusal. That line has to end below the refusal and compare at
# least one tic.
#
# A commit that does not build, load or run gets a line with `error` instead,
# and the walk goes on. OUT/night.jsonl gets one line per commit walked, and
# OUT/logs/ each commit's output.
#
# Environment:
#   TIP             the commit to walk up to, default origin/main
#   MAX_COMMITS    commits to walk after the parent, default 8
#   BUDGET_MINUTES  no commit starts once the walk has run this long, default 240
#   SEARCH_TICS     the first diff's span, default 2000
#   TRACES          where probe traces are kept by ROM and probe version,
#                   default OUT/traces
#   CLICKDOOM_DATABASE  the database each commit loads, default regress
#   CH_HOST, CH_HTTP_PORT and CLICKHOUSE_PASSWORD, as for make
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -ne 2 ]; then
    echo "usage: $0 HISTORY OUT" >&2
    exit 2
fi
history=$1
mkdir -p "$2"
out=$(cd "$2" && pwd)
max_commits=${MAX_COMMITS:-8}
budget_minutes=${BUDGET_MINUTES:-240}
search_tics=${SEARCH_TICS:-2000}
traces=${TRACES:-$out/traces}
tip=${TIP:-origin/main}
conn=(--host "${CH_HOST:-localhost}" --port "${CH_HTTP_PORT:-8123}"
    --database "${CLICKDOOM_DATABASE:-regress}")
export CARGO_TARGET_DIR="$PWD/target"

mkdir -p "$out/logs" "$traces"
night="$out/night.jsonl"
: >"$night"
work=$(mktemp -d)
trap 'rm -rf "$work"; git worktree prune' EXIT

last=""
if [ -s "$history" ]; then
    last=$(tail -n 1 "$history" | jq -r .commit)
fi
if [ -n "$last" ] && git merge-base --is-ancestor "$last" "$tip" 2>/dev/null; then
    parent=$last
    mapfile -t newer < <(git rev-list --first-parent --reverse "$last..$tip")
else
    parent=$(git rev-parse "$tip^")
    newer=("$(git rev-parse "$tip")")
fi
if [ "${#newer[@]}" -eq 0 ]; then
    echo "nothing on $tip after $last"
    exit 0
fi
if [ "${#newer[@]}" -gt "$max_commits" ]; then
    echo "${#newer[@]} commits after $parent; walking the oldest $max_commits"
    newer=("${newer[@]:0:$max_commits}")
fi

# The ROM this job built, which a commit pinning the same hash reuses.
built_rom=""
if [ -f rom/build/doom-rv32im.bin ]; then
    built_rom=$(sha256sum rom/build/doom-rv32im.bin | cut -d' ' -f1)
fi

# Records why the step that called it failed, for the error line.
fail() {
    printf '%s' "$1" >"$work/why"
    return 1
}

# One diff of SPAN tics in WT, writing its line to RECORD. Exit 3 is a
# refusal or a divergence, which the line records. Any other exit is a
# failure.
diff_span() {
    local wt=$1 bin=$2 trace=$3 span=$4 record=$5 status=0
    (cd "$wt" && "$bin/clickdoom" native diff "$span" --probe "$trace" \
        "${conn[@]}" --record "$record") || status=$?
    if [ "$status" -ne 0 ] && [ "$status" -ne 3 ]; then
        fail "native diff $span exited $status"
    elif [ ! -f "$record" ] || [ "$(wc -l <"$record")" -ne 1 ]; then
        fail "native diff $span wrote no line"
    fi
}

# Builds and measures SHA, and writes the line to keep to $work/SHA.line.
measure() {
    local sha=$1 wt="$work/$1" bin="$work/bin/$1"
    git worktree add --detach --quiet "$wt" "$sha" || fail "checkout failed" || return
    (cd "$wt" && cargo build --locked --release -p clickdoom-driver -p refemu) \
        || fail "cargo build failed" || return
    mkdir -p "$bin"
    cp "$CARGO_TARGET_DIR/release/clickdoom" "$CARGO_TARGET_DIR/release/refemu" "$bin/"

    if [ "$(cat "$wt/rom/PINNED_HASH")" = "$built_rom" ]; then
        mkdir -p "$wt/rom/build"
        cp rom/build/doom-rv32im.bin rom/build/doom-rv32im.elf rom/build/manifest.json "$wt/rom/build/"
    else
        make -C "$wt/rom" || fail "the ROM build failed" || return
    fi

    # A trace depends only on the ROM and on the probe that dumps it.
    local rom_blob probe_tree trace
    rom_blob=$(git rev-parse "$sha:rom/PINNED_HASH") || fail "no rom/PINNED_HASH" || return
    probe_tree=$(git rev-parse "$sha:refemu/probe") || fail "no refemu/probe" || return
    trace="$traces/$rom_blob-$probe_tree.tsv"
    if [ ! -f "$trace" ]; then
        make -C "$wt" gen-probe-trace REFEMU="$bin/refemu" \
            || fail "make gen-probe-trace failed" || return
        cp "$wt/refemu/reference_traces/demo3/probe.$(cut -c1-12 "$wt/rom/PINNED_HASH").tsv" "$trace" \
            || fail "make gen-probe-trace wrote no trace" || return
    fi

    (cd "$wt" && "$bin/clickdoom" native load --fresh "${conn[@]}") \
        || fail "native load failed" || return

    local search="$work/$sha.search.jsonl" span="$work/$sha.span.jsonl"
    local line="$work/$sha.line" refused
    diff_span "$wt" "$bin" "$trace" "$search_tics" "$search" || return
    if jq -e '.compared_through != null' "$search" >/dev/null; then
        cp "$search" "$line"
    else
        refused=$(jq -r '.first_refused_tic' "$search")
        if [ "$refused" -le 1 ]; then
            fail "tic $refused refused, so no tic can be compared"
            return
        fi
        diff_span "$wt" "$bin" "$trace" "$((refused - 1))" "$span" || return
        jq -e --argjson refused "$refused" \
            '.first_refused_tic == null and .compared_through < $refused and .compared_tics > 0' \
            "$span" >/dev/null \
            || fail "native diff $((refused - 1)) did not compare the tics before the refusal at $refused" \
            || return
        jq -c -s '.[1] + {first_refused_tic: .[0].first_refused_tic, first_refused_bits: .[0].first_refused_bits}' \
            "$search" "$span" >"$line"
    fi
    [ "$(jq -r .commit "$line")" = "$sha" ] \
        || fail "the line names commit $(jq -r .commit "$line"), not $sha"
}

for sha in "$parent" "${newer[@]}"; do
    if [ "$SECONDS" -ge "$((budget_minutes * 60))" ]; then
        echo "stopping after $budget_minutes minutes; the next run starts from the last line written"
        break
    fi
    short=${sha:0:12}
    echo "::group::$short"
    rm -f "$work/why"
    if measure "$sha" 2>&1 | tee "$out/logs/$short.log"; then
        cat "$work/$sha.line" >>"$night"
        echo "$short: $(cat "$work/$sha.line")"
    else
        why=$(cat "$work/why" 2>/dev/null || echo "failed; see logs/$short.log")
        echo "::error::$short: $why"
        jq -n -c --arg commit "$sha" --arg error "$why" --arg run_id "${GITHUB_RUN_ID:-}" \
            '{commit: $commit, error: $error, run_id: (if $run_id == "" then null else $run_id end)}' >>"$night"
    fi
    git worktree remove --force "$work/$sha" 2>/dev/null || true
    echo "::endgroup::"
done
echo "$(wc -l <"$night") line(s) in $night"
