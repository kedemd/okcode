#!/usr/bin/env bash
# JS vs Rust okcode on one repo, measured by CPU time (user+sys) and peak RSS: the figures that
# stay fair on a loaded machine. Wall clock is printed too but needs a quiet slot to mean anything.
#
#   rs/tools/bench-cpu.sh <repo> <scratch> <libokcode_node.so> [reps=3]
#
# Per side (js = src/ on @kedem/okdb, rs = OKCODE_NATIVE=1 with every port on) and per config
# (B = fts full, C = fts symbols): `reps` cold runs on a fresh copy + wiped store, each followed by
# a warm reopen, via bench/compare.js (the same driver bench/compare.sh uses). Prints the median.
set -euo pipefail
REPO=${1:?repo}; SCRATCH=${2:?scratch}; SO=${3:?libokcode_node.so}; REPS=${4:-3}
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
# the JS side runs the JS implementation: on the Rust-only tree (release plan C3+) point JS_TREE at a master checkout
JS_TREE=${JS_TREE:-$HERE}
export TMPDIR=${TMPDIR:-$HOME/.cache/okcode-rs/tmp} OKDB_LOG_LEVEL=error
mkdir -p "$SCRATCH" "$TMPDIR"

fresh_copy() {
    rm -rf "$1"; mkdir -p "$1"
    (cd "$REPO" && git ls-files -z) | rsync -a --files-from=- --from0 "$REPO/" "$1/"
}
# one timed run → "cpu_s rss_mb wall_s"
timed() {
    local tf="$SCRATCH/t.$$"
    /usr/bin/time -f '%U %S %M %e' -o "$tf" -- "$@" >/dev/null 2>>"$SCRATCH/runs.err"
    awk '{printf "%.2f %.0f %.2f\n", $1+$2, $3/1024, $4}' "$tf"
}
median() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }

tree_of() { if [ "$1" = rs ]; then echo "$HERE"; else echo "$JS_TREE"; fi; }
side_env() {
    if [ "$1" = rs ]; then
        echo "OKCODE_NATIVE_BINARY=$SO"
    fi
}

echo "| side | config | cold CPU s | cold peak RSS MB | cold wall s | warm CPU s | warm peak RSS MB | warm wall s |"
echo "|---|---|---|---|---|---|---|---|"
for cfg in B C; do
    fts=$([ $cfg = B ] && echo full || echo symbols)
    for side in js rs; do
        : > "$SCRATCH/cold.$side.$cfg"; : > "$SCRATCH/warm.$side.$cfg"
        for i in $(seq 1 "$REPS"); do
            d="$SCRATCH/$side-$cfg"; fresh_copy "$d/repo"; rm -rf "$d/store"
            timed env $(side_env $side) node "$(tree_of $side)/bench/compare.js" cold --repo "$d/repo" --store "$d/store" --fts $fts >> "$SCRATCH/cold.$side.$cfg"
            timed env $(side_env $side) node "$(tree_of $side)/bench/compare.js" warm --repo "$d/repo" --store "$d/store" --fts $fts >> "$SCRATCH/warm.$side.$cfg"
        done
        m() { cut -d' ' -f"$2" "$SCRATCH/$1.$side.$cfg" | median; }
        echo "| $side | $cfg ($fts) | $(m cold 1) | $(m cold 2) | $(m cold 3) | $(m warm 1) | $(m warm 2) | $(m warm 3) |"
    done
done
