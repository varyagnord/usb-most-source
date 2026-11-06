#!/usr/bin/env python3
"""Optimized Android ART/JNI to an owned Windows VM, real signed kernel HID.

The USB endpoint is synthetic and lives on host loopback behind adb reverse.
No physical USB, host driver, public listener or user account is used. Private
fixture credentials and rollback bytes remain in the private evidence directory.
"""
import argparse
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time

from lab_android_active_soak import process_identity, protect_android

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'
RECEIVER = r'C:\Program Files\USBMost\usb-most-desktop.exe'
# Every native child retains its own process handle, bounded deadline and stdin
# EOF. Never kill the guest service or another process by name on timeout.
CHILD = r'''
$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'
function Receiver([string[]]$Arguments,[string]$InputBase64='') {
 $info=[Diagnostics.ProcessStartInfo]::new('C:\Program Files\USBMost\usb-most-desktop.exe')
 $info.UseShellExecute=$false
 $info.RedirectStandardInput=$true; $info.RedirectStandardOutput=$true; $info.RedirectStandardError=$true
 foreach($argument in $Arguments){$info.ArgumentList.Add($argument)}
 $child=[Diagnostics.Process]::Start($info)
 try {
  $output=$child.StandardOutput.ReadToEndAsync(); $errorOutput=$child.StandardError.ReadToEndAsync()
  if($InputBase64){
   $inputBytes=[Convert]::FromBase64String($InputBase64)
   if($inputBytes.Length -gt 4096){throw 'Bounded receiver configuration required'}
   $child.StandardInput.BaseStream.Write($inputBytes,0,$inputBytes.Length)
  }
  $child.StandardInput.Close()
  if(!$child.WaitForExit(90000)){throw 'Receiver child deadline'}
  if(!$output.Wait(5000) -or !$errorOutput.Wait(5000)){throw 'Receiver child output deadline'}
  if($child.ExitCode){throw ('Receiver child exit '+$child.ExitCode+': '+$errorOutput.GetAwaiter().GetResult())}
  return $output.GetAwaiter().GetResult()
 } finally {
  if(!$child.HasExited){$child.Kill();[void]$child.WaitForExit(5000)}
  $child.Dispose()
 }
}
function Idle {
 $end=[DateTime]::UtcNow.AddSeconds(30)
 do {
  $health=Receiver -Arguments @('--health')|ConvertFrom-Json
  if(!$health.status.connected -and $health.status.active -eq 0){return $health}
  if([DateTime]::UtcNow -ge $end){throw 'Receiver remains active; refuse reconfiguration'}
  Start-Sleep -Milliseconds 200
 } while($true)
}
'''


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--adb', type=Path, required=True)
    p.add_argument('--serial', required=True)
    p.add_argument('--apk', type=Path, required=True)
    p.add_argument('--test-apk', type=Path, required=True)
    p.add_argument('--lab', type=Path, required=True)
    p.add_argument('--owner', default='owner-v3.json')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--transport', choices=('ssh', 'direct'), required=True)
    p.add_argument('--private-lan', action='store_true',
                   help='Temporarily classify the single owned NAT network as Private, then restore its category')
    a = p.parse_args()
    os.umask(0o077)
    lab, out = a.lab.resolve(), a.output.resolve()
    if not lab.is_relative_to(ROOT / 'build/vm-lab') or not out.is_relative_to(ROOT / 'build/e2e'):
        raise ValueError('Owned lab and private evidence paths required')
    if not re.fullmatch(r'owner-v[0-9]+\.json', a.owner):
        raise ValueError('Explicit owned VM record required')
    owner = json.loads((lab / a.owner).read_text())
    if owner.get('existing_disk') != 'system-v2.qcow2' or not owner.get('no_host_disks_or_USB'):
        raise ValueError('Explicit disposable Windows VM without host USB required')
    if process_identity(owner['qemu']['pid']) != owner['qemu']:
        raise ValueError('Owned VM identity changed')
    argv = Path('/proc', str(owner['qemu']['pid']), 'cmdline').read_bytes()
    if any(word in argv for word in (b'usb-host', b'vfio-pci', b'-virtfs', b'-fsdev', b'/dev/sd', b'/dev/nvme')):
        raise ValueError('Refuse host device/filesystem passthrough')
    if (lab / 'product-gates-running.flag').exists():
        raise ValueError('Product gates may own SSH; do not interrupt')
    protect_android(a.serial)
    locks = ROOT / 'build/e2e/android-locks'
    locks.mkdir(parents=True, exist_ok=True)
    held = []
    for path in (locks / (a.serial + '.lock'), lab / 'product-operation.lock'):
        lock = path.open('a'); fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB); held.append(lock)
    out.mkdir(mode=0o700, exist_ok=False)
    adb = [str(a.adb.resolve()), '-s', a.serial]
    ssh = ['ssh', '-n', '-F', '/dev/null', '-i', str(lab / 'client'), '-p', str(owner['ssh_port']),
           '-o', 'IdentitiesOnly=yes', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts-v2'), '-o', 'ConnectTimeout=15',
           'fixture@127.0.0.1']
    # This isolated seed exposes exec/SCP, deliberately no SFTP subsystem.
    # Transfer only fixed nonce-owned paths over the same pinned SSH channel.
    scp = ['scp', '-O', '-q', '-F', '/dev/null', '-i', str(lab / 'client'), '-P', str(owner['ssh_port']),
           '-o', 'IdentitiesOnly=yes', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
           '-o', 'UserKnownHostsFile=' + str(lab / 'known_hosts-v2'), '-o', 'ConnectTimeout=15']
    nonce = os.urandom(16).hex()
    guest_dir = 'C:\\USBMostLab\\android-' + nonce
    counter = 0

    def android(*args):
        return subprocess.check_output(adb + list(args), text=True, stderr=subprocess.STDOUT, timeout=90).strip()

    def remote(script):
        nonlocal counter
        assert process_identity(owner['qemu']['pid']) == owner['qemu']
        counter += 1
        # Do not put a potentially large rollback catalog on cmd.exe's command
        # line. SCP transfers private bounded files; a short launcher verifies
        # the exact file hash before execution, and SSH stdin stays at EOF.
        source = out / f'private-command-{counter}.ps1'
        source.write_text(CHILD + script, encoding='utf-8-sig')
        if source.stat().st_size > 65536:
            raise ValueError('Bounded guest fixture required')
        name = f'command-{counter}.ps1'
        transfer = subprocess.run(scp + [str(source), 'fixture@127.0.0.1:C:/USBMostLab/android-' + nonce + '/' + name],
                                  capture_output=True, timeout=60)
        (out / f'transfer-{counter}.stdout').write_bytes(transfer.stdout)
        (out / f'transfer-{counter}.stderr').write_bytes(transfer.stderr)
        if transfer.returncode:
            raise RuntimeError('Private guest script transfer failed')
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        path = guest_dir + '\\' + name
        launch = "if((Get-FileHash -LiteralPath '" + path + "').Hash.ToLowerInvariant() -cne '" + digest + "'){throw 'Fixture hash changed'}; & '" + path + "'"
        encoded = base64.b64encode(launch.encode('utf-16le')).decode()
        command = r'C:\USBMostLab\pwsh\pwsh.exe -NoProfile -NonInteractive -EncodedCommand ' + encoded
        r = subprocess.run(ssh + [command], capture_output=True, timeout=160)
        (out / f'guest-{counter}.stdout').write_bytes(r.stdout)
        (out / f'guest-{counter}.stderr').write_bytes(r.stderr)
        if r.returncode:
            raise RuntimeError('Owned guest operation failed; inspect private evidence')
        return r.stdout.decode('utf-8-sig').strip()

    def qmp(command):
        assert process_identity(owner['qemu']['pid']) == owner['qemu']
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(10); sock.connect(str(lab / 'qmp-v3.sock'))
            stream = sock.makefile('rwb', buffering=0); json.loads(stream.readline())
            for index, request in enumerate(({'execute': 'qmp_capabilities'},
                {'execute': 'human-monitor-command', 'arguments': {'command-line': command}})):
                request['id'] = index
                stream.write(json.dumps(request).encode() + b'\n')
                while (reply := json.loads(stream.readline())).get('id') != index:
                    pass
                if 'error' in reply:
                    raise RuntimeError('Owned QMP command failed')
            returned = reply['return'].strip()
            removed = 'host forwarding rule for ' + command.rsplit(' ', 1)[-1] + ' removed'
            if returned and not (command.startswith('hostfwd_remove ') and returned == removed):
                raise RuntimeError('Owned QMP host forward was not accepted')

    def save():
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')

    assert android('shell', 'getprop', 'ro.kernel.qemu') == '1'
    manifest = json.loads((ROOT / 'server/windows-receiver-lock-x86_64.json').read_text())['artifact']
    report = dict(status='running', transport=a.transport, api=int(android('shell', 'getprop', 'ro.build.version.sdk')),
                  apk_sha256=hashlib.sha256(a.apk.read_bytes()).hexdigest(),
                  test_apk_sha256=hashlib.sha256(a.test_apk.read_bytes()).hexdigest(),
                  receiver=manifest, qemu_identity=owner['qemu'],
                  scope='Optimized ART/JNI/ReceiverAssets and actual Windows SCM/signed kernel HID; synthetic loopback USB; not Android UsbManager/service UI or physical hardware', cleanup_errors=[])
    snapshot = None; network = None; changed = False; fixture = None; instrumentation = None; port = None; forwarded = False; reverse = None
    fixture_file = f'/sdcard/Android/data/{APP}/files/transport-fixture.json'
    try:
        # The top-level lab directory is not a secret store. Create an empty,
        # nonce-owned protected child before transferring any credential bytes.
        setup = r'''
$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'
if(([IO.File]::GetAttributes('C:\USBMostLab') -band [IO.FileAttributes]::ReparsePoint) -ne 0){throw 'Reparse lab refused'}
$path='GUEST_DIR'
if(Test-Path -LiteralPath $path){throw 'Private command directory already exists'}
[void][IO.Directory]::CreateDirectory($path)
$acl=[Security.AccessControl.DirectorySecurity]::new()
$acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)')
Set-Acl -LiteralPath $path -AclObject $acl
$actual=Get-Acl -LiteralPath $path
if(!$actual.AreAccessRulesProtected -or $actual.Access.Count -ne 2){throw 'Private command ACL not established'}
'''.replace('GUEST_DIR', guest_dir)
        encoded = base64.b64encode(setup.encode('utf-16le')).decode()
        result = subprocess.run(ssh + [r'C:\USBMostLab\pwsh\pwsh.exe -NoProfile -NonInteractive -EncodedCommand ' + encoded],
                                capture_output=True, timeout=60)
        (out / 'private-directory-setup.stdout').write_bytes(result.stdout)
        (out / 'private-directory-setup.stderr').write_bytes(result.stderr)
        assert result.returncode == 0, 'Private guest command directory was not established'
        snapshot = json.loads(remote(r'''
$null=Idle
$root='C:\ProgramData\USBMost'; $files=@{}
foreach($name in @('receiver.json','pairs.json')){
 $path=Join-Path $root $name
 if(([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint) -ne 0){throw 'Reparse state refused'}
 $files[$name]=[Convert]::ToBase64String([IO.File]::ReadAllBytes($path))
}
@{files=$files;identity=(Get-FileHash (Join-Path $root 'identity.json')).Hash.ToLowerInvariant();binary=(Get-FileHash 'C:\Program Files\USBMost\usb-most-desktop.exe').Hash.ToLowerInvariant()}|ConvertTo-Json -Compress
'''))
        (out / 'private-rollback.json').write_text(json.dumps(snapshot))
        assert snapshot['binary'] == manifest['sha256'], 'Installed Windows receiver is not current'
        if a.private_lan:
            network = json.loads(remote("$nets=@(Get-NetConnectionProfile); if($nets.Count -ne 1){throw 'Single isolated NAT network required'}; $nets[0]|Select-Object InterfaceIndex,NetworkCategory|ConvertTo-Json -Compress"))
            assert network['NetworkCategory'] in (0, 1), 'Do not change a domain network'
            assert isinstance(network['InterfaceIndex'], int) and network['InterfaceIndex'] > 0
            report['network_category_before'] = network['NetworkCategory']
            remote("Set-NetConnectionProfile -InterfaceIndex " + str(network['InterfaceIndex']) + " -NetworkCategory Private; if([int](Get-NetConnectionProfile -InterfaceIndex " + str(network['InterfaceIndex']) + ").NetworkCategory -ne 1){throw 'Private network not applied'}")
            report['test_network_category'] = 1
        android('shell', 'am', 'force-stop', APP)
        for apk in (a.apk, a.test_apk):
            assert 'Success' in android('install', '-r', str(apk.resolve()))
        android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        android('shell', 'wm', 'dismiss-keyguard')
        with (out / 'synthetic-usb.log').open('w') as log:
            fixture = subprocess.Popen([str(ROOT / 'build/rust-target/debug/examples/android_usb_fixture'), str(out)],
                env={**os.environ, 'USB_MOST_OWNED_EMULATOR_LAB': '1'}, stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 15
        while not (out / 'bridge.json').exists():
            assert fixture.poll() is None and time.monotonic() < deadline
            time.sleep(.05)
        bridge = json.loads((out / 'bridge.json').read_text()); reverse = bridge['port']
        android('reverse', '--no-rebind', f'tcp:{reverse}', f'tcp:{reverse}')
        pins = [base64.urlsafe_b64encode(hashlib.sha256(base64.b64decode(line.split()[2])).digest()).decode().rstrip('=')
                for line in (lab / 'known_hosts-v2').read_text().splitlines() if len(line.split()) >= 3]
        assert pins
        profile = dict(transport='ssh', endpoint=dict(host='10.0.2.2', port=owner['ssh_port']),
                       user='fixture', host_key=None, reverse_port=0, prepare=True)
        secret = dict(private_key=(lab / 'client').read_text(), client_id=os.urandom(16).hex())
        if a.transport == 'direct':
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0)); port = sock.getsockname()[1]
            config = base64.b64encode(json.dumps(dict(transport='direct', port=port)).encode()).decode()
            changed = True
            code = remote("$null=Idle; $null=Receiver -Arguments @('--install-stdin') -InputBase64 '" + config + "'; Receiver -Arguments @('--pair','10.0.2.2')")
            assert code.startswith('usbmost:1:')
            encoded = code.split(':', 2)[2]
            pair = json.loads(base64.urlsafe_b64decode(encoded + '=' * (-len(encoded) % 4)))
            assert pair['endpoint'] == dict(host='10.0.2.2', port=port)
            profile = dict(transport='direct', endpoint=pair['endpoint'], server_pin=pair['server_pin'])
            secret = dict(pair_code=code, pair_nonce=base64.urlsafe_b64encode(os.urandom(16)).decode().rstrip('='))
            qmp(f'hostfwd_add n0 tcp:127.0.0.1:{port}-:{port}'); forwarded = True
        config = dict(scope='owned-emulator-QEMU-synthetic-USB', profile=profile, secret=secret,
                      proof=bridge['proof'], bridge_port=reverse, host_pins=pins, steady_seconds=20)
        private = out / 'transport-fixture.json'; private.write_text(json.dumps(config))
        android('shell', 'mkdir', '-p', str(Path(fixture_file).parent)); android('push', str(private), fixture_file)
        with (out / 'instrumentation.log').open('w') as log:
            changed = True
            instrumentation = subprocess.Popen(adb + ['shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'transport',
                APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation'], stdout=log, stderr=subprocess.STDOUT)
        healths = []; deadline = time.monotonic() + 420
        for round_index in range(2):
            phase = 'TRANSPORT_CONNECTED round=' + str(round_index)
            while phase not in (out / 'instrumentation.log').read_text():
                assert instrumentation.poll() is None and time.monotonic() < deadline, 'Android transport did not connect; inspect private log'
                time.sleep(.1)
            h = json.loads(remote("Receiver -Arguments @('--health')"))
            # SCM/kernel import may follow the authenticated connection event.
            until = time.monotonic() + 15
            while not (h['status']['connected'] and h['status']['active'] == 1):
                assert time.monotonic() < until, 'Windows did not import the synthetic device'
                time.sleep(.2); h = json.loads(remote("Receiver -Arguments @('--health')"))
            healths.append(h)
        assert instrumentation.wait(timeout=90) == 0
        log = (out / 'instrumentation.log').read_text()
        assert 'INSTRUMENTATION_CODE: -1' in log and 'FAIL ' not in log
        assert 'ART JNI transport and saved-credential reconnect finished' in log
        (out / 'stop-fixture').touch(); assert fixture.wait(timeout=10) == 0
        stats = json.loads((out / 'usb-result.json').read_text())
        assert stats['imports'] == 2 and stats['controls'] > 0 and stats['reports'] > 0, stats
        report.update(status='passed', health=healths, usb=stats,
                      checks=int(re.search(r'PASS optimized release API \d+: (\d+) checks', log)[1]))
    except BaseException as error:
        report.update(status='failed', error_type=type(error).__name__, error=str(error))
        raise
    finally:
        def cleanup(label, action):
            try:
                action()
            except BaseException as error:
                report['cleanup_errors'].append(dict(step=label, error_type=type(error).__name__))
        cleanup('stop Android session', lambda: android('shell', 'am', 'force-stop', APP))
        if instrumentation is not None:
            cleanup('instrumentation exit', lambda: instrumentation.wait(timeout=15))
        cleanup('remove private device fixture', lambda: android('shell', 'rm', '-f', fixture_file))
        if reverse is not None:
            cleanup('remove own adb reverse', lambda: android('reverse', '--remove', f'tcp:{reverse}'))
        if fixture is not None and fixture.poll() is None:
            (out / 'stop-fixture').touch()
            cleanup('synthetic endpoint exit', lambda: fixture.wait(timeout=10))
        if forwarded:
            cleanup('remove own QEMU forward', lambda: qmp(f'hostfwd_remove n0 tcp:127.0.0.1:{port}'))
        if changed and snapshot is not None:
            def restore():
                files = snapshot['files']
                result = json.loads(remote("$null=Idle; Stop-Service USBMostReceiver; (Get-Service USBMostReceiver).WaitForStatus('Stopped',[TimeSpan]::FromSeconds(30)); "
                    + "[IO.File]::WriteAllBytes('C:\\ProgramData\\USBMost\\pairs.json',[Convert]::FromBase64String('" + files['pairs.json'] + "')); "
                    + "$null=Receiver -Arguments @('--install-stdin') -InputBase64 '" + files['receiver.json'] + "'; $h=Idle; "
                    + "@{config=[Convert]::ToBase64String([IO.File]::ReadAllBytes('C:\\ProgramData\\USBMost\\receiver.json'));pairs=[Convert]::ToBase64String([IO.File]::ReadAllBytes('C:\\ProgramData\\USBMost\\pairs.json'));identity=(Get-FileHash 'C:\\ProgramData\\USBMost\\identity.json').Hash.ToLowerInvariant();binary=(Get-FileHash 'C:\\Program Files\\USBMost\\usb-most-desktop.exe').Hash.ToLowerInvariant();health=$h}|ConvertTo-Json -Compress -Depth 6"))
                assert result['config'] == files['receiver.json'] and result['pairs'] == files['pairs.json']
                assert result['identity'] == snapshot['identity'] and result['binary'] == snapshot['binary']
                report['original_receiver_configuration_pairing_identity_restored'] = True
            cleanup('restore original receiver configuration and pairing', restore)
        if network is not None:
            def restore_network():
                category = 'Public' if network['NetworkCategory'] == 0 else 'Private'
                observed = remote("Set-NetConnectionProfile -InterfaceIndex " + str(network['InterfaceIndex']) + " -NetworkCategory " + category + "; [int](Get-NetConnectionProfile -InterfaceIndex " + str(network['InterfaceIndex']) + ").NetworkCategory")
                assert int(observed) == network['NetworkCategory']
                report['original_network_category_restored'] = True
            cleanup('restore original owned NAT network category', restore_network)
        if report['cleanup_errors']:
            report['status'] = 'failed'
        save()
    print(json.dumps(report), flush=True)
    if report['status'] != 'passed':
        raise SystemExit('Windows Android gate failed; private evidence retained')


if __name__ == '__main__':
    main()
