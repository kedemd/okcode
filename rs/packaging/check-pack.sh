#!/usr/bin/env bash
# The pack guard (release plan C6, with check-glibc.sh as npm `prepack`): what @kedem/okcode ships stays what it should.
# - package.json "files" never lists rs/, test/, grammars/, bench/, scripts/ or a source map;
# - no *.map file and no sourceMappingURL in the shipped JS (src/, bin/);
# - the bundled binaries are stripped (no .symtab, no .debug_* sections).
# The shipped JS is readable thin wrappers over the native addon, as 0.2's JS was readable (0.2 shipped its sources unminified).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
fail=0
bad=$(node -e "const f=require('./package.json').files||[];const b=f.filter(x=>/^(rs|test|grammars|bench|scripts)(\/|$)|\.map$/.test(x));console.log(b.join(' '))")
[ -z "$bad" ] || { echo "check-pack: package.json files lists $bad"; fail=1; }
maps=$(find src bin -name '*.map' 2>/dev/null; grep -rl 'sourceMappingURL' src bin 2>/dev/null)
[ -z "$maps" ] || { echo "check-pack: source maps shipped: $maps"; fail=1; }
for f in native/*/*; do
    [ -f "$f" ] || continue
    if file -b "$f" | grep -q ELF; then
        n=$(readelf -S "$f" 2>/dev/null | grep -cE '\.symtab|\.debug_')
        [ "$n" = 0 ] || { echo "check-pack: $f is not stripped ($n symbol/debug sections)"; fail=1; }
    fi
done
[ $fail = 0 ] && echo "check-pack: ok" || { echo "check-pack: FAILED"; exit 1; }
