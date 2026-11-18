# chdlite

Ziehen Sie ein Disc-Image (CUE + BIN, GDI + BIN, ISO) auf chdlite, und es wird zu CHD oder RVZ;
ziehen Sie eine `.chd` oder `.rvz` darauf, und Sie erhalten CUE + BIN, GDI + BIN oder ISO zurück. Es
liest außerdem Metadaten und erkennt die Plattform, hasht den Inhalt von CHD, RVZ, CSO und PBP, bringt
NES-, N64- und SNES-Dumps ins gewählte Format und entschlüsselt 3DS-Titel. Keine Datenbank und keine
Einrichtung nötig. Es nutzt den Kern von romexplorer und wird separat veröffentlicht (Version 0.3.4).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · **Deutsch** · [한국어](README.ko.md) · [Polski](README.pl.md)

**Download:** [CHD-Lite-Releases](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Änderungen:** [Versionshinweise](../docs/chdlite_release_notes.md) · Teil von [romexplorer](../README.de.md)

> **chdlite verbreitet und lädt keine ROMs, BIOS-Dateien oder Schlüssel herunter und unterstützt
> keine Piraterie.** Nutzen Sie es mit Spielen, die Sie besitzen und selbst ausgelesen haben.

## Fenster

Öffnen Sie `chdlite-gui` (unter macOS `chdlite.app`, unter Linux das chdlite-AppImage). Es hat vier Modi:

| Modus | Beschreibung |
|---|---|
| **Lite** | Komprimiert schnell. Eine `.chd` oder `.rvz` wird zu CUE + BIN, GDI + BIN oder ISO entpackt. |
| **Comp** | Komprimiert langsam auf die kleinste Datei. Eine `.chd` oder `.rvz` wird ebenso entpackt. |
| **Read** | Liest die Metadaten und erkennt die Plattform. |
| **Hash** | Hasht den Inhalt einer Datei. |

Wählen Sie einen Modus und ziehen Sie Dateien oder Ordner auf das Fenster (oder wählen Sie sie).
**Settings** enthält den Rest: Threads und gleichzeitige Dateien, Ausgabeordner und Ersetzen
vorhandener Ausgaben, Lite-Voreinstellung und Comp-Codecs, RVZ-Codec, -Stufe und -Blockgröße,
Hashes, Cue-Stil und GD-ROM-Sheet, das Format für Modul-Dumps und 3DS-Titel sowie die Rückfrage vor
einer verlustbehafteten Ausgabe (WUA, ZAR, XISO). Das Fenster merkt sie sich, und die Kommandozeile
nutzt dieselben Einstellungen.

**Android.** Die chdlite-App ist dasselbe Fenster. Dateien und Ordner wählen Sie in ihrem eigenen
Browser.

**macOS und Linux.** Die Mac-App ist nicht notarisiert: Beim ersten Start Rechtsklick > Öffnen.

## Konvertierung

Das Ausgabeformat wird anhand des Images gewählt.

**Komprimieren**

| Eingabe | Ausgabe |
|---|---|
| CD: CUE + BIN, TOC + BIN oder ein einzelner BIN/IMG-Track | CD-CHD |
| GD-ROM (Dreamcast): GDI + BIN/RAW | GD-ROM-CHD |
| DVD (PS2, PSP und andere): ISO | DVD-CHD |
| PSP: CSO | DVD-CHD |
| PS1: PBP | CD-CHD |
| GameCube, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**Entpacken**

| Eingabe | Ausgabe |
|---|---|
| CD-CHD | CUE + BIN (eine BIN pro Track) |
| GD-ROM-CHD | GDI + BIN, oder CUE + BIN mit `--gdrom-sheet cue` |
| DVD-CHD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR und XISO behalten die Dateien des Spiels, aber nicht die Disc selbst, und lassen sich daher
nicht ins Original-Image zurückverwandeln. Nur verlustfreie Eingaben werden angenommen: nicht WUA,
ZAR oder XISO. Eine Wii-U-Disc braucht ihren Schlüssel daneben.

**Modul-Dumps** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`) werden nicht
komprimiert. Sie werden im für ihre Plattform eingestellten Format geschrieben: NES mit oder ohne
Header (oder als `.unh`), N64 in einer seiner drei Byte-Reihenfolgen, SNES mit oder ohne
Copier-Header. Ob ein NES-Dump einen Header braucht, wird per Hash nachgeschlagen, daher werden nur
bekannte Spiele konvertiert.

**3DS-Titel** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` und die komprimierten `.zcci`, `.zcia`, `.zcxi`,
`.z3dsx`) werden entschlüsselt: Ein Spiel wird zu `.cci`/`.3ds`, `.cxi` oder komprimiert `.zcci`/`.zcxi`;
Updates, DLC und Demos bleiben `.cia` (komprimiert `.zcia`). chdlite enthält keine Schlüssel.

**Hashes.** SHA-1, MD5, CRC32, SHA-256, XXH3 und der RetroAchievements-Hash, der auch anzeigt, ob
RetroAchievements Erfolge für das Spiel hat (laut mitgelieferter Liste).

## Read

Read erkennt die Plattform anhand der Daten der Disc und liest daraus Seriennummer und Titel des
Spiels. Außerdem zeigt es:

| Datei | Angezeigt |
|---|---|
| CHD | CHD-Version und -Typ (CD, GD-ROM, DVD, Festplatte); Plattform, Seriennummer und Titel; volle Größe, Dateigröße und Kompressionsrate; Hunk-Größe und -Anzahl; Unit-Größe; Codecs; SHA-1 des Ganzen und der Daten; SHA-1 des Parents (falls vorhanden); Typ und Länge jedes Tracks |
| RVZ | GameCube oder Wii; Spiel-ID und Titel; Disc-Größe, Dateigröße und Rate; Blockgröße |
| CSO | CSO-Version und Blöcke; Plattform, Seriennummer und Titel; ISO-Größe, Dateigröße und Rate |
| PBP | Anzahl der Discs; Plattform, Seriennummer und Titel; Tracks und Größe jeder Disc |
| CUE + BIN, GDI + BIN, ISO | Plattform, Seriennummer und Titel; Ausgabeformat |
| Wii U WUD, WUX | Disc-Größe; Titel-ID und Version |
| 3DS-Titel | Container (CIA, Modul-Image, NCCH, 3DSX); komprimiert oder nicht; Titeltyp; verschlüsselt oder nicht; Ausgabeformat |
| Modul-Dumps | Plattform und Größe; aktuelles Format (Header, Byte-Reihenfolge); Ausgabeformat |

## Drag & Drop

Jedes Werkzeug hat eine Aufgabe: Wer Dateien auf seine ausführbare Datei zieht, löst sie aus, mit den
gespeicherten Einstellungen.

| Werkzeug | Aktion beim Ablegen |
|---|---|
| `chdlite` | Komprimiert schnell. Eine `.chd` oder `.rvz` wird zu CUE + BIN, GDI + BIN oder ISO entpackt. |
| `chdcomp` | Komprimiert auf die kleinste Datei. Eine `.chd` oder `.rvz` wird zu CUE + BIN, GDI + BIN oder ISO entpackt. |
| `chdread` | Liest die Metadaten und erkennt die Plattform. |
| `chdhash` | Hasht den Inhalt einer `.chd`, `.rvz`, `.cso` oder `.pbp`, oder die Datei selbst. |

## Kommandozeile

Jedes Werkzeug kennt alle Befehle und Optionen. In den Apps liegen die Werkzeuge in
`chdlite.app/Contents/MacOS/` oder laufen mit `chdlite.AppImage chdlite ...`.

```
chdlite [Befehl] <Datei|Ordner>... [Optionen]
```

| Befehl | Beschreibung |
|---|---|
| `auto` | `.chd`/`.rvz` werden entpackt, alles andere mit der schnellen Voreinstellung erstellt. |
| `create` | Erstellt mit der schnellen Voreinstellung. |
| `comp` | Erstellt die kleinste Datei. |
| `extract` | Entpackt `.chd`, `.rvz`, `.cso` und `.pbp` zu CUE + BIN, GDI + BIN oder ISO sowie komprimierte 3DS-Titel. |
| `read` | Liest die Metadaten und erkennt die Plattform. |
| `hash` | Hasht den Inhalt. |

| Option | Beschreibung |
|---|---|
| `-o`, `--output <dir>` | Schreibt hierhin statt neben jede Eingabe. |
| `-f`, `--force` | Ersetzt bereits vorhandene Ausgaben. |
| `-np`, `--threads <n>` | Threads insgesamt (Standard: physische Kerne minus 4). |
| `-jf`, `--concurrent-files <n>` | Gleichzeitig konvertierte Dateien. |
| `--preset pc\|mobile` | Schnelle Voreinstellung für `create`. |
| `--comp best\|chdman\|custom` | Modus von `comp`. |
| `-c`, `--compression <list>` | CHD-Codecs, bis zu 4. |
| `-hs`, `--hunksize <bytes>` | CHD-Hunk-Größe. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | RVZ-Einstellungen. `--rvz-scrub` entfernt Fülldaten und lässt sich nicht rückgängig machen. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (Standard `sha1,rahash`). |
| `--cue-style <style>` | Stil des entpackten Cue-Sheets: `redump`, `redump_catalog` oder `chdman`. |
| `--gdrom-sheet gdi\|cue` | Sheet eines entpackten GD-ROM. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | Format, in dem Modul-Dumps und 3DS-Titel geschrieben werden. |
| `-h`, `--help` / `-V`, `--version` | Hilfe und Version. |

Standardwerte kommen aus `chdlite.toml`, neben dem Programm oder in Ihrem Einstellungsordner
(`--help` zeigt, wo).

## Mitgelieferte Bibliotheken

| Bibliothek | Lizenz |
|---|---|
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/chdman-simd.LICENSE) |
| [MAME](https://github.com/mamedev/mame) | [GPL-2.0+](../app_root_ex/lib/LICENSES/mame.LICENSE) |
| [libchdr](https://github.com/rtissera/libchdr) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/libchdr.LICENSE) |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | [GPL-2.0+](../app_root_ex/lib/LICENSES/dolphin.LICENSE) |
| [Cemu](https://github.com/cemu-project/Cemu) | [MPL-2.0](../app_root_ex/lib/LICENSES/cemu.LICENSE) |
| [ZArchive](https://github.com/Exzap/ZArchive) | [MIT-0](../app_root_ex/lib/LICENSES/ZArchive.LICENSE) |
| [zstd](https://github.com/facebook/zstd) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/zstd.LICENSE) |
| [fmt](https://github.com/fmtlib/fmt) | [MIT](../app_root_ex/lib/LICENSES/fmt.LICENSE) |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | [BSD](../app_root_ex/lib/LICENSES/extract-xiso.LICENSE) |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | [GPL-2.0](../app_root_ex/lib/LICENSES/beetle-psx-libretro.LICENSE) |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | [GPL-3.0](../app_root_ex/lib/LICENSES/RALibretro.LICENSE) |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | [zlib](../app_root_ex/lib/LICENSES/zlib-ng.LICENSE) |
| [OpenSSL](https://github.com/openssl/openssl) | [Apache-2.0](../app_root_ex/lib/LICENSES/openssl.LICENSE) |

## Lizenz

[AGPL-3.0](../LICENSE), wie romexplorer.
