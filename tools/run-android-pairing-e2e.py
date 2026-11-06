#!/usr/bin/env python3
"""QR/camera/NSD tests on an explicitly disposable emulator, no physical USB.
Resets only our test app on that emulator; every installation must succeed.
The default camera lifecycle suite requires a scene WITHOUT a valid pairing QR;
use --camera-qr separately with the imagefile fixture (automatic scanner completion).
"""
import argparse, hashlib, json, pathlib, re, subprocess
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', type=pathlib.Path, required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=pathlib.Path, required=True)
p.add_argument('--test-apk', type=pathlib.Path, required=True)
p.add_argument('--camera-qr', action='store_true', help='AVD must use the prepared imagefile camera fixture')
p.add_argument('--output', type=pathlib.Path, required=True)
a = p.parse_args()
assert re.fullmatch(r'emulator-\d+', a.serial)
adb = [str(a.adb), '-s', a.serial]
def run(*args):
    return subprocess.check_output(adb + list(args), text=True, stderr=subprocess.STDOUT, timeout=90).strip()
assert run('shell', 'getprop', 'ro.kernel.qemu') == '1'
assert run('shell', 'getprop', 'sys.boot_completed') == '1'
api = int(run('shell', 'getprop', 'ro.build.version.sdk'))
a.output.mkdir(parents=True, exist_ok=True, mode=0o700)
app = 'ru.levchatkin.rutokenbridge'
for pkg in [app + '.test', app]:
    if run('shell', 'pm', 'path', pkg):
        assert 'Success' in run('uninstall', pkg)
for file in [a.apk, a.test_apk]:
    assert 'Success' in run('install', '--no-incremental', str(file.resolve()))
if api >= 33:
    run('shell', 'pm', 'grant', app, 'android.permission.POST_NOTIFICATIONS')
results = []
for suite in (['camera-qr'] if a.camera_qr else ['camera-denied', 'pairing']):
    run('shell', 'am', 'force-stop', app)
    run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    run('shell', 'wm', 'dismiss-keyguard')
    if suite in ('pairing', 'camera-qr'):
        run('shell', 'pm', 'grant', app, 'android.permission.CAMERA')
    file = a.output / (suite + '.log')
    with file.open('w') as log:
        result = subprocess.run(adb + ['shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', suite,
                                      app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
                                stdout=log, stderr=subprocess.STDOUT, timeout=180)
    text = file.read_text()
    passed = result.returncode == 0 and 'INSTRUMENTATION_CODE: -1' in text and 'FAIL ' not in text
    results.append({'suite': suite, 'passed': passed})
    if not passed:
        print(text)
        raise SystemExit('Android pairing suite failed: ' + suite)
run('shell', 'am', 'force-stop', app)
report = {'status': 'PASS', 'api': api, 'suites': results,
          'apk_sha256': hashlib.sha256(a.apk.read_bytes()).hexdigest(),
          'test_apk_sha256': hashlib.sha256(a.test_apk.read_bytes()).hexdigest(),
          'scope': 'real ART/R8 QR, Camera2 lifecycle and Android NSD; no physical USB'}
(a.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report))
