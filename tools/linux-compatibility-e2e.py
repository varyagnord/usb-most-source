#!/usr/bin/env python3
"""Collect actual OS/security evidence and run receiver E2E in an owned VM.

No host mounts, physical USB or security-policy changes. Reports are retained,
including failed runs. A probe alone is not a compatibility pass.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
PROBE = r'''
import json,subprocess,shutil,os
from pathlib import Path
def command(args):
 try:
  p=subprocess.run(args,capture_output=True,text=True,stdin=subprocess.DEVNULL,
   env={**os.environ,'LC_ALL':'C'},timeout=20)
  return {'exit':p.returncode,'stdout':p.stdout[:65536],'stderr':p.stderr[:4096]}
 except FileNotFoundError:return {'unavailable':True}
paths=['/usr/local/libexec/usb-most','/usr/local/libexec/usb-most/usb-most-desktop',
 '/var/lib/usb-most','/var/lib/usb-most/receiver.json','/var/lib/usb-most/active',
 '/etc/systemd/system/usb-most-desktop.service','/etc/modules-load.d/usb-most.conf']
result={
 'os_release':Path('/etc/os-release').read_text(),
 'kernel':command(['uname','-r']),
 'architecture':command(['uname','-m']),
 'selinux':command(['getenforce']),
 'apparmor':command(['aa-status']),
 'apparmor_kernel':Path('/sys/module/apparmor/parameters/enabled').read_text().strip()
   if Path('/sys/module/apparmor/parameters/enabled').exists() else 'unavailable',
 'kernel_module':command(['modinfo','vhci_hcd']),
 'systemd':command(['systemctl','--version']),
 'service':command(['systemctl','show','usb-most-desktop.service',
   '--property=ActiveState,SubState,UnitFileState,MainPID']),
 'labels':{p:command(['ls','-Zd',p]) for p in paths if Path(p).exists()},
 'policy_packages':command(['rpm','-q','selinux-policy','selinux-policy-targeted','policycoreutils']),
 # SSH sends this script through stdin. Explicitly force log input, otherwise
 # ausearch can silently search the exhausted pipe instead of the audit log.
 'avc':command(['ausearch','--input-logs','-m','AVC,USER_AVC','-ts','boot']),
 'audit_status':command(['auditctl','-s']),
 'audit_service':command(['systemctl','is-active','auditd']),
}
print(json.dumps(result))
'''


def audit_clean(snapshot):
    audit = snapshot.get('avc', {})
    status = snapshot.get('audit_status', {})
    service = snapshot.get('audit_service', {})
    if (audit.get('exit') != 1 or audit.get('stdout', '').strip()
            or audit.get('stderr', '').strip() != '<no matches>'
            or status.get('exit') != 0 or status.get('stderr', '').strip()
            or service.get('exit') != 0 or service.get('stdout', '').strip() != 'active'):
        return False
    values = dict(line.split(None, 1) for line in status.get('stdout', '').splitlines()
                  if len(line.split(None, 1)) == 2)
    return (values.get('enabled') in ('1', '2') and values.get('lost') == '0'
            and values.get('pid', '').isdigit() and int(values['pid']) > 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lab', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--require-enforcing', action='store_true')
    parser.add_argument('--probe-only', action='store_true')
    parser.add_argument('--fixture', type=Path,
                        default=ROOT / 'build/rust-connect-new-nightly/debug/examples/receiver_lab')
    args = parser.parse_args()
    lab = args.lab.resolve()
    assert lab.is_relative_to(ROOT / 'build/vm-lab')
    assert args.tag.replace('-', '').replace('_', '').isalnum()
    metadata = json.loads((lab / 'connection.json').read_text())
    assert metadata['ssh_host'] == '127.0.0.1' and metadata['ssh_user'] == 'fixture'
    pid = int((lab / 'pid').read_text())
    process = Path('/proc', str(pid), 'cmdline').read_bytes()
    assert b'qemu-system-x86_64' in process and str(lab / 'guest.qcow2').encode() in process
    lab_lock = (lab / 'soak.lock').open('a')
    fcntl.flock(lab_lock, (fcntl.LOCK_SH if args.probe_only else fcntl.LOCK_EX) | fcntl.LOCK_NB)
    fixture = args.fixture.resolve()
    assert fixture.is_relative_to(ROOT / 'build') and fixture.is_file()
    port = int(metadata['ssh_port'])
    assert f'hostfwd=tcp:127.0.0.1:{port}-:22'.encode() in process
    ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(port),
           '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'ConnectTimeout=10',
           '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'),
           'fixture@127.0.0.1']
    output = lab / ('compatibility-' + args.tag + '.json')
    assert not output.exists(), 'Retain old evidence; use a new tag for a rerun'
    binary = args.artifacts.resolve() / 'usb-most-linux-x86_64'
    report = {
        'schema': 1, 'status': 'running',
        'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'worktree_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
        'runner_sha256': hashlib.file_digest(Path(__file__).open('rb'), 'sha256').hexdigest(),
        'receiver_sha256': hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest(),
        'fixture_sha256': hashlib.file_digest(fixture.open('rb'), 'sha256').hexdigest(),
        'base_sha512': metadata['base_sha512'], 'tests': [],
        'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'scope': 'Owned QEMU VM, synthetic HID. No physical USB, interactive PolicyKit or untested OS claimed.',
    }

    def save():
        output.write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')

    def snapshot():
        result = subprocess.run(ssh + ['sudo -n python3 -'], input=PROBE, capture_output=True, text=True, timeout=180)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    def enforcing(snapshot):
        if args.require_enforcing:
            assert snapshot['selinux'].get('stdout', '').strip() == 'Enforcing', 'SELinux is not enforcing'

    save()
    try:
        report['before'] = snapshot()
        save()
        enforcing(report['before'])
        if args.probe_only:
            report['status'] = 'probe_only'
            return 0
        scenarios = [
            ('ssh', 'linux-receiver-e2e.py', ['--artifacts', str(args.artifacts.resolve()), '--fixture', str(fixture), '--tag', args.tag + '-ssh']),
            ('installer', 'linux-installer-e2e.py', ['--artifacts', str(args.artifacts.resolve()), '--fixture', str(fixture), '--tag', args.tag + '-installer']),
            ('protected-probe', 'receiver-probe-e2e.py', []),
            ('direct', 'linux-receiver-e2e.py', ['--artifacts', str(args.artifacts.resolve()), '--fixture', str(fixture), '--tag', args.tag + '-tls', '--direct']),
            ('direct-reuse', 'linux-receiver-e2e.py', ['--artifacts', str(args.artifacts.resolve()), '--fixture', str(fixture), '--tag', args.tag + '-tls-reuse', '--direct', '--reuse-pairing']),
            ('discovery', 'linux-discovery-e2e.py', ['--tag', args.tag + '-discovery']),
        ]
        for name, script, extra in scenarios:
            log = lab / (args.tag + '-' + name + '.log')
            print('RUN', name, flush=True)
            with log.open('xb') as stream:
                result = subprocess.run([sys.executable, str(ROOT / 'tools' / script), '--lab', str(lab), *extra], cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT, timeout=600)
            record = {'name': name, 'status': 'passed' if result.returncode == 0 else 'failed', 'log': log.name}
            report['tests'].append(record)
            report['after'] = snapshot()
            save()
            enforcing(report['after'])
            assert result.returncode == 0, 'Scenario failed: ' + name + '; see ' + str(log)
        # An AVC review is required even when functional operations succeeded.
        # A missing audit tool is not evidence that there were no denials.
        clean = audit_clean(report['before']) and audit_clean(report['after'])
        report['status'] = 'passed' if not args.require_enforcing or clean else 'audit_review_required'
        return 0 if report['status'] == 'passed' else 2
    except Exception as error:
        report['status'] = 'failed'
        report['failure'] = str(error)[:4096]
        try:
            report['after'] = snapshot()
        except Exception:
            report['after_unavailable'] = True
        print(report['failure'], file=sys.stderr)
        return 1
    finally:
        report['finished_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save()
        print('Evidence:', output, 'status:', report['status'], flush=True)


if __name__ == '__main__':
    sys.exit(main())
