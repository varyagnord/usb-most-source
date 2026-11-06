#!/usr/bin/env python3
"""Black-box checks of a signed, optimized APK on a disposable emulator.

The baseline and candidate must use the same TEST key. This runner resets only
this app on the explicitly selected emulator. Never use a production device.
"""
import argparse
import hashlib
import json
import pathlib
import re
import socket
import struct
import shlex
import ipaddress
import subprocess
import time
import xml.etree.ElementTree as ET

APP = 'ru.levchatkin.rutokenbridge'
ACTIVITY = APP + '/org.cgutman.usbip.config.UsbIpConfig'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--adb', required=True)
parser.add_argument('--serial', required=True)
parser.add_argument('--apk', type=pathlib.Path, required=True)
parser.add_argument('--baseline', type=pathlib.Path, required=True)
parser.add_argument('--test-apk', type=pathlib.Path, required=True)
parser.add_argument('--output', type=pathlib.Path, required=True)
parser.add_argument('--diagnostic', action='store_true')
args = parser.parse_args()
if not re.fullmatch(r'emulator-\d+', args.serial):
    parser.error('Only an explicit emulator serial is accepted')
args.output.mkdir(parents=True, exist_ok=True)
checks = []


def adb(*parts, timeout=60, must=True):
    result = subprocess.run([args.adb, '-s', args.serial, *parts],
                            capture_output=True, text=True, timeout=timeout)
    if must and result.returncode:
        raise RuntimeError(f'ADB {parts}: {result.stdout} {result.stderr}')
    return result.stdout.strip()


def check(condition, label):
    if not condition:
        raise AssertionError(label)
    checks.append(label)
    print('PASS', label, flush=True)


def ui():
    dumped = adb('shell', 'uiautomator', 'dump', '/sdcard/usb-most-e2e.xml')
    if 'dumped to:' not in dumped:
        raise RuntimeError('UI hierarchy unavailable; refusing stale XML: ' + dumped[:200])
    raw = adb('shell', 'cat', '/sdcard/usb-most-e2e.xml')
    (args.output / 'last-ui.xml').write_text(raw)
    return list(ET.fromstring(raw).iter('node'))


def node(text=None, cls=None, rid=None):
    for attempt in range(10):
        nodes = ui()
        for n in nodes:
            if ((text is None or text.casefold() in n.get('text', '').casefold()) and
                (cls is None or n.get('class') == cls) and
                (rid is None or n.get('resource-id', '').endswith(rid))):
                return n
        scroll = next((n for n in nodes if n.get('scrollable') == 'true'), None)
        if scroll is not None:
            x1, y1, x2, y2 = map(int, re.findall(r'\d+', scroll.get('bounds')))
            low, high = int(y1 + (y2 - y1) * .8), int(y1 + (y2 - y1) * .2)
            start, end = (low, high) if attempt < 5 else (high, low)
            adb('shell', 'input', 'swipe', str((x1+x2)//2), str(start),
                str((x1+x2)//2), str(end), '300')
        time.sleep(.3)
    raise AssertionError(f'UI node missing: {text} {cls} {rid}')


def tap(n):
    x1, y1, x2, y2 = map(int, re.findall(r'\d+', n.get('bounds')))
    adb('shell', 'input', 'tap', str((x1 + x2)//2), str((y1 + y2)//2))
    time.sleep(.4)


def start_screen():
    adb('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    adb('shell', 'wm', 'dismiss-keyguard')
    adb('shell', 'cmd', 'statusbar', 'collapse', must=False)
    adb('shell', 'am', 'start', '-W', '-n', ACTIVITY)
    # API26 can restore focus/IME after an APK update. Dismiss the keyboard
    # only when actually shown; an unconditional BACK would close the Activity.
    if re.search(r'mInputShown=true\b', adb('shell', 'dumpsys', 'input_method')):
        adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')


def edit(value):
    fields = [n for n in ui() if n.get('class') == 'android.widget.EditText']
    chosen = next((n for n in fields if n.get('resource-id', '').endswith(':id/peer_input')), None)
    tap(chosen if chosen is not None else node(cls='android.widget.EditText'))
    adb('shell', 'input', 'keyevent', 'KEYCODE_MOVE_END')
    adb('shell', 'input', 'keyevent', '--longpress', *(['KEYCODE_DEL'] * 32))
    adb('shell', 'input', 'text', value)
    adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')


def legacy_page():
    tap(node(text='Изменить компьютер', cls='android.widget.Button'))
    tap(node(text='⋮', cls='android.widget.Button'))
    tap(node(text='Изменить подключение', cls='android.widget.Button'))


def save_address():
    tap(node(text='Сохранить компьютер', cls='android.widget.Button'))
    node(text='Добавить компьютер', cls='android.widget.Button')
    tap(node(text='Назад', cls='android.widget.Button'))


def grant_notifications(api):
    if api >= 33:
        adb('shell', 'pm', 'grant', APP, 'android.permission.POST_NOTIFICATIONS')


try:
    api = int(adb('shell', 'getprop', 'ro.build.version.sdk'))
    check(adb('shell', 'getprop', 'ro.kernel.qemu') == '1', 'emulator identity')
    adb('uninstall', APP + '.test', must=False)
    adb('uninstall', APP, must=False)
    check('Success' in adb('install', str(args.baseline)), 'baseline installs')
    grant_notifications(api)
    start_screen()
    edit('100.100.100.100')
    tap(node(text='Запустить', cls='android.widget.Button'))
    node(text='Готово.')
    tap(node(text='Остановить', cls='android.widget.Button'))
    adb('shell', 'am', 'force-stop', APP)
    check('Success' in adb('install', '-r', str(args.apk)), 'optimized APK updates baseline with same test signer')
    start_screen()
    check('100.100.100.100' in node(text='100.100.100.100').get('text'), 'saved server summary survives update without onboarding')
    legacy_page()
    check(node(cls='android.widget.EditText',rid=':id/peer_input').get('text') == '100.100.100.100', 'saved server survives update')
    save_address()
    tap(node(text='Настройки', cls='android.widget.Button'))
    tap(node(text='Инструкция для компьютера', cls='android.widget.Button'))
    check(node(text='Подключите Tailscale').get('text').endswith('пока недоступен.'),
          'VPN help explains unavailable tablet address without invented commands')
    check(not any('Копировать команды' == n.get('text') for n in ui()),
          'no copy action without a usable tablet IP')
    tap(node(text='Закрыть', cls='android.widget.Button'))
    # Clean-install scenario also tests initial permission denial on Android 13+.
    check('Success' in adb('shell', 'pm', 'clear', APP), 'fresh app state')
    check('Success' in adb('install', str(args.test_apk)), 'instrumentation installs')
    adb('logcat', '-b', 'crash', '-c')
    # Only fixture construction in the test APK uses hidden framework constructors.
    # The release implementation uses public USB APIs. Restore emulator policy afterwards.
    original_hidden_policy = adb('shell', 'settings', 'get', 'global', 'hidden_api_policy')
    if api >= 28:
        adb('shell', 'settings', 'put', 'global', 'hidden_api_policy', '1')
    try:
        with (args.output / 'instrumentation.log').open('w') as log:
            subprocess.run([args.adb, '-s', args.serial, 'shell', 'am', 'instrument', '-w', '-r',
                            '-e', 'diagnostic', 'true' if args.diagnostic else 'false',
                            APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
                           stdout=log, stderr=subprocess.STDOUT, timeout=360, check=True)
    finally:
        if api >= 28:
            if original_hidden_policy in ('', 'null'):
                adb('shell', 'settings', 'delete', 'global', 'hidden_api_policy')
            else:
                adb('shell', 'settings', 'put', 'global', 'hidden_api_policy', original_hidden_policy)
    output = (args.output / 'instrumentation.log').read_text()
    check('PASS optimized release API' in output and 'INSTRUMENTATION_CODE: -1' in output
          and 'FAIL' not in output, 'optimized release instrumentation')
    grant_notifications(api)
    # Instrumentation can leave an Activity/dialog stack behind. Start a fresh
    # process while preserving preferences before the independent host UI suite.
    adb('shell', 'am', 'force-stop', APP)
    start_screen()
    # Validate the other permission path: notifications granted, real UI start/stop.
    tap(node(text='Запустить', cls='android.widget.Button'))
    node(text='Готово.')
    notification = adb('shell', 'dumpsys', 'notification', '--noredact')
    check(any('NotificationRecord' in line and 'pkg=' + APP in line
              for line in notification.splitlines()), 'foreground notification is posted')
    tap(node(text='Остановить', cls='android.widget.Button'))
    node(text='Служба остановлена')
    # Real Storage Access Framework provider, not a mocked OutputStream.
    tap(node(text='Настройки', cls='android.widget.Button'))
    tap(node(text='Сохранить журнал', cls='android.widget.Button'))
    filename = f'usb-most-e2e-api{api}.txt'
    field = node(cls='android.widget.EditText')
    tap(field)
    adb('shell', 'input', 'keyevent', 'KEYCODE_MOVE_END')
    adb('shell', 'input', 'keyevent', '--longpress', *(['KEYCODE_DEL'] * 64))
    adb('shell', 'input', 'text', filename)
    adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
    # AOSP picker opens the writable Downloads location by default.
    tap(node(rid=':id/button1'))
    time.sleep(2)
    remote = '/sdcard/Download/' + filename
    data = adb('shell', 'cat', remote)
    check('service_start' in data and 'api=' + str(api) in data, 'SAF export writes readable diagnostic log')
    check('service_stop' in data, 'SAF export includes stop event')
    package = adb('shell', 'dumpsys', 'package', APP)
    version = re.search(r'versionName=(\S+)', package).group(1)
    check('version=' + version in data, 'exported log reports installed version')
    (args.output / 'exported-log.txt').write_text(data + '\n')
    adb('shell', 'rm', remote)
    # Explicit process death and relaunch must keep settings and usable UI.
    adb('shell', 'am', 'force-stop', APP)
    start_screen()
    tap(node(text='Запустить', cls='android.widget.Button'))
    node(text='Готово.')
    tap(node(text='Остановить', cls='android.widget.Button'))
    node(text='Служба остановлена')
    check(True, 'process death and relaunch recover cleanly')
    # Real Android TCP listener. Some images/multi-emulator networks do not use
    # classic NAT guest 10.0.2.15. Console redir targets that fixed address, so
    # use a temporary loopback-only guest relay when it does not match.
    helper_pid = None
    addresses = adb('shell', 'ip', '-4', 'addr')
    guest_ip = next((ip for ip in re.findall(r'inet (\d+\.\d+\.\d+\.\d+)/', addresses)
                     if ipaddress.ip_address(ip).is_private
                     and not ipaddress.ip_address(ip).is_loopback
                     and not ipaddress.ip_address(ip).is_link_local), None)
    check(guest_ip is not None, 'emulator has private LAN IPv4')
    legacy_transport = api <= 26 or guest_ip != '10.0.2.15'
    peer_ip = '10.0.2.2'
    if legacy_transport:
        peer_ip = guest_ip
        check(ipaddress.ip_address(peer_ip).is_private, 'guest LAN helper uses private IPv4')
    legacy_page()
    tap(node(text='Локальная сеть', cls='android.widget.RadioButton'))
    edit(peer_ip)
    save_address()
    tap(node(text='Запустить', cls='android.widget.Button'))
    node(text='Готово.')
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        forwarded_port = reservation.getsockname()[1]
    mapping = f'tcp:{forwarded_port}:3240'
    if legacy_transport:
        command = f'toybox nc -L -s 127.0.0.1 -p 3242 /system/bin/toybox nc {peer_ip} 3240 >/data/local/tmp/usb-most-lan-proxy.log 2>&1 < /dev/null & echo $!'
        helper_pid = adb('shell', 'sh', '-c', shlex.quote(command)).strip()
        check(helper_pid.isdigit(), 'temporary guest LAN relay process')
        adb('forward', f'tcp:{forwarded_port}', 'tcp:3242')
        time.sleep(.3)
    else:
        check('OK' in adb('emu', 'redir', 'add', mapping), 'temporary emulator LAN TCP forwarding')
    try:
        with socket.create_connection(('127.0.0.1', forwarded_port), timeout=5) as peer:
            peer.settimeout(5)
            peer.sendall(struct.pack('!HHI', 0x111, 0x8005, 0))
            reply = bytearray()
            while len(reply) < 12:
                chunk = peer.recv(12-len(reply))
                if not chunk:
                    break
                reply.extend(chunk)
        check(bytes(reply) == struct.pack('!HHII', 0x111, 5, 0, 0), 'LAN DEVLIST from host reaches real Android service')
        tap(node(text='Остановить', cls='android.widget.Button'))
        node(text='Служба остановлена')
        legacy_page()
        edit(str(ipaddress.ip_address(peer_ip) + 1))
        save_address()
        tap(node(text='Запустить', cls='android.widget.Button'))
        node(text='Готово.')
        rejected = False
        try:
            with socket.create_connection(('127.0.0.1', forwarded_port), timeout=5) as wrong_peer:
                wrong_peer.settimeout(5)
                wrong_peer.sendall(struct.pack('!HHI', 0x111, 0x8005, 0))
                rejected = wrong_peer.recv(1) == b''
        except (ConnectionResetError, BrokenPipeError):
            rejected = True
        check(rejected, 'LAN service rejects a source different from configured PC')
        tap(node(text='Остановить', cls='android.widget.Button'))
        node(text='Служба остановлена')
    finally:
        if legacy_transport:
            adb('forward', '--remove', f'tcp:{forwarded_port}', must=False)
            if helper_pid and helper_pid.isdigit():
                adb('shell', 'kill', helper_pid, must=False)
        else:
            adb('emu', 'redir', 'del', f'tcp:{forwarded_port}', must=False)

    crash = adb('logcat', '-b', 'crash', '-d')
    (args.output / 'crash.log').write_text(crash + '\n')
    check('FATAL EXCEPTION' not in crash and 'Fatal signal' not in crash, 'no Java/native crashes')
    result = {'api': api, 'serial': args.serial, 'status': 'passed', 'checks': checks,
              'apk_sha256': hashlib.sha256(args.apk.read_bytes()).hexdigest(),
              'scope': 'emulator; real USB hardware not present; baseline re-signed with local test key',
              'host_tcp_transport': 'ADB + temporary guest LAN relay' if legacy_transport else 'emulator NAT'}
except BaseException as exc:
    result = {'status': 'failed', 'checks': checks, 'error': str(exc)}
    try:
        (args.output / 'failure-logcat.txt').write_text(adb('logcat', '-d', '-t', '1000'))
    except Exception:
        pass
    raise
finally:
    result['apk_sha256'] = hashlib.sha256(args.apk.read_bytes()).hexdigest()
    result['test_apk_sha256'] = hashlib.sha256(args.test_apk.read_bytes()).hexdigest()
    result['baseline_apk_sha256'] = hashlib.sha256(args.baseline.read_bytes()).hexdigest()
    (args.output / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
