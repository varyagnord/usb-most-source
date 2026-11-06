#!/usr/bin/env python3
"""Standalone ART/JNI/TCP metadata checks on owned x86_64 emulators, without an APK.

This complements, never replaces, optimized APK/inventory/UI and actual OS tests.
Uses the project's already cached AGP D8 and already built Rust release library.
No USB, host mounts, physical device, production service or user data access.
"""
import argparse
import concurrent.futures
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--adb', type=Path, required=True)
    parser.add_argument('--serial', action='append', required=True)
    parser.add_argument('--sdk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if len(set(args.serial)) != len(args.serial) or not all(re.fullmatch(r'emulator-\d+', s) for s in args.serial):
        parser.error('Distinct owned emulator serials are required')
    output = args.output.resolve()
    if not output.is_relative_to(ROOT / 'build'):
        parser.error('Output must be an unused directory in build/')
    output.mkdir(parents=True, exist_ok=False)
    classes, dex = output / 'classes', output / 'dex'
    classes.mkdir()
    dex.mkdir()
    sources = sorted(p for tree in ('server', 'core', 'utils') for p in (ROOT / 'app/src/main/java/org/cgutman/usbip' / tree).rglob('*.java'))
    sources += [ROOT / p for p in ('app/src/main/java/org/cgutman/usbip/jni/NativeManagement.java',
                                 'tests/java/ManagementJniRegressionTests.java',
                                 'tests/java/ManagedServerRegressionTests.java')]
    builders = list((Path.home() / '.gradle/caches/modules-2/files-2.1/com.android.tools.build/builder/8.5.1').glob('*/builder-8.5.1.jar'))
    if len(builders) != 1:
        raise RuntimeError('Require the cached pinned AGP 8.5.1 builder used by this project')
    library = ROOT / 'app/build/generated/rustJniLibs/x86_64/libusblib.so'
    assert library.is_file(), 'Build optimized Rust JNI first'
    with (output / 'compile.log').open('x') as log:
        for command in (
            ['javac', '--release', '11', '-d', str(classes), *map(str, sources)],
            ['jar', '--create', '--file', str(output / 'input.jar'), '-C', str(classes), '.'],
            ['java', '-cp', str(builders[0]), 'com.android.tools.r8.D8', '--min-api', '26',
             '--lib', str(args.sdk / 'platforms/android-34/android.jar'), '--output', str(dex), str(output / 'input.jar')],
        ):
            subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=120)
    hashes = {'library_sha256': hashlib.sha256(library.read_bytes()).hexdigest(),
              'dex_sha256': hashlib.sha256((dex / 'classes.dex').read_bytes()).hexdigest(),
              'd8_sha256': hashlib.sha256(builders[0].read_bytes()).hexdigest()}
    remote = '/data/local/tmp/usb-most-managed-art-' + hashes['dex_sha256'][:12]

    def run(serial):
        adb = [str(args.adb), '-s', serial]

        def call(*parts):
            return subprocess.check_output(adb + list(parts), text=True, stderr=subprocess.STDOUT, timeout=90)

        record = dict(serial=serial, status='failed', **hashes)
        try:
            assert call('shell', 'getprop', 'ro.kernel.qemu').strip() == '1'
            assert call('shell', 'getprop', 'ro.product.cpu.abi').strip() == 'x86_64'
            record['api'] = int(call('shell', 'getprop', 'ro.build.version.sdk').strip())
            call('shell', 'mkdir', '-p', remote)
            call('push', str(library), remote + '/libusblib.so')
            call('push', str(dex / 'classes.dex'), remote + '/classes.dex')
            for name, expected in [('ManagementJniRegressionTests', 'PASS management JNI: 124 checks; 4000 concurrent close/stale lookups'),
                                   ('ManagedServerRegressionTests', 'PASS managed server + real JNI: 228 checks')]:
                result = subprocess.run(adb + ['shell', 'CLASSPATH=' + remote + '/classes.dex', 'dalvikvm', '-Xcheck:jni',
                    '-Djava.library.path=' + remote + ':/system/lib64:/vendor/lib64', name],
                    text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
                (output / (serial + '-' + name + '.log')).write_text(result.stdout)
                assert result.returncode == 0 and expected in result.stdout, name + ' failed; retained log'
            record['status'] = 'passed'
        except Exception as error:
            record['error'] = str(error)
        return record

    with concurrent.futures.ThreadPoolExecutor(max_workers=min(5, len(args.serial))) as workers:
        records = list(workers.map(run, args.serial))
    report = {'scope': 'Standalone ART + optimized Rust JNI + loopback TCP; synthetic leases. Not APK UI, physical USB or OS ejection evidence.',
              'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
              'worktree_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
              'results': records}
    (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report), flush=True)
    if not all(r['status'] == 'passed' for r in records):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
