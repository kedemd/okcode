# stdin: root, then one skip-directory name per field.
# stdout: base64 of NUL-separated (relpath, size, mtime-ticks) triples.
#
# The walk is explicit so pruning happens BEFORE descent: skip names and
# dot-directories below the root are never entered (the root itself may be a
# dot directory). Reparse points are not followed. AN UNREADABLE FILE IS
# SKIPPED, NOT FATAL: a workspace holding a running database or any file
# another process has locked must cost that file, never the whole listing.
$skip = @{}
while ($null -ne ($s = NextField)) { $skip[$s] = $true }
$sb = New-Object System.Text.StringBuilder
$prefix = $root.TrimEnd('\', '/').Length + 1
$stack = New-Object System.Collections.Stack
$stack.Push($root)
while ($stack.Count -gt 0) {
    $dir = $stack.Pop()
    try { $entries = (New-Object IO.DirectoryInfo $dir).GetFileSystemInfos() } catch { continue }
    foreach ($e in $entries) {
        if ($e.Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
        if ($e -is [IO.DirectoryInfo]) {
            if (-not $skip.ContainsKey($e.Name) -and -not $e.Name.StartsWith('.')) { $stack.Push($e.FullName) }
            continue
        }
        try { $fs = [IO.File]::Open($e.FullName, 'Open', 'Read', 'ReadWrite'); $fs.Dispose() } catch { continue }
        $rel = $e.FullName.Substring($prefix).Replace('\', '/')
        [void]$sb.Append($rel).Append([char]0).Append($e.Length).Append([char]0).Append($e.LastWriteTimeUtc.Ticks).Append([char]0)
    }
}
W ([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($sb.ToString())))
