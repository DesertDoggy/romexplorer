# cia-tool

The library of cia-tool, a native Rust port of [Batch CIA 3DS Decryptor Redux](https://github.com/xxmichibxx/Batch-CIA-3DS-Decryptor-Redux).
It decrypts Nintendo 3DS `.cia` and `.3ds`/`.cci` files without `ctrtool.exe`, `makerom.exe` or `decrypt.exe`:
pure Rust, no external programs.

This copy is the library alone; its command-line front end is left out. romexplorer's copy jobs and chdlite use it
through `romexplorer::rom::n3ds`, which adds the `.cxi` output and Azahar's compressed `.zcci` / `.zcia` / `.zcxi` /
`.z3dsx` files on top.

File names are never renamed or stripped: Japanese, Chinese, Korean, Cyrillic, emoji and other non-ASCII names work
as they are.

## Keys (not included)

No console keys are shipped. The user provides their own, dumped from their console:

* `aes_keys.txt` in the Citra / Azahar / Lime3DS format. Needed entries:
  `slot0x2CKeyX`, `slot0x25KeyX`, `slot0x18KeyX`, `slot0x1BKeyX` (NCCH), `slot0x3DKeyX` and `common0`..`common5` (CIA title keys).
  Optional: `fixedSys=` for the rare system titles that use the fixed system key.
* or `boot9.bin` / `boot9_prot.bin` (provides KeyX 0x2C and 0x3D; the other entries still come from `aes_keys.txt`).
* `seeddb.bin` for games that use seed crypto (titles released after system update 9.6).

romexplorer and chdlite look for these beside the file being converted, beside the program, and in the app's own
folder, each also in its `assets` subfolder. Nothing asks for them.

## How it differs from the batch script

The batch script extracted each NCCH with `decrypt.exe` and rebuilt the CIA/CCI with `makerom`. This tool decrypts
in a single streaming pass instead:

* **CIA**: the title key is decrypted from the ticket with the common key, each content's CIA layer (AES-CBC) and
  NCCH layer (AES-CTR: ExHeader, ExeFS, RomFS, with 0x2C/0x25/0x18/0x1B keys, seeds and fixed keys) are removed,
  the NCCH flags are set to "no crypto", and the TMD content records get the decrypted flag and new SHA-256 hashes.
  Certificates, ticket, version and meta (icon) are kept, so the title version is preserved and DLC/updates keep
  all their contents. Signatures are not re-signed, the same as makerom's `-ignoresign`; emulators accept this.
* **CCI**: each partition is decrypted in place and the copy of the first NCCH header in the card info area is updated.
  Unused space after the last partition is trimmed.
* **CIA → CCI**: the NCSD header and card info are generated from the TMD, with content N placed in partition N.
* **DSi (TWL) CIAs**: the CIA layer is removed; the SRL inside is not NCCH-encrypted. The manual is kept.

## Tests

`cargo test -p cia-tool` runs unit tests (text encodings, naming, key scrambler) and an end-to-end test that
builds encrypted CIA files with fake keys under Japanese, Cyrillic and emoji file names and checks the decrypted
output byte for byte. The `testkit` feature exposes those builders to romexplorer's own tests.

## Credits

* Batch CIA 3DS Decryptor Redux by xxmichibxx, based on Batch CIA 3DS Decryptor by matiffeder
* Format details from 3dbrew, Project_CTR (ctrtool/makerom) and pyctr by ihaveamac

## License

AGPL-3.0-or-later, the same as romexplorer (`LICENSE` at the repository root).
