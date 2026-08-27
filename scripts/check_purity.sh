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
  # #169: `git ls-files`, not a raw filesystem walk. A plain `grep -R`
  # has no concept of .gitignore and happily walks into whatever happens
  # to be sitting on disk -- a local `.venv/` left over from `uv sync`,
  # `__pycache__/`, `node_modules/`, any future generated-artifact
  # directory -- none of which are gitignored from grep's perspective,
  # only from git's. Scoping to tracked files makes this check about the
  # PROJECT's own code, not whatever a developer's last dependency
  # install happened to leave lying around: a check that fails on
  # untracked vendor code isn't evidence of a real defect, same shape as
  # Non-negotiable #5's "a check that never ran isn't evidence of a
  # pass" but facing the other direction (see #169 -- this exact false
  # positive hit two different agents' local `check_purity.sh` runs the
  # same day).
  local files
  files=$(git ls-files -- "$dir" | grep -E '\.(sql|py|sh)$') || true
  [ -n "$files" ] || return 0
  for pat in "$@"; do
    if printf '%s\n' "$files" | xargs grep -InE "$pat" -- \
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
