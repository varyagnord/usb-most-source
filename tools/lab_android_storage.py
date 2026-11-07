#!/usr/bin/env python3
"""Guest-only half of the owned Android/QEMU storage test. No recursive cleanup.

The host must exclusively create the two image files and attach them with the
exact generated serials. This script is copied into that disposable Q35 guest;
it is not a product installer or a tool for physical disks.
"""
from pathlib import Path
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import threading
import time


def owned_serials(tag):
    # QEMU also derives device_id from serial and limits it to 20 characters.
    if not re.fullmatch(r'[a-z0-9]{1,11}', tag):
        raise ValueError('Owned USB tag must fit the QEMU serial limit')
    return ['UMOSTAND' + tag + suffix for suffix in ('A', 'B')]


def storage_tools():
    tools = ['/usr/bin/udevadm', '/usr/sbin/mkfs.ext4', '/usr/bin/mount',
             '/usr/bin/umount', '/usr/sbin/modprobe', '/usr/sbin/usbip', '/usr/sbin/usbipd']
    sfdisk = '/usr/sbin/sfdisk' if Path('/usr/sbin/sfdisk').is_file() else '/usr/bin/sfdisk'
    for path in [*tools, sfdisk]:
        if not Path(path).is_file() or not os.access(path, os.X_OK):
            raise RuntimeError('Owned storage fixture tool unavailable: ' + path)
    return dict(sfdisk=sfdisk, required=tools)


class OwnedWriter:
    """Bounded rotating writes to one new file on this run's owned mount."""
    def __init__(self, path, max_operations=4096, interval=.025):
        if type(max_operations) is not int or not 1 <= max_operations <= 30000:
            raise ValueError('Bounded owned writer operation count required')
        if not .025 <= interval <= 1:
            raise ValueError('Bounded owned writer interval required')
        self.path = path
        self.max_operations = max_operations
        self.interval = interval
        self.stop = threading.Event()
        self.ready = threading.Event()
        self.lock = threading.Lock()
        self.completed = 0
        self.error = None
        self.slots = [bytes(65536) for _ in range(4)]
        self.thread = threading.Thread(target=self.write, name='owned-storage-writer')
        self.thread.start()

    def write(self):
        try:
            with self.path.open('x+b', buffering=0) as file:
                file.truncate(4 * 65536)
                for sequence in range(1, self.max_operations + 1):
                    if self.stop.is_set():
                        return
                    slot = (sequence - 1) % 4
                    payload = (b'USBMOST-WRITER\0' + sequence.to_bytes(8, 'big')).ljust(
                        65536, bytes([sequence % 251]))
                    offset = 0
                    while offset < len(payload):
                        n = os.pwrite(file.fileno(), payload[offset:], slot * 65536 + offset)
                        assert n > 0, 'short owned writer progress'
                        offset += n
                    os.fdatasync(file.fileno())
                    with self.lock:
                        self.slots[slot] = payload
                        self.completed = sequence
                    self.ready.set()
                    if self.stop.wait(self.interval):
                        return
                raise AssertionError('owned writer reached bounded operation limit')
        except BaseException as error:
            with self.lock:
                self.error = error
            self.ready.set()

    def count(self):
        with self.lock:
            if self.error is not None:
                raise AssertionError('owned writer failed') from self.error
            return self.completed

    def finish(self):
        self.stop.set()
        self.thread.join(timeout=180)
        assert not self.thread.is_alive(), 'retain source: owned writer has not retired'
        count = self.count()
        expected = b''.join(self.slots)
        assert self.path.read_bytes() == expected
        return dict(bytes=len(expected), sha256=hashlib.sha256(expected).hexdigest(),
                    completed_synced_writes=count)


def main():
    tag = sys.argv[1]
    serials = owned_serials(tag)
    if os.geteuid() != 0 or 'Q35' not in Path('/sys/class/dmi/id/product_name').read_text():
        raise RuntimeError('Only owned root Q35 guest fixture')
    tools = storage_tools()
    if sys.argv[2:] == ['preflight']:
        print(json.dumps(dict(status='ready', tools=tools)), flush=True)
        return
    mode = sys.argv[2:3]
    assert mode in ([], ['manual'], ['two-volumes'], ['two-volumes-writer'], ['reboot'])
    soak_seconds = int(sys.argv[3]) if len(sys.argv) == 4 else 0
    if len(sys.argv) > 4 or not (soak_seconds == 0 or 60 <= soak_seconds <= 28800):
        raise ValueError('Bounded owned active soak duration required')
    interrupted = mode == ['reboot']
    manual = mode == ['manual']
    active_writer = mode == ['two-volumes-writer']
    two_volumes = mode == ['two-volumes'] or active_writer
    if soak_seconds and not active_writer:
        raise ValueError('Active soak requires the owned writer')

    def run(args, timeout=45, input=None):
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout, input=input)
        if result.returncode:
            raise RuntimeError('owned guest command failed: ' + repr(args) + ': ' + result.stderr)
        return result

    def until(fn, label, seconds=45):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            value = fn()
            if value:
                return value
            time.sleep(.1)
        raise AssertionError(label)

    def candidates(serial, imported):
        return [p.resolve() for p in Path('/sys/bus/usb/devices').iterdir()
                if (p / 'serial').is_file() and (p / 'serial').read_text().strip() == serial
                and ('/vhci_hcd.' in str(p.resolve())) == imported]

    def usb(serial, imported=False):
        found = until(lambda: candidates(serial, imported), 'owned USB missing')
        assert len(found) == 1
        device = found[0]
        assert (device / 'idVendor').read_text().strip() == '46f4'
        assert (device / 'idProduct').read_text().strip() == '0001'
        return device

    def block(parent):
        found = until(lambda: [p for p in Path('/sys/class/block').iterdir()
                              if p.resolve().is_relative_to(parent) and not (p / 'partition').exists()],
                      'owned block device missing')
        assert len(found) == 1 and found[0].name.startswith('sd')
        node = Path('/dev', found[0].name)

        def ready():
            # sysfs registers the name before SCSI capacity and udev's node are
            # ready; the old imported device may have used this same sdX name.
            try:
                major, minor = map(int, (found[0] / 'dev').read_text().split(':'))
                info = node.stat()
                return (stat.S_ISBLK(info.st_mode) and info.st_rdev == os.makedev(major, minor)
                        and int((found[0] / 'size').read_text()) == 131072)
            except (FileNotFoundError, ValueError):
                return False

        until(ready, 'owned block capacity/node not ready')
        run(['/usr/bin/udevadm', 'settle', '--timeout=15'])
        assert found[0].resolve().is_relative_to(parent) and ready()
        return node

    def volumes(parent, disk):
        if not two_volumes:
            return [disk]
        disk_sysfs = Path('/sys/class/block', disk.name).resolve(strict=True)
        assert disk_sysfs.is_relative_to(parent)

        def ready():
            found = sorted((p for p in Path('/sys/class/block').iterdir()
                            if (p / 'partition').exists() and p.resolve().parent == disk_sysfs),
                           key=lambda p: int((p / 'partition').read_text()))
            if len(found) != 2:
                return False
            result = []
            for i, part in enumerate(found):
                assert int((part / 'partition').read_text()) == i + 1
                assert int((part / 'start').read_text()) == 2048 + 61440 * i
                assert int((part / 'size').read_text()) == 61440
                node = Path('/dev', part.name)
                try:
                    info = node.stat()
                except FileNotFoundError:
                    return False
                major, minor = map(int, (part / 'dev').read_text().split(':'))
                if not stat.S_ISBLK(info.st_mode) or info.st_rdev != os.makedev(major, minor):
                    return False
                result.append(node)
            return result

        return until(ready, 'exact owned two partition nodes missing')

    def digest(file):
        return hashlib.sha256(file.read_bytes()).hexdigest()

    def report(phase, **fields):
        print(json.dumps(dict(phase=phase, **fields)), flush=True)

    def command(expected):
        while True:
            value = sys.stdin.readline().strip()
            if value == 'quit' or not value:
                raise EOFError('owned test stopped')
            if value == 'soak':
                if not soak_seconds or writer is None or not writer.thread.is_alive():
                    raise RuntimeError('Owned soak writer is not active')
                neighbor_ok()
                if not all(os.path.ismount(p) for p in mounts):
                    raise RuntimeError('Owned soak volume unexpectedly unmounted')
                report('soak_sample', synced_writes=writer.count(),
                       neighbor_unchanged=True, all_owned_volumes_mounted=True)
                continue
            assert value == expected, (value, expected)
            return

    physical = [usb(serial) for serial in serials]
    assert physical[0] != physical[1]
    devices = [block(p) for p in physical]
    if two_volumes:
        # Only the source created with exclusive xb by this host invocation.
        # The neighbor remains a separate whole-device filesystem.
        source = devices[0]
        assert Path('/sys/class/block', source.name).resolve().is_relative_to(physical[0])
        assert not any(p.resolve().parent == Path('/sys/class/block', source.name).resolve()
                       and (p / 'partition').exists() for p in Path('/sys/class/block').iterdir())
        assert not any(line.split()[0] == str(source) for line in Path('/proc/mounts').read_text().splitlines())
        run([tools['sfdisk'], str(source)],
            input='label: dos\nunit: sectors\n\nstart=2048, size=61440, type=83\nstart=63488, size=61440, type=83\n')
        run(['/usr/bin/udevadm', 'settle', '--timeout=15'])
    source_volumes = volumes(physical[0], devices[0])
    for dev, parent in [(p, physical[0]) for p in source_volumes] + [(devices[1], physical[1])]:
        assert Path('/sys/class/block', dev.name).resolve().is_relative_to(parent)
        assert not any(line.split()[0] == str(dev) for line in Path('/proc/mounts').read_text().splitlines())
        # Exact serials belong to this invocation's exclusively-created images.
        run(['/usr/sbin/mkfs.ext4', '-q', str(dev)])
    own = Path('/run', 'usb-most-android-' + tag + '-owned')
    second = Path('/run', 'usb-most-android-' + tag + '-second')
    other = Path('/run', 'usb-most-android-' + tag + '-neighbor')
    own.mkdir(); other.mkdir()
    mounts = [own]
    if two_volumes:
        second.mkdir(); mounts.append(second)
    daemon = held = writer = None
    writer_result = None
    exported = False
    cleanup_errors = []
    avc_path = Path('/var/log/audit/audit.log')
    if Path('/usr/sbin/getenforce').is_file():
        selinux = run(['/usr/sbin/getenforce']).stdout.strip()
    else:
        if Path('/sys/fs/selinux/enforce').exists():
            raise RuntimeError('SELinux mounted but observation tool unavailable')
        selinux = 'not_mounted'
    audit_start = (avc_path.stat().st_ino, avc_path.stat().st_size) if selinux == 'Enforcing' else None
    try:
        run(['/usr/bin/mount', str(devices[1]), str(other)])
        neighbor = other / 'fixture.txt'
        neighbor.write_bytes(b'Owned neighboring virtual disk must remain mounted\n')
        neighbor_hash = digest(neighbor)

        def neighbor_ok():
            assert os.path.ismount(other) and digest(neighbor) == neighbor_hash

        run(['/usr/sbin/modprobe', 'usbip_host'])
        run(['/usr/sbin/modprobe', 'vhci_hcd'])
        run(['/usr/sbin/usbip', 'bind', '-b', physical[0].name]); exported = True
        daemon = subprocess.Popen(['/usr/sbin/usbipd', '-4'], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        time.sleep(.4)
        assert daemon.poll() is None, 'refuse to share an unknown usbipd'
        report('ready', bus=physical[0].name, serial=serials[0], selinux=selinux,
               speeds=[(p / 'speed').read_text().strip() for p in physical])
        command('mount')
        virtual = usb(serials[0], True)
        imported = block(virtual)
        # The filesystem is known because this harness created it. Avoid
        # unrelated filesystem probes over a deliberately delayed USB transport.
        # This is preparation, not the product's bounded ejection operation.
        imported_volumes = volumes(virtual, imported)
        data_files, hashes, contents = [], [], []
        for i, (dev, mountpoint) in enumerate(zip(imported_volumes, mounts)):
            run(['/usr/bin/mount', '-t', 'ext4', str(dev), str(mountpoint)], timeout=180)
            data = mountpoint / 'fixture.txt'
            payload = (f'Android multi-volume fixture {i + 1} preserves data\n'.encode()
                       if two_volumes else b'Android UI -> TLS -> actual UDisks2 preserves data\n')
            data.write_bytes(payload * 8192)
            data_files.append(data); hashes.append(digest(data))
            contents.append(dict(partition=i + 1, bytes=data.stat().st_size, sha256=hashes[-1]))
        own_hash = hashes[0]
        held = data_files[-1].open('rb')
        writes_before_eject = 0
        if active_writer:
            writer = OwnedWriter(second / 'active.bin',
                                 max_operations=soak_seconds + 900 if soak_seconds else 4096,
                                 interval=1 if soak_seconds else .025)
            assert writer.ready.wait(timeout=120), 'first owned synced write deadline'
            writes_before_eject = writer.count()
            assert writes_before_eject > 0 and writer.thread.is_alive()
        if interrupted:
            os.sync()  # Known committed checkpoint, not a claim about in-flight writes.
        report('held', imported=imported.name, bytes=sum(c['bytes'] for c in contents),
               sha256=own_hash, volumes=contents, synced_writes_before_eject=writes_before_eject)
        if interrupted:
            command('interrupted')
            held.close(); held = None
            until(lambda: not candidates(serials[0], True), 'old import survives Android reboot', seconds=90)
            for path in mounts:
                run(['/usr/bin/umount', str(path)], timeout=120)
                assert not os.path.ismount(path)
            neighbor_ok()
            run(['/usr/sbin/usbip', 'unbind', '-b', physical[0].name]); exported = False
            restored = block(physical[0])
            for dev, mountpoint, expected in zip(volumes(physical[0], restored), mounts, hashes):
                run(['/usr/bin/mount', '-t', 'ext4', '-o', 'ro,noload', str(dev), str(mountpoint)])
                assert digest(mountpoint / 'fixture.txt') == expected
                run(['/usr/bin/umount', str(mountpoint)])
            neighbor_ok()
            report('interrupted_verified', readback_sha256=own_hash, volumes_readback=contents,
                   neighbor_unchanged=True, original_import_retired=True,
                   checkpoint_synced_before_interrupt=True, safe_ejection_claimed=False)
            return
        command('manual' if manual else 'busy')
        assert os.path.ismount(mounts[-1]) and digest(data_files[-1]) == hashes[-1]
        if two_volumes:
            assert not os.path.ismount(own), 'first volume must have been unmounted before second veto'
            until_at = time.monotonic() + 3
            while time.monotonic() < until_at:
                assert not os.path.ismount(own), 'partial unmount was silently reversed'
                assert os.path.ismount(second) and candidates(serials[0], True) == [virtual]
                time.sleep(.1)
        if writer is not None:
            assert writer.thread.is_alive(), 'writer must still be active when OS reports Busy'
            assert writer.count() > writes_before_eject, 'no live writes across the ejection interval'
            writer_result = writer.finish()
            writer_result['active_when_busy'] = True
            writer_result['synced_writes_before_eject'] = writes_before_eject
            writer = None
        neighbor_ok()
        assert held.read(7) == b'Android'
        held.close(); held = None
        if manual:
            # Ordinary OS unmount must complete while the Android service and
            # its exact imported device still exist. Never detach to escape a
            # slow unmount. Read the persisted data through that same import.
            run(['/usr/bin/umount', str(own)], timeout=180)
            assert not os.path.ismount(own)
            assert candidates(serials[0], True) == [virtual]
            assert Path('/sys/class/block', imported.name).resolve().is_relative_to(virtual)
            run(['/usr/bin/mount', '-t', 'ext4', '-o', 'ro,noload', str(imported), str(own)], timeout=180)
            assert digest(own / 'fixture.txt') == own_hash
            run(['/usr/bin/umount', str(own)], timeout=180)
            neighbor_ok()
            report('manual_verified', readback_sha256=own_hash, neighbor_unchanged=True,
                   unmounted=True, same_import_still_present=True)
            command('stopped')
        else:
            report('released', busy_preserved_mount=True, neighbor_unchanged=True,
                   first_volume_unmounted=two_volumes, held_volume_preserved=True,
                   partial_unmount_not_automatically_reversed=two_volumes,
                   active_writer=writer_result)
            command('safe')
        until(lambda: all(not os.path.ismount(p) for p in mounts), 'UI Safe without all OS unmounts')
        neighbor_ok()
        until(lambda: not candidates(serials[0], True), 'import did not retire after Safe')
        run(['/usr/sbin/usbip', 'unbind', '-b', physical[0].name]); exported = False
        restored = block(physical[0])
        for dev, mountpoint, expected in zip(volumes(physical[0], restored), mounts, hashes):
            run(['/usr/bin/mount', '-t', 'ext4', '-o', 'ro,noload', str(dev), str(mountpoint)])
            assert digest(mountpoint / 'fixture.txt') == expected
            if writer_result is not None:
                if mountpoint == second:
                    assert digest(mountpoint / 'active.bin') == writer_result['sha256']
                else:
                    assert not (mountpoint / 'active.bin').exists()
            run(['/usr/bin/umount', str(mountpoint)])
        neighbor_ok()
        avc = []
        if audit_start is not None:
            assert avc_path.stat().st_ino == audit_start[0], 'audit rotated; evidence incomplete'
            with avc_path.open('rb') as f:
                f.seek(audit_start[1]); new = f.read().decode(errors='replace')
            avc = [line for line in new.splitlines() if 'type=AVC ' in line or 'type=USER_AVC ' in line]
            assert not avc, 'SELinux denial during owned run'
        report('verified', readback_sha256=own_hash, volumes_readback=contents,
               active_writer_readback=writer_result, neighbor_unchanged=True, avc=avc)
        assert sys.stdin.readline().strip() == 'quit'
    finally:
        if writer is not None:
            try:
                writer.finish()
            except Exception as error:
                # A failed test may already have closed its data transport.
                # Preserve that write failure; a retired writer still permits
                # ordinary unmount. A live writer never permits source teardown.
                retired = not writer.thread.is_alive()
                report('writer_failed', error_type=type(error).__name__, retired=retired)
                if not retired:
                    cleanup_errors.append('writer has not retired')
        if held is not None:
            held.close()
        # Never remove images, contents, mountpoints or their parents. Ordinary
        # unmount must succeed before source teardown. Failure retains the disk.
        for path in ([] if cleanup_errors else [*mounts, other]):
            if os.path.ismount(path):
                try:
                    run(['/usr/bin/umount', str(path)])
                except Exception as error:
                    cleanup_errors.append(type(error).__name__ + ':' + str(path))
        if not cleanup_errors:
            if exported:
                run(['/usr/sbin/usbip', 'unbind', '-b', physical[0].name])
            if daemon is not None:
                daemon.terminate(); daemon.wait(timeout=5)
        if cleanup_errors:
            report('cleanup_failed', retained=cleanup_errors)
            raise RuntimeError('cleanup incomplete; retain source and mountpoints: ' + repr(cleanup_errors))
        report('cleaned', images_and_directories_retained=True)


if __name__ == '__main__':
    main()
