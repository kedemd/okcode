# The atomic commit primitive.
# stdin: root, op ("create"|"replace"), relpath, expected sha1 (lowercase or
# "-"), payload sha1 (lowercase), then the payload as base64 to end of input.
# stdout: one line — OK <hash> | STALE <hash> | EXISTS | MISSING | SYMLINK |
# NOATOMIC <why> | CORRUPT <hash>.
#
# Bytes are never written to the destination directly: a COMPLETED temp file
# in the target's own directory is published. `create` uses `ln` (link(2),
# atomic fail-if-exists — never a check-then-write); `replace` uses `mv -f`
# (rename(2), atomic within one filesystem — refused rather than silently
# copied across a device boundary). If the proven call fails for any other
# reason the answer is NOATOMIC and the temp file is removed; it never degrades
# to a copy. The payload's hash travels with it: a payload clipped or mangled
# in transit is caught (CORRUPT) before anything is published.
IFS= read -r -d '' op
IFS= read -r -d '' path
IFS= read -r -d '' expected
IFS= read -r -d '' want
f="./$path"
dir=$(dirname -- "$f")
# A symlink destination is refused before anything is read or written:
# ln/mv would act on the link, not on what it points to.
if [ -L "$f" ]; then echo SYMLINK; exit 0; fi
if [ "$op" = create ]; then
    if [ -e "$f" ]; then echo EXISTS; exit 0; fi
    mkdir -p -- "$dir" 2>/dev/null || { echo 'NOATOMIC mkdir'; exit 0; }
elif [ ! -f "$f" ]; then
    echo MISSING; exit 0
fi
tmp="$dir/.okcode-tmp-$$-$RANDOM$RANDOM"
trap 'rm -f -- "$tmp" 2>/dev/null' EXIT
( set -C; base64 -d > "$tmp" ) 2>/dev/null || { echo 'NOATOMIC write'; exit 0; }
tmpHash=$(hashf "$tmp") || { echo 'NOATOMIC hash'; exit 0; }
if [ "$tmpHash" != "$want" ]; then echo "CORRUPT $tmpHash"; exit 0; fi
if [ "$op" = create ]; then
    if ln -- "$tmp" "$f" 2>/dev/null; then echo "OK $tmpHash"
    elif [ -e "$f" ] || [ -L "$f" ]; then echo EXISTS
    else echo 'NOATOMIC link'; fi
    exit 0
fi
# Mode bits survive an edit (the executable bit, notably).
m=$(stat -L -c %a "$f" 2>/dev/null || stat -L -f %Lp "$f" 2>/dev/null) && chmod "$m" "$tmp" 2>/dev/null
# The destination is re-hashed HERE, immediately before the rename — the
# narrowest window this primitive can achieve. It does not close the window
# against a non-cooperating writer landing between this line and the rename;
# that is a documented limit, not a guarantee.
if [ -L "$f" ]; then echo SYMLINK; exit 0; fi
cur=$(hashf "$f") || { echo MISSING; exit 0; }
if [ "$expected" != - ] && [ "$cur" != "$expected" ]; then echo "STALE $cur"; exit 0; fi
tmpDev=$(stat -c %d "$tmp" 2>/dev/null || stat -f %d "$tmp" 2>/dev/null)
dstDev=$(stat -c %d "$f" 2>/dev/null || stat -f %d "$f" 2>/dev/null)
if [ -n "$tmpDev" ] && [ -n "$dstDev" ] && [ "$tmpDev" != "$dstDev" ]; then echo 'NOATOMIC device'; exit 0; fi
if mv -f -- "$tmp" "$f" 2>/dev/null; then echo "OK $tmpHash"; else echo 'NOATOMIC rename'; fi
