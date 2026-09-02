#!/usr/bin/env bash
# Mechanical enforcement of PURITY.md.
#
# Greps are deliberately blunt: a false positive costs a comment explaining
# itself, a false negative costs the project its claim. PURITY.md's Enforcement
# table says which rules this script reaches and which are review-only. Keep the
# two in step; a gate whose documented reach is wider than its implementation is
# worse than no gate.
#
# Provenance functions are not scanned. `version()`, `hostName()` and `uptime()`
# appear throughout the benchmark harnesses, recording what a measurement ran
# against, and no emulator result depends on them. Scanning for them would cost
# eight annotations and catch nothing.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

scan() { # scan <rule> <dir> <description> <pattern...>
  local rule="$1"; shift
  local dir="$1"; shift
  local desc="$1"; shift
  [ -d "$dir" ] || return 0
  # `git ls-files`, not a raw filesystem walk. A plain `grep -R` has no concept
  # of .gitignore and walks into whatever happens to be on disk: a local
  # `.venv/` left by `uv sync`, `__pycache__/`, any generated-artifact
  # directory. Tracked files and untracked files git would not ignore are the
  # project's own code; a new file is scanned before it is ever staged, so
  # `make lint` reports it the same way CI will.
  #
  # This script is excluded from its own scan. It names every forbidden pattern
  # as a literal, so scanning it would report itself.
  local files
  files=$(git ls-files --cached --others --exclude-standard -- "$dir" | grep -E '\.(sql|sh|rs)$' | grep -v '^scripts/check_purity\.sh$') || true
  [ -n "$files" ] || return 0
  for pat in "$@"; do
    if printf '%s\n' "$files" | xargs grep -InE "$pat" -- | grep -v 'purity-ok:' ; then
      echo "::error::${rule}: forbidden pattern '$pat' (${desc}) found in ${dir}/. See ${rule} in PURITY.md. If a hit is genuinely benign, annotate the line with 'purity-ok: <reason>' saying why it is outside the rule."
      fail=1
    fi
  done
}

# PUR-9: no mechanism that delegates computation to a subprocess.
UDF_PATTERNS=('executable' 'python\(' 'CREATE FUNCTION.*AS.*script')
for d in sqlcpu executor driver native scripts; do
  scan PUR-9 "$d" "executable UDF / subprocess delegation" "${UDF_PATTERNS[@]}"
done

# PUR-12: no wall-clock or host-environment dependence on a computation path.
# now64/rand32/rand64/randCanonical/generateUUIDv4 are live ClickHouse functions
# with the same consequence as the ones the list started with. blockNumber and
# rowNumberInAllBlocks are here because a result that depends on block order is
# not reproducible either.
CLOCK_PATTERNS=(
  'now\(\)' 'now64\(' 'today\(\)' 'yesterday\(\)'
  '\brand\(' '\brandom\(' '\brand32\(' '\brand64\(' 'randCanonical\('
  'randomString\(' 'randomPrintableASCII\(' 'generateRandom' 'generateUUIDv4\('
  'blockNumber\(' 'rowNumberInAllBlocks\('
  'Instant::now' 'SystemTime::now' 'std::time::' '\brand::' 'thread_rng\(' 'getrandom'
)
for d in sqlcpu executor driver native scripts; do
  scan PUR-12 "$d" "wall clock / randomness / block order" "${CLOCK_PATTERNS[@]}"
done

if [ "$fail" -ne 0 ]; then exit 1; fi
echo "Purity check passed."
