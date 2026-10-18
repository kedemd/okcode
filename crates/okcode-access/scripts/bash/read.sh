# stdin: root, then one relative path per field.
# GNU userland — the batch in two forks, not one per file:
#   stdout: base64 of NUL-separated (path, size) pairs for every readable
#   regular file, in order; a line "@DATA"; base64 of those files' bytes
#   concatenated in the same order (`cat`, sliced for ARG_MAX).
# The caller splits the bytes by the sizes and checks the total. A file that
# changed size between the find and the cat (or failed mid-read) breaks the
# total, and the caller falls back to read-each for the batch — never a
# silently mis-split answer. BSD userland: behaves exactly like read-each.
ps=()
while IFS= read -r -d '' p; do ps+=("./$p"); done
n=${#ps[@]}
[ "$n" -eq 0 ] && exit 0
if [ -z "$gnufind" ]; then
    i=0
    for f in "${ps[@]}"; do
        if [ -f "$f" ] && [ -r "$f" ]; then
            printf '@%s\n' "$i"
            base64 < "$f" 2>/dev/null || printf '\n!\n'
        fi
        i=$((i + 1))
    done
    exit 0
fi
files=()
sizes=()
while IFS= read -r -d '' f && IFS= read -r -d '' s; do
    files+=("$f")
    sizes+=("$s")
done < <(for ((o = 0; o < n; o += 500)); do find -L "${ps[@]:o:500}" -maxdepth 0 -type f -readable -printf '%p\0%s\0' 2>/dev/null; done)
m=${#files[@]}
for ((i = 0; i < m; i++)); do printf '%s\0%s\0' "${files[i]}" "${sizes[i]}"; done | base64
printf '@DATA\n'
for ((o = 0; o < m; o += 500)); do cat -- "${files[@]:o:500}" 2>/dev/null; done | base64
