#!/usr/bin/env python3
"""Real emulator layout/touch/keyboard checks for all three connection forms."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--test-apk', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
assert re.fullmatch(r'emulator-\d+', a.serial)
adb = [a.adb, '-s', a.serial]
def run(*args):
    return subprocess.check_output(adb + list(args), text=True, stderr=subprocess.STDOUT, timeout=90).strip()
assert run('shell', 'getprop', 'ro.kernel.qemu') == '1'
assert run('shell', 'getprop', 'sys.boot_completed') == '1'
for path in [a.apk, a.test_apk]:
    assert 'Success' in run('install', '-r', '--no-incremental', str(path.resolve()))
saved = {key: run('shell', 'settings', 'get', table, key) for table, key in [
    ('system', 'font_scale'), ('system', 'accelerometer_rotation'), ('system', 'user_rotation'),
    ('secure', 'show_ime_with_hard_keyboard')]}
geometry = {kind: run('shell', 'wm', kind) for kind in ['size', 'density']}
app = 'ru.levchatkin.rutokenbridge'
a.output.mkdir(parents=True, exist_ok=True)
results = []
try:
    run('shell', 'settings', 'put', 'system', 'accelerometer_rotation', '0')
    run('shell', 'settings', 'put', 'system', 'user_rotation', '0')
    run('shell', 'settings', 'put', 'secure', 'show_ime_with_hard_keyboard', '1')
    for width, height, font in [(320,640,1), (320,640,2), (360,720,1), (640,360,1), (640,360,2), (600,960,2)]:
        name = f'{width}x{height}-font{font}'
        out = a.output / name
        out.mkdir(exist_ok=True)
        run('shell', 'am', 'force-stop', app)
        run('shell', 'wm', 'density', '160')
        run('shell', 'wm', 'size', f'{width}x{height}')
        run('shell', 'settings', 'put', 'system', 'font_scale', str(font))
        run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        run('shell', 'wm', 'dismiss-keyguard')
        with (out / 'instrumentation.log').open('w') as log:
            result = subprocess.run(adb + ['shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'connection-layout',
                app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'], stdout=log, stderr=subprocess.STDOUT, timeout=180)
        text = (out / 'instrumentation.log').read_text()
        passed = result.returncode == 0 and 'INSTRUMENTATION_CODE: -1' in text and 'FAIL ' not in text
        results.append({'geometry': name, 'passed': passed})
        for mode in range(3):
            for edge in ['top','bottom']:
                file = f'connection-layout-{mode}-{edge}.png'
                subprocess.run(adb + ['pull', f'/sdcard/Android/data/{app}/files/{file}', str(out / file)], capture_output=True, timeout=30)
        (a.output / 'result.json').write_text(json.dumps({'apk_sha256':hashlib.sha256(a.apk.read_bytes()).hexdigest(),
            'test_apk_sha256':hashlib.sha256(a.test_apk.read_bytes()).hexdigest(), 'scenarios':results}, indent=2)+'\n')
        if not passed:
            print(text)
            raise SystemExit('Connection layout failed: '+name)
        print('PASS '+name, flush=True)
finally:
    run('shell', 'am', 'force-stop', app)
    for kind, value in geometry.items():
        override = re.search(r'Override \w+: (\S+)', value)
        run('shell', 'wm', kind, override[1] if override else 'reset')
    for key, value in saved.items():
        table = 'secure' if key == 'show_ime_with_hard_keyboard' else 'system'
        if value == 'null': run('shell', 'settings', 'delete', table, key)
        else: run('shell', 'settings', 'put', table, key, value)
