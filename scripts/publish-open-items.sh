#!/usr/bin/env bash
#
# Publish the funded research/build items onto a second mate's Space.
#
#   ./scripts/publish-open-items.sh <workspace_id> [source]
#
# Find the Space with `herdr workspace list`. `source` defaults to open-items
# and is the handle this publisher owns: re-running with the same source
# replaces its own tokens and touches nobody else's.
#
# Herdr stores agent-authored text but never writes it, so these are ordinary
# display-only metadata tokens published from outside. Each value is capped at
# 80 characters with control characters stripped
# (MAX_METADATA_TOKEN_VALUE_LEN), which is why multi-line text is an ordered
# token family -- `build`, `build_2`, `build_3` -- rather than a longer value.
#
# Undo:  herdr workspace report-metadata <workspace_id> --source open-items \
#          --clear-all-tokens

set -euo pipefail

workspace_id=${1:-}
source_id=${2:-open-items}

if [[ -z $workspace_id ]]; then
    echo "usage: $0 <workspace_id> [source]" >&2
    echo "hint:  herdr workspace list" >&2
    exit 2
fi

# Item 4 -- funded, not started. See OPEN-ITEMS.md.
build=(
    "build: smooth row motion, today it steps ~4x18px on a 9x18 cell"
    "needs sub-cell placement + finer frame tier + pixel trunk, together"
    "none of the three works alone; +1 cell transparent pad per card image"
    "refactor-risk: anim engine, graphics placement, char/pixel split"
    "measured in data/herdr-row-slide-reflow/subcell-test/RESULT.md"
)

# Item 5 -- funded, not started. See OPEN-ITEMS.md.
research=(
    "research: dissolve density, find the value between 1 and 21 per cell"
    "1/cell reads as corruption, 21/cell frays the cards; ends are ruled out"
    "more frames is unambiguous: 220ms is 4 frames per half and reads dead"
    "cost: ~16ms raster vs ~1.4ms encode, reuse SidebarCardLayer::undissolved"
    "harness: cargo test --release --bin herdr dissolve_capture -- --ignored"
)

args=()

# First member of a family is unsuffixed, the rest take _2, _3, ... in order.
add_family() {
    local name=$1
    shift
    local i=1
    local line
    for line in "$@"; do
        if ((${#line} > 80)); then
            echo "warn: '$name' line $i is ${#line} chars, will truncate at 80" >&2
        fi
        if ((i == 1)); then
            args+=(--token "$name=$line")
        else
            args+=(--token "${name}_${i}=$line")
        fi
        ((i++))
    done
}

add_family build "${build[@]}"
add_family research "${research[@]}"

herdr workspace report-metadata "$workspace_id" --source "$source_id" "${args[@]}"

echo "published ${#build[@]} build and ${#research[@]} research lines to $workspace_id"
