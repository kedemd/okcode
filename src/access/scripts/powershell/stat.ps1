# stdin: root, want-hash ("1"/"0"), then one relative path per field.
# stdout: "i size ticks [hash]" or "i -" per path, by request index.
# stat before hashing (see the bash dialect for why the order matters).
$want = NextField
$i = 0
while ($null -ne ($p = NextField)) {
    $fi = New-Object IO.FileInfo (P $p)
    if (-not $fi.Exists) { W "$i -" }
    else {
        $line = "$i $($fi.Length) $($fi.LastWriteTimeUtc.Ticks)"
        if ($want -eq '1') { try { $line += ' ' + (H $fi.FullName) } catch { $line = "$i -" } }
        W $line
    }
    $i++
}
