#!/usr/bin/env bash
# Builds okcode's linux-x64-gnu addon + standalone CLI against an OLD glibc (manylinux_2_28: glibc 2.28) inside Docker, so they
# load on Debian 11+ / Ubuntu 20.04+ / RHEL 8+ (node:24-bookworm-slim is glibc 2.36). A plain cargo build here links against the
# host's glibc (2.43) and the binaries then fail to load elsewhere ("GLIBC_2.xx not found"). Same approach as okdb's
# release build. Then proves it: the glibc floor, and both binaries load in node:24-bookworm-slim.
#
#   scripts/release/build-linux-x64.sh                  # dist profile -> native/linux-x64-gnu/{okcode.node,okcode}
#   PROFILE=release scripts/release/build-linux-x64.sh  # faster, for iterating
#   SKIP_VERIFY=1 …                                  # build only
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
BUILD_IMAGE="${BUILD_IMAGE:-quay.io/pypa/manylinux_2_28_x86_64}"
TEST_IMAGE="${TEST_IMAGE:-node:24-bookworm-slim}"
PROFILE="${PROFILE:-dist}"
CACHE="${CACHE:-$HOME/.cache/okcode-build/docker}"
OUT="${OUT:-$REPO/native/linux-x64-gnu}"
JOBS="${CARGO_BUILD_JOBS:-3}"
# reuse okdb's bootstrapped toolchain when present (read-only is not possible for rustup: share the dir)
RUSTUP_DIR="${RUSTUP_DIR:-$HOME/.cache/okdb-rs-docker/rustup}"
mkdir -p "$CACHE/cargo-home" "$CACHE/target" "$OUT" "$RUSTUP_DIR"
# seed the registry from the host's (no network for crates already fetched)
[ -d "$CACHE/cargo-home/registry" ] || { [ -d "$HOME/.cargo/registry" ] && cp -al "$HOME/.cargo/registry" "$CACHE/cargo-home/registry" 2>/dev/null || true; }
step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
PDIR="$PROFILE"; [ "$PROFILE" = dev ] && PDIR=debug

step "1. cargo build --profile $PROFILE -p okcode-node -p okcode-cli in $BUILD_IMAGE"
docker run --rm --user "$(id -u):$(id -g)" \
    -v "$REPO:/src:ro" -v "$RUSTUP_DIR:/rustup" -v "$CACHE/cargo-home:/cargo-home" -v "$CACHE/target:/target" \
    -e RUSTUP_HOME=/rustup -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS="$JOBS" -e HOME=/tmp -e TMPDIR=/tmp \
    -w /src "$BUILD_IMAGE" \
    sh -c 'set -e
           export PATH=/cargo-home/bin:/rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH
           if ! command -v cargo >/dev/null; then
               curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable
           fi
           rustc --version; ldd --version | head -1; cc --version | head -1
           cargo build --locked --profile '"$PROFILE"' -p okcode-node -p okcode-cli'
cp "$CACHE/target/$PDIR/libokcode_node.so" "$OUT/okcode.node"
cp "$CACHE/target/$PDIR/okcode" "$OUT/okcode"
ls -la "$OUT"
[ -n "${SKIP_VERIFY:-}" ] && exit 0

step "2. glibc floor"
"$HERE/check-glibc.sh" "$OUT/okcode.node" "$OUT/okcode"

step "3. load in $TEST_IMAGE"
docker run --rm --user "$(id -u):$(id -g)" -v "$OUT:/x:ro" "$TEST_IMAGE" sh -c '
    ldd --version | head -1
    node -e "const m = require(\"/x/okcode.node\"); console.log(\"okcode addon loads:\", Object.keys(m).length, \"exports\")"
    /x/okcode --help | head -1'
echo "build-linux-x64.sh: OK -> $OUT"
