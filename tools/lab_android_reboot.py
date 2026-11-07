"""Real reboot or verified APK update during an owned synthetic service import.

The first instrumentation is intentionally interrupted, never called a passed
suite. Postboot instrumentation must use the unchanged encrypted saved catalog;
no preboot pairing secret is supplied to its fixture.
"""
import hashlib
import ipaddress
import json
from pathlib import Path
import re
import subprocess
import time

from lab_android_active_soak import process_identity

APP = 'ru.levchatkin.rutokenbridge'


def reboot_active_service(*, adb, owner, report, save, output, config, private,
                          fixture_file, instrumentation, instrument_log,
                          upgrade_apk=None, expected_upgrade_version=None):
    if not owner or owner['serial'] != adb[2]:
        raise ValueError('Explicit exact emulator owner required')

    def command(*words, timeout=30, check=True):
        if process_identity(owner['qemu']['pid']) != owner['qemu']:
            raise RuntimeError('Owned emulator changed')
        result = subprocess.run(adb+list(words), capture_output=True, text=True, timeout=timeout)
        if words and words[0] == 'install':
            (output/'package-manager-install-result.json').write_text(json.dumps(
                {'exit_code': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}, indent=2)+'\n')
        if check and result.returncode:
            failure = {'operation': list(words[:3]), 'exit_code': result.returncode,
                       'stdout': result.stdout[-4096:], 'stderr': result.stderr[-4096:]}
            (output/'reboot-command-failure.json').write_text(json.dumps(failure, indent=2)+'\n')
            raise RuntimeError('Owned Android reboot operation failed: '+repr(words[:3]))
        return result.stdout.strip()

    def boot_id():
        return command('shell', 'cat', '/proc/sys/kernel/random/boot_id')

    catalog = '/data/user/0/'+APP+'/no_backup/computers.enc'

    def catalog_hash():
        value = command('shell', 'su', '0', 'sha256sum', catalog).split()[0]
        if len(value) != 64 or any(c not in '0123456789abcdef' for c in value):
            raise RuntimeError('Exact encrypted saved catalog missing')
        return value

    if command('shell', 'getprop', 'ro.kernel.qemu') != '1':
        raise ValueError('Never reboot a physical Android device')
    first_log = (output/'instrumentation.log').read_text()
    if 'PASS OS_EJECT_REBOOT_PENDING' not in first_log or 'FAIL ' in first_log:
        raise RuntimeError('Active service checkpoint was not reached')
    def package_info():
        text = command('shell', 'dumpsys', 'package', APP)
        version = re.search(r'\bversionCode=(\d+)', text)
        uid = re.search(r'\buserId=(\d+)', text)
        if not version or not uid:
            raise RuntimeError('Actual installed package version/UID missing')
        return {'version_code': int(version[1]), 'uid': int(uid[1])}

    event = 'apk_update' if upgrade_apk else 'kernel_reboot'
    record = {'status': 'before_interrupt', 'lifecycle_event': event,
              'package_before': package_info(), 'boot_id_before': boot_id(),
              'encrypted_catalog_before_sha256': catalog_hash(),
              'first_instrumentation_intentionally_interrupted': True,
              'old_instrumentation_pid': instrumentation.pid,
              'android_pid_before': command('shell', 'pidof', APP)}
    report['android_apk_update_evidence' if upgrade_apk else 'android_reboot_evidence'] = record
    save()
    if upgrade_apk:
        if not expected_upgrade_version or record['package_before']['version_code'] >= expected_upgrade_version:
            raise ValueError('An actual forward package upgrade is required')
        command('install', '-r', str(upgrade_apk.resolve()), timeout=180)
        # The CLI's human status may use stderr. Successful exit plus actual
        # PackageManager version/UID and installed APK digest are authoritative.
        record['adb_install_exit'] = 0
        current = boot_id()
        if current != record['boot_id_before']:
            raise RuntimeError('APK update unexpectedly rebooted the emulator')
        after = package_info()
        if after['version_code'] != expected_upgrade_version or after['uid'] != record['package_before']['uid']:
            raise RuntimeError('Actual upgrade version or preserved UID mismatch')
        paths = command('shell', 'pm', 'path', APP).splitlines()
        if len(paths) != 1 or not paths[0].startswith('package:/data/app/'):
            raise RuntimeError('One exact installed APK required')
        installed_hash = command('shell', 'sha256sum', paths[0][8:]).split()[0]
        if installed_hash != hashlib.sha256(upgrade_apk.read_bytes()).hexdigest():
            raise RuntimeError('Installed APK differs from requested upgrade')
        record.update(package_after=after, installed_apk_sha256=installed_hash,
                      uid_preserved=True, actual_version_increased=True,
                      no_kernel_reboot=True)
    else:
        command('reboot')
        deadline = time.monotonic()+150
        while True:
            try:
                current = boot_id()
                complete = command('shell', 'getprop', 'sys.boot_completed')
                if current != record['boot_id_before'] and complete == '1':
                    break
            except (RuntimeError, subprocess.TimeoutExpired):
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError('Real owned Android boot did not complete')
            time.sleep(.5)
    record['boot_id_after'] = current
    record['api'] = int(command('shell', 'getprop', 'ro.build.version.sdk'))
    command('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    command('shell', 'wm', 'dismiss-keyguard')
    record['encrypted_catalog_after_sha256'] = catalog_hash()
    if record['encrypted_catalog_after_sha256'] != record['encrypted_catalog_before_sha256']:
        raise RuntimeError('Encrypted saved catalog changed across reboot')
    services = command('shell', 'dumpsys', 'activity', 'services', APP)
    (output/'services-after-boot.txt').write_text(services+'\n')
    if 'ServiceRecord{' in services and 'UsbIpService' in services:
        raise RuntimeError('Unexpected automatic foreground service after boot')
    record['no_automatic_service_after_boot'] = True
    # Wait on the exact old handle. A timeout is a failure, not permission to
    # restart an allegedly dead test or reinterpret its old state file.
    record['old_instrumentation_exit'] = instrumentation.wait(timeout=45)
    instrument_log.close()
    (output/'instrumentation.log').rename(output/'instrumentation-before-reboot.log')
    (output/'os-ejection-before-reboot.json').write_bytes(private.read_bytes())
    # The first test saved a separate secret-free resume fixture in the app's
    # no-backup directory and fsynced it before signaling REBOOT_PENDING. Let
    # the app UID read it after boot; do not overwrite Android external storage.
    # The shared instrumentation requires an actual local IPv4 before it starts;
    # Android's boot flag can precede DHCP. Do not require Internet validation.
    network_deadline = time.monotonic()+90
    while True:
        addresses = command('shell', 'ip', '-4', 'address', 'show')
        private_ipv4 = [value for value in re.findall(r'inet ([0-9.]+)/', addresses)
                        if ipaddress.ip_address(value).is_private
                        and not ipaddress.ip_address(value).is_loopback
                        and not ipaddress.ip_address(value).is_link_local]
        if private_ipv4:
            record['postboot_private_ipv4'] = private_ipv4
            break
        if time.monotonic() >= network_deadline:
            raise RuntimeError('Owned emulator local IPv4 not ready after real boot')
        time.sleep(.5)
    log = (output/'instrumentation.log').open('w')
    child = subprocess.Popen(adb+['shell', 'am', 'instrument', '-w', '-r', '-e',
        'suite', 'os-ejection', '-e', 'reboot-resume', 'true', APP+'.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'],
        stdout=log, stderr=subprocess.STDOUT)
    record.update(status='real_'+event+'_complete_resume_started',
                  resume_fixture_has_no_pairing_secret=True,
                  resume_instrumentation=process_identity(child.pid),
                  source_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest())
    save()
    return child, log
