# stdin: root, want-hash ("1"/"0"), then one relative path per field.
# stdout: base64 of NUL-separated records, each keyed by the path exactly as
# the script named it ("./<rel>"):
#   S <path> "<size> <mtime>"   a regular file (links followed)
#   H <path> <sha1>             its hash (only when asked)
#   "<sha1>  <path>"            the same, straight from `sha1sum -z`
# A path with no S record is missing / not a regular file; with hash asked and
# no hash record it is unreadable. Both answer "missing".
#
# GNU userland: ONE `find` for every path (and ONE `sha1sum -z`), in slices so
# a huge batch cannot overrun ARG_MAX — a per-path fork costs ~2 ms, and stat is
# the hottest call a caller makes. BSD userland: one stat (+ hash) per path.
#
# stat BEFORE hashing: a write landing in between pairs a newer hash with an
# older mtime, which the caller's next stat corrects. The reverse order could
# pair an OLD hash with a NEW mtime — a wrong answer that would stick.
IFS= read -r -d '' want
ps=()
while IFS= read -r -d '' p; do ps+=("./$p"); done
n=${#ps[@]}
[ "$n" -eq 0 ] && exit 0
sha1z=
if [ "$want" = 1 ] && [ "$sha1cmd" = sha1sum ] && sha1sum -z < /dev/null > /dev/null 2>&1; then sha1z=1; fi
{
    if [ -n "$gnufind" ]; then
        for ((o = 0; o < n; o += 500)); do
            find -L "${ps[@]:o:500}" -maxdepth 0 -type f -printf 'S\0%p\0%s %T@\0' 2>/dev/null
        done
        if [ "$want" = 1 ]; then
            if [ -n "$sha1z" ]; then
                for ((o = 0; o < n; o += 500)); do
                    sha1sum -z -- "${ps[@]:o:500}" 2>/dev/null
                done
            else
                for f in "${ps[@]}"; do h=$(hashf "$f") && printf 'H\0%s\0%s\0' "$f" "$h"; done
            fi
        fi
    else
        for f in "${ps[@]}"; do
            s=$(statf "$f")
            [ -n "$s" ] || continue
            printf 'S\0%s\0%s\0' "$f" "$s"
            if [ "$want" = 1 ] && h=$(hashf "$f"); then printf 'H\0%s\0%s\0' "$f" "$h"; fi
        done
    fi
} | base64
