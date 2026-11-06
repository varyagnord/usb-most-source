# Shared journal + actual SCM + signed stack, only an isolated Windows runner.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -and $env:USB_MOST_WINDOWS_VM_LAB -ne '1') { throw 'Requires an isolated Windows VM' }
$name = 'USBMostReceiver'
$stateRoot = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'USBMost'
$installed = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'USBMost/usb-most-desktop.exe'
if (Get-Service -Name $name -ErrorAction SilentlyContinue) { throw 'Previous service fixture must have stopped and removed its service' }
if (!(Test-Path $installed) -or !(Test-Path (Join-Path $stateRoot 'receiver.json'))) { throw 'Run the SCM lifecycle fixture first' }
cargo test --locked -p usb-most-desktop deployment::windows::tests
if ($LASTEXITCODE) { throw 'Windows transaction fault/lock tests failed' }
cargo build --locked --release --features installation -p usb-most-desktop --target-dir build/rust-target
if ($LASTEXITCODE) { throw 'Installer build failed' }
$installer = (Resolve-Path (Join-Path $PSScriptRoot '../build/rust-target/release/usb-most-desktop.exe')).Path
function Install([string]$configuration, [bool]$success) {
    $info = [Diagnostics.ProcessStartInfo]::new($installer)
    $info.ArgumentList.Add('--install-stdin')
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $proc = [Diagnostics.Process]::Start($info)
    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()
    $proc.StandardInput.Write($configuration); $proc.StandardInput.Close()
    if (!$proc.WaitForExit(240000)) { $proc.Kill(); $proc.WaitForExit(); throw 'Installer timed out; retaining journal' }
    $output = $outTask.GetAwaiter().GetResult()
    $errorOutput = $errTask.GetAwaiter().GetResult()
    if (($proc.ExitCode -eq 0) -ne $success) { throw "Installer outcome unexpected: $($proc.ExitCode) $errorOutput" }
    if ($success -and ($output.Length -gt 0 -or $errorOutput.Length -gt 0)) { throw 'Unexpected installer output' }
    $proc.Dispose()
}
function Firewall([int]$port) {
    $tls = Get-NetFirewallRule -DisplayName 'USB Most Receiver TLS v1' -ErrorAction Stop
    $mdns = Get-NetFirewallRule -DisplayName 'USB Most Receiver Discovery v1' -ErrorAction Stop
    if (@($tls).Count -ne 1 -or @($mdns).Count -ne 1) { throw 'Ambiguous product firewall rule' }
    if (($tls | Get-NetFirewallPortFilter).LocalPort -ne [string]$port -or ($mdns | Get-NetFirewallPortFilter).LocalPort -ne '5353') { throw 'Firewall port not restored' }
    foreach ($rule in @($tls, $mdns)) {
        if ([int]$rule.Profile -ne 3 -or ($rule | Get-NetFirewallAddressFilter).RemoteAddress -ne 'LocalSubnet') { throw 'Broad firewall exception' }
        if (($rule | Get-NetFirewallApplicationFilter).Program -ne $installed.Replace('/', '\') -or ($rule | Get-NetFirewallServiceFilter).Service -ne $name) { throw 'Firewall application/service mismatch' }
    }
}
function FreePort {
    $p = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $p.Start(); $value = $p.LocalEndpoint.Port; $p.Stop(); return $value
}
function Config([int]$port) { return ('{"transport":"direct","port":' + $port + '}') }
function Hash([string]$path) { return (Get-FileHash -Algorithm SHA256 $path).Hash }
function Healthy {
    $data = & $installed --health
    if ($LASTEXITCODE) { throw 'Authenticated receiver health failed' }
    $value = $data | ConvertFrom-Json
    if ($value.result -ne 'health' -or !$value.status.stack) { throw 'Receiver not ready' }
}
function OwnService {
    $value = Get-CimInstance Win32_Service -Filter "Name='$name'"
    if (!$value -or $value.PathName -ne ('"' + $installed.Replace('/', '\') + '" --service')) { throw 'Service ownership mismatch' }
    return $value
}
$configPath = Join-Path $stateRoot 'receiver.json'
$activePath = Join-Path $stateRoot 'active'
$beforeBinary = Hash $installed
$beforeConfig = Hash $configPath
$busy = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Any, 0)
$busy.Start(); $busyPort = $busy.LocalEndpoint.Port
try {
    # Failed first install must remove only the new service and restore old files.
    Install (Config $busyPort) $false
    if (Get-Service -Name $name -ErrorAction SilentlyContinue) { throw 'Failed fresh install leaked service' }
    if ((Hash $installed) -ne $beforeBinary -or (Hash $configPath) -ne $beforeConfig -or (Test-Path $activePath)) { throw 'Fresh failure changed prior files' }
    if (Get-NetFirewallRule -DisplayName 'USB Most Receiver TLS v1' -ErrorAction SilentlyContinue) { throw 'Cold failure leaked TLS rule' }
    if (Get-NetFirewallRule -DisplayName 'USB Most Receiver Discovery v1' -ErrorAction SilentlyContinue) { throw 'Cold failure leaked discovery rule' }
    $port = FreePort
    Install (Config $port) $true
    Healthy
    Firewall $port
    $service = OwnService
    if ($service.StartMode -ne 'Auto' -or $service.State -ne 'Running') { throw 'Missing automatic startup' }
    $processId = $service.ProcessId
    $binaryTime = (Get-Item $installed).LastWriteTimeUtc
    $configTime = (Get-Item $configPath).LastWriteTimeUtc
    Install (Config $port) $true
    Healthy
    if ((OwnService).ProcessId -ne $processId -or (Get-Item $installed).LastWriteTimeUtc -ne $binaryTime -or (Get-Item $configPath).LastWriteTimeUtc -ne $configTime) { throw 'Identical install restarted or rewrote the receiver' }
    # Repair a deleted owned rule even when files and activation are unchanged.
    Get-NetFirewallRule -DisplayName 'USB Most Receiver TLS v1' | Remove-NetFirewallRule
    Install (Config $port) $true
    Healthy
    Firewall $port
    $processId = (OwnService).ProcessId
    $nextPort = FreePort
    Install (Config $nextPort) $true
    Healthy
    if ((OwnService).ProcessId -eq $processId -or (Get-Item $installed).LastWriteTimeUtc -ne $binaryTime) { throw 'Configuration update did not reuse binary/restart service' }
    Firewall $nextPort
    $goodConfig = Hash $configPath
    $goodBinary = Hash $installed
    $goodActive = Hash $activePath
    Install (Config $busyPort) $false
    Healthy
    if ((Hash $configPath) -ne $goodConfig -or (Hash $installed) -ne $goodBinary -or (Hash $activePath) -ne $goodActive -or (OwnService).StartMode -ne 'Auto') { throw 'Failed update did not restore prior files/service' }
    Firewall $nextPort
    # Restore both manual and disabled services exactly after failed updates.
    foreach ($mode in @('demand', 'disabled')) {
        Stop-Service $name
        (Get-Service $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(120))
        & "$env:SystemRoot\System32\sc.exe" config $name start= $mode | Out-Null
        if ($LASTEXITCODE) { throw 'Cannot configure fixture start type' }
        $previousMode = (OwnService).StartMode
        Install (Config $busyPort) $false
        if ((OwnService).StartMode -ne $previousMode -or (OwnService).State -ne 'Stopped' -or (Hash $configPath) -ne $goodConfig) { throw 'Startup mode was lost on rollback' }
    }
    Install (Config $nextPort) $true
    Healthy
    if ((OwnService).StartMode -ne 'Auto') { throw 'Successful preparation did not enable receiver' }
    if (Test-Path (Join-Path $stateRoot 'install-journal.json')) { throw 'Successful commit left unfinished journal' }
    Firewall $nextPort
    Write-Output 'PASS Windows installer: file fault/crash boundaries, cold failure, install/health, exact no-op, config-only update, rollback files/activation/service, manual/disabled recovery'
} finally {
    $busy.Stop()
    if (Get-Service -Name $name -ErrorAction SilentlyContinue) {
        $null = OwnService
        if ((Get-Service $name).Status -ne 'Stopped') {
            Stop-Service $name
            (Get-Service $name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(120))
        }
        & "$env:SystemRoot\System32\sc.exe" delete $name
        if ($LASTEXITCODE) { throw 'Owned service cleanup failed' }
    }
    # Keep journals/snapshots on failure. No recursive file removal.
}
