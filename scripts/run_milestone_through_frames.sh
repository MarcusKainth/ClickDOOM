#!/usr/bin/env bash
# scripts/run_milestone.sh stops at the FIRST FRAME_COMMIT it encounters
# after each invocation, regardless of --target-icount -- by design (its
# own header: "the resumable batch-loop runner for #110's Phase 2
# milestone: DOOM reaches its first FRAME_COMMIT inside ClickHouse").
# #110's original target (icount 15,393,136) happened to BE the first
# frame's exact icount, so "reached target" and "hit a frame" were the
# same event and this was invisible. Past frame 0, reaching a later frame
# means re-invoking run_milestone.sh once per intervening FRAME_COMMIT.
#
# This is that re-invocation, and nothing else: a pure driver loop with no
# CPU/game/rendering logic of its own (PURITY.md). It re-runs
# run_milestone.sh unchanged with the exact arguments it was given, every
# time, and only inspects the `final_icount` provenance line
# run_milestone.sh already prints on every exit path -- it never re-derives
# icount, frame state, or anything else about the run.
#
# This is a stopgap, not the fix -- see #210. Phase 3's demo3 has ~2,172
# frames; one run_milestone.sh invocation per frame (each paying its own
# preflight-gate cost) is not a workable driver for a multi-day run. #210
# tracks giving run_milestone.sh a native continue-through-frames mode;
# this script is the proof of what that mode needs to do, built to reach
# frame 25 for #110's milestone blog post while #210 is open.
#
# Usage: identical to run_milestone.sh -- every argument is passed through
# unchanged, every invocation. --target-icount is required (same as
# run_milestone.sh) and is also this loop's own stop condition.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

TARGET_ICOUNT=""
args=("$@")
i=0
while [ "$i" -lt "${#args[@]}" ]; do
  if [ "${args[$i]}" = "--target-icount" ]; then
    TARGET_ICOUNT="${args[$((i + 1))]}"
  fi
  i=$((i + 1))
done
if [ -z "$TARGET_ICOUNT" ]; then
  echo "::error::--target-icount is required (same as run_milestone.sh)" >&2
  exit 1
fi

while true; do
  OUTPUT=$(./scripts/run_milestone.sh "$@" 2>&1)
  rc=$?
  printf '%s\n' "$OUTPUT"
  if [ "$rc" -ne 0 ]; then
    exit "$rc"
  fi
  # final_icount is run_milestone.sh's own provenance line (printed on
  # every exit path) -- read, not re-derived.
  FINAL_ICOUNT=$(printf '%s\n' "$OUTPUT" | awk -F'\t' '/^final_icount/ { v = $2 } END { print v }')
  if [ -n "$FINAL_ICOUNT" ] && [ "$FINAL_ICOUNT" -ge "$TARGET_ICOUNT" ]; then
    exit 0
  fi
done
