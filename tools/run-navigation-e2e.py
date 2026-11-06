#!/usr/bin/env python3
"""On owned emulators only: optimized Activity flow, state restoration and responsive screenshots."""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import subprocess
from lab_android_active_soak import protect_android

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--test-apk', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--matrix', action='store_true')
p.add_argument('--suite', choices=('navigation', 'privacy'), default='navigation')
a = p.parse_args()
assert re.fullmatch(r'emulator-\d+', a.serial)
protect_android(a.serial)
lockdir = Path(__file__).resolve().parents[1] / 'build/e2e/android-locks'
lockdir.mkdir(parents=True, exist_ok=True)
android_lock = (lockdir / (a.serial + '.lock')).open('a')
fcntl.flock(android_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
adb = [a.adb, '-s', a.serial]
def run(*args, timeout=90):
    return subprocess.check_output(adb + list(args), text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()
assert run('shell', 'getprop', 'ro.kernel.qemu') == '1'
assert run('shell', 'getprop', 'sys.boot_completed') == '1'
app = 'ru.levchatkin.rutokenbridge'
a.output.mkdir(parents=True, exist_ok=True)
for apk in [a.apk, a.test_apk]:
    assert 'Success' in run('install', '-r', str(apk.resolve()))
original = {k: run('shell', 'settings', 'get', 'system', k) for k in
            ['font_scale', 'accelerometer_rotation', 'user_rotation']}
hidden = run('shell', 'settings', 'get', 'global', 'hidden_api_policy')
geometry = {k: run('shell', 'wm', k) for k in ['size', 'density']}
night = re.search(r'Night mode: (\w+)', run('shell', 'cmd', 'uimode', 'night'), re.I)[1].lower()
results = []
report = {'status': 'running', 'api': int(run('shell', 'getprop', 'ro.build.version.sdk')),
          'suite': a.suite,
          'apk_sha256': hashlib.sha256(a.apk.read_bytes()).hexdigest(),
          'test_apk_sha256': hashlib.sha256(a.test_apk.read_bytes()).hexdigest(),
          'scenarios': results}
try:
    # ART reads this policy when the target process starts. Changing it only
    # inside instrumentation is too late for framework USB fixture constructors.
    # This is an owned emulator/test APK boundary, never a production requirement.
    if report['api'] >= 28:
        run('shell', 'settings', 'put', 'global', 'hidden_api_policy', '1')
    run('shell', 'settings', 'put', 'system', 'accelerometer_rotation', '0')
    run('shell', 'settings', 'put', 'system', 'user_rotation', '0')
    cases = [(360,800,1,'no')]
    if a.matrix:
        cases = [(320,640,1,'no'), (320,640,2,'yes'), (360,800,1,'no'),
                 (412,915,2,'no'), (640,360,1,'no'), (640,360,2,'no'),
                 (600,960,2,'no'), (800,1280,1,'no')]
    for width, height, font, dark in cases:
        name = f'{width}x{height}-font{font}-night{dark}'
        out = a.output / name
        out.mkdir(exist_ok=True)
        run('shell', 'am', 'force-stop', app)
        run('shell', 'wm', 'density', '160')
        run('shell', 'wm', 'size', f'{width}x{height}')
        run('shell', 'settings', 'put', 'system', 'font_scale', str(font))
        run('shell', 'cmd', 'uimode', 'night', dark)
        run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        run('shell', 'wm', 'dismiss-keyguard')
        result = run('shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', a.suite,
                     app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation', timeout=180)
        (out / 'instrumentation.log').write_text(result + '\n')
        assert 'INSTRUMENTATION_CODE: -1' in result and 'FAIL ' not in result, result
        run('pull', '/sdcard/Android/data/' + app + '/files/' + a.suite + '-proof/.', str(out))
        if a.suite == 'privacy':
            count = re.search(r'PASS optimized release API (\d+): (\d+) checks', result)
            assert count and int(count[1]) == report['api'], 'missing exact privacy assertion count'
            scenario = {'checks': int(count[2]), 'screenshots': ['top.png', 'bottom.png']}
            assert all((out / name).is_file() for name in scenario['screenshots'])
            scenario['captures'] = {
                stage: json.loads((out / (stage + '.json')).read_text())
                for stage in ('top', 'bottom')
            }
            assert all(item['api'] == report['api'] and item['capture'] in
                       ('display', 'activity_surface_pixelcopy')
                       for item in scenario['captures'].values())
        else:
            scenario = json.loads((out / 'result.json').read_text())
        results.append({'viewport_dp': [width, height], 'font_scale': font, 'system_night': dark,
                        'result': scenario})
        (a.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print('PASS ' + a.suite + ', recreation and geometry: ' + name, flush=True)
    report['status'] = 'passed'
except BaseException as error:
    report['status'] = 'failed'
    report['error'] = str(error)
    raise
finally:
    run('shell', 'am', 'force-stop', app)
    if report['api'] >= 28:
        if hidden == 'null': run('shell', 'settings', 'delete', 'global', 'hidden_api_policy')
        else: run('shell', 'settings', 'put', 'global', 'hidden_api_policy', hidden)
    for key, value in geometry.items():
        override = re.search(r'Override \w+: (\S+)', value)
        run('shell', 'wm', key, override[1] if override else 'reset')
    run('shell', 'cmd', 'uimode', 'night', night)
    for key, value in original.items():
        if value == 'null': run('shell', 'settings', 'delete', 'system', key)
        else: run('shell', 'settings', 'put', 'system', key, value)
    (a.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
