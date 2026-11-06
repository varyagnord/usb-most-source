"""Observe/restore an owned receiver around Android v1 migration tests.

Only existing exact-hash product installer may restore the saved direct config.
No raw SSH proof, identity, pairing secret, USB device, mount or cleanup is used.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import time
from lab_receiver_upgrade import BINARY, CONFIG, regular, require, snapshot, verify_trust


def direct_config(value):
    require(isinstance(value, dict) and set(value) == {'transport', 'port'}
            and value['transport'] == 'direct' and type(value['port']) is int
            and 1024 <= value['port'] <= 65535, 'Expected bounded direct receiver configuration')
    return value


def restore(settings):
    before = settings['before']
    original = direct_config(settings['configuration'])
    peer = settings['peer']
    require(re.fullmatch('[a-f0-9]{32}', peer) is not None, 'Invalid owned peer')
    deadline = time.monotonic() + 30
    while True:
        try:
            current = snapshot(settings['current_sha256'], False)
            break
        except RuntimeError as error:
            if str(error) != 'Receiver is not in expected idle/session state' or time.monotonic() >= deadline:
                raise
            time.sleep(.2)
    verify_trust(before, current)
    configuration = json.loads(regular(CONFIG, 4096)[0])
    if configuration != original:
        require(set(configuration) == {'transport', 'port', 'proof', 'peer'}
                and configuration['transport'] == 'ssh' and configuration['peer'] == peer,
                'Refuse to overwrite unrelated receiver configuration')
        result = subprocess.run([str(BINARY), '--install-stdin'],
            input=json.dumps(original).encode(), capture_output=True, timeout=120)
        require(result.returncode == 0, 'Original configuration restore failed')
    after = snapshot(settings['current_sha256'], False)
    verify_trust(before, after)
    for name, record in before['files'].items():
        require(all(after['files'][name][key] == record[key]
                    for key in ('sha256', 'bytes', 'mode', 'uid', 'gid')),
                'Original receiver file content/permissions changed')
    require(after['config_hash'] == before['config_hash'], 'Original receiver configuration identity differs')
    return dict(restored=after, exact_original_deployment_preserved=True)


def connected(settings):
    transport = settings['transport']
    require(transport in ('ssh', 'direct'), 'Invalid migration transport')
    # Health.peer identifies the configured SSH client; direct TLS deliberately
    # exposes no peer id in this API. Its token authentication is performed by
    # the real app, while connected confirms the receiver's authenticated USB
    # device-list roundtrip. That poll runs every two seconds, after TLS ready.
    peer = settings['peer'] if transport == 'ssh' else None
    deadline = time.monotonic() + 15
    while True:
        try:
            observed = snapshot(settings['current_sha256'], True, peer)
            break
        except RuntimeError as error:
            if str(error) != 'Receiver is not in expected idle/session state' or time.monotonic() >= deadline:
                raise
            time.sleep(.2)
    verify_trust(settings['before'], observed)
    if transport == 'direct':
        require(observed['config_hash'] == settings['before']['config_hash'],
                'TLS receiver configuration changed')
    return observed


def run(settings):
    require(os.geteuid() == 0, 'Owned guest root required')
    product = Path('/sys/class/dmi/id/product_name').read_text()
    require('QEMU' in product or 'Standard PC' in product, 'Only owned QEMU guest')
    require(re.fullmatch('[a-f0-9]{64}', settings['current_sha256']) is not None,
            'Invalid expected receiver hash')
    if settings['action'] == 'before':
        observed = snapshot(settings['current_sha256'], False)
        return dict(before=observed, configuration=direct_config(json.loads(regular(CONFIG, 4096)[0])))
    if settings['action'] == 'connected':
        return connected(settings)
    if settings['action'] == 'restore':
        return restore(settings)
    raise ValueError('Unknown migration lab action')


if __name__ == '__main__':
    print(json.dumps(run(SETTINGS)))
