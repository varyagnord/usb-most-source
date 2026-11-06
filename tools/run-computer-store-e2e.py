#!/usr/bin/env python3
"""Actual optimized Android storage adapter, only on a disposable emulator."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--adb', required=True)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--apk', type=Path, required=True)
    parser.add_argument('--test-apk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--scenario', choices=['secure', 'legacy', 'corrupt', 'missing-key',
                                             'mismatch', 'empty-v2', 'cleanup', 'durable-seed',
                                             'durable-resume', 'forgotten-resume'], action='append')
    args = parser.parse_args()
    if not re.fullmatch(r'emulator-\d+', args.serial):
        parser.error('Only owned emulator serials are accepted')
    adb = [args.adb, '-s', args.serial]

    def run(*command, timeout=90):
        return subprocess.check_output(adb + list(command), text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()

    assert run('shell', 'getprop', 'ro.kernel.qemu') == '1'
    assert run('shell', 'getprop', 'sys.boot_completed') == '1'
    args.output.mkdir(parents=True, exist_ok=False)
    report = {'status': 'running', 'serial': args.serial,
              'api': int(run('shell', 'getprop', 'ro.build.version.sdk')),
              'apk_sha256': hashlib.sha256(args.apk.read_bytes()).hexdigest(),
              'test_apk_sha256': hashlib.sha256(args.test_apk.read_bytes()).hexdigest(),
              'scenarios': [],
              'scope': 'Real ART/JNI, Android Keystore AES-GCM, encrypted file migration, '
                       'repository/service leases and storage failure fixtures. '
                       'Does not test the new UI integration, physical USB or real power failure.'}
    app = 'ru.levchatkin.rutokenbridge'
    try:
        for apk in (args.apk, args.test_apk):
            assert 'Success' in run('install', '-r', str(apk.resolve()))
        scenarios = args.scenario or ['secure', 'legacy', 'corrupt', 'missing-key',
                                      'mismatch', 'empty-v2', 'cleanup', 'durable-seed',
                                      'durable-resume', 'forgotten-resume']
        for scenario in scenarios:
            run('shell', 'am', 'force-stop', app)
            if scenario not in ('durable-resume', 'forgotten-resume'):
                assert 'Success' in run('shell', 'pm', 'clear', app)
            run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
            run('shell', 'wm', 'dismiss-keyguard')
            start = time.monotonic()
            result = run('shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'computer-store',
                         '-e', 'scenario', scenario,
                         app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation', timeout=150)
            (args.output / (scenario + '.log')).write_text(result + '\n')
            assert 'INSTRUMENTATION_CODE: -1' in result and 'FAIL ' not in result, result
            count = re.search(r'PASS optimized release API \d+: (\d+) checks', result)
            assert count, result
            report['scenarios'].append({'name': scenario, 'status': 'passed',
                                        'checks': int(count[1]), 'elapsed_seconds': time.monotonic() - start})
            (args.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
            print(f'PASS API {report["api"]}: {scenario}, {count[1]} checks', flush=True)
        report['status'] = 'passed'
    except BaseException as error:
        report['status'] = 'failed'
        report['error'] = str(error)
        raise
    finally:
        try:
            run('shell', 'am', 'force-stop', app)
        finally:
            (args.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
