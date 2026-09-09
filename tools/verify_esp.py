#!/usr/bin/env python3
"""Verify a FAT16 ESP image built by tools/mkesp.py against a source tree."""
import os
import struct
import sys

SECTOR = 512


def verify(img_path, src):
    img = open(img_path, "rb").read()
    start, size = struct.unpack_from("<II", img, 446 + 8)
    base = start * SECTOR
    bs = img[base:base + SECTOR]
    spc = bs[13]
    res = struct.unpack_from("<H", bs, 14)[0]
    nfats = bs[16]
    root_entries = struct.unpack_from("<H", bs, 17)[0]
    fatsz = struct.unpack_from("<H", bs, 22)[0]
    fat = img[base + res * SECTOR: base + (res + fatsz) * SECTOR]
    root_off = base + (res + nfats * fatsz) * SECTOR
    data_off = root_off + root_entries * 32

    def chain(first):
        out, c = [first], first
        while True:
            nxt = struct.unpack_from("<H", fat, c * 2)[0]
            if nxt >= 0xFFF8:
                return out
            out.append(nxt)
            c = nxt

    def read_file(e):
        first = struct.unpack_from("<H", e, 26)[0]
        length = struct.unpack_from("<I", e, 28)[0]
        data = bytearray()
        for c in chain(first):
            off = data_off + (c - 2) * spc * SECTOR
            data += img[off:off + spc * SECTOR]
        return bytes(data[:length])

    def walk(dir_off, count, prefix=""):
        files = {}
        for i in range(count):
            e = img[dir_off + i * 32: dir_off + i * 32 + 32]
            if e[0] == 0:
                break
            if e[0] == 0xE5 or e[11] == 0x0F or e[0] == 0x2E:  # deleted / LFN / . ..
                continue
            name = e[0:8].decode().rstrip()
            if e[8:11].strip():
                name += "." + e[8:11].decode().rstrip()
            if e[11] & 0x10:
                sub = struct.unpack_from("<H", e, 26)[0]
                files.update(walk(data_off + (sub - 2) * spc * SECTOR, spc * SECTOR // 32,
                                  prefix + name + "/"))
            else:
                files[prefix + name] = read_file(e)
        return files

    # Map the source tree case-insensitively: FAT stores names uppercase.
    source = {}
    for root, _dirs, names in os.walk(src):
        for n in names:
            path = os.path.join(root, n)
            source[os.path.relpath(path, src).lower()] = path

    files = walk(root_off, root_entries)
    # Firmware may have written its variable store into the volume when the
    # image was booted; it is not part of the source tree.
    files.pop("NVVARS", None)
    ok = len(files) > 0
    for name, data in sorted(files.items()):
        path = source.get(name.lower())
        same = path is not None and open(path, "rb").read() == data
        ok &= same
        print(f"  {name:24s} {len(data):8d} bytes  {'OK' if same else 'MISMATCH'}")
    print(f"verified {len(files)} files from {img_path}: {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(verify(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "build/esp"))
