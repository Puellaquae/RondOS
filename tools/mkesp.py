#!/usr/bin/env python3
"""Build a bootable EFI System Partition image from a directory tree.

`mtools` and `mkfs.vfat` are not available everywhere (and are overkill for a
handful of 8.3-named files), so this writes a FAT16 filesystem directly:

    tools/mkesp.py build/esp build/rondos-esp.img [size_mb]

The result is a **hybrid** image: an MBR at LBA 0 with one type-0x0E partition
starting at LBA 2048 and the FAT16 volume inside it.  `dd` it to a USB stick and
any UEFI firmware finds `\\EFI\\BOOT\\BOOTX64.EFI`; `dd` it to a partition and the
filesystem alone is valid too.

Only short (8.3) names are supported — everything RondOS ships fits, and UEFI
looks the boot file up case-insensitively.
"""
import os
import struct
import sys

SECTOR = 512
PART_LBA = 2048
ATTR_DIR = 0x10
ATTR_ARCHIVE = 0x20
VOLUME_LABEL = b"RONDOS     "


def short_name(name):
    """Encode a name as an 8.3 directory entry name (uppercase, space padded)."""
    if name in (".", ".."):
        return name.encode().ljust(11)
    stem, dot, ext = name.rpartition(".")
    if not dot:
        stem, ext = name, ""
    stem, ext = stem.upper(), ext.upper()
    if len(stem) > 8 or len(ext) > 3 or not stem:
        raise SystemExit(f"name {name!r} does not fit 8.3 (no long-name support)")
    allowed = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&'()-@^_`{}~"
    for ch in stem + ext:
        if ch not in allowed:
            raise SystemExit(f"character {ch!r} not allowed in {name!r}")
    return stem.ljust(8).encode() + ext.ljust(3).encode()


class Fat16:
    def __init__(self, size_bytes, hidden_sectors=PART_LBA):
        # The whole image: MBR at 0, FAT16 volume at PART_LBA.
        self.base = PART_LBA * SECTOR
        self.sectors = size_bytes // SECTOR - PART_LBA
        self.spc = 4                      # 2 KiB clusters
        self.reserved = 1
        self.nfats = 2
        self.root_entries = 512
        self.root_sectors = (self.root_entries * 32 + SECTOR - 1) // SECTOR
        self.fat_sectors = 1
        clusters = 0
        while True:
            data_sectors = (self.sectors - self.reserved - self.nfats * self.fat_sectors
                            - self.root_sectors)
            clusters = data_sectors // self.spc
            need = ((clusters + 2) * 2 + SECTOR - 1) // SECTOR
            if need <= self.fat_sectors:
                break
            self.fat_sectors = need
        if clusters >= 65525:
            raise SystemExit("image too large for FAT16")
        self.clusters = clusters
        self.hidden = hidden_sectors
        self.data = bytearray(size_bytes)
        self.fat = [0] * (clusters + 2)
        self.fat[0] = 0xFFF8
        self.fat[1] = 0xFFFF
        self.next_cluster = 2
        self.root = []
        self._write_boot_sector()
        self._write_partition_table()

    # ---------------------------------------------------------------- layout
    def _data_offset(self, cluster):
        first = self.reserved + self.nfats * self.fat_sectors + self.root_sectors
        return self.base + (first + (cluster - 2) * self.spc) * SECTOR

    def _write_boot_sector(self):
        bs = bytearray(SECTOR)
        bs[0:3] = b"\xEB\x3C\x90"
        bs[3:11] = b"RONDOS  "
        struct.pack_into("<HBHBHHBHHHII", bs, 11,
                         SECTOR, self.spc, self.reserved, self.nfats,
                         self.root_entries,
                         self.sectors if self.sectors < 0x10000 else 0,
                         0xF8, self.fat_sectors, 32, 64, self.hidden,
                         self.sectors if self.sectors >= 0x10000 else 0)
        bs[36] = 0x00
        bs[38] = 0x29
        struct.pack_into("<I", bs, 39, 0x524F4E44)
        bs[43:54] = VOLUME_LABEL
        bs[54:62] = b"FAT16   "
        bs[510:512] = b"\x55\xAA"
        self.data[self.base:self.base + SECTOR] = bs

    def _write_partition_table(self):
        entry = bytearray(16)
        entry[0] = 0x80
        entry[1:4] = b"\x00\x20\x21"
        entry[4] = 0xEF                       # EFI System Partition
        entry[5:8] = b"\xFE\xFF\xFF"
        struct.pack_into("<II", entry, 8, PART_LBA, self.sectors)
        self.data[446:462] = entry
        self.data[510:512] = b"\x55\xAA"

    # ------------------------------------------------------------- allocation
    def _alloc(self, count):
        first = self.next_cluster
        for i in range(count):
            self.fat[first + i] = first + i + 1
        self.fat[first + count - 1] = 0xFFFF
        self.next_cluster += count
        if self.next_cluster > self.clusters + 1:
            raise SystemExit("image is full")
        return first

    def _write_chain(self, first, data):
        c, off = first, 0
        while off < len(data):
            n = min(len(data) - off, self.spc * SECTOR)
            base = self._data_offset(c)
            self.data[base:base + n] = data[off:off + n]
            off += n
            c = self.fat[c]

    @staticmethod
    def _entry(name, attr, cluster, size):
        e = bytearray(32)
        e[0:11] = short_name(name)
        e[11] = attr
        struct.pack_into("<H", e, 26, cluster & 0xFFFF)
        struct.pack_into("<I", e, 28, size)
        return e

    def add_file(self, path):
        with open(path, "rb") as fh:
            data = fh.read()
        clusters = max(1, (len(data) + self.spc * SECTOR - 1) // (self.spc * SECTOR))
        first = self._alloc(clusters)
        self._write_chain(first, data)
        return first, len(data)

    def add_dir(self, path):
        children = []
        for name in sorted(os.listdir(path)):
            child = os.path.join(path, name)
            if os.path.isdir(child):
                children.append(self._entry(name, ATTR_DIR, self.add_dir(child), 0))
            else:
                cluster, size = self.add_file(child)
                children.append(self._entry(name, ATTR_ARCHIVE, cluster, size))
        entries = 2 + len(children)
        clusters = max(1, (entries * 32 + self.spc * SECTOR - 1) // (self.spc * SECTOR))
        first = self._alloc(clusters)
        raw = self._entry(".", ATTR_DIR, first, 0) + self._entry("..", ATTR_DIR, 0, 0)
        for e in children:
            raw += e
        self._write_chain(first, raw)
        return first

    #: Firmware scratch files that must not end up in the image.
    SKIP = {"NvVars", "nvram.txt"}

    def build_root(self, src):
        entries = []
        for name in sorted(os.listdir(src)):
            if name in self.SKIP or name.startswith("."):
                print(f"  - skipping {name}")
                continue
            path = os.path.join(src, name)
            if os.path.isdir(path):
                cluster = self.add_dir(path)
                entries.append(self._entry(name, ATTR_DIR, cluster, 0))
                print(f"  + {name}/")
            else:
                cluster, size = self.add_file(path)
                entries.append(self._entry(name, ATTR_ARCHIVE, cluster, size))
                print(f"  + {name} ({size} bytes)")
        self.root = entries

    def finish(self):
        root = bytearray()
        for e in self.root:
            root += e
        root_off = self.base + (self.reserved + self.nfats * self.fat_sectors) * SECTOR
        self.data[root_off:root_off + len(root)] = root
        fat = bytearray()
        for v in self.fat:
            fat += struct.pack("<H", v)
        for i in range(self.nfats):
            off = self.base + (self.reserved + i * self.fat_sectors) * SECTOR
            self.data[off:off + len(fat)] = fat


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    src, dst = sys.argv[1], sys.argv[2]
    size_mb = int(sys.argv[3]) if len(sys.argv) > 3 else 64
    fs = Fat16(size_mb * 1024 * 1024)
    print(f"FAT16: {fs.sectors} sectors, {fs.clusters} clusters, "
          f"{fs.fat_sectors} sectors/FAT, partition at LBA {PART_LBA}")
    fs.build_root(src)
    fs.finish()
    with open(dst, "wb") as fh:
        fh.write(fs.data)
    print(f"wrote {dst} ({len(fs.data)} bytes)")


if __name__ == "__main__":
    sys.exit(main())
