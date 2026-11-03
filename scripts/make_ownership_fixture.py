#!/usr/bin/env python3
"""Builds the ownership-badge fixture: one shared catalogue, three machines holding
different subsets of it.

Eight entries named after which machines hold them -- `test LMP`, where the three bits are
**L**ocal (the client), **M**ain (the main server) and **P**eer (a share). So `test 101` is
held locally and by a peer, but *not* by the main server, which is the case the loud badge
exists for; `test 000` is in the catalogue and held by nobody.

    name       local  main  peer   what it should look like on the client
    test 000     .     .     .     nothing
    test 001     .     .     x     remote-only (peer), LOUD (not on server)
    test 010     .     x     .     remote-only (main), quiet
    test 011     .     x     x     remote-only, quiet
    test 100     x     .     .     LOUD -- held here, the main server lacks it
    test 101     x     .     x     LOUD -- a peer having it must not soften this
    test 110     x     x     .     quiet -- backed up
    test 111     x     x     x     quiet -- backed up

Everything written is synthetic: the names are literally the bit patterns, the files are
generated, and all of it lands in git-ignored tests/output/.

The catalogue half is one DAT listing all eight, imported identically on all three machines
-- which is what makes the mirror work at all, since a mirrored file row references a
`content` row that has to already exist locally. The ownership half is the per-machine
directory, holding only that machine's subset of the same files.
"""

import hashlib
import pathlib
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "tests" / "output" / "fixture"
# Which bit belongs to which machine, most significant first.
MACHINES = ("local", "main", "peer")


def rom_bytes(name: str) -> bytes:
    """A minimally plausible iNES image, distinct per name so the hashes differ."""
    header = b"NES\x1a" + bytes([1, 1, 0, 0]) + bytes(8)
    body = (name.encode() + b" romexplorer ownership fixture ").ljust(16384 + 8192, b"\x00")
    return header + body[: 16384 + 8192]


def main() -> None:
    combos = [f"{i:03b}" for i in range(8)]
    OUT.mkdir(parents=True, exist_ok=True)
    pool = OUT / "roms"
    pool.mkdir(exist_ok=True)

    entries = []
    for bits in combos:
        name = f"test {bits}"
        data = rom_bytes(name)
        path = pool / f"{name}.nes"
        path.write_bytes(data)
        entries.append(
            {
                "name": name,
                "file": path.name,
                "size": len(data),
                "crc": f"{zlib.crc32(data) & 0xFFFFFFFF:08x}",
                "md5": hashlib.md5(data).hexdigest(),
                "sha1": hashlib.sha1(data).hexdigest(),
            }
        )

    # One DAT listing all eight: the shared catalogue every machine imports.
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">',
        "<datafile>",
        "\t<header>",
        "\t\t<name>Nintendo - Nintendo Entertainment System</name>",
        "\t\t<description>romexplorer ownership fixture</description>",
        "\t\t<version>fixture-1</version>",
        "\t\t<author>romexplorer</author>",
        "\t</header>",
    ]
    for e in entries:
        lines += [
            f'\t<game name="{e["name"]}">',
            f'\t\t<description>{e["name"]}</description>',
            f'\t\t<rom name="{e["file"]}" size="{e["size"]}" crc="{e["crc"]}" md5="{e["md5"]}" sha1="{e["sha1"]}"/>',
            "\t</game>",
        ]
    lines.append("</datafile>")
    dat = OUT / "ownership-fixture.dat"
    dat.write_text("\n".join(lines) + "\n", encoding="utf-8")

    # One directory per machine, holding only that machine's subset. Hard links, so every
    # machine's copy is byte-identical and hashes to the same `content` row -- which is the
    # property the whole mirror depends on.
    for slot, machine in enumerate(MACHINES):
        d = OUT / machine
        d.mkdir(exist_ok=True)
        for old in d.glob("*.nes"):
            old.unlink()
        held = []
        for bits in combos:
            if bits[slot] == "1":
                src = pool / f"test {bits}.nes"
                (d / src.name).hardlink_to(src)
                held.append(f"test {bits}")
        print(f"{machine:6} holds {len(held)}: {', '.join(held)}")

    print(f"\ndat  {dat}")
    print(f"dirs {OUT}/<{'|'.join(MACHINES)}>")


if __name__ == "__main__":
    main()
