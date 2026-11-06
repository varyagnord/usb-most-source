#!/usr/bin/env python3
"""Reboot one owned QEMU guest, verify durable receiver state and reuse TLS pairing.

The lab uses QEMU -no-reboot, so its process exits on the guest's reboot request.
Restart the same private overlay and SSH endpoint without changing its host key.
No host devices, mounts, physical USB or real user configuration are touched.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--lab', type=Path, required=True)
p.add_argument('--base', type=Path, required=True)
p.add_argument('--artifacts', type=Path, required=True)
p.add_argument('--slirp-lib', type=Path, required=True)
p.add_argument('--require-enforcing', action='store_true')
p.add_argument('--tag', required=True)
p.add_argument('--fixture', type=Path,
               default=ROOT / 'build/rust-connect-new-nightly/debug/examples/receiver_lab')
p.add_argument('--sudo-group', choices=('sudo', 'wheel'), default='sudo')
a = p.parse_args()
lab = a.lab.resolve()
assert lab.is_relative_to(ROOT/'build/vm-lab')
assert a.tag.replace('-', '').isalnum()
meta = json.loads((lab/'connection.json').read_text())
assert meta['ssh_host'] == '127.0.0.1' and meta['ssh_user'] == 'fixture'
pid = int((lab/'pid').read_text())
process = Path('/proc', str(pid), 'cmdline')
command = process.read_bytes()
assert b'qemu-system-x86_64' in command and str(lab/'guest.qcow2').encode() in command
lab_lock = (lab / 'soak.lock').open('a')
fcntl.flock(lab_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
fixture = a.fixture.resolve()
assert fixture.is_relative_to(ROOT/'build') and fixture.is_file()
port = int(meta['ssh_port'])
assert f'hostfwd=tcp:127.0.0.1:{port}-:22'.encode() in command
assert a.base.resolve().is_relative_to(ROOT/'build/vm-lab')
assert hashlib.file_digest(a.base.open('rb'), 'sha512').hexdigest() == meta['base_sha512']
assert (lab/'known_hosts').is_file() and (lab/'direct-token').is_file()
ssh = ['ssh', '-F', '/dev/null', '-i', str(lab/'client'), '-p', str(port),
       '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'ConnectTimeout=3',
       '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile='+str(lab/'known_hosts'),
       'fixture@127.0.0.1']
snapshot = r'''
import json,hashlib,subprocess,shutil
from pathlib import Path
paths=['/usr/local/libexec/usb-most/usb-most-desktop','/var/lib/usb-most/receiver.json',
 '/var/lib/usb-most/active','/etc/systemd/system/usb-most-desktop.service',
 '/etc/modules-load.d/usb-most.conf','/var/lib/usb-most/identity.json']
health=json.loads(subprocess.check_output([paths[0],'--health'],text=True))
print(json.dumps({'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
 'kernel':subprocess.check_output(['uname','-r'],text=True).strip(),
 'enabled':subprocess.check_output(['systemctl','is-enabled','usb-most-desktop.service'],text=True).strip(),
 'active':subprocess.check_output(['systemctl','is-active','usb-most-desktop.service'],text=True).strip(),
 'selinux':subprocess.run(['getenforce'],capture_output=True,text=True).stdout.strip() if shutil.which('getenforce') else 'unavailable',
 'stack':Path('/sys/module/vhci_hcd').is_dir(),
 'files':{p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in paths if Path(p).exists()},
 'health':health}))
'''
output = lab/(a.tag+'-reboot.json')
assert not output.exists(), 'Use a new evidence tag'
report = {'status': 'running', 'scope': __doc__, 'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
          'runner_sha256': hashlib.file_digest(Path(__file__).open('rb'),'sha256').hexdigest(),
          'fixture_sha256': hashlib.file_digest(fixture.open('rb'),'sha256').hexdigest(),
          'source_commit': subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
          'worktree_dirty': bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT)),
          'expected_receiver_sha256': hashlib.file_digest((a.artifacts/'usb-most-linux-x86_64').open('rb'),'sha256').hexdigest()}

def save():
    output.write_text(json.dumps(report, indent=2)+'\n')

def same_process():
    try:
        return process.read_bytes() == command
    except FileNotFoundError:
        return False

def remote_snapshot():
    result = subprocess.run(ssh+['sudo -n python3 -'], input=snapshot,
                            capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, result.stderr
    data = json.loads(result.stdout)
    assert data['active'] == 'active' and data['enabled'] == 'enabled' and data['stack']
    assert data['health']['status']['active'] == 0 and not data['health']['status']['connected']
    if a.require_enforcing:
        assert data['selinux'] == 'Enforcing'
    return data

try:
    report['before'] = remote_snapshot()
    assert report['before']['files']['/usr/local/libexec/usb-most/usb-most-desktop'] == report['expected_receiver_sha256']
    save()
    result = subprocess.run(ssh+['sudo -n systemctl reboot'], capture_output=True, timeout=15)
    assert result.returncode in (0, 255)
    deadline = time.monotonic()+90
    while same_process():
        assert time.monotonic() < deadline, 'Guest did not complete reboot shutdown'
        time.sleep(.3)
    serial = lab/'serial.log'
    if serial.is_file():
        assert serial.stat().st_size <= 32*1024*1024, 'retain oversized serial log without automatic replacement'
        previous_serial = lab/(a.tag+'-preboot-serial.log')
        with previous_serial.open('xb') as out:
            out.write(serial.read_bytes())
        report['preboot_serial_sha256'] = hashlib.file_digest(previous_serial.open('rb'),'sha256').hexdigest()
    with (lab/(a.tag+'-qemu.log')).open('xb') as log:
        launch = [sys.executable, str(ROOT/'tools/linux-vm-lab.py'),
            '--base', str(a.base.resolve()), '--lab', str(lab), '--sudo-group', a.sudo_group,
            '--slirp-lib', str(a.slirp_lib.resolve()), '--ssh-port', str(port), '--launch']
        if meta.get('usb_eject_fixture'):
            launch.append('--usb-eject-fixture')
        child = subprocess.Popen(launch,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    deadline = time.monotonic()+120
    while True:
        assert child.poll() is None, 'Owned QEMU did not restart; see retained log'
        try:
            after = remote_snapshot()
            break
        except (AssertionError, subprocess.TimeoutExpired):
            assert time.monotonic() < deadline, 'Receiver did not resume after reboot'
            time.sleep(1)
    report['after'] = after
    assert after['boot_id'] != report['before']['boot_id']
    for field in ('kernel','files','selinux'):
        assert after[field] == report['before'][field], 'Unexpected durable state change: '+field
    newpid = int((lab/'pid').read_text())
    newcommand = Path('/proc', str(newpid), 'cmdline').read_bytes()
    assert str(lab/'guest.qcow2').encode() in newcommand and b'qemu-system-x86_64' in newcommand
    save()
    logpath = lab/(a.tag+'-postboot-tls.log')
    with logpath.open('xb') as log:
        result = subprocess.run([sys.executable, str(ROOT/'tools/linux-receiver-e2e.py'),
            '--lab',str(lab),'--artifacts',str(a.artifacts.resolve()),'--tag',a.tag+'-postboot',
            '--fixture',str(fixture),
            '--direct','--reuse-pairing'],stdout=log,stderr=subprocess.STDOUT,timeout=400)
    assert result.returncode == 0, 'Postboot saved pairing failed; see '+str(logpath)
    report['postboot'] = json.loads((lab/(a.tag+'-postboot-result.json')).read_text())
    report['status'] = 'passed'
except BaseException as error:
    report['status'] = 'failed'
    report['failure'] = str(error)
    raise
finally:
    report['finished_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    save()
    print('Reboot evidence:', output, report['status'], flush=True)
