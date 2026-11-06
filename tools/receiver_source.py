#!/usr/bin/env python3
"""Portable fingerprint of receiver sources and pinned build inputs.

Text CRLF is canonicalized to LF so Git's Windows checkout policy cannot change
the identity. Paths and lengths are framed; no build outputs or local settings
are included. This is source correspondence, not a substitute for binary hashes.
"""
from pathlib import Path
import hashlib

ROOT = Path(__file__).resolve().parents[1]
FILES = (
    'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo/config.toml',
    'server/windows-driver-lock-x86_64.json', 'server/usb-most-desktop.service',
    'tools/receiver_source.py', 'tools/prepare-windows-driver-lab.ps1',
)
TREES = (
    'native/usb-most-desktop', 'native/usb-most-platform',
    'native/usb-most-connection', 'native/usb-most-protocol',
    'third_party/russh',
)


def fingerprint(root=ROOT):
    paths = set(FILES)
    for tree in TREES:
        for path in (root / tree).rglob('*'):
            if path.is_symlink():
                raise ValueError('Receiver source symlinks are not supported')
            if path.is_file():
                paths.add(path.relative_to(root).as_posix())
    digest = hashlib.sha256(b'usb-most-receiver-source-v1\0')
    for name in sorted(paths):
        path = root / name
        if path.is_symlink():
            raise ValueError('Receiver source symlinks are not supported')
        data = path.read_bytes().replace(b'\r\n', b'\n')
        digest.update(name.encode('utf-8') + b'\0')
        digest.update(len(data).to_bytes(8, 'big'))
        digest.update(data)
    return digest.hexdigest()


if __name__ == '__main__':
    print(fingerprint())
