# stdin: root, then one skip-directory name per field.
# stdout: base64 of NUL-separated (relpath, size, mtime) triples. Encoding the
# whole listing once (one fork) keeps any filename byte — spaces, newlines,
# non-ASCII — intact without a per-file base64 fork.
#
# Two rules here are load-bearing:
#
# DOT-DIRECTORIES ARE PRUNED BY THE WALK, not filtered afterwards — otherwise the
# walk still descends every byte of .git or a working database directory. The
# prune is anchored BELOW the root (we cd into the root and match `.?*`, which
# needs two characters, so `.` itself never matches): a root that is itself a
# dot directory must list normally, not prune the whole walk at its first step.
#
# AN UNREADABLE FILE IS SKIPPED, NOT FATAL. A workspace containing a running
# database, or any file another process holds locked, must cost that file and
# nothing more — never the whole listing.
args=()
while IFS= read -r -d '' s; do args+=(-name "$s" -o); done
if [ -n "$gnufind" ]; then
    find . -mindepth 1 \( -type d \( "${args[@]}" -name '.?*' \) -prune \) -o \( -type f -readable -printf '%P\0%s\0%T@\0' \) 2>/dev/null | base64
else
    find . -mindepth 1 \( -type d \( "${args[@]}" -name '.?*' \) -prune \) -o \( -type f -print0 \) 2>/dev/null |
    while IFS= read -r -d '' f; do
        [ -r "$f" ] || continue
        s=$(stat -f '%z %m' "$f" 2>/dev/null) || continue
        printf '%s\0%s\0%s\0' "${f#./}" "${s% *}" "${s#* }"
    done | base64
fi
