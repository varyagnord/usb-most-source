#!/usr/bin/env python3
"""Sign an internal AAB with a private upload key, then verify every entry.

Does not publish/upload, overwrite artifacts, or copy private key material.
RuStore application-signing setup and the corresponding source release are
separate required gates. Passwords are read by JDK tools from a private file.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def private_file(path):
    path = Path(path).absolute()
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o077:
        raise ValueError('Signing inputs must be private regular files')
    if info.st_uid != os.getuid():
        raise ValueError('Signing inputs must belong to the current user')
    return path


def payloads(path):
    with zipfile.ZipFile(path) as archive:
        items = archive.infolist()
        names = [item.filename for item in items]
        if len(set(names)) != len(names) or len(names) > 16384:
            raise ValueError('Duplicate or excessive bundle entries')
        required = {'BundleConfig.pb', 'base/manifest/AndroidManifest.xml', 'base/dex/classes.dex'}
        if not required.issubset(names) or sum(item.file_size for item in items) > 512 * 1024**2:
            raise ValueError('Unexpected bundle layout or size')
        result = {}
        for item in items:
            name = item.filename
            if name.startswith('/') or '..' in name.split('/') or '\\' in name:
                raise ValueError('Unsafe archive entry')
            upper = name.upper()
            tail = upper.removeprefix('META-INF/')
            if upper.startswith('META-INF/') and '/' not in tail and (tail == 'MANIFEST.MF' or tail.endswith(('.SF', '.RSA', '.DSA', '.EC'))):
                continue
            if not item.is_dir():
                with archive.open(item) as stream:
                    result[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--keystore', required=True, type=Path)
    parser.add_argument('--password-file', required=True, type=Path)
    parser.add_argument('--alias', default='upload')
    args = parser.parse_args()
    source = args.input.resolve(strict=True)
    output = args.output.absolute()
    if output.exists() or output.is_symlink() or source == output:
        raise ValueError('Output must be new; existing artifacts are never overwritten')
    if not args.alias or not all(c.isascii() and (c.isalnum() or c in '-_') for c in args.alias):
        raise ValueError('Invalid signing alias')
    key = private_file(args.keystore)
    password = private_file(args.password_file)
    before = payloads(source)
    with zipfile.ZipFile(source) as archive:
        if any(n.upper().startswith('META-INF/') and n.upper().endswith(('.SF', '.RSA', '.DSA', '.EC')) for n in archive.namelist()):
            raise ValueError('Input must be unsigned')
    paths = []
    try:
        for suffix in ('.aab', '.pem'):
            descriptor, name = tempfile.mkstemp(prefix='.usb-most-sign-', suffix=suffix, dir=output.parent)
            os.close(descriptor)
            paths.append(Path(name))
        pending, certificate = paths
        subprocess.run(['keytool', '-exportcert', '-rfc', '-keystore', str(key), '-alias', args.alias,
                        '-storepass:file', str(password), '-file', str(certificate)], check=True, capture_output=True)
        subprocess.run(['jarsigner', '-keystore', str(key), '-storepass:file', str(password),
                        '-keypass:file', str(password), '-sigalg', 'SHA256withRSA', '-digestalg', 'SHA-256',
                        '-signedjar', str(pending), str(source), args.alias], check=True, capture_output=True)
        if payloads(pending) != before:
            raise ValueError('Signing changed bundle payloads')
        subprocess.run(['java', str(ROOT / 'tools/VerifyBundleSignature.java'), str(pending), str(certificate)], check=True)
        with pending.open('rb') as stream:
            os.fsync(stream.fileno())
        # Same-directory hard link gives exclusive publication of the finished
        # file. A concurrently created destination is never replaced.
        os.link(pending, output)
        descriptor = os.open(output.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        print(json.dumps({'aab': str(output), 'sha256': hashlib.file_digest(output.open('rb'), 'sha256').hexdigest(),
                          'certificate_pem_sha256': hashlib.file_digest(certificate.open('rb'), 'sha256').hexdigest(),
                          'bytes': output.stat().st_size, 'uploaded': False}, indent=2))
    finally:
        for path in paths:
            path.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
