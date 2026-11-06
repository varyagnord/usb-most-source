#!/usr/bin/env python3
"""Long-lived SSH + kernel VHCI fixture, with periodic input and memory evidence.

Only owned QEMU guests; no physical USB or mounts. Keep the log and report on
failure/interruption. This is a continuous synthetic HID baseline, not Android
battery evidence or a substitute for mixed-device/network-transition tests.
"""
import argparse
import datetime
import fcntl
import hashlib
import json
from pathlib import Path
import signal
import subprocess
import sys
import time
from lab_usb_reader import SCRIPT

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lab', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--seconds', type=int, default=28800)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--fixture', type=Path,
                        default=ROOT / 'build/rust-connect-new-nightly/debug/examples/receiver_lab',
                        help='Separately built owned host fixture; never replace a running soak binary')
    args = parser.parse_args()
    assert 30 <= args.seconds <= 86400
    assert args.tag.replace('-', '').replace('_', '').isalnum()
    lab = args.lab.resolve()
    assert lab.is_relative_to(ROOT / 'build/vm-lab')
    meta = json.loads((lab / 'connection.json').read_text())
    assert meta['ssh_host'] == '127.0.0.1' and meta['ssh_user'] == 'fixture'
    command = Path('/proc', (lab / 'pid').read_text().strip(), 'cmdline').read_bytes()
    assert b'qemu-system-x86_64' in command and str(lab / 'guest.qcow2').encode() in command
    lock = (lab / 'soak.lock').open('a')
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    output = lab / (args.tag + '-soak.json')
    log = lab / (args.tag + '-soak-session.log')
    assert not output.exists() and not log.exists(), 'Use a new tag; never overwrite evidence'
    ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(meta['ssh_port']),
           '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'ConnectTimeout=10',
           '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'),
           'fixture@127.0.0.1', 'sudo -n python3 -']
    executable = args.fixture.resolve()
    assert executable.is_relative_to(ROOT / 'build') and executable.is_file()
    binary = args.artifacts.resolve() / 'usb-most-linux-x86_64'
    report = {
        'schema': 1, 'status': 'running', 'requested_seconds': args.seconds,
        'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'worktree_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
        'runner_sha256': hashlib.file_digest(Path(__file__).open('rb'), 'sha256').hexdigest(),
        'fixture_sha256': hashlib.file_digest(executable.open('rb'), 'sha256').hexdigest(),
        'receiver_sha256': hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest(),
        'samples': [], 'input_windows': [],
        'scope': 'Continuous SSH, real guest VHCI and synthetic HID; no Android energy or physical USB claim.',
    }

    def save():
        output.write_text(json.dumps(report, indent=2) + '\n')

    def remote(script, timeout):
        result = subprocess.run(ssh, input=script, text=True, capture_output=True, timeout=timeout)
        assert result.returncode == 0, result.stderr[-4096:]
        return json.loads(result.stdout)

    def interrupted(_signum, _frame):
        raise KeyboardInterrupt('Soak interrupted')

    signal.signal(signal.SIGTERM, interrupted)
    save()
    started = time.monotonic()
    process = None
    try:
        with log.open('xb') as stream:
            process = subprocess.Popen([str(executable), str(lab), str(args.artifacts.resolve()), str(args.seconds)], cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT)
            report['fixture_pid'] = process.pid
            save()
            connected = None
            next_sample = 0
            next_input = 0
            while process.poll() is None:
                elapsed = time.monotonic() - started
                text = log.read_text()
                if 'STATE connected' in text and connected is None:
                    connected = time.monotonic()
                    next_sample = connected + 5
                    next_input = connected + 10
                    report['connected_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
                if connected is None:
                    assert elapsed < 250, 'Connection deadline exceeded'
                else:
                    now = time.monotonic()
                    if now >= next_sample:
                        sample = remote(r'''
import json,subprocess
from pathlib import Path
binary='/usr/local/libexec/usb-most/usb-most-desktop'
health=json.loads(subprocess.check_output([binary,'--health'],timeout=10))['status']
pid=int(subprocess.check_output(['systemctl','show','usb-most-desktop.service','--property=MainPID','--value']))
assert pid>0 and health['stack'] and health['connected'] and health['active']==1
memory={line.split(':')[0]:int(line.split()[1]) for line in Path('/proc',str(pid),'status').read_text().splitlines() if line.startswith(('VmRSS:','VmHWM:','Threads:'))}
print(json.dumps({'receiver_pid':pid,'receiver_memory':memory,'active':health['active']}))
''', 30)
                        host = Path('/proc', str(process.pid), 'status').read_text()
                        sample['fixture_memory'] = {line.split(':')[0]: int(line.split()[1]) for line in host.splitlines() if line.startswith(('VmRSS:', 'VmHWM:', 'Threads:'))}
                        sample['elapsed_seconds'] = round(elapsed, 2)
                        report['samples'].append(sample)
                        assert sample['receiver_memory']['VmRSS'] < 256 * 1024, 'Receiver exceeds the 256 MiB soak bound'
                        assert sample['fixture_memory']['VmRSS'] < 256 * 1024, 'Fixture exceeds the 256 MiB soak bound'
                        save()
                        next_sample = time.monotonic() + 30
                    if now >= next_input and args.seconds - (now - connected) > 35:
                        reader = SCRIPT.replace('ENUMERATION_SECONDS', '20').replace('READ_SECONDS', '10')
                        report['input_windows'].append({'elapsed_seconds': round(elapsed, 2), 'result': remote(reader, 60)})
                        save()
                        print('Input window passed at', round(elapsed), 'seconds', flush=True)
                        next_input = time.monotonic() + 3600
                assert elapsed <= args.seconds + 300, 'Soak deadline exceeded'
                time.sleep(1)
        assert process.returncode == 0, 'Fixture failed; inspect retained session log'
        report['elapsed_seconds'] = time.monotonic() - started
        assert connected is not None and time.monotonic() - connected >= args.seconds - 2
        assert log.read_text().count('STATE connected') == 1, 'Continuous session reconnected unexpectedly'
        assert report['samples'] and report['input_windows'], 'Required samples missing'
        report['usb'] = json.loads((lab / 'usb-result.json').read_text())
        assert report['usb']['imports'] == 1 and report['usb']['reports'] > 0
        report['status'] = 'passed'
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report['status'] = 'failed'
        report['failure'] = str(error)[:4096]
        print(report['failure'], file=sys.stderr)
        return 1
    finally:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        report['finished_utc'] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        save()
        lock.close()
        print('Evidence:', output, 'status:', report['status'], flush=True)


if __name__ == '__main__':
    sys.exit(main())
