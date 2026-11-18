# chdlite

Upuść obraz płyty (CUE + BIN, GDI + BIN, ISO) na chdlite, a stanie się CHD lub RVZ; upuść `.chd` lub
`.rvz`, a odzyskasz CUE + BIN, GDI + BIN lub ISO. Czyta też metadane i wykrywa platformę, liczy hash
zawartości CHD, RVZ, CSO i PBP, zapisuje zrzuty NES, N64 i SNES w wybranym formacie i odszyfrowuje
tytuły 3DS. Nie wymaga bazy danych ani konfiguracji. Używa rdzenia romexplorer i jest wydawany
osobno (wersja 0.3.4).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · **Polski**

**Pobieranie:** [wydania CHD Lite](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Zmiany:** [informacje o wydaniu](../docs/chdlite_release_notes.md) · Część [romexplorer](../README.pl.md)

> **chdlite nie rozpowszechnia ani nie pobiera ROM-ów, BIOS-ów ani kluczy i nie wspiera piractwa.**
> Używaj go z grami, które posiadasz i samodzielnie zrzuciłeś.

## Okno

Otwórz `chdlite-gui` (na macOS `chdlite.app`, na Linuksie AppImage chdlite). Ma cztery tryby:

| Tryb | Opis |
|---|---|
| **Lite** | Kompresuje szybko. `.chd` lub `.rvz` jest rozpakowywany do CUE + BIN, GDI + BIN lub ISO. |
| **Comp** | Kompresuje wolno, do najmniejszego pliku. `.chd` lub `.rvz` jest rozpakowywany tak samo. |
| **Read** | Czyta metadane i wykrywa platformę. |
| **Hash** | Liczy hash zawartości pliku. |

Wybierz tryb, a potem upuść pliki lub foldery na okno (albo je wybierz). **Settings** zawiera resztę:
wątki i pliki naraz, folder wyjściowy i zastępowanie istniejących plików, preset Lite i kodeki Comp,
kodek, poziom i rozmiar bloku RVZ, hashe, styl cue i arkusz GD-ROM, format zrzutów kartridży i
tytułów 3DS oraz pytanie przed stratnym wynikiem (WUA, ZAR, XISO). Okno je zapamiętuje, a wiersz
poleceń używa tych samych ustawień.

**Android.** Aplikacja chdlite to to samo okno. Pliki i foldery wybiera się w jej własnej
przeglądarce.

**macOS i Linux.** Aplikacja na Maca nie jest notaryzowana: przy pierwszym uruchomieniu kliknij prawym przyciskiem > Otwórz.

## Konwersja

Format wyjściowy jest wybierany na podstawie obrazu.

**Kompresja**

| Wejście | Wyjście |
|---|---|
| CD: CUE + BIN, TOC + BIN lub pojedyncza ścieżka BIN/IMG | CHD CD |
| GD-ROM (Dreamcast): GDI + BIN/RAW | CHD GD-ROM |
| DVD (PS2, PSP i inne): ISO | CHD DVD |
| PSP: CSO | CHD DVD |
| PS1: PBP | CHD CD |
| GameCube, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**Rozpakowanie**

| Wejście | Wyjście |
|---|---|
| CHD CD | CUE + BIN (jeden BIN na ścieżkę) |
| CHD GD-ROM | GDI + BIN lub CUE + BIN z `--gdrom-sheet cue` |
| CHD DVD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR i XISO zachowują pliki gry, ale nie samą płytę, więc nie da się z nich odtworzyć
oryginalnego obrazu. Przyjmowane są tylko wejścia bezstratne: nie WUA, ZAR ani XISO. Płyta Wii U
wymaga klucza obok.

**Zrzuty kartridży** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`) nie są
kompresowane. Są zapisywane w formacie ustawionym dla platformy: NES z nagłówkiem lub bez (albo jako
`.unh`), N64 w jednej z trzech kolejności bajtów, SNES z nagłówkiem kopiarki lub bez. To, czy zrzut
NES potrzebuje nagłówka, sprawdza się po hashu, więc konwertowane są tylko znane gry.

**Tytuły 3DS** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` i skompresowane `.zcci`, `.zcia`, `.zcxi`,
`.z3dsx`) są odszyfrowywane: gra staje się `.cci`/`.3ds`, `.cxi` lub skompresowanym `.zcci`/`.zcxi`;
aktualizacje, DLC i dema zostają `.cia` (skompresowane `.zcia`). chdlite nie zawiera kluczy.

**Hashe.** SHA-1, MD5, CRC32, SHA-256, XXH3 i hash RetroAchievements, który pokazuje też, czy
RetroAchievements ma osiągnięcia dla gry (według dołączonej listy).

## Read

Read wykrywa platformę na podstawie danych płyty i odczytuje z nich numer seryjny i tytuł gry.
Pokazuje też:

| Plik | Pokazywane |
|---|---|
| CHD | Wersja i typ CHD (CD, GD-ROM, DVD, dysk twardy); platforma, numer seryjny i tytuł; pełny rozmiar, rozmiar pliku i stopień kompresji; rozmiar i liczba hunków; rozmiar jednostki; kodeki; SHA-1 całości i danych; SHA-1 rodzica (jeśli jest); typ i długość każdej ścieżki |
| RVZ | GameCube czy Wii; ID i tytuł gry; rozmiar płyty, rozmiar pliku i stopień; rozmiar bloku |
| CSO | Wersja i bloki CSO; platforma, numer seryjny i tytuł; rozmiar ISO, rozmiar pliku i stopień |
| PBP | Liczba płyt; platforma, numer seryjny i tytuł; ścieżki i rozmiar każdej płyty |
| CUE + BIN, GDI + BIN, ISO | Platforma, numer seryjny i tytuł; format wyjściowy |
| Wii U WUD, WUX | Rozmiar płyty; ID i wersja tytułu |
| Tytuły 3DS | Kontener (CIA, obraz kartridża, NCCH, 3DSX); skompresowany czy nie; typ tytułu; zaszyfrowany czy nie; format wyjściowy |
| Zrzuty kartridży | Platforma i rozmiar; obecny format (nagłówek, kolejność bajtów); format wyjściowy |

## Przeciągnij i upuść

Każde narzędzie ma jedno zadanie: upuszczenie plików na jego plik wykonywalny je wykonuje, z
zapisanymi ustawieniami.

| Narzędzie | Działanie po upuszczeniu |
|---|---|
| `chdlite` | Kompresuje szybko. `.chd` lub `.rvz` jest rozpakowywany do CUE + BIN, GDI + BIN lub ISO. |
| `chdcomp` | Kompresuje do najmniejszego pliku. `.chd` lub `.rvz` jest rozpakowywany do CUE + BIN, GDI + BIN lub ISO. |
| `chdread` | Czyta metadane i wykrywa platformę. |
| `chdhash` | Liczy hash zawartości `.chd`, `.rvz`, `.cso` lub `.pbp` albo samego pliku. |

## Wiersz poleceń

Każde narzędzie przyjmuje wszystkie polecenia i opcje. W aplikacjach narzędzia są w
`chdlite.app/Contents/MacOS/` lub uruchamia się je przez `chdlite.AppImage chdlite ...`.

```
chdlite [polecenie] <plik|folder>... [opcje]
```

| Polecenie | Opis |
|---|---|
| `auto` | `.chd`/`.rvz` są rozpakowywane, reszta tworzona z szybkim presetem. |
| `create` | Tworzy z szybkim presetem. |
| `comp` | Tworzy najmniejszy plik. |
| `extract` | Rozpakowuje `.chd`, `.rvz`, `.cso` i `.pbp` do CUE + BIN, GDI + BIN lub ISO oraz skompresowane tytuły 3DS. |
| `read` | Czyta metadane i wykrywa platformę. |
| `hash` | Liczy hash zawartości. |

| Opcja | Opis |
|---|---|
| `-o`, `--output <dir>` | Zapisuje tutaj zamiast obok każdego pliku. |
| `-f`, `--force` | Zastępuje istniejące pliki wyjściowe. |
| `-np`, `--threads <n>` | Wątki łącznie (domyślnie: rdzenie fizyczne minus 4). |
| `-jf`, `--concurrent-files <n>` | Pliki konwertowane naraz. |
| `--preset pc\|mobile` | Szybki preset używany przez `create`. |
| `--comp best\|chdman\|custom` | Tryb `comp`. |
| `-c`, `--compression <list>` | Kodeki CHD, do 4. |
| `-hs`, `--hunksize <bytes>` | Rozmiar hunka CHD. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | Ustawienia RVZ. `--rvz-scrub` usuwa zbędne dane i nie da się tego cofnąć. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (domyślnie `sha1,rahash`). |
| `--cue-style <style>` | Styl rozpakowanego arkusza cue: `redump`, `redump_catalog` lub `chdman`. |
| `--gdrom-sheet gdi\|cue` | Arkusz rozpakowanego GD-ROM. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | Format, w jakim zapisywane są zrzuty kartridży i tytuły 3DS. |
| `-h`, `--help` / `-V`, `--version` | Pomoc i wersja. |

Wartości domyślne pochodzą z `chdlite.toml`, obok programu lub w folderze ustawień (`--help`
pokazuje gdzie).

## Dołączone biblioteki

| Biblioteka | Licencja |
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

## Licencja

[AGPL-3.0](../LICENSE), jak romexplorer.
