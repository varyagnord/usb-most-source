#!/usr/bin/env python3
"""Minimal framework Activity/Dialog recreation, isolated from the product.

Only owned emulators. No USB, Rust, network, or product APK. Retains failures,
window dumps and test artifacts. Expected OS limitations still exit nonzero.
The supplied keystore must be the internal test key with password 'android'.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--sdk', type=Path, required=True)
p.add_argument('--serial', required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--builder-jar', type=Path, required=True, help='Pinned AGP 8.5.1 builder JAR containing D8')
p.add_argument('--test-keystore', type=Path, required=True)
p.add_argument('--variant', choices=['plain', 'dialog', 'keyboard'], default='dialog')
p.add_argument('--transition', choices=['recreate', 'font', 'density', 'resize', 'rotate'], default='recreate')
a = p.parse_args()
assert re.fullmatch(r'emulator-\d+', a.serial)
base = a.output.resolve()
assert base.is_relative_to(ROOT / 'build')
base.mkdir(parents=True, exist_ok=False)
adb = [str(a.sdk / 'platform-tools/adb'), '-s', a.serial]
package = 'org.usbmost.lab.recreate'
def call(*args, timeout=45):
    return subprocess.check_output(adb + list(args), text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()
assert call('shell', 'getprop', 'ro.kernel.qemu') == '1'
baseline = {k: call('shell', 'wm', k) for k in ['size', 'density']}
font = call('shell', 'settings', 'get', 'system', 'font_scale')
report = {'status': 'running', 'api': int(call('shell', 'getprop', 'ro.build.version.sdk')),
          'variant': a.variant, 'transition': a.transition, 'baseline': baseline, 'font': font,
          'scope': 'Standalone Android framework only; no product code, USB or Rust'}
try:
    source = ROOT / 'tests/android-window-fixture'
    bt = a.sdk / 'build-tools/34.0.0'
    android = a.sdk / 'platforms/android-34/android.jar'
    for folder in ['classes', 'dex']:
        (base / folder).mkdir()
    def run(args):
        subprocess.run(list(map(str, args)), check=True, timeout=120)
    run(['javac', '--release', '11', '-cp', android, '-d', base / 'classes', *sorted((source / 'src').rglob('*.java'))])
    run(['jar', '--create', '--file', base / 'input.jar', '-C', base / 'classes', '.'])
    run(['java', '-cp', a.builder_jar, 'com.android.tools.r8.D8', '--min-api', '26', '--lib', android,
         '--output', base / 'dex', base / 'input.jar'])
    run([bt / 'aapt2', 'link', '--manifest', source / 'AndroidManifest.xml', '-I', android, '-o', base / 'unsigned.apk'])
    with zipfile.ZipFile(base / 'unsigned.apk', 'a', zipfile.ZIP_DEFLATED) as archive:
        archive.write(base / 'dex/classes.dex', 'classes.dex')
    run([bt / 'apksigner', 'sign', '--ks', a.test_keystore, '--ks-key-alias', 'test', '--ks-pass', 'pass:android',
         '--key-pass', 'pass:android', '--out', base / 'fixture.apk', base / 'unsigned.apk'])
    report['apk_sha256'] = hashlib.sha256((base / 'fixture.apk').read_bytes()).hexdigest()
    call('shell', 'am', 'force-stop', package)
    assert 'Success' in call('install', '-r', '-t', str(base / 'fixture.apk'))
    call('shell', 'wm', 'size', '360x800')
    call('shell', 'wm', 'density', '160')
    call('shell', 'settings', 'put', 'system', 'font_scale', '1.0')
    result = call('shell', 'am', 'instrument', '-w', '-r', '-e', 'variant', a.variant,
                  '-e', 'transition', a.transition, package + '/.Runner')
    (base / 'instrumentation.log').write_text(result + '\n')
    call('pull', '/sdcard/Android/data/' + package + '/files', str(base / 'evidence'))
    assert 'INSTRUMENTATION_CODE: -1' in result and 'FAIL ' not in result, 'Framework reproduction failed; see retained log'
    report['status'] = 'passed'
except BaseException as error:
    report.update(status='failed', error=str(error))
    raise
finally:
    try:
        call('shell', 'am', 'force-stop', package)
        for kind, value in baseline.items():
            match = re.search(r'Override (?:size|density): (\S+)', value)
            call('shell', 'wm', kind, match[1] if match else 'reset')
        if font == 'null':
            call('shell', 'settings', 'delete', 'system', 'font_scale')
        else:
            call('shell', 'settings', 'put', 'system', 'font_scale', font)
    finally:
        (base / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report))
