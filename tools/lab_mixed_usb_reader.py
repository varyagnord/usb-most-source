"""Owned-VM mixed load: exact RAM disk, synthetic CCID and synthetic mouse only.

No filesystem mounts, formats, recursive cleanup or host device access. The host
runner must establish its QEMU identity and create the unique root-owned guard.
"""
import argparse
import array
import collections
from contextlib import contextmanager
import ctypes
import fcntl
import hashlib
import json
import mmap
import os
from pathlib import Path
import re
import select
import stat
import struct
import sys
import threading
import time

SIZES = (512, 4096, 65536, 131072)
CAPACITY = 64 * 1024 * 1024
STARTED = time.monotonic()
PROGRESS_LOCK = threading.Lock()


def progress(stage, **fields):
    with PROGRESS_LOCK:
        print(json.dumps({'stage': stage, 'elapsed_seconds': round(time.monotonic() - STARTED, 3),
                          **fields}), file=sys.stderr, flush=True)


def require(ok, reason):
    if not ok:
        raise RuntimeError(reason)


def validate_identity(pid, serial, ancestry, capacity=None, mounted=False):
    expected = {2: 'USBMOSTEJECT113', 3: 'USBMOSTCCID113'}
    require(pid in expected and serial == expected[pid], 'unexpected synthetic USB identity')
    require('/vhci_hcd.' in ancestry, 'device is not imported through owned guest VHCI')
    if pid == 2:
        require(capacity == CAPACITY, 'RAM disk has unexpected capacity')
        require(not mounted, 'RAM fixture or a child is mounted; no write allowed')


def guard(path, nonce):
    require(os.geteuid() == 0, 'owned guest root required')
    require(re.fullmatch(r'[0-9a-f]{32}', nonce) is not None, 'invalid owned guard nonce')
    require(path == Path('/run/usb-most-mixed-' + nonce), 'invalid owned guard path')
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_uid == 0
            and stat.S_IMODE(info.st_mode) == 0o600, 'guard ownership or mode differs')
    require(path.read_bytes() == nonce.encode(), 'guard belongs to another run')
    product = Path('/sys/class/dmi/id/product_name').read_text()
    require('QEMU' in product or 'Standard PC' in product, 'owned QEMU guest required')
    fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        opened = os.fstat(fd)
        require((opened.st_dev, opened.st_ino) == (info.st_dev, info.st_ino), 'guard replaced')
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        require(os.read(fd, 33) == nonce.encode(), 'opened guard contents differ')
        return os.fdopen(fd, 'r+b')
    except BaseException:
        os.close(fd)
        raise


def inventory(timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        devices = {}
        for directory in Path('/sys/bus/usb/devices').iterdir():
            vendor = directory / 'idVendor'
            if not vendor.is_file() or vendor.read_text().strip() != 'cafe':
                continue
            pid = int((directory / 'idProduct').read_text().strip(), 16)
            if pid in (2, 3):
                require(pid not in devices, 'ambiguous synthetic USB identity')
                serial = (directory / 'serial').read_text().strip()
                validate_identity(pid, serial, str(directory.resolve()),
                                  capacity=CAPACITY if pid == 2 else None)
                devices[pid] = directory.resolve()
        if len(devices) == 2:
            for block in Path('/sys/class/block').iterdir():
                if block.resolve().is_relative_to(devices[2]) and not (block / 'partition').exists():
                    return devices, block
        time.sleep(.1)
    raise RuntimeError('synthetic RAM disk and CCID did not enumerate')


def mounted_children(block):
    children = {entry.joinpath('dev').read_text().strip()
                for entry in Path('/sys/class/block').iterdir()
                if entry.resolve().is_relative_to(block.resolve())}
    require(bool(children), 'block identity disappeared')
    mounted = {line.split()[2] for line in Path('/proc/self/mountinfo').read_text().splitlines()}
    return bool(children & mounted)


@contextmanager
def open_ram_disk(usb, block, writable):
    require(re.fullmatch(r'sd[a-z]+', block.name) is not None, 'refuse non-USB disk name')
    validate_identity(2, (usb / 'serial').read_text().strip(), str(block.resolve()),
                      int((block / 'size').read_text()) * 512, mounted_children(block))
    major, minor = map(int, (block / 'dev').read_text().strip().split(':'))
    path = Path('/dev') / block.name
    fd = os.open(path, (os.O_RDWR if writable else os.O_RDONLY)
                 | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_DIRECT)
    try:
        info = os.fstat(fd)
        require(stat.S_ISBLK(info.st_mode) and info.st_rdev == os.makedev(major, minor),
                'opened disk identity differs')
        size = array.array('Q', [0])
        fcntl.ioctl(fd, 0x80081272, size, True)  # BLKGETSIZE64, owned x86_64 guest.
        validate_identity(2, (usb / 'serial').read_text().strip(), str(block.resolve()),
                          size[0], mounted_children(block))
        yield fd, size[0]
    finally:
        os.close(fd)


def disk_work(usb, block, round_index):
    with open_ram_disk(usb, block, True) as (fd, capacity), mmap.mmap(-1, max(SIZES)) as buffer:
        results = []
        for index, length in enumerate(SIZES):
            # Reserved data area, disjoint from the FAT16 fixture metadata.
            offset = 8 * 1024 * 1024 + index * 256 * 1024
            pattern = bytes((n * 37 + round_index * 53 + index) % 256 for n in range(length))
            buffer[:length] = pattern
            view = memoryview(buffer)[:length]
            try:
                require(not mounted_children(block), 'fixture mounted during test; no further write')
                started = time.monotonic()
                progress('storage_write_start', bytes=length)
                require(os.pwrite(fd, view, offset) == length, 'short direct RAM write')
                os.fdatasync(fd)
                progress('storage_write_done', bytes=length)
                buffer[:length] = b'\0' * length
                require(os.preadv(fd, [view], offset) == length, 'short direct RAM read')
                digest = hashlib.sha256(view).hexdigest()
                expected = hashlib.sha256(pattern).hexdigest()
                require(digest == expected, 'direct RAM disk readback differs')
                progress('storage_readback_verified', bytes=length)
                results.append({'bytes': length, 'offset': offset, 'sha256': digest,
                                'seconds': round(time.monotonic() - started, 4)})
            finally:
                view.release()
        return {'direct_io': True, 'capacity': capacity, 'bytes_written': sum(SIZES),
                'ranges': results, 'mounts': False}


def fault_disk_work(usb, block, mode):
    require(mode in ('write', 'readback'), 'invalid commit-fault operation')
    length, offset = 4096, 16 * 1024 * 1024
    pattern = bytes((n * 43 + 17) % 256 for n in range(length))
    with open_ram_disk(usb, block, mode == 'write') as (fd, capacity), \
            mmap.mmap(-1, length) as buffer:
        view = memoryview(buffer)
        try:
            require(os.preadv(fd, [view], offset) == length, 'short fault range read')
            if mode == 'readback':
                require(bytes(view) == pattern, 'committed data changed after reconnect')
                return {'mode': mode, 'sha256': hashlib.sha256(view).hexdigest(),
                        'bytes': length, 'offset': offset, 'capacity': capacity, 'direct_io': True}
            require(bytes(view) == bytes(length), 'reserved fault range was not initially empty')
            buffer[:] = pattern
            require(not mounted_children(block), 'RAM fixture mounted before fault write')
            progress('fault_write_start', bytes=length, offset=offset)
            started = time.monotonic()
            try:
                count = os.pwrite(fd, view, offset)
            except OSError as error:
                return {'mode': mode, 'write_result': 'error', 'errno': error.errno,
                        'seconds': round(time.monotonic() - started, 4),
                        'attempted_bytes': length, 'offset': offset, 'direct_io': True,
                        'expected_sha256': hashlib.sha256(pattern).hexdigest()}
            raise RuntimeError('lost-ack write unexpectedly returned success: ' + str(count))
        finally:
            view.release()


class Bulk(ctypes.Structure):
    _fields_ = [('ep', ctypes.c_uint), ('length', ctypes.c_uint),
                ('timeout', ctypes.c_uint), ('data', ctypes.c_void_p)]


def card_work(usb, round_index):
    require(ctypes.sizeof(Bulk) == 24, 'only known x86_64 guest USB ABI')
    validate_identity(3, (usb / 'serial').read_text().strip(), str(usb))
    bus, number = [int((usb / name).read_text()) for name in ('busnum', 'devnum')]
    major, minor = map(int, (usb / 'dev').read_text().strip().split(':'))
    fd = os.open(f'/dev/bus/usb/{bus:03}/{number:03}', os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW)
    claimed = False
    library = ctypes.CDLL(None, use_errno=True)
    library.ioctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_void_p]
    library.ioctl.restype = ctypes.c_int
    try:
        info = os.fstat(fd)
        require(stat.S_ISCHR(info.st_mode) and info.st_rdev == os.makedev(major, minor),
                'opened CCID device identity differs')
        interface = array.array('I', [0])
        fcntl.ioctl(fd, 0x8004550f, interface, True)  # USBDEVFS_CLAIMINTERFACE.
        claimed = True

        def transfer(endpoint, data=None):
            buffer = ctypes.create_string_buffer(data, len(data)) if data is not None else ctypes.create_string_buffer(4096)
            operation = Bulk(endpoint, len(buffer), 30000, ctypes.addressof(buffer))
            result = library.ioctl(fd, 0xc0185502, ctypes.byref(operation))
            if result < 0:
                raise OSError(ctypes.get_errno(), 'owned synthetic CCID bulk transfer')
            require(result <= len(buffer), 'CCID returned oversized data')
            if data is not None:
                require(result == len(data), 'short CCID write')
                return None
            return buffer.raw[:result]

        def exchange(kind, sequence, payload, expected_kind, expected_data, status=0):
            request = bytes([kind]) + struct.pack('<I', len(payload)) + bytes([0, sequence, 0, 0, 0]) + payload
            started = time.monotonic()
            transfer(2, request)
            reply = transfer(0x82)
            expected = bytes([expected_kind]) + struct.pack('<I', len(expected_data)) + bytes([0, sequence, status, 0, 0]) + expected_data
            require(reply == expected, 'CCID sequence, status or payload differs')
            return round(time.monotonic() - started, 4)

        exchange(0x62, 0, b'', 0x80, b'\x3b\0')
        exchange(0x65, 1, b'', 0x81, b'')
        results = []
        for index, length in enumerate((0, 1, 31, 255, 512, 1023, 4084) * 2):
            sequence = index + 2
            payload = bytes((n * 13 + sequence + round_index * 7) % 256 for n in range(length))
            elapsed = exchange(0x6f, sequence, payload, 0x80, payload + b'\x90\0')
            results.append({'sequence': sequence, 'bytes': length, 'seconds': elapsed,
                            'payload_sha256': hashlib.sha256(payload).hexdigest()})
        exchange(0x63, 31, b'', 0x81, b'', status=1)
        return {'exchanges': results, 'commands': 17, 'cryptography': False}
    finally:
        try:
            if claimed:
                fcntl.ioctl(fd, 0x80045510, array.array('I', [0]), True)
        finally:
            os.close(fd)


def run_owned(round_index, minimum_seconds, work_seconds=170, enumeration_seconds=110):
    require(round_index in (0, 1) and 10 <= minimum_seconds <= 90, 'invalid mixed load budget')
    require(170 <= work_seconds <= 430, 'invalid mixed workload deadline')
    require(110 <= enumeration_seconds <= 180, 'invalid enumeration deadline')
    progress('enumeration_start', round=round_index)
    devices, block = inventory(enumeration_seconds)
    progress('enumeration_complete', round=round_index)
    mouse = []
    for entry in Path('/sys/class/input').glob('event*'):
        if (entry / 'device/name').read_text().strip() == 'USB Most Lab USB Most Synthetic Mouse':
            require('/vhci_hcd.' in str(entry.resolve()), 'mouse is not a VHCI fixture')
            mouse.append((Path('/dev/input') / entry.name, entry))
    require(len(mouse) == 1, 'synthetic mouse missing or ambiguous')
    major, minor = map(int, (mouse[0][1] / 'dev').read_text().strip().split(':'))
    fd = os.open(mouse[0][0], os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_NOFOLLOW)
    workers, results, errors = [], {}, []
    counts, values, arrivals = collections.Counter(), collections.defaultdict(set), []
    started = time.monotonic()

    def work(name, operation):
        try:
            results[name] = operation()
            progress('worker_complete', worker=name)
        except BaseException as error:
            errors.append({'worker': name, 'error': type(error).__name__, 'message': str(error)})
            progress('worker_failed', worker=name, error_type=type(error).__name__)

    try:
        info = os.fstat(fd)
        require(stat.S_ISCHR(info.st_mode) and info.st_rdev == os.makedev(major, minor),
                'opened mouse identity differs')
        fcntl.ioctl(fd, 0x40044590, 1)  # EVIOCGRAB; only the identified synthetic mouse.
        for name, operation in [('storage', lambda: disk_work(devices[2], block, round_index)),
                                ('ccid', lambda: card_work(devices[3], round_index))]:
            thread = threading.Thread(target=work, args=(name, operation))
            thread.start()
            workers.append(thread)
        deadline = started + work_seconds
        while any(worker.is_alive() for worker in workers) or time.monotonic() - started < minimum_seconds:
            require(time.monotonic() < deadline, 'mixed guest workload deadline exceeded')
            ready, _, _ = select.select([fd], [], [], .2)
            if not ready:
                continue
            data = os.read(fd, 24 * 128)
            require(len(data) % 24 == 0, 'partial Linux input event')
            now = time.monotonic()
            for _sec, _usec, kind, code, value in struct.iter_unpack('llHHi', data):
                require(not (kind == 0 and code == 3), 'SYN_DROPPED during mixed load')
                if kind == 0 and code == 0:
                    arrivals.append(now)
                if kind in (1, 2):
                    counts[f'{kind}:{code}'] += 1
                    values[f'{kind}:{code}'].add(value)
        for worker in workers:
            worker.join()
        require(not errors, 'mixed worker failed: ' + json.dumps(errors))
        for code in (272, 273, 274, 275, 276):
            require(values[f'1:{code}'] == {0, 1}, f'missing mouse button transition {code}')
        for code in (0, 1, 8):
            require(values[f'2:{code}'] == {-1, 1}, f'missing mouse direction {code}')
        require(len(arrivals) >= 12, 'mouse starved during mixed load')
        gaps = sorted(b - a for a, b in zip(arrivals, arrivals[1:]))
        return {'counts': dict(counts), 'values': {k: sorted(v) for k, v in values.items()},
                'sync_reports': len(arrivals), 'elapsed_seconds': round(time.monotonic() - started, 3),
                'mouse_gap_seconds': {'max': round(max(gaps), 4),
                                      'p95': round(gaps[min(len(gaps) - 1, int(len(gaps) * .95))], 4)},
                'gap_scope': 'arrival intervals, not one-way latency', **results}
    finally:
        os.close(fd)
        # Retain the run's ownership lock until every operation has returned.
        # On transport failure the host stops only this fixture, then waits for
        # this lock; it must retain evidence if the guest still has active I/O.
        for worker in workers:
            worker.join()


def run(path, nonce, round_index, minimum_seconds, work_seconds=170, enumeration_seconds=110):
    with guard(path, nonce):
        return run_owned(round_index, minimum_seconds, work_seconds, enumeration_seconds)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--guard', type=Path, required=True)
    parser.add_argument('--nonce', required=True)
    parser.add_argument('--round', type=int, required=True)
    parser.add_argument('--seconds', type=int, required=True)
    parser.add_argument('--work-seconds', type=int, default=170)
    parser.add_argument('--enumeration-seconds', type=int, default=110)
    parser.add_argument('--fault', choices=('write', 'readback'))
    args = parser.parse_args()
    if args.fault:
        with guard(args.guard, args.nonce):
            devices, block = inventory(args.enumeration_seconds)
            result = fault_disk_work(devices[2], block, args.fault)
    else:
        result = run(args.guard, args.nonce, args.round, args.seconds,
                     args.work_seconds, args.enumeration_seconds)
    print(json.dumps(result), flush=True)
