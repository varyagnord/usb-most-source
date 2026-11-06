#!/usr/bin/env python3
"""Download a successful private CI bundle and pin its verified source identity.

Developer-only operation, never invoked by the APK or target computer. Source
builders may produce the same manifest on their own isolated Windows test VM;
the APK packager validates its local pin without depending on GitHub access.
"""
import argparse
from pathlib import Path
import json
import subprocess
import tomllib
from receiver_source import ROOT
from windows_receiver_asset import validate

p = argparse.ArgumentParser(description=__doc__)
source = p.add_mutually_exclusive_group(required=True)
source.add_argument('--run-id', type=int)
source.add_argument('--local-directory', type=Path,
                    help='Bundle produced by prepare-windows-driver-lab.ps1 -ProductPreparation in your own disposable VM')
p.add_argument('--reuse-download', action='store_true',
               help='Revalidate a previously downloaded run artifact without downloading it again')
a = p.parse_args()
if a.reuse_download and a.run_id is None:
    p.error('--reuse-download requires --run-id')
run = None
if a.run_id is not None:
    if a.run_id <= 0:
        raise SystemExit('Invalid CI run')
    repo = 'varyagnord/usb-most'
    run = json.loads(subprocess.check_output(
        ['gh', 'api', f'repos/{repo}/actions/runs/{a.run_id}'], text=True))
    if (run['conclusion'] != 'success' or run['status'] != 'completed'
            or run['repository']['full_name'] != repo or not run['repository']['private']
            or run['path'] != '.github/workflows/windows-receiver.yml'
            or run['head_branch'] != 'feature/connection-bootstrap'):
        raise SystemExit('Run is not a completed trusted receiver workflow')
    directory = ROOT / 'build' / f'windows-receiver-ci-{a.run_id}'
    if not a.reuse_download:
        directory.mkdir(exist_ok=False)
        subprocess.run(['gh', 'run', 'download', str(a.run_id), '-R', repo,
                        '-n', 'windows-receiver-x86_64', '-D', str(directory)], check=True)
    validation = {'repository': repo, 'run_id': a.run_id, 'url': run['html_url']}
else:
    directory = a.local_directory.resolve()
    if not directory.is_relative_to(ROOT / 'build'):
        raise SystemExit('Local bundle must be within this build workspace')
    validation = {'kind': 'developer-windows-vm', 'run_id': None}
version = tomllib.loads((ROOT / 'native/usb-most-desktop/Cargo.toml').read_text())['package']['version']
manifest, _ = validate(directory, version)
if run is not None and manifest['source_commit'] != run['head_sha']:
    raise SystemExit('Artifact source commit does not match successful workflow')
pin = {'schema': 1, 'artifact': manifest, 'validation': validation}
destination = ROOT / 'server/windows-receiver-lock-x86_64.json'
destination.write_text(json.dumps(pin, indent=2, sort_keys=True) + '\n')
print(f'Pinned tested Windows receiver; build with USB_MOST_WINDOWS_RECEIVER_DIR={directory}')
