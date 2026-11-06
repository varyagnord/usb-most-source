#!/usr/bin/env python3
"""Build locked Rust JNI libraries for the four Android ABIs, without changing global toolchains."""
import argparse
import os
import pathlib
import shutil
import subprocess
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--output', type=pathlib.Path, required=True)
parser.add_argument('--diagnostic', action='store_true')
args = parser.parse_args()
sdk = pathlib.Path(os.environ.get('ANDROID_HOME', pathlib.Path.home()/'.local/share/android-sdk'))
ndk = sdk/'ndk/27.0.12077973'
clang = ndk/'toolchains/llvm/prebuilt/linux-x86_64/bin'
if not clang.is_dir():
    raise SystemExit('Android NDK 27.0.12077973 is required; set ANDROID_HOME')
channel = tomllib.loads((ROOT/'rust-toolchain.toml').read_text())['toolchain']['channel']
cargo = shutil.which('cargo') or str(pathlib.Path.home()/'.cargo/bin/cargo')
for abi, target, prefix in [
    ('arm64-v8a', 'aarch64-linux-android', 'aarch64-linux-android'),
    ('armeabi-v7a', 'armv7-linux-androideabi', 'armv7a-linux-androideabi'),
    ('x86', 'i686-linux-android', 'i686-linux-android'),
    ('x86_64', 'x86_64-linux-android', 'x86_64-linux-android'),
]:
    # Validate the same UAPI constants/layout against actual NDK headers.
    uapi = r"""
#include <stddef.h>
#include <linux/usbdevice_fs.h>
#include <sys/ioctl.h>
_Static_assert(sizeof(struct usbdevfs_urb) == (sizeof(void*) == 8 ? 56 : 44), "urb size");
_Static_assert(offsetof(struct usbdevfs_urb, buffer) == (sizeof(void*) == 8 ? 16 : 12), "buffer offset");
_Static_assert(offsetof(struct usbdevfs_urb, usercontext) == (sizeof(void*) == 8 ? 48 : 40), "context offset");
_Static_assert(USBDEVFS_SUBMITURB == (sizeof(void*) == 8 ? 0x8038550aU : 0x802c550aU), "submit ioctl");
_Static_assert(USBDEVFS_RELEASEINTERFACE == 0x80045510U, "release ioctl");
_Static_assert(USBDEVFS_CONNECT == 0x5517U, "connect ioctl");
_Static_assert(sizeof(struct usbdevfs_ioctl) == (sizeof(void*) == 8 ? 16 : 12), "interface ioctl size");
_Static_assert(offsetof(struct usbdevfs_ioctl, data) == 8, "interface ioctl data offset");
_Static_assert(USBDEVFS_IOCTL == (sizeof(void*) == 8 ? 0xc0105512U : 0xc00c5512U), "interface ioctl code");
_Static_assert(USBDEVFS_DISCARDURB == 0x550bU, "discard ioctl");
_Static_assert(USBDEVFS_REAPURBNDELAY == (sizeof(void*) == 8 ? 0x4008550dU : 0x4004550dU), "reap ioctl");
"""
    subprocess.run([str(clang/f'{prefix}26-clang'), '-x', 'c', '-std=c11', '-fsyntax-only', '-'], input=uapi, text=True, check=True)
    env = os.environ.copy()
    env['RUSTUP_TOOLCHAIN'] = channel
    target_dir = ROOT/'build'/('rust-target-diagnostic' if args.diagnostic else 'rust-target')
    env['CARGO_TARGET_DIR'] = str(target_dir)
    key = target.upper().replace('-', '_')
    env[f'CARGO_TARGET_{key}_LINKER'] = str(clang/f'{prefix}26-clang')
    env[f'CC_{target.replace("-", "_")}'] = str(clang/f'{prefix}26-clang')
    env[f'AR_{target.replace("-", "_")}'] = str(clang/'llvm-ar')
    env[f'CARGO_TARGET_{key}_RUSTFLAGS'] = '-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=4096'
    command = [cargo, 'build', '--lib', '--locked', '--release', '--target', target, '-p', 'usb-most-native', '-j', '2']
    if args.diagnostic:
        command += ['--features', 'diagnostics']
    subprocess.run(command, cwd=ROOT, env=env, check=True)
    output = args.output.resolve()/abi
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(target_dir/target/'release/libusblib.so', output/'libusblib.so')
    print('Built', abi, flush=True)
