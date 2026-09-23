# Every field on stdin is one line of base64 (UTF-8 inside), the first being the
# workspace root. Base64 keeps stdin pure ASCII, so a console codepage cannot
# corrupt a path or payload; nothing is ever interpolated into these scripts.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$stdin = [Console]::In
function D([string]$s) { [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($s)) }
function W([string]$s) { [Console]::Out.Write($s + "`n") }
function NextField { $l = $stdin.ReadLine(); if ($null -eq $l) { return $null }; return (D $l) }
$root = NextField
if ($null -eq $root -or -not [IO.Directory]::Exists($root)) { [Console]::Error.WriteLine("okcode: workspace root not found: $root"); exit 3 }
$root = [IO.Path]::GetFullPath($root)
function P([string]$rel) { [IO.Path]::GetFullPath([IO.Path]::Combine($root, $rel)) }
function H([string]$p) { (Get-FileHash -Algorithm SHA1 -LiteralPath $p).Hash }
