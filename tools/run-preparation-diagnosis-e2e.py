#!/usr/bin/env python3
"""Actual Android SSH preparation refusal in owned QEMU Linux guests.

No physical USB. The non-admin case preserves the installed receiver and removes
only its disposable account. The missing-driver case only observes an untouched
guest: no account, module, package, service or policy mutation is permitted.
"""
import argparse
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
from lab_android_active_soak import process_identity

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'


def validate_serial(serial):
    if not re.fullmatch(r'emulator-\d+', serial) or serial == 'emulator-5566':
        raise ValueError('Only unprotected owned emulators')
    protected = ROOT / 'build/e2e/current-active-service-eight-hour-api36/result.json'
    if protected.exists():
        owner = json.loads(protected.read_text())
        if owner.get('serial') == serial:
            identity = owner['runner_identity']
            try:
                live = process_identity(identity['pid'])
            except (FileNotFoundError, ProcessLookupError):
                live = None
            if live == identity:
                raise ValueError('Android belongs to the live protected active soak')


def collect_proof(android, output, cases):
    remote = f'/sdcard/Android/data/{APP}/files/diagnosis-preparation-proof/'
    android('pull', remote + 'result.json', str(output / 'result.json'))
    result = json.loads((output / 'result.json').read_text())
    expected = [dict(case=case['name'], status='passed', expected_failure=case['failure'],
                     observed_failure=case['failure']) for case in cases]
    if result.get('status') != 'passed' or result.get('cases') != expected:
        raise RuntimeError('Diagnostic proof does not match the current requested cases')
    for case in cases:
        name = case['name']
        if not re.fullmatch(r'[a-z0-9-]{1,48}', name):
            raise ValueError('Invalid owned proof name')
        android('pull', remote + name + '.png', str(output / (name + '.png')))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['adb', 'apk', 'test-apk', 'lab', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--missing-driver', action='store_true',
                        help='Read-only SELinux Enforcing guest without vhci_hcd or installed receiver')
    args = parser.parse_args()
    validate_serial(args.serial)
    lab, output = args.lab.resolve(), args.output.resolve()
    if not (lab.is_relative_to(ROOT / 'build/vm-lab')
            and output.is_relative_to(ROOT / 'build/e2e')):
        raise ValueError('Only owned test and evidence directories')
    command = Path('/proc', (lab / 'pid').read_text().strip(), 'cmdline').read_bytes()
    if (b'qemu-system-x86_64' not in command or str(lab / 'guest.qcow2').encode() not in command
            or any(word in command for word in
                   [b'usb-host', b'vfio-pci', b'-virtfs', b'-fsdev', b'/dev/sd', b'/dev/nvme'])):
        raise ValueError('Expected owned QEMU without host device or filesystem passthrough')
    # Acquire before any Android action or guest mutation; never bypass a soak.
    with (lab / 'soak.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        run(args, lab, output, command)


def run(args, lab, output, command):
    metadata = json.loads((lab / 'connection.json').read_text())
    if metadata['ssh_host'] != '127.0.0.1' or metadata['ssh_user'] != 'fixture':
        raise ValueError('Only lab loopback SSH')
    key = lab / 'client'
    if key.is_symlink() or not key.is_file() or not 0 < key.stat().st_size <= 16384:
        raise ValueError('Missing bounded lab credential')
    ssh = ['ssh', '-F', '/dev/null', '-i', str(key), '-p', str(metadata['ssh_port']),
           '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'), 'fixture@127.0.0.1']
    adb = [str(args.adb.resolve()), '-s', args.serial]

    def android(*words, timeout=60):
        return subprocess.check_output(adb + list(words), text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()

    if android('shell', 'getprop', 'ro.kernel.qemu') != '1':
        raise ValueError('No physical Android devices')
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    guest_code = (ROOT / 'tools' / ('lab_missing_driver.py' if args.missing_driver
                                  else 'lab_unprivileged_user.py')).read_text()
    nonce = os.urandom(16).hex()

    def guest(action, **values):
        settings = dict(action=action, nonce=nonce, **values)
        result = subprocess.run(ssh + ['sudo -n python3 -'], text=True, capture_output=True,
                                input='SETTINGS = ' + repr(settings) + '\n' + guest_code, timeout=45)
        if result.returncode:
            (output / ('guest-' + action + '.stderr')).write_text(result.stderr)
            raise RuntimeError('Owned guest ' + action + ' failed; inspect retained error')
        return json.loads(result.stdout)

    report = dict(status='running', scope=__doc__, fixture_nonce=nonce,
                  scenario='missing_driver' if args.missing_driver else 'no_administrator',
                  source_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                  qemu_cmd_sha256=hashlib.sha256(command).hexdigest(),
                  apk_sha256=hashlib.sha256(args.apk.read_bytes()).hexdigest(),
                  test_apk_sha256=hashlib.sha256(args.test_apk.read_bytes()).hexdigest())
    fixture = output / 'diagnosis-fixture.json'
    remote = f'/sdcard/Android/data/{APP}/files/diagnosis-fixture.json'
    created = False
    before = None
    try:
        before = guest('snapshot')
        report['receiver_before'] = before
        if args.missing_driver:
            account = dict(user='fixture', uid=int(subprocess.check_output(
                ssh + ['id -u'], text=True, timeout=20).strip()))
        else:
            public_key = subprocess.check_output(['ssh-keygen', '-y', '-f', str(key)], text=True, timeout=10)
            created = True  # Preserve a partial create if cleanup cannot prove ownership.
            account = guest('create', public_key=public_key)
        report['account'] = account
        # Prove SSH authentication succeeds independently, before testing sudo refusal.
        actual_uid = subprocess.check_output(ssh[:-1] + [account['user'] + '@127.0.0.1', 'id -u'],
                                             text=True, timeout=20).strip()
        if actual_uid != str(account['uid']):
            raise RuntimeError('Disposable account SSH identity did not match')
        report['ssh_authentication_passed'] = True
        pins = [base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(line.split()[2])).digest()).decode().rstrip('=')
                for line in (lab / 'known_hosts').read_text().splitlines()
                if len(line.split()) >= 3 and line.split()[1] == 'ssh-ed25519']
        if len(pins) != 1:
            raise ValueError('Expected one trusted lab host key')
        cases = []
        inputs = ([('ssh-linux-driver-missing', None)] if args.missing_driver else
                  [('ssh-no-admin-no-password', None),
                   ('ssh-no-admin-wrong-password', 'synthetic-wrong-' + os.urandom(16).hex())])
        for name, password in inputs:
            credentials = dict(private_key=key.read_text(), client_id=os.urandom(16).hex())
            if password is not None:
                credentials['sudo_password'] = password
            cases.append(dict(name=name, failure='linux_usb_driver_unavailable' if args.missing_driver
                              else 'preparation_failed', credentials=credentials,
                profile=dict(transport='ssh', endpoint=dict(host='10.0.2.2', port=metadata['ssh_port']),
                             user=account['user'], host_key=pins[0], reverse_port=0, prepare=True),
                summary=('В текущем ядре Linux недоступен драйвер USB/IP (vhci_hcd). Нужен совместимый пакет '
                         'модуля для этого ядра; отключение SELinux не решает эту причину.' if args.missing_driver else
                         'Подготовка компьютера не завершилась. Причина может быть в разрешениях или '
                         'компонентах системы; проверьте установщик на компьютере.')))
        with fixture.open('x') as stream:
            fixture.chmod(0o600)
            json.dump(dict(owned_emulator_only=True, preparation_only=True, cases=cases), stream)
        android('shell', 'am', 'force-stop', APP)
        for apk in (args.apk, args.test_apk):
            if 'Success' not in android('install', '-r', str(apk.resolve())):
                raise RuntimeError('APK install failed')
        android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        android('shell', 'wm', 'dismiss-keyguard')
        android('shell', 'mkdir', '-p', f'/sdcard/Android/data/{APP}/files')
        android('push', str(fixture), remote)
        fixture.unlink()
        result = android('shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'diagnostics',
                         APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation', timeout=220)
        (output / 'instrumentation.log').write_text(result + '\n')
        if 'INSTRUMENTATION_CODE: -1' not in result or 'FAIL ' in result:
            raise RuntimeError('Android diagnosis failed; inspect instrumentation log')
        report['ui'] = collect_proof(android, output, cases)
        report['status'] = 'passed'
    except BaseException as error:
        report.update(status='failed', error=str(error))
        raise
    finally:
        errors = []

        def cleanup(label, action):
            try:
                action()
            except BaseException as error:
                errors.append(label + ': ' + type(error).__name__)

        cleanup('stop test application', lambda: android('shell', 'am', 'force-stop', APP))
        cleanup('remove remote fixture', lambda: android('shell', 'rm', '-f', remote))
        cleanup('remove local fixture', lambda: fixture.unlink(missing_ok=True))
        if created:
            # SSH children need a bounded interval to observe the closed transport.
            time.sleep(2)
            def remove_account():
                report['account_cleanup'] = guest('remove')
            cleanup('remove owned account', remove_account)
        if before is not None:
            def verify_receiver():
                after = guest('snapshot')
                report['receiver_after'] = after
                # Audit backlog may briefly change as SSH sessions end. Lost
                # events/enforcement are independently checked by the observer.
                compare_before = {k: v for k, v in before.items() if not args.missing_driver or k != 'audit'}
                compare_after = {k: v for k, v in after.items() if not args.missing_driver or k != 'audit'}
                if compare_before != compare_after:
                    raise RuntimeError('Receiver installation or state changed after refusal')
                report['receiver_unchanged'] = True
            cleanup('verify unchanged receiver', verify_receiver)
        report['cleanup_errors'] = errors
        if errors:
            report['status'] = 'failed'
        (output / 'runner-result.json').write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
        if errors:
            raise RuntimeError('Owned fixture cleanup or receiver preservation failed')
    print('PASS real SSH prerequisite refusal, exact diagnosis, saved secrets and receiver preservation')


if __name__ == '__main__':
    main()
