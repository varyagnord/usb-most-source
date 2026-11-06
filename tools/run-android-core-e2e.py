#!/usr/bin/env python3
"""Fresh optimized core/lifecycle or layout checks on an owned emulator only.

The full upgrade, host TCP and SAF path remains tools/run-release-e2e.py.
USB hardware here is synthetic. No real device or user data is accessed.
"""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import subprocess
from lab_android_active_soak import protect_android


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--adb', required=True)
    parser.add_argument('--serial', required=True)
    parser.add_argument('--apk', type=Path, required=True)
    parser.add_argument('--test-apk', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--suite', choices=['core', 'layout', 'connection-layout', 'key-import', 'usb-rules', 'managed-ejection', 'quick-actions', 'quick-actions-ui', 'quick-actions-keyguard', 'licenses', 'access', 'access-endpoints'], default='core')
    parser.add_argument('--usb-transition', choices=['rotate', 'font', 'density', 'resize', 'split'], default='rotate')
    args = parser.parse_args()
    if not re.fullmatch(r'emulator-\d+', args.serial):
        parser.error('Only owned emulator serials are accepted')
    protect_android(args.serial)
    lockdir = Path(__file__).resolve().parents[1] / 'build/e2e/android-locks'
    lockdir.mkdir(parents=True, exist_ok=True)
    android_lock = (lockdir / (args.serial + '.lock')).open('a')
    fcntl.flock(android_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    adb = [args.adb, '-s', args.serial]

    def run(*parts, timeout=90):
        return subprocess.check_output(adb + list(parts), text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()

    assert run('shell', 'getprop', 'ro.kernel.qemu') == '1'
    assert run('shell', 'getprop', 'sys.boot_completed') == '1'
    api = int(run('shell', 'getprop', 'ro.build.version.sdk'))
    if args.usb_transition != 'rotate' and args.suite != 'usb-rules':
        parser.error('--usb-transition requires --suite usb-rules')
    if args.usb_transition == 'split' and api != 26:
        parser.error('This split-screen fixture uses the API26 docked-stack command')
    args.output.mkdir(parents=True, exist_ok=False)
    app = 'ru.levchatkin.rutokenbridge'
    policy = run('shell', 'settings', 'get', 'global', 'hidden_api_policy')
    configuration = None
    if args.usb_transition not in ('rotate', 'split'):
        configuration = {kind: run('shell', 'wm', kind) for kind in ('size', 'density')}
        configuration['font'] = run('shell', 'settings', 'get', 'system', 'font_scale')
    report = {'status': 'running', 'api': api, 'suite': args.suite,
              'apk_sha256': hashlib.sha256(args.apk.read_bytes()).hexdigest(),
              'test_apk_sha256': hashlib.sha256(args.test_apk.read_bytes()).hexdigest(),
              'scope': ('Optimized Android/JNI, synthetic USB inventory, real service/TCP/lifecycle. '
                        'No physical USB, upgrade or host-side SAF claim.' if args.suite == 'core' else
                        'Optimized Android/JNI/inventory and ejection UI with synthetic USB and OS replies; no physical USB or actual OS ejection.' if args.suite == 'managed-ejection' else
                        'Optimized Android/JNI/Keystore and device-menu UI with injected USB inventory; no physical USB or network.' if args.suite == 'usb-rules' else
                        'SystemUI tile, immutable PendingIntent and actual foreground Activity/service, with synthetic USB; no physical USB or remote receiver.' if args.suite == 'quick-actions' else
                        'Actual saved-identity/clipboard/forget UI, encrypted repository and foreground TCP teardown, then fresh-process persistence. No USB backend or remote revocation claim.' if args.suite == 'access' else
                        'Actual saved endpoint editing, pin/credential isolation, explicit new claim and fresh-process persistence. Loopback observer only; no USB or remote authentication claim.' if args.suite == 'access-endpoints' else
                        'Optimized Android UI, file-provider I/O/lifecycle or layout as selected by suite. No real USB or network claim.')}
    try:
        run('shell', 'am', 'force-stop', app)
        if configuration is not None:
            run('shell', 'wm', 'size', '360x800')
            run('shell', 'wm', 'density', '160')
            run('shell', 'settings', 'put', 'system', 'font_scale', '1.0')
            report['configuration_baseline'] = configuration
        for apk in (args.apk, args.test_apk):
            assert 'Success' in run('install', '-r', str(apk.resolve()))
        assert 'Success' in run('shell', 'pm', 'clear', app)
        if args.suite == 'quick-actions':
            denied = subprocess.run(adb + ['shell', 'am', 'start', '-n',
                app + '/org.cgutman.usbip.config.QuickActionActivity'],
                text=True, capture_output=True, timeout=30)
            refusal = denied.stdout + denied.stderr
            (args.output / 'private-entry-refusal.txt').write_text(refusal)
            assert 'Permission Denial' in refusal or 'SecurityException' in refusal, 'Private entry accepted an external caller'
            report['external_private_entry_refused'] = True
        run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        run('shell', 'wm', 'dismiss-keyguard')
        if api >= 28:
            run('shell', 'settings', 'put', 'global', 'hidden_api_policy', '1')
        command = ['shell', 'am', 'instrument', '-w', '-r']
        if args.suite != 'core':
            command += ['-e', 'suite', args.suite]
        if args.suite == 'usb-rules':
            command += ['-e', 'usb-transition', args.usb_transition]
            report['usb_transition'] = args.usb_transition
        log_path = args.output / 'instrumentation.log'
        with log_path.open('w') as log:
            subprocess.run(adb + command + [app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
                           stdout=log, stderr=subprocess.STDOUT, timeout=420, check=True)
        result = log_path.read_text()
        assert 'INSTRUMENTATION_CODE: -1' in result and 'FAIL ' not in result, 'Instrumentation failed; inspect retained log'
        count = re.search(r'PASS optimized release API \d+: (\d+) checks', result)
        assert count, 'Missing completed check count'
        report['checks'] = int(count[1])
        if args.suite == 'managed-ejection':
            run('pull', f'/sdcard/Android/data/{app}/files/managed-ejection-proof', str(args.output / 'screenshots'))
        if args.suite == 'quick-actions-ui':
            run('pull', f'/sdcard/Android/data/{app}/files/quick-actions-proof', str(args.output / 'screenshots'))
        if args.suite in ('access', 'access-endpoints'):
            run('pull', f'/sdcard/Android/data/{app}/files/access-proof', str(args.output / 'screenshots'))
        if args.suite in ('usb-rules', 'quick-actions', 'access', 'access-endpoints'):
            if args.suite == 'usb-rules':
                run('pull', f'/sdcard/Android/data/{app}/files/usb-rules-proof', str(args.output / 'screenshots'))
            report['checks_by_phase'] = {'ui_access' if args.suite in ('access', 'access-endpoints') else 'ui_inventory': report['checks']}
            run('shell', 'am', 'force-stop', app)
            with (args.output / 'resume.log').open('w') as log:
                subprocess.run(adb + ['shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', args.suite + '-resume',
                    app + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
                    stdout=log, stderr=subprocess.STDOUT, timeout=180, check=True)
            resumed = (args.output / 'resume.log').read_text()
            assert 'INSTRUMENTATION_CODE: -1' in resumed and 'FAIL ' not in resumed, 'Process recovery failed; inspect resume.log'
            count = re.search(r'PASS optimized release API \d+: (\d+) checks', resumed)
            assert count, 'Missing restarted rule check count'
            report['checks_by_phase']['new_process'] = int(count[1])
            report['checks'] += int(count[1])
            report['actual_process_restart'] = True
        report['status'] = 'passed'
    except BaseException as error:
        report['status'] = 'failed'
        report['error'] = str(error)
        raise
    finally:
        try:
            if args.suite == 'quick-actions-keyguard':
                marker = f'/sdcard/Android/data/{app}/files/quick-actions-keyguard-owned.json'
                retained = run('shell', 'test', '-f', marker, '&&', 'cat', marker, '||', 'true')
                if retained:
                    baseline = json.loads(retained)
                    assert baseline.get('disabled') in ('true', 'false'), 'Unknown keyguard cleanup marker'
                    cleared = run('shell', 'locksettings', 'clear', '--old', '482619')
                    assert 'cleared' in cleared.lower(), 'Owned emulator fixture credential cleanup failed'
                    run('shell', 'locksettings', 'set-disabled', baseline['disabled'])
                    run('shell', 'rm', marker)
                    report['keyguard_fallback_cleanup'] = True
            run('shell', 'am', 'force-stop', app)
            if configuration is not None:
                for kind in ('size', 'density'):
                    override = re.search(r'Override (?:size|density): (\S+)', configuration[kind])
                    run('shell', 'wm', kind, override[1] if override else 'reset')
                if configuration['font'] == 'null':
                    run('shell', 'settings', 'delete', 'system', 'font_scale')
                else:
                    run('shell', 'settings', 'put', 'system', 'font_scale', configuration['font'])
            if api >= 28:
                if policy in ('', 'null'):
                    run('shell', 'settings', 'delete', 'global', 'hidden_api_policy')
                else:
                    run('shell', 'settings', 'put', 'global', 'hidden_api_policy', str(int(policy)))
        except BaseException as error:
            report['status'] = 'failed'
            report['cleanup_error'] = str(error)
            raise
        finally:
            (args.output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report), flush=True)


if __name__ == '__main__':
    main()
