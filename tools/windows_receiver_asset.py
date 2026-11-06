"""Fail-closed validation of a tested Windows bundle before APK packaging."""
from pathlib import Path
import hashlib
import json
import re
import struct
from receiver_source import ROOT, fingerprint

NAME = 'usb-most-windows-x86_64.exe'
MAX_BYTES = 20 * 1024 * 1024


def validate(directory, version, *, pin=None, root=ROOT):
    directory = Path(directory)
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError('Receiver artifact directory must be a real directory')
    if {p.name for p in directory.iterdir()} != {NAME, 'manifest.json'}:
        raise ValueError('Unexpected Windows artifact contents')
    metadata = directory / 'manifest.json'
    binary = directory / NAME
    if any(p.is_symlink() or not p.is_file() for p in (metadata, binary)):
        raise ValueError('Receiver artifacts must be regular files')
    if metadata.stat().st_size > 8192 or not 0 < binary.stat().st_size <= MAX_BYTES:
        raise ValueError('Receiver artifact exceeds size bound')
    with metadata.open('rb') as stream:
        encoded = stream.read(8193)
    if len(encoded) > 8192:
        raise ValueError('Receiver manifest exceeds size bound')
    manifest = json.loads(encoded)
    fields = {'schema', 'source_commit', 'source_sha256', 'target', 'version',
              'name', 'bytes', 'sha256', 'driver_manifest_sha256'}
    if set(manifest) != fields or manifest['schema'] != 1:
        raise ValueError('Unsupported receiver manifest')
    if (manifest['name'] != NAME or manifest['target'] != 'x86_64-pc-windows-msvc'
            or manifest['version'] != version
            or not re.fullmatch('[a-f0-9]{40}', manifest['source_commit'])):
        raise ValueError('Receiver identity mismatch')
    for field in ('sha256', 'source_sha256', 'driver_manifest_sha256'):
        if not re.fullmatch('[a-f0-9]{64}', manifest[field]):
            raise ValueError('Invalid receiver digest')
    if manifest['source_sha256'] != fingerprint(root):
        raise ValueError('Windows receiver does not match current native sources; rebuild and test it')
    driver = (root / 'server/windows-driver-lock-x86_64.json').read_bytes()
    # Git on Windows may check this text file out with CRLF. The attestation
    # records the actual build input's hash; accept only the exact LF/CRLF
    # encodings of our pinned content, never arbitrary JSON reformatting.
    driver_lf = driver.replace(b'\r\n', b'\n')
    driver_hashes = {hashlib.sha256(value).hexdigest()
                     for value in (driver_lf, driver_lf.replace(b'\n', b'\r\n'))}
    if manifest['driver_manifest_sha256'] not in driver_hashes:
        raise ValueError('Windows receiver driver pin mismatch')
    with binary.open('rb') as stream:
        data = stream.read(MAX_BYTES + 1)
    if len(data) != manifest['bytes'] or hashlib.sha256(data).hexdigest() != manifest['sha256']:
        raise ValueError('Windows receiver binary digest mismatch')
    if len(data) < 64 or data[:2] != b'MZ':
        raise ValueError('Windows receiver is not a PE executable')
    pe = struct.unpack_from('<I', data, 60)[0]
    if (pe < 64 or pe > len(data) - 26 or data[pe:pe + 4] != b'PE\0\0'
            or struct.unpack_from('<H', data, pe + 4)[0] != 0x8664
            or struct.unpack_from('<H', data, pe + 24)[0] != 0x20b):
        raise ValueError('Windows receiver must be x86_64 PE32+')
    if pin is not None and pin.get('artifact') != manifest:
        raise ValueError('Windows receiver is not the pinned tested artifact')
    return manifest, data
