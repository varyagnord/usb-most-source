#!/usr/bin/env python3
"""Kill an actual UI save blocked at AtomicFile.startWrite, on an owned emulator.

The .new FIFO is a deterministic fault fixture, not an ordinary filesystem or
power-failure test. Production APK/code is unchanged. No network or real USB.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import time
import xml.etree.ElementTree as ET
from lab_android_active_soak import protect_android, process_identity

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', required=True)
p.add_argument('--serial', required=True)
p.add_argument('--qemu-pid', type=int, required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
protect_android(a.serial)
identity = process_identity(a.qemu_pid)
command = Path('/proc', str(a.qemu_pid), 'cmdline').read_bytes().split(b'\0')
assert b'qemu-system' in command[0]
assert command[command.index(b'-port') + 1].decode() == a.serial.split('-')[1]
assert Path('/proc', str(a.qemu_pid)).stat().st_uid == os.getuid()
root = Path(__file__).resolve().parents[1]
lockdir = root / 'build/e2e/android-locks'
lockdir.mkdir(parents=True, exist_ok=True)
lock = (lockdir / (a.serial + '.lock')).open('a')
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
a.output.mkdir(parents=True, exist_ok=False)
adb = [a.adb, '-s', a.serial]
app = 'ru.levchatkin.rutokenbridge'
base = '/data/user/0/' + app + '/no_backup/computers.enc'
directory = base.rsplit('/', 1)[0]
original_directory = None
directory_changed = False
checks = []
report = {'status': 'running', 'checks': checks, 'qemu_identity': identity,
          'apk_sha256': hashlib.sha256(a.apk.read_bytes()).hexdigest(),
          'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
          'scope': __doc__}


def run(*args, check=True):
    result = subprocess.run(adb + list(args), capture_output=True, text=True, timeout=60)
    if check and result.returncode:
        raise RuntimeError('ADB operation failed: ' + str(args[:3]) + ': ' + result.stderr[:200])
    return result.stdout.strip()


def shell(script):
    return run('shell', 'su 0 sh -c ' + shlex.quote(script))


def check(value, label):
    assert value, label
    checks.append(label)
    print('PASS ' + label, flush=True)


def nodes():
    run('shell', 'uiautomator', 'dump', '/sdcard/usb-most-killed-save.xml')
    return list(ET.fromstring(run('shell', 'cat', '/sdcard/usb-most-killed-save.xml')).iter('node'))


def bounds(n):
    return list(map(int, re.findall(r'\d+', n.get('bounds', ''))))


def find(text=None, field=False):
    for attempt in range(16):
        tree = nodes()
        for i, n in enumerate(tree):
            if text is not None and n.get('text', '').casefold() != text.casefold() and n.get('content-desc') != text:
                continue
            if field:
                n = next((c for c in (tree[i+1:] if text else tree)
                          if c.get('class') == 'android.widget.EditText'), None)
                if n is None:
                    continue
            elif n.get('clickable') != 'true':
                continue
            x, y, xx, yy = bounds(n)
            if xx > x and yy-y >= 40:
                return n
        area = next((n for n in tree if n.get('scrollable') == 'true'), None)
        if area is not None:
            x, y, xx, yy = bounds(area)
            lo, hi = y+(yy-y)*3//4, y+(yy-y)//4
            begin, end = (lo, hi) if attempt < 8 else (hi, lo)
            run('shell', 'input', 'swipe', str((x+xx)//2), str(begin), str((x+xx)//2), str(end), '150')
        else:
            time.sleep(.2)
    raise AssertionError('Visible control missing: ' + str(text))


def tap(n):
    x, y, xx, yy = bounds(n)
    run('shell', 'input', 'tap', str((x+xx)//2), str((y+yy)//2))
    time.sleep(.2)


def click(text):
    tap(find(text))


def replace(n, text):
    tap(n)
    run('shell', 'input', 'keyevent', 'KEYCODE_MOVE_END')
    run('shell', 'input', 'keyevent', '--longpress', *(['KEYCODE_DEL']*80))
    run('shell', 'input', 'text', text)
    run('shell', 'input', 'keyevent', 'KEYCODE_BACK')


def launch():
    run('shell', 'am', 'start', '-W', '-f', '0x10200000', '-n',
        app + '/org.cgutman.usbip.config.UsbIpConfig')


def cipher_hash():
    value = shell('sha256sum ' + base).split()[0]
    assert re.fullmatch('[0-9a-f]{64}', value)
    return value


def restore_directory():
    global directory_changed
    if directory_changed:
        assert process_identity(a.qemu_pid) == identity
        uid, gid, mode = original_directory.split(':')
        shell('toybox chown ' + uid + ':' + gid + ' ' + directory
              + ' && toybox chmod ' + mode + ' ' + directory)
        assert shell('stat -c %u:%g:%a ' + directory) == original_directory
        directory_changed = False


try:
    check(run('shell', 'getprop', 'ro.kernel.qemu') == '1', 'owned Android emulator')
    report['api'] = int(run('shell', 'getprop', 'ro.build.version.sdk'))
    check('Success' in run('install', '-r', str(a.apk.resolve())), 'exact optimized APK installed')
    check('Success' in run('shell', 'pm', 'clear', app), 'only disposable application data reset')
    run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    run('shell', 'wm', 'dismiss-keyguard')
    launch()
    click('Настроить подключение')
    click('Через SSH')
    replace(find('Адрес компьютера', field=True), '127.0.0.1')
    replace(find('Имя пользователя', field=True), 'killed-save-fixture')
    replace(find('Пароль SSH', field=True), 'synthetic-retained-password')
    click('Сохранить компьютер')
    click('Изменить компьютер')
    click('Доступ и настройки: 127.0.0.1')
    click('Переименовать')
    replace(find(field=True), 'uncommitted-name')
    previous_hash = cipher_hash()
    report['committed_ciphertext_sha256'] = previous_hash
    check(shell('test ! -e ' + base + '.new && echo absent') == 'absent', 'no prior temporary file')
    uid = shell('stat -c %u ' + base)
    assert uid.isdecimal() and int(uid) >= 10000
    context = shell('ls -Z ' + base).split()[0]
    assert re.fullmatch(r'u:object_r:app_data_file:s0(?::c[0-9]+(?:,c?[0-9]+)*)?', context)
    original_directory = shell('stat -c %u:%g:%a ' + directory)
    assert re.fullmatch(r'[0-9]+:[0-9]+:[0-7]{3,4}', original_directory)
    shell('toybox mkfifo ' + base + '.new && toybox chmod 666 ' + base + '.new'
          + ' && toybox chcon ' + context + ' ' + base + '.new')
    # read() reloads the last committed catalog before changing it. AtomicFile
    # normally discards .new then. A root-owned sticky directory temporarily
    # prevents only that unlink while permitting the real save to open the FIFO.
    # The exact original directory metadata is restored before restart/finally.
    directory_changed = True
    shell('toybox chown 0:0 ' + directory + ' && toybox chmod 1777 ' + directory)
    report['fault_fixture'] = 'MCS-matched FIFO; temporary sticky directory prevents pre-save unlink; no SELinux policy change'
    check(shell('test -p ' + base + '.new && echo fifo') == 'fifo', 'owned temporary FIFO installed')
    click('Сохранить')
    pid = run('shell', 'pidof', app)
    assert re.fullmatch(r'\d+', pid)
    waiting = ''
    blocked_stack = ''
    for _ in range(40):
        waiting = shell('for t in /proc/' + pid + '/task/*; do '
                        'printf "%s " "${t##*/}"; cat "$t/wchan"; echo; done')
        blocked = [line.split()[0] for line in waiting.splitlines()
                   if 'wait_for_partner' in line or 'pipe_wait' in line]
        for tid in blocked:
            assert tid.isdecimal()
            blocked_stack = shell('cat /proc/' + pid + '/task/' + tid + '/stack')
            if 'fifo_open' in blocked_stack:
                break
        if 'fifo_open' in blocked_stack:
            break
        time.sleep(.1)
    (a.output/'blocked-thread-wchan.txt').write_text(waiting + '\n')
    (a.output/'blocked-thread-stack.txt').write_text(blocked_stack + '\n')
    check('fifo_open' in blocked_stack, 'actual save worker kernel stack blocked in FIFO open before commit')
    check(cipher_hash() == previous_hash, 'committed ciphertext unchanged while actual save is pending')
    check(run('shell', 'su', '0', 'cat', '/proc/' + pid + '/cmdline').split('\0')[0] == app,
          'exact application process verified before SIGKILL')
    run('shell', 'input', 'keyevent', 'KEYCODE_HOME')
    check(process_identity(a.qemu_pid) == identity, 'same owned VM before process kill')
    run('shell', 'su', '0', 'kill', '-9', pid)
    for _ in range(40):
        if not run('shell', 'pidof', app, check=False):
            break
        time.sleep(.1)
    check(not run('shell', 'pidof', app, check=False), 'actual pending save process died')
    check(cipher_hash() == previous_hash, 'last committed ciphertext survives SIGKILL byte-for-byte')
    restore_directory()
    check(not directory_changed, 'exact original directory ownership/mode restored before restart')
    launch()
    new_pid = run('shell', 'pidof', app)
    check(bool(new_pid) and new_pid != pid, 'catalog loaded by a new process')
    # The killed activity draft may be restored; return to the persisted main
    # screen without completing that draft, then inspect the repository UI.
    for _ in range(5):
        if any(n.get('text') == 'Изменить компьютер' for n in nodes()):
            break
        run('shell', 'input', 'keyevent', 'KEYCODE_BACK')
    click('Изменить компьютер')
    check(find('Доступ и настройки: 127.0.0.1') is not None, 'original computer remains selected and named')
    check(not any(n.get('text') == 'uncommitted-name' for n in nodes()), 'uncommitted name was not published')
    check(cipher_hash() == previous_hash, 'restart did not rewrite or reset encrypted catalog')
    check(shell('test ! -e ' + base + '.new && echo absent') == 'absent', 'AtomicFile discarded only incomplete temporary FIFO')
    click('Доступ и настройки: 127.0.0.1')
    click('Переименовать')
    replace(find(field=True), 'committed-after-recovery')
    click('Сохранить')
    check(find('Доступ и настройки: committed-after-recovery') is not None, 'fresh UI save succeeds after recovery')
    check(cipher_hash() != previous_hash, 'successful fresh save produces a new encrypted catalog')
    report['status'] = 'passed'
except BaseException as error:
    report['status'] = 'failed'
    report['error'] = str(error)
    raise
finally:
    run('shell', 'am', 'force-stop', app, check=False)
    restore_directory()
    # Preserve evidence, never recursively remove an app directory or a mount.
    (a.output/'result.json').write_text(json.dumps(report, indent=2) + '\n')
