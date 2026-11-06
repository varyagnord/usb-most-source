#!/usr/bin/env python3
"""Real systemd install/idempotence/busy/health rollback, only in our QEMU lab."""
from pathlib import Path
import argparse,json,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--lab',type=Path,required=True);p.add_argument('--artifacts',type=Path,required=True)
p.add_argument('--fixture',type=Path,default=ROOT/'build/rust-connect-new-nightly/debug/examples/receiver_lab')
p.add_argument('--tag',default='installer')
a=p.parse_args();lab=a.lab.resolve();assert lab.is_relative_to(ROOT/'build/vm-lab')
assert a.tag.replace('-','').replace('_','').isalnum()
fixture=a.fixture.resolve();assert fixture.is_relative_to(ROOT/'build') and fixture.is_file()
for suffix in ['-session.log','-result.txt']:
 if (lab/(a.tag+suffix)).exists():raise FileExistsError('Retain evidence; choose a new tag')
m=json.loads((lab/'connection.json').read_text());assert m['ssh_host']=='127.0.0.1' and m['ssh_user']=='fixture'
pid=int((lab/'pid').read_text());cmd=Path('/proc',str(pid),'cmdline').read_bytes()
assert b'qemu-system-x86_64' in cmd and str(lab/'guest.qcow2').encode() in cmd
ssh=['ssh','-F','/dev/null','-i',str(lab/'client'),'-p',str(m['ssh_port']),'-o','BatchMode=yes','-o','IdentitiesOnly=yes','-o','StrictHostKeyChecking=yes','-o','UserKnownHostsFile='+str(lab/'known_hosts'),'fixture@127.0.0.1']
common=r'''
import hashlib,json,os,stat,subprocess,time
from pathlib import Path
binary='/usr/local/libexec/usb-most/usb-most-desktop'
paths=[Path(binary),Path('/var/lib/usb-most/receiver.json'),Path('/etc/systemd/system/usb-most-desktop.service'),Path('/etc/modules-load.d/usb-most.conf'),Path('/var/lib/usb-most/active')]
def snapshot():
 result={str(p):[hashlib.sha256(p.read_bytes()).hexdigest(),stat.S_IMODE(p.stat().st_mode),p.stat().st_mtime_ns] for p in paths}
 for property in ['MainPID','ActiveState','UnitFileState']:
  result[property]=subprocess.check_output(['/usr/bin/systemctl','show','usb-most-desktop.service','--property='+property,'--value'],text=True).strip()
 assert not Path('/var/lib/usb-most/install-journal.json').exists()
 return result
'''
busy=r'''
before=snapshot();assert before['ActiveState']=='active' and before['UnitFileState']=='enabled'
original=paths[1].read_bytes()
import base64
helper=CONFIGURE_HELPER
expected=before[str(paths[0])][0]
payload=('UMCONF1\n'+expected+'\n').encode()+base64.b64encode(original)+b'\n'
assert len(payload)<8192,'configuration-only payload must not contain the executable'
for prefix in [b'',b'fixture-password\n',b'UMCONF1\n']:
 r=subprocess.run(['/bin/sh','-c',helper],input=prefix+payload,capture_output=True,timeout=15)
 assert r.returncode==0,r.stderr
 assert snapshot()==before,'verified configuration-only preparation mutated an identical deployment'
print('PASS configuration-only root helper: tiny payload, unread sudo password/marker and identical deployment',flush=True)
for invalid,exitcode in [
 (payload.replace(expected.encode(),b'0'*64,1),78),
 (payload+b'extra-transaction\n',70),
 (('UMCONF1\n'+expected+'\n').encode()+base64.b64encode(b'invalid-json')+b'\n',79),
]:
 r=subprocess.run(['/bin/sh','-c',helper],input=invalid,capture_output=True,timeout=15)
 assert r.returncode==exitcode,(r.returncode,r.stderr)
 assert snapshot()==before,'invalid configure-only request changed the receiver'
print('PASS root helper independently rejects changed hash, extra stdin and invalid config without mutation',flush=True)
started=time.monotonic();r=subprocess.run([binary,'--install-stdin'],input=original,capture_output=True,timeout=20)
assert r.returncode==0,r.stderr
assert time.monotonic()-started<10
assert snapshot()==before,'identical deployment mutated files or restarted service'
print('PASS real systemd idempotence: files, mtime and MainPID preserved',flush=True)
config=json.loads(original);config['peer']='ffffffffffffffffffffffffffffffff';config['proof']='AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE'
started=time.monotonic();r=subprocess.run([binary,'--install-stdin'],input=json.dumps(config).encode(),capture_output=True,timeout=20)
assert r.returncode!=0 and b'WouldBlock' in r.stderr,r.stderr
assert time.monotonic()-started<10
assert snapshot()==before,'foreign live peer changed files/service'
print('PASS foreign active peer rejected before installation',flush=True)
'''
busy=busy.replace('CONFIGURE_HELPER',repr((ROOT/'native/usb-most-connection/src/bootstrap/configure-linux.sh').read_text()))
rollback=r'''
before=snapshot();original=paths[1].read_bytes();config=json.loads(original)
config['peer']='ffffffffffffffffffffffffffffffff';config['port']=65534;config['proof']='AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE'
# The high loopback port has no fixture listener. Authentication/health must fail.
started=time.monotonic();r=subprocess.run([binary,'--install-stdin'],input=json.dumps(config).encode(),capture_output=True,timeout=100)
assert r.returncode!=0,r.stdout
assert time.monotonic()-started>=40,'expected real health timeout'
after=snapshot()
assert after['ActiveState']==before['ActiveState'] and after['UnitFileState']==before['UnitFileState']
for p in paths:assert after[str(p)][:2]==before[str(p)][:2],f'rollback failed {p}'
print('PASS real failed health restores binary/config/unit/module file/activation/modes and service state',flush=True)
'''
log=lab/(a.tag+'-session.log')
def remote(script):
 r=subprocess.run(ssh+['sudo -n python3 -'],input=common+script,text=True,capture_output=True,timeout=130)
 assert r.returncode==0,r.stderr+'\n'+r.stdout
 return r.stdout
with log.open('wb') as out:
 proc=subprocess.Popen([str(fixture),str(lab),str(a.artifacts.resolve()),'30'],stdout=out,stderr=subprocess.STDOUT,cwd=ROOT)
 try:
  until=time.monotonic()+120
  while time.monotonic()<until:
   if 'STATE connected' in log.read_text():break
   if proc.poll() is not None:raise RuntimeError(log.read_text())
   time.sleep(.2)
  else:raise RuntimeError('session did not become ready')
  # Wait for the kernel import, not merely the installation health probe.
  time.sleep(3)
  checks=remote(busy)
  assert proc.wait(timeout=50)==0,log.read_text()
 finally:
  if proc.poll() is None:proc.terminate();proc.wait(timeout=10)
time.sleep(5) # bounded supervisor polling after our owned SSH connection is closed
checks+=remote(rollback)
(lab/(a.tag+'-result.txt')).write_text(checks)
print(checks,end='')
