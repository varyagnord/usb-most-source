#!/usr/bin/env python3
"""Build Linux and package the tested pinned Windows receiver, entirely offline.

The Android package owns the manifest/artifacts. Names and SHA256 never come
from a remote probe/QR. Windows assets are added only after driver/installer E2E.
"""
from pathlib import Path
from windows_receiver_asset import validate as validate_windows
import argparse, gzip, hashlib, json, os, shutil, subprocess, tomllib

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
output = a.output.resolve() / 'receivers'
assert output.is_relative_to(ROOT / 'build') or output.is_relative_to(ROOT / 'app/build')
output.mkdir(parents=True, exist_ok=True)
version = tomllib.loads((ROOT / 'native/usb-most-desktop/Cargo.toml').read_text())['package']['version']
connection_version = tomllib.loads((ROOT / 'native/usb-most-connection/Cargo.toml').read_text())['package']['version']
if connection_version != version:
    raise SystemExit('Connection runtime and bundled receiver versions must match: native manifest and SSH health checks enforce this contract')
pin = json.loads((ROOT / 'server/windows-receiver-lock-x86_64.json').read_text())
windows_dir = Path(os.environ.get('USB_MOST_WINDOWS_RECEIVER_DIR',
    str(ROOT / 'build' / f"windows-receiver-ci-{pin['validation']['run_id']}"))).resolve()
if not windows_dir.is_relative_to(ROOT / 'build'):
    raise SystemExit('Windows receiver cache must be inside the build workspace')
windows_manifest, windows_binary = validate_windows(windows_dir, version, pin=pin)
channel = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel']
cargo = shutil.which('cargo') or str(Path.home() / '.cargo/bin/cargo')
cc = shutil.which('musl-gcc')
if not cc:
    raise SystemExit('Install musl-tools to build the static Linux receiver')
env = os.environ.copy()
env.update({
    'RUSTUP_TOOLCHAIN': channel,
    'CARGO_TARGET_DIR': str(ROOT / 'build/rust-receivers'),
    'CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER': cc,
    'CC_x86_64_unknown_linux_musl': cc,
})
target = 'x86_64-unknown-linux-musl'
subprocess.run([cargo, 'build', '--locked', '--release', '-p', 'usb-most-desktop',
                '--features', 'installation,local-ui', '--target', target, '-j', '2'], cwd=ROOT, env=env, check=True)
source = Path(env['CARGO_TARGET_DIR']) / target / 'release/usb-most-desktop'
elf = source.read_bytes()
if elf[:6] != b'\x7fELF\x02\x01' or int.from_bytes(elf[18:20], 'little') != 62:
    raise SystemExit('Receiver is not a little-endian x86_64 ELF')
headers = subprocess.check_output(['readelf', '-l', str(source)], text=True)
if 'INTERP' in headers:
    raise SystemExit('Receiver must be static: no target glibc dependency')
if subprocess.check_output([str(source), '--version'], text=True).strip() != version:
    raise SystemExit('Receiver version mismatch')
if not 0 < len(elf) <= 20 * 1024 * 1024:
    raise SystemExit('Receiver exceeds the APK/provisioner size limit')
name = 'usb-most-linux-x86_64'
# AAPT transparently gunzips assets ending in .gz and strips the extension.
# Keep the gzip stream opaque so runtime bounds/hash verification sees the
# exact bytes named by our generated asset contract.
archive = output / (name + '.gz.bin')
# Reproducible gzip header: no timestamp or developer filesystem path.
archive.write_bytes(gzip.compress(elf, compresslevel=9, mtime=0))
manifest = {'version': version, 'artifacts': [{
    'name': name, 'bytes': len(elf), 'sha256': hashlib.sha256(elf).hexdigest(),
}]}
windows_archive = output / (windows_manifest['name'] + '.gz.bin')
windows_archive.write_bytes(gzip.compress(windows_binary, compresslevel=9, mtime=0))
manifest['artifacts'].append({key: windows_manifest[key] for key in ('name', 'bytes', 'sha256')})
(output / 'manifest.json').write_text(json.dumps(manifest, sort_keys=True, separators=(',', ':')) + '\n')
# Unknown/stale artifacts must not silently enter a release after an interrupted build.
allowed = {'manifest.json', archive.name, windows_archive.name}
extra = [f.name for f in output.iterdir() if f.name not in allowed]
if extra:
    raise SystemExit('Unexpected receiver assets retained; inspect explicitly: ' + ', '.join(extra))
print(f'Built static Linux x86_64 receiver {len(elf)} bytes, packaged {archive.stat().st_size} bytes', flush=True)
print(f'Verified Windows x86_64 receiver {len(windows_binary)} bytes, packaged {windows_archive.stat().st_size} bytes', flush=True)
