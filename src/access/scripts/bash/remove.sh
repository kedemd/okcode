# stdin: root, then one relative path per field. Best effort, never fatal;
# `rm -f` refuses directories.
while IFS= read -r -d '' p; do
    rm -f -- "./$p" 2>/dev/null
done
true
