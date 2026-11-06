#!/usr/bin/env python3
"""Real v1 Keystore fixture -> APK replacement -> migration -> SSH/TLS/restart.

Owned emulator and QEMU only. Uses actual saved-profile UI and foreground
service, with no selected USB. Does not claim historical APK execution or
device payload testing. Existing TLS token and receiver trust must survive.
"""
import argparse
import base64
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
import zipfile
from lab_android_active_soak import process_identity, protect_android

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('adb', 'apk', 'test-apk', 'lab', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--transport', choices=('ssh', 'direct'), required=True)
    args = parser.parse_args()
    protect_android(args.serial)
    lab, output = args.lab.resolve(), args.output.resolve()
    if not lab.is_relative_to(ROOT / 'build/vm-lab') or not output.is_relative_to(ROOT / 'build/e2e'):
        raise ValueError('Only owned VM/evidence directories')
    qemu = process_identity(int((lab / 'pid').read_text()))
    cmd = Path('/proc', str(qemu['pid']), 'cmdline').read_bytes()
    if (b'qemu-system-x86_64' not in cmd or str(lab / 'guest.qcow2').encode() not in cmd
            or any(x in cmd for x in (b'usb-host', b'vfio-pci', b'-virtfs', b'-fsdev', b'/dev/sd', b'/dev/nvme'))):
        raise ValueError('Owned virtual QEMU without host passthrough required')
    with (lab / 'soak.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        run(args, lab, output, qemu)


def run(args, lab, output, qemu):
    metadata = json.loads((lab / 'connection.json').read_text())
    if metadata['ssh_host'] != '127.0.0.1' or metadata['ssh_user'] != 'fixture':
        raise ValueError('Owned loopback SSH only')
    with zipfile.ZipFile(args.apk) as archive:
        manifest = json.loads(archive.read('assets/receivers/manifest.json'))
        receiver = next(x for x in manifest['artifacts'] if x['name'] == 'usb-most-linux-x86_64')
        payload = gzip.decompress(archive.read('assets/receivers/usb-most-linux-x86_64.gz.bin'))
    if len(payload) != receiver['bytes'] or hashlib.sha256(payload).hexdigest() != receiver['sha256']:
        raise ValueError('Embedded receiver manifest mismatch')
    del payload
    adb = [str(args.adb.resolve()), '-s', args.serial]
    ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(metadata['ssh_port']),
           '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'), 'fixture@127.0.0.1']

    def android(*words, timeout=90):
        return subprocess.check_output(adb + list(words), text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()

    if android('shell', 'getprop', 'ro.kernel.qemu') != '1':
        raise ValueError('Refuse physical Android')
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    common = (ROOT / 'tools/lab_receiver_upgrade.py').read_text()
    helper = (ROOT / 'tools/lab_profile_migration.py').read_text()
    marker, peer = os.urandom(16).hex(), os.urandom(16).hex()
    report = dict(status='running', scope=__doc__, serial=args.serial, transport=args.transport,
        qemu_identity=qemu, source_commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        worktree_dirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT)),
        apk_sha256=hashlib.sha256(args.apk.read_bytes()).hexdigest(),
        test_apk_sha256=hashlib.sha256(args.test_apk.read_bytes()).hexdigest(),
        receiver=receiver, stages=[], rounds=[], harness_sources={
            str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest()
            for p in [Path(__file__).resolve(),ROOT/'tools/lab_receiver_upgrade.py',ROOT/'tools/lab_profile_migration.py']})

    def save():
        temporary = output / 'result.tmp'
        temporary.write_text(json.dumps(report,indent=2)+'\n');temporary.replace(output/'result.json')

    def guest(action, **extra):
        settings = dict(action=action,current_sha256=receiver['sha256'],peer=peer,
                        transport=args.transport,**extra)
        code = ('import types,sys\n'
                "module=types.ModuleType('lab_receiver_upgrade')\n"
                'exec('+repr(common)+',module.__dict__)\n'
                "sys.modules['lab_receiver_upgrade']=module\n"
                'SETTINGS='+repr(settings)+'\n'+helper)
        result = subprocess.run(ssh+['sudo -n python3 -'], input=code,text=True,capture_output=True,timeout=160)
        if result.returncode:
            (output/('guest-'+action+'.stderr')).write_text(result.stderr)
            raise RuntimeError('Owned guest '+action+' failed; original evidence retained')
        return json.loads(result.stdout)

    def monitor(command):
        with socket.socket(socket.AF_UNIX) as s:
            s.settimeout(10);s.connect(str(lab/'monitor.sock'))
            data=b''
            while b'(qemu)' not in data:
                part=s.recv(8192)
                if not part:raise RuntimeError('QEMU monitor ended')
                data+=part
            s.sendall(command.encode()+b'\n');data=b''
            while b'(qemu)' not in data:
                part=s.recv(8192)
                if not part:raise RuntimeError('QEMU monitor ended')
                data+=part
            if any(x in data for x in (b'Error',b'Could not',b'failed')):raise RuntimeError('Owned QEMU forward command failed')

    baseline = guest('before');report['baseline']=baseline;save()
    forward = None
    proc = None
    root = f'/sdcard/Android/data/{APP}/files'
    remote = root+'/migration-fixture.json'
    started = time.monotonic()
    try:
        if args.transport == 'ssh':
            pins=[base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(line.split()[2])).digest()).decode().rstrip('=')
                  for line in (lab/'known_hosts').read_text().splitlines() if len(line.split())>=3 and line.split()[1]=='ssh-ed25519']
            if len(pins)!=1:raise ValueError('Expected one exact guest SSH pin')
            profile=dict(transport='ssh',endpoint=dict(host='10.0.2.2',port=metadata['ssh_port']),user='fixture',host_key=pins[0],reverse_port=0,prepare=True)
            credentials=dict(private_key=(lab/'client').read_text(),client_id=peer)
        else:
            code=(lab/'direct-pair-code').read_text().strip()
            if not code.startswith('usbmost:1:'):raise ValueError('Invalid saved guest TLS identity')
            raw=code[len('usbmost:1:'):];pair=json.loads(base64.urlsafe_b64decode(raw+'='*(-len(raw)%4)))
            token=(lab/'direct-token').read_text().strip()
            if not re.fullmatch('[A-Za-z0-9_-]{43}',token):raise ValueError('Invalid saved guest token')
            peer=hashlib.sha256(base64.urlsafe_b64decode(token+'=')).hexdigest()[:32]
            with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
            monitor(f"hostfwd_add lab tcp:127.0.0.1:{port}-:{baseline['configuration']['port']}");forward=port
            profile=dict(transport='direct',endpoint=dict(host='10.0.2.2',port=port),server_pin=pair['server_pin'])
            credentials=dict(token=token);del token,code,pair
        config=dict(owned_emulator_only=True,marker=marker,legacy_client_id=peer,profile=profile,credentials=credentials)
        android('shell','am','force-stop',APP)
        for apk in (args.apk,args.test_apk):
            if 'Success' not in android('install','-r',str(apk.resolve())):raise RuntimeError('APK install failed')
        if 'Success' not in android('shell','pm','clear',APP):raise RuntimeError('Owned emulator clear failed')
        android('shell','input','keyevent','KEYCODE_WAKEUP');android('shell','wm','dismiss-keyguard')
        for stage in ('seed','connect','resume'):
            android('shell','am','force-stop',APP)
            if stage=='connect':
                # Real package replacement preserves the v1 sandbox/Keystore.
                if 'Success' not in android('install','-r',str(args.apk.resolve())):raise RuntimeError('Replacement APK installation failed')
            fixture=output/'migration-fixture.json'
            with fixture.open('x') as stream:
                fixture.chmod(0o600);json.dump(config,stream)
            android('shell','mkdir','-p',root);android('push',str(fixture),remote);fixture.unlink()
            logpath=output/(stage+'.log')
            with logpath.open('w') as log:
                proc=subprocess.Popen(adb+['shell','am','instrument','-w','-r','-e','suite','migration-connection','-e','stage',stage,
                    APP+'.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],stdout=log,stderr=subprocess.STDOUT)
                for round in range(0 if stage=='seed' else 2 if stage=='connect' else 1):
                    phase=f'MIGRATION_CONNECTED stage={stage} round={round}'
                    deadline=time.monotonic()+160
                    while phase not in logpath.read_text():
                        if proc.poll() is not None or time.monotonic()>=deadline:raise RuntimeError('Missing actual service phase: '+phase)
                        time.sleep(.1)
                    observed=guest('connected',before=baseline['before'])
                    # Instrumentation holds the connected UI at this barrier.
                    # Capture after Activity transition animation; screencap
                    # does not attach a second UiAutomation owner.
                    time.sleep(.7)
                    png=subprocess.check_output(adb+['exec-out','screencap','-p'],timeout=20)
                    if not png.startswith(b'\x89PNG\r\n\x1a\n') or not 0<len(png)<10*1024*1024:
                        raise RuntimeError('Invalid actual display capture')
                    screen=output/f'{stage}-connected-{round}.png'
                    with screen.open('xb') as image:image.write(png)
                    report['rounds'].append(dict(stage=stage,round=round,guest=observed,
                        elapsed_seconds=round_time(started),screen_sha256=hashlib.sha256(png).hexdigest()));save()
                    ack=output/'ack';ack.write_bytes(b'\x01')
                    target=root+f'/migration-{marker}-{stage}-{round}.go'
                    android('push',str(ack),target+'.part');android('shell','mv',target+'.part',target)
                if proc.wait(timeout=45)!=0:raise RuntimeError('Instrumentation transport failed')
            text=logpath.read_text()
            match=re.search(r'PASS optimized release API (\d+): (\d+) checks',text)
            if not match or 'INSTRUMENTATION_CODE: -1' not in text or 'FAIL ' in text:raise RuntimeError('Migration instrumentation failed: '+stage)
            report['stages'].append(dict(stage=stage,api=int(match[1]),checks=int(match[2])));save()
        android('pull',root+'/migration-'+marker,str(output/'proof'))
        report['status']='passed'
    except BaseException as error:
        report.update(status='failed',error=str(error));raise
    finally:
        try:
            android('shell','am','force-stop',APP)
            if proc is not None and proc.poll() is None:proc.terminate();proc.wait(timeout=10)
            android('shell','rm','-f',remote)
            report['restoration']=guest('restore',**baseline)
            if forward is not None:monitor(f'hostfwd_remove lab tcp:127.0.0.1:{forward}')
        except BaseException as error:
            report.update(status='failed',restoration_error=str(error));raise
        finally:save()
    print('PASS actual migrated '+args.transport+' service: two connections and one after process restart; original receiver restored')


def round_time(started):
    return round(time.monotonic()-started,3)


if __name__=='__main__':main()
