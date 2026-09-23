# stdin: root, pid. stdout: "HOST <nodename>" then "ALIVE <starttime|->" | "DEAD"
# | "UNKNOWN". The host is the TARGET's, so a lock is only ever judged by the
# machine its pid could live on. /proc (Linux) answers for any user's process
# and gives the start time (field 22, parsed after the last ')' since the
# command name may contain spaces) so a reused pid is told apart from the
# lock's owner. Without /proc: `kill -0`, then `ps -p` for another user's
# process (kill -0 fails with EPERM there, which must not read as dead).
IFS= read -r -d '' target
printf 'HOST %s\n' "$(uname -n)"
case "$target" in ''|*[!0-9]*) echo UNKNOWN; exit 0 ;; esac
if [ -d /proc/self ]; then
    if [ -d "/proc/$target" ]; then
        s=$(cat "/proc/$target/stat" 2>/dev/null)
        s=${s##*) }
        set -- $s
        echo "ALIVE ${20:--}"
    else
        echo DEAD
    fi
elif kill -0 "$target" 2>/dev/null || ps -p "$target" >/dev/null 2>&1; then
    echo 'ALIVE -'
else
    echo DEAD
fi
