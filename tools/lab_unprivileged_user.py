#!/usr/bin/env python3
"""Disposable no-sudo account inside an exclusively owned QEMU guest only.

The caller verifies QEMU ownership and holds its soak.lock. No recursive removal:
unexpected files, identity changes or live account processes preserve the fixture.
"""
import grp
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import stat
import subprocess
import sys
import time


def identity(nonce):
    if not re.fullmatch(r'[0-9a-f]{32}', nonce):
        raise ValueError('Invalid owned fixture nonce')
    return 'umtest' + nonce[:16], Path('/run/usb-most-no-sudo-' + nonce)


def validate_account(record, entry):
    user, root = identity(record['nonce'])
    if (entry.pw_name != user or entry.pw_uid != record['uid'] or entry.pw_uid < 1000
            or entry.pw_dir != str(root / 'home') or entry.pw_shell != '/bin/sh'):
        raise RuntimeError('Owned account identity changed; preserving it')


def regular(path, uid, mode=None):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != uid:
        raise RuntimeError('Unexpected file identity; preserving it')
    if mode is not None and stat.S_IMODE(info.st_mode) != mode:
        raise RuntimeError('Unexpected file permissions; preserving it')
    return info


def directory(path, uid, names):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != uid:
        raise RuntimeError('Unexpected directory identity; preserving it')
    if {p.name for p in path.iterdir()} != set(names):
        raise RuntimeError('Unexpected fixture contents; preserving them')


def validate_home(home, uid):
    cache = home / '.cache'
    has_cache = cache.exists() or cache.is_symlink()
    directory(home, uid, ['.ssh', '.cache'] if has_cache else ['.ssh'])
    if not has_cache:
        return None
    # Ubuntu PAM creates this empty acknowledgement on first SSH login. Only
    # this exact empty regular file is disposable; all other cache data stays.
    directory(cache, uid, ['motd.legal-displayed'])
    marker = cache / 'motd.legal-displayed'
    if regular(marker, uid).st_size != 0:
        raise RuntimeError('Unexpected nonempty PAM marker; preserving it')
    return marker


def snapshot():
    binary = Path('/usr/local/libexec/usb-most/usb-most-desktop')
    paths = [binary, Path('/etc/systemd/system/usb-most-desktop.service'),
             Path('/etc/modules-load.d/usb-most.conf')]
    state = Path('/var/lib/usb-most')
    paths.extend(sorted(state.iterdir()))
    files = {}
    for path in paths:
        info = path.lstat()
        if stat.S_ISSOCK(info.st_mode) and path.name == 'active':
            continue
        regular(path, 0)
        files[str(path)] = dict(sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
                               mode=stat.S_IMODE(info.st_mode), uid=info.st_uid, gid=info.st_gid)
    health = json.loads(subprocess.check_output([str(binary), '--health'], text=True, timeout=10))['status']
    if health['connected'] or health['active'] != 0:
        raise RuntimeError('Receiver is in use')
    service = subprocess.check_output(['systemctl', 'show', 'usb-most-desktop.service',
                                      '-p', 'MainPID', '-p', 'ActiveState'], text=True, timeout=10)
    if 'ActiveState=active' not in service or 'MainPID=0\n' in service:
        raise RuntimeError('Expected healthy owned receiver')
    return dict(files=files, service=service, connected=False, active=0,
                config_hash=health['config_hash'], version=health['version'])


def create(nonce, public_key):
    user, root = identity(nonce)
    if not re.fullmatch(r'ssh-ed25519 [A-Za-z0-9+/=]+(?: [^\r\n]*)?\n?', public_key):
        raise ValueError('Only owned Ed25519 public key fixture')
    try:
        pwd.getpwnam(user)
    except KeyError:
        pass
    else:
        raise RuntimeError('Account already exists')
    defaults = subprocess.check_output(['useradd', '-D'], text=True, timeout=10)
    if 'CREATE_MAIL_SPOOL=no' not in defaults.splitlines():
        raise RuntimeError('Owned Ubuntu fixture requires mailbox creation disabled')
    root.mkdir(mode=0o711)
    marker = root / 'owner.json'
    record = dict(nonce=nonce, uid=None, key_sha256=hashlib.sha256(public_key.encode()).hexdigest())
    with marker.open('x') as stream:
        marker.chmod(0o600)
        json.dump(record, stream)
    group = grp.getgrnam('nogroup')
    result = subprocess.run(['useradd', '-M', '-N', '-l', '-g', group.gr_name,
                             '-d', str(root / 'home'), '-s', '/bin/sh', user],
                            capture_output=True, text=True, timeout=15)
    if result.returncode:
        raise RuntimeError('Owned useradd failed: ' + result.stderr[:4096])
    account = pwd.getpwnam(user)
    record['uid'] = account.pw_uid
    marker.write_text(json.dumps(record))
    validate_account(record, account)
    for path in (root / 'home', root / 'home/.ssh'):
        path.mkdir(mode=0o700)
        os.chown(path, account.pw_uid, account.pw_gid)
    key = root / 'home/.ssh/authorized_keys'
    with key.open('x') as stream:
        key.chmod(0o600)
        stream.write(public_key)
    os.chown(key, account.pw_uid, account.pw_gid)
    groups = os.getgrouplist(user, account.pw_gid)
    if groups != [group.gr_gid]:
        raise RuntimeError('Unexpected supplemental group')
    denied = subprocess.run(['runuser', '-u', user, '--', 'sudo', '-n', 'true'],
                            capture_output=True, timeout=10)
    if denied.returncode == 0:
        raise RuntimeError('Test account unexpectedly has sudo access')
    return dict(user=user, uid=account.pw_uid, groups=groups, sudo_denied=True)


def remove(nonce):
    user, root = identity(nonce)
    marker = root / 'owner.json'
    regular(marker, 0, 0o600)
    record = json.loads(marker.read_text())
    if record['nonce'] != nonce:
        raise RuntimeError('Owner marker changed')
    try:
        account = pwd.getpwnam(user)
    except KeyError:
        if record['uid'] is not None:
            raise RuntimeError('Previously created account disappeared; preserving evidence')
        # useradd rejected before creating any account/home. Remove only its
        # root-owned reservation, after proving there are no other contents.
        directory(root, 0, ['owner.json'])
        marker.unlink()
        root.rmdir()
        return dict(account_never_created=True, fixture_directory_removed=True)
    validate_account(record, account)
    # PAM/systemd may retain the owned user manager briefly after the last SSH
    # channel closes. Wait for normal retirement; never kill an unknown process.
    started = time.monotonic()
    while True:
        active = False
        for process in Path('/proc').iterdir():
            if process.name.isdecimal():
                try:
                    active |= process.stat().st_uid == account.pw_uid
                except FileNotFoundError:
                    pass
        if not active:
            break
        if time.monotonic() - started >= 20:
            raise RuntimeError('Owned account process still active; preserving it')
        time.sleep(.2)
    directory(root, 0, ['owner.json', 'home'])
    motd = validate_home(root / 'home', account.pw_uid)
    directory(root / 'home/.ssh', account.pw_uid, ['authorized_keys'])
    key = root / 'home/.ssh/authorized_keys'
    regular(key, account.pw_uid, 0o600)
    if hashlib.sha256(key.read_bytes()).hexdigest() != record['key_sha256']:
        raise RuntimeError('Authorized key changed; preserving it')
    # All checks precede deletion. Never userdel -r, rmtree or broad unlink.
    subprocess.run(['userdel', user], check=True, capture_output=True, timeout=15)
    key.unlink()
    (root / 'home/.ssh').rmdir()
    if motd is not None:
        motd.unlink()
        motd.parent.rmdir()
    (root / 'home').rmdir()
    marker.unlink()
    root.rmdir()
    return dict(account_removed=True, fixture_directory_removed=True,
                normal_process_retirement_seconds=round(time.monotonic() - started, 3))


if __name__ == '__main__':
    if os.getuid() != 0:
        raise RuntimeError('Guest root required')
    # SETTINGS is prepended by the host via SSH stdin, never from shell text.
    action = SETTINGS['action']
    result = (snapshot() if action == 'snapshot' else
              create(SETTINGS['nonce'], SETTINGS['public_key']) if action == 'create' else
              remove(SETTINGS['nonce']) if action == 'remove' else None)
    if result is None:
        raise ValueError('Unknown owned fixture action')
    print(json.dumps(result))
