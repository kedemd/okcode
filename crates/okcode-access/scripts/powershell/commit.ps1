# The atomic commit primitive (same framing and answers as the bash dialect).
# stdin: root, op, relpath, expected hash or "-", payload hash, then the payload
# base64 as its own final line (not re-encoded).
#
# BOTH operations publish a COMPLETED temp file; bytes are never written
# directly to the destination. `create` uses [IO.File]::Move, whose 2-argument
# overload THROWS if the destination exists — genuinely atomic fail-if-present,
# a single OS rename, not a check-then-write. `replace` uses [IO.File]::Replace,
# the proven atomic swap. Neither ever falls back to Move-Item -Force or a copy:
# any other failure is NOATOMIC and the temp file is deleted.
#
# The destination's Attributes are copied onto the temp file before a replace,
# so hidden/readonly/etc. survive an edit (Windows has no POSIX mode bits; this
# is the closest analogue). A reparse-point destination (symlink/junction) is
# refused with SYMLINK before anything is read or written.
$op = NextField
$rel = NextField
$expected = NextField
$want = NextField
$payload = $stdin.ReadLine()
if ($null -eq $payload) { $payload = '' }
$path = P $rel
$dir = [IO.Path]::GetDirectoryName($path)
$attrs = $null
try { $attrs = [IO.File]::GetAttributes($path) } catch {}
if ($null -ne $attrs -and ($attrs -band [IO.FileAttributes]::ReparsePoint)) { W 'SYMLINK'; exit 0 }
if ($op -eq 'create') {
    if ($null -ne $attrs) { W 'EXISTS'; exit 0 }
    try { [void][IO.Directory]::CreateDirectory($dir) } catch { W 'NOATOMIC mkdir'; exit 0 }
} elseif ($null -eq $attrs -or ($attrs -band [IO.FileAttributes]::Directory)) { W 'MISSING'; exit 0 }
$tmp = [IO.Path]::Combine($dir, '.okcode-tmp-' + [Guid]::NewGuid().ToString('N'))
try {
    try { [IO.File]::WriteAllBytes($tmp, [Convert]::FromBase64String($payload)) } catch { W 'NOATOMIC write'; exit 0 }
    $tmpHash = H $tmp
    if ($tmpHash -ne $want) { W "CORRUPT $tmpHash"; exit 0 }
    if ($op -eq 'create') {
        try { [IO.File]::Move($tmp, $path) }
        catch { if ([IO.File]::Exists($path) -or [IO.Directory]::Exists($path)) { W 'EXISTS' } else { W 'NOATOMIC move' }; exit 0 }
        W "OK $tmpHash"; exit 0
    }
    try { [IO.File]::SetAttributes($tmp, $attrs) } catch {}
    # Re-hashed immediately before the publish: the narrowest window available.
    # A non-cooperating writer landing between here and Replace is a
    # documented limit, not a guarantee.
    try { $cur = H $path } catch { W 'MISSING'; exit 0 }
    if ($expected -ne '-' -and $cur -ne $expected) { W "STALE $cur"; exit 0 }
    # On .NET Framework 4.8 (Windows PowerShell 5.1) Replace($tmp, $path, $null)
    # throws "The path is not of a legal form" — a real framework quirk. A real,
    # disposable backup path avoids it; it is discarded right after the swap.
    $backup = [IO.Path]::Combine($dir, '.okcode-bak-' + [Guid]::NewGuid().ToString('N'))
    try { [IO.File]::Replace($tmp, $path, $backup) } catch { W 'NOATOMIC replace'; exit 0 }
    try { [IO.File]::Delete($backup) } catch {}
    W "OK $tmpHash"
} finally {
    try { if ([IO.File]::Exists($tmp)) { [IO.File]::Delete($tmp) } } catch {}
}
