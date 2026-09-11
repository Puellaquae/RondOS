#!/usr/bin/env python3
"""Print the RondOS boot records from a `\\rondos\\bootlog.bin` dump.

The UEFI loader keeps one fixed 40-byte record per boot in that file (newest
first), which is the only channel that survives both a reset *and* a firmware
that rewrites CMOS during POST.  Copy the file off the stick and run this:

    python3 tools/dumplog.py bootlog.bin
"""

import struct
import sys

MAGIC = 0x4452_4252
VERSION = 1
MEM_SHOWN = 24
REC = 40 + MEM_SHOWN * 24
KEEP = 16

STAGES = {
    0x01: "entered kmain",
    0x02: "BootInfo adopted",
    0x03: "kernel-owned CR3",
    0x04: "framebuffer console",
    0x05: "self_check",
    0x06: "GDT/TSS/percpu/IDT",
    0x07: "scheduler bring-up",
    0x08: "interrupts on (sti)",
    0x09: "starting /bin/init",
    0x10: "bring_up_scheduler",
    0x11: "PIC remapped",
    0x12: "PIT programmed",
    0x13: "scheduler + idle stack",
    0x15: "interrupts on (sti)",
    0xFF: "idle (clean)",
}


def parse_mem(raw):
    """Decode the [addr, len, kind] triples."""
    return [(a, l, k) for a, l, k in raw]


def main(argv):
    show_mem = "--mem" in argv
    argv = [a for a in argv if a != "--mem"]
    if len(argv) != 2:
        raise SystemExit(__doc__)
    data = open(argv[1], "rb").read()
    magic, version, boots, _pad = struct.unpack_from("<IIII", data, 0)
    if magic != MAGIC or version != VERSION:
        raise SystemExit(f"not a RondOS boot log (magic {magic:#x} version {version})")
    print(f"{boots} boot(s) recorded")
    for i in range(KEEP):
        off = 16 + i * REC
        if off + REC > len(data):
            break
        boot, stage, mask, count, sig, lo, hi, mem_count = struct.unpack_from("<IIIIIIII", data, off)
        mem = parse_mem([struct.unpack_from("<QQQ", data, off + 36 + i * 24) for i in range(MEM_SHOWN)])
        if boot == 0:
            break
        lo_mask, hi_mask = mask & 0xFF, (mask >> 8) & 0xFF
        stages = [str(b) for b in range(16) if lo_mask & (1 << b)]
        marks = [f"{b + 16:#04x}" for b in range(16) if hi_mask & (1 << b)]
        what = STAGES.get(stage, f"mark {stage:#04x}")
        print(
            f"  #{boot:<3} last stage {stage:#04x} ({what})  "
            f"stages [{' '.join(stages)}]  marks [{' '.join(marks)}]  "
            f"writes {count}  sig {sig:#04x}  mem {mem_count} entries"
        )
        if show_mem:
            usable = sum(l for _a, l, k in mem[:mem_count] if k == 1)
            print(f"        usable shown: {usable / 1024 / 1024:.0f} MiB")
            for a, l, k in mem[:mem_count]:
                if k == 1 or l >= 0x100000:
                    print(f"        {a:#012x} len {l:#010x} kind {k}")


if __name__ == "__main__":
    main(sys.argv)
