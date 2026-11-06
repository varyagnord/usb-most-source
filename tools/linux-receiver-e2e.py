#!/usr/bin/env python3
"""Exercise the real SSH/session/bootstrap/kernel path in our isolated QEMU VM.
No physical USB, host modules or mounts. Retain all lab files and logs.
"""
from pathlib import Path
import argparse,hashlib,json,os,signal,subprocess,time,socket
ROOT=Path(__file__).resolve().parents[1]
def interrupted(_signum,_frame):raise KeyboardInterrupt('cancelled; restoring owned VM network')
signal.signal(signal.SIGTERM,interrupted)
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--lab',type=Path,required=True)
p.add_argument('--artifacts',type=Path,required=True)
p.add_argument('--tag',default='kernel-hid')
p.add_argument('--delay-ms',type=int,default=0)
p.add_argument('--jitter-ms',type=int,default=0)
p.add_argument('--loss-percent',type=float,default=0)
p.add_argument('--handshake-count',type=int,default=0)
p.add_argument('--direct', action='store_true', help='Actual pinned TLS/pairing instead of SSH data transport')
p.add_argument('--reuse-pairing', action='store_true')
p.add_argument('--fixture', type=Path, default=ROOT/'build/rust-connect-new-nightly/debug/examples/receiver_lab')
a=p.parse_args();lab=a.lab.resolve();assert lab.is_relative_to(ROOT/'build/vm-lab')
assert a.tag.replace('-','').replace('_','').isalnum()
assert 0<=a.delay_ms<=2000 and 0<=a.jitter_ms<=a.delay_ms and 0<=a.loss_percent<=2
assert 0<=a.handshake_count<=100
assert not a.direct or not a.handshake_count
assert not a.reuse_pairing or a.direct
fixture=a.fixture.resolve()
assert fixture.is_relative_to(ROOT/'build') and fixture.is_file()
for suffix in ['-session.log','-result.json','-input.json']:
 if (lab/(a.tag+suffix)).exists():raise FileExistsError('Retain evidence; choose a new tag')
enumeration_seconds=max(20,a.delay_ms*60//1000)
read_seconds=max(10,a.delay_ms*50//1000)
connected_seconds=enumeration_seconds+read_seconds+10
m=json.loads((lab/'connection.json').read_text());assert m['ssh_host']=='127.0.0.1' and m['ssh_user']=='fixture'
pid=int((lab/'pid').read_text());cmd=Path('/proc',str(pid),'cmdline').read_bytes()
assert b'qemu-system-x86_64' in cmd and str(lab/'guest.qcow2').encode() in cmd
ssh=['ssh','-F','/dev/null','-i',str(lab/'client'),'-p',str(m['ssh_port']),'-o','BatchMode=yes','-o','IdentitiesOnly=yes','-o','ConnectTimeout=5','-o','StrictHostKeyChecking=yes','-o','UserKnownHostsFile='+str(lab/'known_hosts'),'fixture@127.0.0.1']
# The fixture USB's unique VID/name cannot match the VM PS/2 or host devices.
from lab_usb_reader import SCRIPT as reader
reader=reader.replace('ENUMERATION_SECONDS',str(enumeration_seconds)).replace('READ_SECONDS',str(read_seconds))

def netem(enable):
 code=r'''
import json,subprocess
from pathlib import Path
interfaces=[p.name for p in Path('/sys/class/net').iterdir() if p.name!='lo']
assert len(interfaces)==1
dev=interfaces[0]
q=json.loads(subprocess.check_output(['/usr/sbin/tc','-j','qdisc','show','dev',dev]))
root=[v for v in q if v.get('root')]
assert all(v['kind'] in ['noqueue','mq','fq_codel','pfifo_fast'] or (v['kind']=='netem' and v['handle']=='111:') for v in root)
ENABLE
'''
 if enable:
  cmd=['/usr/sbin/tc','qdisc','replace','dev','INTERFACE','root','handle','111:','netem','delay',str(a.delay_ms)+'ms',str(a.jitter_ms)+'ms','loss',str(a.loss_percent)+'%']
  statement='args='+repr(cmd)+";args[4]=dev;subprocess.run(args,check=True)"
 else:statement="if any(v['kind']=='netem' and v['handle']=='111:' for v in root):subprocess.run(['/usr/sbin/tc','qdisc','del','dev',dev,'root','handle','111:'],check=True)"
 reply=subprocess.run(ssh+['sudo -n python3 -'],input=code.replace('ENABLE',statement),capture_output=True,text=True,timeout=25)
 assert reply.returncode==0,reply.stderr

log=lab/(a.tag+'-session.log')
direct_port=None
def monitor(command):
 with socket.socket(socket.AF_UNIX) as s:
  s.settimeout(5);s.connect(str(lab/'monitor.sock'))
  output=b''
  while b'(qemu)' not in output:output+=s.recv(4096)
  s.sendall(command.encode()+b'\n');output=b''
  while b'(qemu)' not in output:output+=s.recv(4096)
  assert b'Could not' not in output and b'Error' not in output,output.decode(errors='replace')
if a.direct:
 if a.reuse_pairing:
  direct_port=int((lab/'direct-port').read_text());assert (lab/'direct-token').is_file()
 else:
  with socket.socket() as s:s.bind(('127.0.0.1',0));direct_port=s.getsockname()[1]
  (lab/'direct-port').write_text(str(direct_port))
  (lab/'direct-token').unlink(missing_ok=True) # exact synthetic token, never recursive
 assert 1024<=direct_port<=65535
 preparation=r'''
import json,subprocess,time
binary='/usr/local/libexec/usb-most/usb-most-desktop'
end=time.monotonic()+20
while True:
 h=subprocess.run([binary,'--health'],capture_output=True,text=True,timeout=5)
 if h.returncode:break
 state=json.loads(h.stdout)['status']
 if not state['connected'] and state['active']==0:break
 assert time.monotonic()<end,'previous fixture still active'
 time.sleep(.2)
r=subprocess.run([binary,'--install-stdin'],input=json.dumps({'transport':'direct','port':PORT}).encode(),capture_output=True,timeout=90)
assert r.returncode==0,r.stderr.decode()
r=subprocess.run([binary,'--pair','127.0.0.1'],capture_output=True,timeout=10)
assert r.returncode==0,r.stderr.decode()
print(r.stdout.decode().strip())
'''.replace('PORT',str(direct_port))
 result=subprocess.run(ssh+['sudo -n python3 -'],input=preparation,capture_output=True,text=True,timeout=130)
 assert result.returncode==0,result.stderr
 pair=lab/'direct-pair-code'
 with os.fdopen(os.open(pair,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),'w') as f:f.write(result.stdout.strip())
 monitor(f'hostfwd_add lab tcp:127.0.0.1:{direct_port}-:{direct_port}')
with log.open('wb') as out:
 if a.delay_ms:netem(True)
 command=[str(fixture),str(lab),str(a.artifacts.resolve()),str(connected_seconds)]
 if a.handshake_count:command.append(str(a.handshake_count))
 if a.direct:command.append('direct')
 proc=subprocess.Popen(command,stdout=out,stderr=subprocess.STDOUT,cwd=ROOT)
 try:
  if a.handshake_count:
   exitcode=proc.wait(timeout=a.handshake_count*90)
   result=json.loads((lab/'ssh-stress-result.json').read_text())
   result['status']='PASS' if exitcode==0 and not result['failed'] else 'FAIL'
   result['network']={'egress_delay_ms':a.delay_ms,'jitter_ms':a.jitter_ms,'loss_percent':a.loss_percent}
   (lab/(a.tag+'-result.json')).write_text(json.dumps(result,indent=2)+'\n')
   assert exitcode==0 and result['rounds']==a.handshake_count and not result['failed'],'SSH stress failed; retained '+str(log)
   print(json.dumps(result));raise SystemExit(0)
  until=time.monotonic()+240
  while time.monotonic()<until:
   if 'STATE connected' in log.read_text():break
   if proc.poll() is not None:raise RuntimeError('session failed; see '+str(log))
   time.sleep(.2)
  else:raise RuntimeError('session did not become ready')
  events=subprocess.run(ssh+['sudo -n python3 -'],input=reader,text=True,capture_output=True,timeout=enumeration_seconds+read_seconds+30)
  (lab/(a.tag+'-input.json')).write_text(events.stdout)
  assert events.returncode==0,events.stderr+'\n'+events.stdout
  assert proc.wait(timeout=connected_seconds+30)==0,log.read_text()
  stats=json.loads((lab/'usb-result.json').read_text())
  assert stats['imports']==1,'unexpected USB reimport: '+str(stats)
  result={'result':'PASS','scope':'real VM Linux kernel/VHCI and synthetic HID; no physical USB','transport':'direct TLS' if a.direct else 'SSH','reused_pairing':a.reuse_pairing,'network':{'egress_delay_ms':a.delay_ms,'jitter_ms':a.jitter_ms,'loss_percent':a.loss_percent},'input':json.loads(events.stdout),'usb':stats}
  result['fixture_sha256']=hashlib.sha256(fixture.read_bytes()).hexdigest()
  (lab/(a.tag+'-result.json')).write_text(json.dumps(result,indent=2)+'\n')
  print(json.dumps(result))
 finally:
  if proc.poll() is None:
   proc.terminate()
   try:proc.wait(timeout=10)
   except subprocess.TimeoutExpired:proc.kill();proc.wait()
  if a.delay_ms:netem(False)
  if direct_port:monitor(f'hostfwd_remove lab tcp:127.0.0.1:{direct_port}')
