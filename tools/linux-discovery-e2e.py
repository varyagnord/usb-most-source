#!/usr/bin/env python3
"""Real receiver mDNS publication/query/goodbye, inside our owned Linux VM only."""
from pathlib import Path
import argparse
import json
import subprocess

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--lab', type=Path, required=True)
p.add_argument('--tag', default='discovery')
a = p.parse_args()
lab = a.lab.resolve()
assert lab.is_relative_to(ROOT / 'build/vm-lab')
assert a.tag.replace('-', '').replace('_', '').isalnum()
output = lab / (a.tag + '-result.txt')
if output.exists():
    raise FileExistsError('Retain evidence; choose a new tag')
m = json.loads((lab / 'connection.json').read_text())
assert m['ssh_host'] == '127.0.0.1' and m['ssh_user'] == 'fixture'
command = Path('/proc', (lab / 'pid').read_text().strip(), 'cmdline').read_bytes()
assert b'qemu-system-x86_64' in command and str(lab / 'guest.qcow2').encode() in command
ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(m['ssh_port']),
       '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
       '-o', 'UserKnownHostsFile='+str(lab / 'known_hosts'), 'fixture@127.0.0.1']
script = r'''
import json,socket,struct,subprocess,time
from pathlib import Path
binary='/usr/local/libexec/usb-most/usb-most-desktop'
config=json.dumps({'transport':'direct','port':3241}).encode()
deadline=time.monotonic()+20
while True:
 health=subprocess.run([binary,'--health'],capture_output=True,text=True,timeout=5)
 if health.returncode!=0:break
 state=json.loads(health.stdout)['status']
 if not state['connected'] and state['active']==0:break
 assert time.monotonic()<deadline, 'previous owned fixture has not retired'
 time.sleep(.2)
installed=subprocess.run([binary,'--install-stdin'],input=config,timeout=90,capture_output=True)
assert installed.returncode==0,installed.stderr.decode()
service='_usbmost._tcp.local.'
def wire(name):return b''.join(bytes([len(p)])+p.encode() for p in name.rstrip('.').split('.'))+b'\0'
query=struct.pack('!HHHHHH',0,0,1,0,0,0)+wire(service)+struct.pack('!HH',12,1)
def name(data,at,seen=None):
 seen=set() if seen is None else seen
 assert at not in seen and len(seen)<64
 seen.add(at);parts=[];end=None
 while True:
  length=data[at];at+=1
  if length&192==192:
   offset=((length&63)<<8)|data[at];at+=1
   rest,_=name(data,offset,seen);parts.append(rest.rstrip('.'));break
  assert length<64
  if length==0:break
  parts.append(data[at:at+length].decode());at+=length
 return '.'.join(parts)+'.',at
def records(data):
 _,flags,questions,answers,authorities,additionals=struct.unpack_from('!6H',data)
 if not flags&0x8000:return []
 at=12
 for _ in range(questions):_,at=name(data,at);at+=4
 result=[]
 for _ in range(answers+authorities+additionals):
  owner,at=name(data,at)
  kind,cls,ttl,size=struct.unpack_from('!HHIH',data,at);at+=10
  end=at+size;assert end<=len(data)
  value=None
  if kind==12:value=name(data,at)[0]
  elif kind==33:value=(struct.unpack_from('!HHH',data,at)[2],name(data,at+6)[0])
  elif kind==16:
   value=[];pos=at
   while pos<end:
    n=data[pos];pos+=1;assert pos+n<=end;value.append(data[pos:pos+n].decode());pos+=n
  elif kind==1:value=socket.inet_ntop(socket.AF_INET,data[at:end])
  result.append((owner,kind,ttl,value));at=end
 return result
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
s.bind(('',5353))
interfaces=json.loads(subprocess.check_output(['/usr/sbin/ip','-j','address'],text=True))
addresses=[a['local'] for i in interfaces for a in i['addr_info'] if a['family']=='inet' and a['scope']=='global']
assert len(addresses)==1
# This deliberately offline guest has no default route. Select its real LAN
# interface rather than assuming INADDR_ANY can route multicast.
interface=socket.inet_aton(addresses[0])
s.setsockopt(socket.IPPROTO_IP,socket.IP_ADD_MEMBERSHIP,socket.inet_aton('224.0.0.251')+interface)
s.setsockopt(socket.IPPROTO_IP,socket.IP_MULTICAST_IF,interface)
s.settimeout(.5)
found=[];end=time.monotonic()+20;next_query=0
while time.monotonic()<end:
 if time.monotonic()>=next_query:s.sendto(query,('224.0.0.251',5353));next_query=time.monotonic()+1
 try:found+=records(s.recv(65536))
 except socket.timeout:continue
 if all(any(r[1]==kind and r[2]>0 for r in found) for kind in [12,33,16,1]):break
instances={value for owner,kind,ttl,value in found if owner==service and kind==12 and ttl>0}
assert len(instances)==1, 'receiver PTR missing or ambiguous'
instance=next(iter(instances))
srvs=[value for owner,kind,ttl,value in found if owner==instance and kind==33 and ttl>0]
assert srvs and all(v[0]==3241 for v in srvs), 'wrong advertised receiver port'
txt=[value for owner,kind,ttl,value in found if owner==instance and kind==16 and ttl>0]
assert txt and all(v==['v=1'] for v in txt), 'discovery exposes unexpected metadata'
assert any(owner==srvs[0][1] and kind==1 and not value.startswith('127.') for owner,kind,ttl,value in found)
subprocess.run(['/usr/bin/systemctl','stop','usb-most-desktop.service'],check=True,timeout=30)
goodbye=False;end=time.monotonic()+5
while time.monotonic()<end:
 try:
  if any(ttl==0 and (owner==instance or value==instance) for owner,kind,ttl,value in records(s.recv(65536))):goodbye=True;break
 except socket.timeout:pass
s.close()
assert goodbye, 'receiver did not withdraw its announcement'
print('PASS actual receiver multicast PTR/SRV/TXT/A and TTL0 goodbye; TXT contains version only',flush=True)
'''
# A second network namespace represents another LAN computer. mdns-sd correctly
# disables multicast loopback for its own outgoing packets; two sockets in the
# same namespace would not test an actual LAN peer.
start = script.index("service='_usbmost._tcp.local.'")
query = 'import json,socket,struct,subprocess,time\n' + script[start:]
script = script[:start] + r'''
import uuid
suffix=uuid.uuid4().hex[:8]
namespace='usbmost-mdns-'+suffix
server='umds'+suffix
client='umdc'+suffix
created=False
try:
 subprocess.run(['/usr/sbin/ip','netns','add',namespace],check=True);created=True
 subprocess.run(['/usr/sbin/ip','link','add',server,'type','veth','peer','name',client],check=True)
 subprocess.run(['/usr/sbin/ip','link','set',client,'netns',namespace],check=True)
 subprocess.run(['/usr/sbin/ip','address','add','192.0.2.1/30','dev',server],check=True)
 subprocess.run(['/usr/sbin/ip','link','set',server,'up'],check=True)
 def inside(args):subprocess.run(['/usr/sbin/ip','netns','exec',namespace,*args],check=True)
 inside(['/usr/sbin/ip','link','set','lo','up'])
 inside(['/usr/sbin/ip','address','add','192.0.2.2/30','dev',client])
 inside(['/usr/sbin/ip','link','set',client,'up'])
 time.sleep(6) # Receiver discovers a new interface using its five-second check.
 result=subprocess.run(['/usr/sbin/ip','netns','exec',namespace,'python3','-'],input=QUERY,text=True,capture_output=True,timeout=45)
 print(result.stdout,end='');assert result.returncode==0,result.stderr
finally:
 try:
  if created:subprocess.run(['/usr/sbin/ip','netns','delete',namespace],check=True)
  # Deleting the namespace removes its veth peer, never an existing guest NIC.
 finally:
  # The goodbye test stops the service deliberately. Leave this newly installed
  # fixture running, including when the query or namespace cleanup fails.
  subprocess.run(['/usr/bin/systemctl','start','usb-most-desktop.service'],check=True,timeout=30)
  deadline=time.monotonic()+15
  while True:
   health=subprocess.run([binary,'--health'],capture_output=True,text=True,timeout=5)
   if health.returncode==0:
    state=json.loads(health.stdout)['status']
    assert not state['connected'] and state['active']==0, 'unexpected receiver session after discovery'
    break
   assert time.monotonic()<deadline, 'receiver did not resume after goodbye test'
   time.sleep(.2)
  print('PASS receiver healthy and idle after discovery cleanup',flush=True)
'''.replace('QUERY', repr(query))
result = subprocess.run(ssh + ['sudo -n python3 -'], input=script, text=True, capture_output=True, timeout=160)
output.write_text(result.stdout + result.stderr)
assert result.returncode == 0, result.stderr
print(result.stdout, end='')
