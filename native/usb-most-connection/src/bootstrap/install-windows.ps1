# Fixed stdin-only bootstrap.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$stage = $null
$owned = $false
$config = $null
$phase = 'input'
try {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 77 }
    $magic = [Console]::In.ReadLine()
    $expected = [Console]::In.ReadLine()
    $binary = [Console]::In.ReadLine()
    $configuration = [Console]::In.ReadLine()
    if ($magic -cne 'UMBOOT1' -or $expected -cnotmatch '^[0-9a-f]{64}$' -or !$binary -or $binary.Length -gt 27962028 -or !$configuration -or $configuration.Length -gt 5464 -or $null -ne [Console]::In.ReadLine()) { exit 70 }
    $bytes = [Convert]::FromBase64String($binary); $binary = $null
    $config = [Convert]::FromBase64String($configuration); $configuration = $null
    if ($bytes.Length -eq 0 -or $bytes.Length -gt 20971520 -or $config.Length -gt 4096) { exit 70 }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $actual = ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }
    if ($actual -cne $expected) { exit 74 }
    $phase = 'stage'
    $stage = Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) ('USBMost-stage-' + [Guid]::NewGuid().ToString('N'))
    if (Test-Path -LiteralPath $stage) { exit 73 }
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
    $null = [IO.Directory]::CreateDirectory($stage, $acl)
    $item = Get-Item -LiteralPath $stage -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { exit 73 }
    if (!(Protected $stage $true)) { [Console]::Error.WriteLine('bootstrap_stage_acl:' + $script:protectionFailure); exit 73 }
    $owned = $true
    $phase = 'write'
    $receiver = Join-Path $stage 'receiver.exe'
    $file = [IO.File]::Open($receiver, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try { $file.Write($bytes, 0, $bytes.Length); $file.Flush($true) } finally { $file.Dispose(); [Array]::Clear($bytes, 0, $bytes.Length) }
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $receiver; $info.Arguments = '--install-stdin'; $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    $phase = 'execute'
    [Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
    $child = [Diagnostics.Process]::Start($info)
    $output = $child.StandardOutput.ReadToEndAsync(); $errors = $child.StandardError.ReadToEndAsync()
    try {
        $child.StandardInput.BaseStream.Write($config, 0, $config.Length)
        $child.StandardInput.Close(); [Array]::Clear($config, 0, $config.Length)
        if (!$child.WaitForExit(600000)) { $child.Kill(); $child.WaitForExit(); exit 75 }
        $null = $output.GetAwaiter().GetResult(); $diagnostic = $errors.GetAwaiter().GetResult()
        foreach ($line in ($diagnostic -split "`n" | Select-Object -First 16)) { if ($line.Trim() -cmatch '^USB Most(?:: [A-Za-z]+ os=-?[0-9]+| setup: [a-z_]+| driver code: -?[0-9]+)$') { [Console]::Error.WriteLine($line.Trim()) } }
        if ($child.ExitCode -eq 76) { exit 76 }
        if ($child.ExitCode -ne 0) { exit 79 }
    } finally { if (!$child.HasExited) { $child.Kill(); $child.WaitForExit() }; $child.Dispose() }
    exit 0
} catch { [Console]::Error.WriteLine('bootstrap_' + $phase + ':' + $_.Exception.GetType().Name); exit 73 }
finally {
    if ($null -ne $config) { [Array]::Clear($config, 0, $config.Length) }
    if ($owned -and $null -ne $stage -and (Test-Path -LiteralPath $stage -PathType Container)) {
        # Exact owned files only; never recursive.
        try { [IO.File]::Delete((Join-Path $stage 'receiver.exe')); [IO.Directory]::Delete($stage, $false) } catch { }
    }
}
