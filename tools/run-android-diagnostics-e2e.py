#!/usr/bin/env python3
"""Owned emulator: real TLS/SSH failures through the foreground service and diagnosis UI.

TLS and plaintext peers listen on host loopback only. SSH uses an already owned
QEMU fixture; no receiver installation, USB import, host routing or user keys.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import threading

ROOT = Path(__file__).resolve().parents[1]
APP = 'ru.levchatkin.rutokenbridge'


def b64(value):
    return base64.urlsafe_b64encode(value).decode().rstrip('=')


class Peer:
    def __init__(self, context=None):
        self.context = context
        self.socket = socket.socket()
        self.socket.bind(('127.0.0.1', 0))
        self.port = self.socket.getsockname()[1]
        self.socket.listen(4)
        self.socket.settimeout(.2)
        self.stop = threading.Event()
        self.accepted = self.established = self.application_bytes = self.rejected = 0
        self.error = None
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            while not self.stop.is_set():
                try:
                    connection, _ = self.socket.accept()
                except socket.timeout:
                    continue
                with connection:
                    connection.settimeout(5)
                    self.accepted += 1
                    try:
                        if self.context:
                            with self.context.wrap_socket(connection, server_side=True) as tls:
                                self.established += 1
                                self.application_bytes += len(tls.recv(1024))
                        else:
                            connection.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n')
                            # Drain just the TLS hello so the client receives the wrong protocol
                            # before close. Never retain or log its bytes.
                            connection.recv(4096)
                    except (ssl.SSLError, ConnectionError, TimeoutError):
                        self.rejected += 1
        except BaseException as error:
            self.error = type(error).__name__

    def close(self):
        self.stop.set()
        self.thread.join(6)
        self.socket.close()
        if self.thread.is_alive() or self.error:
            raise RuntimeError('owned peer cleanup failed: ' + str(self.error))

    def counts(self):
        return dict(accepted=self.accepted, established=self.established,
                    application_bytes=self.application_bytes, rejected=self.rejected)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--adb', type=Path, required=True)
    p.add_argument('--serial', required=True)
    p.add_argument('--apk', type=Path, required=True)
    p.add_argument('--test-apk', type=Path, required=True)
    p.add_argument('--lab', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    assert re.fullmatch(r'emulator-\d+', a.serial) and a.serial != 'emulator-5566'
    lab, output = a.lab.resolve(), a.output.resolve()
    assert lab.is_relative_to(ROOT / 'build/vm-lab')
    assert output.is_relative_to(ROOT / 'build/e2e')
    command = Path('/proc', (lab / 'pid').read_text().strip(), 'cmdline').read_bytes()
    assert b'qemu-system-x86_64' in command and str(lab / 'guest.qcow2').encode() in command
    metadata = json.loads((lab / 'connection.json').read_text())
    assert metadata['ssh_host'] == '127.0.0.1' and metadata['ssh_user'] == 'fixture'
    key = lab / 'client'
    assert key.is_file() and not key.is_symlink() and 0 < key.stat().st_size <= 16384
    pins = [b64(hashlib.sha256(base64.b64decode(line.split()[2])).digest())
            for line in (lab / 'known_hosts').read_text().splitlines()
            if len(line.split()) >= 3 and line.split()[1] == 'ssh-ed25519']
    assert len(pins) == 1
    adb = [str(a.adb.resolve()), '-s', a.serial]

    def android(*args, timeout=60):
        return subprocess.check_output(adb + list(args), text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()

    assert android('shell', 'getprop', 'ro.kernel.qemu') == '1'
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    private = output / 'tls-fixture-key.pem'
    certificate = output / 'tls-fixture-cert.pem'
    fixture = output / 'diagnosis-fixture.json'
    remote = f'/sdcard/Android/data/{APP}/files/diagnosis-fixture.json'
    peers = []
    report = dict(status='running', api=int(android('shell', 'getprop', 'ro.build.version.sdk')),
                  apk_sha256=hashlib.sha256(a.apk.read_bytes()).hexdigest(),
                  test_apk_sha256=hashlib.sha256(a.test_apk.read_bytes()).hexdigest(),
                  scope='owned loopback TLS and owned QEMU OpenSSH; no USB or production installer claim')
    try:
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                        'ec_paramgen_curve:prime256v1', '-noenc', '-keyout', str(private),
                        '-out', str(certificate), '-subj', '/CN=usb-most', '-days', '1'],
                       check=True, capture_output=True, timeout=20)
        private.chmod(0o600)
        pin = b64(hashlib.sha256(ssl.PEM_cert_to_DER_cert(certificate.read_text())).digest())
        contexts = []
        for alpn in [True, False]:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_3
            context.maximum_version = ssl.TLSVersion.TLSv1_3
            context.load_cert_chain(certificate, private)
            if alpn:
                context.set_alpn_protocols(['usb-most/1'])
            contexts.append(context)
        peers = [Peer(contexts[0]), Peer(contexts[1]), Peer()]
        changed_pin = b64(os.urandom(32))
        identity = 'Ключ компьютера не подтверждён или изменился. Сверьте его на компьютере перед новым подключением.'
        protocol = 'Ответ приёмника не соответствует протоколу. Проверьте совместимость версий приложения и приёмника.'
        login = 'Компьютер отклонил данные входа. Проверьте учётную запись и пароль или SSH-ключ.'
        cases = []
        for name, peer, saved_pin, pairing, summary in [
            ('tls-saved-pin', peers[0], changed_pin, False, identity),
            ('tls-pairing-pin', peers[0], changed_pin, True, identity),
            ('tls-missing-alpn', peers[1], pin, False, protocol),
            ('tls-plaintext-peer', peers[2], pin, False, protocol),
        ]:
            endpoint = dict(host='10.0.2.2', port=peer.port)
            profile = dict(transport='direct', endpoint=endpoint, server_pin=saved_pin)
            if pairing:
                code = dict(endpoint=endpoint, server_pin=saved_pin, claim=b64(os.urandom(32)))
                credentials = dict(pair_code='usbmost:1:' + b64(json.dumps(code).encode()),
                                   pair_nonce=b64(os.urandom(16)))
            else:
                credentials = dict(token=b64(os.urandom(32)))
            cases.append(dict(name=name, profile=profile, credentials=credentials, summary=summary,
                              failure='host_key_rejected' if summary == identity else 'protocol_error'))
        for name, host_pin, user, summary, failure in [
            ('ssh-saved-pin', changed_pin, 'fixture', identity, 'host_key_rejected'),
            # A nonexistent disposable account avoids failed-password counters on fixture/user accounts.
            ('ssh-denied-account', pins[0], 'usbmost-denied-' + os.urandom(4).hex(), login, 'authentication_failed'),
        ]:
            cases.append(dict(name=name, profile=dict(transport='ssh',
                endpoint=dict(host='10.0.2.2', port=metadata['ssh_port']), user=user,
                host_key=host_pin, reverse_port=0, prepare=False),
                credentials=dict(private_key=key.read_text(), client_id=os.urandom(16).hex()),
                summary=summary, failure=failure))
        with fixture.open('x') as stream:
            fixture.chmod(0o600)
            json.dump(dict(owned_emulator_only=True, cases=cases), stream)
        android('shell', 'am', 'force-stop', APP)
        for apk in [a.apk, a.test_apk]:
            assert 'Success' in android('install', '-r', str(apk.resolve()))
        android('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
        android('shell', 'wm', 'dismiss-keyguard')
        android('shell', 'mkdir', '-p', f'/sdcard/Android/data/{APP}/files')
        android('push', str(fixture), remote)
        fixture.unlink()
        result = android('shell', 'am', 'instrument', '-w', '-r', '-e', 'suite', 'diagnostics',
                         APP + '.test/ru.levchatkin.rutokenbridge.SmokeInstrumentation', timeout=300)
        (output / 'instrumentation.log').write_text(result + '\n')
        assert 'INSTRUMENTATION_CODE: -1' in result and 'FAIL ' not in result, result
        android('pull', f'/sdcard/Android/data/{APP}/files/diagnosis-proof/.', str(output))
        report['ui'] = json.loads((output / 'result.json').read_text())
        # Two rejected saved identities, one accepted identity with wrong ALPN,
        # and one plaintext reply. No retry and no application token is sent.
        assert [peer.accepted for peer in peers] == [2, 1, 1]
        assert peers[0].established == 0 and peers[1].established == 1
        assert all(peer.application_bytes == 0 for peer in peers)
        report['status'] = 'passed'
    except BaseException as error:
        report['status'] = 'failed'
        report['error'] = type(error).__name__
        raise
    finally:
        cleanup = []
        for action in [lambda: android('shell', 'am', 'force-stop', APP),
                       lambda: android('shell', 'rm', '-f', remote),
                       *[peer.close for peer in peers],
                       lambda: fixture.unlink(missing_ok=True),
                       lambda: private.unlink(missing_ok=True)]:
            try:
                action()
            except BaseException as error:
                cleanup.append(type(error).__name__)
        report['peers'] = [peer.counts() for peer in peers]
        report['cleanup_errors'] = cleanup
        if cleanup:
            report['status'] = 'failed'
        (output / 'runner-result.json').write_text(json.dumps(report, indent=2) + '\n')
        if cleanup:
            raise RuntimeError('owned diagnostic fixture cleanup failed')
    print('PASS real TLS/SSH failure diagnosis, saved-state preservation and Activity recreation')


if __name__ == '__main__':
    main()
