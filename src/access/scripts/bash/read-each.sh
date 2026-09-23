# stdin: root, then one relative path per field. The fallback (and BSD) read:
# per readable regular file, "@i" (its index in the request) then its base64,
# any line wrapping; a "!" line after it voids that entry (the read failed
# part-way). `@` and `!` are outside the base64 alphabet, so they cannot collide
# with content. A missing or unreadable file is SKIPPED, not fatal — one absent
# file must not lose every other file in the batch.
i=0
while IFS= read -r -d '' p; do
    f="./$p"
    if [ -f "$f" ] && [ -r "$f" ]; then
        printf '@%s\n' "$i"
        base64 < "$f" 2>/dev/null || printf '\n!\n'
    fi
    i=$((i + 1))
done
