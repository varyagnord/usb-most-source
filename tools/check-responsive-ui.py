#!/usr/bin/env python3
"""Real touch/layout checks on a disposable emulator; restores display settings."""
import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import time
import xml.etree.ElementTree as ET
p = argparse.ArgumentParser()
p.add_argument('--adb', required=True)
p.add_argument('--serial', required=True)
p.add_argument('--output', type=pathlib.Path, required=True)
p.add_argument('--apk', type=pathlib.Path, required=True)
p.add_argument('--test-apk', type=pathlib.Path, help='Optional layout/USB fixture instrumentation')
p.add_argument('--resume', action='store_true', help='Resume checked scenarios for this exact APK')
p.add_argument('--geometry-matrix', action='store_true', help='Four touch scenarios plus full fixture geometry matrix')
a = p.parse_args()
if not re.fullmatch('emulator-\\d+', a.serial):
    p.error('Disposable emulator required')
a.output.mkdir(parents=True, exist_ok=True)
APP = 'ru.levchatkin.rutokenbridge'
ACT = APP + '/org.cgutman.usbip.config.UsbIpConfig'

def adb(*args, binary=False, timeout=35):
    r = subprocess.run([a.adb, '-s', a.serial, *args], capture_output=True, timeout=timeout, check=True)
    return r.stdout if binary else r.stdout.decode().strip()

def nodes():
    adb('shell', 'uiautomator', 'dump', '/sdcard/usb-most-responsive.xml')
    return list(ET.fromstring(adb('shell', 'cat', '/sdcard/usb-most-responsive.xml')).iter('node'))

def find(text, cls=None, min_height=48):
    for attempt in range(36):
        visible = nodes()
        for n in visible:
            if text.casefold() in n.get('text', '').casefold() and (cls is None or n.get('class') == cls):
                b = list(map(int, re.findall('\\d+', n.get('bounds', ''))))
                if len(b) == 4 and b[2] > b[0] and (b[3] - b[1] >= min_height):
                    return n
        scrollables = [n for n in visible if n.get('scrollable') == 'true'
                       and n.get('class') == 'android.widget.ScrollView']
        bounds = scrollables[-1].get('bounds') if scrollables else f'[0,0][{width},{height}]'
        x, y, xx, yy = map(int, re.findall('\\d+', bounds))
        low, high = y + (yy - y) * 3 // 4, y + (yy - y) // 4
        start, end = (low, high) if attempt < 18 else (high, low)
        adb('shell', 'input', 'swipe', str((x + xx) // 2), str(start),
            str((x + xx) // 2), str(end), '250')
    raise AssertionError(f'Missing reachable control {text} {cls}')

def tap(n):
    x, y, xx, yy = map(int, re.findall('\\d+', n.get('bounds')))
    assert 0 <= x < xx <= width and 0 <= y < yy <= height
    adb('shell', 'input', 'tap', str((x + xx) // 2), str((y + yy) // 2))
    time.sleep(0.5)

def target(n):
    x, y, xx, yy = map(int, re.findall('\\d+', n.get('bounds')))
    assert yy - y >= 48, f"Touch target clipped: {n.get('text')} {n.get('bounds')}"
adb('install', '-r', str(a.apk))
results = []
progress_file = a.output / 'progress.json'
apk_hash = hashlib.sha256(a.apk.read_bytes()).hexdigest()
if a.resume:
    previous = json.loads(progress_file.read_text())
    assert previous['apk_sha256'] == apk_hash, 'Cannot reuse checks from a different APK'
    results = previous['scenarios']

def checkpoint():
    progress_file.write_text(json.dumps({'status': 'in_progress', 'apk_sha256': apk_hash,
                                        'scenarios': results}, indent=2) + '\n')

original_font = adb('shell', 'settings', 'get', 'system', 'font_scale')
original_size = adb('shell', 'wm', 'size')
original_keyboard = adb('shell', 'settings', 'get', 'secure', 'show_ime_with_hard_keyboard')
override = re.search('Override size: (\\d+x\\d+)', original_size)
try:
    adb('shell', 'settings', 'put', 'secure', 'show_ime_with_hard_keyboard', '0')
    screens = ([(320, 640, ['1.0', '2.0']), (640, 360, ['2.0']), (800, 1280, ['1.0'])]
               if a.geometry_matrix else
               [(320, 640, ['1.0', '1.3', '2.0']), (360, 800, ['1.0', '2.0']),
                (412, 915, ['1.0', '2.0']), (640, 360, ['1.0', '2.0']),
                (600, 960, ['1.0']), (800, 1280, ['1.0', '2.0'])])
    for width, height, fonts in screens:
        for font in fonts:
            if any(r['width'] == width and r['height'] == height and r['font_scale'] == font
                   and 'kind' not in r for r in results):
                print(f'REUSE checked APK: {width}x{height}, font {font}', flush=True)
                continue
            adb('shell', 'am', 'force-stop', APP)
            adb('shell', 'wm', 'size', f'{width}x{height}')
            adb('shell', 'settings', 'put', 'system', 'font_scale', font)
            adb('shell', 'am', 'start', '-W', '-n', ACT)
            time.sleep(1)
            tap(find('Настройки', 'android.widget.Button'))
            tap(find('Настроить компьютер', 'android.widget.Button'))
            tap(find('По IP ·', 'android.widget.Button'))
            lan = find('Локальная сеть', 'android.widget.RadioButton')
            target(lan)
            lan_bounds = list(map(int, re.findall('\\d+', lan.get('bounds'))))
            if width == 360 and font == '2.0':
                assert lan_bounds[2] - lan_bounds[0] >= 260, 'Large-font network mode must use full width'
            tap(lan)
            assert find('Локальная сеть', 'android.widget.RadioButton').get('checked') == 'true'
            if (width == 320 and font in ('1.0', '2.0')) or (width == 640 and font == '2.0'):
                adb('shell', 'settings', 'put', 'secure', 'show_ime_with_hard_keyboard', '1')
                peer = find('', 'android.widget.EditText')
                for focus_attempt in range(5):
                    tap(peer)
                    time.sleep(1)
                    fields = [n for n in nodes() if n.get('resource-id', '').endswith(':id/peer_input')]
                    if fields and fields[0].get('focused') == 'true':
                        break
                    peer = find('', 'android.widget.EditText')
                else:
                    raise AssertionError('Touch did not focus the peer field')
                value = '192.168.123.' + ('125' if width == 640 else '124' if font == '2.0' else '123')
                adb('shell', 'input', 'keyevent', 'KEYCODE_MOVE_END')
                adb('shell', 'input', 'keyevent', '--longpress', *(['KEYCODE_DEL'] * 32))
                adb('shell', 'input', 'text', value)
                time.sleep(1.2)
                field = find(value, 'android.widget.EditText', min_height=1)
                assert field.get('text') == value, 'IPv4 changed by narrow field'
                keyboard = adb('shell', 'dumpsys', 'input_method')
                assert 'mInputShown=true' in keyboard or 'mIsInputViewShown=true' in keyboard, 'Soft keyboard not shown'
                (a.output / f'{width}x{height}-font{font}-keyboard.png').write_bytes(adb('exec-out', 'screencap', '-p', binary=True))
                adb('shell', 'input', 'keyevent', 'KEYCODE_ENTER')
                time.sleep(1.2)
                assert APP in adb('shell', 'dumpsys', 'activity', 'activities').split('mResumedActivity:')[-1].splitlines()[0], 'Done from IME left the app'
                adb('shell', 'settings', 'put', 'secure', 'show_ime_with_hard_keyboard', '0')

            peer = find('', 'android.widget.EditText')
            if not peer.get('text'):
                tap(peer)
                adb('shell', 'input', 'text', '192.168.123.123')
            tap(find('Сохранить адрес', 'android.widget.Button'))
            toggle = find('Запустить', 'android.widget.Button')
            target(toggle)
            assert 'Запустить' in toggle.get('text', ''), 'Done must not start USB bridge'
            (a.output / f'{width}x{height}-font{font}-connection.png').write_bytes(adb('exec-out', 'screencap', '-p', binary=True))
            tap(find('Настройки', 'android.widget.Button'))
            help_button = find('Инструкция для компьютера', 'android.widget.Button')
            target(help_button)
            tap(help_button)
            command_text = find('sudo modprobe', min_height=1)
            assert command_text.get('resource-id', '').endswith(':id/connection_commands'), 'Help commands missing'
            assert 'usbip list -r ' in command_text.get('text', ''), 'Incomplete connection commands'
            target(find('Копировать команды', 'android.widget.Button'))
            (a.output / f'{width}x{height}-font{font}-help.png').write_bytes(adb('exec-out', 'screencap', '-p', binary=True))
            close_help = find('Закрыть', 'android.widget.Button')
            target(close_help)
            tap(close_help)
            tap(find('Назад', 'android.widget.Button'))
            all_devices = find('Все и новые', 'android.widget.CheckBox')
            target(all_devices)
            tap(all_devices)
            assert find('Все и новые', 'android.widget.CheckBox').get('checked') == 'true'
            tap(find('Все и новые', 'android.widget.CheckBox'))
            assert find('Все и новые', 'android.widget.CheckBox').get('checked') == 'false'
            tap(find('Настройки', 'android.widget.Button'))
            logs = find('Сохранить журнал', 'android.widget.Button')
            target(logs)
            tap(logs)
            assert any(('documentsui' in n.get('package', '') for n in nodes())), 'SAF picker not opened'
            adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
            time.sleep(0.5)
            about = find('О приложении', 'android.widget.Button')
            target(about)
            tap(about)
            package = adb('shell', 'dumpsys', 'package', APP)
            version = re.search(r'versionName=(\S+)', package).group(1)
            assert any('USB Мост · ' + version == n.get('text', '') for n in nodes()), 'About version mismatch'
            close = find('Понятно', 'android.widget.Button')
            target(close)
            tap(close)
            tap(find('Назад', 'android.widget.Button'))
            for _ in range(4):
                adb('shell', 'input', 'swipe', str(width // 2), str(height // 4), str(width // 2), str(height * 3 // 4), '150')
            (a.output / f'{width}x{height}-font{font}.png').write_bytes(adb('exec-out', 'screencap', '-p', binary=True))
            results.append({'width': width, 'height': height, 'font_scale': font, 'status': 'passed'})
            checkpoint()
            print(f'PASS touch/layout/SAF/about: {width}x{height}, font {font}', flush=True)
    if a.test_apk:
        adb('install', '-r', str(a.test_apk))
        original_policy = adb('shell', 'settings', 'get', 'global', 'hidden_api_policy')
        original_night = adb('shell', 'cmd', 'uimode', 'night')
        night = re.search(r'Night mode: (\w+)', original_night, re.IGNORECASE)
        assert night, 'Cannot determine system night-mode setting'
        try:
            adb('shell', 'settings', 'put', 'global', 'hidden_api_policy', '1')
            fixtures = [(800, 1280, '1.0', 'no'), (800, 1280, '1.3', 'no'),
                        (800, 1280, '2.0', 'no'), (320, 800, '2.0', 'yes')]
            if a.geometry_matrix:
                fixtures = [(w, h, f, 'no') for w, h, fs in
                            [(320, 640, ['1.0', '1.3', '2.0']), (360, 800, ['1.0', '2.0']),
                             (412, 915, ['1.0', '2.0']), (640, 360, ['1.0', '2.0']),
                             (600, 960, ['1.0']), (800, 1280, ['1.0', '2.0'])] for f in fs]
                fixtures.append((320, 800, '2.0', 'yes'))
            for width, height, font, mode in fixtures:
                if any(r.get('kind') and r['width'] == width and r['height'] == height
                       and r['font_scale'] == font and r.get('system_night') == mode for r in results):
                    print('REUSE checked fixtures:', width, height, font, mode, flush=True)
                    continue
                adb('shell', 'am', 'force-stop', APP)
                adb('shell', 'wm', 'size', f'{width}x{height}')
                adb('shell', 'settings', 'put', 'system', 'font_scale', font)
                adb('shell', 'cmd', 'uimode', 'night', mode)
                result = adb('shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'layout',
                             APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation', timeout=180)
                label = f'{width}x{height}-font{font}-night{mode}'
                (a.output / f'fixtures-{label}.log').write_text(result + '\n')
                assert 'PASS responsive geometry' in result and 'INSTRUMENTATION_CODE: -1' in result and 'FAIL' not in result, result
                folder = a.output / ('fixtures-' + label)
                folder.mkdir(exist_ok=True)
                adb('pull', '/sdcard/Android/data/' + APP + '/files/layout-proof/.', str(folder))
                results.append({'width': width, 'height': height, 'font_scale': font,
                                'system_night': mode, 'kind': 'USB fixtures/live parent resize/text bounds/consent dialogs',
                                'status': 'passed'})
                checkpoint()
                print('PASS fixtures/live resize/light theme/dialogs:', label, flush=True)
        finally:
            adb('shell', 'cmd', 'uimode', 'night', night.group(1).lower())
            if original_policy in ('', 'null'):
                adb('shell', 'settings', 'delete', 'global', 'hidden_api_policy')
            else:
                adb('shell', 'settings', 'put', 'global', 'hidden_api_policy', original_policy)
finally:
    adb('shell', 'am', 'force-stop', APP)
    adb('shell', 'wm', 'size', override.group(1) if override else 'reset')
    if original_keyboard in ('', 'null'):
        adb('shell', 'settings', 'delete', 'secure', 'show_ime_with_hard_keyboard')
    else:
        adb('shell', 'settings', 'put', 'secure', 'show_ime_with_hard_keyboard', original_keyboard)
    if original_font in ('', 'null'):
        adb('shell', 'settings', 'delete', 'system', 'font_scale')
    else:
        adb('shell', 'settings', 'put', 'system', 'font_scale', original_font)
(a.output / 'result.json').write_text(json.dumps({'status': 'passed', 'apk_sha256': hashlib.sha256(a.apk.read_bytes()).hexdigest(), 'scenarios': results}, indent=2) + '\n')
