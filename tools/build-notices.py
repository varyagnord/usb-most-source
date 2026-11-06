#!/usr/bin/env python3
"""Package license texts from exact Cargo/Gradle dependencies, without web fetches."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--android', type=Path, required=True)
a = p.parse_args()
output = a.output.resolve() / 'licenses'
assert output.is_relative_to(ROOT / 'app/build') or output.is_relative_to(ROOT / 'build')
output.mkdir(parents=True, exist_ok=True)
texts, entries = {}, []

def notice(data):
    data = data.replace('\r\n', '\n').strip() + '\n'
    encoded = data.encode('utf-8')
    if not 0 < len(encoded) <= 512 * 1024:
        raise ValueError('License text exceeds display bound')
    name = hashlib.sha256(encoded).hexdigest() + '.txt'
    texts[name] = encoded
    return name

apache = notice((ROOT / 'third_party/Apache-2.0.txt').read_text())
entries.append({'name':'USB Мост', 'version':'', 'license':'GPL-3.0-only',
    'texts':[notice((ROOT / 'LICENSE').read_text())],
    'attribution':'На основе USB/IP Server for Android — Cameron Gutman. Изменения: проект USB Мост.'})
entries.append({'name':'usbip-win2 signed driver', 'version':'0.9.8.1', 'license':'BSD-2-Clause',
    'texts':[notice((ROOT / 'third_party/usbip-win2-LICENSE.txt').read_text())],
    'attribution':'https://github.com/vadimgrn/usbip-win2'})
entries.append({'name':'Google Material Icons: USB', 'version':'d3d4aca5a7cf', 'license':'Apache-2.0',
    'texts':[apache], 'attribution':(ROOT / 'third_party/material-icons-usb.txt').read_text().strip()})
cargo = shutil.which('cargo') or str(Path.home() / '.cargo/bin/cargo')
packages = {}
for target, roots in [('aarch64-linux-android', ['usb-most-native']),
                      ('x86_64-unknown-linux-musl', ['usb-most-desktop']),
                      ('x86_64-pc-windows-msvc', ['usb-most-desktop'])]:
    graph = json.loads(subprocess.check_output([cargo, 'metadata', '--locked', '--all-features',
        '--format-version', '1', '--filter-platform', target], cwd=ROOT))
    available = {v['id']:v for v in graph['packages']}
    nodes = {v['id']:v for v in graph['resolve']['nodes']}
    pending = [v['id'] for v in graph['packages'] if v['name'] in roots]
    seen = set()
    while pending:
        key = pending.pop()
        if key in seen: continue
        seen.add(key)
        package = available[key]
        # Locally patched third-party crates retain their own attribution.
        if package['source'] or Path(package['manifest_path']).is_relative_to(ROOT / 'third_party'):
            packages[key] = package
        pending.extend(dep['pkg'] for dep in nodes[key]['deps']
                       if any(kind['kind'] != 'dev' for kind in dep['dep_kinds']))
for package in sorted(packages.values(), key=lambda v:(v['name'],v['version'])):
    root = Path(package['manifest_path']).parent
    included = set()
    for path in sorted(root.rglob('*')):
        if path.is_file() and path.name.lower().startswith(('license','copying','notice')):
            included.add(notice(path.read_text(encoding='utf-8')))
    if package.get('license_file'):
        included.add(notice((root / package['license_file']).read_text()))
    if not included and (package['name'], package['version']) == ('qrcodegen', '1.8.0'):
        header = (root / 'src/lib.rs').read_text().split('*/', 1)[0] + '*/'
        if not header.startswith('/*') or 'Copyright (c) Project Nayuki' not in header or 'Permission is hereby granted' not in header:
            raise ValueError('QR generator license header changed; review it')
        included.add(notice(header))
    if not included:
        # A few Apache/dual-licensed crates omit the standard text from their
        # crate archive. Select Apache only where it is an explicit alternative;
        # never substitute it for an unknown license or an AND obligation.
        expression = package['license'] or ''
        if 'Apache-2.0' not in expression or 'AND' in expression or 'WITH' in expression:
            raise ValueError('Missing license text: '+package['name'])
        included.add(apache)
    attribution = '\n'.join([*(package['authors'] or []), package['repository'] or ''])
    entries.append({'name':package['name'], 'version':package['version'],
        'license':package['license'] or 'see included license',
        'attribution':attribution, 'texts':sorted(included)})
for artifact in json.loads(a.android.read_text()):
    group = artifact['group']
    if not (group.startswith('androidx.') or group in ('org.jetbrains','org.jetbrains.kotlin','org.jetbrains.kotlinx','com.google.guava')):
        raise ValueError('Review new Android dependency license: '+group)
    included = {apache}
    with zipfile.ZipFile(artifact['file']) as archive:
        for entry in archive.infolist():
            if not entry.is_dir() and Path(entry.filename).name.lower().startswith(('license','notice')):
                if entry.file_size > 512 * 1024: raise ValueError('Oversized Android license')
                included.add(notice(archive.read(entry).decode('utf-8')))
    entries.append({'name':group+':'+artifact['name'], 'version':artifact['version'],
        'license':'Apache-2.0', 'texts':sorted(included),
        'attribution':('Android Open Source Project / AndroidX' if group.startswith('androidx.')
                       else 'JetBrains' if group.startswith('org.jetbrains') else 'Google / Guava')})
if len(entries)>512 or sum(map(len,texts.values()))>4*1024*1024:
    raise ValueError('License bundle exceeds reviewed bounds')
index = json.dumps(entries, ensure_ascii=False, sort_keys=True, separators=(',',':')).encode()
if len(index)>512*1024: raise ValueError('License index exceeds bound')
for path in output.iterdir():
    if path.name!='index.json' and not re.fullmatch(r'[a-f0-9]{64}\.txt',path.name):
        raise ValueError('Unexpected file in generated licenses; inspect explicitly')
for name,data in texts.items(): (output/name).write_bytes(data)
(output/'index.json').write_bytes(index)
# Delete only recognized obsolete generated text files, never directories.
for path in output.iterdir():
    if path.name!='index.json' and path.name not in texts: path.unlink()
print(f'Packaged {len(entries)} notices, {len(texts)} unique texts, {sum(map(len,texts.values()))} text bytes')
