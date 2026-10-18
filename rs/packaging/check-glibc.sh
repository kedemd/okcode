#!/usr/bin/env bash
# The packaging check (release plan C6): every bundled ELF binary needs at most GLIBC_$GLIBC_FLOOR (default 2.28, manylinux_2_28)
# and no GLIBCXX/CXXABI symbol at all (okcode links no C++ runtime), so it loads on Debian 11+ / Ubuntu 20.04+ / RHEL 8+.
#   rs/packaging/check-glibc.sh [files…]     # default: native/linux-x64-gnu/*  (npm's prepack runs it)
set -uo pipefail
FLOOR="${GLIBC_FLOOR:-2.28}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
[ $# -gt 0 ] || set -- "$ROOT"/native/linux-x64-gnu/*
fail=0
for f in "$@"; do
    [ -f "$f" ] || { echo "check-glibc: missing $f"; fail=1; continue; }
    max="$(objdump -T "$f" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
    cxx="$(objdump -T "$f" | grep -oE '(GLIBCXX|CXXABI)_[0-9.]+' | sort -uV | tail -1)"
    ok=1
    [ "$(printf '%s\n%s\n' "${max#GLIBC_}" "$FLOOR" | sort -V | tail -1)" = "$FLOOR" ] || ok=0
    [ -z "$cxx" ] || ok=0
    echo "$(basename "$f"): ${max:-no GLIBC} ${cxx:+$cxx }$([ $ok = 1 ] && echo ok || echo "FAIL (floor GLIBC_$FLOOR, no C++ ABI)")"
    [ $ok = 1 ] || fail=1
done
[ $fail = 0 ] || { echo "check-glibc: rebuild with rs/packaging/build-linux-x64.sh"; exit 1; }
