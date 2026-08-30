#!/usr/bin/env bash
#
# Architecture Decision Records: scaffold one, or check the set is consistent.
#
# Usage:
#   scripts/adr.sh --check
#   scripts/adr.sh --new <slug>
#
# An accepted ADR is immutable. Superseding one is a new record that says so,
# not an edit to the old one.
set -euo pipefail
cd "$(dirname "$0")/.."

dir=docs/adr
index=$dir/README.md
template=$dir/_template.md

fail() { echo "adr.sh: $1" >&2; exit 1; }

records() { find "$dir" -maxdepth 1 -name '[0-9][0-9][0-9][0-9]-*.md' -exec basename {} \; | sort; }

check() {
    local bad=0 n=0 expected=1
    [ -f "$index" ] || fail "$index not found"

    while read -r f; do
        [ -n "$f" ] || continue
        n=$((n + 1))
        local num="${f%%-*}"

        if [ "$((10#$num))" -ne "$expected" ]; then
            echo "::error::ADR numbering: expected $(printf '%04d' "$expected"), found $num ($f)"
            bad=1
        fi
        expected=$((expected + 1))

        if ! grep -q '^\*\*Status:\*\*' "$dir/$f"; then
            echo "::error::$f has no '**Status:**' line"
            bad=1
        fi

        if ! grep -qF "($f)" "$index"; then
            echo "::error::$f is not listed in $index"
            bad=1
        fi
    done <<<"$(records)"

    [ "$n" -gt 0 ] || fail "no ADRs found under $dir"

    # Every link in the index resolves to a record that exists.
    local linked
    linked=$(grep -oE '\(0[0-9]{3}-[a-z0-9-]+\.md\)' "$index" | tr -d '()' | sort -u) || true
    while read -r f; do
        [ -n "$f" ] || continue
        [ -f "$dir/$f" ] || { echo "::error::$index links $f, which does not exist"; bad=1; }
    done <<<"$linked"

    [ "$bad" -eq 0 ] || exit 1
    echo "adr.sh: $n record(s), numbering contiguous, all listed and all resolving"
}

new() {
    local slug="${1:-}"
    [ -n "$slug" ] || fail "usage: scripts/adr.sh --new <slug>"
    case "$slug" in
        *[!a-z0-9-]*) fail "slug must be lowercase letters, digits and hyphens: '$slug'" ;;
    esac
    [ -f "$template" ] || fail "$template not found"

    local last num path
    last=$(records | tail -1)
    num=$(printf '%04d' "$(( 10#${last%%-*} + 1 ))")
    path="$dir/$num-$slug.md"
    [ -e "$path" ] || : ; [ ! -e "$path" ] || fail "$path already exists"

    sed "s/ADR-NNNN/ADR-$num/" "$template" > "$path"
    echo "$path"
    echo "adr.sh: add it to $index before opening the pull request" >&2
}

case "${1:-}" in
    --check) check ;;
    --new)   shift; new "${1:-}" ;;
    *)       fail "usage: scripts/adr.sh --check | --new <slug>" ;;
esac
