# rommusic

Ziehen Sie ein ROM oder ein Disc-Image auf `rommusic`, und es schreibt die Musik des Spiels als
FLAC. Es nutzt dieselben Leser wie die romexplorer-App für ROM-Musik und wird separat
veröffentlicht (Version 0.2.1).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · **Deutsch** · [한국어](README.ko.md) · [Polski](README.pl.md)

**Download:** [Rom-Music-Releases](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Änderungen:** [Versionshinweise](../docs/rommusic_release_notes.md) · Teil von [romexplorer](../README.de.md)

> **rommusic verbreitet und lädt keine ROMs, BIOS-Dateien oder Schlüssel herunter und unterstützt
> keine Piraterie.** Nutzen Sie es mit Spielen, die Sie besitzen und selbst ausgelesen haben.

## Fenster

Öffnen Sie `rommusic-gui` (unter macOS `rommusic.app`, unter Linux das rommusic-AppImage):

1. Wählen Sie Dateien oder einen Ordner, oder ziehen Sie sie auf das Fenster. ROMs, Disc-Images
   und ausgelesene Spielordner funktionieren.
2. Wählen Sie **FLAC**, **Original audio** oder **Both**.
3. Wählen Sie einen Ausgabeordner, oder lassen Sie ihn leer, um neben jede Datei zu schreiben.
4. Drücken Sie **Start**.

**Options** enthält den Rest: Jingles, Soundeffekte und Videoton, eine Titelgrenze, Abtastrate
und Bits sowie den SNES-Lauf (wie lange das Spiel läuft, und ob dabei Start gedrückt wird). Das
Fenster merkt sie sich.

**Android.** Die rommusic-App (Android 9 oder neuer) ist dasselbe Fenster. Beim ersten Start fragt
sie nach Zugriff auf alle Dateien, um ROMs zu lesen und die Musik zu schreiben; Dateien und Ordner
wählen Sie in ihrem eigenen Browser, und das Schreiben läuft bei ausgeschaltetem Bildschirm weiter.

**macOS und Linux.** `rommusic.AppImage --install` fügt das Fenster dem Desktop-Menü hinzu. Die
Mac-App ist nicht notarisiert: Beim ersten Start Rechtsklick > Öffnen.

## Unterstützte Spiele

| Plattform | Dateien | Musik |
|---|---|---|
| CD-Spiele: PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast und andere | CHD, CUE + BIN, GDI + BIN, ISO | CD-Audiotracks; Musikdateien auf der Disc (ADX, XA, AT3 und andere, auch in CPK- und AFS-Archiven) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360 und andere | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, ausgelesene Spielordner | Musikdateien auf der Disc (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA und andere, auch in CPK-, AFS-, ARC- und SZS-Archiven) |
| Wii U | WUA; WUD, WUX oder ein Titelordner mit seinem Schlüssel daneben | Musikdateien auf der Disc (BFSTM und andere) |
| Nintendo DS | NDS | SDAT-Soundarchiv (STRM-Streams, SSEQ-Sequenzen) |
| Nintendo 3DS | 3DS, CCI (nur entschlüsselt) | Musikdateien im Soundarchiv (BCSTM und andere) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, auch in zip oder 7z | BGM-Binärcode |
| SNES / Super Famicom | ROM, auch in zip oder 7z | BGM-Binärcode |
| Nintendo 64 | ROM, auch in zip oder 7z | BGM-Binärcode |
| PC-98 | Disketten-Images (FDI, HDM, FDD und andere), auch in zip oder 7z | PMD- und FMP-Musikdateien auf der Diskette |

Auch auf einer unterstützten Plattform wird aus technischen Gründen nicht jedes Spiel unterstützt.
Disc-Images in zip oder 7z, verschlüsselte Images, PS3, Vita und heruntergeladene Pakete werden
nicht gelesen.

## Ausgabe

Für jede Eingabe legt rommusic daneben (oder im Ausgabeordner) einen Ordner `<Name> (music)` an.
Dateien eines früheren Laufs darin werden ersetzt.

| Ausgabe | Inhalt |
|---|---|
| **FLAC** (Standard) | `01 - <Titel>.flac`, `02 - ...`: die Hintergrundmusik. Ein Titel mit Schleife spielt die Schleife zweimal und blendet aus. `index.txt` listet die Titel. |
| **Original audio** | Die Musikdateien so, wie das Spiel sie speichert, in einem Ordner `original`. CD-Audiotracks und Modulmusik haben keine; dafür FLAC verwenden. |
| **Both** | FLAC und die Originaldateien. |

Originaldateien enthalten Code und Daten des Spiels. Bewahren Sie sie bei Ihren ROMs auf und geben
Sie sie nicht weiter.

## Drag & Drop

Ziehen Sie eine oder mehrere Dateien oder Ordner auf `rommusic.exe` (unter Linux und macOS
`rommusic`). Es schreibt FLAC mit den Standardeinstellungen, wie **Start** im Fenster ohne Änderungen.

## Kommandozeile

Das Kommandozeilenwerkzeug ist `rommusic` (in den Apps: `rommusic.app/Contents/MacOS/rommusic`
oder `rommusic.AppImage rommusic ...`). Jede Fenstereinstellung gibt es als Option.

```
rommusic [Optionen] <ROM, Disc-Image oder Ordner>...
```

Beispiele:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Option | Beschreibung |
|---|---|
| `--all` | Auch Jingles, Soundeffekte und Videoton. Standardmäßig wird nur Hintergrundmusik geschrieben. |
| `--max N` | Höchstens N Titel pro Eingabe. |
| `--out DIR` | Legt die Ordner `<Name> (music)` in `DIR` statt neben jeder Eingabe an. |
| `--rate native` | Standard: jeder Titel mit der Rate, mit der er dekodiert wird. |
| `--cd` / `--rate cd` | 44.1 kHz, 16 Bit. |
| `--dvd` / `--rate dvd` | 48 kHz, 24 Bit. |
| `--hires` / `--rate hires` | 96 kHz, 24 Bit. |
| `--rate <Hz>` | Jede Rate von 8000 bis 192000. |
| `--bits 16\|24` | Bits pro Sample (Standard 16, mit `--dvd` und `--hires` 24). |
| `--format flac` | Standard: nur FLAC. |
| `--format original` | Die Musik so, wie das Spiel sie speichert, in einem Ordner `original`, ohne FLAC. |
| `--format both` | FLAC und die Originaldateien. |
| `--seconds N` | SNES: wie lange das Spiel läuft, bevor seine Musik übernommen wird (Standard 20). |
| `--start` | SNES: drückt während des Laufs ab und zu Start, für Spiele, deren Musik nach dem Titelbild beginnt. |
| `--list` | Listet nur die Titel auf und schreibt nichts. Ein Titel mit Schleife zeigt einen Durchlauf und „loop“; seine Datei spielt die Schleife zweimal und blendet aus. |
| `--verbose` | Sagt, warum eine Datei oder ein Titel übersprungen wurde. |
| `--no-pause` | Wartet am Ende nicht auf Enter. Gewartet wird nur im Fenster, das sich für eine abgelegte Datei öffnet. |
| `-h`, `--help` / `-V`, `--version` | Hilfe und Version. |

Eine andere Rate als `native` wird umgerechnet. Das fügt keine Details hinzu; es ist für Player,
die alle Dateien mit derselben Rate wollen oder ungewöhnliche Raten wie 37,8 kHz schlecht
verarbeiten. Eine umgerechnete Datei vermerkt das in ihrem Kommentar-Tag.

## Mitgelieferte Bibliotheken

| Bibliothek | Lizenz |
|---|---|
| [game-music-emu](https://github.com/libgme/game-music-emu) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/gme.LICENSE) |
| [Game_Music_Emu (kode54)](https://github.com/kode54/Game_Music_Emu) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/gme-kode54.LICENSE) |
| [agbplay](https://github.com/ipatix/agbplay) | [LGPL-3.0](../app_root_ex/lib/LICENSES/agbplay.LICENSE) |
| [SSEQPlayer](https://github.com/kode54/SSEQPlayer) | [WTFPL](../app_root_ex/lib/LICENSES/SSEQPlayer.LICENSE) |
| [lazyusf2](https://gitlab.com/kode54/lazyusf2) | [GPL-2.0+](../app_root_ex/lib/LICENSES/lazyusf2.LICENSE) |
| [Highly Experimental](https://gitlab.com/kode54/highly_experimental) | [GPL-3.0](../app_root_ex/lib/LICENSES/highly_experimental.LICENSE) |
| [Highly Theoretical](https://gitlab.com/kode54/highly_theoretical) | [GPL-3.0](../app_root_ex/lib/LICENSES/highly_theoretical.LICENSE) |
| [mGBA](https://github.com/mgba-emu/mgba) | [MPL-2.0](../app_root_ex/lib/LICENSES/mgba.LICENSE) |
| [Beetle WonderSwan](https://github.com/libretro/beetle-wswan-libretro) | [GPL-2.0+](../app_root_ex/lib/LICENSES/beetle-wswan.LICENSE) |
| [libvgm](https://github.com/ValleyBell/libvgm) | [BSD-3-Clause, GPL-2.0+](../app_root_ex/lib/LICENSES/libvgm.LICENSE) |
| [VGMTrans](https://github.com/vgmtrans/vgmtrans) | [zlib](../app_root_ex/lib/LICENSES/vgmtrans.LICENSE) |
| [TinySoundFont](https://github.com/schellingb/TinySoundFont) | [MIT](../app_root_ex/lib/LICENSES/TinySoundFont.LICENSE) |
| [98fmplayer](https://github.com/myon98/98fmplayer) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/98fmplayer.LICENSE) |
| [vgmstream](https://github.com/vgmstream/vgmstream) | [ISC](../app_root_ex/lib/LICENSES/vgmstream.LICENSE) |
| [FFmpeg](https://github.com/FFmpeg/FFmpeg) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/ffmpeg.LICENSE) |
| [fmt](https://github.com/fmtlib/fmt) | [MIT](../app_root_ex/lib/LICENSES/fmt.LICENSE) |
| [spdlog](https://github.com/gabime/spdlog) | [MIT](../app_root_ex/lib/LICENSES/spdlog.LICENSE) |
| [unarr](https://github.com/selmf/unarr) | [LGPL-3.0](../app_root_ex/lib/LICENSES/unarr.LICENSE) |
| [zlib](https://github.com/madler/zlib) | [zlib](../app_root_ex/lib/LICENSES/zlib.LICENSE) |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/chdman-simd.LICENSE) |
| [MAME](https://github.com/mamedev/mame) | [GPL-2.0+](../app_root_ex/lib/LICENSES/mame.LICENSE) |
| [libchdr](https://github.com/rtissera/libchdr) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/libchdr.LICENSE) |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | [GPL-2.0+](../app_root_ex/lib/LICENSES/dolphin.LICENSE) |
| [Cemu](https://github.com/cemu-project/Cemu) | [MPL-2.0](../app_root_ex/lib/LICENSES/cemu.LICENSE) |
| [ZArchive](https://github.com/Exzap/ZArchive) | [MIT-0](../app_root_ex/lib/LICENSES/ZArchive.LICENSE) |
| [zstd](https://github.com/facebook/zstd) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/zstd.LICENSE) |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | [BSD](../app_root_ex/lib/LICENSES/extract-xiso.LICENSE) |
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/7-Zip-zstd.LICENSE) |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | [zlib](../app_root_ex/lib/LICENSES/zlib-ng.LICENSE) |

## Lizenz

[AGPL-3.0](../LICENSE), wie romexplorer.
