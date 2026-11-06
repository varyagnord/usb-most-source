# Exercise actual child execution/EOF/deadline, including an unrelated process.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'lab-windows-process.ps1')
$pwsh = Join-Path $PSHOME 'pwsh.exe'
$prefix = @('-NoProfile', '-NonInteractive', '-Command')
$result = Invoke-LabProcess $pwsh ($prefix + @('[Console]::Out.Write("ok"); [Console]::Error.Write("error"); exit 7'))
if ($result.ExitCode -ne 7 -or $result.Output -cne 'ok' -or $result.ErrorOutput -cne 'error') { throw 'Exit code or streams lost' }
$result = Invoke-LabProcess $pwsh ($prefix + @('if ([Console]::In.ReadToEnd().Length -ne 0) { exit 2 }; [Console]::Out.Write("EOF")'))
if ($result.ExitCode -ne 0 -or $result.Output -cne 'EOF') { throw 'Child stdin was not closed' }
$info = [Diagnostics.ProcessStartInfo]::new($pwsh)
$info.UseShellExecute = $false
foreach ($arg in ($prefix + @('Start-Sleep -Seconds 60'))) { $info.ArgumentList.Add($arg) }
$neighbor = [Diagnostics.Process]::Start($info)
try {
    $timedOut = $false
    $watch = [Diagnostics.Stopwatch]::StartNew()
    try { Invoke-LabProcess $pwsh ($prefix + @('Start-Sleep -Seconds 60')) -TimeoutMilliseconds 500 | Out-Null }
    catch [TimeoutException] { $timedOut = $true }
    if (!$timedOut -or $watch.Elapsed.TotalSeconds -gt 15) { throw 'Execution deadline did not bound the child' }
    if ($neighbor.HasExited) { throw 'Unrelated child was stopped' }
} finally {
    if (!$neighbor.HasExited) { $neighbor.Kill(); [void]$neighbor.WaitForExit(5000) }
    $neighbor.Dispose()
}
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$client = [Net.Sockets.TcpClient]::new()
$accepted = $null
try {
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    if ($port -notin @(Get-LabListeningPorts -OwnerProcessId $PID)) { throw 'Native listener owner/port missing' }
    if (@(Get-LabListeningPorts -OwnerProcessId 2147483647).Count) { throw 'Foreign owner admitted' }
    $client.Connect([Net.IPAddress]::Loopback, $port)
    $accepted = $listener.AcceptTcpClient()
    $connections = [Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpConnections()
    if (!@($connections | Where-Object { $_.LocalEndPoint.Port -eq $port -and $_.State -eq [Net.NetworkInformation.TcpState]::Established }).Count) { throw 'Actual established local connection missing' }
} finally {
    if ($accepted) { $accepted.Dispose() }
    $client.Dispose()
    $listener.Stop()
}
Write-Output 'PASS actual lab child: stdout, stderr, nonzero exit, stdin EOF, bounded timeout, unrelated process retained; native listener ownership and established TCP'
