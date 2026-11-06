#!/usr/bin/env python3
"""Run Android/Windows SSH E2E with a bounded, exactly restored lab-only policy."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

from lab_android_active_soak import process_identity, protect_android

ROOT = Path(__file__).resolve().parents[1]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for name in ('adb', 'apk', 'test-apk', 'lab', 'output'):
        p.add_argument('--' + name, type=Path, required=True)
    p.add_argument('--serial', required=True)
    a = p.parse_args()
    os.umask(0o077)
    lab, out = a.lab.resolve(), a.output.resolve()
    if not lab.is_relative_to(ROOT / 'build/vm-lab') or not out.is_relative_to(ROOT / 'build/e2e'):
        raise ValueError('Only owned private lab/evidence directories')
    owner = json.loads((lab / 'owner-v3.json').read_text())
    if not owner.get('no_host_disks_or_USB') or process_identity(owner['qemu']['pid']) != owner['qemu']:
        raise ValueError('Owned isolated Windows VM identity required')
    if (lab / 'product-gates-running.flag').exists():
        raise ValueError('Do not interrupt active product SSH gates')
    protect_android(a.serial)
    out.mkdir(mode=0o700, exist_ok=False)
    nonce = os.urandom(16).hex()
    task = 'USBMostAndroidSSH-' + nonce
    directory = 'C:\\USBMostLab\\android-forwarding-' + nonce
    helper = ROOT / 'tools/lab-windows-android-forwarding.ps1'
    helper_hash = hashlib.sha256(helper.read_bytes()).hexdigest()
    ssh = ['ssh', '-n', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(owner['ssh_port']),
           '-o', 'IdentitiesOnly=yes', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts-v2'), '-o', 'ConnectTimeout=15',
           'fixture@127.0.0.1']
    scp = ['scp', '-O', '-q', '-F', '/dev/null', '-i', str(lab / 'client'), '-P', str(owner['ssh_port']),
           '-o', 'IdentitiesOnly=yes', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts-v2'), '-o', 'ConnectTimeout=15']
    counter = 0

    def remote(script):
        nonlocal counter
        assert process_identity(owner['qemu']['pid']) == owner['qemu']
        encoded = base64.b64encode(script.encode('utf-16le')).decode()
        command = 'powershell.exe -NoProfile -NonInteractive -EncodedCommand ' + encoded
        assert len(command) < 8191
        counter += 1
        r = subprocess.run(ssh + [command], capture_output=True, timeout=180)
        (out / f'control-{counter}.stdout').write_bytes(r.stdout)
        (out / f'control-{counter}.stderr').write_bytes(r.stderr)
        if r.returncode:
            raise RuntimeError('Owned forwarding control failed; inspect private evidence')
        return r.stdout.decode('utf-8-sig').strip()

    serial = lab / 'serial-v3.log'
    st = serial.stat(); offset = st.st_size
    report = dict(status='running', qemu_identity=owner['qemu'], helper_sha256=helper_hash,
                  scope='Temporary remote-only forwarding on isolated pinned management SSH; exact original bytes restored; actual optimized Android/kernel HID gate',
                  phases=[], cleanup_errors=[])
    flag = lab / 'android-forwarding-running.flag'
    with flag.open('x') as file:
        json.dump(dict(nonce=nonce, task=task, qemu=owner['qemu']), file)
    attempted = False

    def phase(expected, timeout):
        nonlocal offset
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            assert process_identity(owner['qemu']['pid']) == owner['qemu']
            current = serial.stat()
            assert (current.st_dev, current.st_ino) == (st.st_dev, st.st_ino) and current.st_size >= offset
            with serial.open('rb') as stream:
                stream.seek(offset); data = stream.read()
            for line in data.splitlines(keepends=True):
                if not line.endswith(b'\n'):
                    break
                offset += len(line)
                if not line.startswith(b'USBMOST_ANDROID_FORWARDING '):
                    continue
                value = json.loads(base64.b64decode(line.split()[1]))
                if value['nonce'] != nonce:
                    continue
                report['phases'].append(value)
                if value['status'] == expected:
                    return value
                if value['status'] == 'restored' and expected == 'ready':
                    raise RuntimeError('Forwarding fixture retired before becoming ready')
            time.sleep(.2)
        raise TimeoutError('Forwarding phase deadline: ' + expected)

    try:
        remote(r'''$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue'
$path='DIRECTORY'
if(Test-Path $path){throw 'Do not overwrite private fixture directory'}
$acl=New-Object Security.AccessControl.DirectorySecurity
$acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
$null=[IO.Directory]::CreateDirectory($path,$acl)
'''.replace('DIRECTORY', directory))
        r = subprocess.run(scp + [str(helper), 'fixture@127.0.0.1:' + directory.replace('\\', '/') + '/fixture.ps1'],
                           capture_output=True, timeout=60)
        (out / 'transfer.stderr').write_bytes(r.stderr)
        assert r.returncode == 0, 'Private fixture transfer failed'
        attempted = True
        remote(r'''$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue'
$path='DIRECTORY\fixture.ps1'; $name='TASK'
if((Get-FileHash $path).Hash.ToLowerInvariant() -cne 'HASH'){throw 'Fixture source changed'}
$tokens=$null;$errors=$null
$null=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors)
if($errors.Count){throw 'Fixture syntax rejected'}
if(Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue){throw 'Do not overwrite a task'}
$arguments='-NoProfile -NonInteractive -Command "$env:USB_MOST_WINDOWS_VM_LAB=''1''; & ''DIRECTORY\fixture.ps1'' -Nonce ''NONCE''"'
$action=New-ScheduledTaskAction -Execute 'C:\USBMostLab\pwsh\pwsh.exe' -Argument $arguments
$principal=New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 15)
Register-ScheduledTask -TaskName $name -Action $action -Principal $principal -Settings $settings|Out-Null
Start-ScheduledTask -TaskName $name
'''.replace('DIRECTORY', directory).replace('TASK', task).replace('HASH', helper_hash).replace('NONCE', nonce))
        report['ready'] = phase('ready', 180)
        command = ['python3', str(ROOT / 'tools/run-windows-android-e2e.py'), '--adb', str(a.adb),
                   '--serial', a.serial, '--apk', str(a.apk), '--test-apk', str(a.test_apk),
                   '--lab', str(lab), '--transport', 'ssh', '--output', str(out / 'native')]
        with (out / 'native-run.log').open('w') as log:
            result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, timeout=720)
        report['native_exit_code'] = result.returncode
        report['native_result'] = json.loads((out / 'native/result.json').read_text())
        assert result.returncode == 0 and report['native_result']['status'] == 'passed'
        report['status'] = 'passed'
    except BaseException as error:
        report.update(status='failed', error_type=type(error).__name__, error=str(error))
        raise
    finally:
        if attempted:
            try:
                remote("$ErrorActionPreference='Stop';[IO.File]::WriteAllBytes('" + directory + "\\finish',[byte[]]@(1))")
                restored = phase('restored', 180)
                assert restored['restored'] and 'failure' not in restored
                assert restored['original_sha256'] == report.get('ready', restored)['original_sha256']
                report['original_management_ssh_bytes_restored'] = True
                remote(r'''$ErrorActionPreference='Stop';$ProgressPreference='SilentlyContinue'
$name='TASK'; $end=[DateTime]::UtcNow.AddSeconds(30)
while((Get-ScheduledTask -TaskName $name).State -eq 'Running'){
 if([DateTime]::UtcNow -ge $end){throw 'Owned task has not retired'}
 Start-Sleep -Milliseconds 200
}
$task=Get-ScheduledTask -TaskName $name
$expected='-NoProfile -NonInteractive -Command "$env:USB_MOST_WINDOWS_VM_LAB=''1''; & ''DIRECTORY\fixture.ps1'' -Nonce ''NONCE''"'
if($task.Actions.Count -ne 1 -or $task.Actions[0].Execute -ine 'C:\USBMostLab\pwsh\pwsh.exe' -or $task.Actions[0].Arguments -cne $expected){throw 'Task identity changed'}
if((Get-ScheduledTaskInfo -TaskName $name).LastTaskResult -ne 0){throw 'Task reported failure'}
Unregister-ScheduledTask -TaskName $name -Confirm:$false
'''.replace('TASK', task).replace('NONCE', nonce).replace('DIRECTORY', directory))
                report['temporary_task_removed'] = True
            except BaseException as error:
                report['cleanup_errors'].append(dict(error_type=type(error).__name__, error=str(error)))
        if report['cleanup_errors']:
            report['status'] = 'failed'
        if report.get('original_management_ssh_bytes_restored') or not attempted:
            flag.rename(lab / ('android-forwarding-terminal-' + nonce + '.json'))
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report), flush=True)
    if report['status'] != 'passed':
        raise SystemExit('Windows Android SSH gate failed; private evidence retained')


if __name__ == '__main__':
    main()
