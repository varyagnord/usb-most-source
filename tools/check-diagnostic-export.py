#!/usr/bin/env python3
"""Real taps, scroll and Android document export on an owned disposable emulator.

Clears only the fixture app's emulator data. Never accepts a physical device.
The report is checked for synthetic credentials and copied into local evidence.
"""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import time
import xml.etree.ElementTree as ET
from lab_android_active_soak import protect_android

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--process-death', action='store_true',
               help='Kill only the owned app while DocumentsUI holds the pending export')
p.add_argument('--cancel', action='store_true',
               help='Cancel the system export and verify no document and a usable fresh preview')
p.add_argument('--provider-apk', type=Path,
               help='Separate emulator-only DocumentsProvider/accessibility probe APK')
p.add_argument('--provider-fault', choices=['open', 'write'],
               help='Use the test provider and require an actual export error, then retry successfully')
a = p.parse_args()
if bool(a.provider_apk) != bool(a.provider_fault) or (a.cancel and a.provider_fault):
    p.error('Provider fault requires its APK and cannot be combined with cancellation')
assert re.fullmatch(r'emulator-\d+', a.serial)
protect_android(a.serial)
lockdir = Path(__file__).resolve().parents[1] / 'build/e2e/android-locks'
lockdir.mkdir(parents=True, exist_ok=True)
android_lock = (lockdir / (a.serial + '.lock')).open('a')
fcntl.flock(android_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
adb = [a.adb, '-s', a.serial]
app = 'ru.levchatkin.rutokenbridge'
a.output.mkdir(parents=True, exist_ok=False)
checks = []

def run(*args, check=True):
    result = subprocess.run(adb + list(args), capture_output=True, text=True, timeout=60)
    if check and result.returncode:
        raise RuntimeError('ADB operation failed: ' + args[0])
    return result.stdout.strip()

def verify(ok, message):
    assert ok, message
    checks.append(message)
    print('PASS', message, flush=True)

def nodes():
    for attempt in range(4):
        try:
            result = run('shell', 'uiautomator', 'dump', '/sdcard/usb-most-report-ui.xml')
            assert 'dumped to:' in result
            return list(ET.fromstring(run('shell', 'cat', '/sdcard/usb-most-report-ui.xml')).iter('node'))
        except (RuntimeError, AssertionError, ET.ParseError):
            if attempt == 3:
                raise
            time.sleep(.3)

def box(node):
    return list(map(int, re.findall(r'\d+', node.get('bounds', ''))))

def find(predicate, scroll=True, min_height=24):
    for attempt in range(22):
        tree = nodes()
        for node in tree:
            if not predicate(node):
                continue
            x, y, xx, yy = box(node)
            if xx > x and yy - y >= min_height:
                return node
        area = next((n for n in tree if n.get('scrollable') == 'true'), None)
        if scroll and area is not None:
            x, y, xx, yy = box(area)
            low, high = y + (yy-y)*3//4, y + (yy-y)//4
            start, end = (low, high) if attempt < 11 else (high, low)
            run('shell', 'input', 'swipe', str((x+xx)//2), str(start), str((x+xx)//2), '200')
        else:
            time.sleep(.2)
    raise AssertionError('Expected visible control not found')

def tap(node):
    x, y, xx, yy = box(node)
    run('shell', 'input', 'tap', str((x+xx)//2), str((y+yy)//2))
    time.sleep(.2)

def click(text):
    tap(find(lambda n: n.get('text', '').casefold() == text.casefold() and n.get('clickable') == 'true'))

def replace(node, value):
    tap(node)
    run('shell', 'input', 'keyevent', 'KEYCODE_MOVE_END')
    run('shell', 'input', 'keyevent', '--longpress', *(['KEYCODE_DEL'] * 64))
    run('shell', 'input', 'text', value)
    run('shell', 'input', 'keyevent', 'KEYCODE_BACK')

def setup_field(label, value):
    # Setup labels and their fields are adjacent native views, also at large font sizes.
    for _ in range(20):
        tree = nodes()
        for i, n in enumerate(tree):
            if n.get('text') != label:
                continue
            candidate = next((v for v in tree[i+1:] if v.get('class') == 'android.widget.EditText'), None)
            if candidate is not None and box(candidate)[3]-box(candidate)[1] >= 40:
                replace(candidate, value)
                return
        area = next(n for n in tree if n.get('scrollable') == 'true')
        x, y, xx, yy = box(area)
        run('shell', 'input', 'swipe', str((x+xx)//2), str(y+(yy-y)*3//4),
            str((x+xx)//2), str(y+(yy-y)//4), '200')
    raise AssertionError('Setup field not reachable: ' + label)

report = {'status': 'running', 'checks': checks,
          'process_death': a.process_death,
          'outcome': 'provider-failure-' + a.provider_fault if a.provider_fault else ('cancel' if a.cancel else 'save'),
          'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
          'apk_sha256': hashlib.sha256(a.apk.read_bytes()).hexdigest()}
probe_package = 'ru.levchatkin.usbmost.testdocs'
prior_accessibility = None
active_recorder = None


def provider_events():
    return run('shell', 'run-as', probe_package, 'cat', 'files/events.txt', check=False).splitlines()


def start_recorder(name):
    global active_recorder
    assert active_recorder is None
    pidfile = '/data/local/tmp/usb-most-events-' + str(time.time_ns()) + '.pid'
    path = a.output / (name + '-accessibility.txt')
    stream = path.open('w')
    script = 'echo $$ > ' + pidfile + '; exec uiautomator events'
    child = subprocess.Popen(adb + ['shell', 'sh -c ' + shlex.quote(script)],
                             stdout=stream, stderr=subprocess.STDOUT)
    active_recorder = dict(child=child, stream=stream, path=path, pidfile=pidfile)
    for _ in range(100):
        if 'EventType:' in path.read_text():
            pid = run('shell', 'cat', pidfile)
            assert pid.isdecimal()
            cmd = run('shell', 'cat', '/proc/' + pid + '/cmdline')
            assert recorder_command(cmd)
            active_recorder['pid'] = pid
            active_recorder['stat'] = run('shell', 'cat', '/proc/' + pid + '/stat').rsplit(') ', 1)[1].split()[19]
            return path
        assert child.poll() is None, 'UI event recorder exited'
        time.sleep(.1)
    raise AssertionError('UI event recorder did not become ready')


def recorder_command(cmd):
    # The framework Launcher sets argv[0] to "uiautomator" on some API levels.
    return cmd.split('\0')[0] == 'uiautomator' or (
        'com.android.commands.uiautomator.Launcher' in cmd and 'events' in cmd)


def stop_recorder():
    global active_recorder
    if active_recorder is None:
        return
    r = active_recorder
    if r['child'].poll() is None:
        if 'pid' not in r:
            pid = run('shell', 'cat', r['pidfile'])
            assert pid.isdecimal()
            r['pid'] = pid
        cmd = run('shell', 'cat', '/proc/' + r['pid'] + '/cmdline', check=False)
        if not cmd:
            r['child'].wait(timeout=10)
            r['stream'].close()
            active_recorder = None
            return
        assert recorder_command(cmd)
        if 'stat' in r:
            assert run('shell', 'cat', '/proc/' + r['pid'] + '/stat').rsplit(') ', 1)[1].split()[19] == r['stat']
        run('shell', 'kill', '-TERM', r['pid'])
    r['child'].wait(timeout=10)
    r['stream'].close()
    active_recorder = None


def await_toast(path, text):
    for _ in range(100):
        lines = path.read_text().splitlines()
        if any('TYPE_NOTIFICATION_STATE_CHANGED' in line and text in line for line in lines):
            break
        time.sleep(.1)
    stop_recorder()
    lines = path.read_text().splitlines()
    return [line for line in lines if 'TYPE_NOTIFICATION_STATE_CHANGED' in line]


try:
    verify(run('shell', 'getprop', 'ro.kernel.qemu') == '1', 'owned emulator')
    report['api'] = int(run('shell', 'getprop', 'ro.build.version.sdk'))
    if a.provider_fault:
        report['provider_apk_sha256'] = hashlib.sha256(a.provider_apk.read_bytes()).hexdigest()
        if run('shell', 'pm', 'path', probe_package, check=False).startswith('package:'):
            verify('Success' in run('uninstall', probe_package), 'previous disposable provider removed')
        verify('Success' in run('install', '-r', str(a.provider_apk.resolve())), 'separate fault provider installed')
        verify('Success' in run('shell', 'pm', 'clear', probe_package), 'only test provider data reset')
        prior_accessibility = {key: run('shell', 'settings', 'get', 'secure', key)
                               for key in ('enabled_accessibility_services', 'accessibility_enabled')}
    verify('Success' in run('install', '-r', str(a.apk.resolve())), 'exact candidate installed')
    verify('Success' in run('shell', 'pm', 'clear', app), 'isolated fixture data reset')
    run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    run('shell', 'wm', 'dismiss-keyguard')
    run('shell', 'input', 'keyevent', 'KEYCODE_HOME')
    nodes()  # Settle the old DocumentsUI/activity task after clearing fixture data.
    run('shell', 'am', 'start', '-W', '-n', app + '/org.cgutman.usbip.config.UsbIpConfig')
    click('Настроить подключение')
    click('Через SSH')
    setup_field('Адрес компьютера', '127.0.0.1')
    setup_field('Имя пользователя', 'report-fixture-user')
    setup_field('Пароль SSH', 'report-fixture-password')
    click('Сохранить компьютер')
    click('Настройки')
    click('Проверить подключение')
    click('Сохранить отчёт')
    tree = nodes()
    preview = '\n'.join(n.get('text', '') for n in tree)
    verify('Отчёт для поддержки' in preview, 'preview precedes system export')
    preview_report = max((n.get('text', '') for n in tree
                          if n.get('text', '').startswith('USB Мост')), key=len)
    for secret in ('127.0.0.1', 'report-fixture-user', 'report-fixture-password'):
        verify(secret not in preview, 'preview excludes synthetic private value: ' + secret.split('-')[-1])
    click('Отмена')
    click('Сохранить отчёт')
    # Save the exact pending report, not a report regenerated after restoration.
    preview_report = max((n.get('text', '') for n in nodes()
                          if n.get('text', '').startswith('USB Мост')), key=len)
    click('Сохранить')
    filename = ('fail-' + a.provider_fault + '-' if a.provider_fault else 'usb-most-check-e2e-') + str(time.time_ns()) + '.txt'
    replace(find(lambda n: n.get('class') == 'android.widget.EditText', scroll=False), filename)
    tree = nodes()
    # The emulator's DocumentsUI uses English labels; a Russian image is accepted too.
    save = next((n for n in tree if n.get('resource-id', '').endswith('/button1')
                 and n.get('text', '').lower() in ('save', 'сохранить') and n.get('enabled') == 'true'), None)
    if save is None:
        root_button = next((n for n in tree if n.get('content-desc') in
                            ('Show roots', 'Показать корневые каталоги', 'Open navigation drawer')), None)
        if root_button is not None:
            tap(root_button)
        tap(find(lambda n: n.get('text') in ('Downloads', 'Загрузки'), scroll=False))
        save = find(lambda n: n.get('text', '').lower() in ('save', 'сохранить')
                    and n.get('enabled') == 'true', scroll=False)
    if a.provider_fault:
        tree = nodes()
        root_button = next((n for n in tree if n.get('content-desc') in
                            ('Show roots', 'Показать корневые каталоги', 'Open navigation drawer')), None)
        if root_button is not None:
            tap(root_button)
        tap(find(lambda n: n.get('text') == 'USB Most tests'
                 and n.get('resource-id') == 'android:id/title', scroll=False, min_height=1))
        replace(find(lambda n: n.get('class') == 'android.widget.EditText', scroll=False), filename)
        save = find(lambda n: n.get('resource-id', '').endswith('/button1')
                    and n.get('text', '').lower() in ('save', 'сохранить')
                    and n.get('enabled') == 'true', scroll=False)
    if a.process_death:
        verify(save.get('package', '').endswith('documentsui'), 'system document provider owns foreground')
        before = run('shell', 'pidof', app)
        verify(bool(re.fullmatch(r'\d+', before)), 'single owned application PID')
        verify(run('shell', 'su', '0', 'cat', '/proc/' + before + '/cmdline').split('\0')[0] == app,
               'exact application process verified before kill')
        # Userdebug emulator only. Force-stop would discard the pending result
        # and would not model Android reclaiming the background process.
        run('shell', 'su', '0', 'kill', '-9', before)
        for _ in range(30):
            if not run('shell', 'pidof', app, check=False):
                break
            time.sleep(.1)
        verify(not run('shell', 'pidof', app, check=False), 'actual process died while SAF remained open')
        save = find(lambda n: n.get('text', '').lower() in ('save', 'сохранить')
                    and n.get('enabled') == 'true', scroll=False)
        verify(save.get('package', '').endswith('documentsui'), 'system export survives client process death')
    if a.cancel:
        run('shell', 'input', 'keyevent', 'KEYCODE_BACK')
    else:
        if a.provider_fault:
            error_log = start_recorder('provider-failure')
        tap(save)
    if a.provider_fault:
        notifications = await_toast(error_log, 'Не удалось сохранить отчёт')
        events = provider_events()
        (a.output/'provider-events-before-retry.txt').write_text('\n'.join(events)+'\n')
        verify('fail-' + a.provider_fault in events, 'actual provider exercised the selected failure path')
        verify(any('Не удалось сохранить отчёт' in n for n in notifications), 'actual app announced export failure')
        verify(not any('Отчёт сохранён' in n for n in notifications), 'failed provider never produced a success message')
    find(lambda n: n.get('resource-id', '').endswith('/diagnosis_refresh'))
    if a.process_death:
        after = run('shell', 'pidof', app)
        verify(bool(re.fullmatch(r'\d+', after)) and before != after,
               'SAF result restored a new application process')
    if a.provider_fault:
        tap(find(lambda n: n.get('resource-id', '').endswith('/diagnosis_refresh')))
        click('Сохранить отчёт')
        preview = '\n'.join(n.get('text', '') for n in nodes())
        verify('Отчёт для поддержки' in preview, 'fresh preview is usable after real provider failure')
        verify(all(value not in preview for value in ('127.0.0.1', 'report-fixture-user', 'report-fixture-password')),
               'retry preview excludes private fixture values')
        approved_retry = max((n.get('text', '') for n in nodes()
                              if n.get('text', '').startswith('USB Мост')), key=len)
        click('Сохранить')
        tree = nodes()
        root_button = next((n for n in tree if n.get('content-desc') in
                            ('Show roots', 'Показать корневые каталоги', 'Open navigation drawer')), None)
        if root_button is not None:
            tap(root_button)
        tap(find(lambda n: n.get('text') in ('Downloads', 'Загрузки')
                 and n.get('resource-id') == 'android:id/title', scroll=False, min_height=1))
        good_name = 'write-ok-' + str(time.time_ns()) + '.txt'
        replace(find(lambda n: n.get('class') == 'android.widget.EditText', scroll=False), good_name)
        save = find(lambda n: n.get('resource-id', '').endswith('/button1')
                    and n.get('text', '').lower() in ('save', 'сохранить')
                    and n.get('enabled') == 'true', scroll=False)
        success_log = start_recorder('provider-retry')
        tap(save)
        notifications = await_toast(success_log, 'Отчёт сохранён')
        events = provider_events()
        verify(sum('Отчёт сохранён' in n for n in notifications) == 1
               and not any('Не удалось сохранить отчёт' in n for n in notifications),
               'successful retry produces exactly one new success and no error')
        path = '/sdcard/Download/' + good_name
        actual = subprocess.run(adb + ['exec-out', 'cat', path],
                                capture_output=True, check=True, timeout=60).stdout
        verify(actual == approved_retry.encode('utf-8'), 'retry saved byte-exact newly approved report')
        (a.output/'provider-events.txt').write_text('\n'.join(events)+'\n')
        (a.output/'retry-report.txt').write_bytes(actual)
        report['retry_document_sha256'] = hashlib.sha256(actual).hexdigest()
        report['retry_provider'] = 'system Downloads after failing test provider'
        find(lambda n: n.get('resource-id', '').endswith('/diagnosis_refresh'))
    elif a.cancel:
        for directory in ('/sdcard/Download', '/sdcard/Documents'):
            probe = subprocess.run(adb + ['shell', 'test', '-e', directory + '/' + filename],
                                   capture_output=True, timeout=60)
            verify(probe.returncode == 1, 'cancel created no document in ' + directory)
        tap(find(lambda n: n.get('resource-id', '').endswith('/diagnosis_refresh')))
        click('Сохранить отчёт')
        preview = '\n'.join(n.get('text', '') for n in nodes())
        verify('Отчёт для поддержки' in preview, 'fresh preview remains available after canceled result')
        verify(all(value not in preview for value in ('127.0.0.1', 'report-fixture-user', 'report-fixture-password')),
               'fresh preview after cancel excludes all private fixture values')
        click('Отмена')
        find(lambda n: n.get('resource-id', '').endswith('/diagnosis_refresh'))
        report['canceled_document_filename'] = filename
    else:
        path = None
        for _ in range(30):
            paths = run('shell', 'find', '/sdcard/Download', '/sdcard/Documents', '-maxdepth', '1',
                        '-name', filename, check=False).splitlines()
            path = next((x for x in paths if x in ('/sdcard/Download/'+filename, '/sdcard/Documents/'+filename)), None)
            if path:
                break
            time.sleep(.2)
        verify(path is not None, 'SAF created expected document in emulator shared storage')
        # exec-out preserves terminal newlines; the ordinary text helper strips
        # them and cannot prove byte-for-byte equality with the approved preview.
        actual = subprocess.run(adb + ['exec-out', 'cat', path], capture_output=True,
                                check=True, timeout=60).stdout
        value = actual.decode('utf-8')
        verify(value.startswith('USB Мост') and 'USB: найдено' in value, 'actual saved document is a complete report')
        verify(actual == preview_report.encode('utf-8'), 'saved bytes match the report approved before export')
        verify(all(s not in value for s in ('127.0.0.1', 'report-fixture-user', 'report-fixture-password')),
               'actual file excludes host user and password')
        (a.output/'exported-report.txt').write_bytes(actual)
        report['document_filename'] = filename
        report['document_sha256'] = hashlib.sha256(actual).hexdigest()
    report['status'] = 'passed'
except BaseException as error:
    report['status'] = 'failed'
    report['error'] = str(error)
    raise
finally:
    stop_recorder()
    run('shell', 'am', 'force-stop', app, check=False)
    if prior_accessibility is not None:
        report['original_accessibility_settings_restored'] = all(
            run('shell', 'settings', 'get', 'secure', key) == value
            for key, value in prior_accessibility.items())
        assert report['original_accessibility_settings_restored']
        run('shell', 'am', 'force-stop', probe_package, check=False)
    (a.output/'result.json').write_text(json.dumps(report, indent=2)+'\n')
