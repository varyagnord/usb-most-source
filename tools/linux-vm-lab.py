#!/usr/bin/env python3
"""Isolated disposable Linux receiver VM. No host USB/device passthrough or mounts.

All files are retained under build/vm-lab. Never recursively clean this lab: it
may be in use. Its only host network listener is a loopback SSH port.
"""
from pathlib import Path
import argparse,hashlib,json,os,pwd,socket,subprocess,uuid

ROOT=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--base',type=Path,required=True)
p.add_argument('--lab',type=Path,required=True)
p.add_argument('--launch',action='store_true')
p.add_argument('--ssh-port',type=int,help='Reuse an owned lab loopback port after a verified reboot')
p.add_argument('--accel',choices=['kvm','tcg'],default='kvm')
p.add_argument('--sudo-group',choices=['sudo','wheel'],default='sudo',help='Distribution administrator group in the disposable guest')
p.add_argument('--slirp-lib',type=Path,help='Owned build-only libslirp directory; never installed system-wide')
p.add_argument('--usb-eject-fixture',action='store_true',help='Add an empty QEMU xHCI controller for synthetic disk ejection tests; never host USB passthrough')
a=p.parse_args()
base=a.base.resolve();lab=a.lab.resolve()
assert base.is_file() and lab.is_relative_to(ROOT/'build/vm-lab')
lab.mkdir(mode=0o700,parents=True,exist_ok=True);lab.chmod(0o700)
def run(args):subprocess.run(args,check=True,stdout=subprocess.DEVNULL)
if not (lab/'client').exists():run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(lab/'client')])
public=(lab/'client.pub').read_text().strip()
if not (lab/'guest.qcow2').exists():
    image=json.loads(subprocess.check_output(['qemu-img','info','--output=json',str(base)]))
    assert image['format']=='qcow2' and image['virtual-size']>0
    # Never truncate a larger cloud image to the old fixed 8 GiB overlay size.
    size=max(8*1024**3,image['virtual-size'])
    run(['qemu-img','create','-f','qcow2','-F','qcow2','-b',str(base),str(lab/'guest.qcow2'),str(size)])
    (lab/'meta-data').write_text('instance-id: usb-most-'+uuid.uuid4().hex+'\nlocal-hostname: usb-most-lab\n')
    (lab/'user-data').write_text('''#cloud-config
ssh_pwauth: false
disable_root: true
users:
  - name: fixture
    shell: /bin/bash
    groups: ['''+a.sudo_group+''']
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys:
      - '''+public+'''
write_files:
  - path: /etc/ssh/sshd_config.d/usb-most-lab.conf
    permissions: '0644'
    content: |
      PasswordAuthentication no
      KbdInteractiveAuthentication no
      PermitRootLogin no
      AllowTcpForwarding remote
      GatewayPorts no
      PermitListen 127.0.0.1:*
package_update: false
package_upgrade: false
''')
    run(['cloud-localds',str(lab/'seed.iso'),str(lab/'user-data'),str(lab/'meta-data')])
if not a.launch:
    print('Prepared isolated lab:',lab)
else:
    if (lab/'pid').exists():
        pid=int((lab/'pid').read_text())
        if Path('/proc',str(pid)).exists():raise RuntimeError('lab VM is already running')
    if a.ssh_port is not None:assert 1024<=a.ssh_port<=65535
    with socket.socket() as s:
        # A verified stopped VM can leave TIME_WAIT sockets on its old port.
        # This still rejects an actual listener, while permitting a lab reboot.
        s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
        s.bind(('127.0.0.1',a.ssh_port or 0));port=s.getsockname()[1]
    metadata={'lab':str(lab),'ssh_host':'127.0.0.1','ssh_port':port,'ssh_user':'fixture','base_sha512':hashlib.file_digest(base.open('rb'),'sha512').hexdigest(),'scope':'QEMU/KVM; virtual disk only; no USB or host device passthrough'}
    metadata['usb_eject_fixture']=a.usb_eject_fixture
    library=None
    if a.slirp_lib:
        library=a.slirp_lib.resolve()
        assert library.is_relative_to(ROOT/'build/tools')
        binary=library/'libslirp.so.0'
        assert binary.is_file() and binary.resolve().is_relative_to(library)
        metadata['slirp_library']=str(binary.resolve())
        metadata['slirp_sha256']=hashlib.file_digest(binary.open('rb'),'sha256').hexdigest()
    (lab/'connection.json').write_text(json.dumps(metadata,indent=2)+'\n')
    cmd=['/usr/bin/qemu-system-x86_64','-machine',f'q35,accel={a.accel}','-cpu','max' if a.accel=='tcg' else 'host','-no-reboot','-m','2048','-smp','2','-nodefaults','-device','VGA','-display','none',
         '-drive',f'file={lab}/guest.qcow2,format=qcow2,if=virtio',
         '-drive',f'file={lab}/seed.iso,format=raw,if=virtio,readonly=on',
         '-netdev',f'user,id=lab,restrict=on,hostfwd=tcp:127.0.0.1:{port}-:22','-device','virtio-net-pci,netdev=lab',
         '-object','rng-random,id=rng,filename=/dev/urandom','-device','virtio-rng-pci,rng=rng',
         '-serial',f'file:{lab}/serial.log','-monitor',f'unix:{lab}/monitor.sock,server=on,wait=off','-pidfile',str(lab/'pid')]
    if a.usb_eject_fixture:cmd+=['-device','qemu-xhci,id=eject113-xhci']
    print('Launching isolated lab SSH port',port,flush=True)
    user=pwd.getpwuid(os.getuid()).pw_name
    if library:cmd=['/usr/bin/env','LD_LIBRARY_PATH='+str(library),*cmd]
    os.execvp('sudo',['sudo','-n','-u',user,'-g','kvm',*cmd])
