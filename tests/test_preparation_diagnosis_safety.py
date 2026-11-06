#!/usr/bin/env python3
"""Fail-closed account/file/serial guards; never create an OS account on the host."""
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import tempfile
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


guest = module('owned_no_sudo', ROOT / 'tools/lab_unprivileged_user.py')
runner = module('preparation_runner', ROOT / 'tools/run-preparation-diagnosis-e2e.py')


class Safety(unittest.TestCase):
    def test_proof_copies_only_current_case_and_rejects_stale_result(self):
        output = Path(tempfile.mkdtemp(prefix='usb-most-proof-'))
        cases = [dict(name='ssh-linux-driver-missing', failure='linux_usb_driver_unavailable')]
        expected = dict(status='passed', cases=[dict(case=cases[0]['name'], status='passed',
                        expected_failure=cases[0]['failure'], observed_failure=cases[0]['failure'])])
        calls = []
        def android(*args):
            calls.append(args)
            if args[1].endswith('result.json'):
                Path(args[2]).write_text(json.dumps(expected))
            else:
                Path(args[2]).write_bytes(b'owned screenshot')
        try:
            self.assertEqual(runner.collect_proof(android, output, cases), expected)
            self.assertEqual([Path(call[1]).name for call in calls],
                             ['result.json', 'ssh-linux-driver-missing.png'])
            expected['cases'][0]['case'] = 'old-case'
            with self.assertRaisesRegex(RuntimeError, 'current requested'):
                runner.collect_proof(android, output, cases)
            self.assertEqual(len(calls), 3)
        finally:
            for name in ('result.json', 'ssh-linux-driver-missing.png'):
                (output / name).unlink(missing_ok=True)
            output.rmdir()

    def test_live_soak_owner_is_protected_and_stale_pid_is_not_identity(self):
        identity = dict(pid=42, start_ticks=100, command_sha256='a' * 64)
        record = json.dumps(dict(serial='emulator-7778', runner_identity=identity))
        with patch.object(Path, 'exists', return_value=True), \
                patch.object(Path, 'read_text', return_value=record):
            with patch.object(runner, 'process_identity', return_value=identity), \
                    self.assertRaisesRegex(ValueError, 'live protected'):
                runner.validate_serial('emulator-7778')
            with patch.object(runner, 'process_identity', return_value=identity | {'start_ticks': 200}):
                runner.validate_serial('emulator-7778')
            with patch.object(runner, 'process_identity', side_effect=FileNotFoundError):
                runner.validate_serial('emulator-7778')

    def test_serial_refuses_physical_and_protected_before_adb(self):
        for serial in ['192.0.2.1:5555', 'physical-serial', 'emulator-5566', 'emulator-5558;id']:
            with self.subTest(serial=serial), self.assertRaises(ValueError):
                runner.validate_serial(serial)
        runner.validate_serial('emulator-5558')

    def test_nonce_cannot_name_other_paths_or_accounts(self):
        for nonce in ['', '../fixture', 'a' * 31, 'a' * 33, 'A' * 32, '0' * 31 + '\n']:
            with self.subTest(nonce=nonce), self.assertRaises(ValueError):
                guest.identity(nonce)

    def test_changed_account_uid_home_or_shell_is_preserved(self):
        nonce = 'a' * 32
        user, root = guest.identity(nonce)
        record = dict(nonce=nonce, uid=1003)
        fields = dict(pw_name=user, pw_uid=1003, pw_dir=str(root / 'home'), pw_shell='/bin/sh')
        guest.validate_account(record, SimpleNamespace(**fields))
        for field, value in [('pw_name', 'fixture'), ('pw_uid', 0), ('pw_uid', 1004),
                             ('pw_dir', '/home/fixture'), ('pw_shell', '/bin/bash')]:
            with self.subTest(field=field, value=value), self.assertRaises(RuntimeError):
                guest.validate_account(record, SimpleNamespace(**(fields | {field: value})))

    def test_symlink_key_is_refused_without_touching_target(self):
        # Only exact regular files in an unmounted host test directory; no recursive cleanup.
        directory = Path(tempfile.mkdtemp(prefix='usb-most-guard-'))
        original, link = directory / 'original', directory / 'link'
        try:
            original.write_bytes(b'keep')
            original.chmod(0o600)
            link.symlink_to(original)
            with self.assertRaises(RuntimeError):
                guest.regular(link, os.getuid(), 0o600)
            self.assertEqual(original.read_bytes(), b'keep')
        finally:
            link.unlink(missing_ok=True)
            original.unlink(missing_ok=True)
            directory.rmdir()

    def test_unexpected_files_and_owner_refused_without_deletion(self):
        directory = Path(tempfile.mkdtemp(prefix='usb-most-guard-'))
        unexpected = directory / 'unexpected'
        try:
            unexpected.write_bytes(b'keep')
            with self.assertRaises(RuntimeError):
                guest.directory(directory, os.getuid(), [])
            with self.assertRaises(RuntimeError):
                guest.regular(unexpected, os.getuid() + 1)
            self.assertEqual(unexpected.read_bytes(), b'keep')
        finally:
            unexpected.unlink()
            directory.rmdir()

    def test_only_empty_regular_pam_marker_may_be_retired(self):
        home = Path(tempfile.mkdtemp(prefix='usb-most-guard-'))
        ssh, cache = home / '.ssh', home / '.cache'
        marker = cache / 'motd.legal-displayed'
        try:
            ssh.mkdir()
            self.assertIsNone(guest.validate_home(home, os.getuid()))
            cache.mkdir()
            marker.write_bytes(b'')
            self.assertEqual(guest.validate_home(home, os.getuid()), marker)
            marker.write_bytes(b'preserve')
            with self.assertRaises(RuntimeError):
                guest.validate_home(home, os.getuid())
            self.assertEqual(marker.read_bytes(), b'preserve')
            marker.unlink()
            marker.symlink_to('/does-not-exist')
            with self.assertRaises(RuntimeError):
                guest.validate_home(home, os.getuid())
            self.assertTrue(marker.is_symlink())
        finally:
            marker.unlink(missing_ok=True)
            cache.rmdir()
            ssh.rmdir()
            home.rmdir()


if __name__ == '__main__':
    unittest.main()
