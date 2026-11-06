"""Read-only Linux test mounts; failed cleanup must always preserve data.

No TemporaryDirectory, recursive deletion, mount(8), or umount(8): a mounted
directory must never be treated as a disposable directory tree. The direct
umount syscall also avoids libmount's potentially expensive FAT statfs probe.
"""
import ctypes
import os
from pathlib import Path
import tempfile


class ReadOnlyMount:
    def __init__(self, source, filesystem, *, parent='/run', options=''):
        self.source = os.fsencode(source)
        self.filesystem = filesystem.encode('ascii')
        self.options = options.encode('ascii')
        self.parent = parent
        self.path = None
        self.mounted = False

    def _mount(self):
        libc = ctypes.CDLL(None, use_errno=True)
        libc.mount.argtypes = [ctypes.c_char_p, ctypes.c_char_p,
                               ctypes.c_char_p, ctypes.c_ulong, ctypes.c_char_p]
        libc.mount.restype = ctypes.c_int
        # MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC, never caller-controlled.
        if libc.mount(self.source, os.fsencode(self.path), self.filesystem,
                      1 | 2 | 4 | 8, self.options) != 0:
            raise OSError(ctypes.get_errno(), 'read-only mount failed', str(self.path))

    def _unmount(self):
        libc = ctypes.CDLL(None, use_errno=True)
        libc.umount2.argtypes = [ctypes.c_char_p, ctypes.c_int]
        libc.umount2.restype = ctypes.c_int
        # Ordinary unmount: never force or lazily detach a busy filesystem.
        if libc.umount2(os.fsencode(self.path), 0) != 0:
            raise OSError(ctypes.get_errno(), 'unmount failed; path preserved', str(self.path))

    def __enter__(self):
        if self.path is not None:
            raise RuntimeError('mount context cannot be reused')
        # mkdtemp has no destructor that might recursively delete a live mount.
        self.path = Path(tempfile.mkdtemp(prefix='usb-most-ro-', dir=self.parent))
        try:
            self._mount()
        except BaseException:
            # rmdir only: even an interrupted successful mount cannot be erased.
            try:
                self.path.rmdir()
            except OSError:
                pass
            raise
        self.mounted = True
        return self.path

    def close(self):
        if self.mounted:
            self._unmount()  # Any failure leaves the path and all its data intact.
            self.mounted = False
        if self.path is not None and self.path.exists():
            self.path.rmdir()  # Fails safely if unexpected data remains.

    def __exit__(self, exc_type, exc_value, traceback):
        self.close()
        return False
