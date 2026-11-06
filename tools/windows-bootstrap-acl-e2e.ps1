# Fast isolated PowerShell 5.1 ACL contract before compiling the receiver.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -and $env:USB_MOST_WINDOWS_VM_LAB -ne '1') { throw 'Requires disposable Windows VM' }
. (Join-Path $PSScriptRoot '../native/usb-most-connection/src/bootstrap/protection-windows.ps1')
$path = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) ('USBMost-acl-fixture-' + [Guid]::NewGuid().ToString('N'))
$acl = New-Object Security.AccessControl.DirectorySecurity
$acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
$null = [IO.Directory]::CreateDirectory($path, $acl)
try {
    $actual = Get-Acl -LiteralPath $path
    Write-Output ('fixture_acl=' + $actual.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::All))
    Write-Output ('fixture_protected=' + $actual.AreAccessRulesProtected + ';owner=' + $actual.GetOwner([Security.Principal.SecurityIdentifier]).Value)
    foreach ($rule in $actual.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        Write-Output ('fixture_ace=' + $rule.IdentityReference.Value + ';rights=' + $rule.FileSystemRights + ';inheritance=' + $rule.InheritanceFlags + ';propagation=' + $rule.PropagationFlags + ';type=' + $rule.AccessControlType)
    }
    if (!(Protected $path $true)) { throw 'Actual .NET Framework directory fails product ACL predicate' }
    Write-Output 'PASS bootstrap ACL creation and validation under Windows PowerShell 5.1'
} finally { [IO.Directory]::Delete($path, $false) }

# Reproduce the exact cmd.exe -> fixed UTF8 ScriptBlock context before a long build.
# Invalid MZ bytes reach Process.Start but cannot execute any application.
$source = [IO.File]::ReadAllText((Join-Path $PSScriptRoot '../native/usb-most-connection/src/bootstrap/protection-windows.ps1')) + [IO.File]::ReadAllText((Join-Path $PSScriptRoot '../native/usb-most-connection/src/bootstrap/install-windows.ps1'))
$encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($source))
$command = 'powershell.exe -NoLogo -NoProfile -NonInteractive -Command "& ([ScriptBlock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(''' + $encoded + '''))))"'
if ($command.Length -ge 8191) { throw 'Bootstrap exceeds cmd.exe bound' }
$info = New-Object Diagnostics.ProcessStartInfo
$info.FileName = 'cmd.exe'
$info.Arguments = '/d /s /c "' + $command + '"'
$info.UseShellExecute = $false
$info.RedirectStandardInput = $true
$info.RedirectStandardOutput = $true
$info.RedirectStandardError = $true
$child = [Diagnostics.Process]::Start($info)
try {
    $output = $child.StandardOutput.ReadToEndAsync()
    $errors = $child.StandardError.ReadToEndAsync()
    $bytes = [Text.Encoding]::ASCII.GetBytes('MZ')
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $hash = ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
    $config = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes('{"transport":"direct","port":43333}'))
    $child.StandardInput.Write("UMBOOT1`n$hash`n" + [Convert]::ToBase64String($bytes) + "`n$config`n")
    $child.StandardInput.Close()
    if (!$child.WaitForExit(30000)) { throw 'Bootstrap shell context timeout' }
    $stdout = $output.GetAwaiter().GetResult()
    $stderr = $errors.GetAwaiter().GetResult()
    if ($child.ExitCode -ne 73 -or $stderr -notmatch 'bootstrap_execute:') { throw ('Bootstrap shell context rejected before executable validation: exit=' + $child.ExitCode + ';stderr=' + $stderr) }
    Write-Output 'PASS bootstrap cmd.exe/ScriptBlock context reaches invalid executable rejection'
} finally {
    if (!$child.HasExited) { $child.Kill(); $child.WaitForExit() }
    $child.Dispose()
}

# Byte-for-byte child stdin contract: bypassing a StreamWriter must not append
# its encoding preamble when Close flushes it. This tiny owned fixture never
# installs a service or touches any USB device.
$fixture = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) ('USBMost-stdin-fixture-' + [Guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($fixture, $acl)
try {
    $exe = Join-Path $fixture 'stdin-check.exe'
    Add-Type -OutputAssembly $exe -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
using System.Text;
public class StdinCheck {
    public static int Main(string[] args) {
        if (args.Length != 1 || args[0] != "--install-stdin") return 60;
        byte[] expected = Encoding.UTF8.GetBytes("{\"transport\":\"direct\",\"port\":43333}");
        using (MemoryStream bytes = new MemoryStream()) {
            Console.OpenStandardInput().CopyTo(bytes);
            byte[] actual = bytes.ToArray();
            if (actual.Length != expected.Length) {
                Console.Error.WriteLine("USB Most setup: fixture_stdin_length");
                Console.Error.WriteLine("USB Most driver code: " + actual.Length);
                return 61;
            }
            for (int i = 0; i < actual.Length; i++) if (actual[i] != expected[i]) {
                Console.Error.WriteLine("USB Most setup: fixture_stdin_bytes");
                return 62;
            }
        }
        return 0;
    }
}
'@
    $bytes = [IO.File]::ReadAllBytes($exe)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $hash = ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
    $child = [Diagnostics.Process]::Start($info)
    try {
        $output = $child.StandardOutput.ReadToEndAsync()
        $errors = $child.StandardError.ReadToEndAsync()
        $child.StandardInput.Write("UMBOOT1`n$hash`n" + [Convert]::ToBase64String($bytes) + "`n$config`n")
        $child.StandardInput.Close()
        if (!$child.WaitForExit(30000)) { throw 'Stdin child fixture timeout' }
        $stdout = $output.GetAwaiter().GetResult(); $stderr = $errors.GetAwaiter().GetResult()
        if ($child.ExitCode -ne 0) { throw ('Bootstrap child stdin differs from decoded input: exit=' + $child.ExitCode + ';stderr=' + $stderr) }
        Write-Output 'PASS Windows PowerShell bootstrap forwards exact configuration bytes to native child'
    } finally { if (!$child.HasExited) { $child.Kill(); $child.WaitForExit() }; $child.Dispose() }
} finally { [IO.File]::Delete((Join-Path $fixture 'stdin-check.exe')); [IO.Directory]::Delete($fixture, $false) }
