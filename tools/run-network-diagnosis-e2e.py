#!/usr/bin/env python3
"""Real refused TCP and silent TLS peers on owned Android emulators; no VM or USB."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--adb', type=Path, required=True)
    p.add_argument('--serial', required=True)
    p.add_argument('--apk', type=Path, required=True)
    p.add_argument('--test-apk', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    if not re.fullmatch(r'emulator-\d+', a.serial) or a.serial == 'emulator-5566':
        raise ValueError('Only unprotected owned emulator serials')
    output = a.output.resolve()
    if not output.is_relative_to(ROOT / 'build/e2e'):
        raise ValueError('Keep test evidence under build/e2e')
    adb = [str(a.adb.resolve()), '-s', a.serial]

    def android(*args, timeout=60):
        return subprocess.check_output(adb + list(args), text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()

    if android('shell', 'getprop', 'ro.kernel.qemu') != '1':
        raise ValueError('No physical Android devices')
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    stop = threading.Event()
    counts = {'accepted': 0, 'client_closed': 0, 'hello_bytes': 0}
    durations, errors = [], []
    # A bound socket without listen reserves the refusal port for this test.
    refused, silent = socket.socket(), socket.socket()
    refused.bind(('127.0.0.1', 0))
    silent.bind(('127.0.0.1', 0))
    silent.listen(4)
    silent.settimeout(.2)

    def serve():
        try:
            while not stop.is_set():
                try:
                    connection, _ = silent.accept()
                except socket.timeout:
                    continue
                counts['accepted'] += 1
                started = time.monotonic()
                with connection:
                    connection.settimeout(.2)
                    while not stop.is_set():
                        try:
                            data = connection.recv(4096)
                        except socket.timeout:
                            continue
                        except ConnectionResetError:
                            data = b''
                        if not data:
                            counts['client_closed'] += 1
                            durations.append(round(time.monotonic() - started, 3))
                            break
                        # Count only; never retain TLS hello bytes or peer data.
                        counts['hello_bytes'] += len(data)
        except BaseException as error:
            errors.append(type(error).__name__)

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    remote = f'/sdcard/Android/data/{APP}/files/diagnosis-fixture.json'
    fixture = output / 'diagnosis-fixture.json'
    report = {'status': 'running', 'scope': __doc__,
              'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'apk_sha256': hashlib.file_digest(a.apk.open('rb'), 'sha256').hexdigest(),
              'test_apk_sha256': hashlib.file_digest(a.test_apk.open('rb'), 'sha256').hexdigest()}
    try:
        def token():
            return base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip('=')

        cases = [dict(name=name, failure=failure,
                      profile=dict(transport='direct', endpoint=dict(host='10.0.2.2', port=peer.getsockname()[1]),
                                   server_pin=token()), credentials=dict(token=token()))
                 for name, failure, peer in [('tcp-refused', 'network_unavailable', refused),
                                             ('tls-handshake-timeout', 'timeout', silent)]]
        with fixture.open('x') as stream:
            fixture.chmod(0o600)
            json.dump(dict(owned_emulator_only=True, network_only=True, cases=cases), stream)
        android('shell', 'am', 'force-stop', APP)
        for apk in (a.apk, a.test_apk):
            if 'Success' not in android('install', '-r', str(apk.resolve())):
                raise RuntimeError('APK installation failed')
        android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        android('shell', 'wm', 'dismiss-keyguard')
        android('shell', 'mkdir', '-p', f'/sdcard/Android/data/{APP}/files')
        android('push', str(fixture), remote)
        fixture.unlink()
        result = android('shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'diagnostics',
                         APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation', timeout=220)
        (output / 'instrumentation.log').write_text(result + '\n')
        if 'INSTRUMENTATION_CODE: -1' not in result or 'FAIL ' in result:
            raise RuntimeError('Android diagnostic checks failed; inspect retained instrumentation log')
        android('pull', f'/sdcard/Android/data/{APP}/files/diagnosis-network-proof/.', str(output))
        report['ui'] = json.loads((output / 'result.json').read_text())
        if counts['accepted'] < 1 or counts['client_closed'] < 1 or not any(14 <= d <= 30 for d in durations):
            raise RuntimeError('Actual bounded silent TLS timeout was not observed')
        # The UI has explicitly stopped both attempts before instrumentation returns.
        before = dict(counts)
        time.sleep(5)
        if counts != before:
            raise RuntimeError('Traffic continued after explicit stop')
        report['status'] = 'passed'
    except BaseException as error:
        report['status'] = 'failed'
        report['error'] = str(error)
        raise
    finally:
        cleanup = []
        for action in [lambda: android('shell', 'am', 'force-stop', APP),
                       lambda: android('shell', 'rm', '-f', remote),
                       lambda: fixture.unlink(missing_ok=True)]:
            try:
                action()
            except BaseException as error:
                cleanup.append(type(error).__name__)
        stop.set()
        thread.join(3)
        silent.close()
        refused.close()
        if thread.is_alive() or errors:
            cleanup += errors or ['silent peer did not stop']
        report.update(peer=counts, handshake_seconds=durations, cleanup_errors=cleanup)
        if cleanup:
            report['status'] = 'failed'
        (output / 'runner-result.json').write_text(json.dumps(report, indent=2) + '\n')
        if cleanup:
            raise RuntimeError('Owned peer cleanup failed')
    print('PASS refused TCP, actual TLS timeout, diagnosis, recreation and explicit retry cancellation')


if __name__ == '__main__':
    main()
