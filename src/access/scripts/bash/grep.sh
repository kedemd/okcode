# stdin: root, flags ("i" = ignore case, "-" = none), the literal, then one
# relative path per field.
# stdout: "NOGREP" when the target's grep cannot do this search; otherwise
# "@OK" and base64 of the NUL-separated paths ("./rel") that CONTAIN the
# literal. A pre-filter only: the caller reads these files and matches them
# itself, so the target's regex dialect never decides an answer.
#
# C locale: a fixed-string match over BYTES, and case folding over ASCII only
# (the caller only asks -i for an ASCII literal) — the same set of files the
# caller's own matcher would accept, which is what makes skipping the rest
# sound. -a: a file with a stray NUL is still searched. -s: an unreadable or
# vanished file is simply not listed (it could not be read anyway).
IFS= read -r -d '' flags || exit 3
IFS= read -r -d '' pat || exit 3
ps=()
while IFS= read -r -d '' p; do ps+=("./$p"); done
export LC_ALL=C
opts=(-l -a -s -F --null)
[ "$flags" = i ] && opts+=(-i)
# The probe: a grep that rejects any of these options (or has none) must say
# so rather than answer "no file matches".
if ! printf 'okcode\n' | grep "${opts[@]}" -e okcode >/dev/null 2>&1; then
    echo NOGREP
    exit 0
fi
echo @OK
n=${#ps[@]}
for ((o = 0; o < n; o += 500)); do grep "${opts[@]}" -e "$pat" -- "${ps[@]:o:500}" 2>/dev/null; done | base64
