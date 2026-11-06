#!/usr/bin/env python3
"""Actual optimized APK/Keystore/SSH/bootstrap; disposable emulator + owned QEMU only.

The instrumentation APK contains private fixture credentials and must never be
distributed. The product APK contains no fixture IPC or special test flags.
"""
from pathlib import Path
import argparse
import hashlib
import json
import os
import re
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--adb', type=Path, required=True)
parser.add_argument('--serial', required=True)
parser.add_argument('--apk', type=Path, required=True)
parser.add_argument('--test-apk', type=Path, required=True)
parser.add_argument('--lab', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--prepare-soak', action='store_true',
                    help='Retain only the verified owned-QEMU profile after a successful test; service is stopped')
args = parser.parse_args()
assert args.serial.startswith('emulator-'), 'refuse a physical Android device'
lab = args.lab.resolve()
output = args.output.resolve()
assert lab.is_relative_to(ROOT / 'build/vm-lab')
assert output.is_relative_to(ROOT / 'build/e2e')
metadata = json.loads((lab / 'connection.json').read_text())
assert metadata['ssh_host'] == '127.0.0.1' and metadata['ssh_user'] == 'fixture'
pid = int((lab / 'pid').read_text())
command = Path('/proc', str(pid), 'cmdline').read_bytes()
assert b'qemu-system-x86_64' in command and str(lab / 'guest.qcow2').encode() in command
with zipfile.ZipFile(args.test_apk) as archive:
    fixture = json.loads(archive.read('assets/receiver-fixture.json'))
assert fixture['scope'] == 'isolated-QEMU-only' and fixture['host'] == '10.0.2.2'
assert fixture['port'] == metadata['ssh_port'] and fixture['user'] == 'fixture'
assert fixture['private_key'] == (lab / 'client').read_text()
del fixture
with zipfile.ZipFile(args.apk) as archive:
    assert 'assets/receiver-fixture.json' not in archive.namelist()

adb = [str(args.adb.resolve()), '-s', args.serial]


def run(command, **kwargs):
    return subprocess.run(adb + command, check=True, timeout=60, **kwargs)


assert run(['shell', 'getprop', 'ro.kernel.qemu'], capture_output=True, text=True).stdout.strip() == '1'
output.mkdir(mode=0o700, parents=True, exist_ok=True)
output.chmod(0o700)
app = 'ru.levchatkin.rutokenbridge'
run(['shell', 'am', 'force-stop', app], stdout=subprocess.DEVNULL)
for apk in [args.apk, args.test_apk]:
    run(['install', '-r', str(apk.resolve())], stdout=subprocess.DEVNULL)
api = int(run(['shell', 'getprop', 'ro.build.version.sdk'], capture_output=True, text=True).stdout)
# Permission policy is tested by the general release suite. Here isolate the
# secure first-use workflow from the unrelated Android13 permission dialog.
granted = False
if api >= 33:
    package = run(['shell', 'dumpsys', 'package', app], capture_output=True, text=True).stdout
    previous = re.search(r'android\.permission\.POST_NOTIFICATIONS: granted=(true|false)', package)
    assert previous, 'could not determine prior emulator notification permission'
    granted = previous[1] == 'false'
if granted:
    run(['shell', 'pm', 'grant', app, 'android.permission.POST_NOTIFICATIONS'])
log_path = output / 'instrumentation.log'
try:
    with log_path.open('w') as log:
        os.chmod(log_path, 0o600)
        result = subprocess.run(
            adb + ['shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'connection',
                   '-e', 'retain-owned-fixture', 'true' if args.prepare_soak else 'false',
                   app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
            stdout=log, stderr=subprocess.STDOUT, timeout=600)
finally:
    try:
        run(['shell', 'am', 'force-stop', app], stdout=subprocess.DEVNULL)
    finally:
        if granted:
            run(['shell', 'pm', 'revoke', app, 'android.permission.POST_NOTIFICATIONS'])
text = log_path.read_text()
# adb can return exit0 even when instrumentation reports a failed test.
passed = result.returncode == 0 and 'INSTRUMENTATION_CODE: -1' in text and 'FAIL ' not in text
report = {
    'status': 'PASS' if passed else 'FAIL', 'api': api,
    'owned_fixture_retained': passed and args.prepare_soak,
    'apk_sha256': hashlib.sha256(args.apk.read_bytes()).hexdigest(),
    'test_apk_sha256': hashlib.sha256(args.test_apk.read_bytes()).hexdigest(),
    'scope': 'optimized APK, actual Keystore and isolated QEMU SSH/receiver; no physical USB',
}
(output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report))
if not passed:
    raise SystemExit('Connection E2E failed; inspect retained private instrumentation.log')
