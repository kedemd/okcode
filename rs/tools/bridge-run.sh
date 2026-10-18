#!/usr/bin/env bash
# Release plan C4: the whole check on okdb's C bridge, one command, once okdb's addon tarball exists.
#
#   rs/tools/bridge-run.sh <kedem-okdb-3.x.tgz> [--no-bench]
#
# 1. integrate.sh: merge, one debug build (no okdb crates), okdb's tarball installed into the integration tree, every
#    slice's JS tests as shipped (no preload): api, tools, perf, regress, pure-*, access = all 40 test files but e2e-ollama.
# 2. the Rust CLI against okdb's installed .node: test/cli.test.js (OKCODE_CLI_BIN) and rs/golden/cli-diff.js.
# 3. (unless --no-bench) the CPU/RSS benchmark on the release addon.
set -uo pipefail
PACK=${1:?okdb tarball}; shift
BENCH=1; [ "${1:-}" = --no-bench ] && BENCH=0
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CACHE="${OKCODE_CACHE:-$HOME/.cache/okcode-rs}"
L=$CACHE/logs/bridge; mkdir -p "$L"
export OKCODE_INTEG_BASE=${OKCODE_INTEG_BASE:-master}
T0=$(date +%s)
echo "== 1. suite (integrate.sh, OKDB_PACK=$PACK)"
OKDB_PACK=$PACK JOBS=${JOBS:-3} TEST_JOBS=${TEST_JOBS:-3} timeout 5400 "$REPO/rs/tools/integrate.sh" --only api,tools,perf,regress,pure-text,pure-parse,pure-ts,pure-misc,access > "$L/suite.out" 2>&1
echo "   exit=$?"; sed -n '/^-- /,$p' "$L/suite.out" | grep -v ': pass [0-9]*/[0-9]* fail 0 '
WT=$CACHE/wt/integ
OKDB_NODE=$(ls "$WT"/node_modules/@kedem/okdb/native/*/okdb-node.*.node "$WT"/node_modules/@okdb/native-*/okdb-node.*.node "$WT"/node_modules/@kedem/okdb-native-*/okdb-node.*.node "$WT"/node_modules/@kedem/okdb/node_modules/@okdb/native-*/okdb-node.*.node "$WT"/node_modules/@kedem/okdb/node_modules/@kedem/okdb-native-*/okdb-node.*.node 2>/dev/null | head -1)
echo "== 2. Rust CLI (okdb .node: ${OKDB_NODE:-NOT FOUND})"
( cd "$CACHE/wt/integ/rs" && RUSTC_WRAPPER="${RUSTC_WRAPPER:-}" CARGO_TARGET_DIR=$CACHE/target-integ timeout 1800 cargo build -j3 -p okcode-cli --bins > "$L/cli-build.log" 2>&1 ) || echo "   CLI build failed (see $L/cli-build.log)"
BIN=$CACHE/target-integ/debug/okcode
mkdir -p "$CACHE/tmp/bridge-cli"
( cd "$WT" && OKDB_NATIVE_BINARY=$OKDB_NODE OKCODE_NATIVE_BINARY=$CACHE/target-integ/debug/okcode-node.node OKCODE_CLI_BIN=$BIN TMPDIR=$CACHE/tmp/bridge-cli \
    timeout 600 node --test --test-force-exit --test-reporter=tap test/cli.test.js > "$L/cli.tap" 2>&1 )
echo "   cli.test.js: $(grep -E '^# (pass|fail)' "$L/cli.tap" | tr '\n' ' ')"
( cd "$WT" && OKDB_NATIVE_BINARY=$OKDB_NODE OKCODE_NATIVE_BINARY=$CACHE/target-integ/debug/okcode-node.node TMPDIR=$CACHE/tmp/bridge-cli \
    timeout 900 node rs/golden/cli-diff.js "$BIN" > "$L/cli-diff.out" 2>&1 )
echo "   $(head -1 "$L/cli-diff.out")"; sed -n '2,20p' "$L/cli-diff.out"
rm -rf "$CACHE/tmp/bridge-cli"
if [ $BENCH = 1 ]; then
    echo "== 3. benchmark (release)"
    ( cd "$WT/rs" && RUSTC_WRAPPER="${RUSTC_WRAPPER:-}" CARGO_TARGET_DIR=$CACHE/target-integ timeout 3000 "$REPO/rs/tools/heavy.sh" cargo build -j3 --release -p okcode-node --lib > "$L/rel.log" 2>&1 )
    cp -f "$CACHE/target-integ/release/libokcode_node.so" "$CACHE/target-integ/release/okcode-node.node"
    [ -e "$CACHE/wt/master/.git" ] || git -C "$REPO" worktree add -f "$CACHE/wt/master" master >/dev/null 2>&1
    [ -e "$CACHE/wt/master/node_modules" ] || ln -s "$REPO/node_modules" "$CACHE/wt/master/node_modules"
    ( cd "$WT" && JS_TREE=$CACHE/wt/master timeout 3000 rs/tools/bench-cpu.sh "${BENCH_REPO:?set BENCH_REPO to the repository to benchmark}" "$CACHE/bench-bridge" "$CACHE/target-integ/release/okcode-node.node" 3 > "$L/bench.md" 2>&1 )
    cat "$L/bench.md"; rm -rf "$CACHE/bench-bridge"
fi
echo "== done in $(( $(date +%s) - T0 ))s; logs: $L"
