"""Fault-path guarantees for the emulator-only firewall fixture; no OS mutation."""
import importlib.util
import ast
from pathlib import Path
import subprocess
import unittest
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location(
    'lab_local_only_network', Path(__file__).resolve().parents[1] / 'tools/lab_local_only_network.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class Report:
    def write_text(self, value):
        self.value = value


class Fixture(module.OwnedLocalNetwork):
    def __init__(self):
        super().__init__(['adb', '-s', 'emulator-5558'], Report())
        self.commands, self.rules, self.radios = [], {}, []
        self.fail_wifi = False
        self.report.update(api=31, original_uid='2000')

    def call(self, *args):
        if args[:2] == ('shell', 'getprop'):
            return '1' if args[2] == 'ro.kernel.qemu' else '31'
        if args == ('shell', 'id', '-u'):
            return '2000'
        if args[:4] == ('shell', 'settings', 'get', 'global'):
            return '1'
        raise AssertionError('unexpected shell command')

    def privileged(self, *args):
        self.commands.append(args)
        if args == ('id', '-u'):
            return '0'
        table, op = args[:2]
        if op == '-N':
            if table in self.rules:
                raise subprocess.CalledProcessError(1, args)
            self.rules[table] = 'owned'
        elif op == '-S':
            return self.rules[table]
        elif op == '-A':
            self.rules[table] += repr(args[3:])
        elif op == '-nvx':
            return 'fixture counters'
        elif op == '-X':
            del self.rules[table]
        elif op not in ('-I', '-C', '-D', '-F'):
            raise AssertionError('unexpected firewall command')
        return ''

    def radio(self, kind, state):
        self.radios.append((kind, state))
        if kind == 'wifi' and self.fail_wifi:
            raise subprocess.CalledProcessError(137, ['svc', kind, state])

    def state(self, label):
        return {'wifi_on': '1', 'mobile_data': '1', 'default_is_wifi': True}


class LocalNetworkSafety(unittest.TestCase):
    def test_physical_serial_is_rejected_before_any_command(self):
        with self.assertRaises(ValueError):
            module.OwnedLocalNetwork(['adb', '-s', 'physical-device'], Report())

    def test_protected_soak_emulator_is_rejected(self):
        with self.assertRaises(ValueError):
            module.OwnedLocalNetwork(['adb', '-s', 'emulator-5566'], Report())

    def test_second_chain_collision_preserves_foreign_chain(self):
        fixture = Fixture()
        fixture.rules['ip6tables'] = 'foreign existing chain'
        with self.assertRaises(subprocess.CalledProcessError):
            fixture.prepare()
        fixture.close()
        self.assertEqual(fixture.rules, {'ip6tables': 'foreign existing chain'})
        self.assertFalse(any(cmd[0] == 'ip6tables' and cmd[1] in ('-D', '-F', '-X')
                             for cmd in fixture.commands))
        self.assertEqual(fixture.radios, [])

    def test_changed_owned_chain_is_retained_without_mutation(self):
        fixture = Fixture()
        fixture.tables = fixture.linked = ['iptables']
        fixture.expected['iptables'] = 'owned'
        fixture.rules['iptables'] = 'changed by another actor'
        with self.assertRaises(RuntimeError):
            fixture.close()
        self.assertEqual(fixture.rules['iptables'], 'changed by another actor')
        self.assertFalse(any(cmd[1] in ('-D', '-F', '-X') for cmd in fixture.commands))

    def test_wifi_restore_failure_does_not_skip_mobile_restore(self):
        fixture = Fixture()
        fixture.network_changed = fixture.fail_wifi = True
        with self.assertRaises(RuntimeError):
            fixture.close()
        self.assertEqual(fixture.radios, [('wifi', 'enable'), ('data', 'enable')])
        self.assertFalse(fixture.report['cleanup_passed'])
        self.assertEqual(fixture.report['cleanup_errors'][0]['step'], 'restore wifi')

    def test_actual_ordinary_transport_loop_records_both_timestamps(self):
        source = Path(__file__).resolve().parents[1] / 'tools/run-android-usb-transport-e2e.py'
        tree = ast.parse(source.read_text())
        phase = next(node for node in ast.walk(tree)
                     if isinstance(node, ast.FunctionDef) and node.name == 'phase')
        loop = next(node for node in ast.walk(tree) if isinstance(node, ast.For)
                    and any(isinstance(child, ast.Constant) and isinstance(child.value, str)
                            and child.value.startswith('TRANSPORT_CONNECTED round=')
                            for child in ast.walk(node)))

        class Output:
            def __truediv__(self, _name):
                return self

            def read_text(self):
                return 'TRANSPORT_CONNECTED round=0\nTRANSPORT_CONNECTED round=1'

        windows, phases = [], []
        namespace = {'time': SimpleNamespace(monotonic=lambda: 12.3456), 'began': 10,
                     'a': SimpleNamespace(mixed=False),
                     'output': Output(), 'observed_phases': phases,
                     'input_window': windows.append, 'write_json': lambda *_: None}
        # Execute only the actual phase helper and ordinary loop, with no adb,
        # credentials, network, subprocess or filesystem operation available.
        exec(compile(ast.Module(body=[phase, loop], type_ignores=[]), str(source), 'exec'), namespace)
        self.assertEqual(windows, [0, 1])
        self.assertEqual([item['elapsed_seconds'] for item in phases], [2.346, 2.346])


if __name__ == '__main__':
    unittest.main()
