"""Process ownership checks use actual disposable processes, no Android/USB."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_file_location(
    'android_idle_soak', Path(__file__).resolve().parents[1] / 'tools/android-idle-soak.py')
soak = importlib.util.module_from_spec(spec)
spec.loader.exec_module(soak)


class OwnedProcessTests(unittest.TestCase):
    def test_live_same_process_and_terminal_disappearance(self):
        process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            identity = soak.process_identity(process.pid)
            self.assertEqual(soak.require_same_process(identity), identity)
            process.terminate()
            process.wait(timeout=5)
            with self.assertRaises(soak.FixtureUnavailable):
                soak.require_same_process(identity)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)

    def test_same_pid_different_start_or_command_is_rejected(self):
        process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            identity = soak.process_identity(process.pid)
            changed = {**identity, 'start_ticks': identity['start_ticks'] + 1}
            with self.assertRaises(soak.FixtureUnavailable):
                soak.require_same_process(changed)
            changed = {**identity, 'command_sha256': '0' * 64}
            with self.assertRaises(soak.FixtureUnavailable):
                soak.require_same_process(changed)
            self.assertEqual(soak.require_same_process(identity), identity)
        finally:
            process.terminate()
            process.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
