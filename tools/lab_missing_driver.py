#!/usr/bin/env python3
"""Read-only evidence for an owned QEMU Linux guest without vhci_hcd.

The host holds the VM ownership lock. No package, module, service, policy or
filesystem mutation is performed by this guest observer.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess

RECEIVER_PATHS = (
    '/usr/local/libexec/usb-most',
    '/etc/systemd/system/usb-most-desktop.service',
    '/etc/modules-load.d/usb-most.conf',
    '/var/lib/usb-most',
)


def command(words):
    result = subprocess.run(words, capture_output=True, text=True, timeout=10)
    return dict(exit=result.returncode, stdout=result.stdout, stderr=result.stderr)


def validate(snapshot):
    if snapshot['selinux'] != dict(exit=0, stdout='Enforcing\n', stderr=''):
        raise RuntimeError('Expected unchanged SELinux Enforcing')
    if snapshot['module']['exit'] != 1 or snapshot['module_loaded']:
        raise RuntimeError('Expected vhci_hcd unavailable, not a working USB stack')
    if (set(snapshot['receiver_paths']) != set(RECEIVER_PATHS)
            or any(snapshot['receiver_paths'].values())):
        raise RuntimeError('Receiver files exist; this is not the untouched prerequisite fixture')
    service = snapshot['service']
    if service['exit'] != 0 or set(service['stdout'].splitlines()) != {
            'MainPID=0', 'LoadState=not-found', 'ActiveState=inactive'}:
        raise RuntimeError('Unexpected receiver service state')
    audit = snapshot['audit']
    if (audit['exit'] != 0 or 'lost 0' not in audit['stdout'].splitlines()
            or 'enabled 1' not in audit['stdout'].splitlines()):
        raise RuntimeError('Audit is unavailable or lost events; do not claim complete AVC observation')


def avc_summary(result):
    empty = (result['exit'] == 1 and not result['stdout'].strip()
             and result['stderr'].strip() in ('', '<no matches>'))
    records = sum(line.startswith('type=') for line in result['stdout'].splitlines())
    matched = result['exit'] == 0 and records > 0 and not result['stderr'].strip()
    if not (empty or matched):
        raise RuntimeError('Could not read AVC evidence')
    return dict(exit=result['exit'], sha256=hashlib.sha256(result['stdout'].encode()).hexdigest(),
                records=records)


def snapshot():
    if os.geteuid() != 0:
        raise RuntimeError('Owned guest root observer required')
    product = Path('/sys/class/dmi/id/product_name').read_text()
    if 'QEMU' not in product and 'Standard PC' not in product:
        raise RuntimeError('Expected owned QEMU guest')
    result = dict(os_release=Path('/etc/os-release').read_text(), kernel=command(['uname', '-r']),
                  selinux=command(['getenforce']), module=command(['modinfo', 'vhci_hcd']),
                  module_loaded=Path('/sys/module/vhci_hcd').exists(),
                  receiver_paths={path: Path(path).exists() or Path(path).is_symlink()
                                  for path in RECEIVER_PATHS},
                  service=command(['systemctl', 'show', 'usb-most-desktop.service',
                                   '-p', 'MainPID', '-p', 'LoadState', '-p', 'ActiveState']),
                  audit=command(['auditctl', '-s']))
    avc = command(['ausearch', '-m', 'AVC,USER_AVC', '--raw'])
    # Do not export unrelated usernames/paths from audit logs. Equality proves
    # no new AVC within this interval; it is not a universal SELinux claim.
    result['avc'] = avc_summary(avc)
    validate(result)
    return result


if __name__ == '__main__':
    if SETTINGS['action'] != 'snapshot':
        raise ValueError('Only read-only prerequisite snapshots are supported')
    print(json.dumps(snapshot()))
