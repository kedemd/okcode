#!/usr/bin/env bash
# Machine-wide gate for heavy cargo builds, shared with the okdb session:
# at most 2 at once across both (slots ~/.cache/cargo-heavy.{1,2}.lock).
#   rs/tools/heavy.sh cargo build --release -p okcode-core
# Cheap loops (cargo check -p <crate>, a crate's golden test in debug) don't need it.
#
# flock -o: the lock fd is NOT inherited by the command. Without it, a daemon the
# build starts (the sccache server) keeps the fd open and holds the slot forever.
set -u
BUSY=75
for i in 1 2; do
    flock -n -o -E $BUSY "$HOME/.cache/cargo-heavy.$i.lock" "$@"
    rc=$?
    [ $rc -ne $BUSY ] && exit $rc
done
exec flock -o "$HOME/.cache/cargo-heavy.1.lock" "$@"
