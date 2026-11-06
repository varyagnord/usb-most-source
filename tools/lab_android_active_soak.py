"""Observe the real service during instrumented synthetic storage transfer.

Instrumentation supplies the USB adapter and therefore affects process priority
and memory. This is explicitly not an uninstrumented battery/OS-kill experiment.
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time

APP = 'ru.levchatkin.rutokenbridge'
MAIN = APP + '/org.cgutman.usbip.config.UsbIpConfig'


def process_identity(pid):
    proc = Path('/proc', str(pid))
    before = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    command = (proc / 'cmdline').read_bytes()
    after = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    if (len(before) < 20 or len(after) < 20 or before[19] != after[19]
            or after[0] in ('Z', 'X') or not command):
        raise RuntimeError('Owned QEMU process ended or changed')
    return dict(pid=pid, start_ticks=int(after[19]), command_sha256=hashlib.sha256(command).hexdigest())


def protect_android(serial):
    """Reject a physical device or an emulator owned by the live protected soak."""
    if not re.fullmatch('emulator-[0-9]+', serial) or serial == 'emulator-5566':
        raise ValueError('Only unprotected owned Android emulator')
    protected = Path(__file__).resolve().parents[1] / 'build/e2e/current-active-service-eight-hour-api36/result.json'
    if protected.exists():
        record = json.loads(protected.read_text())
        if record.get('serial') == serial:
            expected = record['runner_identity']
            try:
                live = process_identity(expected['pid'])
            except (FileNotFoundError, ProcessLookupError):
                live = None
            if live == expected:
                raise ValueError('Android belongs to the live active soak')


def validate_sample(sample, pid, previous=None):
    if sample['pid'] != pid or sample['imports'] != 1 or not 0 < sample['pss_kib'] < 256 * 1024:
        raise RuntimeError('Android process/import/PSS changed unexpectedly')
    if previous is not None:
        if (sample['elapsed_ms'] <= previous['elapsed_ms']
                or sample['completed'] <= previous['completed']
                or sample['cpu_ms'] < previous['cpu_ms']):
            raise RuntimeError('No fresh observation or USB progress')


def observe(seconds, *, android, command, guest_phase, health, save, report,
            qemu, instrumentation, directory, marker):
    if not 60 <= seconds <= 28800:
        raise ValueError('Bounded active observation duration required')
    if not re.fullmatch(r'[0-9a-f]{32}', marker):
        raise ValueError('Owned observation marker required')
    pid_text = android('shell', 'pidof', APP)
    if not pid_text.isdecimal():
        raise RuntimeError('Expected one actual Android process')
    pid = int(pid_text)
    record = dict(status='running', requested_seconds=seconds, android_pid=pid,
                  qemu_identity=qemu, scope=__doc__, samples=[], ui_checks=[])
    report['active_soak'] = record
    request = directory + '/' + marker + '-soakui.go'
    ack = directory + '/' + marker + '-soakui.ack'
    started = time.monotonic()

    def foreground():
        android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        android('shell', 'wm', 'dismiss-keyguard')
        android('shell', 'am', 'start', '-W', '-n', MAIN)
        # The test already owns UiAutomation. Ask that owner to inspect the
        # actual screen; do not connect a competing uiautomator process.
        android('shell', 'touch', request)
        deadline = time.monotonic() + 45
        while True:
            try:
                value = android('shell', 'cat', ack)
            except subprocess.CalledProcessError:
                value = ''
            if value == '1':
                android('shell', 'rm', '-f', ack)
                break
            if value or time.monotonic() >= deadline or instrumentation.poll() is not None:
                raise RuntimeError('Actual active-service UI observation failed')
            time.sleep(.2)
        record['ui_checks'].append(round(time.monotonic() - started, 3))

    def background():
        android('shell', 'input', 'keyevent', 'KEYCODE_HOME')
        android('shell', 'input', 'keyevent', 'KEYCODE_SLEEP')

    previous = None
    writes = 0
    try:
        foreground()
        background()
        # The requested observation begins after the first real UI/background
        # transition. Android independently rejects an early completion marker.
        started = time.monotonic()
        next_ui = started + 900
        while True:
            if process_identity(qemu['pid']) != qemu:
                raise RuntimeError('Owned VM process changed during active transfer')
            if instrumentation.poll() is not None:
                raise RuntimeError('Android test ended before active observation completed')
            if android('shell', 'pidof', APP) != pid_text:
                raise RuntimeError('Android process changed during active transfer')
            services = android('shell', 'dumpsys', 'activity', 'services', APP)
            if 'org.cgutman.usbip.service.UsbIpService' not in services or 'isForeground=true' not in services:
                raise RuntimeError('Actual foreground service is absent')
            sample = json.loads(android('shell', 'cat', directory + '/active-soak-live.json'))
            validate_sample(sample, pid, previous)
            command('soak')
            writer = guest_phase('soak_sample')
            if writer['synced_writes'] <= writes:
                raise RuntimeError('Owned synced storage writes stopped progressing')
            writes = writer['synced_writes']
            state = health()
            if not state['connected'] or state['active'] != 1:
                raise RuntimeError('Receiver lost active storage import')
            elapsed = time.monotonic() - started
            record['samples'].append(dict(elapsed_seconds=round(elapsed, 3), android=sample,
                                          synced_writes=writes, receiver_active=1))
            previous = sample
            save()
            if elapsed >= seconds:
                break
            if time.monotonic() >= next_ui:
                foreground()
                background()
                next_ui = time.monotonic() + 900
            time.sleep(min(60, max(0, seconds - (time.monotonic() - started))))
        foreground()
        record.update(status='duration_observed_readback_and_cleanup_pending',
                      elapsed_seconds=round(time.monotonic() - started, 3))
        save()
    except BaseException as error:
        record.update(status='failed', error=type(error).__name__ + ': ' + str(error))
        save()
        raise
    finally:
        android('shell', 'rm', '-f', request, ack)
