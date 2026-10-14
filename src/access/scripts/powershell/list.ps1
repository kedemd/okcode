# stdin: root, the subtree to list ("" = the whole root), the file limit ("0" =
# none), then one skip-directory name per field.
# stdout: base64 of NUL-separated (relpath, size, mtime-ticks) triples.
#
# The walk is explicit so pruning happens BEFORE descent: skip names and a
# FIXED list of known non-source dot-directories ($dotPrune — tooling state,
# caches, vendored output, never a caller's own source) below the root are
# never entered (the root itself may be a dot directory). Anything NOT on
# $dotPrune (`.github/`, `.storybook/`, a `.well-known` route) is walked
# normally and left to `.gitignore` to decide — this used to prune EVERY
# dot-directory (`$e.Name.StartsWith('.')`), which silently dropped real
# source a real project keeps in one. Keep $dotPrune in exact agreement with
# common.js's PRUNE_DOT_DIRS and scripts/bash/list.sh's $dotprune —
# test/access-contract.test.js checks all three. Reparse points are not
# followed. AN UNREADABLE FILE IS SKIPPED, NOT FATAL: a workspace holding a
# running database or any file another process has locked must cost that
# file, never the whole listing.
$dotPrune = @('.git', '.hg', '.svn', '.okcode', '.adcode', '.next', '.nuxt', '.svelte-kit', '.turbo', '.cache', '.parcel-cache', '.venv', '.tox', '.mypy_cache', '.pytest_cache', '.ruff_cache', '.gradle', '.idea', '.vs', '.terraform', '.serverless', '.yarn', '.pnpm-store', '.expo')
$dotPruneSet = @{}
foreach ($d in $dotPrune) { $dotPruneSet[$d] = $true }
#
# A SUBTREE (`under`, a directory or one file) is walked from inside it, so
# skip names and the dot-directory prune apply BELOW it, never to its own
# segments. Confinement: the path is lexically inside the root (checked by the
# caller) and no step of it from the root down may be a reparse point — a link
# on the way could lead anywhere, so it is refused (exit 4), not resolved. THE
# LIMIT stops the walk after limit+1 files; the caller reads the extra one as
# "there was more".
$under = NextField
$limit = [int](NextField)
$skip = @{}
while ($null -ne ($s = NextField)) { $skip[$s] = $true }
$sb = New-Object System.Text.StringBuilder
$prefix = $root.TrimEnd('\', '/').Length + 1
$start = $root
if ($under) {
    $start = P $under
    $step = $root
    foreach ($seg in ($under -split '[\\/]')) {
        if (-not $seg) { continue }
        $step = [IO.Path]::Combine($step, $seg)
        if (-not ([IO.File]::Exists($step) -or [IO.Directory]::Exists($step))) { W ''; exit 0 }
        if ((Get-Item -LiteralPath $step -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
            [Console]::Error.WriteLine("okcode: workspace path escapes the root (link): $under"); exit 4
        }
    }
    if ([IO.File]::Exists($start)) {
        # One file: listed only if it can be opened for reading.
        $e = New-Object IO.FileInfo $start
        try { $fs = [IO.File]::Open($e.FullName, 'Open', 'Read', 'ReadWrite'); $fs.Dispose() } catch { W ''; exit 0 }
        $rel = $e.FullName.Substring($prefix).Replace('\', '/')
        [void]$sb.Append($rel).Append([char]0).Append($e.Length).Append([char]0).Append($e.LastWriteTimeUtc.Ticks).Append([char]0)
        W ([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($sb.ToString())))
        exit 0
    }
}
$n = 0
$stack = New-Object System.Collections.Stack
$stack.Push($start)
while ($stack.Count -gt 0) {
    if ($limit -gt 0 -and $n -gt $limit) { break }
    $dir = $stack.Pop()
    try { $entries = (New-Object IO.DirectoryInfo $dir).GetFileSystemInfos() } catch { continue }
    foreach ($e in $entries) {
        if ($e.Attributes -band [IO.FileAttributes]::ReparsePoint) { continue }
        if ($e -is [IO.DirectoryInfo]) {
            if (-not $skip.ContainsKey($e.Name) -and -not $dotPruneSet.ContainsKey($e.Name)) { $stack.Push($e.FullName) }
            continue
        }
        try { $fs = [IO.File]::Open($e.FullName, 'Open', 'Read', 'ReadWrite'); $fs.Dispose() } catch { continue }
        $rel = $e.FullName.Substring($prefix).Replace('\', '/')
        [void]$sb.Append($rel).Append([char]0).Append($e.Length).Append([char]0).Append($e.LastWriteTimeUtc.Ticks).Append([char]0)
        $n++
    }
}
W ([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($sb.ToString())))
