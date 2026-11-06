"""RAM workload fail-closed guards; never enumerate or open a USB/block device."""
import importlib.util
import ast
from contextlib import nullcontext
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'mixed_reader', Path(__file__).resolve().parents[1] / 'tools/lab_mixed_usb_reader.py')
reader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)


class MixedSafety(unittest.TestCase):
    def test_failed_fault_write_is_attempted_once_without_replay(self):
        with patch.object(reader, 'open_ram_disk', return_value=nullcontext((3, reader.CAPACITY))), \
                patch.object(reader, 'mounted_children', return_value=False), \
                patch.object(reader.os, 'preadv', return_value=4096), \
                patch.object(reader.os, 'pwrite', side_effect=OSError(5, 'lost acknowledgement')) as write:
            result = reader.fault_disk_work(None, None, 'write')
        self.assertEqual(result['errno'], 5)
        self.assertEqual(result['write_result'], 'error')
        self.assertEqual(write.call_count, 1)

    def test_successful_write_cannot_pass_the_lost_ack_scenario(self):
        with patch.object(reader, 'open_ram_disk', return_value=nullcontext((3, reader.CAPACITY))), \
                patch.object(reader, 'mounted_children', return_value=False), \
                patch.object(reader.os, 'preadv', return_value=4096), \
                patch.object(reader.os, 'pwrite', return_value=4096), \
                self.assertRaisesRegex(RuntimeError, 'unexpectedly returned success'):
            reader.fault_disk_work(None, None, 'write')

    def test_fault_readback_requires_exact_pattern_and_never_writes(self):
        expected = bytes((n * 43 + 17) % 256 for n in range(4096))
        for data, passed in [(bytes(4096), False), (expected, True)]:
            def read(_fd, views, _offset):
                views[0][:] = data
                return len(data)
            with patch.object(reader, 'open_ram_disk', return_value=nullcontext((3, reader.CAPACITY))), \
                    patch.object(reader.os, 'preadv', side_effect=read), \
                    patch.object(reader.os, 'pwrite', side_effect=AssertionError('readback must not write')):
                if passed:
                    self.assertEqual(reader.fault_disk_work(None, None, 'readback')['bytes'], 4096)
                else:
                    with self.assertRaisesRegex(RuntimeError, 'committed data changed'):
                        reader.fault_disk_work(None, None, 'readback')

    def test_exact_ram_and_ccid_identities(self):
        reader.validate_identity(2, 'USBMOSTEJECT113', '/sys/devices/platform/vhci_hcd.0/usb1',
                                 reader.CAPACITY)
        reader.validate_identity(3, 'USBMOSTCCID113', '/sys/devices/platform/vhci_hcd.0/usb1')

    def test_physical_wrong_serial_wrong_pid_and_capacity_fail_closed(self):
        for args in [(2, 'USBMOSTEJECT113', '/sys/devices/pci/usb1', reader.CAPACITY),
                     (2, 'OTHER', '/sys/vhci_hcd.0/usb1', reader.CAPACITY),
                     (3, 'USBMOSTEJECT113', '/sys/vhci_hcd.0/usb1', None),
                     (4, 'USBMOSTCCID113', '/sys/vhci_hcd.0/usb1', None),
                     (2, 'USBMOSTEJECT113', '/sys/vhci_hcd.0/usb1', reader.CAPACITY * 2)]:
            with self.subTest(args=args), self.assertRaises(RuntimeError):
                reader.validate_identity(*args)

    def test_mounted_ram_is_never_writable(self):
        with self.assertRaises(RuntimeError):
            reader.validate_identity(2, 'USBMOSTEJECT113', '/sys/vhci_hcd.0/usb1',
                                     reader.CAPACITY, mounted=True)

    def test_guard_rejects_nonroot_without_filesystem_access(self):
        with patch.object(reader.os, 'geteuid', return_value=1000), \
                patch.object(Path, 'lstat', side_effect=AssertionError('unexpected filesystem read')):
            with self.assertRaises(RuntimeError):
                reader.guard(Path('/run/usb-most-mixed-' + 'a' * 32), 'a' * 32)

    def test_guard_rejects_wrong_nonce_or_path_before_filesystem_access(self):
        with patch.object(reader.os, 'geteuid', return_value=0), \
                patch.object(Path, 'lstat', side_effect=AssertionError('unexpected filesystem read')):
            for path, nonce in [(Path('/run/other'), 'a' * 32),
                                (Path('/run/usb-most-mixed-a'), 'a')]:
                with self.subTest(path=path), self.assertRaises(RuntimeError):
                    reader.guard(path, nonce)

    def test_invalid_round_and_budget_do_not_enumerate(self):
        with patch.object(reader, 'inventory', side_effect=AssertionError('unexpected enumeration')):
            for round_index, seconds in [(-1, 10), (2, 10), (0, 9), (1, 91)]:
                with self.subTest(round=round_index, seconds=seconds), self.assertRaises(RuntimeError):
                    reader.run_owned(round_index, seconds)
            for deadline in [169, 431]:
                with self.subTest(deadline=deadline), self.assertRaises(RuntimeError):
                    reader.run_owned(0, 10, deadline)
            for enumeration in [109, 181]:
                with self.subTest(enumeration=enumeration), self.assertRaises(RuntimeError):
                    reader.run_owned(0, 10, 170, enumeration)

    def publish_helper(self, android):
        source = Path(__file__).resolve().parents[1] / 'tools/run-android-usb-transport-e2e.py'
        tree = ast.parse(source.read_text())
        helper = next(n for n in tree.body if isinstance(n, ast.FunctionDef)
                      and n.name == 'complete_mixed_round')

        class HostMarker:
            def __truediv__(self, _name):
                return self

            def write_bytes(self, data):
                if data != b'\x01':
                    raise AssertionError('invalid marker payload')

            def __str__(self):
                return 'owned-host-marker'

        namespace = {'Path': Path, 'output': HostMarker(), 'android': android,
                     'remote_fixture': '/owned-test/transport-fixture.json',
                     'release_marker': 'a' * 32}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), 'exec'), namespace)
        return namespace['complete_mixed_round']

    def test_partial_adb_push_is_invisible_to_android_consumer(self):
        files = {}

        def android(*args):
            if args[0] == 'push':
                files[args[2]] = b''
                # A consumer can run at any instant during adb push.
                self.assertFalse(any(name.endswith('.go') for name in files))
                files[args[2]] = b'\x01'
            elif args[:2] == ('shell', 'mv'):
                self.assertEqual(files[args[2]], b'\x01')
                files[args[3]] = files.pop(args[2])
            else:
                self.fail('unexpected remote mutation')

        self.publish_helper(android)(0)
        self.assertEqual(files, {'/owned-test/mixed-done-' + 'a' * 32 + '-0.go': b'\x01'})

    def test_failed_push_never_publishes_completion(self):
        calls = []

        def android(*args):
            calls.append(args)
            raise OSError('simulated failed upload')

        with self.assertRaises(OSError):
            self.publish_helper(android)(1)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], 'push')
        self.assertTrue(calls[0][-1].endswith('.part'))


if __name__ == '__main__':
    unittest.main()
