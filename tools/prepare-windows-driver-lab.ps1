# Only an isolated hosted Windows VM. No unsigned drivers/security-policy changes.
param([switch]$ProductPreparation)
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true' -and $env:USB_MOST_WINDOWS_VM_LAB -ne '1') {
    throw 'This driver lab requires an explicitly isolated Windows VM'
}
$root = Split-Path -Parent $PSScriptRoot
$cache = Join-Path $root 'build/windows-driver'
New-Item -ItemType Directory -Force $cache | Out-Null
$installer = Join-Path $cache 'USBip-0.9.8.1-x64.exe'
if (-not (Test-Path -LiteralPath $installer)) {
    Invoke-WebRequest -Uri 'https://github.com/vadimgrn/usbip-win2/releases/download/v.0.9.8.1/USBip-0.9.8.1-x64.exe' -OutFile $installer
} else {
    Write-Output 'Reusing vendor installer cache; pinned hash and catalog checks still required'
}
if ((Get-FileHash -Algorithm SHA256 $installer).Hash.ToLowerInvariant() -ne '38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a') { throw 'Vendor package digest mismatch' }
$package = Join-Path $cache 'package-x86_64'
cargo run --locked -p usb-most-artifacts -- $installer $package
if ($LASTEXITCODE) { throw 'Signed package extraction failed' }
$expected = Get-Content (Join-Path $root 'server/windows-driver-lock-x86_64.json') -Raw | ConvertFrom-Json
foreach ($item in $expected.files.PSObject.Properties) {
    $file = Join-Path $package $item.Name
    if ((Get-Item $file).Length -ne $item.Value.bytes -or (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant() -ne $item.Value.sha256) { throw 'Extracted signed file mismatch' }
}
foreach ($name in @('usbip2_ude.cat', 'usbip2_filter.cat')) {
    if ((Get-AuthenticodeSignature (Join-Path $package $name)).Status -ne 'Valid') { throw 'Windows rejected vendor catalog signature' }
}
if ($ProductPreparation) {
    $env:USB_MOST_BUNDLED_DRIVER_DIR = $package
    $env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = '-C target-feature=+crt-static'
    cargo build --locked --release -p usb-most-desktop --features bundled-driver --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE) { throw 'Bundled receiver build failed' }
    $receiver = Join-Path $root 'build/rust-target/x86_64-pc-windows-msvc/release/usb-most-desktop.exe'
    cargo run --locked --release --target x86_64-pc-windows-msvc -p usb-most-connection --example windows_bootstrap -- $receiver
    if ($LASTEXITCODE) { throw 'Actual first-use bootstrap failed' }
} else {
    # Independent driver baseline retained for comparison with product first use.
    & "$env:SystemRoot\System32\pnputil.exe" /add-driver (Join-Path $package 'usbip2_ude.inf')
    if ($LASTEXITCODE -ne 0) { throw 'Signed UDE package staging failed' }
    & "$env:SystemRoot\System32\pnputil.exe" /add-driver (Join-Path $package 'usbip2_filter.inf') /install
    if ($LASTEXITCODE -ne 0) { throw 'Filter installation failed or requires reboot' }
}
$env:USB_MOST_DRIVER_DIR = $package
if ($ProductPreparation) {
    cargo test --locked -p usb-most-desktop --features bundled-driver deployment:: -- --nocapture
    if ($LASTEXITCODE) { throw 'Windows deployment journal/rollback regression failed' }
    cargo test --locked -p usb-most-platform installed_service_has_firewall_identity -- --ignored --nocapture
    if ($LASTEXITCODE) { throw 'Installed receiver lacks its service-bound firewall identity' }
}
cargo test --locked -p usb-most-platform protected_signed_controller -- --ignored --nocapture
if ($LASTEXITCODE) {
    Get-Content "$env:SystemRoot\INF\setupapi.dev.log" -Tail 180
    throw 'Private signed-controller test failed'
}
cargo test --locked -p usb-most-desktop signed_driver_imports_synthetic_hid -- --ignored --nocapture
if ($LASTEXITCODE) {
    Get-Content "$env:SystemRoot\INF\setupapi.dev.log" -Tail 100
    throw 'Windows synthetic HID import/transfer/detach test failed'
}

cargo test --locked -p usb-most-desktop signed_driver_ejects_only_owned_storage -- --ignored --nocapture
if ($LASTEXITCODE) {
    Get-Content "$env:SystemRoot\INF\setupapi.dev.log" -Tail 100
    throw 'Windows RAM storage OS-confirmed ejection test failed'
}

if ($ProductPreparation) {
    $service = Get-CimInstance Win32_Service -Filter "Name='USBMostReceiver'"
    $installed = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'USBMost\usb-most-desktop.exe'
    if ($service.PathName -ne ('"' + $installed + '" --service')) { throw 'Unexpected receiver service ownership' }
    Stop-Service USBMostReceiver
    (Get-Service USBMostReceiver).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(120))
    & "$env:SystemRoot\System32\sc.exe" delete USBMostReceiver
    if ($LASTEXITCODE) { throw 'Owned fixture service cleanup failed' }
    $out = Join-Path $root 'build/windows-receiver-artifact'
    New-Item -ItemType Directory -Path $out | Out-Null
    $name = 'usb-most-windows-x86_64.exe'
    Copy-Item $receiver (Join-Path $out $name)
    $commit = (git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -or $commit -cnotmatch '^[a-f0-9]{40}$') { throw 'Unknown source revision' }
    $version = (& $receiver --version).Trim()
    if ($LASTEXITCODE) { throw 'Receiver version failed' }
    $sourceHash = (python (Join-Path $root 'tools/receiver_source.py')).Trim()
    if ($LASTEXITCODE -or $sourceHash -cnotmatch '^[a-f0-9]{64}$') { throw 'Receiver source fingerprint failed' }
    $manifest = [ordered]@{ schema=1; source_commit=$commit; source_sha256=$sourceHash; target='x86_64-pc-windows-msvc'; version=$version; name=$name; bytes=(Get-Item $receiver).Length; sha256=(Get-FileHash $receiver -Algorithm SHA256).Hash.ToLowerInvariant(); driver_manifest_sha256=(Get-FileHash (Join-Path $root 'server/windows-driver-lock-x86_64.json') -Algorithm SHA256).Hash.ToLowerInvariant() }
    [IO.File]::WriteAllText((Join-Path $out 'manifest.json'), ($manifest | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
    & (Join-Path $PSScriptRoot 'windows-ssh-e2e.ps1') -Receiver $receiver
    cargo run --locked --release --target x86_64-pc-windows-msvc -p usb-most-connection --example windows_direct_lab -- $receiver
    if ($LASTEXITCODE) { throw 'Windows direct TLS/pairing/kernel HID/reconnect failed' }
    Write-Output 'PASS bundled receiver artifact contains only executable and reproducibility metadata'
}
