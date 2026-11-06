#!/usr/bin/env python3
"""Actual APK/service SSH upgrade1.0.11 -> embedded receiver, owned QEMU only.

The guest helper preserves recovery material and exact prior identity/pairings.
Only the Android app supplies the new receiver during the measured transition.
No physical USB, driver/package download, mount or recursive cleanup.
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
import subprocess
import time
import zipfile
from lab_android_active_soak import process_identity
from lab_receiver_upgrade import OLD_SHA

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('adb', 'apk', 'test-apk', 'lab', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--serial', required=True)
    args = parser.parse_args()
    if not re.fullmatch(r'emulator-\d+', args.serial) or args.serial == 'emulator-5566':
        raise ValueError('Only unprotected owned emulators')
    protected = ROOT / 'build/e2e/current-active-service-eight-hour-api36/result.json'
    if protected.exists():
        record = json.loads(protected.read_text())
        if record.get('serial') == args.serial:
            identity = record['runner_identity']
            try:
                actual = process_identity(identity['pid'])
            except (FileNotFoundError, ProcessLookupError):
                actual = None
            if actual == identity:
                raise ValueError('Android belongs to the live active soak')
    lab, output = args.lab.resolve(), args.output.resolve()
    if not lab.is_relative_to(ROOT / 'build/vm-lab') or not output.is_relative_to(ROOT / 'build/e2e'):
        raise ValueError('Only owned lab/evidence paths')
    qemu = process_identity(int((lab / 'pid').read_text()))
    command = Path('/proc', str(qemu['pid']), 'cmdline').read_bytes()
    if (b'qemu-system-x86_64' not in command or str(lab / 'guest.qcow2').encode() not in command
            or any(word in command for word in (b'usb-host', b'vfio-pci', b'-virtfs', b'-fsdev', b'/dev/sd', b'/dev/nvme'))):
        raise ValueError('Expected owned QEMU without host passthrough')
    with (lab / 'soak.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        run(args, lab, output, qemu)


def run(args, lab, output, qemu):
    metadata = json.loads((lab / 'connection.json').read_text())
    if metadata['ssh_host'] != '127.0.0.1' or metadata['ssh_user'] != 'fixture':
        raise ValueError('Only owned loopback guest')
    key = lab / 'client'
    if key.is_symlink() or not key.is_file() or not 0 < key.stat().st_size < 16384:
        raise ValueError('Invalid owned SSH credential')
    old = ROOT / 'build/internal/usb-most-onboarding-1.0.11/usb-most-linux-x86_64'
    old_bytes = old.read_bytes()
    if hashlib.sha256(old_bytes).hexdigest() != OLD_SHA:
        raise ValueError('Historical receiver hash differs')
    with zipfile.ZipFile(args.apk) as archive:
        manifest = json.loads(archive.read('assets/receivers/manifest.json'))
        artifact = next(x for x in manifest['artifacts'] if x['name'] == 'usb-most-linux-x86_64')
        payload = gzip.decompress(archive.read('assets/receivers/usb-most-linux-x86_64.gz.bin'))
    if len(payload) != artifact['bytes'] or hashlib.sha256(payload).hexdigest() != artifact['sha256']:
        raise ValueError('Embedded receiver differs from APK manifest')
    del payload
    ssh = ['ssh', '-F', '/dev/null', '-i', str(key), '-p', str(metadata['ssh_port']),
           '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'), 'fixture@127.0.0.1']
    adb = [str(args.adb.resolve()), '-s', args.serial]

    def android(*words, timeout=60):
        return subprocess.check_output(adb + list(words), text=True, stderr=subprocess.STDOUT,
                                       timeout=timeout).strip()

    if android('shell', 'getprop', 'ro.kernel.qemu') != '1':
        raise ValueError('Refuse physical Android')
    output.mkdir(parents=True, mode=0o700, exist_ok=False)
    nonce, peer = os.urandom(16).hex(), os.urandom(16).hex()
    guest_source = (ROOT / 'tools/lab_receiver_upgrade.py').read_text()

    def guest(action, **extra):
        settings = dict(action=action, nonce=nonce, current_sha256=artifact['sha256'], peer=peer, **extra)
        result = subprocess.run(ssh + ['sudo -n python3 -'],
            input='SETTINGS = ' + repr(settings) + '\n' + guest_source,
            text=True, capture_output=True, timeout=180)
        if result.returncode:
            (output / ('guest-' + action + '.stderr')).write_text(result.stderr)
            raise RuntimeError('Owned receiver ' + action + ' failed; recovery files retained')
        return json.loads(result.stdout)

    report = dict(status='running', scope=__doc__, serial=args.serial, qemu_identity=qemu,
        source_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        worktree_dirty=bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
        runner_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        guest_source_sha256=hashlib.sha256(guest_source.encode()).hexdigest(),
        apk_sha256=hashlib.sha256(args.apk.read_bytes()).hexdigest(),
        test_apk_sha256=hashlib.sha256(args.test_apk.read_bytes()).hexdigest(),
        old_receiver_sha256=OLD_SHA, expected_receiver=artifact, phases=[], rounds=[])
    fixture = output / 'diagnosis-fixture.json'
    remote_root = f'/sdcard/Android/data/{APP}/files'
    remote = remote_root + '/diagnosis-fixture.json'
    instrumentation = None
    attempted = False
    began = time.monotonic()
    log = (output / 'instrumentation.log').open('w')

    def save():
        (output / 'runner-result.json').write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')

    try:
        android('shell', 'am', 'force-stop', APP)
        attempted = True
        report['preparation'] = guest('prepare', old_binary=base64.b64encode(old_bytes).decode())
        del old_bytes
        save()
        for apk in (args.apk, args.test_apk):
            if 'Success' not in android('install', '-r', str(apk.resolve())):
                raise RuntimeError('Owned APK install failed')
        android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        android('shell', 'wm', 'dismiss-keyguard')
        pins = [base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(line.split()[2])).digest()).decode().rstrip('=')
                for line in (lab / 'known_hosts').read_text().splitlines()
                if len(line.split()) >= 3 and line.split()[1] == 'ssh-ed25519']
        if len(pins) != 1:
            raise ValueError('Expected exact owned Ed25519 host pin')
        config = dict(owned_emulator_only=True, upgrade_only=True, upgrade_marker=nonce, cases=[dict(
            name='receiver-upgrade', profile=dict(transport='ssh', endpoint=dict(host='10.0.2.2', port=metadata['ssh_port']),
                user='fixture', host_key=pins[0], reverse_port=0, prepare=True),
            credentials=dict(private_key=key.read_text(), client_id=peer))])
        with fixture.open('x') as stream:
            fixture.chmod(0o600); json.dump(config, stream)
        android('shell', 'mkdir', '-p', remote_root)
        android('push', str(fixture), remote); fixture.unlink()
        instrumentation = subprocess.Popen(adb + ['shell', 'am', 'instrument', '-w', '-r', '-e',
            'suite', 'diagnostics', APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
            stdout=log, stderr=subprocess.STDOUT)
        for index in range(2):
            phase = 'UPGRADE_CONNECTED round=' + str(index)
            deadline = time.monotonic() + 210
            while phase not in (output / 'instrumentation.log').read_text():
                if instrumentation.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError('Actual Android service did not reach ' + phase)
                time.sleep(.1)
            report['phases'].append(dict(phase=phase, elapsed_seconds=round(time.monotonic() - began, 3)))
            observed = guest('observe')
            if observed['version'] != manifest['version']:
                raise RuntimeError('Active receiver version differs from APK')
            if index:
                for name in ('receiver.bin', 'service.unit', 'modules.conf'):
                    if observed['files'][name] != report['rounds'][0]['files'][name]:
                        raise RuntimeError('Repeat connection replaced unchanged receiver files')
                report['binary_reused_on_repeat'] = True
            report['rounds'].append(observed); save()
            marker = output / ('release-' + str(index)); marker.write_bytes(b'\x01')
            target = remote_root + f'/upgrade-{nonce}-{index}.go'
            android('push', str(marker), target + '.part')
            android('shell', 'mv', target + '.part', target)
        if instrumentation.wait(timeout=45) != 0:
            raise RuntimeError('Instrumentation exited unsuccessfully')
        text = (output / 'instrumentation.log').read_text()
        if 'INSTRUMENTATION_CODE: -1' not in text or 'FAIL ' in text:
            raise RuntimeError('Actual upgrade UI checks failed')
        for name in ('result.json', 'receiver-upgrade-0.png', 'receiver-upgrade-1.png'):
            android('pull', remote_root + '/diagnosis-upgrade-proof/' + name, str(output / name))
        ui = json.loads((output / 'result.json').read_text())
        if ui['status'] != 'passed' or ui['cases'] != [dict(case='receiver-upgrade', status='passed', receiver_verified=True, rounds=2)]:
            raise RuntimeError('Unexpected UI evidence')
        report.update(status='passed', ui=ui)
    except BaseException as error:
        report.update(status='failed', error=str(error)); raise
    finally:
        errors = []
        def cleanup(label, action):
            try: action()
            except BaseException as error: errors.append(label + ': ' + type(error).__name__)
        def stop_instrumentation():
            if instrumentation is not None and instrumentation.poll() is None:
                instrumentation.terminate(); instrumentation.wait(timeout=10)
        cleanup('stop instrumentation', stop_instrumentation)
        cleanup('stop owned app', lambda: android('shell', 'am', 'force-stop', APP))
        cleanup('remove private fixture', lambda: android('shell', 'rm', '-f', remote))
        cleanup('remove local fixture', lambda: fixture.unlink(missing_ok=True))
        for index in range(2):
            for suffix in ('.go', '.go.part'):
                path = remote_root + f'/upgrade-{nonce}-{index}' + suffix
                cleanup('remove owned marker', lambda path=path: android('shell', 'rm', '-f', path))
        if attempted:
            def restore(): report['restoration'] = guest('restore')
            cleanup('restore exact original deployment', restore)
        report['cleanup_errors'] = errors
        if errors: report['status'] = 'failed'
        save(); log.close()
        if errors: raise RuntimeError('Upgrade restoration incomplete; preserve recovery directory')
    print('PASS actual service receiver upgrade, repeat reuse, trust preservation and original deployment restoration')


if __name__ == '__main__':
    main()
