"""Active-soak evidence and bounded owned writer guards; no VM/device mutation."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'tools' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


soak = load('lab_android_active_soak')
storage = load('lab_android_storage')


class Evidence(unittest.TestCase):
    def setUp(self):
        self.previous = dict(pid=123, imports=1, pss_kib=30000, elapsed_ms=10000,
                             completed=100, cpu_ms=300)
        self.sample = self.previous | dict(elapsed_ms=20000, completed=130, cpu_ms=310)

    def test_only_fresh_same_process_same_import_with_usb_progress_passes(self):
        soak.validate_sample(self.sample, 123, self.previous)
        for field, value in [('pid', 124), ('imports', 0), ('imports', 2), ('pss_kib', 0),
                             ('pss_kib', 262144), ('elapsed_ms', 10000),
                             ('completed', 100), ('cpu_ms', 299)]:
            with self.subTest(field=field, value=value), self.assertRaises(RuntimeError):
                soak.validate_sample(self.sample | {field: value}, 123, self.previous)

    def test_stale_android_snapshot_cannot_pass_as_later_sample(self):
        with self.assertRaises(RuntimeError):
            soak.validate_sample(self.previous, 123, self.previous)

    def test_qemu_serial_limit_checked_before_guest_or_image_creation(self):
        self.assertEqual(len(storage.owned_serials('a' * 11)[0]), 20)
        for tag in ['', 'a' * 12, '../fixture', 'with space', 'bad;command']:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                storage.owned_serials(tag)


class Writer(unittest.TestCase):
    def setUp(self):
        # Explicitly owned unmounted regular-file fixture; exact-file cleanup only.
        self.directory = Path(tempfile.mkdtemp(prefix='usb-most-writer-'))
        self.path = self.directory / 'owned.bin'
        self.writer = None

    def tearDown(self):
        if self.writer is not None and self.writer.thread.is_alive():
            self.writer.stop.set()
            self.writer.thread.join(timeout=5)
        if self.writer is not None and self.writer.thread.is_alive():
            raise RuntimeError('Live writer; preserve fixture')
        self.path.unlink(missing_ok=True)
        self.directory.rmdir()

    def test_rotation_keeps_file_bounded_and_readback_matches_after_join(self):
        self.writer = storage.OwnedWriter(self.path, max_operations=64, interval=.025)
        self.assertTrue(self.writer.ready.wait(3))
        until = time.monotonic() + 3
        while self.writer.count() < 6:
            self.assertLess(time.monotonic(), until)
            time.sleep(.01)
        result = self.writer.finish()
        self.assertFalse(self.writer.thread.is_alive())
        self.assertEqual(self.path.stat().st_size, 4 * 65536)
        self.assertGreaterEqual(result['completed_synced_writes'], 6)
        self.assertEqual(result['sha256'], hashlib.sha256(self.path.read_bytes()).hexdigest())

    def test_existing_file_is_preserved_and_failed_writer_reported(self):
        self.path.write_bytes(b'existing owned evidence')
        self.writer = storage.OwnedWriter(self.path)
        self.assertTrue(self.writer.ready.wait(3))
        with self.assertRaises(AssertionError):
            self.writer.finish()
        self.assertEqual(self.path.read_bytes(), b'existing owned evidence')

    def test_invalid_limits_fail_before_file_or_thread_creation(self):
        for kwargs in [dict(max_operations=0), dict(max_operations=30001),
                       dict(max_operations=True), dict(interval=0), dict(interval=2)]:
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                storage.OwnedWriter(self.path, **kwargs)
            self.assertFalse(self.path.exists())


if __name__ == '__main__':
    unittest.main()
