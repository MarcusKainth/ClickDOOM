#!/usr/bin/env bash
#
# Applies `.github/labels.yml` to the repository: creates a label that is
# missing, updates the color and description of one that exists, and never
# deletes.
#
# Uses `gh` rather than a labeling action, so the same command works locally and
# in CI with no extra trust placed in a third-party action.
#
# Usage:
#   scripts/sync-labels.sh [--dry-run] [--repo OWNER/NAME]
#
# Environment:
#   GH_TOKEN  a token with `issues: write` on the repository
#   DRY_RUN   `true` is the same as passing --dry-run
set -euo pipefail

cd "$(dirname "$0")/.."

definitions=.github/labels.yml
dry_run=0
repo_args=()

[ "${DRY_RUN:-}" = "true" ] && dry_run=1

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) dry_run=1 ;;
        --repo)
            shift
            repo_args=(--repo "$1")
            ;;
        *)
            echo "sync-labels.sh: unknown argument '$1'" >&2
            exit 2
            ;;
    esac
    shift
done

fail() {
    echo "sync-labels.sh: $1" >&2
    exit 1
}

[ -f "$definitions" ] || fail "$definitions not found"
command -v gh >/dev/null || fail "gh is not installed"

# One TAB-separated name/color/description row per entry. The quoted, fixed
# key order is the file's own convention.
rows=$(awk '
    function value(line,   s) {
        s = line
        sub(/^[^:]*: "/, "", s)
        sub(/"[[:space:]]*$/, "", s)
        return s
    }
    function emit() {
        if (name != "") printf "%s\t%s\t%s\n", name, color, desc
    }
    /^- name: "/       { emit(); name = value($0); color = ""; desc = "" }
    /^  color: "/      { color = value($0) }
    /^  description: "/ { desc = value($0) }
    END                { emit() }
' "$definitions")

[ -n "$rows" ] || fail "no labels parsed from $definitions. Has the format changed?"

created=0
total=0
while IFS=$'\t' read -r name color desc; do
    [ -n "$name" ] || continue
    total=$((total + 1))
    [ -n "$color" ] || fail "'$name' has no color in $definitions"

    if [ "$dry_run" -eq 1 ]; then
        printf 'would sync  %-28s #%s  %s\n' "$name" "$color" "$desc"
        continue
    fi

    # `--force` updates color and description when the label exists, repairing
    # one created elsewhere with a default grey and no description.
    if gh label create "$name" \
        --color "$color" \
        --description "$desc" \
        --force \
        "${repo_args[@]+"${repo_args[@]}"}" >/dev/null; then
        created=$((created + 1))
    else
        fail "failed to sync '$name'"
    fi
done <<<"$rows"

if [ "$dry_run" -eq 1 ]; then
    echo "sync-labels.sh: $total label(s) would be synced (dry run, nothing changed)"
else
    echo "sync-labels.sh: $created of $total label(s) synced"
fi
