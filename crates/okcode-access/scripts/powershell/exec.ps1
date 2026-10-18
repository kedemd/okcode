# stdin: root, command. stdout: "RC <n>", "@OUT", base64(output), "@ERR", "".
# TRUST BOUNDARY: this evaluates a caller-supplied command in the workspace
# root. exec is a capability the host grants per workspace, not a default.
# PowerShell merges the command's error stream into @OUT.
$cmd = NextField
Set-Location -LiteralPath $root
$ok = $true
$global:LASTEXITCODE = 0
$o = ''
try { $o = (Invoke-Expression $cmd 2>&1 | Out-String) } catch { $ok = $false; $o = [string]$o + ($_ | Out-String) }
if ($LASTEXITCODE -ne 0) { $ok = $false }
$rc = 0
if (-not $ok) { $rc = 1 }
W "RC $rc"
W '@OUT'
W ([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([string]$o)))
W '@ERR'
