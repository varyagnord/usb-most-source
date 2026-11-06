#!/usr/bin/env python3
"""Check packaged ELF/ZIP alignment, Android ABIs and the public JNI surface."""
import hashlib
import gzip
import io
import argparse
import json
import pathlib
import struct
import sys
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('apk', type=pathlib.Path)
parser.add_argument('--diagnostic', action='store_true')
args = parser.parse_args()
path = args.apk
expected = {'arm64-v8a': (2, 183), 'armeabi-v7a': (1, 40),
            'x86': (1, 3), 'x86_64': (2, 62)}
methods = {'open', 'close', 'submit', 'cancel', 'reap', 'configureSocket', 'releaseInterface', 'wanCommand'}
if args.diagnostic:
    methods |= {'enableHidDiagnostics', 'takeHidDiagnostics', 'diagnosticRoundTrip', 'wanSelfTest', 'wanNetworkTest'}
report = {'apk': path.name, 'bytes': path.stat().st_size,
          'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'libraries': {}}
with zipfile.ZipFile(path) as archive, path.open('rb') as apk:
    notices = json.loads(archive.read('assets/licenses/index.json'))
    assert 1 < len(notices) <= 512
    license_files = {'assets/licenses/index.json'}
    for notice in notices:
        assert notice['name'] and notice['license'] and notice['texts']
        for name in notice['texts']:
            assert len(name) == 68 and name.endswith('.txt') and all(c in '0123456789abcdef' for c in name[:64])
            entry = 'assets/licenses/' + name
            data = archive.read(entry)
            assert 0 < len(data) <= 512 * 1024
            assert hashlib.sha256(data).hexdigest() == name[:64]
            license_files.add(entry)
    assert {name for name in archive.namelist() if name.startswith('assets/licenses/')} == license_files
    report['license_notices'] = len(notices)
    assert 'assets/receiver-fixture.json' not in archive.namelist(), 'private E2E assets in product APK'
    manifest_path = 'assets/receivers/manifest.json'
    manifest_bytes = archive.read(manifest_path)
    assert len(manifest_bytes) <= 8192
    manifest = json.loads(manifest_bytes)
    artifacts = manifest['artifacts']
    required_receivers = {'usb-most-linux-x86_64', 'usb-most-windows-x86_64.exe'}
    assert {artifact['name'] for artifact in artifacts} == required_receivers, 'missing or unexpected production receiver'
    assert len(artifacts) == len(required_receivers), 'duplicate receiver'
    allowed = {manifest_path}
    report['receivers'] = {}
    for artifact in artifacts:
        name = artifact['name']
        assert name in {f'usb-most-{os}-{arch}{suffix}'
                        for os, suffix in [('linux', ''), ('windows', '.exe')]
                        for arch in ['x86_64', 'aarch64']}
        assert 0 < artifact['bytes'] <= 20 * 1024 * 1024
        entry = 'assets/receivers/' + name + '.gz.bin'
        assert entry not in allowed
        allowed.add(entry)
        # Exercise the final AAPT-packaged bytes, not generated source assets.
        with gzip.GzipFile(fileobj=io.BytesIO(archive.read(entry))) as stream:
            binary = stream.read(20 * 1024 * 1024 + 1)
        assert len(binary) == artifact['bytes']
        assert hashlib.sha256(binary).hexdigest() == artifact['sha256']
        report['receivers'][name] = {'bytes': len(binary), 'sha256': artifact['sha256']}
    assert {n for n in archive.namelist() if n.startswith('assets/receivers/')} == allowed
    libraries = [i for i in archive.infolist() if i.filename.endswith('.so')]
    assert {i.filename for i in libraries} == {f'lib/{a}/libusblib.so' for a in expected}
    for info in libraries:
        abi = info.filename.split('/')[1]
        data = archive.read(info)
        assert data[:4] == b'\x7fELF' and data[5] == 1
        bits, machine = expected[abi]
        assert data[4] == bits and struct.unpack_from('<H', data, 18)[0] == machine
        wide = bits == 2
        phoff = struct.unpack_from('<Q' if wide else '<I', data, 32 if wide else 28)[0]
        phsize, phcount = struct.unpack_from('<HH', data, 54 if wide else 42)
        loads = 0
        for n in range(phcount):
            p = struct.unpack_from('<IIQQQQQQ' if wide else '<IIIIIIII', data, phoff+n*phsize)
            if p[0] == 1:
                offset, address, alignment = (p[2], p[3], p[7]) if wide else (p[1], p[2], p[7])
                assert alignment >= 16384 and offset % 16384 == address % 16384, abi
                loads += 1
        assert loads >= 2
        # Read the actual dynamic symbol table instead of searching string bytes.
        shoff = struct.unpack_from('<Q' if wide else '<I', data, 40 if wide else 32)[0]
        shsize, shcount = struct.unpack_from('<HH', data, 58 if wide else 46)
        sections = [struct.unpack_from('<IIQQQQIIQQ' if wide else '<IIIIIIIIII', data, shoff+n*shsize)
                    for n in range(shcount)]
        exports = set()
        for section in sections:
            if section[1] != 11:  # SHT_DYNSYM
                continue
            strings = sections[section[6]]
            table = data[strings[4]:strings[4]+strings[5]]
            for at in range(section[4], section[4]+section[5], section[9]):
                name = struct.unpack_from('<I', data, at)[0]
                defined = struct.unpack_from('<H', data, at+(6 if wide else 14))[0]
                if defined:
                    exports.add(table[name:table.index(b'\0', name)].decode())
        prefix = 'Java_org_cgutman_usbip_jni_NativeUsb_'
        assert {n[len(prefix):] for n in exports if n.startswith(prefix)} == methods
        policy_prefix = 'Java_org_cgutman_usbip_jni_NativePolicy_'
        assert {n[len(policy_prefix):] for n in exports if n.startswith(policy_prefix)} == {'validHost', 'capability', 'selection', 'describe', 'speed', 'selectionCreate', 'selectionClose', 'connectionCommands'}
        connection_prefix = 'Java_org_cgutman_usbip_jni_NativeConnection_'
        assert {n[len(connection_prefix):] for n in exports if n.startswith(connection_prefix)} == {'start', 'poll', 'approve', 'saved', 'stop', 'decodeQr', 'diagnose', 'computers'}
        management_prefix = 'Java_org_cgutman_usbip_jni_NativeManagement_'
        assert {n[len(management_prefix):] for n in exports if n.startswith(management_prefix)} == {'create', 'close', 'execute'}
        apk.seek(info.header_offset+26)
        name_len, extra_len = struct.unpack('<HH', apk.read(4))
        zip_offset = info.header_offset+30+name_len+extra_len
        assert info.compress_type == zipfile.ZIP_STORED and zip_offset % 16384 == 0
        report['libraries'][abi] = {'bytes': len(data), 'elf_load_segments': loads,
                                    'zip_offset': zip_offset, 'alignment': 16384,
                                    'jni_exports': len(methods)+19}
print(json.dumps(report, indent=2))
