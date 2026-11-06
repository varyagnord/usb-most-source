#!/usr/bin/env python3
"""Actual optimized ART/JNI -> SSH or TLS -> owned QEMU VHCI -> synthetic USB.

ADB reverse supplies a synthetic USB endpoint, not the transport under test.
No physical Android/USB, host routes/drivers, or user credentials are touched.
"""
from pathlib import Path
import argparse, base64, hashlib, json, math, os, re, socket, subprocess, time
import fcntl
from lab_usb_reader import SCRIPT
from lab_tcp_fault import write_json
from lab_local_only_network import OwnedLocalNetwork
from lab_android_active_soak import process_identity, protect_android

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', type=Path, required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--test-apk', type=Path, required=True)
p.add_argument('--lab', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--fixture', type=Path,
               help='Explicit owned synthetic USB executable within build/, for artifact provenance')
p.add_argument('--direct', action='store_true')
p.add_argument('--mixed', action='store_true', help='Concurrent synthetic HID, RAM disk and CCID; owned guest only')
p.add_argument('--delay-ms', type=int, default=0)
p.add_argument('--jitter-ms', type=int, default=0)
p.add_argument('--delay-mode', choices=('serial','pipeline'), default='serial',
               help='serial also limits chunk throughput; pipeline overlaps propagation')
p.add_argument('--ssh-key-name', default='client',
               help='Private authentication fixture filename within the owned lab; never a user key')
p.add_argument('--recovery', action='store_true',
               help='Cut real TCP and exercise the native disconnect acknowledgement barrier')
p.add_argument('--network-handoff', action='store_true',
               help='Use actual owned-emulator Wi-Fi/cellular changes instead of proxy cuts')
p.add_argument('--handoff-start-cellular', action='store_true',
               help='Verify positive cellular-to-Wi-Fi recovery, then refusal on the reverse transition')
p.add_argument('--local-only-network', action='store_true',
               help='Reject public Internet inside the owned emulator; retain only loopback/private LAN')
a = p.parse_args()
lab = a.lab.resolve(); output = a.output.resolve()
if not (lab.is_relative_to(ROOT/'build/vm-lab') and output.is_relative_to(ROOT/'build/e2e')):
 raise ValueError('Only owned lab and evidence directories')
if not re.fullmatch(r'emulator-\d+', a.serial) or a.serial == 'emulator-5566':
 raise ValueError('Only unprotected owned emulators')
protect_android(a.serial)
lockdir=ROOT/'build/e2e/android-locks';lockdir.mkdir(parents=True,exist_ok=True)
android_lock=(lockdir/(a.serial+'.lock')).open('a')
fcntl.flock(android_lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
protected=ROOT/'build/e2e/current-active-service-eight-hour-api36/result.json'
if protected.exists():
 owner=json.loads(protected.read_text())
 if owner.get('serial')==a.serial:
  identity=owner['runner_identity']
  try:live=process_identity(identity['pid'])
  except (FileNotFoundError,ProcessLookupError):live=None
  if live==identity:raise ValueError('Android belongs to the live protected active soak')
if not 0<=a.jitter_ms<=a.delay_ms<=1000:raise ValueError('Invalid bounded network delay')
assert not a.recovery or (a.delay_ms == 0 and a.jitter_ms == 0)
assert not a.network_handoff or a.recovery
assert not a.handoff_start_cellular or a.network_handoff
assert not (a.local_only_network and a.network_handoff)
if a.mixed and a.network_handoff:raise ValueError('Mixed recovery currently requires an exact TCP abort')
steady=(600 if a.delay_ms else 40) if a.mixed else (120 if a.delay_ms else 20)
fixture_binary=ROOT/('build/mixed-fixture113/debug/examples/android_mixed_fixture' if a.mixed else 'build/rust-connect-new-nightly/debug/examples/android_usb_fixture')
if a.fixture:
 fixture_binary=a.fixture.resolve()
 if not fixture_binary.is_relative_to(ROOT/'build') or a.fixture.is_symlink():raise ValueError('Owned compiled fixture within build required')
if not fixture_binary.is_file():raise ValueError('Missing owned USB fixture')
# This endpoint deliberately exposes standard USB/IP, without WAN prefetch.
# Linux queues one HID interrupt read. The fixture's five-button sequence takes
# twelve reports, hence up to twelve network round trips. A fixed 20-second
# window cannot cover it at 2.2-second RTT. Keep every transition assertion and
# budget sixteen round trips plus ten seconds for scheduling/start alignment.
read_seconds=max(10, math.ceil(16 * 2 * (a.delay_ms+a.jitter_ms) / 1000) + 10)
command = Path('/proc', (lab/'pid').read_text().strip(), 'cmdline').read_bytes()
if not (b'qemu-system-x86_64' in command and str(lab/'guest.qcow2').encode() in command):
 raise ValueError('Owned QEMU identity differs')
if a.mixed and any(word in command for word in [b'usb-host',b'vfio-pci',b'-virtfs',b'-fsdev',b'/dev/sd',b'/dev/nvme']):
 raise ValueError('Mixed RAM test refuses host device or filesystem passthrough')
# Share the long-soak lock before any Android action or guest mutation.
lab_lock = (lab / 'soak.lock').open('a')
fcntl.flock(lab_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
metadata = json.loads((lab/'connection.json').read_text())
assert metadata['ssh_host'] == '127.0.0.1' and metadata['ssh_user'] == 'fixture'
key_path = lab / a.ssh_key_name
assert a.ssh_key_name.isascii() and a.ssh_key_name.replace('-', '').isalnum()
assert key_path.is_file() and not key_path.is_symlink() and key_path.parent == lab
assert 0 < key_path.stat().st_size <= 65536
ssh = ['ssh','-F','/dev/null','-i',str(lab/'client'),'-p',str(metadata['ssh_port']),
       '-o','BatchMode=yes','-o','StrictHostKeyChecking=yes','-o','UserKnownHostsFile='+str(lab/'known_hosts'),'fixture@127.0.0.1']
adb = [str(a.adb.resolve()), '-s', a.serial]
def android(*args): return subprocess.run(adb+list(args), check=True, capture_output=True, text=True, timeout=60).stdout.strip()
if android('shell','getprop','ro.kernel.qemu') != '1':raise ValueError('Owned emulator required')
output.mkdir(parents=True, mode=0o700, exist_ok=False)
began=time.monotonic()
observed_phases=[]
app = 'ru.levchatkin.rutokenbridge'
android('shell','am','force-stop',app)
for apk in [a.apk, a.test_apk]: android('install','-r',str(apk.resolve()))
android('shell','input','keyevent','KEYCODE_WAKEUP')
android('shell','wm','dismiss-keyguard')
pins = []
for line in (lab/'known_hosts').read_text().splitlines():
 fields = line.split()
 if len(fields) >= 3:
  pins.append(base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(fields[2])).digest()).decode().rstrip('='))
assert pins
remote_fixture = f'/sdcard/Android/data/{app}/files/transport-fixture.json'
release_marker = os.urandom(16).hex()
remote_release = f'/sdcard/Android/data/{app}/files/recovery-{release_marker}.go'
direct_port = None; reverse_port = None; instrumentation = None; fixture = None; proxy = None
fault_writer = None
fault_workers = []
mixed_nonce=os.urandom(16).hex() if a.mixed else None
mixed_guard='/run/usb-most-mixed-'+mixed_nonce if a.mixed else None
mixed_guard_attempted=False
network_changed = False
network_samples = []
local_network = OwnedLocalNetwork(adb, output / 'local-network.json') if a.local_only_network else None
def network_state(label):
 text=android('shell','dumpsys','connectivity')
 default=re.search(r'^Active default network: (\d+)',text,re.M)
 network=None;transport=None
 if default:
  network=int(default[1])
  for line in text.splitlines():
   if 'NetworkAgentInfo{network{'+str(network)+'}' in line:
    match=re.search(r'Transports: (WIFI|CELLULAR)(?:[ |])',line)
    if match:transport=match[1]
    break
 state={'stage':label,'elapsed_seconds':round(time.monotonic()-began,3),'default_network':network,'transport':transport,
        'wifi_on':android('shell','settings','get','global','wifi_on'),
        'mobile_data':android('shell','settings','get','global','mobile_data')}
 network_samples.append(state)
 write_json(output/'network-transitions.json',network_samples)
 return state
def wait_network(transport,label):
 until=time.monotonic()+45
 while True:
  state=network_state(label)
  if state['transport']==transport:return state
  assert time.monotonic()<until,'Owned emulator did not switch default network to '+transport
  time.sleep(.5)
def restore_network():
 global network_changed
 if not network_changed:return
 android('shell','svc','wifi','enable')
 android('shell','svc','data','enable')
 restored=wait_network('WIFI','restored')
 assert restored['wifi_on']=='1' and restored['mobile_data']=='1','Owned emulator network settings were not restored'
 network_changed=False
def switch_network(transport,label):
 global network_changed
 network_changed=True
 if transport=='WIFI':
  android('shell','svc','wifi','enable')
  wait_network('WIFI',label+'_ready')
  android('shell','svc','data','disable')
 else:
  android('shell','svc','data','enable')
  android('shell','svc','wifi','disable')
 return wait_network(transport,label)
def monitor(command):
 with socket.socket(socket.AF_UNIX) as s:
  s.settimeout(5); s.connect(str(lab/'monitor.sock'))
  data=b''
  while b'(qemu)' not in data: data+=s.recv(4096)
  s.sendall(command.encode()+b'\n'); data=b''
  while b'(qemu)' not in data: data+=s.recv(4096)
  assert b'Could not' not in data and b'Error' not in data
def complete_mixed_round(round_index):
 marker=output/f'mixed-done-{round_index}';marker.write_bytes(b'\x01')
 final=str(Path(remote_fixture).parent/f'mixed-done-{release_marker}-{round_index}.go')
 staging=final+'.part'
 # adb push exposes an empty destination before it finishes writing. Publish
 # only after push completes, with a same-directory atomic rename.
 android('push',str(marker),staging)
 android('shell','mv',staging,final)
log = (output/'instrumentation.log').open('w')
usb_log = (output/'synthetic-usb.log').open('w')
try:
 if local_network:local_network.prepare()
 if a.network_handoff:
  initial=network_state('initial')
  assert initial['transport']=='WIFI' and initial['wifi_on']=='1' and initial['mobile_data']=='1','Handoff fixture requires Wi-Fi and cellular already enabled'
  if a.handoff_start_cellular:switch_network('CELLULAR','prepare_cellular')
 if a.mixed:
  mixed_guard_attempted=True
  script="from pathlib import Path\np=Path(GUARD)\nwith p.open('xb') as f:\n p.chmod(0o600)\n f.write(NONCE.encode())\n".replace('GUARD',repr(mixed_guard)).replace('NONCE',repr(mixed_nonce))
  created=subprocess.run(ssh+['sudo -n python3 -'],input=script,text=True,capture_output=True,timeout=20)
  assert created.returncode==0,created.stderr
 fixture = subprocess.Popen([str(fixture_binary),str(output)],
     env={**os.environ,'USB_MOST_OWNED_EMULATOR_LAB':'1'},stdout=usb_log,stderr=subprocess.STDOUT)
 deadline=time.monotonic()+10
 while not (output/'bridge.json').exists():
  assert fixture.poll() is None and time.monotonic()<deadline
  time.sleep(.05)
 bridge=json.loads((output/'bridge.json').read_text())
 reverse_port=bridge['port']
 android('reverse','--no-rebind',f'tcp:{reverse_port}',f'tcp:{reverse_port}')
 profile={'transport':'ssh','endpoint':{'host':'10.0.2.2','port':metadata['ssh_port']},'user':'fixture','host_key':None,'reverse_port':0,'prepare':True}
 secret={'private_key':key_path.read_text(),'client_id':os.urandom(16).hex()}
 if a.direct:
  with socket.socket() as s: s.bind(('127.0.0.1',0)); port=s.getsockname()[1]
  script=r'''
import json,subprocess,time
binary='/usr/local/libexec/usb-most/usb-most-desktop'
end=time.monotonic()+20
while True:
 h=subprocess.run([binary,'--health'],capture_output=True,text=True,timeout=5)
 if h.returncode:break
 state=json.loads(h.stdout)['status']
 if not state['connected'] and state['active']==0:break
 assert time.monotonic()<end,'previous owned fixture still active'
 time.sleep(.2)
r=subprocess.run([binary,'--install-stdin'],input=json.dumps({'transport':'direct','port':PORT}).encode(),capture_output=True,timeout=90)
assert r.returncode==0,r.stderr.decode()
r=subprocess.run([binary,'--pair','10.0.2.2'],capture_output=True,timeout=10)
assert r.returncode==0,r.stderr.decode()
print(r.stdout.decode().strip())
'''.replace('PORT',str(port))
  result=subprocess.run(ssh+['sudo -n python3 -'],input=script,text=True,capture_output=True,timeout=130)
  assert result.returncode==0,result.stderr
  code=result.stdout.strip(); prefix='usbmost:1:'
  assert code.startswith(prefix)
  encoded=code[len(prefix):]
  pair=json.loads(base64.urlsafe_b64decode(encoded+'='*((-len(encoded))%4)))
  profile={'transport':'direct','endpoint':pair['endpoint'],'server_pin':pair['server_pin']}
  secret={'pair_code':code,'pair_nonce':base64.urlsafe_b64encode(os.urandom(16)).decode().rstrip('=')}
  monitor(f'hostfwd_add lab tcp:127.0.0.1:{port}-:{port}'); direct_port=port
 if a.delay_ms or a.recovery:
  proxy_command=['python3',str(ROOT/'tools'/('lab_tcp_fault.py' if a.recovery else 'lab_tcp_delay.py')),'--output',str(output/'proxy.json'),'--target-port',str(profile['endpoint']['port'])]
  if not a.recovery:proxy_command+=['--delay-ms',str(a.delay_ms),'--jitter-ms',str(a.jitter_ms),'--mode',a.delay_mode]
  proxy=subprocess.Popen(proxy_command,stdout=usb_log,stderr=subprocess.STDOUT)
  until=time.monotonic()+10
  while not (output/'proxy.json').exists():
   assert proxy.poll() is None and time.monotonic()<until
   time.sleep(.05)
  profile['endpoint']['port']=json.loads((output/'proxy.json').read_text())['port']
  if a.direct:
   pair['endpoint']=profile['endpoint']
   secret['pair_code']='usbmost:1:'+base64.urlsafe_b64encode(json.dumps(pair,separators=(',',':')).encode()).decode().rstrip('=')
 config={'steady_seconds':steady,'mixed':a.mixed,'scope':'owned-emulator-QEMU-synthetic-USB','profile':profile,'secret':secret,'proof':bridge['proof'],'bridge_port':reverse_port,'host_pins':pins}
 if a.mixed:config['mixed_done_token']=release_marker
 if a.recovery:config.update(scenario='recovery',release_marker=release_marker)
 config_path=output/'transport-fixture.json'
 config_path.write_text(json.dumps(config)); config_path.chmod(0o600)
 android('shell','mkdir','-p',str(Path(remote_fixture).parent))
 android('push',str(config_path),remote_fixture)
 instrumentation=subprocess.Popen(adb+['shell','am','instrument','-w','-r','-e','suite','transport',app+'.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],stdout=log,stderr=subprocess.STDOUT)
 reports=[]; recovery=None
 def phase(label):
  deadline=time.monotonic()+(300 if a.mixed else 170)
  while label not in (output/'instrumentation.log').read_text():
   assert instrumentation.poll() is None and time.monotonic()<deadline,'Android did not reach '+label+'; inspect private log'
   time.sleep(.1)
  observed_phases.append({'phase':label,'elapsed_seconds':round(time.monotonic()-began,3)})
  write_json(output/'observed-phases.json',observed_phases)
 def input_window(round_index):
  if a.mixed:
   reader=(ROOT/'tools/lab_mixed_usb_reader.py').read_text()
   command=f'sudo -n python3 - --guard {mixed_guard} --nonce {mixed_nonce} --round {round_index} --seconds {read_seconds} --work-seconds {430 if a.delay_ms else 170} --enumeration-seconds {180 if a.delay_ms else 110}'
   try:
    events=subprocess.run(ssh+[command],input=reader,text=True,capture_output=True,timeout=640 if a.delay_ms else 290)
   except subprocess.TimeoutExpired as error:
    for suffix,data in [('json',error.stdout),('stderr',error.stderr)]:
     (output/f'input-round{round_index}.{suffix}').write_bytes(data.encode() if isinstance(data,str) else (data or b''))
    raise
   (output/f'input-round{round_index}.json').write_text(events.stdout)
   (output/f'input-round{round_index}.stderr').write_text(events.stderr)
   assert events.returncode==0,events.stderr
   reports.append(json.loads(events.stdout));return
  reader=SCRIPT.replace('ENUMERATION_SECONDS','90' if a.delay_ms else '15').replace('READ_SECONDS',str(read_seconds))
  events=subprocess.run(ssh+['sudo -n python3 -'],input=reader,text=True,capture_output=True,timeout=100+read_seconds if a.delay_ms else 35)
  (output/f'input-round{round_index}.json').write_text(events.stdout)
  assert events.returncode==0,events.stderr
  reports.append(json.loads(events.stdout))
 def proxy_control(sequence,online):
  write_json(output/'proxy.command.json',dict(sequence=sequence,online=online))
  until=time.monotonic()+10
  while time.monotonic()<until:
   assert proxy.poll() is None,'fault fixture stopped'
   state=json.loads((output/'proxy.state.json').read_text())
   if state['sequence']==sequence:
    assert state['online']==online
    return state
   time.sleep(.05)
  raise AssertionError('fault fixture did not acknowledge control')
 def fence_phase(expected):
  until=time.monotonic()+20
  while time.monotonic()<until:
   assert fixture.poll() is None,'RAM fixture stopped'
   path=output/'write-fence.json'
   if path.exists():
    state=json.loads(path.read_text())
    if state['phase']==expected:return state
    assert state['phase'] not in ('Cancelled','TimedOut'),'write fence failed'
   time.sleep(.02)
  raise AssertionError('RAM write fence did not reach '+expected)
 def fault_guest(mode):
  command=f'sudo -n python3 - --guard {mixed_guard} --nonce {mixed_nonce} --round 0 --seconds 10 --fault {mode}'
  with (output/f'fault-{mode}.json').open('w') as stdout, (output/f'fault-{mode}.stderr').open('w') as stderr:
   process=subprocess.Popen(ssh+[command],stdin=subprocess.PIPE,stdout=stdout,stderr=stderr,text=True)
  fault_workers.append(process)
  process.stdin.write((ROOT/'tools/lab_mixed_usb_reader.py').read_text());process.stdin.close()
  return process
 if a.recovery:
  phase('RECOVERY_CONNECTED round=0');input_window(0)
  write_fault=None
  if a.mixed:
   (output/'arm-write-fence').touch();armed=fence_phase('Armed')
   assert armed['commits']==0 and armed['total_bytes_written']==sum([512,4096,65536,131072])
   fault_writer=fault_guest('write');committed=fence_phase('Held')
   assert fault_writer.poll() is None,'guest write completed before acknowledgement'
   assert committed['commits']==1 and committed['bytes']==4096 and committed['first_import']==1,committed
  if a.network_handoff:
   first_cut=switch_network('WIFI' if a.handoff_start_cellular else 'CELLULAR','positive_recovery')
  else:first_cut=proxy_control(1,False)
  if a.mixed:
   # The outer authenticated channel is already aborted. Only now permit the
   # backend response; no reply can overtake the deliberate network cut.
   (output/'release-write-fence').touch();fence_phase('Released')
  phase('RECOVERY_BARRIER_HELD')
  if a.mixed:
   assert fault_writer.wait(timeout=30)==0,'owned fault write did not retire cleanly'
   write_result=json.loads((output/'fault-write.json').read_text())
   assert write_result['write_result']=='error' and write_result['errno'] in (5,19,6),write_result
  before=proxy_control(1 if a.network_handoff else 2,True)
  time.sleep(2)
  after=json.loads((output/'proxy.state.json').read_text())
  assert before['accepted']==after['accepted'] and before['upstream_opened']==after['upstream_opened'],'TCP retried before barrier acknowledgement'
  # The Android side cannot release the barrier until this owned marker exists.
  marker_file=output/'release-barrier';marker_file.write_bytes(b'1')
  android('push',str(marker_file),remote_release+'.part')
  android('shell','mv',remote_release+'.part',remote_release)
  phase('RECOVERY_BARRIER_RELEASED')
  phase('RECOVERY_CONNECTED round=1');input_window(1)
  if a.mixed:
   readback_process=fault_guest('readback')
   assert readback_process.wait(timeout=30)==0,'committed RAM data did not survive reconnect'
   readback=json.loads((output/'fault-readback.json').read_text())
   assert readback['sha256']==write_result['expected_sha256']
   final_fence=fence_phase('Released')
   assert final_fence['commits']==1 and final_fence['bytes']==4096,final_fence
   write_fault={'committed_before_cut':committed,'write':write_result,'readback':readback,
                'after_reconnect':final_fence,'no_replayed_write':True}
  if a.network_handoff:
   second_cut=switch_network('CELLULAR' if a.handoff_start_cellular else 'WIFI','refused_recovery')
  else:second_cut=proxy_control(3,False)
  phase('RECOVERY_SAVE_REJECTED')
  rejected_before=proxy_control(2 if a.network_handoff else 4,True)
  phase('RECOVERY_FINISHED')
  rejected_after=json.loads((output/'proxy.state.json').read_text())
  assert rejected_before['accepted']==rejected_after['accepted'] and rejected_before['upstream_opened']==rejected_after['upstream_opened'],'TCP retried after rejected storage barrier'
  recovery={'first_cut':first_cut,'held_barrier_before':before,'held_barrier_after':after,
      'second_cut':second_cut,'rejected_barrier_before':rejected_before,'rejected_barrier_after':rejected_after,
      'same_native_session':True,'acknowledgement_adapter':'instrumentation, not actual Android storage consent'}
  recovery['fault_source']='Android emulator Wi-Fi/cellular handoff' if a.network_handoff else 'loopback TCP abort'
  if write_fault is not None:recovery['write_fault']=write_fault
 else:
  for round_index in range(2):
   phase(f'TRANSPORT_CONNECTED round={round_index}');input_window(round_index)
   if a.mixed:
    complete_mixed_round(round_index)
 assert instrumentation.wait(timeout=steady+40)==0
 text=(output/'instrumentation.log').read_text()
 assert 'INSTRUMENTATION_CODE: -1' in text and 'FAIL ' not in text
 (output/'stop-fixture').touch(); assert fixture.wait(timeout=10)==0
 counters=json.loads((output/'usb-result.json').read_text())
 if a.mixed:
  assert all(counters[name]['imports']==2 for name in ['hid','storage','ccid']),counters
  assert counters['storage']['bytes_written']==2*sum([512,4096,65536,131072])+(4096 if a.recovery else 0),counters
  if a.recovery:assert counters['write_fence']['commits']==1 and counters['write_fence']['bytes']==4096,counters
  assert counters['ccid']['commands']==34 and counters['ccid']['exchanges']==28,counters
  assert counters['hid']['reports']>0 and len(reports)==2
 else:assert counters['imports']==2 and counters['controls']>0 and counters['reports']>0,counters
 restore_network()
 if local_network:local_network.assert_isolated('after_two_USB_imports')
 result={'status':'PASS','api':int(android('shell','getprop','ro.build.version.sdk')),'transport':'TLS' if a.direct else 'SSH',
     'apk_sha256':hashlib.sha256(a.apk.read_bytes()).hexdigest(),'test_apk_sha256':hashlib.sha256(a.test_apk.read_bytes()).hexdigest(),
     'scope':'optimized ART/JNI Session, owned QEMU receiver/VHCI, synthetic '+('HID/RAM disk/CCID' if a.mixed else 'mouse')+' via ADB reverse; no Android usbfs or physical USB',
     'delay_each_way_ms':a.delay_ms,'jitter_ms':a.jitter_ms,'read_window_seconds':read_seconds,
     'delay_mode':a.delay_mode if a.delay_ms else None,
     'fixture_protocol':'standard USB/IP; no WAN-prefetch capability',
     'local_only_network':a.local_only_network,
     'fixture_sha256':hashlib.sha256(fixture_binary.read_bytes()).hexdigest(),
     'mixed':a.mixed,
     'mixed_reader_sha256':hashlib.sha256((ROOT/'tools/lab_mixed_usb_reader.py').read_bytes()).hexdigest() if a.mixed else None,
     'runner_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
     'network_fixture_sha256':hashlib.sha256((ROOT/'tools/lab_local_only_network.py').read_bytes()).hexdigest(),
     'ssh_key_fixture':None if a.direct else a.ssh_key_name,
     'observed_phases':observed_phases,
     'recovery':recovery,
     'rounds':2,'buttons':5,'axes_and_wheel':True,'usb':counters}
except BaseException as error:
 (output/'result.json').write_text(json.dumps({'status':'FAIL',
     'apk_sha256':hashlib.sha256(a.apk.read_bytes()).hexdigest(),
     'delay_each_way_ms':a.delay_ms,'jitter_ms':a.jitter_ms,'read_window_seconds':read_seconds,
     'scope':'Owned Android/QEMU synthetic standard-USB/IP transport fixture',
     'error':str(error)},indent=2)+'\n')
 raise
finally:
 cleanup_errors=[]
 def cleanup(label,action):
  try:action()
  except Exception as error:cleanup_errors.append({'step':label,'error_type':type(error).__name__})
 def terminate_owned(process):
  if process is None or process.poll() is not None:return
  process.terminate()
  try:process.wait(timeout=10)
  except subprocess.TimeoutExpired:
   process.kill();process.wait(timeout=5)
   raise
 cleanup('restore emulator network',restore_network)
 cleanup('stop instrumentation',lambda:terminate_owned(instrumentation))
 cleanup('stop owned Android app',lambda:android('shell','am','force-stop',app))
 cleanup('remove owned fixture file',lambda:android('shell','rm','-f',remote_fixture))
 if a.recovery:cleanup('remove owned acknowledgement marker',lambda:android('shell','rm','-f',remote_release))
 if a.recovery:cleanup('remove owned staging acknowledgement marker',lambda:android('shell','rm','-f',remote_release+'.part'))
 if a.mixed:
  for index in range(2):
   cleanup('remove owned mixed completion marker',lambda index=index:android('shell','rm','-f',str(Path(remote_fixture).parent/f'mixed-done-{release_marker}-{index}.go')))
   cleanup('remove owned mixed staging marker',lambda index=index:android('shell','rm','-f',str(Path(remote_fixture).parent/f'mixed-done-{release_marker}-{index}.go.part')))
 if reverse_port:cleanup('remove owned ADB reverse',lambda:android('reverse','--remove',f'tcp:{reverse_port}'))
 cleanup('stop owned TCP proxy',lambda:terminate_owned(proxy))
 if direct_port:cleanup('remove owned VM forward',lambda:monitor(f'hostfwd_remove lab tcp:127.0.0.1:{direct_port}'))
 if fixture and fixture.poll() is None:
  (output/'stop-fixture').touch()
  cleanup('stop synthetic USB endpoint',lambda:fixture.wait(timeout=10))
 for worker in fault_workers:
  cleanup('join owned guest fault operation',lambda worker=worker:worker.wait(timeout=30))
 if mixed_guard_attempted:
  def remove_mixed_guard():
   script="from pathlib import Path\nimport fcntl,time\np=Path(GUARD)\nif p.exists():\n if p.is_symlink() or p.read_bytes()!=NONCE.encode():raise RuntimeError('guard changed; retain it')\n with p.open('r+b') as f:\n  end=time.monotonic()+15\n  while True:\n   try:fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB);break\n   except BlockingIOError:\n    if time.monotonic()>=end:raise RuntimeError('guest I/O still active; retain guard')\n    time.sleep(.2)\n  p.unlink()\n".replace('GUARD',repr(mixed_guard)).replace('NONCE',repr(mixed_nonce))
   result=subprocess.run(ssh+['sudo -n python3 -'],input=script,text=True,capture_output=True,timeout=25)
   if result.returncode:raise RuntimeError('owned mixed guard cleanup failed')
  cleanup('remove exact owned guest guard',remove_mixed_guard)
  def verify_mixed_idle():
   script=r'''
import json,subprocess,time
from pathlib import Path
end=time.monotonic()+20
while True:
 devices=[]
 for p in Path('/sys/bus/usb/devices').iterdir():
  try:
   if (p/'idVendor').read_text().strip()=='cafe' and (p/'idProduct').read_text().strip() in ('0001','0002','0003'):devices.append(p.name)
  except FileNotFoundError:pass
 h=subprocess.run(['/usr/local/libexec/usb-most/usb-most-desktop','--health'],capture_output=True,text=True,timeout=5,check=True)
 status=json.loads(h.stdout)['status']
 if not devices and not status['connected'] and status['active']==0:break
 if time.monotonic()>=end:raise RuntimeError('owned mixed devices or connection still active')
 time.sleep(.2)
print(json.dumps({'connected':False,'active':0,'synthetic_devices':0}))
'''
   probe=subprocess.run(ssh+['sudo -n python3 -'],input=script,text=True,capture_output=True,timeout=30)
   (output/'post-cleanup.json').write_text(probe.stdout)
   (output/'post-cleanup.stderr').write_text(probe.stderr)
   if probe.returncode:raise RuntimeError('owned mixed devices did not retire')
  cleanup('verify receiver and VHCI idle',verify_mixed_idle)
 if local_network:cleanup('restore owned emulator Internet',local_network.close)
 log.close();usb_log.close()
 if cleanup_errors:
  path=output/'result.json'
  failed=json.loads(path.read_text()) if path.exists() else {'transport_checks_status':'PASS'}
  failed.update(status='FAIL',cleanup_errors=cleanup_errors)
  write_json(path,failed)
  raise RuntimeError('Owned transport fixture cleanup failed; inspect result.json')
# Never issue PASS before cleanup and network restoration have completed.
result['cleanup']='passed'
write_json(output/'result.json',result)
print(json.dumps(result))
