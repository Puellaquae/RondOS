#!/usr/bin/env python3
"""Convert a binary PPM (P6) to PNG using only the standard library.

QEMU's monitor `screendump` writes P6 PPM; there is no PIL and no ImageMagick on
the target host, so this is the smallest thing that lets a screenshot be looked
at.  It is diagnostics-only tooling: nothing in the build depends on it.

    python3 tools/ppm2png.py shot.ppm shot.png
"""

import struct
import sys
import zlib


def read_ppm(path):
    with open(path, "rb") as f:
        data = f.read()
    if not data.startswith(b"P6"):
        raise SystemExit(f"{path}: not a binary PPM (P6)")

    # Header: magic, width, height, maxval, then exactly one whitespace byte.
    fields = []
    i = 2
    while len(fields) < 3:
        while i < len(data) and data[i : i + 1].isspace():
            i += 1
        if data[i : i + 1] == b"#":
            while data[i : i + 1] not in (b"\n", b""):
                i += 1
            continue
        start = i
        while i < len(data) and not data[i : i + 1].isspace():
            i += 1
        fields.append(int(data[start:i]))
    i += 1  # the single whitespace byte after maxval

    width, height, maxval = fields
    if maxval != 255:
        raise SystemExit(f"{path}: unsupported maxval {maxval}")
    pixels = data[i:]
    expected = width * height * 3
    if len(pixels) != expected:
        raise SystemExit(f"{path}: {len(pixels)} pixel bytes, expected {expected}")
    return width, height, pixels


def write_png(path, width, height, rgb):
    raw = bytearray()
    stride = width * 3
    for y in range(height):
        raw.append(0)  # filter: none
        raw += rgb[y * stride : (y + 1) * stride]

    def chunk(tag, payload):
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def main(argv):
    if len(argv) != 3:
        raise SystemExit(__doc__)
    width, height, rgb = read_ppm(argv[1])
    write_png(argv[2], width, height, rgb)
    print(f"{argv[2]}: {width}x{height}")


if __name__ == "__main__":
    main(sys.argv)
