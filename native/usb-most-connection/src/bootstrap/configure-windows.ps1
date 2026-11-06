$ErrorActionPreference = 'Stop'
try {
    $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    if (!$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 77 }
    $magic = [Console]::In.ReadLine(); $expected = [Console]::In.ReadLine(); $encoded = [Console]::In.ReadLine()
    if ($magic -cne 'UMCONF1' -or $expected -cnotmatch '^[0-9a-f]{64}$' -or !$encoded -or $encoded.Length -gt 5464 -or $null -ne [Console]::In.ReadLine()) { exit 70 }
    $directory = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'USBMost'
    $binary = Join-Path $directory 'usb-most-desktop.exe'
    if (!(Protected $directory $true) -or !(Protected $binary $false)) { exit 78 }
    if ((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expected) { exit 78 }
    $config = [Convert]::FromBase64String($encoded)
    if ($config.Length -gt 4096) { exit 70 }
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $binary; $info.Arguments = '--install-stdin'; $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    [Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
    $child = [Diagnostics.Process]::Start($info)
    $output = $child.StandardOutput.ReadToEndAsync(); $errors = $child.StandardError.ReadToEndAsync()
    try {
        $child.StandardInput.BaseStream.Write($config, 0, $config.Length); $child.StandardInput.Close()
        [Array]::Clear($config, 0, $config.Length)
        if (!$child.WaitForExit(600000)) { $child.Kill(); $child.WaitForExit(); exit 75 }
        $null = $output.GetAwaiter().GetResult(); $diagnostic = $errors.GetAwaiter().GetResult()
        foreach ($line in ($diagnostic -split "`n" | Select-Object -First 16)) { if ($line.Trim() -cmatch '^USB Most(?:: [A-Za-z]+ os=-?[0-9]+| setup: [a-z_]+| driver code: -?[0-9]+)$') { [Console]::Error.WriteLine($line.Trim()) } }
        if ($child.ExitCode -eq 76) { exit 76 }
        if ($child.ExitCode -ne 0) { exit 79 }
    } finally { if (!$child.HasExited) { $child.Kill(); $child.WaitForExit() }; $child.Dispose() }
    exit 0
} catch { exit 79 }
