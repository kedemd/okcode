# stdin: root, then one relative path per field. Best effort, never fatal.
while ($null -ne ($p = NextField)) {
    try { $f = P $p; if ([IO.File]::Exists($f)) { [IO.File]::Delete($f) } } catch {}
}
