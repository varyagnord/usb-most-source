"""Upgrade fixture ownership/refusal tests; no real receiver/system changes."""
import base64
import copy
import importlib.util
import json
from pathlib import Path
import stat
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'upgrade', Path(__file__).resolve().parents[1] / 'tools/lab_receiver_upgrade.py')
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


class Safety(unittest.TestCase):
    def test_nonce_names_only_new_private_fixture_directory(self):
        self.assertEqual(upgrade.directory('a' * 32), Path('/var/tmp/usb-most-upgrade-' + 'a' * 32))
        for nonce in ('', '../home', 'A' * 32, 'a' * 31, 'a' * 33, 'a' * 32 + '\n'):
            with self.subTest(nonce=nonce), self.assertRaises(RuntimeError):
                upgrade.directory(nonce)

    def test_nonroot_never_reads_host_configuration(self):
        with patch.object(upgrade.os, 'geteuid', return_value=1000), \
                patch.object(Path, 'read_text', side_effect=AssertionError('unexpected read')), \
                self.assertRaisesRegex(RuntimeError, 'guest root'):
            upgrade.run({})

    def test_changed_identity_pairings_or_boot_is_not_success(self):
        before = dict(trust={'identity': {'sha256': 'a'}, 'pairs': {'sha256': 'b'}}, boot_id='boot')
        upgrade.verify_trust(before, copy.deepcopy(before))
        for field in ('identity', 'pairs', 'boot'):
            after = copy.deepcopy(before)
            if field == 'boot': after['boot_id'] = 'different'
            else: after['trust'][field]['sha256'] = 'changed'
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                upgrade.verify_trust(before, after)

    def test_unpinned_old_binary_is_rejected_before_directory_or_install(self):
        with patch.object(upgrade, 'snapshot', return_value={}), \
                patch.object(upgrade, 'regular', return_value=(b'{"transport":"direct"}', {})), \
                patch.object(Path, 'mkdir', side_effect=AssertionError('unexpected mutation')), \
                patch.object(upgrade.subprocess, 'run', side_effect=AssertionError('unexpected execution')), \
                self.assertRaisesRegex(RuntimeError, 'pinned historical'):
            upgrade.prepare(Path('/unused'), dict(current_sha256='a' * 64,
                old_binary=base64.b64encode(b'not the historical receiver').decode()))

    def test_backup_path_injection_is_rejected_before_read(self):
        settings = dict(nonce='a' * 32, current_sha256='b' * 64)
        record = settings | {'backups': {'../foreign': {}}}
        with patch.object(Path, 'lstat', return_value=SimpleNamespace(
                st_mode=stat.S_IFDIR | 0o700, st_uid=0)), \
                patch.object(Path, 'is_mount', return_value=False), \
                patch.object(upgrade, 'regular', return_value=(json.dumps(record).encode(), {})) as read, \
                self.assertRaisesRegex(RuntimeError, 'Unexpected backup path'):
            upgrade.owner(Path('/unused'), settings)
        self.assertEqual(read.call_count, 1)

    def test_unknown_installed_binary_is_never_executed_or_restored_over(self):
        with patch.object(upgrade, 'owner', return_value={}), \
                patch.object(upgrade, 'regular', return_value=(b'', {'sha256': 'c' * 64})), \
                patch.object(upgrade.subprocess, 'check_output', side_effect=AssertionError('unexpected execution')), \
                self.assertRaisesRegex(RuntimeError, 'Unrecognized installed image'):
            upgrade.restore(Path('/unused'), dict(current_sha256='b' * 64))


if __name__ == '__main__':
    unittest.main()
