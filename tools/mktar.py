#!/usr/bin/env python3
"""Pack files into a deterministic ustar archive for the RondOS boot image.

The kernel's tar reader (`kernel64/src/exec.rs`) is intentionally tiny: flat
names, regular files only, no GNU extensions.  Using this script instead of
`tar` keeps the archive free of `./` prefixes, owner names and timestamps that
would only make the reader bigger.

    mktar.py out.tar init.elf=build/user/init.elf crash.elf=build/user/crash.elf
"""
import sys


def octal(value: int, width: int) -> bytes:
    return ("%0*o" % (width - 1, value)).encode() + b"\0"


def header(name: bytes, size: int) -> bytes:
    if len(name) > 100:
        raise SystemExit(f"name too long for ustar: {name!r}")
    hdr = bytearray(512)
    hdr[0:len(name)] = name
    hdr[100:108] = octal(0o644, 8)          # mode
    hdr[108:116] = octal(0, 8)              # uid
    hdr[116:124] = octal(0, 8)              # gid
    hdr[124:136] = octal(size, 12)          # size
    hdr[136:148] = octal(0, 12)             # mtime
    hdr[148:156] = b" " * 8                 # checksum placeholder
    hdr[156:157] = b"0"                     # regular file
    hdr[257:263] = b"ustar\0"
    hdr[263:265] = b"00"
    checksum = sum(hdr)
    hdr[148:156] = ("%06o" % checksum).encode() + b"\0 "
    return bytes(hdr)


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    out = sys.argv[1]
    with open(out, "wb") as archive:
        for spec in sys.argv[2:]:
            name, _, path = spec.partition("=")
            if not path:
                name, path = name.rsplit("/", 1)[-1], name
            with open(path, "rb") as src:
                data = src.read()
            archive.write(header(name.encode(), len(data)))
            archive.write(data)
            archive.write(b"\0" * ((512 - len(data) % 512) % 512))
        archive.write(b"\0" * 1024)  # end-of-archive
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
