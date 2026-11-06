#!/usr/bin/env python3
"""Build a separate emulator-only SAF fault provider; never modify the product APK."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import zipfile

root = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--sdk', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
out = a.output.absolute()
assert out == out.resolve() and out.is_relative_to(root/'build')
out.mkdir(parents=True, exist_ok=False, mode=0o700)
fixture = root/'tests/document-provider'
tools = a.sdk/'build-tools/34.0.0'
android = a.sdk/'platforms/android-34/android.jar'

def run(args):
    result = subprocess.run([str(x) for x in args], capture_output=True, text=True, timeout=120)
    with (out/'build.log').open('a') as log: log.write(result.stdout + result.stderr)
    if result.returncode: raise RuntimeError('Fixture command failed: ' + str(args[0]))

classes = out/'classes'; classes.mkdir()
dex = out/'dex'; dex.mkdir()
run([tools/'aapt2', 'link', '-I', android, '--manifest', fixture/'AndroidManifest.xml',
     '-o', out/'resources.apk'])
run(['javac', '-source', '8', '-target', '8', '-bootclasspath', android, '-d', classes,
     *sorted(fixture.glob('*.java'))])
run([tools/'d8', '--min-api', '26', '--lib', android, '--output', dex, *sorted(classes.rglob('*.class'))])
with zipfile.ZipFile(out/'resources.apk') as src, zipfile.ZipFile(out/'unsigned.apk', 'x') as dest:
    for info in src.infolist(): dest.writestr(info, src.read(info.filename))
    dest.write(dex/'classes.dex', 'classes.dex', compress_type=zipfile.ZIP_DEFLATED)
# Publicly-known synthetic fixture password, never an application/release key.
run(['keytool', '-genkeypair', '-keystore', out/'fixture.jks', '-alias', 'fixture',
     '-storepass', 'android', '-keypass', 'android', '-keyalg', 'RSA', '-keysize', '2048',
     '-validity', '30', '-dname', 'CN=Disposable SAF Fixture'])
run([tools/'zipalign', '-f', '4', out/'unsigned.apk', out/'aligned.apk'])
run([tools/'apksigner', 'sign', '--ks', out/'fixture.jks', '--ks-key-alias', 'fixture',
     '--ks-pass', 'pass:android', '--out', out/'fixture.apk', out/'aligned.apk'])
run([tools/'apksigner', 'verify', '--verbose', out/'fixture.apk'])
record = {'status': 'built_verified_fixture_only', 'apk_sha256': hashlib.sha256((out/'fixture.apk').read_bytes()).hexdigest(),
          'source_files': {str(f.relative_to(root)): hashlib.sha256(f.read_bytes()).hexdigest()
                           for f in sorted(fixture.rglob('*')) if f.is_file()},
          'no_product_change': True, 'no_network_permission': True}
(out/'result.json').write_text(json.dumps(record, indent=2)+'\n')
print(json.dumps(record, indent=2))
