# Every script's stdin starts with the workspace root as a NUL-terminated field.
# Nothing is interpolated into these scripts: root, paths, skip names and
# payloads all arrive on stdin, so quoting the script is the only quoting a
# transport ever needs. `read -d ''` reads a pipe one byte at a time, so it
# never over-consumes: a payload that follows the fields is left for the next
# reader (`base64 -d`).
IFS= read -r -d '' root || { echo 'okcode: no workspace root on stdin' >&2; exit 3; }
cd -- "$root" 2>/dev/null || { echo "okcode: workspace root not found: $root" >&2; exit 3; }
