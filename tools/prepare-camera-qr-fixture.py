#!/usr/bin/env python3
"""Render synthetic pairing.pgm as an emulator imagefile input (requires Pillow).

The current emulator projects imagefile content into its camera view, rather
than returning original pixels. Put this QR in the visible left portion of the
640x480 fixture; a centered QR was clipped in the API34 virtual sensor. This
fixture contains documentation-only addresses and no usable pairing secret.
"""
import argparse
from pathlib import Path
from PIL import Image

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('fixture_directory', type=Path)
a = p.parse_args()
qr = Image.open(a.fixture_directory / 'pairing.pgm').convert('RGB')
if qr.size != (276, 276):
    raise SystemExit('Unexpected synthetic QR dimensions; inspect framing explicitly')
frame = Image.new('RGB', (640, 480), 'white')
frame.paste(qr, (24, 102))
output = a.fixture_directory / 'pairing-camera-left.png'
frame.save(output)
print(f'Start the disposable AVD with -camera-back imagefile:{output.resolve()}')
