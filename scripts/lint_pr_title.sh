#!/usr/bin/env bash
# Lint a PR title against the commit convention: `scope: imperative summary`.
# Usage: lint_pr_title.sh "<title>"
set -euo pipefail

TITLE="${1:?usage: lint_pr_title.sh \"<pr title>\"}"
SCOPES="spec|rom|refemu|sqlcpu|executor|driver|render|test|bench|ci|docs"

if ! printf '%s' "$TITLE" | grep -Eq "^(${SCOPES})!?: .+$"; then
  echo "::error::PR title must match '^(${SCOPES})!?: <summary>' — got: '$TITLE'"
  exit 1
fi
if [ "${#TITLE}" -gt 72 ]; then
  echo "::error::PR title exceeds 72 chars (${#TITLE}): '$TITLE'"
  exit 1
fi
case "$TITLE" in
  *": "[a-z]*|*": "[A-Z]*) : ;;
  *) echo "::error::Summary after 'scope: ' looks empty"; exit 1 ;;
esac
echo "PR title OK: $TITLE"
