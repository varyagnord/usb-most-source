"""No guest or Android mutation: validate prerequisite evidence fail-closed."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'missing_driver', Path(__file__).resolve().parents[1] / 'tools/lab_missing_driver.py')
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)


class MissingDriver(unittest.TestCase):
    def test_raw_audit_no_match_differs_from_read_failure(self):
        for stderr in ('', '<no matches>\n'):
            self.assertEqual(observer.avc_summary(dict(exit=1, stdout='', stderr=stderr))['records'], 0)
        self.assertEqual(observer.avc_summary(dict(exit=0, stdout='type=AVC synthetic\n', stderr=''))['records'], 1)
        for result in [dict(exit=1, stdout='', stderr='permission denied'),
                       dict(exit=2, stdout='', stderr=''),
                       dict(exit=0, stdout='', stderr=''),
                       dict(exit=1, stdout='unexpected', stderr='')]:
            with self.subTest(result=result), self.assertRaises(RuntimeError):
                observer.avc_summary(result)

    def valid(self):
        return dict(selinux=dict(exit=0, stdout='Enforcing\n', stderr=''),
                    module=dict(exit=1), module_loaded=False,
                    receiver_paths=dict.fromkeys(observer.RECEIVER_PATHS, False),
                    service=dict(exit=0, stdout='MainPID=0\nLoadState=not-found\nActiveState=inactive\n'),
                    audit=dict(exit=0, stdout='enabled 1\nlost 0\nbacklog 2\n'))

    def test_exact_unavailable_fixture(self):
        observer.validate(self.valid())

    def test_available_module_or_existing_installation_refused(self):
        variants = []
        for key, value in [('module', dict(exit=0)), ('module_loaded', True)]:
            state = self.valid(); state[key] = value; variants.append(state)
        for path in observer.RECEIVER_PATHS:
            state = self.valid(); state['receiver_paths'][path] = True; variants.append(state)
        state = self.valid(); state['receiver_paths'] = {}; variants.append(state)
        state = self.valid(); state['service']['stdout'] = 'MainPID=42\nLoadState=loaded\nActiveState=active\n'; variants.append(state)
        for state in variants:
            with self.subTest(state=state), self.assertRaises(RuntimeError):
                observer.validate(state)

    def test_disabled_selinux_and_lost_audit_do_not_pass(self):
        for key, value in [('selinux', dict(exit=0, stdout='Permissive\n', stderr='')),
                           ('audit', dict(exit=0, stdout='enabled 0\nlost 0\n')),
                           ('audit', dict(exit=0, stdout='enabled 1\nlost 1\n')),
                           ('audit', dict(exit=1, stdout='enabled 1\nlost 0\n'))]:
            state = self.valid(); state[key] = value
            with self.subTest(key=key, value=value), self.assertRaises(RuntimeError):
                observer.validate(state)

    def test_nonroot_observer_never_invokes_commands(self):
        with patch.object(observer.os, 'geteuid', return_value=1000), \
                patch.object(observer, 'command', side_effect=AssertionError('unexpected command')), \
                self.assertRaisesRegex(RuntimeError, 'guest root'):
            observer.snapshot()


if __name__ == '__main__':
    unittest.main()
