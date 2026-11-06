$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$nonce = [Console]::In.ReadLine()
if ($null -eq $nonce -or $nonce -cnotmatch '^[0-9a-f]{32}$') { exit 70 }
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { exit 72 }
$machine = $env:PROCESSOR_ARCHITECTURE
if ($env:PROCESSOR_ARCHITEW6432) { $machine = $env:PROCESSOR_ARCHITEW6432 }
switch ($machine.ToUpperInvariant()) {
    'AMD64' { $arch = 'x86_64' }
    'ARM64' { $arch = 'aarch64' }
    default { exit 72 }
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
$elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
# Query the local service manager directly; detecting one driver must not depend
# on WMI provider startup or remote-management configuration.
Add-Type -AssemblyName System.ServiceProcess
$drivers = [ServiceProcess.ServiceController]::GetDevices()
$stack = $false
try {
    foreach ($driver in $drivers) {
        if ($driver.ServiceName -eq 'usbip2_ude') {
            $stack = ($driver.Status -eq [ServiceProcess.ServiceControllerStatus]::Running)
            break
        }
    }
} finally { foreach ($driver in $drivers) { $driver.Dispose() } }
$directory = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'USBMost'
$binary = Join-Path $directory 'usb-most-desktop.exe'
$version = $null
$hash = $null
if ((Protected $directory $true) -and (Protected $binary $false)) {
    $hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $binary; $info.Arguments = '--version'; $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    $child = [Diagnostics.Process]::Start($info)
    $output = $child.StandardOutput.ReadToEndAsync()
    $errors = $child.StandardError.ReadToEndAsync()
    if (!$child.WaitForExit(5000)) { $child.Kill(); $child.WaitForExit(); exit 75 }
    $candidate = $output.GetAwaiter().GetResult().Trim()
    $null = $errors.GetAwaiter().GetResult()
    if ($child.ExitCode -eq 0 -and $candidate -cmatch '^[0-9A-Za-z.-]{1,32}$') { $version = $candidate }
    $child.Dispose()
}
$service = Get-Service -Name 'USBMostReceiver' -ErrorAction SilentlyContinue
$running = ($null -ne $service -and $service.Status -eq 'Running')
$reboot = (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') -or
    (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired')
[ordered]@{
    schema = 1; nonce = $nonce; os = 'windows'; arch = $arch;
    elevated = [bool]$elevated; usb_stack = [bool]$stack; agent_version = $version; agent_sha256 = $hash;
    service_running = [bool]$running; reboot_pending = [bool]$reboot
} | ConvertTo-Json -Compress
