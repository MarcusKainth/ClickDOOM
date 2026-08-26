#!/usr/bin/env bash
# #180's K-sweep, at a FIXED instruction window rather than a fixed batch
# count.
#
# ## Why the window is held fixed and the batch count varies
#
# The naive sweep -- "one batch at K = 30,000 / 60,000 / 120,000, fit
# T(K) = S + c*K" -- has a confound that would have decided the issue on
# its own: batches are chained, so a K = 120,000 batch executes a
# completely different 120,000 instructions than a K = 30,000 batch's
# first 30,000. The boot window is not homogeneous (WAD load, R_Init*,
# strncasecmp-heavy lump lookups), so per-instruction cost is not constant
# across it, and any fitted slope would be partly instruction mix.
#
# Instead every arm executes THE SAME instruction window (default 120,000
# instructions from the reset state), differing only in how many batches it
# is cut into:
#
#   K =  15,000  ->  8 batches  ->  T = 8S + W
#   K =  30,000  ->  4 batches  ->  T = 4S + W
#   K =  60,000  ->  2 batches  ->  T = 2S + W
#   K = 120,000  ->  1 batch    ->  T =  S + W
#
# where S is the per-batch fixed setup and W = c * 120,000 is the per-step
# work, IDENTICAL across arms by construction. Successive differences give
# three independent estimates of S -- (T8-T4)/4, (T4-T2)/2, (T2-T1) -- which
# is where the error bar comes from, rather than from a single noisy pair.
#
# A K = 1 arm is also run: its fold time is S plus one step, i.e. a direct
# reading of the intercept that does not depend on the fit at all. Two
# independent methods agreeing is the point.
#
# ## Known bias, stated rather than corrected
#
# `min_count_to_compile_expression = 3`, so an 8-batch arm reaches the
# compiled regime (4th execution onward) and a 1-batch arm never does. That
# makes small-K arms relatively FASTER, which shrinks the fitted S -- i.e.
# it biases against #180's hypothesis, so a confirmed S here is a lower
# bound. `CompileFunction` is recorded per batch in each arm's JSON so the
# regime is visible next to the number (#166).
#
# Usage: ksweep.sh [--window 120000] [--outdir DIR]
set -euo pipefail
cd "$(dirname "$0")/../../.."   # repo root

WINDOW=120000
OUTDIR="${SQ2_OUTDIR:-/tmp/sq2-bench}"
while [ $# -gt 0 ]; do
  case "$1" in
    --window) WINDOW="$2"; shift 2 ;;
    --outdir) OUTDIR="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

HERE="executor/bench/commit_mutation"

# Direct intercept reading: K = 1 is almost pure setup.
"$HERE/arm.sh" --label "K_sweep_k1" --outdir "$OUTDIR" -- --k 1 --batches 5

for K in 15000 30000 60000 120000; do
  BATCHES=$(( WINDOW / K ))
  [ "$BATCHES" -ge 1 ] || { echo "::error::window $WINDOW < K $K" >&2; exit 1; }
  "$HERE/arm.sh" --label "K_sweep_k${K}" --outdir "$OUTDIR" -- --k "$K" --batches "$BATCHES"
done

echo "# ksweep done; fit with: python3 $HERE/fit.py $OUTDIR/K_sweep_k*.json" >&2
