#!/usr/bin/env python3
"""Exercise actual fixed Linux probe against protected/untrusted paths in our VM."""
from pathlib import Path
import argparse, json, subprocess
ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--lab', type=Path, required=True)
a = p.parse_args(); lab = a.lab.resolve()
assert lab.is_relative_to(ROOT / 'build/vm-lab')
m = json.loads((lab / 'connection.json').read_text())
assert m['ssh_host'] == '127.0.0.1' and m['ssh_user'] == 'fixture'
pid = int((lab / 'pid').read_text()); cmd = Path('/proc', str(pid), 'cmdline').read_bytes()
assert b'qemu-system-x86_64' in cmd and str(lab / 'guest.qcow2').encode() in cmd
ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(m['ssh_port']), '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'), 'fixture@127.0.0.1']
script = r'''
from pathlib import Path
import hashlib,json,os,stat,subprocess,uuid
probe=PROBE
binary=Path('/usr/local/libexec/usb-most/usb-most-desktop')
nonce='a'*32
original=hashlib.sha256(binary.read_bytes()).hexdigest()
def run():
 r=subprocess.run(['/bin/sh','-c',probe],input=(nonce+'\n').encode(),capture_output=True,timeout=25)
 assert r.returncode==0,(r.returncode,r.stderr)
 data=json.loads(r.stdout)
 assert data['nonce']==nonce and data['os']=='linux'
 return data
assert run()['agent_sha256']==original
suffix=uuid.uuid4().hex
saved=binary.with_name('probe-backup-'+suffix)
marker=Path('/var/tmp/probe-marker-'+suffix)
mode=stat.S_IMODE(binary.parent.stat().st_mode)
assert not saved.exists() and not marker.exists()
binary.rename(saved)
try:
 # A lookalike would leave this marker if the advisory probe executed it.
 code=('#!/bin/sh\nprintf pwned > '+str(marker)+'\nprintf 1.0.11\n').encode()
 binary.write_bytes(code);binary.chmod(0o777)
 data=run();assert data['agent_sha256'] is None and data['agent_version'] is None
 assert not marker.exists(),'probe executed a writable executable'
 binary.chmod(0o755);binary.parent.chmod(0o777)
 data=run();assert data['agent_sha256'] is None and not marker.exists()
 binary.parent.chmod(mode);binary.unlink();binary.symlink_to(saved)
 data=run();assert data['agent_sha256'] is None and data['agent_version'] is None
 binary.unlink()
 data=run();assert data['agent_sha256'] is None
finally:
 binary.parent.chmod(mode)
 if binary.is_symlink() or binary.is_file():binary.unlink()
 saved.rename(binary)
 if marker.is_file():marker.unlink()
assert run()['agent_sha256']==original
print('PASS Linux probe: trusted hash, writable file/parent, symlink, missing binary, original restoration')
'''.replace('PROBE', repr((ROOT / 'native/usb-most-connection/src/bootstrap/probe-linux.sh').read_text()))
r = subprocess.run(ssh + ['sudo -n python3 -'], input=script, text=True, capture_output=True, timeout=150)
assert r.returncode == 0, r.stderr
print(r.stdout, end='')
