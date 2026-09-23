# stdin: root, then the command as base64 to end of input.
# stdout: "RC <n>", "@OUT", base64(stdout), "@ERR", base64(stderr).
# TRUST BOUNDARY: this evaluates a caller-supplied command in the workspace
# root. exec is a capability the host grants per workspace, not a default.
cmd=$(base64 -d; printf x)
cmd=${cmd%x}
d=$(mktemp -d 2>/dev/null || mktemp -d -t okcode) || exit 3
trap 'rm -rf -- "$d"' EXIT
( eval "$cmd" ) < /dev/null > "$d/o" 2> "$d/e"
rc=$?
printf 'RC %s\n@OUT\n' "$rc"
base64 < "$d/o"
printf '@ERR\n'
base64 < "$d/e"
