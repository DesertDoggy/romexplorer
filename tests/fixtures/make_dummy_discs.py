#!/usr/bin/env python3
"""Writes small dummy disc images for exercising the native libraries' conversions.

No game data: every image is generated here, filled with a deterministic pattern, and is only
shaped like the real thing as far as the converters look -- enough for chdman, dolphinrvz and
the scan's detection, never enough to boot. Real dumps can't be committed or uploaded, so this
is what the stream tests run on.

    python3 tests/fixtures/make_dummy_discs.py <out dir>

Writes:
    cd/game.cue + game (track 1).bin, game (track 2).bin   MODE1/2352 data + an audio track
    gdi/game.gdi + track01.bin, track02.raw, track03.bin   a three-track GD-ROM layout
    ps2/game.iso      ISO 9660 with SYSTEM.CNF (BOOT2 = cdrom0:\\SLUS_000.00;1)
    psp/game.iso      ISO 9660 with UMD_DATA.BIN and PSP_GAME/PARAM.SFO
    gc/game.iso       a GameCube disc header and an empty FST, 8 MiB
    wii/game.iso      a Wii disc header with no partitions, 16 MiB

The ISO 9660 images need pycdlib (pip install pycdlib).
"""

import os
import struct
import sys

SECTOR = 2352


def pattern(n, seed):
    """n bytes of a cheap repeatable pattern, compressible but not all one byte."""
    out = bytearray(n)
    x = seed & 0xFFFFFFFF or 1
    for i in range(0, n, 4):
        x ^= (x << 13) & 0xFFFFFFFF
        x ^= x >> 17
        x ^= (x << 5) & 0xFFFFFFFF
        # Keep the high bits low so the data compresses like real disc data, roughly.
        out[i:i + 4] = struct.pack("<I", x & 0x0F0F0F0F)[: n - i]
    return bytes(out)


def mode1_sectors(count, start_lba, seed):
    """MODE1/2352 sectors: sync, BCD MSF header, mode byte, 2048 bytes of data, zeroed EDC/ECC."""
    out = bytearray()
    data = pattern(2048 * count, seed)
    for i in range(count):
        lba = start_lba + i + 150
        m, s, f = lba // (60 * 75), (lba // 75) % 60, lba % 75
        bcd = lambda v: ((v // 10) << 4) | (v % 10)
        out += b"\x00" + b"\xff" * 10 + b"\x00"
        out += bytes([bcd(m), bcd(s), bcd(f), 1])
        out += data[i * 2048:(i + 1) * 2048]
        out += b"\x00" * 288
    return bytes(out)


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data)


def make_cd(root):
    write(os.path.join(root, "cd", "game (track 1).bin"), mode1_sectors(600, 0, 1))
    write(os.path.join(root, "cd", "game (track 2).bin"), pattern(SECTOR * 300, 2))
    with open(os.path.join(root, "cd", "game.cue"), "w", newline="\r\n") as f:
        f.write('FILE "game (track 1).bin" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n')
        f.write('FILE "game (track 2).bin" BINARY\n  TRACK 02 AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n')


def make_gdi(root):
    d = os.path.join(root, "gdi")
    write(os.path.join(d, "track01.bin"), mode1_sectors(300, 0, 3))
    write(os.path.join(d, "track02.raw"), pattern(SECTOR * 300, 4))
    # The high-density area starts at LBA 45000 on every GD-ROM.
    write(os.path.join(d, "track03.bin"), mode1_sectors(1200, 45000, 5))
    with open(os.path.join(d, "game.gdi"), "w", newline="\r\n") as f:
        f.write("3\n1 0 4 2352 track01.bin 0\n2 450 0 2352 track02.raw 0\n3 45000 4 2352 track03.bin 0\n")


def make_iso(path, files, volume):
    import io
    import pycdlib

    iso = pycdlib.PyCdlib()
    iso.new(interchange_level=3, vol_ident=volume)
    dirs = set()
    for name, data in files:
        parts = name.split("/")
        for i in range(1, len(parts)):
            d = "/" + "/".join(parts[:i])
            if d not in dirs:
                iso.add_directory(d)
                dirs.add(d)
        iso.add_fp(io.BytesIO(data), len(data), "/" + name + ";1")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    iso.write(path)
    iso.close()


def param_sfo(entries):
    """A PARAM.SFO holding string entries only."""
    keys = b""
    data = b""
    index = b""
    for key, value in entries:
        v = value.encode() + b"\x00"
        padded = v + b"\x00" * ((-len(v)) % 4)
        index += struct.pack("<HHIII", len(keys), 0x0204, len(v), len(padded), len(data))
        keys += key.encode() + b"\x00"
        data += padded
    keys += b"\x00" * ((-len(keys)) % 4)
    key_off = 20 + len(index)
    return struct.pack("<4sIIII", b"\x00PSF", 0x101, key_off, key_off + len(keys), len(entries)) + index + keys + data


def make_ps2(root):
    cnf = b"BOOT2 = cdrom0:\\SLUS_000.00;1\r\nVER = 1.00\r\nVMODE = NTSC\r\n"
    make_iso(os.path.join(root, "ps2", "game.iso"), [("SYSTEM.CNF", cnf), ("SLUS_000.00", pattern(1 << 20, 6))], "DUMMY_PS2")


def make_psp(root):
    sfo = param_sfo([("CATEGORY", "UG"), ("DISC_ID", "ULUS00000"), ("TITLE", "Dummy")])
    make_iso(os.path.join(root, "psp", "game.iso"), [("UMD_DATA.BIN", b"ULUS-00000|0000000000000001|0001|G"), ("PSP_GAME/PARAM.SFO", sfo), ("PSP_GAME/SYSDIR/EBOOT.BIN", pattern(1 << 20, 7))], "UMD")


def make_gc(root):
    size = 8 << 20
    disc = bytearray(pattern(size, 8))
    header = bytearray(0x440)
    header[0:6] = b"GXXE01"
    header[0x1C:0x20] = struct.pack(">I", 0xC2339F3D)
    header[0x20:0x20 + 11] = b"Dummy disc\x00"
    fst_offset = 0x10000
    struct.pack_into(">III", header, 0x424, fst_offset, 0x0C + 1, 0x0C + 1)
    disc[0:0x440] = header
    # An FST holding only its root: no files.
    disc[fst_offset:fst_offset + 0x0D] = struct.pack(">BBHII", 1, 0, 0, 0, 1) + b"\x00"
    write(os.path.join(root, "gc", "game.iso"), bytes(disc))


def make_wii(root):
    """A Wii disc header with an empty partition table: no partition means nothing to decrypt,
    so no console key is needed, and Dolphin still treats it as a Wii disc."""
    size = 16 << 20
    disc = bytearray(pattern(size, 9))
    header = bytearray(0x440)
    header[0:6] = b"RXXE01"
    header[0x18:0x1C] = struct.pack(">I", 0x5D1C9EA3)
    header[0x20:0x20 + 15] = b"Dummy Wii disc\x00"
    disc[0:0x440] = header
    # Four partition groups, all empty.
    disc[0x40000:0x40020] = b"\x00" * 0x20
    # Region info.
    disc[0x4E000:0x4E020] = b"\x00" * 0x20
    write(os.path.join(root, "wii", "game.iso"), bytes(disc))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = sys.argv[1]
    make_cd(root)
    make_gdi(root)
    make_gc(root)
    make_wii(root)
    make_ps2(root)
    make_psp(root)
    print(root)


if __name__ == "__main__":
    main()
