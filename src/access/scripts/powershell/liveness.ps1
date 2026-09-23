# stdin: root, pid. stdout: "HOST <name>" then "ALIVE <start|->" | "DEAD" | "UNKNOWN".
# The host is the TARGET's ([Net.Dns]::GetHostName matches Node's os.hostname).
$target = NextField
W ('HOST ' + [Net.Dns]::GetHostName())
$n = 0
if (-not [int]::TryParse($target, [ref]$n)) { W 'UNKNOWN'; exit 0 }
$proc = $null
try { $proc = Get-Process -Id $n -ErrorAction Stop } catch {}
if ($proc) { $st = '-'; try { $st = $proc.StartTime.ToFileTimeUtc() } catch {}; W "ALIVE $st" } else { W 'DEAD' }
