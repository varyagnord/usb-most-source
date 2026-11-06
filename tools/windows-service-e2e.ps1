# Actual SCM + product receiver in a disposable VM. No physical USB or firewall changes.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -and $env:USB_MOST_WINDOWS_VM_LAB -ne '1') {
    throw 'SCM tests require an explicitly isolated Windows VM'
}
$name = 'USBMostReceiver'
$stateRoot = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'USBMost'
$binaryRoot = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'USBMost'
if ((Get-Service -Name $name -ErrorAction SilentlyContinue) -or (Test-Path $stateRoot) -or (Test-Path $binaryRoot)) {
    throw 'Refusing to replace a pre-existing receiver or its files'
}
function PrivateAcl([bool]$directory) {
    $acl = if ($directory) { [System.Security.AccessControl.DirectorySecurity]::new() } else { [System.Security.AccessControl.FileSecurity]::new() }
    $sddl = if ($directory) { 'O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)' } else { 'O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)' }
    $acl.SetSecurityDescriptorSddlForm($sddl)
    return $acl
}
function WriteConfig([string]$text) {
    $path = Join-Path $stateRoot 'receiver.json'
    [IO.File]::WriteAllText($path, $text, [Text.UTF8Encoding]::new($false))
    Set-Acl -LiteralPath $path -AclObject (PrivateAcl $false)
}
# .NET CreateDirectory extension takes the DACL at creation, not after publication.
[IO.FileSystemAclExtensions]::CreateDirectory((PrivateAcl $true), $stateRoot) | Out-Null
[IO.FileSystemAclExtensions]::CreateDirectory((PrivateAcl $true), $binaryRoot) | Out-Null
cargo build --locked -p usb-most-desktop --target-dir build/rust-target
if ($LASTEXITCODE) { throw 'Receiver build failed' }
$exe = Join-Path $binaryRoot 'usb-most-desktop.exe'
Copy-Item (Join-Path $PSScriptRoot '../build/rust-target/debug/usb-most-desktop.exe') $exe
Set-Acl -LiteralPath $exe -AclObject (PrivateAcl $false)
$probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$probe.Start(); $port = $probe.LocalEndpoint.Port; $probe.Stop()
WriteConfig ('{"transport":"direct","port":' + $port + '}')
$created = $false
try {
    & "$env:SystemRoot\System32\sc.exe" create $name binPath= ('"' + $exe + '" --service') start= demand obj= LocalSystem
    if ($LASTEXITCODE) { throw 'SCM create failed' }
    $created = $true
    $seen = @()
    for ($round = 0; $round -lt 3; $round++) {
        Start-Service $name
        $svc = Get-Service $name
        $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        do {
            $raw = & $exe --health 2>$null
            if ($LASTEXITCODE -eq 0) { break }
            Start-Sleep -Milliseconds 200
        } while ([DateTime]::UtcNow -lt $deadline)
        if ($LASTEXITCODE) { throw 'SCM Running without authenticated health IPC' }
        $health = $raw | ConvertFrom-Json
        if ($health.result -ne 'health' -or !$health.status.stack -or $health.status.active -ne 0 -or $health.status.connected) { throw 'Unexpected idle receiver health' }
        $pidBefore = (Get-CimInstance Win32_Service -Filter "Name='$name'").ProcessId
        if (!$pidBefore -or $seen.Contains($pidBefore)) { throw 'Service did not create a fresh process' }
        $seen += $pidBefore
        & "$env:SystemRoot\System32\sc.exe" interrogate $name | Out-Null
        if ($LASTEXITCODE) { throw 'SCM interrogate failed' }
        Stop-Service $name
        $svc.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(120))
        $proc = Get-Process -Id $pidBefore -ErrorAction SilentlyContinue
        if ($proc -and !$proc.WaitForExit(5000)) { throw 'Stopped status left service process alive' }
        & $exe --health 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { throw 'Old owner IPC remained active after SCM stop' }
        # Listener is actually released, not merely hidden by stale health data.
        $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Any, $port)
        $listener.Start(); $listener.Stop()
    }
    WriteConfig '{"transport":"direct","port":0}'
    try { Start-Service $name; throw 'Invalid configuration reached Running' }
    catch { if ((Get-Service $name).Status -eq 'Running') { throw } }
    (Get-Service $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    $bad = Get-CimInstance Win32_Service -Filter "Name='$name'"
    if ($bad.ExitCode -ne 1066 -or $bad.ServiceSpecificExitCode -eq 0) { throw 'Startup error was not reported to SCM' }
    WriteConfig ('{"transport":"direct","port":' + $port + '}')
    # An occupied socket must fail startup rather than announce a healthy service.
    $busy = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Any, $port)
    $busy.Start()
    try {
        try { Start-Service $name; throw 'Busy listener reached Running' }
        catch { if ((Get-Service $name).Status -eq 'Running') { throw } }
        (Get-Service $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
        if ((Get-CimInstance Win32_Service -Filter "Name='$name'").ExitCode -ne 1066) { throw 'Bind failure not reported to SCM' }
    } finally { $busy.Stop() }
    Start-Service $name
    (Get-Service $name).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
    Stop-Service $name
    (Get-Service $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(120))
    & $exe --service 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { throw 'Service command silently fell back to console mode' }
    Write-Output 'PASS SCM: start/health/interrogate/stop x3, process/listener retirement, bad config, occupied socket, restart after failure, no console fallback'
} finally {
    if ($created) {
        $owned = Get-CimInstance Win32_Service -Filter "Name='$name'"
        if ($owned.PathName -ne ('"' + $exe + '" --service')) { throw 'Service ownership changed; preserving it' }
        if ((Get-Service $name).Status -ne 'Stopped') {
            Stop-Service $name
            (Get-Service $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(120))
        }
        & "$env:SystemRoot\System32\sc.exe" delete $name
        if ($LASTEXITCODE) { throw 'Cannot delete owned test service' }
    }
    # Preserve exact fixture files for diagnosis. No recursive cleanup.
}
