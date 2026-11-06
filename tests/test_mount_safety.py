import errno
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
from readonly_mount import ReadOnlyMount


class MountSafetyTests(unittest.TestCase):
    def test_busy_unmount_preserves_every_file(self):
        with tempfile.TemporaryDirectory() as parent:
            mount = ReadOnlyMount('test', 'test', parent=parent)
            with patch.object(mount, '_mount'), patch.object(
                    mount, '_unmount', side_effect=OSError(errno.EBUSY, 'busy')):
                path = mount.__enter__()
                sentinel = path / 'existing-user-data'
                sentinel.write_bytes(b'preserve this exactly')
                with self.assertRaises(OSError):
                    mount.close()
                self.assertTrue(mount.mounted)
                self.assertEqual(sentinel.read_bytes(), b'preserve this exactly')

    def test_exception_does_not_turn_cleanup_into_deletion(self):
        with tempfile.TemporaryDirectory() as parent:
            mount = ReadOnlyMount('test', 'test', parent=parent)
            with patch.object(mount, '_mount'), patch.object(
                    mount, '_unmount', side_effect=OSError(errno.EBUSY, 'busy')):
                with self.assertRaises(OSError):
                    with mount as path:
                        (path / 'sentinel').write_bytes(b'original')
                        raise RuntimeError('test aborted')
                self.assertEqual((mount.path / 'sentinel').read_bytes(), b'original')

    def test_successful_unmount_only_removes_empty_directory(self):
        with tempfile.TemporaryDirectory() as parent:
            mount = ReadOnlyMount('test', 'test', parent=parent)
            with patch.object(mount, '_mount'), patch.object(mount, '_unmount') as unmount:
                with mount as path:
                    self.assertTrue(path.is_dir())
                unmount.assert_called_once()
                self.assertFalse(path.exists())

    def test_unexpected_unmounted_data_is_preserved(self):
        with tempfile.TemporaryDirectory() as parent:
            mount = ReadOnlyMount('test', 'test', parent=parent)
            with patch.object(mount, '_mount'), patch.object(mount, '_unmount'):
                path = mount.__enter__()
                (path / 'sentinel').write_bytes(b'original')
                with self.assertRaises(OSError):
                    mount.close()
                self.assertEqual((path / 'sentinel').read_bytes(), b'original')

    def test_mount_failure_does_not_delete_unexpected_data(self):
        with tempfile.TemporaryDirectory() as parent:
            mount = ReadOnlyMount('test', 'test', parent=parent)
            def failing_mount():
                (mount.path / 'sentinel').write_bytes(b'original')
                raise OSError(errno.EIO, 'failed after setup')
            with patch.object(mount, '_mount', side_effect=failing_mount):
                with self.assertRaises(OSError):
                    mount.__enter__()
                self.assertEqual((mount.path / 'sentinel').read_bytes(), b'original')

    def test_repeated_close_cannot_reuse_mount_context(self):
        with tempfile.TemporaryDirectory() as parent:
            mount = ReadOnlyMount('test', 'test', parent=parent)
            with patch.object(mount, '_mount'), patch.object(mount, '_unmount'):
                with mount:
                    pass
                mount.close()
                with self.assertRaises(RuntimeError):
                    mount.__enter__()
