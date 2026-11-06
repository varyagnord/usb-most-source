"""Source correspondence and binary tampering gates, with no executable fixtures."""
import hashlib
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
import receiver_source
import windows_receiver_asset as asset


class ReceiverArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='usb-most-artifact-unit-')
        self.addCleanup(self.temporary.cleanup)  # Only owned regular-file fixtures; no mounts.
        self.root = Path(self.temporary.name)
        for name in receiver_source.FILES:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'fixture\n')
        for tree in receiver_source.TREES:
            directory = self.root / tree
            directory.mkdir(parents=True)
            (directory / 'fixture.rs').write_bytes(b'// fixture\n')
        self.directory = self.root / 'artifact'
        self.directory.mkdir()
        data = bytearray(128)
        data[:2] = b'MZ'
        struct.pack_into('<I', data, 60, 64)
        data[64:68] = b'PE\0\0'
        struct.pack_into('<H', data, 68, 0x8664)
        struct.pack_into('<H', data, 88, 0x20b)
        self.data = bytes(data)
        self.manifest = dict(schema=1, source_commit='a' * 40,
            source_sha256=receiver_source.fingerprint(self.root), target='x86_64-pc-windows-msvc',
            version='1.0.11', name=asset.NAME, bytes=len(data),
            sha256=hashlib.sha256(data).hexdigest(),
            driver_manifest_sha256=hashlib.sha256(b'fixture\n').hexdigest())
        self.write()

    def write(self):
        (self.directory / asset.NAME).write_bytes(self.data)
        (self.directory / 'manifest.json').write_text(json.dumps(self.manifest))

    def validate(self, pin=None):
        return asset.validate(self.directory, '1.0.11', pin=pin, root=self.root)

    def test_binary_hash_source_version_and_pin_are_independent_gates(self):
        self.assertEqual(self.validate({'artifact': self.manifest})[1], self.data)
        with self.assertRaises(ValueError):
            self.validate({'artifact': {}})
        (self.directory / asset.NAME).write_bytes(self.data + b'x')
        with self.assertRaisesRegex(ValueError, 'digest mismatch'):
            self.validate()
        self.write()
        (self.root / 'native/usb-most-platform/fixture.rs').write_text('// changed\n')
        with self.assertRaisesRegex(ValueError, 'current native sources'):
            self.validate()

    def test_crlf_is_portable_but_path_identity_matters(self):
        before = receiver_source.fingerprint(self.root)
        path = self.root / 'native/usb-most-platform/fixture.rs'
        path.write_bytes(b'// fixture\r\n')
        self.assertEqual(before, receiver_source.fingerprint(self.root))
        path.rename(path.with_name('other.rs'))
        self.assertNotEqual(before, receiver_source.fingerprint(self.root))

    def test_modified_vendored_ssh_invalidates_old_receiver(self):
        path = self.root / 'third_party/russh/fixture.rs'
        path.write_text('// dependency changed\n')
        with self.assertRaisesRegex(ValueError, 'current native sources'):
            self.validate()

    def test_malformed_pe_and_unknown_contents_rejected(self):
        (self.directory / 'unexpected').write_text('no')
        with self.assertRaisesRegex(ValueError, 'Unexpected'):
            self.validate()
        (self.directory / 'unexpected').unlink()
        for offset in (0, 63, 103, 0xffffffff):
            with self.subTest(offset=offset):
                data = bytearray(self.data)
                struct.pack_into('<I', data, 60, offset)
                self.data = bytes(data)
                self.manifest['sha256'] = hashlib.sha256(data).hexdigest()
                self.write()
                with self.assertRaisesRegex(ValueError, 'PE32'):
                    self.validate()

    def test_driver_attestation_accepts_only_exact_lf_or_crlf_content(self):
        path = self.root / 'server/windows-driver-lock-x86_64.json'
        for local in (b'fixture\n', b'fixture\r\n'):
            path.write_bytes(local)
            for remote in (b'fixture\n', b'fixture\r\n'):
                self.manifest['driver_manifest_sha256'] = hashlib.sha256(remote).hexdigest()
                self.write()
                self.assertEqual(self.validate()[1], self.data)
        for changed in (b'fixture \n', b'other\r\n', b'fixture\r'):
            self.manifest['driver_manifest_sha256'] = hashlib.sha256(changed).hexdigest()
            self.write()
            with self.assertRaisesRegex(ValueError, 'driver pin mismatch'):
                self.validate()

    def test_symlink_and_oversized_artifacts_rejected(self):
        binary = self.directory / asset.NAME
        binary.unlink()
        binary.symlink_to(self.root / 'Cargo.lock')
        with self.assertRaisesRegex(ValueError, 'regular'):
            self.validate()
        binary.unlink()
        with binary.open('wb') as output:
            output.truncate(asset.MAX_BYTES + 1)
        with self.assertRaisesRegex(ValueError, 'bound'):
            self.validate()


if __name__ == '__main__':
    unittest.main()
