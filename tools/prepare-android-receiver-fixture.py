#!/usr/bin/env python3
"""Private TEST-APK assets for our isolated VM. Never production receiver credentials."""
from pathlib import Path
import argparse,base64,hashlib,json,os,secrets,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--lab',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
lab=a.lab.resolve();output=a.output.resolve()
assert lab.is_relative_to(ROOT/'build/vm-lab') and output.is_relative_to(ROOT/'build/e2e')
m=json.loads((lab/'connection.json').read_text());assert m['ssh_host']=='127.0.0.1' and m['ssh_user']=='fixture'
pid=int((lab/'pid').read_text());cmd=Path('/proc',str(pid),'cmdline').read_bytes();assert b'qemu-system-x86_64' in cmd and str(lab/'guest.qcow2').encode() in cmd
ssh=['ssh','-F','/dev/null','-i',str(lab/'client'),'-p',str(m['ssh_port']),'-o','BatchMode=yes','-o','IdentitiesOnly=yes','-o','ConnectTimeout=3','-o','StrictHostKeyChecking=accept-new','-o','UserKnownHostsFile='+str(lab/'known_hosts'),'fixture@127.0.0.1']
until=time.monotonic()+100
while True:
 r=subprocess.run(ssh+['cat /etc/ssh/ssh_host_ed25519_key.pub /etc/ssh/ssh_host_ecdsa_key.pub /etc/ssh/ssh_host_rsa_key.pub'],capture_output=True,text=True,timeout=8)
 if r.returncode==0:break
 if time.monotonic()>=until:raise RuntimeError('isolated VM SSH did not start')
 time.sleep(1)
pins=[base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(line.split()[1])).digest()).decode().rstrip('=') for line in r.stdout.splitlines()]
output.mkdir(mode=0o700,parents=True,exist_ok=True);output.chmod(0o700)
metadata=output/'receiver-fixture.json'
if metadata.exists():
 assert not metadata.is_symlink() and metadata.stat().st_mode & 0o077 == 0
 old=json.loads(metadata.read_text());assert old['port']==m['ssh_port'] and old['host_pins']==pins
 password=old['sudo_password']
else:
 password=secrets.token_urlsafe(24)
 payload={'scope':'isolated-QEMU-only','host':'10.0.2.2','port':m['ssh_port'],'user':'fixture','host_pins':pins,'private_key':(lab/'client').read_text(),'sudo_password':password}
 # Persist the recovery credential BEFORE changing the disposable VM. A crash
 # after sudo policy changes must not strand the fixture without its password.
 fd=os.open(metadata,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
 with os.fdopen(fd,'w') as f:json.dump(payload,f);f.flush();os.fsync(f.fileno())
 fd=os.open(output,os.O_RDONLY|os.O_DIRECTORY)
 try:os.fsync(fd)
 finally:os.close(fd)
assert len(password)<=128 and all(c.isalnum() or c in '_-' for c in password)
# stdin, never argv/logs. The VM has no host USB/filesystem passthrough or egress.
script='''from pathlib import Path
import subprocess
password=PASSWORD
subprocess.run(['/usr/sbin/chpasswd'],input=('fixture:'+password+'\\n').encode(),check=True)
p=Path('/etc/sudoers.d/99-usb-most-fixture-password')
desired='fixture ALL=(ALL) PASSWD: ALL\\n'
if p.exists():assert not p.is_symlink() and p.read_text()==desired
else:p.write_text(desired);p.chmod(0o440)
subprocess.run(['/usr/sbin/visudo','-cf',str(p)],check=True,stdout=subprocess.DEVNULL)
'''.replace('PASSWORD',repr(password))
import shlex
# Consume only an unread NOPASSWD credential line, then pass Python its source.
decoder="IFS= read -r marker; if [ \"$marker\" != UMFIXTURE1 ]; then IFS= read -r marker; fi; [ \"$marker\" = UMFIXTURE1 ] || exit 70; exec /usr/bin/python3 -"
r=subprocess.run(ssh+["sudo -k -S -p '' -- /bin/sh -c "+shlex.quote(decoder)],input=password+'\nUMFIXTURE1\n'+script,capture_output=True,text=True,timeout=15)
assert r.returncode==0,r.stderr
print('Prepared private TEST-APK-only credentials for isolated QEMU, with password-required sudo')
