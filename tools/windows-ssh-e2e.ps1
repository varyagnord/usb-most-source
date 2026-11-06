# Actual embedded client against Windows OpenSSH on an isolated hosted VM.
param([string]$Receiver, [switch]$PreflightOnly)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -and $env:USB_MOST_WINDOWS_VM_LAB -ne '1') { throw 'Isolated Windows VM required' }
. (Join-Path $PSScriptRoot 'lab-windows-process.ps1')
$root = Split-Path -Parent $PSScriptRoot
$lab = Join-Path $env:ProgramData ('USBMost-ssh-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $lab | Out-Null
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
Set-Acl -LiteralPath $lab -AclObject $acl
$sshd = Join-Path $env:SystemRoot 'System32/OpenSSH/sshd.exe'
if (-not (Test-Path -LiteralPath $sshd)) {
    $capability = Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
    if ($capability.RestartNeeded) { throw 'OpenSSH fixture requires reboot' }
}
if (-not (Test-Path -LiteralPath $sshd)) { throw 'Built-in Windows OpenSSH server unavailable' }
$keygen = Join-Path $env:SystemRoot 'System32/OpenSSH/ssh-keygen.exe'
foreach ($name in @('client', 'host')) {
    & $keygen -q -t ed25519 -N '' -f (Join-Path $lab $name)
    if ($LASTEXITCODE) { throw 'Fixture key generation failed' }
}
$user = 'usbmostsshfixture'
if (Get-LocalUser -Name $user -ErrorAction SilentlyContinue) { throw 'Fixture account already exists' }
$password = ConvertTo-SecureString ([Guid]::NewGuid().ToString('N') + 'aA1!') -AsPlainText -Force
$administrators = Get-LocalGroup -SID 'S-1-5-32-544'
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop()
$forwardPath = $lab.Replace('\', '/')
$logLevel = if ($PreflightOnly) { 'DEBUG1' } else { 'ERROR' }
$config = @"
Port $port
ListenAddress 127.0.0.1
HostKey $forwardPath/host
AuthorizedKeysFile $forwardPath/client.pub
AllowUsers $user
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowTcpForwarding remote
GatewayPorts no
PermitTTY no
AllowAgentForwarding no
LogLevel $logLevel
"@
[IO.File]::WriteAllText((Join-Path $lab 'sshd_config'), $config, [Text.UTF8Encoding]::new($false))
# ssh-keygen adds its invoking account's ACE, which is correct for client keys
# but not for a machine host key consumed by the SYSTEM service. Apply the
# documented machine-key/administrator-authorized-key ACL explicitly.
foreach ($name in @('host','client.pub','sshd_config')) {
    $fileAcl = [Security.AccessControl.FileSecurity]::new()
    $fileAcl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)')
    Set-Acl -LiteralPath (Join-Path $lab $name) -AclObject $fileAcl
}
& $sshd -t -f (Join-Path $lab 'sshd_config')
if ($LASTEXITCODE) { throw 'Invalid isolated OpenSSH configuration' }
$serviceName = 'sshd' # Windows OpenSSH registers this fixed SCM service name.
$original = Get-CimInstance Win32_Service -Filter "Name='sshd'"
if ($original -and $original.State -notin @('Stopped','Running')) { throw 'SSH fixture service is transitioning' }
$wasRunning = $original -and $original.State -eq 'Running'
if ($wasRunning) {
    $ports = @(Get-LabListeningPorts -OwnerProcessId $original.ProcessId)
    if (!$ports.Count) { throw 'Cannot confirm running SSH service listener ownership' }
    $active = @([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpConnections() |
        Where-Object { $_.State -eq [Net.NetworkInformation.TcpState]::Established -and $_.LocalEndPoint.Port -in $ports })
    if ($active.Count) { throw 'Refuse to interrupt an established SSH connection' }
}
$fixturePath = '"' + $sshd + '" -f "' + (Join-Path $lab 'sshd_config') + '" -E "' + (Join-Path $lab 'sshd.log') + '"'
$userCreated = $false
$serviceChanged = $false
$failed = $true
try {
    New-LocalUser -Name $user -Password $password -AccountNeverExpires | Out-Null
    $userCreated = $true
    Add-LocalGroupMember -Group $administrators -Member $user
    $serviceChanged = $true
    if ($wasRunning) { Stop-Service $serviceName; (Get-Service $serviceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30)) }
    if ($original) {
        $changed = Invoke-CimMethod -InputObject $original -MethodName Change -Arguments @{PathName=$fixturePath; StartMode='Manual'}
        if ($changed.ReturnValue -ne 0) { throw 'Could not configure isolated SSH fixture' }
    } else { New-Service -Name $serviceName -BinaryPathName $fixturePath -StartupType Manual | Out-Null }
    Start-Service $serviceName
    $known = '[127.0.0.1]:' + $port + ' ' + [IO.File]::ReadAllText((Join-Path $lab 'host.pub')).Trim() + "`n"
    [IO.File]::WriteAllText((Join-Path $lab 'known_hosts'), $known, [Text.UTF8Encoding]::new($false))
    $ssh = Join-Path $env:SystemRoot 'System32/OpenSSH/ssh.exe'
    $probe = Invoke-LabProcess -FilePath $ssh -Arguments @(
        '-n', '-v', '-E', (Join-Path $lab 'client.log'), '-F', 'NUL',
        '-i', (Join-Path $lab 'client'), '-p', [string]$port,
        '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes',
        '-o', ('UserKnownHostsFile=' + (Join-Path $lab 'known_hosts')),
        '-o', 'ConnectTimeout=10', '-o', 'ServerAliveInterval=10', '-o', 'ServerAliveCountMax=6',
        ($user + '@127.0.0.1'), 'cmd.exe /d /c echo USB_MOST_FIXTURE_READY'
    )
    if ($probe.ExitCode -ne 0 -or $probe.Output.Trim() -ne 'USB_MOST_FIXTURE_READY') {
        throw 'Windows OpenSSH fixture authentication/exec preflight failed'
    }
    $metadata = @{scope='isolated-Windows-OpenSSH-only'; port=$port; user=$user} | ConvertTo-Json -Compress
    [IO.File]::WriteAllText((Join-Path $lab 'fixture.json'), $metadata, [Text.UTF8Encoding]::new($false))
    if ($PreflightOnly) {
        cargo run --locked --release --target x86_64-pc-windows-msvc -p usb-most-connection --example windows_ssh_lab -- $lab --probe-only
        if ($LASTEXITCODE) { throw 'Embedded SSH platform/probe preflight failed' }
        $failed = $false
        Write-Output 'PASS Windows OpenSSH fixture service/public-key/cmd and embedded platform/probe preflight'
        return
    }
    if (-not $Receiver -or -not (Test-Path -LiteralPath $Receiver)) { throw 'Tested receiver executable required' }
    $artifacts = Join-Path $lab 'receivers'
    New-Item -ItemType Directory -Path $artifacts | Out-Null
    $name = 'usb-most-windows-x86_64.exe'
    Copy-Item -LiteralPath $Receiver -Destination (Join-Path $artifacts $name)
    $manifest = @{version=(& $Receiver --version).Trim(); artifacts=@(@{name=$name; bytes=(Get-Item $Receiver).Length; sha256=(Get-FileHash $Receiver -Algorithm SHA256).Hash.ToLowerInvariant()})}
    [IO.File]::WriteAllText((Join-Path $artifacts 'manifest.json'), ($manifest | ConvertTo-Json -Compress -Depth 4), [Text.UTF8Encoding]::new($false))
    cargo run --locked --release --target x86_64-pc-windows-msvc -p usb-most-connection --example windows_ssh_lab -- $lab $artifacts
    if ($LASTEXITCODE) { throw 'Embedded Windows SSH/service/HID scenario failed' }
    $failed = $false
} finally {
    $service = Get-Service $serviceName -ErrorAction SilentlyContinue
    if ($service -and $serviceChanged) {
        if ($service.Status -ne 'Stopped') { Stop-Service $serviceName; $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30)) }
        if ($failed) {
            foreach ($name in @('sshd.log','client.log')) {
                $path = Join-Path $lab $name
                if (Test-Path -LiteralPath $path) {
                    $stream = $null; $reader = $null
                    try {
                        # sshd session children can retain the file after the
                        # SCM parent exits. Read diagnostics with shared access.
                        $stream = [IO.File]::Open($path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
                            [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
                        if ($stream.Length -gt 65536) { [void]$stream.Seek(-65536, [IO.SeekOrigin]::End) }
                        $reader = [IO.StreamReader]::new($stream)
                        $reader.ReadToEnd() -split "`n" | Select-Object -Last 70 | Write-Output
                    } catch { Write-Warning ('Fixture diagnostic read failed: ' + $_.Exception.GetType().Name) }
                    finally { if ($reader) { $reader.Dispose() } elseif ($stream) { $stream.Dispose() } }
                }
            }
        }
        if ($original) {
            $current = Get-CimInstance Win32_Service -Filter "Name='sshd'"
            $mode = if ($original.StartMode -eq 'Auto') { 'Automatic' } else { $original.StartMode }
            $restored = Invoke-CimMethod -InputObject $current -MethodName Change -Arguments @{PathName=$original.PathName; StartMode=$mode}
            if ($restored.ReturnValue -ne 0) { throw 'Could not restore original SSH fixture service' }
            if ($wasRunning) { Start-Service $serviceName }
        } else { & "$env:SystemRoot/System32/sc.exe" delete $serviceName | Out-Null }
    }
    if ($userCreated) { Remove-LocalUser -Name $user }
    # Keep owned lab files for private diagnosis; no recursive cleanup or uploads.
}
