"""Do not turn an empty/unavailable/lossy audit source into an SELinux pass."""
import copy
import importlib.util
from pathlib import Path
import unittest

path = Path(__file__).resolve().parents[1] / 'tools/linux-compatibility-e2e.py'
spec = importlib.util.spec_from_file_location('compatibility', path)
compatibility = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compatibility)


class AuditEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.valid = {
            'avc': {'exit': 1, 'stdout': '', 'stderr': '<no matches>\n'},
            'audit_status': {'exit': 0, 'stdout': 'enabled 1\npid 831\nlost 0\n', 'stderr': ''},
            'audit_service': {'exit': 0, 'stdout': 'active\n', 'stderr': ''},
        }

    def test_confirmed_log_search_with_live_audit_can_pass(self):
        self.assertTrue(compatibility.audit_clean(self.valid))

    def test_silent_pipe_or_missing_source_cannot_pass(self):
        for avc in ({}, {'unavailable': True}, {'exit': 1, 'stdout': '', 'stderr': ''},
                    {'exit': 1, 'stdout': '', 'stderr': 'Could not open audit log'},
                    {'exit': 0, 'stdout': 'type=AVC denied', 'stderr': ''}):
            with self.subTest(avc=avc):
                value = copy.deepcopy(self.valid)
                value['avc'] = avc
                self.assertFalse(compatibility.audit_clean(value))

    def test_disabled_audit_or_lost_events_cannot_pass(self):
        for status in ('enabled 0\npid 831\nlost 0\n', 'enabled 1\npid 0\nlost 0\n',
                       'enabled 1\npid 831\nlost 1\n', 'enabled 1\npid unknown\nlost 0\n', ''):
            with self.subTest(status=status):
                value = copy.deepcopy(self.valid)
                value['audit_status']['stdout'] = status
                self.assertFalse(compatibility.audit_clean(value))
        self.valid['audit_service']['stdout'] = 'inactive\n'
        self.assertFalse(compatibility.audit_clean(self.valid))


if __name__ == '__main__':
    unittest.main()
