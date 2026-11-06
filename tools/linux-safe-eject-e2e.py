#!/usr/bin/env python3
"""Real UDisks2 removal in our Q35 VM; exclusively-created synthetic USB disks.

Requires the lab booted with --usb-eject-fixture and guest udisks2/usbip/e2fsprogs.
No host USB passthrough. Never recursively clean mounts or disk images. All
images/logs remain in the lab. A failed ordinary unmount leaves the mount intact.
"""
from pathlib import Path
import argparse, hashlib, json, re, socket, subprocess

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--lab', type=Path, required=True)
p.add_argument('--binary', type=Path, required=True)
p.add_argument('--tag', required=True)
p.add_argument('--sandbox', action='store_true', help='Run the adapter in the receiver service mount/device/capability sandbox')
p.add_argument('--udisks-state', choices=['normal', 'restart', 'no-bus'], default='normal',
               help='Owned VM: masked service then activation recovery, or hide system bus in the test service only')
p.add_argument('--usb-ports', help='Two explicit vacant QEMU root ports, e.g. 1,2; no automatic hub insertion')
p.add_argument('--require-speed', choices=['high', 'super'], help='Require actual 480 Mbit/s or at least 5000 Mbit/s on the owned physical QEMU source')
a = p.parse_args()
if a.udisks_state == 'no-bus' and not a.sandbox:
    p.error('--udisks-state no-bus requires --sandbox')
ports = a.usb_ports.split(',') if a.usb_ports else None
if ports is not None:
    assert len(ports) == 2 and len(set(ports)) == 2 and all(re.fullmatch(r'[1-9]|1[0-6]', p) for p in ports)
lab = a.lab.resolve()
assert lab.is_relative_to(ROOT / 'build/vm-lab')
assert re.fullmatch(r'[a-z0-9]{1,12}', a.tag)
m = json.loads((lab / 'connection.json').read_text())
assert m['ssh_host'] == '127.0.0.1' and m['ssh_user'] == 'fixture' and m['usb_eject_fixture']
cmd = Path('/proc', str(int((lab / 'pid').read_text())), 'cmdline').read_bytes()
assert b'qemu-system-x86_64' in cmd and str(lab / 'guest.qcow2').encode() in cmd
assert b'qemu-xhci,id=eject113-xhci' in cmd and b'usb-host' not in cmd
binary = a.binary.resolve()
assert binary.is_file() and binary.is_relative_to(ROOT / 'build')
ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(m['ssh_port']),
       '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes',
       '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'), 'fixture@127.0.0.1']

def monitor(command):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(10)
        s.connect(str(lab / 'monitor.sock'))
        out = b''
        while b'(qemu)' not in out:
            out += s.recv(4096)
        s.sendall(command.encode() + b'\n')
        out = b''
        while b'(qemu)' not in out:
            out += s.recv(4096)
        # The monitor echoes each key with ANSI cursor movement. Keep only the
        # response after that echoed command, not kilobytes of terminal escapes.
        response = out.decode(errors='replace').split('\r\n')[1:]
        assert not any('Error' in line or 'failed' in line for line in response), response

serials = ['UMOSTEJ' + a.tag + suffix for suffix in ['A', 'B']]
images = []
for index, serial in enumerate(serials):
    image = lab / f'{a.tag}-disk{index}.raw'
    # Never truncate a reused image, even if an earlier run failed.
    with image.open('xb') as f:
        f.truncate(64 * 1024 * 1024)
    images.append(str(image))
    drive = f'{a.tag}-drive{index}'
    monitor(f'drive_add 0 if=none,id={drive},file={image},format=raw')
    port = ',port=' + ports[index] if ports is not None else ''
    monitor(f'device_add usb-storage,id={a.tag}-usb{index},drive={drive},bus=eject113-xhci.0,serial={serial}{port}')

guest_binary = '/home/fixture/eject-linux-lab-' + a.tag
with binary.open('rb') as source:
    subprocess.run(ssh + ['cat > ' + guest_binary], stdin=source, check=True)
subprocess.run(ssh + ['chmod 700 ' + guest_binary], check=True)

script = r'''
from pathlib import Path
import hashlib,json,os,select,shutil,subprocess,time
serials=SERIALS
tag=TAG
binary=BINARY
sandbox=SANDBOX
udisks_state=UDISKS_STATE
required_speed=REQUIRED_SPEED
assert 'Q35' in Path('/sys/class/dmi/id/product_name').read_text()
assert os.geteuid()==0
selinux=subprocess.check_output(['/usr/sbin/getenforce'],text=True).strip() if Path('/usr/sbin/getenforce').exists() else 'not-installed'
audit=Path('/var/log/audit/audit.log')
audit_before=(audit.stat().st_ino,audit.stat().st_size) if selinux=='Enforcing' else None
def run(args,**kw):return subprocess.run(args,check=True,capture_output=True,text=True,timeout=40,**kw)
def usb(serial):
 deadline=time.monotonic()+20
 while time.monotonic()<deadline:
  found=[p.resolve() for p in Path('/sys/bus/usb/devices').iterdir() if (p/'serial').is_file() and (p/'serial').read_text().strip()==serial]
  if found:
   assert len(found)==1
   d=found[0]
   assert (d/'idVendor').read_text().strip()=='46f4' and (d/'idProduct').read_text().strip()=='0001'
   return d
  time.sleep(.1)
 raise RuntimeError('synthetic USB missing')
def block(parent):
 deadline=time.monotonic()+20
 while time.monotonic()<deadline:
  found=[p for p in Path('/sys/class/block').iterdir() if p.resolve().is_relative_to(parent) and not (p/'partition').exists()]
  if found:
   assert len(found)==1
   return Path('/dev',found[0].name)
  time.sleep(.1)
 raise RuntimeError('synthetic block device missing')
physical=[usb(serial) for serial in serials]
assert physical[0]!=physical[1] and all('/vhci_hcd.' not in str(p) for p in physical)
physical_speeds=[int(float((p/'speed').read_text().strip())) for p in physical]
print('OWNED_USB_SPEEDS',physical_speeds,flush=True)
if required_speed=='super':assert all(speed>=5000 for speed in physical_speeds)
if required_speed=='high':assert all(speed==480 for speed in physical_speeds)
devices=[block(p) for p in physical]
assert len(set(devices))==2 and all(p.name.startswith('sd') for p in devices)
for dev,parent in zip(devices,physical):
 # Immediately verify exact synthetic ancestry before the sole formatting step.
 assert Path('/sys/class/block',dev.name).resolve().is_relative_to(parent)
 assert not any(line.split()[0]==str(dev) for line in Path('/proc/mounts').read_text().splitlines())
 run(['/usr/sbin/mkfs.ext4','-q',str(dev)])
owned_mount=Path('/run','usb-most-eject-'+tag+'-owned')
foreign_mount=Path('/run','usb-most-eject-'+tag+'-foreign')
owned_mount.mkdir();foreign_mount.mkdir()
run(['/usr/bin/mount',str(devices[1]),str(foreign_mount)])
foreign_file=foreign_mount/'fixture.txt';foreign_file.write_bytes(b'foreign fixture must remain mounted\n')
foreign_hash=hashlib.sha256(foreign_file.read_bytes()).hexdigest()
process=None;daemon=None;held=None;events=[];exported=False;udisks_masked=False
udisks_was_active=False
mask=Path('/run/systemd/system/udisks2.service')
def unmask_owned():
 global udisks_masked
 if udisks_masked:
  assert mask.is_symlink() and os.readlink(mask)=='/dev/null'
  run(['/usr/bin/systemctl','unmask','--runtime','udisks2.service'])
  udisks_masked=False
try:
 run(['/usr/sbin/modprobe','usbip_host'])
 run(['/usr/sbin/modprobe','vhci_hcd'])
 run(['/usr/sbin/usbip','bind','-b',physical[0].name])
 exported=True
 daemon=subprocess.Popen(['/usr/sbin/usbipd','-4'],stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
 time.sleep(.3);assert daemon.poll() is None,'owned usbipd failed; do not share an unknown daemon'
 marker=Path('/run/usb-most-eject-owned-lab')
 if marker.exists():assert marker.read_text()=='USBMOST_EJECT113_SYNTHETIC_ONLY\n'
 else:marker.write_text('USBMOST_EJECT113_SYNTHETIC_ONLY\n')
 launch=[binary,'--owned-qemu-lab',physical[0].name]
 if sandbox:
  service_binary=Path('/usr/local/libexec','usb-most-eject-lab-'+tag)
  with service_binary.open('xb') as out,open(binary,'rb') as source:shutil.copyfileobj(source,out)
  service_binary.chmod(0o700)
  launch=['/usr/bin/systemd-run','--pipe','--wait','--collect','--quiet','--unit=usb-most-eject-'+tag,
    '--property=PrivateTmp=yes','--property=PrivateDevices=yes','--property=ProtectSystem=strict',
    '--property=ProtectHome=yes','--property=NoNewPrivileges=yes','--property=CapabilityBoundingSet=',
    '--property=ProtectKernelModules=yes','--property=ProtectControlGroups=yes',
    '--property=ReadWritePaths=/sys/devices/platform/vhci_hcd.0/attach',
    '--property=RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK',
    str(service_binary),'--owned-qemu-lab',physical[0].name]
 if udisks_state=='no-bus':launch.insert(1,'--property=InaccessiblePaths=/run/dbus')
 stderr_path=Path('/run','usb-most-eject-'+tag+'-stderr.log')
 stderr_file=stderr_path.open('x')
 process=subprocess.Popen(launch,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=stderr_file,text=True,bufsize=1)
 def line(timeout=45):
  assert select.select([process.stdout],[],[],timeout)[0],'fixture result deadline'
  value=process.stdout.readline().strip()
  if not value:
   process.wait(timeout=5)
   raise RuntimeError('fixture exited '+str(process.returncode)+': '+stderr_path.read_text()[-8192:])
  events.append(value);print(value,flush=True);return value
 assert line().startswith('READY ')
 status=Path('/sys/devices/platform/vhci_hcd.0/status').read_text()
 port=int(events[0].split()[1]);rows=[row.split() for row in status.splitlines()[1:]]
 local=[row[6] for row in rows if int(row[1])==port and row[2]=='006']
 assert len(local)==1
 virtual=Path('/sys/bus/usb/devices',local[0]).resolve()
 assert '/vhci_hcd.0/' in str(virtual)
 imported=block(virtual)
 run(['/usr/bin/mount',str(imported),str(owned_mount)])
 own_file=owned_mount/'fixture.txt';own_file.write_bytes(b'pending file data must survive eject\n'*4096)
 own_hash=hashlib.sha256(own_file.read_bytes()).hexdigest()
 if udisks_state=='restart':
  # Only our disposable guest; never alter a pre-existing mask or unit override.
  assert not mask.exists() and not mask.is_symlink()
  state=run(['/usr/bin/systemctl','show','--property=UnitFileState','--value','udisks2.service']).stdout.strip()
  assert state not in ('masked','masked-runtime','not-found')
  udisks_was_active=subprocess.run(['/usr/bin/systemctl','is-active','--quiet','udisks2.service']).returncode==0
  run(['/usr/bin/systemctl','mask','--runtime','--now','udisks2.service']);udisks_masked=True
 if udisks_state!='normal':
  process.stdin.write('eject\n');process.stdin.flush()
  assert line()=='RESULT Unsupported'
  assert os.path.ismount(owned_mount) and hashlib.sha256(own_file.read_bytes()).hexdigest()==own_hash
  assert os.path.ismount(foreign_mount) and hashlib.sha256(foreign_file.read_bytes()).hexdigest()==foreign_hash
  if udisks_state=='restart':
   unmask_owned()
   assert subprocess.run(['/usr/bin/systemctl','is-active','--quiet','udisks2.service']).returncode!=0
 if udisks_state=='no-bus':
  # The operation did not unmount anything. Test cleanup uses normal umount,
  # explicitly outside the feature; do not record this as OS-confirmed Safe.
  run(['/usr/bin/umount',str(owned_mount)])
 else:
  # An open file makes normal host-namespace unmount return DeviceBusy.
  held=own_file.open('rb')
  process.stdin.write('eject\n');process.stdin.flush()
  assert line()=='RESULT Busy'
  if udisks_state=='restart':run(['/usr/bin/systemctl','is-active','--quiet','udisks2.service'])
  assert os.path.ismount(owned_mount) and hashlib.sha256(own_file.read_bytes()).hexdigest()==own_hash
  assert os.path.ismount(foreign_mount) and hashlib.sha256(foreign_file.read_bytes()).hexdigest()==foreign_hash
  held.close();held=None
  process.stdin.write('eject\n');process.stdin.flush()
  assert line()=='RESULT Safe'
  assert not os.path.ismount(owned_mount)
 assert os.path.ismount(foreign_mount) and hashlib.sha256(foreign_file.read_bytes()).hexdigest()==foreign_hash
 process.stdin.write('quit\n');process.stdin.flush();assert process.wait(timeout=10)==0,stderr_path.read_text()[-8192:]
 run(['/usr/sbin/usbip','unbind','-b',physical[0].name]);exported=False
 restored=block(physical[0])
 run(['/usr/bin/mount','-o','ro',str(restored),str(owned_mount)])
 assert hashlib.sha256((owned_mount/'fixture.txt').read_bytes()).hexdigest()==own_hash
 run(['/usr/bin/umount',str(owned_mount)])
 avc=[]
 if audit_before:
  assert audit.stat().st_ino==audit_before[0] and audit.stat().st_size>=audit_before[1], 'Audit rotated; do not claim a complete denial check'
  with audit.open('rb') as stream:stream.seek(audit_before[1]);new=stream.read().decode(errors='replace')
  avc=[line for line in new.splitlines() if line.startswith(('type=AVC ','type=USER_AVC '))]
  assert not avc, 'Unexpected SELinux denial; retain audit and investigate before passing'
 package_version=run(['/usr/bin/rpm','-q','udisks2']).stdout.strip() if Path('/usr/bin/rpm').exists() else run(['/usr/bin/dpkg-query','-W','-f=${Version}','udisks2']).stdout.strip()
 print(json.dumps({'status':'passed','events':events,'sandbox':sandbox,'foreign_mounted_unchanged':True,
  'physical_speeds_mbps':physical_speeds,'selinux':selinux,'audit_checked':audit_before is not None,'new_avc_count':len(avc),
  'owned_sha256_after_reconnect':own_hash,'foreign_sha256':foreign_hash,
  'udisks_state':udisks_state,'udisks':package_version}),flush=True)
finally:
 unmask_owned()
 if udisks_was_active:run(['/usr/bin/systemctl','start','udisks2.service'])
 if process:
  print('FIXTURE_STDERR',stderr_path.read_text()[-8192:],flush=True)
 if held:held.close()
 # Ordinary unmount before terminating the test's import. A failed unmount
 # must retain its mountpoint/files. There is never recursive removal here.
 retained=False
 for mount in [owned_mount,foreign_mount]:
  if os.path.ismount(mount):
   result=subprocess.run(['/usr/bin/umount',str(mount)],capture_output=True,text=True)
   if result.returncode:retained=True;print('RETAINED_BUSY_MOUNT',str(mount),flush=True)
 if not retained and process and process.poll() is None:
  process.stdin.write('quit\n');process.stdin.flush()
  try:process.wait(timeout=10)
  except subprocess.TimeoutExpired:retained=True;print('RETAINED_IMPORT_PROCESS',process.pid,flush=True)
 if not retained and exported:
  subprocess.run(['/usr/sbin/usbip','unbind','-b',physical[0].name],capture_output=True,text=True,timeout=20)
 if not retained and daemon and daemon.poll() is None:daemon.terminate();daemon.wait(timeout=10)
'''.replace('SERIALS', repr(serials)).replace('TAG', repr(a.tag)).replace('BINARY', repr(guest_binary)).replace('SANDBOX', repr(a.sandbox)).replace('UDISKS_STATE', repr(a.udisks_state)).replace('REQUIRED_SPEED', repr(a.require_speed))

log = lab / (a.tag + '-safe-eject.log')
result = lab / (a.tag + '-safe-eject.json')
compile(script, '<owned VM safe eject>', 'exec')
with log.open('x') as output:
    run = subprocess.run(ssh + ['sudo -n python3 -'], input=script, text=True, stdout=output, stderr=subprocess.STDOUT, timeout=180)
record = {'status': 'passed' if run.returncode == 0 else 'failed', 'exit': run.returncode,
          'sandbox': a.sandbox, 'udisks_state': a.udisks_state, 'required_speed': a.require_speed,
          'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'images': images,
          'scope': 'QEMU synthetic disks; real guest USB/IP, VHCI, system UDisks2; no Android integration'}
result.write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))
raise SystemExit(run.returncode)
