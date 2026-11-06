#!/usr/bin/env python3
"""Real kernel TCP without QEMU NAT: two private namespaces and an isolated sshd.

No existing host interfaces/routes/qdiscs/firewall/sshd are changed. Retain all
files; remove only the two explicitly created, randomly named network objects.
"""
from pathlib import Path
import argparse
import json
import os
import pwd
import signal
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--rounds', type=int, default=30)
parser.add_argument('--delay-ms', type=int, default=1000)
parser.add_argument('--jitter-ms', type=int, default=100)
parser.add_argument('--loss-percent', type=float, default=1)
args = parser.parse_args()
assert 1 <= args.rounds <= 100
assert 0 <= args.jitter_ms <= args.delay_ms <= 2000 and 0 <= args.loss_percent <= 2
suffix = uuid.uuid4().hex[:12]
client_ns, server_ns = 'usbmost-c-' + suffix, 'usbmost-s-' + suffix
base = ROOT / 'build' / ('ssh-netem-' + suffix)
base.mkdir(mode=0o700)
user = pwd.getpwuid(os.getuid()).pw_name
namespaces, daemon, client = [], None, None


def interrupted(_signum, _frame):
    raise KeyboardInterrupt('cancelled; cleaning only owned test namespaces/processes')


signal.signal(signal.SIGTERM, interrupted)


def root(command, **kwargs):
    return subprocess.run(['sudo', '-n', *command], check=True, timeout=20, **kwargs)


def inside(ns, command, **kwargs):
    return root(['/usr/bin/ip', 'netns', 'exec', ns, *command], **kwargs)


for key in ['host', 'client']:
    subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(base / key)], check=True)
(base / 'authorized_keys').write_bytes((base / 'client.pub').read_bytes())
(base / 'authorized_keys').chmod(0o600)
config = base / 'sshd.conf'
config.write_text(f'''Port 38222
ListenAddress 192.0.2.2
HostKey {base / 'host'}
PidFile {base / 'sshd.pid'}
AuthorizedKeysFile {base / 'authorized_keys'}
AllowUsers {user}
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
PermitRootLogin no
AllowTcpForwarding no
AllowAgentForwarding no
X11Forwarding no
PermitTTY no
LogLevel ERROR
''')
root(['/usr/sbin/sshd', '-t', '-f', str(config)])
try:
    for ns in [client_ns, server_ns]:
        root(['/usr/bin/ip', 'netns', 'add', ns])
        namespaces.append(ns)
        inside(ns, ['/usr/bin/ip', 'link', 'set', 'lo', 'up'])
    # The pair is created INSIDE the private server namespace, never the host.
    inside(server_ns, ['/usr/bin/ip', 'link', 'add', 'srv0', 'type', 'veth', 'peer', 'name', 'cli0'])
    inside(server_ns, ['/usr/bin/ip', 'link', 'set', 'cli0', 'netns', client_ns])
    for ns, device, address in [(server_ns, 'srv0', '192.0.2.2/30'), (client_ns, 'cli0', '192.0.2.1/30')]:
        inside(ns, ['/usr/bin/ip', 'address', 'add', address, 'dev', device])
        inside(ns, ['/usr/bin/ip', 'link', 'set', device, 'up'])
    if args.delay_ms or args.loss_percent:
        inside(server_ns, ['/usr/sbin/tc', 'qdisc', 'add', 'dev', 'srv0', 'root', 'handle', '111:',
                           'netem', 'delay', f'{args.delay_ms}ms', f'{args.jitter_ms}ms',
                           'loss', f'{args.loss_percent}%'])
    with (base / 'sshd.log').open('w') as server_log, (base / 'client.log').open('w') as client_log:
        daemon = subprocess.Popen(['sudo', '-n', '/usr/bin/ip', 'netns', 'exec', server_ns,
                                   '/usr/sbin/sshd', '-D', '-e', '-f', str(config)],
                                  stdout=server_log, stderr=subprocess.STDOUT)
        until = time.monotonic() + 10
        while not (base / 'sshd.pid').exists():
            assert daemon.poll() is None and time.monotonic() < until
            time.sleep(.05)
        environment = {
            'USB_MOST_FIXTURE_HOST': '192.0.2.2', 'USB_MOST_FIXTURE_PORT': '38222',
            'USB_MOST_FIXTURE_DENIED_PORT': '38223', 'USB_MOST_FIXTURE_USER': user,
            'USB_MOST_FIXTURE_KEY': str(base / 'client'),
            'USB_MOST_FIXTURE_HOST_PUB': str(base / 'host.pub'),
            'USB_MOST_FIXTURE_KEY_DIRECTORY': str(base),
            'USB_MOST_FIXTURE_STRESS_ROUNDS': str(args.rounds),
        }
        command = ['sudo', '-n', '/usr/bin/ip', 'netns', 'exec', client_ns,
                   '/usr/bin/sudo', '-n', '-u', user, '/usr/bin/env',
                   *[key + '=' + value for key, value in environment.items()],
                   str(ROOT / 'build/rust-connect-new-nightly/debug/examples/ssh_fixture')]
        client = subprocess.Popen(command, stdout=client_log, stderr=subprocess.STDOUT, start_new_session=True)
        exitcode = client.wait(timeout=args.rounds * 90)
        result = json.loads((base / 'stress-result.json').read_text())
        result.update(status='PASS' if exitcode == 0 and not result['failed'] else 'FAIL',
                      network=dict(egress_delay_ms=args.delay_ms, jitter_ms=args.jitter_ms,
                                   loss_percent=args.loss_percent))
        (base / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result), flush=True)
        assert result['status'] == 'PASS', 'inspect retained private client.log and sshd.log'
finally:
    if client is not None and client.poll() is None:
        command = Path('/proc', str(client.pid), 'cmdline').read_bytes()
        assert client_ns.encode() in command and os.getpgid(client.pid) == client.pid
        root(['/bin/kill', '-TERM', '--', '-' + str(client.pid)])
        client.wait(timeout=15)
    if daemon is not None and daemon.poll() is None:
        pid = int((base / 'sshd.pid').read_text())
        command = Path('/proc', str(pid), 'cmdline').read_bytes()
        assert str(config).encode() in command and b'sshd' in command
        root(['/bin/kill', '-TERM', str(pid)])
        daemon.wait(timeout=15)
    for ns in reversed(namespaces):
        root(['/usr/bin/ip', 'netns', 'delete', ns])
