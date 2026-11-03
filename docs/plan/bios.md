# BIOS/system-file heuristic

## Purpose and scope

A **size-based pre-filter**, used when scanning a directory already believed to belong to
one known platform, to flag "this particular file is small enough to plausibly be a BIOS/
system file rather than a ROM dump" before doing anything more expensive. Confirmation is
always by hash afterward -- against this project's own `retrobios`-sourced data
(`database::dat_parse::parse_retrobios_json`, already wired into the `hashes`/`content`
tables via the `retrobios` DAT source) -- never by size alone. Size only decides whether a
candidate is worth hash-checking at all.

**Out of scope, deliberately**: a mixed collection spanning multiple platforms, or any file
whose platform isn't already known, should not use this heuristic at all -- hash the file
first and match it against the DAT/retrobios database directly (the general path this
project already has). This size heuristic only earns its keep in the narrower case: within
one already-identified platform's directory, cheaply separating "likely BIOS, worth a hash
check" from "clearly ROM-sized, skip the check" without hashing every file up front.

## Data source and confidence

Every size below marked *(retrobios)* was read directly out of this project's own
`retrobios` database (`app_root_ex/assets/download/dats/retrobios/database_*.json`), the
same file already integrated as a DAT source -- not guessed, not taken secondhand from a
web page. A few platforms retrobios's catalog doesn't clearly cover (or where its listing
mixes in non-BIOS files under the same folder) are marked *(other sources)* with the
specific reference. Where retrobios's own folder grouping mixes real BIOS files with
unrelated data (game ROMs, arcade sets, peripheral audio, emulator distribution archives),
those contaminating entries are called out and excluded, not used as the reported max.

## Per-platform system files

### 3DS
`aes_keys.txt`, `keys.txt`, `aeskeydb.bin`, `boot9.bin`, `boot11.bin`, `dspfirm.cdc`,
`otp.bin`, `sector0z96.bin`, `seeddb.bin`, `shared_font.bin`, `movable.sed`,
`secret_sector.bin` -- 256B-64KB for the key/seed files, plus `shared_font.bin` **3MB**
*(retrobios)* -- a real required Citra system file, much larger than the rest of this
platform's files; don't drop it from the max just because it's an outlier.

### 64DD (Nintendo64Dd)
`IPL.z64` 4MB *(retrobios: `64DD_IPL_*.bin`, 4,194,304 bytes, consistent across
dev/USA/JPN revisions)`
`DriveController.bin` 16-32KB

### Dreamcast
`Boot.bin`/`Bios.bin` **2MB** *(retrobios: `dc_bios.bin`/`naomi_boot.bin`, 2,097,152 bytes)*
-- corrects an earlier draft of this doc, which had this at 4MB.
`Flash.bin` 128KB

### Famicom Disk System
`DiskSystem.rom`/`Bios.bin` 8KB *(retrobios)*

### Game Boy
`BootRom.gb`/`Bios.bin` 256B

### Game Boy Color
`BootRom.gbc`/`Bios.bin` 2.3KB
Challenger GB/Genius GB Backup Station/Touch Boy System ROM up to 512KB

### Game Boy Advance
`Bios.gba`/`Bios.bin` 16KB *(retrobios)*

### Genesis/Mega Drive (SegaGenesis)
`Bios.md`/`Bios.bin` 2KB
`Teradrive.md` 4KB
WonderMega/M2 128KB
(SegaCd's own BIOS is listed separately below -- SegaCd is its own platform in this
project's `Platform` enum, not folded into Genesis.)

### Sega CD (SegaCd)
`Bios.md`/`Bios.bin` **128KB** *(retrobios: `us_scd2_9306.bin`/`eu_mcd2_9306.bin`/
`jp_mcd1_9112.bin`, 131,072 bytes, consistent across regions)*

### Nintendo DS
`Firmware.bin` 256-512KB
`Bios7.bin` 16KB
`Bios9.bin` 4KB

### Neo-Geo CD (NeoGeoCd)
`Bios.bin` 512KB *(retrobios)*

### Neo-Geo Pocket / Neo-Geo Pocket Color
`Bios.bin` 64KB each *(retrobios: `boot.rom` and both regional BIOS dumps, 65,536 bytes)*

### GameCube
`IPL.bin` 2MB *(retrobios)*
`DVDbios.bin` 128KB *(retrobios: `gc-dvd-*.bin`, 131,072 bytes)*

### Nintendo DSi
`CID.bin` 16B
`bios7i.bin` 64KB
`bios9i.bin` 64KB
`firmware.bin` 128KB
`nogbafooter.bin` 64B
`NAND.bin` 240MB -- NAND-class, not a simple BIOS; see "NAND/large-firmware class" below.
`Sdcard.bin` 512MB -- same, SD-card-class, open-ended in practice.

### PocketStation
`Firmware.bin` 16KB

### PS1 (PlayStation)
`Bios.bin` 512KB *(retrobios: e.g. `scph5501.bin`/`scph1001_v20.bin`, 524,288 bytes,
overwhelmingly the standard size; one rare devkit-class variant, `scph5903.bin`, is 1MB --
noted, not used as the representative max)*

### PS2 (PlayStation2)
`Bios.bin` (rom1) 4MB *(retrobios: uniformly 4,194,304 bytes across every ROM revision
checked -- this is the platform the top-level `MAX_BIOS_SIZE` constant is based on)*
`DVDbios.bin` 2MB
`Bios.bin` (rom1/2) 512KB

### PS3 (PlayStation3)
`PS3UPDAT.PUP` 196.6MB *(retrobios: 206,126,236 bytes -- confirms the figure already in
this doc)* -- firmware-package-class, not a simple BIOS; see below.

### PS Vita (PlayStationVita)
`PSVUPDAT.PUP` 127.6MB *(retrobios: 133,834,240 bytes -- confirms the figure already in
this doc; a smaller `PSP2UPDAT.PUP` variant, ~54.1MB, also exists)* --
firmware-package-class.

### Sega Saturn (SegaSaturn)
`Bios.bin` 512KB *(retrobios: e.g. `SAT_1.01-(J).bin`/`mpr-18100.bin`, 524,288 bytes)*

### Sega 32X (Sega32x)
`32X_M_BIOS.BIN` (Master) 2KB, `32X_S_BIOS.BIN` (Slave) 1KB, `32X_G_BIOS.BIN` (security/
Genesis-side) 256B *(retrobios, confirmed)*. **Correction**: an earlier draft of this doc
had Master/Slave at 2MB/1MB -- off by roughly 1000x; the real files are kilobyte-scale.

### Super Famicom (SuperFamicom)
`SufamiCDromBootRom.sfc` 128KB
`DSPetc.bin` 3-160KB (co-processor dumps, size varies by chip)
`SuperGameboyEnhancementChip.bin` 256B

### Sufami Turbo (SufamiTurbo)
`Bios.sfc`/`Bios.bin` 256KB

### Sega Master System (SegaMasterSystem)
`Bios.sms`/`Bios.bin` 8KB *(retrobios: `japanese-bios.sms`, 8,192 bytes -- the standard
size)*. One regional outlier exists (`SMS_kr.sms`, 128KB) -- noted, not used as the
representative max, since 8KB is what every mainstream emulator's BIOS folder expects.

### TurboGrafx-CD / PC Engine CD-ROM² (TurboGrafxCd)
`GamesExpressCDCard.pce`/`.bin` 16-32KB *(retrobios: `gecard.pce`, 32,768 bytes)*
`SystemCard.pce`/`SystemCard.bin` 256KB *(retrobios: `syscard3.pce` etc, 262,144 bytes)*

### WonderSwan
`BootRom.ws`/`BootRom.bin` 4KB *(retrobios)*

### WonderSwan Color
`WonderSwanColorBootRom.wsc`/`.bin` 8KB, `SwanCrystalBootRom.wsc`/`.bin` 8KB *(retrobios,
both confirmed at 8,192 bytes)*

### Xbox
`Bios.bin` 256KB-1MB *(retrobios: several real dumps at exactly 1,048,576 bytes, confirming
the upper end of this range)*

### Nintendo Switch (NintendoSwitch)
`dev.keys` 10.1KB
`key_retail.bin` 160B
`prod.keys` ~15-16KB *(retrobios: 16,025 bytes, close to the figure already here)*
`title.keys` variable length -- grows with how many titles' keys are included, not a fixed
size; don't treat it as a ceiling.
Full firmware/NAND packages run into the hundreds of MB -- NAND-class, see below.

### Game Gear (SegaGameGear)
`Bios.gg`/`Bios.bin` 1KB *(retrobios)*

### MSX
`MSX.rom` 32KB
`KANJI.rom` 128KB *(retrobios, confirmed)*

### MSX2
`MSX2.rom` 32KB
`MSX2EXT.rom` 16KB
`MSXDOS2.rom` 64KB *(retrobios, confirmed)*

### MSX2+
`MSX2P.rom` 32KB
`MSX2PEXT.rom` 16KB

### Nintendo Switch / Switch 2
NAND-class only (no small standalone BIOS file convention documented) -- see below.

---

## Platforms added (researched against retrobios; not in the original list)

### Atari 5200
`5200.rom` 2KB *(retrobios: `ROM_5200_A`/`5200.rom`, 2,048 bytes)*

### Atari 7800
`7800 BIOS (U).rom` 4KB (NTSC), `7800 BIOS (E).rom` 16KB (PAL) *(retrobios, both
confirmed)*

### Atari Jaguar (AtariJaguar)
`boot.rom`/`boot2.rom` 128KB, `boot1.rom` 256KB *(retrobios, confirmed; also cross-checked
against [RetroSix Wiki](https://www.retrosix.wiki/bios-boot-atari-jaguar)'s "1Mbit =
128KB" figure for the base unit's own boot ROM)*

### Atari Lynx (AtariLynx)
`lynxboot.img` 512B *(retrobios: `Atari_LYNX_boot.img`, 512 bytes)*

### Atari ST/STE (AtariStSte)
Original TOS ROMs 512KB (e.g. `tos306uk.img`/`tos404.img`/`falcon.img`, 524,288 bytes,
*retrobios*); EmuTOS/open-source replacements run up to 1MB (`emutos.img`) -- not original
hardware TOS, noted separately.

### Atari 8-bit (Atari8Bit -- 400/800/XL/XE)
OS ROM 16KB is standard (`ATARIXL.ROM` and most revisions, *retrobios*, 16,384 bytes); one
larger revision (`c101687.rom`) is 32KB.

### Sharp X68000 (SharpX68000)
`iplrom.dat` 128KB, `cgrom.dat` (character generator) 768KB *(retrobios, both confirmed;
cross-checked against [the retrobios repo's own file listing](https://github.com/Abdess/retrobios/blob/main/bios/Sharp/X68000/iplrom.dat))*

### FM Towns (FmTowns)
`FMT_DOS.ROM`/`FMT_DIC.ROM`/`FMT_F20.ROM` 512KB each, `FMT_FNT.ROM`/`FMT_SYS.ROM` 256KB
each *(retrobios, confirmed; matches the [RetroBat wiki](https://wiki.retrobat.org/systems-and-emulators/supported-game-systems/home-computer/fujitsu/fm-towns)'s
documented set)*. A combined `FMT_ALL.ROM` variant (~4MB) also exists.

### PC-8801 (Pc8801)
`PC8801 MKII SR BIOS.rom` 384KB, `KANJI1.ROM` 128KB *(retrobios, both confirmed)*

### PC-98 (Pc98)
`n88jisho.rom` 512KB, `font.rom` ~282KB, `bios.rom`/`bios9821.rom` 96KB *(retrobios,
confirmed)*

### PC-FX (PcFx)
`PCFX_bios.bin`/`pcfxv101.bin` 1MB *(retrobios, confirmed; matches [Mednafen's PC-FX
documentation](https://mednafen.github.io/documentation/pcfx.html))*

### 3DO (ThreeDo)
`panafz1.bin` and every other regional/devkit BIOS variant 1MB *(retrobios, uniformly
1,048,576 bytes across every 3DO BIOS in the catalog; matches
[trapexit/3do-bios](https://github.com/trapexit/3do-bios))*

### Pokemon Mini (PokemonMini)
`bios.min` 4KB *(retrobios, confirmed; matches [pokemon-mini.net's own BIOS
documentation](https://www.pokemon-mini.net/documentation/bios/))*

### Neo Geo MVS/AES arcade (NeoGeoMvs/NeoGeoAes)
P-ROM (program BIOS, e.g. `sp-j3.sp1`) 512KB *(retrobios)*; sound-CPU BIOS (`sm1.sm1`)
128KB (cross-checked via [neogeo-projects/neogeo-bios](https://github.com/neogeo-projects/neogeo-bios)).
The full standard MVS/AES BIOS set is several small ROM chips together, not one file --
512KB is the largest single chip.

### TurboGrafx-16 / SuperGrafx (cart-only)
No distinct BIOS file of their own -- only the CD add-on (`TurboGrafxCd`, above) needs a
system card.

## Explicitly out of scope (no single small system-file convention exists)

- **PS4, PS5, Xbox 360, Xbox One, Xbox Series** -- modern platforms with no standard
  single downloadable BIOS blob; what emulation exists (where it exists at all) works from
  many system modules/firmware images, not one file this heuristic could size-match.
- **PSP (PlayStationPortable)** -- the mainstream emulator (PPSSPP) uses HLE and needs no
  external BIOS file at all for most content.
- **Wii, Wii U** -- no single required small BIOS the way GameCube's `IPL.bin` is; a full
  NAND dump (hundreds of MB to several GB) is the only "system file" concept that exists,
  and Wii U additionally has large embedded font files (up to ~9.6MB) plus a
  variable-length `keys.txt` -- neither is a fixed-size BIOS blob.
- **PlayStationVr, GoogleStadia** -- no local hardware/BIOS concept applicable.
- **Arcade (general MAME), PcWindows, PcDos, Mac, Linux** -- arcade has thousands of
  per-board BIOS sets (out of this heuristic's scope entirely, unlike the one well-defined
  Neo Geo MVS/AES set above); general computing platforms have no single "BIOS file"
  concept this heuristic's model fits.

## NAND/large-firmware class

Files far larger than any real simple BIOS (full NAND dumps, SD-card images, `.PUP`
firmware packages) are a different category with no meaningful *upper* bound -- a user's
SD-card image can be arbitrarily large. This heuristic doesn't need a second ceiling
constant for them: anything larger than `MAX_BIOS_SIZE` (below) is already, by definition,
not a candidate for the simple-BIOS size check. Platforms with files in this class:
NintendoDsi (`NAND.bin`, `Sdcard.bin`), Wii (`nand.bin`, `WiiSD.raw`), WiiU (full NAND),
NintendoSwitch/NintendoSwitch2 (firmware/NAND), **Xbox360** (NAND flash dumps -- a real,
common file type in that scene, added here even though `Xbox360` is separately listed
under "Explicitly out of scope" above; that section is about the simple-BIOS-*blob*
heuristic only, not about excluding Xbox 360 from NAND-class recognition), PlayStation3
(`PUP`), PlayStationVita (`PUP`).

Unlike a BIOS file, a NAND dump can't be identified by hash against a shared catalog --
it bakes in per-console keys/CID, so no two consoles' NAND dumps are byte-identical.
Detection here can only classify *"this is a NAND/SD-image dump of platform X"*
structurally, never confirm identity the way `retrobios` hash-matching does above. Size
is also not a usable discriminator (a NAND image can be smaller *or* larger than that
platform's own ROMs), so this is a pure structural/magic-signature check, independent of
`MAX_BIOS_SIZE` and every per-platform constant below.

### Trigger: not part of the per-file ROM detection cycle

NAND-class files never enter `platforms::deep_try_platform`'s per-candidate cycle the way
an ordinary ROM/BIOS-file candidate does -- a DSi/3DS/Switch NAND or SD dump is not "a
candidate ROM for platform X" the cycle should even try. Instead this is a
**directory-context-gated pre-filter**, the size check inverted from the simple-BIOS
heuristic above:

1. Precondition: the file sits in a directory already resolved to one known NAND-capable
   platform (DSi/3DS/New3DS/Switch/Switch2/Xbox360) -- same precondition this heuristic
   already requires for the simple-BIOS case.
2. Location/extension check: a `.bin` (or other NAND-typical extension) file sitting
   directly in that platform's tree, **larger** than the platform's already-known
   small-system-file ceiling (the `MAX_BIOS_SIZE_*` constant below) and not matching a
   known firmware/ARM-bin size window; or any file sitting in that platform's `bios`/
   `system` subdirectory, size `> MAX_BIOS_SIZE` (the global 4MB ceiling).
3. **Mixed/scattered collections (multiple platforms and/or BIOS+ROMs not cleanly
   separated) are out of scope**, same policy as the simple-BIOS heuristic -- leave the
   file unclassified; it falls back to the general hash-first DAT-match path.
4. Only on a size+location match does the actual signature check below run; no match
   means no classification is attempted.

The pure signature-check functions (byte buffer in, match/no-match out; see
`src/rom/detect/nand_dispatch.rs`) have no dependency on a scanner and are implemented.
The directory-context trigger itself depends on the file-scanning/ownership-table pass
this project doesn't have yet (see `docs/plan/database.md`'s "Implementation status") --
documented above, not yet wired to real scan code.

### Signatures (researched, confidence stated per platform -- not uniform)

**3DS/New3DS -- high confidence.** Reuses the already-implemented NCSD parser
(`nintendo::read_3ds_ncsd_container_metadata`) rather than a new detector: 3DS NAND dumps
are NCSD-format containers, exactly like cartridge dumps, distinguished by the NCSD
header's media-type index byte at offset `0x18D` (confirmed via GBATEK: `0`=`InnerDevice`
/eMMC/NAND, `1`/`2`=Card1/Card2, `3`=`ExtendedDevice`) --
`nintendo::read_3ds_ncsd_media_type`/`is_3ds_nand_dump`.

**Nintendo Switch -- high confidence (rawnand/GPP only).** A full NAND backup (BOOT0 +
BOOT1 + rawnand, Hekate's own dump convention) has its GPT-partitioned rawnand/GPP portion
start with a protective MBR at LBA0 followed by the real GPT header at LBA1: ASCII
`"EFI PART"` at file offset `0x200`, the standard UEFI GPT signature (confirmed against
the UEFI spec directly) -- `nand_dispatch::switch_nand_gpt_signature`. **BOOT0/BOOT1 are
not covered** -- no verified signature found for those; not guessed.

**Nintendo DSi NAND -- medium confidence (footer-dependent).** ASCII marker
`"DSi eMMC CID/CPU"` -- the footer-ID string dumping tools (dumpTool, SafeNANDManager)
append, holding the eMMC CID + console ID needed for decryption. Offset is not fixed (it
varies by eMMC vendor -- Samsung vs. ST use different conventions/sizes), so this scans a
tail window rather than trusting one offset -- `nand_dispatch::dsi_nand_footer_signature`.
**Real limitation**: a dump made without this optional footer is not identifiable this
way.

**Xbox 360 NAND -- medium confidence (single-source, composite header).** Not a single
magic -- a structural check across several fields: byte `0x00` = `0xFF`, plus pointer
fields at `0x08` (CB offset)/`0x0C` (CF1 offset)/`0x6C` (keyvault offset)/`0x7C` (SMC
offset) parsing as plausible in-range values against the *real file size* (not the small
header window itself), plus SMC length (`0x78`) non-zero and in-range --
`nand_dispatch::xbox360_nand_signature`. Verified against one auto-summarized source (the
Free60 wiki); the primary reference (ConsoleMods "Standard NAND") 403'd during research
and was never cross-checked -- flagged as needing a second-source confirmation before
relying on this beyond a soft/contextual classification.

### Nintendo SD card family -- folder-marker identification

Wii, 3DS, and Switch each write a distinctive, documented root-level folder structure to
any SD card they've used -- a much stronger discriminator than a generic MBR/GPT check,
since it identifies *which* platform, not just "this is a disk image":

- **Wii**: `private/wii/title/<4-char-titleID>/` (WiiWare/VC/save data moved to SD) and/or
  `apps/<name>/boot.dol` (Homebrew Channel).
- **3DS/New3DS**: root `Nintendo 3DS/<ID0>/<ID1>/` folder.
- **Switch**: root `Nintendo/Contents/registered/` (official installed content) and/or
  `atmosphere/` (the near-ubiquitous CFW).
- **DSi**: no equivalent root-folder convention found -- flagged as *not found*, not
  asserted absent; DSi's SD usage leans on raw `Sdcard.bin`-style image dumps instead,
  hence the contextual heuristic below rather than a folder marker.

Two mechanisms, by how the SD content is presented -- `nand_dispatch::sd_family_marker_in_directory`
(an already-unpacked directory tree: trivial root-level presence check, no byte parsing)
and `nand_dispatch::sd_family_marker_in_fat_image` (a raw `.img`/`.bin` disk image: a
minimal FAT12/16/32 boot-sector parse locating just the root directory's entries, matched
by short/8.3 name rather than full VFAT long-filename parsing -- FAT always writes a short
name alongside any long name, so this is sufficient and much simpler).

**DSi SD card image (raw-image-only, contextual, not a true platform signature).** When a
raw image (1) *is* a partitioned/FAT disk image (MBR `0x55 0xAA` at byte 510, or GPT
`"EFI PART"` at `0x200`), (2) matches none of the three folder markers above, and (3)
isn't a known older system's HDD image (reusing `platforms::mycom`'s PC-98 MBR+IPL-string
exclusion check), it's classified as a likely DSi SD card image with medium/contextual
confidence, not a hard match -- `nand_dispatch::is_likely_dsi_sd_card_image`. Extension is
a cheap first-pass hint, not a hard filter, since the two conventions already differ in
this doc's own entries (DSi's `Sdcard.bin` vs. Wii's `WiiSD.raw`, `.bin` vs. `.raw`) but
renamed/relabeled files are common in this scene -- a `.bin` file failing the DSi check
(or a `.raw` file failing the Wii folder-marker scan) still falls through to the other
check rather than being rejected outright. Per the app's actual input domain (a ROM
manager, not a general disk-image tool): false positives against unrelated real-world SD
card content (phones/cameras/PC storage) are not a concern here.

### System-card files stay in the ordinary BIOS/system-file bucket

TurboGrafxCd's `SystemCard.pce`/`GamesExpressCDCard.pce` (above) and SegaCd's
`Bios.md`/`Bios.bin` (the Mega-CD's own boot ROM, playing the same required-add-on-boot-
file role) are ordinary BIOS/system files -- size-prefilter + hash-confirm via retrobios,
same bucket as everything else in this doc -- not NAND-class, and not touched by anything
in this section.

---

## Constants

Moved to `src/rom/detect/systemfile.rs` as real `pub const` items (`MAX_BIOS_SIZE` +
`MAX_BIOS_SIZE_<PLATFORM>`, one per platform, values unchanged from this doc's own
research above) -- not re-derived from `biosdb.bin` at load time, so they stay a single,
directly-readable source of truth next to the detection code that uses them.

## Implementation

`src/rom/detect/systemfile.rs` implements this heuristic: `identify_system_file(path,
dir_type, platform)` -- size-only gating (no extension check; a real BIOS dump's
extension varies too much in the wild to gate on reliably, confirmed against retrobios's
own PlayStation 2 entries, which alone span `.BIN`/`.EROM`/`.ROM1`/`.ROM2`/`.NVM`/`.MEC`),
25% margin, hash lookup against an embedded `biosdb.bin` (generated by `cargo run --bin
biosdb_gen` from the already-integrated retrobios JSON, mirroring `convert::nes`'s
`fcdb.bin` precedent), falling back to `nand_dispatch.rs`'s structural signature checks
for NAND/SD-capable platforms above the hash-lookup ceiling. `SystemFileType::Firmware`
is a type tag independent of size (NDS/DSi's small `Firmware.bin` and PS3/Vita's huge
`.PUP` are both `Firmware`, distinguished by filename pattern in the generator, not by
which size bucket they fall into). `Nand`/`Sdcard` are never `biosdb.bin` entries --
NAND/SD dumps aren't hashable against a shared catalog (per-console keys/CID baked in);
those two variants only ever come from a structural-signature match.

Not yet wired to real directory-scanning: `dir_type`/`platform` are supplied by the
caller today, same as `nand_dispatch.rs` -- both depend on the file-scanning/ownership-
table pass this project doesn't have yet (`docs/plan/database.md`'s "Implementation
status"). Also not implemented this pass: zip/7z archive-member-level hashing (this
heuristic currently treats `path` as a plain on-disk file; a member's extracted size vs.
the archive's on-disk compressed size is a known, documented gap in
`systemfile.rs::file_size`'s doc comment).
