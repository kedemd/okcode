#!/usr/bin/env bash
# The ONE heavy step per round (waves 2-3), run by a single builder:
#   rs/tools/integrate.sh [--no-merge] [--tests-only] [--only key,key] [--env "K=V K=V"]
#
#   --only api,tools,perf          wave 3: merge and test only these slices (the default set)
#   --only api,tools,perf,regress  … plus the wave 2 tests no wave 3 slice owns (no merge)
#   --env "OKCODE_NATIVE_API=1 OKCODE_NATIVE_TOOLS=1"  extra env for EVERY test run (e.g. all ports on)
#   --gates auto|on|off            when a slice's SLICE_ENV (its preload gate flag) applies:
#                                  auto (default) = only once the integration tree holds that slice's
#                                  commits (not reverted) — a skeleton slice runs the JS fallback;
#                                  on = always; off = never (the pure JS-on-top baseline)
#
# 1. resets the integration worktree (~/.cache/okcode-rs/wt/integ, branch rust-port-integ) to
#    rust-port and merges every rust-port-<key> branch (rs/tools/slices.conf);
# 2. builds the binding (debug) through rs/tools/heavy.sh; on compile errors, attributes them to
#    slices by file path (the files each slice touched since its merge base, plus its owned files),
#    reverts those slices to the skeleton (rust-port) and rebuilds — until it compiles. Errors only
#    in shared files: reverts merged slices one at a time (newest first) until it compiles;
# 3. runs each slice's JS test files natively (3 files at a time, TMPDIR under ~/.cache), each with
#    its slice's SLICE_ENV (the preload gate flags: api → OKCODE_NATIVE_API=1, tools →
#    OKCODE_NATIVE_TOOLS=1) plus COMMON_ENV and --env. A file listed by two slices runs once per
#    slice (logs: <key>.<test>.tap);
# 4. prints a per-slice summary (merge, compile errors, pass/fail counts, first failures), build and
#    test wall time and loadavg. Full logs: ~/.cache/okcode-rs/logs/integ/ (<test>.tap, build-N.log).
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CACHE="${OKCODE_CACHE:-$HOME/.cache/okcode-rs}"
WT=${OKCODE_INTEG_WT:-$CACHE/wt/integ}
BASE=${OKCODE_INTEG_BASE:-master}
LOGS=${OKCODE_INTEG_LOGS:-$CACHE/logs/integ}
TARGET=${OKCODE_INTEG_TARGET:-$CACHE/target-integ}
# a second integration tree (OKCODE_INTEG_WT=… OKCODE_INTEG_BRANCH=…) runs beside a builder using the default one
IBR=${OKCODE_INTEG_BRANCH:-rust-port-integ}
JOBS=${JOBS:-3}
TEST_JOBS=${TEST_JOBS:-3}
TEST_TIMEOUT=${TEST_TIMEOUT:-600}
MAX_REBUILDS=${MAX_REBUILDS:-4}
# Our own sccache server (same on-disk cache): the shared default-port server belongs to another
# session and can be left with a deleted TMPDIR ("Failed to create temp dir").
SCCACHE_PORT=${SCCACHE_PORT:-4231}

MERGE=1
TESTS_ONLY=0
ONLY=""
EXTRA_ENV=""
GATES=auto
while [ $# -gt 0 ]; do
    case "$1" in
        --no-merge) MERGE=0 ;;
        --tests-only) TESTS_ONLY=1; MERGE=0 ;;
        --only) ONLY="$2"; shift ;;
        --env) EXTRA_ENV="$2"; shift ;;
        --gates) GATES="$2"; shift ;;
        *) echo "unknown arg $1" >&2; exit 2 ;;
    esac
    shift
done

source "$REPO/rs/tools/slices.conf"
ALL_SLICES=("${SLICES[@]}" "${C1_SLICES[@]}")
if [ -n "$ONLY" ]; then IFS=, read -ra SLICES <<< "$ONLY"; fi
for k in "${SLICES[@]}"; do
    [ -n "${SLICE_TESTS[$k]+x}" ] || { echo "unknown slice $k (slices.conf)" >&2; exit 2; }
done

mkdir -p "$LOGS" "$CACHE/tmp/integ" "$CACHE/wt"
find "$LOGS" -maxdepth 1 \( -name '*.tap' -o -name '*.err' -o -name 'build-*.log' \) -delete
G() { git -C "$WT" "$@"; }
now() { date +%s.%N; }
secs() { awk -v a="$1" -v b="$2" 'BEGIN { printf "%.1f", b - a }'; }

declare -A MERGED REVERTED TOUCHED CERRS GATED
SHARED_ERRS=""

# ── 1. worktree + merges ──────────────────────────────────────────────────
if [ ! -e "$WT/.git" ]; then
    git -C "$REPO" worktree add -f -B "$IBR" "$WT" "$BASE" >/dev/null 2>&1 || { echo "cannot create $WT"; exit 1; }
fi
[ -e "$WT/node_modules" ] || ln -s "$REPO/node_modules" "$WT/node_modules"
for k in "${ALL_SLICES[@]}"; do
    b=rust-port-$k
    git -C "$REPO" rev-parse -q --verify "$b" >/dev/null && TOUCHED[$k]=$(git -C "$REPO" diff --name-only "$BASE...$b" | tr '\n' ' ')
done
if [ $MERGE = 1 ]; then
    G merge --abort >/dev/null 2>&1
    G checkout -q -B "$IBR" "$BASE" && G reset -q --hard "$BASE" && G clean -qfd
    for k in "${SLICES[@]}"; do
        b=rust-port-$k
        if [ -z "${SLICE_FILES[$k]}" ]; then MERGED[$k]="tests only (no branch)"; continue; fi
        if ! git -C "$REPO" rev-parse -q --verify "$b" >/dev/null; then MERGED[$k]="no branch"; continue; fi
        n=$(git -C "$REPO" rev-list --count "$BASE..$b")
        if [ "$n" = 0 ]; then MERGED[$k]="nothing new"; continue; fi
        if G merge -q --no-edit -m "integrate $b" "$b" >/dev/null 2>"$LOGS/merge-$k.err"; then
            MERGED[$k]="merged $n commit(s)"
        else
            G merge --abort >/dev/null 2>&1
            MERGED[$k]="MERGE CONFLICT (skipped, see $LOGS/merge-$k.err)"
            TOUCHED[$k]=""
        fi
        out=""
        for f in ${TOUCHED[$k]:-}; do
            own=0
            for o in ${SLICE_FILES[$k]}; do
                d=${o%.rs}/
                if [ "$f" = "$o" ] || [[ "$f" == "$d"* ]]; then own=1; fi
            done
            [ $own = 1 ] || out="$out $f"
        done
        [ -z "$out" ] || MERGED[$k]="${MERGED[$k]}; WARNING edits outside its files:$out"
    done
fi

# ── 2. build (revert failing slices to the skeleton until it compiles) ──────
slice_of() { # repo-relative file -> slice key
    local f=$1 k t d
    for k in "${SLICES[@]}"; do
        for t in ${TOUCHED[$k]:-} ${SLICE_FILES[$k]}; do
            d=${t%.rs}/
            if [ "$f" = "$t" ] || [[ "$f" == "$d"* ]]; then echo "$k"; return; fi
        done
    done
}
revert_slice() {
    local k=$1 f
    for f in ${TOUCHED[$k]:-}; do
        if G cat-file -e "$BASE:$f" 2>/dev/null; then G checkout -q "$BASE" -- "$f"; else G rm -qf --ignore-unmatch "$f" >/dev/null; fi
    done
    G commit -qam "integrate: revert $k to the skeleton (compile errors)" >/dev/null 2>&1
    REVERTED[$k]=1
}

BUILD_START=$(now)
BUILD_OK=0
round=0
while [ $BUILD_OK = 0 ] && [ $round -le $MAX_REBUILDS ]; do
    round=$((round + 1))
    log=$LOGS/build-$round.log
    if [ $TESTS_ONLY = 1 ]; then BUILD_OK=1; break; fi
    ( cd "$WT/rs" && SCCACHE_SERVER_PORT=$SCCACHE_PORT RUSTC_WRAPPER="${RUSTC_WRAPPER:-}" CARGO_TARGET_DIR=$TARGET CARGO_BUILD_JOBS=$JOBS TMPDIR=$CACHE/tmp \
        "$REPO/rs/tools/heavy.sh" cargo build -p okcode-node --lib --message-format=short ) >"$log" 2>&1
    if [ $? = 0 ]; then BUILD_OK=1; break; fi
    mapfile -t errs < <(grep -E '^[^ ]+\.rs:[0-9]+:[0-9]+: error' "$log")
    if [ ${#errs[@]} = 0 ]; then echo "build failed without attributable errors: $log"; tail -20 "$log"; break; fi
    declare -A bad=()
    unattributed=""
    for e in "${errs[@]}"; do
        f=${e%%:*}
        f=${f#"$WT/"}
        [[ "$f" == rs/* ]] || f=rs/$f
        k=$(slice_of "$f")
        if [ -n "$k" ] && [ -z "${REVERTED[$k]:-}" ]; then
            bad[$k]=1
            CERRS[$k]="${CERRS[$k]:-}    $e"$'\n'
        else
            unattributed="$unattributed    $e"$'\n'
        fi
    done
    if [ ${#bad[@]} = 0 ]; then
        SHARED_ERRS="$unattributed"
        cand=""
        for k in "${SLICES[@]}"; do
            if [[ "${MERGED[$k]:-}" == merged* ]] && [ -z "${REVERTED[$k]:-}" ]; then cand="$k $cand"; fi
        done
        k=${cand%% *}
        if [ -z "$k" ]; then echo "errors in shared files and no slice left to revert: $log"; printf '%s' "$unattributed" | head -20; break; fi
        CERRS[$k]="${CERRS[$k]:-}    (reverted while bisecting errors in shared files)"$'\n'
        revert_slice "$k"
    else
        for k in "${!bad[@]}"; do revert_slice "$k"; done
    fi
    unset bad
done
BUILD_END=$(now)
SO=$TARGET/debug/libokcode_node.so

# ── 3. tests ──────────────────────────────────────────────────────────────
TEST_START=$(now)
if [ $BUILD_OK = 1 ] && [ -f "$SO" ]; then
    # a slice is live when the integration tree holds its commits and it was not reverted
    live() {
        local b=rust-port-$1
        [ -n "${SLICE_FILES[$1]}" ] && [ -z "${REVERTED[$1]:-}" ] || return 1
        git -C "$REPO" rev-parse -q --verify "$b" >/dev/null || return 1
        [ "$(git -C "$REPO" rev-list --count "$BASE..$b")" != 0 ] && G merge-base --is-ancestor "$b" HEAD
    }
    list=()
    for k in "${SLICES[@]}"; do
        genv=${SLICE_ENV[$k]:-}
        case $GATES in
            off) genv="" ;;
            auto) live "$k" || genv="" ;;
        esac
        GATED[$k]=$genv
        for t in ${SLICE_TESTS[$k]}; do list+=("$k|$t|${COMMON_ENV:-} $genv $EXTRA_ENV"); done
    done
    # As shipped (release plan C4): no preload. okcode's loader takes OKCODE_NATIVE_BINARY (a .node-named copy:
    # require() loads addons by extension); @kedem/okdb v3 comes from node_modules (OKDB_PACK: install okdb's local
    # tarball into the integration tree first) and finds its own addon.
    cp -f "$SO" "$TARGET/debug/okcode-node.node"
    export OKCODE_NATIVE_BINARY=$TARGET/debug/okcode-node.node TMPDIR=$CACHE/tmp/integ
    unset NODE_OPTIONS
    if [ -n "${OKDB_PACK:-}" ]; then
        # the repo's node_modules hard-linked (cheap; devDependencies such as @kedem/okjs), okdb v3 installed over it
        rm -rf "$WT/node_modules"; cp -al "$REPO/node_modules" "$WT/node_modules"
        (cd "$WT" && npm install --no-save --no-audit --no-fund --prefer-offline $OKDB_PACK >"$LOGS/npm-okdb.log" 2>&1) || echo "npm install of $OKDB_PACK failed (see $LOGS/npm-okdb.log)"
    fi
    printf '%s\n' "${list[@]}" | xargs -d '\n' -P "$TEST_JOBS" -I{} sh -c \
        'k=${1%%|*}; r=${1#*|}; f=${r%%|*}; e=${r#*|}; b=$k.$(basename "$f" .test.js); cd "$2" && timeout "$3" env $e node --test --test-force-exit --test-reporter=tap "$f" >"$4/$b.tap" 2>"$4/$b.err"; echo "exit $?" >>"$4/$b.tap"' \
        _ {} "$WT" "$TEST_TIMEOUT" "$LOGS"
fi
TEST_END=$(now)

# ── 4. summary ──────────────────────────────────────────────────────────────
tapnum() { grep -E "^# $2 [0-9]+" "$1" 2>/dev/null | tail -1 | awk '{print $3}'; }
echo "== integrate $(date -Is)  base $BASE @ $(git -C "$REPO" rev-parse --short "$BASE")  integ @ $(G rev-parse --short HEAD)"
echo "   gates: $GATES  env: ${COMMON_ENV:-}${EXTRA_ENV:+ $EXTRA_ENV}"
TP=0
TF=0
for k in "${SLICES[@]}"; do
    echo "-- $k: ${MERGED[$k]:-not merged (--no-merge)}${REVERTED[$k]:+; REVERTED to skeleton}${GATED[$k]:+  [env ${GATED[$k]}]}"
    if [ -n "${CERRS[$k]:-}" ]; then echo "   compile errors:"; printf '%s' "${CERRS[$k]}" | head -8; fi
    for t in ${SLICE_TESTS[$k]}; do
        b=$(basename "$t" .test.js)
        tap=$LOGS/$k.$b.tap
        if [ ! -f "$tap" ]; then echo "   $b: not run"; continue; fi
        p=$(tapnum "$tap" pass); f=$(tapnum "$tap" fail); c=$(tapnum "$tap" cancelled); tt=$(tapnum "$tap" tests)
        ex=$(grep -E '^exit ' "$tap" | tail -1 | awk '{print $2}')
        note=""
        [ "$ex" = 124 ] && note=" TIMEOUT"
        echo "   $b: pass ${p:-0}/${tt:-?} fail ${f:-?} cancelled ${c:-0}$note"
        TP=$((TP + ${p:-0}))
        TF=$((TF + ${f:-0}))
        # first failures: leaf "not ok" lines with their error text
        awk '
            /^ *not ok / { name = $0; sub(/^ *not ok [0-9]+ - /, "", name); next }
            name != "" && /^ *error: / { e = $0; sub(/^ *error: */, "", e); if (e !~ /subtests? failed/) print "      " name " :: " substr(e, 1, 180); name = "" }
        ' "$tap" | head -3
    done
done
if [ -n "$SHARED_ERRS" ]; then echo "-- errors in shared files:"; printf '%s' "$SHARED_ERRS" | head -8; fi
echo "== build: $([ $BUILD_OK = 1 ] && echo ok || echo FAILED) in $(secs "$BUILD_START" "$BUILD_END")s (${round} round(s))  tests: pass $TP fail $TF in $(secs "$TEST_START" "$TEST_END")s  loadavg: $(cut -d' ' -f1-3 /proc/loadavg)"
echo "   logs: $LOGS"
