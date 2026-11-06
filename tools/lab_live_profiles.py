"""Owned QEMU only: observe two real receivers and restore their exact configs.

Configuration (including an SSH proof) stays in the private host fixture memory.
It must not be copied into public reports. No USB or mount is manipulated here.
"""
import json
import os
import base64
from contextlib import contextmanager, nullcontext
import hashlib
import hmac
from pathlib import Path
import re
import subprocess
import socket
import struct
import threading
import time
from lab_receiver_upgrade import BINARY, CONFIG, regular, require, snapshot, verify_trust


@contextmanager
def restoration_gateway(config):
    """Restore the old idle SSH fixture through its real installer health check.

    The installer requires an authenticated USB/IP list roundtrip. The old soak
    client has exited, so temporarily serve an empty list on its exact vacant
    loopback port with its original secret. Never import any USB device.
    """
    secret = base64.urlsafe_b64decode(config['proof'] + '=')
    require(len(secret) == 32 and 1024 <= config['port'] <= 65535, 'Bounded original fixture')
    listener = socket.socket()
    listener.bind(('127.0.0.1', config['port']))
    listener.listen(4); listener.settimeout(.2)
    stop = threading.Event()
    failures = []
    def read(stream, count):
        data = b''
        while len(data) < count:
            chunk = stream.recv(count - len(data))
            if not chunk: raise RuntimeError('Restoration gateway EOF')
            data += chunk
        return data
    def serve():
        while not stop.is_set():
            try: stream, _ = listener.accept()
            except socket.timeout: continue
            try:
                with stream:
                    stream.settimeout(5)
                    require(read(stream, 8) == b'UMGATE2\0', 'Restoration gateway magic')
                    client, server = read(stream, 32), os.urandom(32)
                    def tag(role):
                        return hmac.new(secret, b'UMGATE2\0' + role + client + server, hashlib.sha256).digest()
                    stream.sendall(server + tag(b'server'))
                    require(hmac.compare_digest(read(stream, 32), tag(b'client')), 'Restoration authentication')
                    stream.sendall(b'\x01')  # GatewayConnector waits for remote-stream readiness.
                    version, operation, status = struct.unpack('!HHI', read(stream, 8))
                    require(version == 0x111 and status == 0, 'Bounded restoration operation')
                    if operation == 0x8005:
                        stream.sendall(struct.pack('!HHII', version, 5, 0, 0))
                    else:
                        # The receiver also probes optional capabilities after
                        # a successful list. Explicitly decline those probes.
                        require(operation in (0x80df, 0x80e1), 'No USB import in restoration fixture')
                        stream.sendall(struct.pack('!HHI', version, operation & 0x7fff, 95))
            except Exception as error:
                failures.append(str(error))
                stop.set()
    thread = threading.Thread(target=serve)
    thread.start()
    try: yield
    finally:
        stop.set(); thread.join(timeout=6); listener.close()
        require(not thread.is_alive() and not failures,
                'Restoration gateway did not complete cleanly: ' + '; '.join(failures))


def wait_snapshot(expected, connected, peer=None):
    deadline = time.monotonic() + 20
    while True:
        try: return snapshot(expected, connected, peer)
        except RuntimeError as error:
            if str(error) != 'Receiver is not in expected idle/session state' or time.monotonic() >= deadline:
                raise
            time.sleep(.2)


def run(settings):
    require(os.geteuid() == 0, 'Owned guest root required')
    require(any(x in Path('/sys/class/dmi/id/product_name').read_text()
                for x in ('QEMU', 'Standard PC')), 'Owned QEMU required')
    expected = settings['current_sha256']
    require(re.fullmatch('[a-f0-9]{64}', expected) is not None, 'Expected receiver hash required')
    action = settings['action']
    if action == 'before':
        state = snapshot(expected, False)
        return dict(state=state, configuration=json.loads(regular(CONFIG, 4096)[0]))
    before = settings['before']
    connected = action == 'connected'
    peer = settings['peer'] if connected and settings['transport'] == 'ssh' else None
    current = wait_snapshot(expected, connected, peer)
    verify_trust(before['state'], current)
    if action in ('connected', 'idle'):
        return current
    require(action in ('direct', 'restore'), 'Known fixture action required')
    actual = json.loads(regular(CONFIG, 4096)[0])
    if action == 'direct':
        require(actual == before['configuration'], 'Original config changed before fixture setup')
        desired = {'transport': 'direct', 'port': 53412}
    else:
        desired = before['configuration']
        require(actual == desired or
                (settings['transport'] == 'direct' and actual == {'transport': 'direct', 'port': 53412}) or
                (settings['transport'] == 'ssh' and set(actual) == {'transport', 'port', 'proof', 'peer'}
                 and actual['transport'] == 'ssh' and actual['peer'] == settings['peer']),
                'Refuse to replace unrelated receiver configuration')
    if desired != actual:
        gateway = restoration_gateway(desired) if action == 'restore' and desired['transport'] == 'ssh' else nullcontext()
        with gateway:
            result = subprocess.run([str(BINARY), '--install-stdin'], input=json.dumps(desired).encode(),
                                    capture_output=True, timeout=120)
            require(result.returncode == 0, 'Product installer failed; preserve private recovery config')
    after = wait_snapshot(expected, False)
    verify_trust(before['state'], after)
    if action == 'restore':
        for name, entry in before['state']['files'].items():
            require(all(after['files'][name][k] == entry[k] for k in ('sha256', 'bytes', 'uid', 'gid', 'mode')),
                    'Original deployment differs after restore')
        require(after['config_hash'] == before['state']['config_hash'], 'Original config hash differs')
    return after


if __name__ == '__main__':
    print(json.dumps(run(SETTINGS)))
