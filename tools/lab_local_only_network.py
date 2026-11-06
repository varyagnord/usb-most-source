"""Internet isolation inside an explicitly owned Android emulator, never on the host.

Keep loopback and the emulator's private IPv4 LAN. An exclusive firewall chain
rejects other IPv4/IPv6 destinations, including validation probes. Do not alter
global host routing, Tailscale, physical Android devices or existing chains.
"""
import json
import re
import secrets
import subprocess
import time


class OwnedLocalNetwork:
    def __init__(self, adb, output):
        if len(adb) != 3 or adb[1] != '-s' or not re.fullmatch(r'emulator-\d+', adb[2]) or adb[2] == 'emulator-5566':
            raise ValueError('explicit unreserved emulator serial required')
        self.adb, self.output = adb, output
        self.chain = 'UMOST_LAN_' + secrets.token_hex(4)
        self.tables, self.linked, self.expected = [], [], {}
        self.network_changed = False
        self.report = {'scope': 'owned emulator firewall only; no VPN or physical LAN claim',
                       'chain': self.chain, 'samples': [], 'cleanup_errors': []}

    def call(self, *args):
        return subprocess.check_output(self.adb + list(args), text=True,
                                       stderr=subprocess.STDOUT, timeout=30).strip()

    def save(self):
        self.output.write_text(json.dumps(self.report, indent=2) + '\n')

    def privileged(self, *args):
        # Keep adbd as shell: root adbd can create external-storage fixtures
        # invisible to the target app. Only this owned lab OS command is root.
        return self.call('shell', 'su', '0', *args)

    def radio(self, kind, state):
        if kind not in ('wifi', 'data') or state not in ('enable', 'disable'):
            raise ValueError('unsupported lab radio command')
        # API26 shell lacks CHANGE_WIFI_STATE; do not change adbd ownership.
        if self.report['api'] <= 26:
            return self.privileged('svc', kind, state)
        return self.call('shell', 'svc', kind, state)

    def state(self, label):
        text = self.call('shell', 'dumpsys', 'connectivity')
        lines = [line for line in text.splitlines() if 'NetworkAgentInfo{' in line]
        wifi = [line for line in lines if re.search(r'Transports: WIFI(?:[ |])', line)]
        default = re.search(r'^Active default network: (\d+)', text, re.M)
        state = {'label': label, 'wifi_networks': len(wifi),
                 'validated_networks': sum(bool(re.search(r'\bVALIDATED\b', line)) for line in lines),
                 'default_is_wifi': bool(default and any('network{' + default[1] + '}' in line for line in wifi)),
                 'mobile_data': self.call('shell', 'settings', 'get', 'global', 'mobile_data'),
                 'wifi_on': self.call('shell', 'settings', 'get', 'global', 'wifi_on')}
        self.report['samples'].append(state)
        self.save()
        return state

    def assert_isolated(self, label):
        for table in self.tables:
            if self.privileged(table, '-S', self.chain) != self.expected[table]:
                raise RuntimeError('owned firewall changed')
            self.privileged(table, '-C', 'OUTPUT', '-j', self.chain)
        state = self.state(label)
        if state['wifi_networks'] != 1 or not state['default_is_wifi']:
            raise RuntimeError('local Wi-Fi not default')
        if state['validated_networks'] != 0 or state['mobile_data'] != '0':
            raise RuntimeError('internet isolation not observed')

    def prepare(self):
        if self.call('shell', 'getprop', 'ro.kernel.qemu') != '1':
            raise RuntimeError('owned emulator required')
        self.report['api'] = int(self.call('shell', 'getprop', 'ro.build.version.sdk'))
        self.report['original_uid'] = self.call('shell', 'id', '-u')
        self.report['original_wifi'] = self.call('shell', 'settings', 'get', 'global', 'wifi_on')
        self.report['original_mobile'] = self.call('shell', 'settings', 'get', 'global', 'mobile_data')
        if self.report['original_wifi'] != '1' or self.report['original_mobile'] != '1':
            raise RuntimeError('fixture requires both radios enabled')
        self.save()
        if self.report['original_uid'] != '2000':
            raise RuntimeError('keep normal shell adbd for app fixtures')
        if self.privileged('id', '-u') != '0':
            raise RuntimeError('owned emulator must support scoped root commands')
        for table in ('iptables', 'ip6tables'):
            self.privileged(table, '-N', self.chain)
            self.tables.append(table)
            self.expected[table] = self.privileged(table, '-S', self.chain)
            rules = [['-o', 'lo', '-j', 'RETURN']]
            if table == 'iptables':
                rules.append(['-d', '10.0.2.0/24', '-j', 'RETURN'])
            rules.append(['-j', 'REJECT'])
            for rule in rules:
                self.privileged(table, '-A', self.chain, *rule)
                self.expected[table] = self.privileged(table, '-S', self.chain)
            self.privileged(table, '-I', 'OUTPUT', '1', '-j', self.chain)
            self.linked.append(table)
        self.network_changed = True
        self.radio('data', 'disable')
        self.radio('wifi', 'disable')
        time.sleep(2)
        self.radio('wifi', 'enable')
        until = time.monotonic() + 60
        while time.monotonic() < until:
            state = self.state('preparing')
            if state['wifi_networks'] == 1 and state['default_is_wifi'] and state['validated_networks'] == 0:
                self.assert_isolated('ready')
                self.report['prepared'] = True
                self.save()
                return
            time.sleep(1)
        raise RuntimeError('owned Wi-Fi without validated internet was not established')

    def close(self):
        errors = self.report['cleanup_errors']
        for table in reversed(self.tables):
            try:
                if self.privileged(table, '-S', self.chain) != self.expected[table]:
                    raise RuntimeError('owned chain changed; retained')
                self.report[table + '_counters'] = self.privileged(table, '-nvx', '-L', self.chain)
                if table in self.linked:
                    self.privileged(table, '-D', 'OUTPUT', '-j', self.chain)
                self.privileged(table, '-F', self.chain)
                self.privileged(table, '-X', self.chain)
            except Exception as error:
                errors.append({'step': table, 'error': type(error).__name__})
        if self.network_changed:
            for kind in ('wifi', 'data'):
                try:
                    self.radio(kind, 'enable')
                except Exception as error:
                    errors.append({'step': 'restore ' + kind, 'error': type(error).__name__})
            try:
                until = time.monotonic() + 60
                while time.monotonic() < until:
                    state = self.state('restoring')
                    if state['wifi_on'] == state['mobile_data'] == '1' and state['default_is_wifi']:
                        break
                    time.sleep(1)
                else:
                    raise RuntimeError('original emulator network not restored')
            except Exception as error:
                errors.append({'step': 'network', 'error': type(error).__name__})
        try:
            if self.call('shell', 'id', '-u') != self.report['original_uid']:
                raise RuntimeError('adb identity changed')
        except Exception as error:
            errors.append({'step': 'adb identity', 'error': type(error).__name__})
        self.report['cleanup_passed'] = not errors
        self.save()
        if errors:
            raise RuntimeError('owned local-network cleanup failed; inspect retained report')
