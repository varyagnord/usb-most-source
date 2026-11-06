"""Real JDK signature checks using synthetic bundles and disposable test keys."""
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile

ROOT = Path(__file__).resolve().parents[1]


class BundleSigningTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Only created files in this test-owned directory; no guest/USB mounts.
        cls.temporary = tempfile.TemporaryDirectory(prefix='usb-most-signature-tests-')
        cls.root = Path(cls.temporary.name)
        cls.password = cls.root / 'password'
        cls.password.write_text('synthetic-test-key-only\n')
        cls.password.chmod(0o600)
        for name in ['expected', 'other']:
            key = cls.root / (name + '.jks')
            subprocess.run(['keytool', '-genkeypair', '-keystore', str(key), '-storetype', 'JKS',
                            '-alias', 'upload', '-keyalg', 'RSA', '-keysize', '2048', '-validity', '2',
                            '-dname', 'CN=Synthetic signing test', '-storepass:file', str(cls.password),
                            '-keypass:file', str(cls.password)], check=True, capture_output=True)
            key.chmod(0o600)
            subprocess.run(['keytool', '-exportcert', '-rfc', '-keystore', str(key), '-alias', 'upload',
                            '-storepass:file', str(cls.password), '-file', str(cls.root / (name + '.pem'))], check=True, capture_output=True)
        cls.source = cls.root / 'unsigned.aab'
        with zipfile.ZipFile(cls.source, 'w') as archive:
            for name in ['BundleConfig.pb', 'base/manifest/AndroidManifest.xml', 'base/dex/classes.dex',
                         'META-INF/com/android/build/gradle/app-metadata.properties']:
                archive.writestr(name, b'synthetic payload, not an installable application')
        cls.signed = cls.root / 'signed.aab'
        result = cls.sign(cls.signed)
        if result.returncode:
            raise AssertionError(result.stderr + result.stdout)

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    @classmethod
    def sign(cls, output):
        return subprocess.run([sys.executable, str(ROOT / 'tools/sign-bundle.py'), str(cls.source), str(output),
                               '--keystore', str(cls.root / 'expected.jks'), '--password-file', str(cls.password)],
                              text=True, capture_output=True, timeout=40)

    def verify(self, path, certificate='expected'):
        return subprocess.run(['java', str(ROOT / 'tools/VerifyBundleSignature.java'), str(path),
                               str(self.root / (certificate + '.pem'))], capture_output=True, timeout=30).returncode

    def test_expected_certificate_and_complete_payload(self):
        self.assertEqual(self.verify(self.signed), 0)

    def test_wrong_upload_certificate(self):
        self.assertNotEqual(self.verify(self.signed, 'other'), 0)

    def test_modified_payload(self):
        path = self.root / 'modified.aab'
        with zipfile.ZipFile(self.signed) as original, zipfile.ZipFile(path, 'w') as changed:
            for item in original.infolist():
                data = original.read(item)
                if item.filename == 'base/dex/classes.dex':
                    data += b'tampered'
                changed.writestr(item, data)
        self.assertNotEqual(self.verify(path), 0)

    def test_unsigned_addition_including_meta_inf(self):
        for index, name in enumerate(['base/assets/injected.txt', 'META-INF/injected.properties']):
            path = self.root / ('addition' + str(index) + '.aab')
            path.write_bytes(self.signed.read_bytes())
            with zipfile.ZipFile(path, 'a') as archive:
                archive.writestr(name, b'unsigned')
            self.assertNotEqual(self.verify(path), 0)

    def test_removed_signed_entry(self):
        path = self.root / 'removed.aab'
        with zipfile.ZipFile(self.signed) as original, zipfile.ZipFile(path, 'w') as changed:
            for item in original.infolist():
                if item.filename != 'META-INF/com/android/build/gradle/app-metadata.properties':
                    changed.writestr(item, original.read(item))
        self.assertNotEqual(self.verify(path), 0)

    def test_unsigned_bundle_and_duplicate_entries(self):
        self.assertNotEqual(self.verify(self.source), 0)
        path = self.root / 'duplicate.aab'
        path.write_bytes(self.signed.read_bytes())
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(path, 'a') as archive:
                archive.writestr('base/dex/classes.dex', b'duplicate')
        self.assertNotEqual(self.verify(path), 0)

    def test_existing_output_is_preserved(self):
        before = hashlib.sha256(self.signed.read_bytes()).digest()
        self.assertNotEqual(self.sign(self.signed).returncode, 0)
        self.assertEqual(hashlib.sha256(self.signed.read_bytes()).digest(), before)

    def test_shared_password_file_is_rejected(self):
        self.password.chmod(0o644)
        try:
            output = self.root / 'must-not-exist.aab'
            self.assertNotEqual(self.sign(output).returncode, 0)
            self.assertFalse(output.exists())
        finally:
            self.password.chmod(0o600)


if __name__ == '__main__':
    unittest.main()
