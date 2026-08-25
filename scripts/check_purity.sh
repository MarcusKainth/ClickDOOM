#!/usr/bin/env bash
# Mechanical enforcement of PURITY.md. Greps are deliberately blunt: a false
# positive costs a comment explaining itself; a false negative costs the
# project its claim. Extend the lists as new footguns are discovered.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0

scan() { # scan <dir> <description> <pattern...>
  local dir="$1"; shift
  local desc="$1"; shift
  [ -d "$dir" ] || return 0
  for pat in "$@"; do
    if grep -RIn --include='*.sql' --include='*.py' --include='*.sh' -E "$pat" "$dir" \
        | grep -v 'purity-ok:' ; then
      echo "::error::PURITY: forbidden pattern '$pat' (${desc}) found in ${dir}/ — see PURITY.md. If a hit is genuinely benign, annotate the line with 'purity-ok: <reason>'."
      fail=1
    fi
  done
}

# Computation must never leave SQL:
scan sqlcpu   "executable UDF / subprocess delegation" 'executable' 'python\(' 'CREATE FUNCTION.*AS.*script'
scan executor "executable UDF / subprocess delegation" 'executable' 'python\(' 'CREATE FUNCTION.*AS.*script'
# Determinism (SPEC §8) on computation paths:
scan sqlcpu   "wall clock / randomness" 'now\(\)' '\brand(om)?\(' 'generateRandom' 'today\(\)'
scan executor "wall clock / randomness" 'now\(\)' '\brand(om)?\(' 'generateRandom' 'today\(\)'
# The driver computes nothing:
scan driver   "computation smuggled into the driver" 'subprocess' 'numpy' 'struct\.unpack.*fb' 'PIL|Pillow'

if [ "$fail" -ne 0 ]; then exit 1; fi
echo "Purity check passed."
