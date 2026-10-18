# stdin: root, the subtree to list ("" = the whole root), the file limit ("0" =
# none), then one skip-directory name per field.
# stdout: base64 of NUL-separated (relpath, size, mtime) triples. Encoding the
# whole listing once (one fork) keeps any filename byte — spaces, newlines,
# non-ASCII — intact without a per-file base64 fork.
#
# Two rules here are load-bearing:
#
# A FIXED LIST OF KNOWN NON-SOURCE DOT-DIRECTORIES IS PRUNED BY THE WALK, not
# filtered afterwards — otherwise the walk still descends every byte of .git
# or a working database directory. $dotprune is the same list the local and
# PowerShell facades prune (PRUNE_DOT_DIRS). Anything NOT on this list
# (`.github/`, `.storybook/`, a `.well-known` route) is walked normally and
# left to `.gitignore` to decide, so source a project keeps in a
# dot-directory is still found.
# The prune is anchored BELOW the root: a root that is itself a dot
# directory must list normally, not prune the whole walk at its first step.
dotprune=(.git .hg .svn .okcode .adcode .next .nuxt .svelte-kit .turbo .cache .parcel-cache .venv .tox .mypy_cache .pytest_cache .ruff_cache .gradle .idea .vs .terraform .serverless .yarn .pnpm-store .expo)

# AN UNREADABLE FILE IS SKIPPED, NOT FATAL. A workspace containing a running
# database, or any file another process holds locked, must cost that file and
# nothing more — never the whole listing.
#
# A SUBTREE (`under`, a directory or one file) is listed from inside it, so the
# skip names and the dot-directory prune apply BELOW it and never to its own
# segments — that is how an explicitly named node_modules/<pkg> is reached. It
# must stay inside the root by REAL path (`pwd -P`): a symlink on the way that
# resolves elsewhere is refused (exit 4), never followed out. Paths are still
# printed relative to the root: the subtree's own path is the printf prefix,
# escaped for printf.
#
# THE LIMIT stops the walk (head closes the pipe, find dies of SIGPIPE): at
# most limit+1 records come back, and the caller reads the extra one as
# "there was more". A head without -z just means no early stop; the caller
# still caps.
IFS= read -r -d '' under || exit 3
IFS= read -r -d '' limit || exit 3
names=("${dotprune[@]}")
while IFS= read -r -d '' s; do names+=("$s"); done
# Built with `-o` BETWEEN entries, never trailing: a dangling trailing `-o`
# would be a find syntax error. (`names` is never empty — dotprune alone
# guarantees that.)
args=()
last=$((${#names[@]} - 1))
for i in "${!names[@]}"; do
    args+=(-name "${names[$i]}")
    [ "$i" -lt "$last" ] && args+=(-o)
done
cap=(cat)
if [ "${limit:-0}" -gt 0 ] 2>/dev/null && printf 'a\0' | head -z -n 1 >/dev/null 2>&1; then
    cap=(head -z -n $((3 * (limit + 1))))
fi
pfx=
if [ -n "$under" ]; then
    rootp=$(pwd -P)
    if [ -d "$under" ]; then d=$under; else d=$(dirname -- "$under"); fi
    [ -e "./$under" ] && real=$(cd -- "./$d" 2>/dev/null && pwd -P) || { printf '' | base64; exit 0; }
    case "$real/" in
        "$rootp"/*) ;;
        *) echo "okcode: workspace path escapes the root (symlink): $under" >&2; exit 4 ;;
    esac
    if [ ! -d "$under" ]; then
        # One file: listed only if regular, readable and not itself a link.
        if [ -f "$under" ] && [ ! -L "$under" ] && [ -r "$under" ]; then
            s=$(statf "./$under") && printf '%s\0%s\0%s\0' "$under" "${s% *}" "${s#* }" | base64
        else
            printf '' | base64
        fi
        exit 0
    fi
    cd -- "./$under" || exit 3
    pfx="$under/"
fi
if [ -n "$gnufind" ]; then
    fp=${pfx//\\/\\\\}
    fp=${fp//%/%%}
    find . -mindepth 1 \( -type d \( "${args[@]}" \) -prune \) -o \( -type f -readable -printf "$fp%P\0%s\0%T@\0" \) 2>/dev/null | "${cap[@]}" | base64
else
    n=0
    find . -mindepth 1 \( -type d \( "${args[@]}" \) -prune \) -o \( -type f -print0 \) 2>/dev/null |
    while IFS= read -r -d '' f; do
        [ -r "$f" ] || continue
        s=$(stat -f '%z %m' "$f" 2>/dev/null) || continue
        printf '%s\0%s\0%s\0' "$pfx${f#./}" "${s% *}" "${s#* }"
        n=$((n + 1))
        [ "${limit:-0}" -gt 0 ] && [ "$n" -gt "$limit" ] && break
    done | base64
fi
