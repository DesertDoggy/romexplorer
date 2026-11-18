# rommusic

Upuść ROM lub obraz płyty na `rommusic`, a zapisze muzykę z gry jako FLAC. Używa tych samych
czytników co aplikacja romexplorer do muzyki z ROM-ów i jest wydawany osobno (wersja 0.2.1).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · **Polski**

**Pobieranie:** [wydania Rom Music](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Zmiany:** [informacje o wydaniu](../docs/rommusic_release_notes.md) · Część [romexplorer](../README.pl.md)

> **rommusic nie rozpowszechnia ani nie pobiera ROM-ów, BIOS-ów ani kluczy i nie wspiera piractwa.**
> Używaj go z grami, które posiadasz i samodzielnie zrzuciłeś.

## Okno

Otwórz `rommusic-gui` (na macOS `rommusic.app`, na Linuksie AppImage rommusic):

1. Wybierz pliki lub folder albo upuść je na okno. Działają ROM-y, obrazy płyt i zrzucone foldery
   gier.
2. Wybierz **FLAC**, **Original audio** lub **Both**.
3. Wybierz folder wyjściowy albo zostaw go pusty, by zapisać obok każdego pliku.
4. Naciśnij **Start**.

**Options** zawiera resztę: dżingle, efekty dźwiękowe i dźwięk filmów, limit utworów, częstotliwość
i bity oraz uruchomienie SNES (jak długo działa gra i czy w tym czasie naciskać Start). Okno je
zapamiętuje.

**Android.** Aplikacja rommusic (Android 9 lub nowszy) to to samo okno. Przy pierwszym uruchomieniu
prosi o dostęp do wszystkich plików, by czytać ROM-y i zapisywać muzykę; pliki i foldery wybiera się
w jej własnej przeglądarce, a zapis trwa przy wyłączonym ekranie.

**macOS i Linux.** `rommusic.AppImage --install` dodaje okno do menu pulpitu. Aplikacja na Maca nie
jest notaryzowana: przy pierwszym uruchomieniu kliknij prawym przyciskiem > Otwórz.

## Obsługiwane gry

| Platforma | Pliki | Muzyka |
|---|---|---|
| Gry na CD: PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast i inne | CHD, CUE + BIN, GDI + BIN, ISO | Ścieżki audio CD; pliki muzyczne na płycie (ADX, XA, AT3 i inne, także w archiwach CPK i AFS) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360 i inne | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, zrzucone foldery gier | Pliki muzyczne na płycie (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA i inne, także w archiwach CPK, AFS, ARC i SZS) |
| Wii U | WUA; WUD, WUX lub folder tytułu z kluczem obok | Pliki muzyczne na płycie (BFSTM i inne) |
| Nintendo DS | NDS | Archiwum dźwięku SDAT (strumienie STRM, sekwencje SSEQ) |
| Nintendo 3DS | 3DS, CCI (tylko odszyfrowane) | Pliki muzyczne w archiwum dźwięku (BCSTM i inne) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive / Genesis, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, także w zip lub 7z | Kod binarny BGM |
| SNES / Super Famicom | ROM, także w zip lub 7z | Kod binarny BGM |
| Nintendo 64 | ROM, także w zip lub 7z | Kod binarny BGM |
| PC-98 | Obrazy dyskietek (FDI, HDM, FDD i inne), także w zip lub 7z | Pliki muzyczne PMD i FMP na dyskietce |

Nawet na obsługiwanej platformie nie każda gra jest obsługiwana, z przyczyn technicznych. Obrazy płyt
w zip lub 7z, zaszyfrowane obrazy, PS3, Vita i pobrane pakiety nie są czytane.

## Wynik

Dla każdego pliku rommusic tworzy obok (lub w folderze wyjściowym) folder `<nazwa> (music)`.
Pliki z wcześniejszego uruchomienia są w nim zastępowane.

| Wynik | Zawartość |
|---|---|
| **FLAC** (domyślnie) | `01 - <tytuł>.flac`, `02 - ...`: muzyka w tle. Zapętlony utwór gra pętlę dwa razy i wycisza się. `index.txt` wymienia utwory. |
| **Original audio** | Pliki muzyczne tak, jak przechowuje je gra, w folderze `original`. Ścieżki audio CD i muzyka z kartridży ich nie mają; dla nich użyj FLAC. |
| **Both** | FLAC i pliki oryginalne. |

Pliki oryginalne zawierają kod i dane gry. Trzymaj je razem z ROM-ami i nie udostępniaj ich.

## Przeciągnij i upuść

Upuść jeden lub więcej plików albo folderów na `rommusic.exe` (na Linuksie i macOS `rommusic`).
Zapisuje FLAC z ustawieniami domyślnymi, tak jak **Start** w oknie bez żadnych zmian.

## Wiersz poleceń

Narzędzie wiersza poleceń to `rommusic` (w aplikacjach: `rommusic.app/Contents/MacOS/rommusic`
lub `rommusic.AppImage rommusic ...`). Każde ustawienie okna jest dostępne jako opcja.

```
rommusic [opcje] <ROM, obraz płyty lub folder>...
```

Przykłady:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Opcja | Opis |
|---|---|
| `--all` | Także dżingle, efekty dźwiękowe i dźwięk filmów. Domyślnie zapisywana jest tylko muzyka w tle. |
| `--max N` | Najwyżej N utworów z każdego pliku. |
| `--out DIR` | Tworzy foldery `<nazwa> (music)` w `DIR` zamiast obok każdego pliku. |
| `--rate native` | Domyślnie: każdy utwór z częstotliwością, z jaką jest dekodowany. |
| `--cd` / `--rate cd` | 44.1 kHz, 16 bitów. |
| `--dvd` / `--rate dvd` | 48 kHz, 24 bity. |
| `--hires` / `--rate hires` | 96 kHz, 24 bity. |
| `--rate <Hz>` | Dowolna częstotliwość od 8000 do 192000. |
| `--bits 16\|24` | Bity na próbkę (domyślnie 16, 24 z `--dvd` i `--hires`). |
| `--format flac` | Domyślnie: tylko FLAC. |
| `--format original` | Muzyka tak, jak przechowuje ją gra, w folderze `original`, bez FLAC. |
| `--format both` | FLAC i pliki oryginalne. |
| `--seconds N` | SNES: jak długo gra działa, zanim zostanie pobrana jej muzyka (domyślnie 20). |
| `--start` | SNES: co jakiś czas naciska Start, gdy gra działa, dla gier, w których muzyka zaczyna się po ekranie tytułowym. |
| `--list` | Tylko wypisuje utwory, niczego nie zapisuje. Zapętlony utwór pokazuje jedno przejście i „loop”; jego plik gra pętlę dwa razy i wycisza się. |
| `--verbose` | Podaje, dlaczego plik lub utwór został pominięty. |
| `--no-pause` | Nie czeka na Enter na końcu. Czeka tylko okno otwarte dla upuszczonego pliku. |
| `-h`, `--help` / `-V`, `--version` | Pomoc i wersja. |

Częstotliwość inna niż `native` jest przepróbkowywana. To nie dodaje szczegółów; służy odtwarzaczom,
które chcą mieć wszystkie pliki z tą samą częstotliwością lub słabo radzą sobie z nietypowymi, jak
37,8 kHz. Przepróbkowany plik zaznacza to w tagu komentarza.

## Dołączone biblioteki

| Biblioteka | Licencja |
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

## Licencja

[AGPL-3.0](../LICENSE), jak romexplorer.
