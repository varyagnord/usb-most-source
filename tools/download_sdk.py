#!/usr/bin/env python3
"""Download pinned Google SDK archives; verify upstream manifest checksums."""
import concurrent.futures
import hashlib
import pathlib
import os
import stat
import time
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

BASE = pathlib.Path(os.environ.get('BRIDGE_BUILD_CACHE', str(pathlib.Path.home()/'.cache/rutoken-bridge/downloads')))
BASE.mkdir(parents=True, exist_ok=True)
SDK = pathlib.Path(os.environ.get('ANDROID_HOME', str(pathlib.Path.home()/'.local/share/android-sdk')))
WANTED = {'ndk;27.0.12077973': 'ndk/27.0.12077973',
          'platforms;android-34': 'platforms/android-34',
          'build-tools;34.0.0': 'build-tools/34.0.0', 'platform-tools': 'platform-tools'}

def fetch_part(url, start, end, path):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for attempt in range(8):
        try:
            have = path.stat().st_size if path.exists() else 0
            if have == end - start + 1: return path
            if have > end - start + 1: path.unlink(); have = 0
            position = start + have
            request = urllib.request.Request(url, headers={'Range': f'bytes={position}-{end}'})
            with opener.open(request, timeout=60) as response:
                if response.status != 206:
                    raise RuntimeError('Range request not honored')
                if not response.headers.get('Content-Range', '').startswith(f'bytes {position}-{end}/'):
                    raise RuntimeError('Unexpected content range')
                with path.open('ab') as out:
                    while data := response.read(1024 * 256):
                        out.write(data)
            if path.stat().st_size != end - start + 1:
                raise RuntimeError('Truncated download')
            return path
        except Exception:
            if attempt == 7: raise
            time.sleep(min(10, 2 ** attempt))

def package(node):
    path = node.get('path')
    archive = next(a for a in node.findall('archives/archive')
                   if a.findtext('host-os') in (None, 'linux')).find('complete')
    name, size, sha1 = (archive.findtext(k) for k in ('url', 'size', 'checksum'))
    size = int(size)
    dest = BASE / name
    if not dest.exists() or hashlib.sha1(dest.read_bytes()).hexdigest() != sha1:
        count = 8
        chunk = (size + count - 1) // count
        with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
            futures = [pool.submit(fetch_part, 'https://dl.google.com/android/repository/' + name,
                                   i * chunk, min(size - 1, (i + 1) * chunk - 1),
                                   BASE / f'{name}.part{i}') for i in range(count)]
            parts = [f.result() for f in futures]
        with dest.open('wb') as out:
            for part in parts:
                with part.open('rb') as inp:
                    while data := inp.read(1024 * 1024): out.write(data)
                part.unlink()
    if hashlib.sha1(dest.read_bytes()).hexdigest() != sha1:
        raise RuntimeError('Upstream checksum mismatch: ' + name)
    target = SDK / WANTED[path]
    target.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(dest) as z:
        for entry in z.infolist():
            relative = pathlib.PurePosixPath(entry.filename)
            if '..' in relative.parts or relative.is_absolute(): raise RuntimeError('Unsafe archive')
            if len(relative.parts) < 2: continue
            f = target.joinpath(*relative.parts[1:])
            if entry.is_dir(): f.mkdir(parents=True, exist_ok=True)
            elif stat.S_ISLNK(entry.external_attr >> 16):
                link = z.read(entry).decode()
                if not (f.parent/link).resolve().is_relative_to(target): raise RuntimeError('Unsafe symlink')
                f.parent.mkdir(parents=True, exist_ok=True)
                if f.exists() or f.is_symlink(): f.unlink()
                f.symlink_to(link)
            else:
                f.parent.mkdir(parents=True, exist_ok=True)
                if f.is_symlink(): f.unlink()
                with z.open(entry) as inp, f.open('wb') as out:
                    while data := inp.read(1024 * 1024): out.write(data)
                f.chmod((entry.external_attr >> 16) & 0o777 or 0o644)
    print('Verified and installed', path, flush=True)

if __name__ == '__main__':
    if not (BASE / 'repository2-1.xml').exists():
        urllib.request.urlretrieve('https://dl.google.com/android/repository/repository2-1.xml', BASE / 'repository2-1.xml')
    nodes = {}
    for node in ET.parse(BASE / 'repository2-1.xml').getroot():
        if node.tag.endswith('remotePackage') and node.get('path') in WANTED:
            
            if node.get('path') == 'platforms;android-34' and node.findtext('display-name') != 'Android SDK Platform 34': continue
            if node.get('obsolete') == 'true': continue
            nodes.setdefault(node.get('path'), node)
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        for future in [pool.submit(package, n) for n in nodes.values()]: future.result()
