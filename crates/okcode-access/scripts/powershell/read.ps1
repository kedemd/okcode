# stdin: root, then one relative path per field.
# stdout: "@i" then one base64 line per readable file. Missing or unreadable
# files are SKIPPED, not fatal — one absent file must not lose the batch.
$i = 0
while ($null -ne ($p = NextField)) {
    $f = P $p
    if ([IO.File]::Exists($f)) {
        try { $b = [Convert]::ToBase64String([IO.File]::ReadAllBytes($f)); W "@$i"; W $b } catch {}
    }
    $i++
}
