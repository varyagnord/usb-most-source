#!/usr/bin/env python3
"""Actual Android process death with an unsaved SSH destination. No network or USB."""
import argparse
import fcntl
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time
import xml.etree.ElementTree as ET
from lab_android_active_soak import protect_android

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--adb', required=True)
p.add_argument('--serial', required=True)
p.add_argument('--apk', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
assert re.fullmatch(r'emulator-\d+', a.serial)
protect_android(a.serial)
lockdir = Path(__file__).resolve().parents[1] / 'build/e2e/android-locks'
lockdir.mkdir(parents=True, exist_ok=True)
android_lock = (lockdir / (a.serial + '.lock')).open('a')
fcntl.flock(android_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
adb = [a.adb, '-s', a.serial]
app = 'ru.levchatkin.rutokenbridge'
act = app + '/org.cgutman.usbip.config.UsbIpConfig'
a.output.mkdir(parents=True, exist_ok=True)
checks = []

def run(*args, check=True):
    r = subprocess.run(adb + list(args), capture_output=True, text=True, timeout=60)
    if check and r.returncode: raise RuntimeError('ADB command failed: ' + args[0])
    return r.stdout.strip()

def verify(ok, label):
    assert ok, label
    checks.append(label)
    print('PASS', label, flush=True)

def nodes():
    raw = run('shell', 'uiautomator', 'dump', '/sdcard/usb-most-process-ui.xml')
    assert 'dumped to:' in raw
    return list(ET.fromstring(run('shell', 'cat', '/sdcard/usb-most-process-ui.xml')).iter('node'))

def bounds(n): return list(map(int, re.findall(r'\d+', n.get('bounds', ''))))

def find(text, kind='android.widget.Button', field=False):
    for attempt in range(20):
        tree = nodes()
        for index, node in enumerate(tree):
            if text not in (node.get('text'), node.get('content-desc')) or (not field and node.get('class') != kind): continue
            candidate = node
            if field:
                candidate = next((n for n in tree[index+1:] if n.get('class') == 'android.widget.EditText'), None)
                if candidate is None: continue
            box = bounds(candidate)
            if len(box) == 4 and box[2] > box[0] and box[3]-box[1] >= (40 if field or kind == 'android.widget.Button' else 1):
                return candidate
        scroll = next((n for n in tree if n.get('scrollable') == 'true'), None)
        if scroll is None: time.sleep(.3); continue
        x,y,xx,yy = bounds(scroll)
        low,high=y+(yy-y)*3//4,y+(yy-y)//4
        start,end=(low,high) if attempt < 10 else (high,low)
        run('shell','input','swipe',str((x+xx)//2),str(start),str((x+xx)//2),str(end),'200')
    raise AssertionError('Visible field/control not found: '+text)

def tap(node):
    x,y,xx,yy=bounds(node)
    run('shell','input','tap',str((x+xx)//2),str((y+yy)//2))
    time.sleep(.25)

def click(text): tap(find(text))

def set_field(label, value):
    tap(find(label, field=True))
    run('shell','input','keyevent','KEYCODE_MOVE_END')
    run('shell','input','keyevent','--longpress',*(['KEYCODE_DEL']*64))
    run('shell','input','text',value)
    run('shell','input','keyevent','KEYCODE_BACK')

def launch():
    run('shell','am','start','-W','-f','0x10200000',
        '-a','android.intent.action.MAIN','-c','android.intent.category.LAUNCHER','-n',act)
    time.sleep(.5)

report={'status':'running','checks':checks,'apk_sha256':hashlib.sha256(a.apk.read_bytes()).hexdigest()}
try:
    verify(run('shell','getprop','ro.kernel.qemu') == '1','owned emulator identity')
    report['api']=int(run('shell','getprop','ro.build.version.sdk'))
    assert 'Success' in run('install','-r',str(a.apk))
    assert 'Success' in run('shell','pm','clear',app)
    run('shell','input','keyevent','KEYCODE_WAKEUP')
    run('shell','wm','dismiss-keyguard')
    # Settle the launcher after clearing data. API34 may still be removing the
    # previous instrumentation task and can kill a new process before attach.
    run('shell','input','keyevent','KEYCODE_HOME')
    nodes()  # UIAutomator waits for the owned emulator window to become idle.
    launch()
    click('Настроить подключение')
    click('Через SSH')
    set_field('Адрес компьютера','127.0.0.1')
    set_field('Имя пользователя','fixture')
    set_field('Пароль SSH','fixture-password-a')
    click('Сохранить компьютер')
    verify(find('127.0.0.1', 'android.widget.TextView') is not None,'SSH save returns to main without starting service')
    click('Изменить компьютер')
    click('Доступ и настройки: 127.0.0.1')
    click('Изменить подключение')
    set_field('Адрес компьютера','127.0.0.2')
    set_field('Имя пользователя','fixture_b')
    set_field('Пароль SSH','fixture-password-b')
    before=run('shell','pidof',app)
    verify(bool(before),'app process exists before background kill')
    run('shell','input','keyevent','KEYCODE_HOME')
    time.sleep(1)
    verify(bool(re.fullmatch(r'\d+', before)), 'single owned application PID')
    verify(run('shell','su','0','cat','/proc/'+before+'/cmdline').split('\0')[0] == app,
           'exact application process verified before kill')
    # Userdebug emulator only: SIGKILL models memory reclamation without force-stop,
    # which would intentionally discard the Android task restoration state.
    run('shell','su','0','kill','-9',before)
    for _ in range(30):
        if not run('shell','pidof',app,check=False): break
        time.sleep(.1)
    verify(not run('shell','pidof',app,check=False),'real background process death, not Activity recreation')
    launch()
    verify(run('shell','pidof',app) != before,'new process restores task')
    verify(find('Адрес компьютера',field=True).get('text') == '127.0.0.2','unsaved host survives process death')
    verify(find('Имя пользователя',field=True).get('text') == 'fixture_b','unsaved login survives process death')
    verify(find('Пароль SSH',field=True).get('text') == '', 'previous computer password is not combined with restored draft host')
    click('Ключ и дополнительные параметры')
    verify(find('Пароль администратора · если отличается',field=True).get('text') == '', 'no restored administrator password')
    verify(find('Пароль ключа · если задан',field=True).get('text') == '', 'no restored key passphrase')
    click('Назад')
    run('shell','input','keyevent','KEYCODE_BACK')
    time.sleep(.3)
    verify(find('127.0.0.1','android.widget.TextView') is not None,'cancel after process restoration preserves original committed computer')
    report['status']='passed'
except BaseException as error:
    report['status']='failed'
    report['error']=str(error)
    raise
finally:
    run('shell','am','force-stop',app,check=False)
    (a.output/'result.json').write_text(json.dumps(report,indent=2)+'\n')
