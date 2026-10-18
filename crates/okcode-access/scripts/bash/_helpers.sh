# GNU find (and bfs) have -printf; BSD find (macOS) does not and falls back to
# `stat -f`. Chosen once per call. list and stat use the SAME formatter per
# host, so an mtime from list compares equal to one from stat for an unchanged
# file. GNU `%T@` is seconds.fraction, BSD `%m` whole seconds — values are only
# ever compared against earlier values from this same host, never across.
if find . -maxdepth 0 -printf '' >/dev/null 2>&1; then gnufind=1; else gnufind=; fi
if command -v sha1sum >/dev/null 2>&1; then sha1cmd='sha1sum'; else sha1cmd='shasum -a 1'; fi
# Hash is computed HERE, on the target — bytes are never pulled just to hash.
# Lowercase on the wire; the caller uppercases.
hashf() {
    local o
    o=$($sha1cmd < "$1" 2>/dev/null) || return 1
    o=${o%% *}
    [ ${#o} -eq 40 ] || return 1
    printf '%s' "$o"
}
# "size mtime" for a regular file (links followed), nothing otherwise.
statf() {
    if [ -n "$gnufind" ]; then
        find -L "$1" -maxdepth 0 -type f -printf '%s %T@' 2>/dev/null
    else
        [ -f "$1" ] && stat -L -f '%z %m' "$1" 2>/dev/null
    fi
}
