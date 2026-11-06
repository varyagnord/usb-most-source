# Owned VM only: temporarily allow remote forwarding for the existing pinned
# fixture account, then restore the exact management configuration. No new user,
# key, listener, firewall exception or persistent SSH policy is installed.
param([Parameter(Mandatory)][ValidatePattern('^[a-f0-9]{32}$')][string]$Nonce)
$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'
if ($env:USB_MOST_WINDOWS_VM_LAB -ne '1' -or (Get-CimInstance Win32_ComputerSystem).Manufacturer -notmatch 'QEMU') { throw 'Explicit owned Windows QEMU required' }
$directory=Join-Path 'C:\USBMostLab' ('android-forwarding-'+$Nonce)
$acl=Get-Acl -LiteralPath $directory
if (!$acl.AreAccessRulesProtected -or $acl.Access.Count -ne 2) { throw 'Private fixture directory required' }
$sshd=Join-Path $env:SystemRoot 'System32\OpenSSH\sshd.exe'
$service=Get-CimInstance Win32_Service -Filter "Name='sshd'"
if ($service.State -ne 'Running' -or $service.PathName.Trim('"') -ine $sshd) { throw 'Original management service required' }
$configuration=Join-Path $env:ProgramData 'ssh\sshd_config'
$original=[IO.File]::ReadAllBytes($configuration)
$originalHash=(Get-FileHash -LiteralPath $configuration).Hash.ToLowerInvariant()
$text=[Text.Encoding]::UTF8.GetString($original)
$pattern=[regex]'(?m)^AllowTcpForwarding no(?=\r?$)'
if ($pattern.Matches($text).Count -ne 1 -or $text -notmatch '(?m)^Port 22\r?$') { throw 'Expected fixed management policy required' }
[IO.File]::WriteAllBytes((Join-Path $directory 'sshd_config.original'),$original)
$state=@{nonce=$Nonce;status='starting';original_sha256=$originalHash;restored=$false}
function Phase([string]$name) {
 $state.status=$name
 [IO.File]::WriteAllText((Join-Path $directory 'state.json'),($state|ConvertTo-Json -Compress),[Text.UTF8Encoding]::new($false))
 $port=[IO.Ports.SerialPort]::new('COM1',115200);$port.WriteTimeout=10000
 try {$port.Open();$port.WriteLine('USBMOST_ANDROID_FORWARDING '+[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($state|ConvertTo-Json -Compress))))} finally {$port.Dispose()}
}
function Wait-Closed {
 $end=[DateTime]::UtcNow.AddSeconds(120)
 while (@([Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpConnections()|Where-Object {$_.LocalEndPoint.Port -eq 22 -and $_.State -eq [Net.NetworkInformation.TcpState]::Established}).Count) {
  if([DateTime]::UtcNow -ge $end){throw 'SSH still active; refuse to interrupt it'}
  Start-Sleep -Milliseconds 200
 }
}
$touched=$false
try {
 Wait-Closed
 $touched=$true
 Stop-Service sshd; (Get-Service sshd).WaitForStatus('Stopped',[TimeSpan]::FromSeconds(30))
 [IO.File]::WriteAllBytes($configuration,[Text.UTF8Encoding]::new($false).GetBytes($pattern.Replace($text,'AllowTcpForwarding remote')))
 & $sshd -t
 if($LASTEXITCODE){throw 'Forwarding fixture configuration rejected'}
 Start-Service sshd; (Get-Service sshd).WaitForStatus('Running',[TimeSpan]::FromSeconds(30))
 Phase 'ready'
 $end=[DateTime]::UtcNow.AddMinutes(12)
 while (!(Test-Path -LiteralPath (Join-Path $directory 'finish'))) {
  if([DateTime]::UtcNow -ge $end){throw 'Bounded Android fixture expired'}
  Start-Sleep -Milliseconds 200
 }
} catch {
 $state.failure=$_.Exception.GetType().Name
 throw
} finally {
 if ($touched) {
  Wait-Closed
  Stop-Service sshd; (Get-Service sshd).WaitForStatus('Stopped',[TimeSpan]::FromSeconds(30))
  [IO.File]::WriteAllBytes($configuration,$original)
  & $sshd -t
  if($LASTEXITCODE){throw 'Original management configuration rejected'}
  Start-Service sshd; (Get-Service sshd).WaitForStatus('Running',[TimeSpan]::FromSeconds(30))
  if((Get-FileHash -LiteralPath $configuration).Hash.ToLowerInvariant() -cne $originalHash){throw 'Original management bytes changed'}
  $state.restored=$true
 }
 Phase 'restored'
}
