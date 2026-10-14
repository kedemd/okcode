#!/usr/bin/env bash
# Benchmark harness: for ONE source repo, run every configuration FROM ZERO
# (a fresh copy, a wiped store) as its OWN process, and print one markdown
# table. See bench/README.md for what each column means and why each
# configuration is its own process.
#
# usage: bench/compare.sh <repoDir> <scratchDir> [--configs A,B,C,D,E]
#
# env:
#   ADCODE        path to the adcode binary (config A). Unset = skip A.
#   OLLAMA_URL    default http://localhost:11434
#   OLLAMA_MODEL  default qwen3-embedding:latest
set -euo pipefail

REPO="${1:?usage: compare.sh <repoDir> <scratchDir> [--configs A,B,C,D,E]}"
SCRATCH="${2:?usage: compare.sh <repoDir> <scratchDir> [--configs A,B,C,D,E]}"
shift 2 || true
CONFIGS="ABCDE"
while [ $# -gt 0 ]; do
    case "$1" in
        --configs) CONFIGS="$2"; shift 2 ;;
        *) echo "unknown arg $1" >&2; exit 1 ;;
    esac
done

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPARE_JS="$HERE/compare.js"
OLLAMA_URL="${OLLAMA_URL:-http://localhost:11434}"
OLLAMA_MODEL="${OLLAMA_MODEL:-qwen3-embedding:latest}"
ADCODE="${ADCODE:-}"
REPO_NAME="$(basename "$REPO")"

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"

# A fresh copy of the repo's TRACKED files (git ls-files) — never the
# original, never node_modules/.git, and a new copy per configuration so one
# config's cache/store never leaks into another's "cold" measurement. Falls
# back to a plain recursive copy (minus .git) for a source tree that is not
# itself a git repo (adcode's own source, used as a sample codebase here).
fresh_copy() {
    local dst="$1"
    rm -rf "$dst"
    mkdir -p "$dst"
    if git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1; then
        (cd "$REPO" && git ls-files -z) | rsync -a --files-from=- --from0 "$REPO/" "$dst/"
    else
        rsync -a --exclude='.git' "$REPO/" "$dst/"
    fi
}

# /usr/bin/time's own -f output on fd 2 by convention here: callers redirect
# time's stderr to a side file and the wrapped command's own stdout/stderr
# pass through untouched (compare.js's one JSON line is on stdout).
run_timed() {
    local timefile="$1"; shift
    /usr/bin/time -f '%M %e' -o "$timefile" -- "$@"
}

# du, actual disk blocks (not apparent size — what the disk pays for),
# 0 for a path that does not exist (a mode with no vectors, no content FTS).
du_of() {
    if [ -e "$1" ]; then du -sk "$1" 2>/dev/null | awk '{print $1}'; else echo 0; fi
}

# KB -> "N.N MB", one decimal — plain integer MB truncated anything under a
# megabyte to a misleading "0 MB" for every repo small enough to smoke-test.
mb() { awk -v kb="$1" 'BEGIN { printf "%.1f", kb/1024 }'; }

declare -A ROW # config -> markdown row (built incrementally, printed at the end)

bench_okcode() {
    local cfg="$1" fts="$2" embed="$3"
    local copy="$SCRATCH/$cfg/repo"
    local store="$SCRATCH/$cfg/store"
    fresh_copy "$copy"
    rm -rf "$SCRATCH/$cfg/store-dir" && mkdir -p "$SCRATCH/$cfg"

    local coldArgs=(cold --repo "$copy" --store "$store" --fts "$fts")
    [ -n "$embed" ] && coldArgs+=(--embedder "$embed")
    local coldTime="$SCRATCH/$cfg/cold.time"
    local coldOut="$SCRATCH/$cfg/cold.json"
    run_timed "$coldTime" node "$COMPARE_JS" "${coldArgs[@]}" >"$coldOut" 2>"$SCRATCH/$cfg/cold.log"
    local coldRssKb coldWall
    read -r coldRssKb coldWall < "$coldTime"
    local indexMs embedMs
    indexMs=$(node -e "console.log(JSON.parse(require('fs').readFileSync('$coldOut')).indexMs)")
    embedMs=$(node -e "const d=JSON.parse(require('fs').readFileSync('$coldOut')); console.log(d.embedMs==null?'-':d.embedMs)")

    local warmTime="$SCRATCH/$cfg/warm.time"
    local warmOut="$SCRATCH/$cfg/warm.json"
    run_timed "$warmTime" node "$COMPARE_JS" warm --repo "$copy" --store "$store" >"$warmOut" 2>"$SCRATCH/$cfg/warm.log"
    local warmRssKb warmWall
    read -r warmRssKb warmWall < "$warmTime"

    local ftsDisk vecDisk wsDisk
    ftsDisk=$(du_of "$store/~fts")
    wsDisk=$(find "$store" -maxdepth 1 -type d -name 'okcode_*' -print0 2>/dev/null | xargs -0 -r du -sk 2>/dev/null | awk '{s+=$1} END{print s+0}')
    # Vector/pipeline sub-envs are named "<env>:<pipeline>" on disk — anything
    # under the store root with a ':' in its name that isn't the content-FTS
    # dir (already counted above) or the okcode management env.
    vecDisk=$(find "$store" -maxdepth 1 -type d -name '*:*' -print0 2>/dev/null | xargs -0 -r du -sk 2>/dev/null | awk '{s+=$1} END{print s+0}')
    local totalDisk; totalDisk=$(du_of "$store")
    local embedNote=""
    if [ "$embedMs" != "-" ]; then embedNote=", embed ${embedMs}ms"; fi

    ROW[$cfg]="| $cfg | $(mb "$coldRssKb") MB | ${coldWall}s (index ${indexMs}ms${embedNote}) | $(mb "$warmRssKb") MB | ${warmWall}s | $(mb "$totalDisk") MB (fts $(mb "$ftsDisk")M, ws $(mb "$wsDisk")M, vec $(mb "$vecDisk")M) |"
}

bench_adcode() {
    local cfg="A"
    if [ -z "$ADCODE" ]; then
        ROW[$cfg]="| A (adcode) | skipped (\$ADCODE not set) | | | | |"
        return
    fi
    local copy="$SCRATCH/$cfg/repo"
    fresh_copy "$copy"
    local coldTime="$SCRATCH/$cfg/cold.time"
    run_timed "$coldTime" bash -c "cd '$copy' && '$ADCODE' index ." >"$SCRATCH/$cfg/cold.log" 2>&1
    local coldRssKb coldWall
    read -r coldRssKb coldWall < "$coldTime"

    local warmTime="$SCRATCH/$cfg/warm.time"
    run_timed "$warmTime" bash -c "cd '$copy' && '$ADCODE' index ." >"$SCRATCH/$cfg/warm.log" 2>&1
    local warmRssKb warmWall
    read -r warmRssKb warmWall < "$warmTime"

    local diskKb; diskKb=$(du_of "$copy/.adcode")
    ROW[$cfg]="| A (adcode) | $(mb "$coldRssKb") MB | ${coldWall}s | $(mb "$warmRssKb") MB | ${warmWall}s | $(mb "$diskKb") MB (.adcode) |"
}

[[ "$CONFIGS" == *A* ]] && bench_adcode
[[ "$CONFIGS" == *B* ]] && bench_okcode B full ""
[[ "$CONFIGS" == *C* ]] && bench_okcode C symbols ""
[[ "$CONFIGS" == *D* ]] && bench_okcode D full "ollama:$OLLAMA_MODEL@$OLLAMA_URL"
[[ "$CONFIGS" == *E* ]] && bench_okcode E symbols "ollama:$OLLAMA_MODEL@$OLLAMA_URL"

echo
echo "### $REPO_NAME"
echo
echo "| config | cold peak RSS | cold wall (index/embed) | warm peak RSS | warm wall | disk |"
echo "|---|---|---|---|---|---|"
for c in A B C D E; do
    if [[ "$CONFIGS" == *"$c"* ]] && [ -n "${ROW[$c]:-}" ]; then echo "${ROW[$c]}"; fi
done
exit 0
