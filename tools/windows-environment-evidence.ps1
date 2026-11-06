# Read-only evidence for an owned VM; unknown security settings are never PASS.
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -and $env:USB_MOST_WINDOWS_VM_LAB -ne '1') {
    throw 'This evidence collector requires an explicitly isolated Windows VM'
}
$root = Split-Path -Parent $PSScriptRoot
$directory = Join-Path $root 'build/compatibility'
New-Item -ItemType Directory -Force $directory | Out-Null
$os = Get-CimInstance Win32_OperatingSystem
$secureBoot = [ordered]@{ observed = $false; enabled = $null }
try {
    $secureBoot.enabled = [bool](Confirm-SecureBootUEFI -ErrorAction Stop)
    $secureBoot.observed = $true
} catch {
    $secureBoot.reason = 'query_unavailable'
}
$guard = [ordered]@{ observed = $false; virtualizationBasedSecurityStatus = $null; configured = @(); running = @() }
try {
    $state = Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard -ErrorAction Stop
    if ($null -ne $state) {
        $guard.observed = $true
        $guard.virtualizationBasedSecurityStatus = $state.VirtualizationBasedSecurityStatus
        $guard.configured = @($state.SecurityServicesConfigured)
        $guard.running = @($state.SecurityServicesRunning)
    }
} catch {
    $guard.reason = 'query_unavailable'
}
$commit = (git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -or $commit -cnotmatch '^[a-f0-9]{40}$') { throw 'Unknown source revision' }
$evidence = [ordered]@{
    schema = 1
    source_commit = $commit
    captured_utc = [DateTime]::UtcNow.ToString('o')
    os = [ordered]@{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber; architecture = $os.OSArchitecture }
    secure_boot = $secureBoot
    device_guard = $guard
    scope = 'Environment observation only; test success comes from the individual job steps. No desktop Windows, interactive UAC or unobserved protection combination is implied.'
}
[IO.File]::WriteAllText((Join-Path $directory 'environment.json'), ($evidence | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
$evidence | ConvertTo-Json -Depth 6
