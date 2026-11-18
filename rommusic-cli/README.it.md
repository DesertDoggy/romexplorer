# rommusic

Trascina una ROM o un'immagine disco su `rommusic` e scrive la musica del gioco in FLAC.
Usa gli stessi lettori dell'app romexplorer per la musica delle ROM, ed è rilasciato
separatamente (versione 0.2.1).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · **Italiano** · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Download:** [release di Rom Music](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Modifiche:** [note di rilascio](../docs/rommusic_release_notes.md) · Parte di [romexplorer](../README.it.md)

> **rommusic non distribuisce né scarica ROM, BIOS o chiavi, e non sostiene la pirateria.** Usalo
> con giochi che possiedi e che hai estratto tu stesso.

## Finestra

Apri `rommusic-gui` (su macOS `rommusic.app`, su Linux l'AppImage di rommusic):

1. Scegli file o una cartella, oppure trascinali sulla finestra. Vanno bene ROM, immagini disco e
   cartelle di gioco estratte.
2. Scegli **FLAC**, **Original audio** o **Both**.
3. Scegli una cartella di destinazione, o lasciala vuota per scrivere accanto a ogni file.
4. Premi **Start**.

**Options** contiene il resto: jingle, effetti sonori e audio dei filmati, un limite di tracce,
frequenza e bit, e l'esecuzione SNES (per quanto gira il gioco, e se premere Start nel frattempo).
La finestra li ricorda.

**Android.** L'app rommusic (Android 9 o successivo) è la stessa finestra. Al primo avvio chiede
l'accesso a tutti i file per leggere le ROM e scrivere la musica; file e cartelle si scelgono nel
suo browser, e la scrittura continua a schermo spento.

**macOS e Linux.** `rommusic.AppImage --install` aggiunge la finestra al menu del desktop. L'app
Mac non è notarizzata: al primo avvio fai clic destro > Apri.

## Giochi supportati

| Piattaforma | File | Musica |
|---|---|---|
| Giochi su CD: PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast e altri | CHD, CUE + BIN, GDI + BIN, ISO | Tracce audio CD; file musicali sul disco (ADX, XA, AT3 e altri, anche in archivi CPK e AFS) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360 e altri | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, cartelle di gioco estratte | File musicali sul disco (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA e altri, anche in archivi CPK, AFS, ARC e SZS) |
| Wii U | WUA; WUD, WUX o una cartella del titolo con la chiave accanto | File musicali sul disco (BFSTM e altri) |
| Nintendo DS | NDS | Archivio sonoro SDAT (stream STRM, sequenze SSEQ) |
| Nintendo 3DS | 3DS, CCI (solo decifrati) | File musicali nell'archivio sonoro (BCSTM e altri) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive / Genesis, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, anche dentro zip o 7z | Codice binario della BGM |
| SNES / Super Famicom | ROM, anche dentro zip o 7z | Codice binario della BGM |
| Nintendo 64 | ROM, anche dentro zip o 7z | Codice binario della BGM |
| PC-98 | Immagini floppy (FDI, HDM, FDD e altre), anche dentro zip o 7z | File musicali PMD e FMP sul floppy |

Anche su una piattaforma supportata, non tutti i giochi sono supportati, per motivi tecnici. Non
vengono lette immagini disco dentro zip o 7z, immagini cifrate, PS3, Vita e pacchetti scaricati.

## Output

Per ogni file, rommusic crea una cartella `<nome> (music)` accanto (o nella cartella di
destinazione). I file di un'esecuzione precedente vengono sostituiti.

| Output | Contenuto |
|---|---|
| **FLAC** (predefinito) | `01 - <titolo>.flac`, `02 - ...`: la musica di sottofondo. Una traccia in loop ripete il loop due volte e sfuma. `index.txt` elenca le tracce. |
| **Original audio** | I file musicali come li conserva il gioco, in una cartella `original`. Tracce audio CD e musica delle cartucce non ne hanno; per queste usa FLAC. |
| **Both** | FLAC e i file originali. |

I file originali contengono codice e dati del gioco. Tienili con le tue ROM e non condividerli.

## Trascina e rilascia

Trascina uno o più file o cartelle su `rommusic.exe` (o `rommusic` su Linux e macOS). Scrive
FLAC con le impostazioni predefinite, come **Start** nella finestra senza cambiare nulla.

## Riga di comando

Lo strumento da riga di comando è `rommusic` (nelle app: `rommusic.app/Contents/MacOS/rommusic`,
o `rommusic.AppImage rommusic ...`). Ogni impostazione della finestra esiste come opzione.

```
rommusic [opzioni] <ROM, immagine disco o cartella>...
```

Esempi:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Opzione | Descrizione |
|---|---|
| `--all` | Anche jingle, effetti sonori e audio dei filmati. Di default si scrive solo la musica di sottofondo. |
| `--max N` | Al massimo N tracce per file. |
| `--out DIR` | Mette le cartelle `<nome> (music)` in `DIR` invece che accanto a ogni file. |
| `--rate native` | Predefinito: ogni traccia alla frequenza a cui viene decodificata. |
| `--cd` / `--rate cd` | 44.1 kHz, 16 bit. |
| `--dvd` / `--rate dvd` | 48 kHz, 24 bit. |
| `--hires` / `--rate hires` | 96 kHz, 24 bit. |
| `--rate <Hz>` | Qualsiasi frequenza da 8000 a 192000. |
| `--bits 16\|24` | Bit per campione (16 di default, 24 con `--dvd` e `--hires`). |
| `--format flac` | Predefinito: solo FLAC. |
| `--format original` | La musica come la conserva il gioco, in una cartella `original`, senza FLAC. |
| `--format both` | FLAC e i file originali. |
| `--seconds N` | SNES: per quanto gira il gioco prima di prenderne la musica (20 di default). |
| `--start` | SNES: preme Start ogni tanto mentre il gioco gira, per i giochi la cui musica parte dopo la schermata del titolo. |
| `--list` | Elenca solo le tracce, senza scrivere nulla. Una traccia in loop mostra un passaggio e "loop"; il suo file ripete il loop due volte e sfuma. |
| `--verbose` | Dice perché un file o una traccia è stato saltato. |
| `--no-pause` | Non attende Invio alla fine. Attende solo nella finestra che si apre per un file trascinato. |
| `-h`, `--help` / `-V`, `--version` | Aiuto e versione. |

Una frequenza diversa da `native` viene ricampionata. Il ricampionamento non aggiunge dettaglio;
serve ai lettori che vogliono tutti i file alla stessa frequenza o che gestiscono male frequenze
insolite come 37,8 kHz. Un file ricampionato lo indica nel tag di commento.

## Librerie incluse

| Libreria | Licenza |
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
| [98fmplayer](https://github.com/myon98/98fmplayer) | [BSD-2-Clause](https://github.com/myon98/98fmplayer) |
| [vgmstream](https://github.com/vgmstream/vgmstream) | [ISC](https://github.com/vgmstream/vgmstream/blob/master/COPYING) |
| [FFmpeg](https://github.com/FFmpeg/FFmpeg) | [LGPL-2.1+](https://github.com/FFmpeg/FFmpeg/blob/master/LICENSE.md) |
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

## Licenza

[AGPL-3.0](../LICENSE), come romexplorer.
