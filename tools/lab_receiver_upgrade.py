#!/usr/bin/env python3
"""Owned QEMU only: stage a pinned old receiver and preserve exact recovery files.

No USB, mount, recursive cleanup, package or policy change. Product installers
perform the receiver transitions. This helper never supplies the new binary
during Android verification; it uses the saved original only for final restore.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time

BINARY = Path('/usr/local/libexec/usb-most/usb-most-desktop')
CONFIG = Path('/var/lib/usb-most/receiver.json')
FILES = {'receiver.bin': BINARY, 'receiver.json': CONFIG,
         'service.unit': Path('/etc/systemd/system/usb-most-desktop.service'),
         'modules.conf': Path('/etc/modules-load.d/usb-most.conf'),
         'activation': Path('/var/lib/usb-most/active')}
TRUST = [Path('/var/lib/usb-most/identity.json'), Path('/var/lib/usb-most/pairs.json')]
OLD_SHA = '489ae646ef4dddd18981dd3eb7f48fd6836ca1ed4eed6b5d31f5621967230792'


def require(value, message):
    if not value:
        raise RuntimeError(message)


def directory(nonce):
    require(re.fullmatch(r'[a-f0-9]{32}', nonce) is not None, 'Invalid upgrade ownership nonce')
    return Path('/var/tmp/usb-most-upgrade-' + nonce)


def regular(path, maximum=20 * 1024 * 1024):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
            and not info.st_mode & 0o022 and info.st_size <= maximum, 'Unsafe receiver fixture file')
    with path.open('rb') as stream:
        opened = os.fstat(stream.fileno())
        require((opened.st_dev, opened.st_ino) == (info.st_dev, info.st_ino), 'File identity changed')
        data = stream.read(maximum + 1)
    require(len(data) == info.st_size, 'File changed or exceeds bound')
    return data, dict(sha256=hashlib.sha256(data).hexdigest(), bytes=len(data),
                     mode=stat.S_IMODE(info.st_mode), uid=info.st_uid, gid=info.st_gid,
                     inode=info.st_ino, mtime_ns=info.st_mtime_ns)


def snapshot(expected, connected, peer=None):
    # Every executable invocation is preceded by a root-owned exact hash check.
    records = {name: regular(path)[1] for name, path in FILES.items()}
    require(records['receiver.bin']['sha256'] == expected, 'Unexpected installed receiver')
    health = json.loads(subprocess.check_output([str(BINARY), '--health'], text=True, timeout=10))['status']
    require(health['active'] == 0 and health['connected'] == connected, 'Receiver is not in expected idle/session state')
    if connected:
        require(health.get('peer') == peer, 'Connected receiver belongs to another peer')
    service = subprocess.check_output(['systemctl', 'show', 'usb-most-desktop.service',
               '-p', 'MainPID', '-p', 'ActiveState', '-p', 'UnitFileState'], text=True, timeout=10)
    require('ActiveState=active' in service and 'UnitFileState=enabled' in service
            and 'MainPID=0\n' not in service, 'Receiver service is not healthy')
    require(not Path('/var/lib/usb-most/install-journal.json').exists(), 'Incomplete product installation journal')
    return dict(files=records, trust={str(path): regular(path)[1] for path in TRUST},
                service=service, version=health['version'], config_hash=health['config_hash'],
                active=0, connected=connected, kernel=os.uname().release,
                boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip())


def verify_trust(before, after):
    require(before['trust'] == after['trust'] and before['boot_id'] == after['boot_id'],
            'Trust material changed or guest rebooted; retain recovery evidence')


def create_file(path, data, mode):
    with path.open('xb') as stream:
        path.chmod(mode)
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def owner(root, settings):
    info = root.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0
            and stat.S_IMODE(info.st_mode) == 0o700 and not root.is_mount(), 'Invalid recovery directory')
    record = json.loads(regular(root / 'owner.json', 32768)[0])
    require(record['nonce'] == settings['nonce'] and record['current_sha256'] == settings['current_sha256'],
            'Recovery ownership changed')
    for name, entry in record['backups'].items():
        require(name in set(FILES) | {'old.bin'}, 'Unexpected backup path')
        require(regular(root / name)[1] == entry, 'Recovery file changed')
    require(set(record['backups']) == set(FILES) | {'old.bin'}, 'Incomplete recovery set')
    return record


def prepare(root, settings):
    before = snapshot(settings['current_sha256'], False)
    config = regular(CONFIG, 4096)[0]
    require(json.loads(config)['transport'] == 'direct', 'Restore requires an idle direct-TLS baseline')
    old = base64.b64decode(settings['old_binary'], validate=True)
    require(hashlib.sha256(old).hexdigest() == OLD_SHA, 'Only pinned historical1.0.11 artifact')
    root.mkdir(mode=0o700)
    for name, path in FILES.items():
        create_file(root / name, regular(path)[0], 0o700 if name == 'receiver.bin' else 0o600)
    create_file(root / 'old.bin', old, 0o700)
    backups = {name: regular(root / name)[1] for name in [*FILES, 'old.bin']}
    record = dict(nonce=settings['nonce'], current_sha256=settings['current_sha256'],
                  before=before, backups=backups)
    create_file(root / 'owner.json', json.dumps(record).encode(), 0o600)
    require(subprocess.check_output([str(root / 'old.bin'), '--version'], text=True, timeout=10).strip() == '1.0.11',
            'Pinned old receiver version differs')
    installed = subprocess.run([str(root / 'old.bin'), '--install-stdin'], input=config,
                               capture_output=True, timeout=120)
    require(installed.returncode == 0, 'Old receiver installation failed; retain backups')
    after = snapshot(OLD_SHA, False)
    require(after['version'] == '1.0.11', 'Old service version did not become active')
    verify_trust(before, after)
    return dict(before=before, old_installed=after, recovery_directory=str(root))


def restore(root, settings):
    record = owner(root, settings)
    current = regular(BINARY)[1]['sha256']
    require(current in (OLD_SHA, settings['current_sha256']), 'Unrecognized installed image; preserve recovery files')
    deadline = time.monotonic() + 30
    while True:
        health = json.loads(subprocess.check_output([str(BINARY), '--health'], text=True, timeout=10))['status']
        if not health['connected'] and health['active'] == 0:
            break
        require(time.monotonic() < deadline, 'Receiver still in use; retain recovery files')
        time.sleep(.2)
    # Restore through the same product installer, never overwrite active files.
    operation = subprocess.run([str(root / 'receiver.bin'), '--install-stdin'],
                                input=regular(root / 'receiver.json', 4096)[0],
                                capture_output=True, timeout=120)
    require(operation.returncode == 0, 'Original configuration could not be restored')
    after = snapshot(settings['current_sha256'], False)
    verify_trust(record['before'], after)
    for name, before in record['before']['files'].items():
        require(all(after['files'][name][key] == before[key]
                    for key in ('sha256', 'bytes', 'mode', 'uid', 'gid')), 'Original receiver deployment changed')
    require(after['config_hash'] == record['before']['config_hash'], 'Original configuration identity changed')
    # Validate every exact file before unlinking; never recurse or delete a mount.
    owner(root, settings)
    require({path.name for path in root.iterdir()} == set(FILES) | {'old.bin', 'owner.json'},
            'Unexpected recovery contents; preserve them')
    for name in [*FILES, 'old.bin', 'owner.json']:
        (root / name).unlink()
    root.rmdir()
    return dict(restored=after, exact_files_and_trust_preserved=True, recovery_directory_removed=True)


def run(settings):
    require(os.geteuid() == 0, 'Owned guest root required')
    product = Path('/sys/class/dmi/id/product_name').read_text()
    require('QEMU' in product or 'Standard PC' in product, 'Only owned QEMU guest')
    root = directory(settings['nonce'])
    require(re.fullmatch(r'[a-f0-9]{64}', settings['current_sha256']) is not None, 'Invalid expected receiver hash')
    if settings['action'] == 'prepare':
        return prepare(root, settings)
    if settings['action'] == 'observe':
        record = owner(root, settings)
        after = snapshot(settings['current_sha256'], True, settings['peer'])
        verify_trust(record['before'], after)
        return after
    if settings['action'] == 'restore':
        return restore(root, settings)
    raise ValueError('Unknown upgrade action')


if __name__ == '__main__':
    print(json.dumps(run(SETTINGS)))
