#!/usr/bin/env python3
"""Observe a normal foreground-service process on an owned emulator/QEMU pair.

Run run-android-connection-e2e.py --prepare-soak first. Instrumentation is NOT
active during this observation. This is an idle SSH/service lifecycle test,
not USB throughput, physical-device compatibility or battery-life evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time
from datetime import datetime, timezone
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'
MAIN = APP + '/org.cgutman.usbip.config.UsbIpConfig'
CONNECTED = 'Защищённое подключение'


class FixtureUnavailable(RuntimeError):
    """The owned peer stopped or changed; not evidence of an Android crash."""


def process_identity(pid):
    """Identify this exact Linux process, including PID reuse and exec changes."""
    try:
        proc = Path('/proc', str(pid))
        before = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
        command = (proc / 'cmdline').read_bytes()
        after = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    except (OSError, IndexError) as error:
        raise FixtureUnavailable('Owned QEMU process is unavailable') from error
    if (len(before) < 20 or len(after) < 20 or not command or len(command) > 65536
            or before[0] in ('Z', 'X') or after[0] in ('Z', 'X') or before[19] != after[19]):
        raise FixtureUnavailable('Owned QEMU process ended or changed during observation')
    return {'pid': pid, 'start_ticks': int(after[19]),
            'command_sha256': hashlib.sha256(command).hexdigest()}


def require_same_process(expected):
    observed = process_identity(expected['pid'])
    if observed != expected:
        raise FixtureUnavailable('Owned QEMU process identity changed')
    return observed


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--adb', type=Path, required=True)
    p.add_argument('--serial', required=True)
    p.add_argument('--lab', type=Path, required=True)
    p.add_argument('--apk', type=Path, required=True)
    p.add_argument('--preparation', type=Path, required=True)
    p.add_argument('--sudo-password-file', type=Path,
                   help='Optional private synthetic-fixture password for the owned QEMU health probe')
    p.add_argument('--seconds', type=int, default=28800)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    if not re.fullmatch(r'emulator-\d+', a.serial) or not 60 <= a.seconds <= 28800:
        p.error('Owned emulator and duration from 60 to 28800 seconds required')
    output, lab = a.output.resolve(), a.lab.resolve()
    if not output.is_relative_to(ROOT / 'build/e2e') or not lab.is_relative_to(ROOT / 'build/vm-lab'):
        p.error('Reports and QEMU fixtures must be in their owned build directories')
    prep = json.loads((a.preparation / 'result.json').read_text())
    digest = hashlib.sha256(a.apk.read_bytes()).hexdigest()
    assert prep['status'] == 'PASS' and prep['owned_fixture_retained']
    assert prep['apk_sha256'] == digest
    guest = json.loads((lab / 'connection.json').read_text())
    assert guest['ssh_host'] == '127.0.0.1' and guest['ssh_user'] == 'fixture'
    qemu_pid = int((lab / 'pid').read_text())
    qemu_command = Path('/proc', str(qemu_pid), 'cmdline').read_bytes()
    assert b'qemu-system-x86_64' in qemu_command and str(lab / 'guest.qcow2').encode() in qemu_command
    qemu_identity = process_identity(qemu_pid)
    health_input = None
    if a.sudo_password_file:
        password_file = a.sudo_password_file
        metadata = password_file.lstat()
        assert password_file.resolve().is_relative_to(ROOT / 'build')
        assert stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid()
        assert metadata.st_mode & 0o077 == 0 and 0 < metadata.st_size <= 4096
        password = password_file.read_bytes().removesuffix(b'\n')
        assert password and not any(value in password for value in (b'\n', b'\r', b'\0'))
        health_input = password + b'\n'
    ssh = ['ssh', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(guest['ssh_port']),
           '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'ConnectTimeout=5',
           '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts'),
           'fixture@127.0.0.1']
    adb = [str(a.adb.resolve()), '-s', a.serial]

    def call(*args, timeout=30):
        return subprocess.check_output(adb + list(args), text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()

    assert call('shell', 'getprop', 'ro.kernel.qemu') == '1'
    assert call('shell', 'getprop', 'sys.boot_completed') == '1'
    installed = call('shell', 'pm', 'path', APP).splitlines()
    assert len(installed) == 1 and installed[0].startswith('package:/data/app/')
    assert call('shell', 'sha256sum', installed[0][8:]).split()[0] == digest
    assert APP not in call('shell', 'dumpsys', 'activity', 'instrumentation'), 'Instrumentation still active'
    assert 'ServiceRecord{' not in call('shell', 'dumpsys', 'activity', 'services', APP)
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    report = {'schema': 1, 'status': 'running', 'requested_seconds': a.seconds,
              'started_utc': datetime.now(timezone.utc).isoformat(), 'apk_sha256': digest,
              'api': int(call('shell', 'getprop', 'ro.build.version.sdk')),
              'runner_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'qemu_identity': qemu_identity,
              'health_probe_auth': 'owned_fixture_sudo_stdin' if health_input else 'noninteractive_sudo',
              'scope': 'Uninstrumented Android foreground service, actual idle SSH to owned QEMU, background/screen off with periodic UI checks. No physical USB, active transfer or battery-life claim.',
              'samples': [], 'ui_checks': []}

    def save():
        part = output / 'result.tmp'
        part.write_text(json.dumps(report, indent=2) + '\n')
        part.replace(output / 'result.json')

    def tree():
        # Only an owned emulator's UI; no fixture credentials are displayed here.
        name = '/data/local/tmp/usb-most-idle-soak.xml'
        call('shell', 'uiautomator', 'dump', name)
        return ET.fromstring(call('shell', 'cat', name))

    def button(label):
        for node in tree().iter('node'):
            if node.get('package') == APP and node.get('text') == label and node.get('clickable') == 'true':
                box = re.fullmatch(r'\[(\d+),(\d+)\]\[(\d+),(\d+)\]', node.get('bounds', ''))
                assert box
                x1, y1, x2, y2 = map(int, box.groups())
                call('shell', 'input', 'tap', str((x1+x2)//2), str((y1+y2)//2))
                return
        raise AssertionError('Expected app action unavailable: ' + label)

    def foreground():
        call('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        call('shell', 'wm', 'dismiss-keyguard')
        call('shell', 'am', 'start', '-W', '-n', MAIN)

    def wait_connected(timeout):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            require_same_process(qemu_identity)
            current = tree()
            if any(n.get('package') == APP and n.get('text') == CONNECTED for n in current.iter('node')):
                return
            time.sleep(1)
        (output / 'last-ui.xml').write_bytes(ET.tostring(current, encoding='utf-8'))
        raise AssertionError('Actual UI did not confirm the idle authenticated connection')

    def receiver_health():
        require_same_process(qemu_identity)
        sudo = "sudo -S -p ''" if health_input else 'sudo -n'
        result = subprocess.run(ssh + [sudo + ' /usr/local/libexec/usb-most/usb-most-desktop --health'],
                                input=health_input, capture_output=True, timeout=15)
        require_same_process(qemu_identity)
        assert result.returncode == 0, 'Owned receiver health command failed'
        # Retain only public health counters. Never serialize the peer's proof,
        # stored credentials, configuration or complete management response.
        value = json.loads(result.stdout)['status']
        assert value['connected'] is True, 'Owned receiver no longer confirms the Android session'
        assert value['active'] == 0, 'Idle soak unexpectedly imported a USB device'
        return {'connected': True, 'active': 0}

    def background():
        call('shell', 'input', 'keyevent', 'KEYCODE_HOME')
        call('shell', 'input', 'keyevent', 'KEYCODE_SLEEP')

    started = False
    notification_restore = False
    save()
    try:
        # Notification refusal is covered by the core suite. Preserve the owned
        # emulator's setting while isolating this long-running lifecycle case.
        if report['api'] >= 33:
            package = call('shell', 'dumpsys', 'package', APP)
            grant = re.search(r'android\.permission\.POST_NOTIFICATIONS: granted=(true|false)', package)
            assert grant, 'Unknown prior notification permission'
            if grant[1] == 'false':
                call('shell', 'pm', 'grant', APP, 'android.permission.POST_NOTIFICATIONS')
                notification_restore = True
            report['notifications_granted_for_soak'] = True
        foreground()
        button('Запустить мост')
        started = True
        wait_connected(150)
        pid = call('shell', 'pidof', APP)
        assert pid.isdigit()
        report['pid'] = int(pid)
        report['connected_utc'] = datetime.now(timezone.utc).isoformat()
        begin = time.monotonic()
        next_ui = begin + 900
        background()
        while True:
            require_same_process(qemu_identity)
            assert call('shell', 'pidof', APP) == pid, 'Application process changed'
            services = call('shell', 'dumpsys', 'activity', 'services', APP)
            assert 'org.cgutman.usbip.service.UsbIpService' in services and 'isForeground=true' in services
            assert APP not in call('shell', 'dumpsys', 'activity', 'instrumentation')
            memory = call('shell', 'dumpsys', 'meminfo', APP)
            total = re.search(r'^\s*TOTAL PSS:\s*(\d+)', memory, re.M)
            if not total: total = re.search(r'^\s*TOTAL\s+(\d+)\s', memory, re.M)
            assert total, 'No actual Android PSS measurement'
            pss = int(total[1])
            assert pss < 256 * 1024, 'Application exceeded 256 MiB PSS bound'
            elapsed = time.monotonic() - begin
            report['samples'].append({'elapsed_seconds': round(elapsed, 2), 'pss_kib': pss,
                                      'receiver': receiver_health()})
            save()
            if elapsed >= a.seconds: break
            if time.monotonic() >= next_ui:
                foreground()
                wait_connected(15)
                report['ui_checks'].append(round(time.monotonic()-begin, 2))
                background()
                next_ui = time.monotonic() + 900
            time.sleep(min(60, max(0, a.seconds - (time.monotonic()-begin))))
        foreground()
        wait_connected(15)
        report['ui_checks'].append(round(time.monotonic()-begin, 2))
        button('Остановить мост')
        until = time.monotonic() + 30
        while 'ServiceRecord{' in call('shell', 'dumpsys', 'activity', 'services', APP):
            assert time.monotonic() < until, 'Foreground service did not stop'
            time.sleep(1)
        started = False
        report['status'] = 'passed'
    except FixtureUnavailable as error:
        report['status'] = 'fixture_failed'
        report['error'] = str(error)
        raise
    except BaseException as error:
        report['status'] = 'failed'
        report['error'] = str(error)
        raise
    finally:
        # This fixture has no USB. Cleanup affects only the owned emulator app.
        if started:
            subprocess.run(adb + ['shell', 'am', 'force-stop', APP], timeout=30, check=False)
        report['finished_utc'] = datetime.now(timezone.utc).isoformat()
        save()
        if notification_restore:
            call('shell', 'pm', 'revoke', APP, 'android.permission.POST_NOTIFICATIONS')
        call('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    print(json.dumps({k:v for k,v in report.items() if k not in ('samples', 'ui_checks')}), flush=True)


if __name__ == '__main__':
    main()
