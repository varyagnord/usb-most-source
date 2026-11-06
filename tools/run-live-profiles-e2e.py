#!/usr/bin/env python3
"""Two owned QEMU receivers: independent SSH/TLS profiles, UI switch and restart.

Actual optimized Android APK, Keystore, foreground service and receiver sessions.
No selected USB: USB ownership/ejection during switching remains a separate gate.
"""
import argparse
import base64
from contextlib import ExitStack
import fcntl
import gzip
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
import zipfile
from lab_android_active_soak import process_identity

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('adb', 'apk', 'test-apk', 'ssh-lab', 'tls-lab', 'output'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--serial', required=True)
    args = p.parse_args()
    spec = importlib.util.spec_from_file_location('migration_guard', ROOT / 'tools/run-profile-migration-e2e.py')
    guard = importlib.util.module_from_spec(spec); spec.loader.exec_module(guard)
    with ExitStack() as stack:
        guard.protect_android(args.serial)
        # All mutations occur only after both VM locks and this Android lock.
        lockdir = ROOT / 'build/e2e/android-locks'; lockdir.mkdir(exist_ok=True)
        lock = stack.enter_context((lockdir / (args.serial + '.lock')).open('a'))
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        labs = [args.ssh_lab.resolve(), args.tls_lab.resolve()]
        if labs[0] == labs[1]: raise ValueError('Two independent VMs required')
        for lab in sorted(labs):
            if not lab.is_relative_to(ROOT / 'build/vm-lab'): raise ValueError('Owned lab required')
            lock = stack.enter_context((lab / 'soak.lock').open('a'))
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        run(args, labs, guard)


def run(args, labs, guard):
    output = args.output.resolve()
    if not output.is_relative_to(ROOT / 'build/e2e'): raise ValueError('Private evidence path required')
    adb = [str(args.adb.resolve()), '-s', args.serial]
    def android(*words, timeout=90):
        guard.protect_android(args.serial)
        return subprocess.check_output(adb + list(words), text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()
    if android('shell', 'getprop', 'ro.kernel.qemu') != '1': raise ValueError('Owned emulator only')
    with zipfile.ZipFile(args.apk) as archive:
        manifest = json.loads(archive.read('assets/receivers/manifest.json'))
        receiver = next(x for x in manifest['artifacts'] if x['name'] == 'usb-most-linux-x86_64')
        payload = gzip.decompress(archive.read('assets/receivers/usb-most-linux-x86_64.gz.bin'))
        if len(payload) != receiver['bytes'] or hashlib.sha256(payload).hexdigest() != receiver['sha256']:
            raise ValueError('Embedded receiver differs from manifest')
    del payload
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    common = (ROOT / 'tools/lab_receiver_upgrade.py').read_text()
    helper = (ROOT / 'tools/lab_live_profiles.py').read_text()
    endpoints = []
    for index, lab in enumerate(labs):
        ident = process_identity(int((lab / 'pid').read_text()))
        cmd = Path('/proc', str(ident['pid']), 'cmdline').read_bytes()
        if (b'qemu-system-x86_64' not in cmd or str(lab / 'guest.qcow2').encode() not in cmd
                or any(s in cmd for s in (b'usb-host', b'vfio-pci', b'-virtfs', b'-fsdev', b'/dev/sd', b'/dev/nvme'))):
            raise ValueError('Owned QEMU without passthrough required')
        meta = json.loads((lab / 'connection.json').read_text())
        if meta['ssh_host'] != '127.0.0.1' or meta['ssh_user'] != 'fixture': raise ValueError('Owned SSH only')
        endpoints.append(dict(lab=lab, identity=ident, transport='ssh' if index == 0 else 'direct',
            peer=os.urandom(16).hex(), port=meta['ssh_port'],
            ssh=['ssh', '-F', '/dev/null', '-i', str(lab/'client'), '-p', str(meta['ssh_port']),
                 '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'StrictHostKeyChecking=yes',
                 '-o', 'UserKnownHostsFile='+str(lab/'known_hosts'), 'fixture@127.0.0.1']))
    report = dict(status='running', scope=__doc__, serial=args.serial, receiver=receiver,
        source_commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
        worktree_dirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=ROOT)),
        apk_sha256=hashlib.sha256(args.apk.read_bytes()).hexdigest(),
        test_apk_sha256=hashlib.sha256(args.test_apk.read_bytes()).hexdigest(),
        runner_identity=process_identity(os.getpid()), qemu_identities=[x['identity'] for x in endpoints],
        harness_sources={str(f.relative_to(ROOT)):hashlib.sha256(f.read_bytes()).hexdigest()
            for f in (Path(__file__).resolve(),ROOT/'tools/lab_live_profiles.py',ROOT/'tools/lab_receiver_upgrade.py')},
        rounds=[], stages=[], restores=[])
    def save():
        temp=output/'result.tmp';temp.write_text(json.dumps(report,indent=2)+'\n');temp.replace(output/'result.json')
    def guest(index, action):
        e=endpoints[index]
        if process_identity(e['identity']['pid']) != e['identity']: raise RuntimeError('VM identity changed')
        settings=dict(action=action,transport=e['transport'],peer=e['peer'],current_sha256=receiver['sha256'])
        if 'before' in e: settings['before']=e['before']
        source=('import types,sys\nmodule=types.ModuleType("lab_receiver_upgrade")\nexec('+repr(common)+',module.__dict__)\n'
            'sys.modules["lab_receiver_upgrade"]=module\nSETTINGS='+repr(settings)+'\n'+helper)
        result=subprocess.run(e['ssh']+['sudo -n python3 -'],input=source,text=True,capture_output=True,timeout=160)
        if result.returncode:
            # stderr contains bounded assertion/traceback, never the private settings.
            (output/('guest-'+str(index)+'-'+action+'-'+str(time.time_ns())+'.stderr')).write_text(result.stderr)
            raise RuntimeError('Owned guest action failed: '+action)
        return json.loads(result.stdout)
    def monitor(command):
        e=endpoints[1]
        if process_identity(e['identity']['pid']) != e['identity']: raise RuntimeError('VM identity changed')
        with socket.socket(socket.AF_UNIX) as s:
            s.settimeout(10);s.connect(str(e['lab']/'monitor.sock'))
            for request in (None,command):
                if request is not None:s.sendall(request.encode()+b'\n')
                data=b''
                while b'(qemu)' not in data:
                    part=s.recv(8192)
                    if not part:raise RuntimeError('QEMU monitor ended')
                    data+=part
                if request and any(x in data for x in (b'Error',b'Could not',b'failed')):raise RuntimeError('Forward change failed')
    marker=os.urandom(16).hex();forward=None;proc=None
    remote=f'/sdcard/Android/data/{APP}/files';started=time.monotonic()
    try:
        for i,e in enumerate(endpoints):
            e['before']=guest(i,'before')
            # Exact recovery configuration is private; retained if the host dies.
            with (output/f'private-recovery-{i}.json').open('x') as f:
                os.chmod(f.name,0o600);json.dump(e['before'],f)
        report['baselines']=[e['before']['state'] for e in endpoints];save()
        guest(1,'direct')
        with socket.socket() as s:s.bind(('127.0.0.1',0));port=s.getsockname()[1]
        monitor(f'hostfwd_add lab tcp:127.0.0.1:{port}-:53412');forward=port
        a=endpoints[0];lab=a['lab']
        pins=[base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(l.split()[2])).digest()).decode().rstrip('=')
            for l in (lab/'known_hosts').read_text().splitlines() if len(l.split())>=3 and l.split()[1]=='ssh-ed25519']
        if len(pins)!=1:raise ValueError('One pinned SSH host key required')
        code=(endpoints[1]['lab']/'direct-pair-code').read_text().strip()
        if not code.startswith('usbmost:1:'):raise ValueError('Saved receiver identity required')
        raw=code[len('usbmost:1:'):];pair=json.loads(base64.urlsafe_b64decode(raw+'='*(-len(raw)%4)))
        token=(endpoints[1]['lab']/'direct-token').read_text().strip()
        if not re.fullmatch('[A-Za-z0-9_-]{43}',token):raise ValueError('Saved paired token required')
        computers=[dict(name='SSH lab',profile=dict(transport='ssh',endpoint=dict(host='10.0.2.2',port=a['port']),
            user='fixture',host_key=pins[0],reverse_port=0,prepare=True),credentials=dict(private_key=(lab/'client').read_text(),client_id=a['peer'])),
            dict(name='TLS lab',profile=dict(transport='direct',endpoint=dict(host='10.0.2.2',port=forward),
            server_pin=pair['server_pin']),credentials=dict(token=token))]
        fixture=dict(owned_emulator_only=True,marker=marker,computers=computers)
        android('shell','am','force-stop',APP)
        for apk in (args.apk,args.test_apk):
            if 'Success' not in android('install','-r',str(apk.resolve())):raise RuntimeError('Install failed')
        if 'Success' not in android('shell','pm','clear',APP):raise RuntimeError('Owned app clear failed')
        android('shell','input','keyevent','KEYCODE_WAKEUP');android('shell','wm','dismiss-keyguard')
        for stage in ('first','resume'):
            android('shell','am','force-stop',APP)
            path=output/'private-input.json'
            with path.open('x') as f:os.chmod(f.name,0o600);json.dump(fixture,f)
            android('shell','mkdir','-p',remote);android('push',str(path),remote+'/live-profiles-fixture.json');path.unlink()
            logpath=output/(stage+'.log')
            with logpath.open('x') as log:
                proc=subprocess.Popen(adb+['shell','am','instrument','-w','-r','-e','suite','live-profiles','-e','stage',stage,
                    APP+'.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],stdout=log,stderr=subprocess.STDOUT)
                for round_index in range(3):
                    phase=f'PROFILES_CONNECTED stage={stage} round={round_index}';deadline=time.monotonic()+180
                    while phase not in logpath.read_text():
                        if proc.poll() is not None or time.monotonic()>=deadline:raise RuntimeError('Missing phase '+phase)
                        time.sleep(.1)
                    snapshots=[guest(i,'connected' if i==round_index%2 else 'idle') for i in range(2)]
                    report['rounds'].append(dict(stage=stage,round=round_index,receivers=snapshots));save()
                    ack=output/'ack';ack.write_bytes(b'\x01')
                    android('push',str(ack),remote+f'/profiles-{marker}-{stage}-{round_index}.go');ack.unlink()
                proc.wait(timeout=90)
                logtext=logpath.read_text()
                if proc.returncode or 'INSTRUMENTATION_CODE: -1' not in logtext or 'FAIL ' in logtext:
                    raise RuntimeError('Instrumentation failed; see private log')
            proc=None
            for i in range(2):guest(i,'idle')
            android('pull',remote+f'/live-profiles-{marker}/.',str(output/'proof'))
            report['stages'].append(json.loads((output/'proof'/f'{stage}.json').read_text()));save()
        report['status']='passed'
    except BaseException as error:
        report['status']='failed';report['error']=str(error);raise
    finally:
        errors=[]
        try:android('shell','am','force-stop',APP)
        except BaseException as e:errors.append('Android stop: '+str(e))
        try:android('shell','rm','-f',remote+'/live-profiles-fixture.json')
        except BaseException as e:errors.append('Owned fixture retirement: '+str(e))
        if proc is not None:
            try:proc.wait(timeout=15)
            except subprocess.TimeoutExpired:proc.terminate();proc.wait(timeout=5)
        for i,e in enumerate(endpoints):
            if 'before' in e:
                try:report['restores'].append(dict(index=i,state=guest(i,'restore')))
                except BaseException as error:errors.append('Receiver restore: '+str(error))
        if forward is not None:
            try:monitor(f'hostfwd_remove lab tcp:127.0.0.1:{forward}')
            except BaseException as error:errors.append('Owned forward removal: '+str(error))
        report['elapsed_seconds']=round(time.monotonic()-started,3)
        if errors:report['status']='cleanup_failed';report['cleanup_errors']=errors
        save()
        if errors:raise RuntimeError('Recovery evidence retained: '+'; '.join(errors))


if __name__ == '__main__':
    main()
