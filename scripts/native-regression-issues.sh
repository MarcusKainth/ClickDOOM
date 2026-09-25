#!/usr/bin/env bash
#
# Renders each regression `clickdoom native regress --findings` wrote as an
# issue body, in the shape GitHub gives an issue filed through the form.
#
#   scripts/native-regression-issues.sh FINDINGS LINES OUT
#
# FINDINGS is the --findings file. LINES holds every recorded line either
# side of a finding names, history and the night together. For finding N,
# OUT/N/ gets `title`, `labels` (one per line) and `body.md`. A correctness
# finding follows .github/ISSUE_TEMPLATE/1b-native-divergence.yml and a cost
# finding .github/ISSUE_TEMPLATE/3-performance.yml, with each field's label
# as a `###` heading.
#
# A title ends with the metric and the commit in parentheses, which is what
# the filing job searches for before filing, so a regression is filed once.
# A finding whose commit starts with `self-test-` was injected on purpose,
# and its title and body say so.
#
# Environment:
#   RUN_URL      the workflow run, linked from the body
#   SEARCH_TICS  the first diff's span, for the reproduction
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$#" -ne 3 ]; then
    echo "usage: $0 FINDINGS LINES OUT" >&2
    exit 2
fi
findings=$1
lines=$2
out=$3
run_url=${RUN_URL:-}
search_tics=${SEARCH_TICS:-2000}
wad_sha=$(cut -d' ' -f1 rom/wad/doom1.wad.sha256sum)

# The recorded line for COMMIT, the last one if it was measured twice.
line_of() {
    jq -c --arg commit "$1" 'select(.commit == $commit)' "$lines" | tail -n 1
}

# The form's "What disagrees" option for a field named as `kind slot n field`.
what_disagrees() {
    case "$1" in
        mobj\ *) echo "A mobj field" ;;
        sector*|line_side\ *|button\ *|anim\ *) echo "A sector or sector thinker field" ;;
        player\ *|psprite\ *|input\ *) echo "A player field" ;;
        hud\ *) echo "Status bar or message state" ;;
        *rndindex) echo "prndindex or rndindex" ;;
        *) echo "$1" ;;
    esac
}

field() {
    printf '### %s\n\n%s\n\n' "$1" "${2:-_No response_}"
}

fenced() {
    local fence='```'
    printf '### %s\n\n%s%s\n%s\n%s\n\n' "$1" "$fence" "$2" "$3" "$fence"
}

mkdir -p "$out"
n=0
while IFS= read -r finding; do
    n=$((n + 1))
    dir="$out/$n"
    mkdir -p "$dir"
    kind=$(jq -r .kind <<<"$finding")
    commit=$(jq -r .commit <<<"$finding")
    against=$(jq -r .against <<<"$finding")
    metric=$(jq -r .metric <<<"$finding")
    before=$(jq -r .before <<<"$finding")
    after=$(jq -r .after <<<"$finding")
    line=$(line_of "$commit")
    parent_line=$(line_of "$against")
    marker="($metric at ${commit:0:12})"
    note=""
    if [[ $commit == self-test-* ]]; then
        marker="($metric at $commit)"
        note="This finding was injected by the nightly's \`inject_regression\` input to show that filing works. Nothing regressed. Close it."
    fi
    rom_sha=$(git show "$commit:rom/PINNED_HASH" 2>/dev/null || cat rom/PINNED_HASH)
    clickhouse=$(jq -r '.clickhouse // "unknown"' <<<"$line")

    {
        if [ -n "$note" ]; then
            printf '%s\n\n' "$note"
        fi
        printf 'Filed by the nightly native regression walk%s.\n\n' "${run_url:+ in $run_url}"
    } >"$dir/body.md"

    if [ "$kind" = correctness ]; then
        case "$metric" in
            first_divergent_tic)
                tic=${after%% *}
                what=$(what_disagrees "$(jq -r '.first_divergent_field // ""' <<<"$line")")
                ;;
            *)
                tic=$(jq -r .first_refused_tic <<<"$line")
                what="The first refused tic, which is not a field"
                ;;
        esac
        echo "native: divergence at tic $tic $marker" >"$dir/title"
        printf 'divergence\narea: native\n' >"$dir/labels"
        refused=$(jq -r .first_refused_tic <<<"$line")
        repro="git checkout $commit
make up build-clickdoom gen-probe-trace
trace=refemu/reference_traces/demo3/probe.\$(cut -c1-12 rom/PINNED_HASH).tsv
./target/release/clickdoom native load --fresh
./target/release/clickdoom native diff $search_tics --probe \"\$trace\""
        if [ "$refused" != null ] && [ "$refused" -gt 1 ]; then
            repro="$repro
./target/release/clickdoom native diff $((refused - 1)) --probe \"\$trace\""
        fi
        {
            field "ROM sha256" "$rom_sha"
            field "WAD sha256" "$wad_sha"
            field "ClickHouse version" "$clickhouse"
            field "First divergent tic (gametic)" "$tic"
            field "First divergent frame, if a frame differs" ""
            field "What disagrees" "$what"
            fenced "The divergence report" text "$metric moved from $before at $against to $after at $commit.

This commit's line:
$line

The line it was judged against:
$parent_line"
            field "Random draws at that tic" ""
            fenced "Reproduction" shell "$repro"
        } >>"$dir/body.md"
    else
        ratio=${after##*(}
        echo "perf: $metric up ${ratio%)} on its parent $marker" >"$dir/title"
        printf 'performance\narea: native\n' >"$dir/labels"
        {
            field "What kind" "A change made it slower"
            field "Which benchmark" "The nightly native regression walk: \`scripts/native-regression-walk.sh\`, then \`clickdoom native regress\`"
            fenced "The numbers" text "$metric: $before at $against, $after at $commit.
Both measured in the same job on the same runner, ClickHouse $clickhouse.

This commit's line:
$line

Its parent's line:
$parent_line"
            field "The machine, and how quiet it was" "$(jq -r '.runner_cpu // "unknown"' <<<"$line"), a GitHub-hosted runner. Parent and child ran one after the other in the same job, with nothing else in it."
            field "What you think is happening" ""
        } >>"$dir/body.md"
    fi
done <"$findings"
echo "$n issue(s) rendered into $out"
