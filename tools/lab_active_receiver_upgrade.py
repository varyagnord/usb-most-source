"""Owned guest receiver-update checks. All recovery files retained, no mount cleanup."""
from pathlib import Path
if not __debug__:
    raise SystemExit("Refuse optimized Python: active receiver ownership checks must remain enabled")
import base64
import hashlib
import json
import os
import re
import stat
import subprocess
import time

OLD = 'b616b02be60a17e7f2eccbe135f82bc294cf1afcface614bd7fe70d834a195b1'
CURRENT = 'a1c9ce9b292e47ca052d56e7f60d17ed1f10bd821fbc07abdf2acff9c5ddd9b5'
FIXED = 'ac1c682e0440fbd9fad81f665fc87b4b4ae3fe27098e837fb27a625800dd6b3d'
BINARY = Path('/usr/local/libexec/usb-most/usb-most-desktop')
CONFIG = Path('/var/lib/usb-most/receiver.json')
FILES = {'receiver.bin': BINARY, 'receiver.json': CONFIG,
         'service.unit': Path('/etc/systemd/system/usb-most-desktop.service'),
         'modules.conf': Path('/etc/modules-load.d/usb-most.conf'),
         'activation': Path('/var/lib/usb-most/active'),
         'identity.json': Path('/var/lib/usb-most/identity.json'),
         'pairs.json': Path('/var/lib/usb-most/pairs.json')}


def regular(path, maximum=20*1024**2):
    info = path.lstat()
    assert stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
    assert not info.st_mode & 0o022 and 0 < info.st_size <= maximum
    with path.open('rb') as f:
        opened = os.fstat(f.fileno())
        assert (opened.st_dev, opened.st_ino) == (info.st_dev, info.st_ino)
        data = f.read(maximum+1)
    assert len(data) == info.st_size
    return data, dict(sha256=hashlib.sha256(data).hexdigest(), bytes=len(data),
                      mode=stat.S_IMODE(info.st_mode), uid=info.st_uid,
                      gid=info.st_gid, inode=info.st_ino, mtime_ns=info.st_mtime_ns)


def health():
    r = subprocess.run([str(BINARY), '--health'], capture_output=True, text=True, timeout=10)
    assert r.returncode == 0, 'Receiver health unavailable'
    return json.loads(r.stdout)['status']


def wait_idle():
    deadline = time.monotonic()+30
    while True:
        h = health()
        if not h['connected'] and h['active'] == 0:
            return h
        assert time.monotonic() < deadline, 'Retain recovery: receiver still active'
        time.sleep(.2)


def snapshot(expected, active):
    files = {name: regular(path)[1] for name, path in FILES.items()}
    assert files['receiver.bin']['sha256'] == expected
    h = health()
    assert h['active'] == active and h['connected'] == (active > 0)
    pid = int(subprocess.check_output(['systemctl', 'show', 'usb-most-desktop.service',
                                      '-p', 'MainPID', '--value'], text=True).strip())
    assert pid > 0 and Path('/proc', str(pid), 'exe').resolve() == BINARY
    ticks = int(Path('/proc', str(pid), 'stat').read_text().split(') ', 1)[1].split()[19])
    assert not Path('/var/lib/usb-most/install-journal.json').exists()
    return dict(files=files, health=h, service_pid=pid, service_start_ticks=ticks,
                boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip())


def install(binary, data):
    assert regular(binary)[1]['sha256'] in (OLD, CURRENT, FIXED)
    r = subprocess.run([str(binary), '--install-stdin'], input=data, capture_output=True, timeout=120)
    return dict(exit_code=r.returncode, stderr=r.stderr.decode(errors='replace')[-2048:])


def create(path, data, mode):
    with path.open('xb') as f:
        os.chmod(path, mode);f.write(data);f.flush();os.fsync(f.fileno())


def run(settings):
    assert __debug__ and os.geteuid() == 0
    assert 'Q35' in Path('/sys/class/dmi/id/product_name').read_text()
    tag = settings['tag'];assert re.fullmatch('[a-z0-9]{1,11}', tag)
    root = Path('/root/usb-most-active-update-'+tag)
    target = settings.get('target_sha256', CURRENT);assert target in (CURRENT,FIXED)
    action = settings['action'];assert action in ('prepare','busy','update','restore')
    if action == 'prepare':
        assert not root.exists();wait_idle();before=snapshot(target,0)
        root.mkdir(mode=0o700)
        for name,path in FILES.items():create(root/name,regular(path)[0],0o700 if name=='receiver.bin' else 0o600)
        old=base64.b64decode(settings['old_binary'],validate=True);assert hashlib.sha256(old).hexdigest()==OLD
        create(root/'old.bin',old,0o700)
        create(root/'owner.json',json.dumps(dict(tag=tag,before=before)).encode(),0o600)
        result=install(root/'old.bin',regular(root/'receiver.json',4096)[0]);assert result['exit_code']==0,result
        after=snapshot(OLD,0)
        for name in ('identity.json','pairs.json'):
            assert before['files'][name]==after['files'][name], 'Old baseline changed durable trust'
        return dict(status='passed',old_receiver_sha256=OLD,current_receiver_sha256=target,
                    initial_health=before['health'],old_health=after['health'],recovery_directory=str(root))
    owner=json.loads(regular(root/'owner.json',32768)[0]);assert owner['tag']==tag
    assert regular(root/'receiver.bin')[1]['sha256']==target
    assert regular(root/'old.bin')[1]['sha256']==OLD
    if action == 'busy':
        before=snapshot(OLD,1);data=regular(CONFIG,4096)[0]
        results=[]
        # A no-change reinstall must reuse the running service, including USB.
        result=install(root/'old.bin',data);assert result['exit_code']==0,result
        assert snapshot(OLD,1)==before,'Idempotent install disrupted active USB'
        results.append(dict(case='unchanged_reinstall',**result,exact_files_pid_health_unchanged=True))
        for name,payload in [('changed_binary',data),('changed_binary_and_config',
                json.dumps(dict(transport='direct',port=(json.loads(data)['port']%50000)+10000)).encode())]:
            result=install(root/'receiver.bin',payload)
            assert result['exit_code']!=0 and 'WouldBlock' in result['stderr'], result
            assert snapshot(OLD,1)==before,'Refused update changed service, files, trust or active import'
            results.append(dict(case=name,**result,exact_files_pid_health_unchanged=True))
        create(root/'busy-before.json',json.dumps(before).encode(),0o600)
        return dict(status='passed',cases=results,active=1,service_pid=before['service_pid'],
                    receiver_sha256=OLD,old_import_preserved=True)
    if action == 'update':
        wait_idle();before=snapshot(OLD,0)
        result=install(root/'receiver.bin',regular(CONFIG,4096)[0]);assert result['exit_code']==0,result
        after=snapshot(target,0)
        for name in ('receiver.json','identity.json','pairs.json'):
            assert all(before['files'][name][k]==after['files'][name][k]
                       for k in ('sha256','bytes','mode','uid','gid')), 'Update altered config/trust'
        assert before['boot_id']==after['boot_id'] and before['service_pid']!=after['service_pid']
        return dict(status='passed',installed_receiver_sha256=target,old_receiver_sha256=OLD,
                    config_identity_pairing_exact=True,guest_boot_unchanged=True,
                    old_service_pid=before['service_pid'],new_service_pid=after['service_pid'],health=after['health'])
    # Restore only after the main harness stopped its service and retired its pair.
    wait_idle();assert regular(BINARY)[1]['sha256'] in (OLD,target)
    result=install(root/'receiver.bin',regular(root/'receiver.json',4096)[0]);assert result['exit_code']==0,result
    after=snapshot(target,0)
    for name,entry in owner['before']['files'].items():
        assert all(after['files'][name][k]==entry[k] for k in ('sha256','bytes','mode','uid','gid')),name
    assert after['health']==owner['before']['health'] and after['boot_id']==owner['before']['boot_id']
    return dict(status='passed',seven_deployment_and_trust_hashes_restored=True,
                original_health_restored=True,recovery_material_retained=True)


if __name__ == '__main__':
    print(json.dumps(run(SETTINGS)))
