# Private/CI lab helper, PowerShell 7. No production runtime dependency.
function Invoke-LabProcess {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @(),
        [ValidateRange(1, 600000)][int]$TimeoutMilliseconds = 90000
    )
    $info = [Diagnostics.ProcessStartInfo]::new($FilePath)
    $info.UseShellExecute = $false
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $child = [Diagnostics.Process]::Start($info)
    try {
        $output = $child.StandardOutput.ReadToEndAsync()
        $errorOutput = $child.StandardError.ReadToEndAsync()
        $child.StandardInput.Close()
        if (!$child.WaitForExit($TimeoutMilliseconds)) {
            # Retained process handle identifies only this child; never kill by
            # name or PID discovered later. The caller's finally restores sshd.
            $child.Kill()
            if (!$child.WaitForExit(5000)) { throw 'Lab child did not exit after timeout; retain diagnostics' }
            throw [TimeoutException]::new('Lab child exceeded its execution deadline; retain diagnostics')
        }
        if (!$output.Wait(5000) -or !$errorOutput.Wait(5000)) {
            throw [TimeoutException]::new('Lab child output remained open after exit; retain diagnostics')
        }
        return [pscustomobject]@{
            ExitCode = $child.ExitCode
            Output = $output.GetAwaiter().GetResult()
            ErrorOutput = $errorOutput.GetAwaiter().GetResult()
        }
    } finally {
        if (!$child.HasExited) { $child.Kill(); [void]$child.WaitForExit(5000) }
        $child.Dispose()
    }
}

# Avoid slow CDXML/CIM provider initialization in disposable Windows images.
# Read numeric endpoints/PIDs from the OS tool; listener state comes from .NET,
# so localized state/header strings are never used for an ownership decision.
function Get-LabListeningPorts {
    param([ValidateRange(1, 2147483647)][int]$OwnerProcessId)
    $probe = Invoke-LabProcess -FilePath (Join-Path $env:SystemRoot 'System32/netstat.exe') -Arguments @('-a', '-n', '-o')
    if ($probe.ExitCode -ne 0) { throw 'Native TCP ownership query failed' }
    $owned = [Collections.Generic.HashSet[int]]::new()
    foreach ($line in ($probe.Output -split "`n")) {
        if ($line -match '^\s*TCP\s+(\S+)\s+(\S+)\s+\S+\s+(\d+)\s*$') {
            if ([int]$Matches[3] -ne $OwnerProcessId) { continue }
            if ($Matches[1] -notmatch ':(\d+)$') { throw 'Unrecognized native TCP endpoint' }
            [void]$owned.Add([int]$Matches[1])
        }
    }
    $listeners = [Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners()
    foreach ($port in $owned) {
        if (@($listeners | Where-Object Port -eq $port).Count) { $port }
    }
}
