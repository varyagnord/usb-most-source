#!/usr/bin/env python3
"""Actual Android UI/Java server/Rust/TLS -> owned Fedora VHCI/UDisks2.

Only Android usbfs is replaced by a test-APK proxy to two newly created QEMU
images. No physical USB, borrowed VM, arbitrary disk, or recursive cleanup.
The default explicit session does not exercise the foreground-service
coordinator; --foreground-service uses the real saved computer and service.
"""
from pathlib import Path
import argparse
import fcntl
import base64
import hashlib
import json
import os
import re
import select
import socket
import subprocess
import time
from lab_local_only_network import OwnedLocalNetwork
from lab_android_active_soak import observe as active_soak, process_identity, protect_android
from lab_android_storage import owned_serials

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', type=Path, required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--test-apk', type=Path, required=True)
p.add_argument('--lab', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--tag', required=True)
p.add_argument('--usb-ports', required=True, help='Two explicit vacant ports, e.g. 1,2 or 4.6,4.7')
p.add_argument('--delay-ms', type=int, default=0, help='Delay each direction of actual TLS bytes')
p.add_argument('--jitter-ms', type=int, default=0)
p.add_argument('--delay-mode', choices=('serial', 'pipeline'), default='serial')
p.add_argument('--foreground-service', action='store_true',
               help='Actual saved computer/controller/service; only USB request adapter is injected')
p.add_argument('--profile-action', choices=('switch', 'forget'),
               help='Cancel while storage is held; confirm switch/forget only after real OS Safe/readback')
p.add_argument('--revoke', action='store_true', help='Revoke only this run\'s new pairing after safe ejection')
p.add_argument('--manual-fallback', action='store_true',
               help='Require actual Unsupported, then OS unmount/readback before explicit service stop')
p.add_argument('--two-volumes', action='store_true',
               help='Two owned ext4 partitions; second stays busy after first is unmounted')
p.add_argument('--active-writer', action='store_true',
               help='Continuously write/sync a bounded owned file while requesting ejection')
p.add_argument('--active-soak-seconds', type=int, default=0,
               help='60..28800s actual service with synced writes, background and resource samples')
p.add_argument('--require-speed', choices=('full', 'high', 'super'),
               help='Require actual source USB speed12,480 or5000Mb/s before starting Android')
p.add_argument('--trace-udisks', action='store_true',
               help='Owned guest only: profile UDisks D-Bus headers/times without message bodies')
p.add_argument('--local-only-network', action='store_true',
               help='Owned emulator only: reject public internet and verify actual service/diagnosis over local Wi-Fi')
a = p.parse_args()
assert not a.local_only_network or a.foreground_service
lab, output = a.lab.resolve(), a.output.resolve()
assert lab.is_relative_to(ROOT / 'build/vm-lab') and output.is_relative_to(ROOT / 'build/e2e')
protect_android(a.serial)
serials = owned_serials(a.tag)
assert 0 <= a.jitter_ms <= a.delay_ms <= 250
assert not a.revoke or a.foreground_service
assert not a.manual_fallback or (a.foreground_service and not a.revoke)
assert not a.profile_action or (a.foreground_service and not a.revoke and not a.manual_fallback
                               and not a.active_soak_seconds)
assert not a.two_volumes or (a.foreground_service and not a.manual_fallback)
assert not a.active_writer or a.two_volumes
if a.active_soak_seconds and not (60 <= a.active_soak_seconds <= 28800
        and a.foreground_service and a.active_writer and not a.revoke and not a.manual_fallback):
    raise ValueError('Active soak requires bounded actual service and two-volume writer')
ports = a.usb_ports.split(',')
assert len(ports) == 2 and len(set(ports)) == 2 and all(re.fullmatch(r'(?:[1-9]|1[0-6])(?:\.[1-8])?', x) for x in ports)
pid = int((lab / 'pid').read_text())
cmd = Path('/proc', str(pid), 'cmdline').read_bytes()
qemu_identity = process_identity(pid)
assert b'qemu-system-x86_64' in cmd and str(lab / 'guest.qcow2').encode() in cmd
assert b'qemu-xhci,id=eject113-xhci' in cmd and b'usb-host' not in cmd
# Share the long-soak lock before any Android action or guest mutation.
lab_lock = (lab / 'soak.lock').open('a')
fcntl.flock(lab_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
m = json.loads((lab / 'connection.json').read_text())
assert m['ssh_host'] == '127.0.0.1' and m['ssh_user'] == 'fixture' and m['usb_eject_fixture']
ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(m['ssh_port']),
       '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes',
       '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'), 'fixture@127.0.0.1']
adb = [str(a.adb.resolve()), '-s', a.serial]


def android(*args):
    return subprocess.run(adb + list(args), check=True, capture_output=True, text=True, timeout=60).stdout.strip()


assert android('shell', 'getprop', 'ro.kernel.qemu') == '1'
output.mkdir(parents=True, mode=0o700, exist_ok=False)
app = 'ru.levchatkin.rutokenbridge'
directory = f'/sdcard/Android/data/{app}/files'
fixture_file = directory + '/os-ejection-fixture.json'
marker = os.urandom(16).hex()
started = time.monotonic()
report = dict(status='running', scope=__doc__, serial=a.serial, lab=str(lab),
              delay_each_way_ms=a.delay_ms, jitter_ms=a.jitter_ms, delay_mode=a.delay_mode,
              apk_sha256=hashlib.sha256(a.apk.read_bytes()).hexdigest(),
              test_apk_sha256=hashlib.sha256(a.test_apk.read_bytes()).hexdigest(), phases=[], guest=[])
if a.foreground_service:
    report['scope'] = ('Actual UI start/stop, saved computer, durable pairing, ConnectionController, '
                       'foreground service, service-owned Java server, TLS and Fedora UDisks2. '
                       'Test APK injects the USB request/managed-lease adapter before connection admission. '
                       'Android usbfs/UsbSession and the service-specific sessions-map binder are replaced; '
                       'no physical hardware claim.')
report['foreground_service'] = a.foreground_service
report['profile_action'] = a.profile_action
report['remote_revocation'] = a.revoke
report['manual_fallback'] = a.manual_fallback
report['two_volumes'] = a.two_volumes
report['active_writer'] = a.active_writer
report['active_soak_seconds'] = a.active_soak_seconds
report['qemu_identity'] = qemu_identity
report['runner_identity'] = process_identity(os.getpid())
report['required_speed'] = a.require_speed
report['trace_udisks'] = a.trace_udisks
local_network = OwnedLocalNetwork(adb, output / 'local-network.json') if a.local_only_network else None
report['local_only_network'] = a.local_only_network
report['harness_sources'] = {
    str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
    for path in (Path(__file__).resolve(), ROOT / 'tools/lab_android_storage.py', ROOT / 'tools/lab_tcp_delay.py', ROOT / 'tools/lab_local_only_network.py', ROOT / 'tools/lab_android_active_soak.py')
}


def save():
    temp = output / 'result.tmp'
    temp.write_text(json.dumps(report, indent=2) + '\n'); temp.replace(output / 'result.json')


def monitor(command):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(10); s.connect(str(lab / 'monitor.sock'))
        buf = b''
        while b'(qemu)' not in buf:
            chunk = s.recv(8192); assert chunk; buf += chunk
        s.sendall(command.encode() + b'\n'); buf = b''
        while b'(qemu)' not in buf:
            chunk = s.recv(8192); assert chunk; buf += chunk
        result = '\n'.join(buf.decode(errors='replace').split('\r\n')[1:])
        if command.startswith('info '):
            # qtree legitimately contains properties such as account-failed.
            assert not re.search(r'(?mi)^\s*(?:Error:|unknown command|Could not)', result), result
        else:
            assert 'Error' not in result and 'failed' not in result and 'Could not' not in result, result
        return result


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0)); return s.getsockname()[1]


guest = instrumentation = proxy = trace = None
prior_peers = None
trace_out = trace_err = None
forwards, errors, attached = [], [], []
unattached_drives = []
guest_buffer = b''
old_hidden = android('shell', 'settings', 'get', 'global', 'hidden_api_policy')
save()
guest_err = (output / 'guest.stderr').open('w')
instrument_log = (output / 'instrumentation.log').open('w')


def guest_phase(expected, seconds=90):
    global guest_buffer
    until = time.monotonic() + seconds
    while b'\n' not in guest_buffer:
        assert time.monotonic() < until, 'guest deadline: ' + expected
        readable, _, _ = select.select([guest.stdout], [], [], .2)
        if readable:
            chunk = os.read(guest.stdout.fileno(), 8192)
            assert chunk, 'guest exited; inspect retained guest.stderr'
            guest_buffer += chunk
    line, guest_buffer = guest_buffer.split(b'\n', 1)
    event = json.loads(line)
    report['guest'].append(event); save()
    assert event['phase'] == expected, event
    return event


def command(value):
    guest.stdin.write((value + '\n').encode()); guest.stdin.flush()


def phase(value):
    until = time.monotonic() + 300
    while 'PASS OS_EJECT_' + value not in (output / 'instrumentation.log').read_text():
        assert instrumentation.poll() is None and time.monotonic() < until, 'Android phase missing: ' + value
        time.sleep(.1)
    report['phases'].append(dict(phase=value, elapsed_seconds=round(time.monotonic() - started, 3))); save()


def release(value):
    android('shell', 'touch', directory + '/' + marker + '-' + value + '.go')


def admin(request):
    # Proof never leaves the guest or enters a log. This uses the actual
    # receiver's authenticated owner IPC; the loopback address is not authority.
    script = r'''
from pathlib import Path
import base64,hashlib,hmac,json,os,socket,struct
hint=json.loads(Path('/var/lib/usb-most/admin.json').read_text())
proof=base64.urlsafe_b64decode(hint['proof']+'='*(-len(hint['proof'])%4))
assert len(proof)==32 and 1024<=hint['port']<=65535
body=json.dumps(REQUEST,separators=(',',':')).encode();assert len(body)<=1024
with socket.create_connection(('127.0.0.1',hint['port']),5) as s:
 s.settimeout(5)
 def read(n):
  result=b''
  while len(result)<n:
   chunk=s.recv(n-len(result));assert chunk;result+=chunk
  return result
 magic=b'UMGATE2\0';client=os.urandom(32)
 s.sendall(magic+client)
 server=read(32);received=read(32)
 def tag(role):return hmac.new(proof,magic+role+client+server,hashlib.sha256).digest()
 assert hmac.compare_digest(received,tag(b'server'))
 s.sendall(tag(b'client'))
 s.sendall(struct.pack('!H',len(body))+body)
 length=struct.unpack('!H',read(2))[0];assert length<=4096
 print(json.dumps(json.loads(read(length))))
'''.replace('REQUEST', repr(request))
    result = subprocess.run(ssh + ['sudo -n python3 -'], input=script, text=True,
                            capture_output=True, timeout=20)
    assert result.returncode == 0, 'owned admin IPC failed: ' + result.stderr[-4096:]
    return json.loads(result.stdout)


def peers():
    result = admin({'op': 'peers'})
    assert result['result'] == 'peers'
    ids = {tuple(peer[0]) for peer in result['peers']}
    assert len(ids) == len(result['peers'])
    assert all(len(peer) == 16 and all(type(n) is int and 0 <= n <= 255 for n in peer) for peer in ids)
    return ids


try:
    if local_network:
        local_network.prepare()
    # Gate on an idle receiver before provisioning the explicitly owned fixture.
    health = subprocess.run(ssh + ['sudo -n /usr/local/libexec/usb-most/usb-most-desktop --health'],
                            capture_output=True, text=True, check=True, timeout=15)
    state = json.loads(health.stdout)['status']
    assert state['active'] == 0 and not state['connected'], 'receiver is in use'
    identity = subprocess.run(ssh + ['sudo -n sha256sum /usr/local/libexec/usb-most/usb-most-desktop'],
                              capture_output=True, text=True, check=True, timeout=15)
    receiver_hash = identity.stdout.split()[0]
    assert re.fullmatch(r'[a-f0-9]{64}', receiver_hash)
    report['receiver'] = dict(version=state['version'], sha256=receiver_hash)
    save()
    usb = monitor('info usb')
    used = set(re.findall(r'Port ([0-9.]+),', usb))
    assert not set(ports) & used, 'explicit port occupied'
    guest_script = '/home/fixture/android-storage-' + a.tag + '.py'
    subprocess.run(ssh + ['test ! -e ' + guest_script], check=True, timeout=15)
    with (ROOT / 'tools/lab_android_storage.py').open('rb') as source:
        subprocess.run(ssh + ['umask 077; cat > ' + guest_script], stdin=source, check=True, timeout=15)
    preflight = subprocess.run(ssh + ['sudo -n python3 ' + guest_script + ' ' + a.tag + ' preflight'],
                               capture_output=True, text=True, check=True, timeout=15)
    report['guest_preflight'] = json.loads(preflight.stdout)
    save()
    images = []
    report['images_retained'] = images
    for index, suffix in enumerate(('A', 'B')):
        image = lab / f'android-{a.tag}-{index}.raw'
        with image.open('xb') as f:
            f.truncate(64 * 1024 * 1024)
        images.append(str(image))
        save()
        drive = f'and-{a.tag}-drive{index}'
        monitor(f'drive_add 0 if=none,id={drive},file={image},format=raw')
        unattached_drives.append((f'and-{a.tag}-usb{index}', drive))
        monitor(f'device_add usb-storage,id=and-{a.tag}-usb{index},drive={drive},bus=eject113-xhci.0,port={ports[index]},serial={serials[index]}')
        attached.append((f'and-{a.tag}-usb{index}', drive))
        unattached_drives.pop()
    report['images_retained'] = images; save()
    guest_mode = (' two-volumes-writer' if a.active_writer else ' manual' if a.manual_fallback
                  else ' two-volumes' if a.two_volumes else '')
    if a.active_soak_seconds:
        guest_mode += ' ' + str(a.active_soak_seconds)
    guest = subprocess.Popen(ssh + ['sudo -n python3 -u ' + guest_script + ' ' + a.tag + guest_mode],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=guest_err)
    ready = guest_phase('ready')
    if a.require_speed:
        expected_speed = {'full': 12, 'high': 480, 'super': 5000}[a.require_speed]
        assert all(int(float(speed)) == expected_speed for speed in ready['speeds']), 'unexpected source speed'
    backend_port = free_port()
    monitor(f'hostfwd_add lab tcp:127.0.0.1:{backend_port}-:3240'); forwards.append(backend_port)
    direct_port = free_port()
    prepare = '''
import json,subprocess
binary='/usr/local/libexec/usb-most/usb-most-desktop'
r=subprocess.run([binary,'--install-stdin'],input=json.dumps({'transport':'direct','port':PORT}).encode(),capture_output=True,timeout=90)
assert r.returncode==0,r.stderr.decode()
r=subprocess.run([binary,'--pair','10.0.2.2'],capture_output=True,timeout=10)
assert r.returncode==0,r.stderr.decode()
print(r.stdout.decode().strip())
'''.replace('PORT', str(direct_port))
    result = subprocess.run(ssh + ['sudo -n python3 -'], input=prepare, text=True, capture_output=True, timeout=120)
    assert result.returncode == 0, result.stderr
    code = result.stdout.strip(); assert code.startswith('usbmost:1:')
    encoded = code[len('usbmost:1:'):]
    pair = json.loads(base64.urlsafe_b64decode(encoded + '=' * (-len(encoded) % 4)))
    if a.trace_udisks:
        trace_script = r'''
from pathlib import Path
import json,os,re,select,subprocess,sys,time
assert os.geteuid()==0 and 'Q35' in Path('/sys/class/dmi/id/product_name').read_text()
owner=subprocess.check_output(['/usr/bin/busctl','--system','call','org.freedesktop.DBus',
 '/org/freedesktop/DBus','org.freedesktop.DBus','GetNameOwner','s','org.freedesktop.UDisks2'],text=True).strip()
match=re.fullmatch(r's "(:[0-9]+\.[0-9]+)"',owner);assert match
owner=match[1]
child=subprocess.Popen(['/usr/bin/stdbuf','-oL','/usr/bin/busctl','monitor','--system','--json=short',owner],
 stdout=subprocess.PIPE,bufsize=0)
fields=('type','cookie','reply_cookie','timestamp-realtime','sender','destination','path',
 'interface','member','error_name','error-name')
buffer=b'';probe_key=None;ready=False
try:
 time.sleep(.2)
 # Read-only probe proves that the observer is receiving this pinned owner.
 subprocess.run(['/usr/bin/busctl','--system','call',owner,'/org/freedesktop/UDisks2',
  'org.freedesktop.DBus.Introspectable','Introspect'],stdout=subprocess.DEVNULL,check=True,timeout=10)
 while True:
  assert child.poll() is None,'owned D-Bus observer exited'
  readable,_,_=select.select([sys.stdin.fileno(),child.stdout.fileno()],[],[],.2)
  if sys.stdin.fileno() in readable:
   assert sys.stdin.readline().strip()=='quit'
   break
  if child.stdout.fileno() in readable:
   chunk=os.read(child.stdout.fileno(),65536);assert chunk
   buffer+=chunk;assert len(buffer)<=16*1024*1024,'bounded local D-Bus message'
   while b'\n' in buffer:
    line,buffer=buffer.split(b'\n',1)
    if not line.strip():continue
    message=json.loads(line)
    if owner not in (message.get('sender'),message.get('destination')):continue
    # Never write message bodies, volume labels or filesystem contents.
    header={key:message[key] for key in fields if key in message}
    print(json.dumps(header,separators=(',',':')),flush=True)
    if message.get('member')=='Introspect':
     probe_key=(message.get('sender'),message.get('cookie'))
    if not ready and probe_key is not None and message.get('type')=='method_return' and \
       (message.get('destination'),message.get('reply_cookie'))==probe_key:
     ready=True;print('OWNED_UDISKS_PROFILE_READY '+owner,flush=True)
finally:
 if child.poll() is None:child.terminate()
 assert child.wait(timeout=5) in (0,-15)
'''
        trace_path = '/home/fixture/android-udisks-trace-' + a.tag + '.py'
        subprocess.run(ssh + ['test ! -e ' + trace_path], check=True, timeout=15)
        subprocess.run(ssh + ['umask 077; cat > ' + trace_path], input=trace_script.encode(),
                       check=True, timeout=15)
        trace_out = (output / 'udisks-profile.log').open('w')
        trace_err = (output / 'udisks-profile.stderr').open('w')
        trace = subprocess.Popen(ssh + ['sudo -n python3 -u ' + trace_path], stdin=subprocess.PIPE,
                                 stdout=trace_out, stderr=trace_err)
        until = time.monotonic() + 10
        while True:
            captured = (output / 'udisks-profile.log').read_text()
            if 'OWNED_UDISKS_PROFILE_READY ' in captured:
                break
            assert trace.poll() is None and time.monotonic() < until, 'owned D-Bus profile not ready'
            time.sleep(.05)
    prior_peers = peers()
    prior_record = output / 'prior-peer-ids.json'
    prior_record.write_text(json.dumps(sorted(bytes(peer).hex() for peer in prior_peers)) + '\n')
    prior_record.chmod(0o600)
    assert len(prior_peers) < 32, 'owned test receiver pairing capacity reached; preserve prior peers'
    monitor(f'hostfwd_add lab tcp:127.0.0.1:{direct_port}-:{direct_port}'); forwards.append(direct_port)
    if a.delay_ms:
        proxy = subprocess.Popen(['python3', str(ROOT / 'tools/lab_tcp_delay.py'), '--output', str(output / 'delay.json'),
                                  '--target-port', str(direct_port), '--delay-ms', str(a.delay_ms), '--jitter-ms', str(a.jitter_ms), '--mode', a.delay_mode],
                                 stdout=guest_err, stderr=subprocess.STDOUT)
        until = time.monotonic() + 10
        while not (output / 'delay.json').exists():
            assert proxy.poll() is None and time.monotonic() < until
            time.sleep(.05)
        pair['endpoint']['port'] = json.loads((output / 'delay.json').read_text())['port']
        code = 'usbmost:1:' + base64.urlsafe_b64encode(json.dumps(pair, separators=(',', ':')).encode()).decode().rstrip('=')
    config = dict(scope='owned-emulator-QEMU-storage', marker=marker, backend_port=backend_port,
                  foreground_service=a.foreground_service,
                  profile_action=a.profile_action or '',
                  local_only_network=a.local_only_network,
                  active_soak_seconds=a.active_soak_seconds,
                  remote_revocation=a.revoke,
                  manual_fallback=a.manual_fallback,
                  backend_bus=ready['bus'], profile=dict(transport='direct', endpoint=pair['endpoint'], server_pin=pair['server_pin']),
                  secret=dict(pair_code=code, pair_nonce=base64.urlsafe_b64encode(os.urandom(16)).decode().rstrip('=')))
    private = output / 'os-ejection-fixture.json'
    private.write_text(json.dumps(config)); private.chmod(0o600)
    android('shell', 'am', 'force-stop', app)
    for apk in (a.apk, a.test_apk):
        android('install', '-r', str(apk.resolve()))
    android('shell', 'pm', 'clear', app)
    android('shell', 'settings', 'put', 'global', 'hidden_api_policy', '1')
    android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP'); android('shell', 'wm', 'dismiss-keyguard')
    android('shell', 'mkdir', '-p', directory); android('push', str(private), fixture_file)
    instrumentation = subprocess.Popen(adb + ['shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'os-ejection',
                                              app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
                                       stdout=instrument_log, stderr=subprocess.STDOUT)
    report['processes'] = {'instrumentation': process_identity(instrumentation.pid),
                           'guest_ssh': process_identity(guest.pid)}
    if proxy is not None:
        report['processes']['delay_proxy'] = process_identity(proxy.pid)
    save()
    phase('READY'); command('mount'); guest_phase('held', seconds=240); release('held')
    if a.active_soak_seconds:
        phase('SOAK_READY')
        active_soak(a.active_soak_seconds, android=android, command=command,
                    guest_phase=guest_phase, health=lambda: admin({'op': 'health'})['status'],
                    save=save, report=report, qemu=qemu_identity,
                    instrumentation=instrumentation, directory=directory, marker=marker)
        release('soaked'); phase('SOAK_DONE')
    if a.manual_fallback:
        phase('MANUAL'); command('manual'); guest_phase('manual_verified', seconds=300)
        health = admin({'op': 'health'})['status']
        assert health['connected'] and health['active'] == 1, 'manual readback must precede service stop'
        report['manual_connection_preserved'] = True
        release('manualverified')
    else:
        phase('BUSY'); command('busy'); guest_phase('released'); release('released')
        phase('SAFE'); command('safe'); guest_phase('verified'); release('verified')
    if a.revoke:
        phase('REVOKE_CONNECTED')
        health = admin({'op': 'health'})['status']
        assert health['connected'] and health['active'] == 0, 'revoke only after safe storage retirement'
        current_peers = peers()
        assert prior_peers.issubset(current_peers) and len(current_peers - prior_peers) == 1, 'ambiguous pairing owner'
        owned_peer = next(iter(current_peers - prior_peers))
        assert admin({'op': 'revoke', 'id': list(owned_peer)})['result'] == 'revoked'
        assert peers() == prior_peers, 'revocation changed unrelated peers'
        report['revocation'] = dict(one_new_owned_peer_revoked=True, previous_peers_unchanged=True)
        release('revoked')
    assert instrumentation.wait(timeout=120 if a.foreground_service else 40) == 0
    text = (output / 'instrumentation.log').read_text()
    assert 'INSTRUMENTATION_CODE: -1' in text and 'FAIL ' not in text, 'instrumentation failed'
    matched = re.search(r'PASS optimized release API (\d+): (\d+) checks', text)
    assert matched, 'missing actual check count'
    report.update(api=int(matched[1]), checks=int(matched[2]))
    if a.foreground_service:
        if a.profile_action == 'forget':
            assert 'PASS OS_EJECT_PROFILE_FORGOTTEN' in text, 'safe profile forget did not finish'
        else:
            assert 'PASS OS_EJECT_SERVICE_REPEAT' in text, 'service repeat did not finish'
        if a.profile_action:
            assert 'PASS OS_EJECT_PROFILE_STORAGE_CANCELLED' in text
            if a.profile_action == 'switch': assert 'PASS OS_EJECT_PROFILE_SWITCHED' in text
        until = time.monotonic() + 15
        while True:
            result = subprocess.run(ssh + ['sudo -n /usr/local/libexec/usb-most/usb-most-desktop --health'],
                                    capture_output=True, text=True, check=True, timeout=10)
            state = json.loads(result.stdout)['status']
            if not state['connected'] and state['active'] == 0:
                report['after_service_stop'] = dict(connected=False, active=0)
                break
            assert time.monotonic() < until, 'service left receiver connection/import active'
            time.sleep(.2)
    if a.revoke:
        assert 'PASS OS_EJECT_REVOKED_AND_RETRY_REFUSED' in text
        assert peers() == prior_peers, 'refused client silently paired again'
        report['revocation']['no_implicit_pairing_after_retry'] = True
    if a.manual_fallback:
        assert 'PASS OS_EJECT_MANUAL_VERIFIED' in text
        command('stopped'); guest_phase('verified')
    command('quit'); guest_phase('cleaned'); assert guest.wait(timeout=15) == 0
    android('pull', directory + '/os-ejection-proof', str(output / 'screenshots'))
    android('pull', directory + '/os-ejection-states.json', str(output / 'states.json'))
    if local_network:
        assert 'PASS OS_EJECT_LOCAL_NETWORK_DIAGNOSIS' in text
        local_network.assert_isolated('after_actual_service_checks')
    report['status'] = 'passed'
    if a.active_soak_seconds:
        report['active_soak']['status'] = 'duration_and_safe_readback_passed_cleanup_pending'
except BaseException as error:
    report.update(status='failed', error=type(error).__name__ + ': ' + str(error))
    raise
finally:
    # A failure never permits cleanup of mounted contents. Ask the guest to
    # ordinary-unmount its exact paths first; any failure is retained and fails
    # the report. Do not kill the QEMU or its long-lived source daemon blindly.
    if guest is not None and guest.poll() is None:
        try:
            command('quit')
            remaining, _ = guest.communicate(timeout=55)
            for line in (guest_buffer + remaining).splitlines():
                event = json.loads(line)
                report['guest'].append(event)
                if event.get('phase') == 'cleanup_failed':
                    errors.append('guest retained a mounted filesystem')
        except Exception as e:
            errors.append('guest cleanup: ' + type(e).__name__)
    # Only a guest-confirmed ordinary unmount authorizes unplugging these exact
    # synthetic devices. Images are retained. A failed teardown retains sources.
    if any(event.get('phase') == 'cleaned' for event in report['guest']):
        for device, drive in attached:
            try:
                monitor('device_del ' + device)
                until = time.monotonic() + 10
                while 'ID: ' + device in monitor('info usb'):
                    assert time.monotonic() < until, 'owned QEMU unplug deadline'
                    time.sleep(.1)
                monitor('drive_del ' + drive)
            except Exception as e:
                errors.append('owned disk detach: ' + type(e).__name__)
    for device, drive in unattached_drives:
        try:
            # device_add failed after drive_add. A plain unattached drive was
            # never accessible to the guest and cannot contain a mounted FS.
            # If a timeout hid successful attachment, retain it instead.
            if 'ID: ' + device in monitor('info usb'):
                raise RuntimeError('partial attachment exists; retain source')
            monitor('drive_del ' + drive)
        except Exception as e:
            errors.append('unattached owned drive retained: ' + type(e).__name__)
    try:
        android('pull', directory + '/os-ejection-proof', str(output / 'failure-screenshots'))
        android('pull', directory + '/os-ejection-states.json', str(output / 'states.json'))
    except Exception:
        pass
    try:
        android('shell', 'am', 'force-stop', app)
        android('shell', 'rm', '-f', fixture_file)
        if old_hidden == 'null':
            android('shell', 'settings', 'delete', 'global', 'hidden_api_policy')
        else:
            android('shell', 'settings', 'put', 'global', 'hidden_api_policy', old_hidden)
        assert android('shell', 'settings', 'get', 'global', 'hidden_api_policy') == old_hidden
    except Exception as e:
        errors.append('emulator cleanup: ' + type(e).__name__)
    if instrumentation is not None and instrumentation.poll() is None:
        instrumentation.terminate(); instrumentation.wait(timeout=10)
    if proxy is not None and proxy.poll() is None:
        try:
            proxy.terminate(); proxy.wait(timeout=10)
        except subprocess.TimeoutExpired:
            errors.append('owned latency proxy did not retire')
            proxy.kill(); proxy.wait(timeout=5)
    for port in forwards:
        try:
            monitor(f'hostfwd_remove lab tcp:127.0.0.1:{port}')
        except Exception as e:
            errors.append('forward cleanup: ' + type(e).__name__)
    if trace is not None:
        try:
            if trace.poll() is None:
                trace.stdin.write(b'quit\n'); trace.stdin.flush()
            assert trace.wait(timeout=10) == 0
            report['udisks_trace_stopped'] = True
        except Exception as e:
            errors.append('owned D-Bus observer cleanup: ' + type(e).__name__)
    if trace_out is not None:
        trace_out.close()
    if trace_err is not None:
        trace_err.close()
    if prior_peers is not None:
        try:
            # Do not accumulate credentials from repeated owned-VM runs. Only
            # the exact set difference belongs to this invocation; other peers
            # are preserved, and active imports prevent credential retirement.
            current_peers = peers()
            assert prior_peers.issubset(current_peers), 'prior pairing changed during test'
            added = current_peers - prior_peers
            assert len(added) <= 1, 'ambiguous new test pairing'
            if added:
                until = time.monotonic() + 20
                while True:
                    health = admin({'op': 'health'})['status']
                    if not health['connected'] and health['active'] == 0:
                        break
                    assert time.monotonic() < until, 'retain pairing for active import'
                    time.sleep(.2)
                owned_peer = next(iter(added))
                assert admin({'op': 'revoke', 'id': list(owned_peer)})['result'] == 'revoked'
            assert peers() == prior_peers, 'pairing cleanup changed unrelated access'
            report['pairing_cleanup'] = dict(owned_new_pairing_removed=bool(added),
                                            previous_peers_preserved=True)
        except Exception as e:
            errors.append('owned pairing retained: ' + type(e).__name__)
    if local_network:
        try:
            local_network.close()
        except Exception as e:
            errors.append('owned local network: ' + type(e).__name__)
    guest_err.close(); instrument_log.close()
    report['cleanup_errors'] = errors
    if errors:
        report['status'] = 'failed'
    if a.active_soak_seconds and 'active_soak' in report:
        report['active_soak']['status'] = 'passed' if report['status'] == 'passed' else 'failed'
    report['elapsed_seconds'] = round(time.monotonic() - started, 3); save()
    if errors:
        raise RuntimeError('cleanup failed; inspect retained report')
