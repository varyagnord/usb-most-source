#!/usr/bin/env python3
"""Optional API 31 smoke-test environment, downloaded from Google's repository."""
import concurrent.futures
import hashlib
import pathlib
import xml.etree.ElementTree as ET
import zipfile
import download_sdk as sdk

def install(node, prefix, destination):
    archive=next(a for a in node.findall('archives/archive') if a.findtext('host-os') in (None,'linux')).find('complete')
    name=archive.findtext('url');size=int(archive.findtext('size'));digest=archive.findtext('checksum')
    target=sdk.BASE/name
    if not target.exists() or hashlib.sha1(target.read_bytes()).hexdigest()!=digest:
        chunk=(size+31)//32
        with concurrent.futures.ThreadPoolExecutor(max_workers=32) as pool:
            futures=[pool.submit(sdk.fetch_part,prefix+name,i*chunk,min(size-1,(i+1)*chunk-1),sdk.BASE/(name+'.chunk'+str(i))) for i in range(32)]
            parts=[f.result() for f in futures]
        with target.open('wb') as out:
            for part in parts:
                with part.open('rb') as inp:
                    while data:=inp.read(1024*1024):out.write(data)
                part.unlink()
    if hashlib.sha1(target.read_bytes()).hexdigest()!=digest:raise RuntimeError('checksum')
    with zipfile.ZipFile(target) as z:
        for entry in z.infolist():
            relative=pathlib.PurePosixPath(entry.filename)
            if '..' in relative.parts or relative.is_absolute():raise RuntimeError('unsafe archive')
            if len(relative.parts)<2:continue
            p=destination.joinpath(*relative.parts[1:])
            if entry.is_dir():p.mkdir(parents=True,exist_ok=True)
            else:
                p.parent.mkdir(parents=True,exist_ok=True)
                with z.open(entry) as inp,p.open('wb') as out:
                    while data:=inp.read(1024*1024):out.write(data)
                p.chmod((entry.external_attr>>16)&0o777 or 0o644)
    print('Verified and installed',node.get('path'),flush=True)

if __name__=='__main__':
    for name, url in [('repository2-1.xml','https://dl.google.com/android/repository/repository2-1.xml'),('sys-img.xml','https://dl.google.com/android/repository/sys-img/android/sys-img2-1.xml')]:
        if not (sdk.BASE/name).exists():sdk.urllib.request.urlretrieve(url,sdk.BASE/name)
    jobs=[]
    for manifest,package,prefix,destination in [
        ('repository2-1.xml','emulator','https://dl.google.com/android/repository/',sdk.SDK/'emulator'),
        ('sys-img.xml','system-images;android-31;default;x86_64','https://dl.google.com/android/repository/sys-img/android/',sdk.SDK/'system-images/android-31/default/x86_64')]:
        node=next(n for n in ET.parse(sdk.BASE/manifest).getroot() if n.tag.endswith('remotePackage') and n.get('path')==package)
        jobs.append((node,prefix,destination))
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        for result in [pool.submit(install,*job) for job in jobs]:result.result()
