# rommusic

Déposez une ROM ou une image disque sur `rommusic` et il écrit la musique du jeu en FLAC.
Il utilise les mêmes lecteurs que l'application romexplorer pour la musique des ROM, et il est
publié séparément (version 0.2.1).

[English](README.md) · [日本語](README.ja.md) · **Français** · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Téléchargement :** [versions de Rom Music](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Changements :** [notes de version](../docs/rommusic_release_notes.md) · Fait partie de [romexplorer](../README.fr.md)

> **rommusic ne distribue ni ne télécharge de ROM, de BIOS ou de clés, et ne soutient pas le
> piratage.** Utilisez-le avec des jeux que vous possédez et que vous avez extraits vous-même.

## Fenêtre

Ouvrez `rommusic-gui` (sur macOS `rommusic.app`, sur Linux l'AppImage de rommusic) :

1. Choisissez des fichiers ou un dossier, ou déposez-les sur la fenêtre. ROM, images disque et
   dossiers de jeu extraits sont acceptés.
2. Choisissez **FLAC**, **Original audio** ou **Both**.
3. Choisissez un dossier de sortie, ou laissez-le vide pour écrire à côté de chaque fichier.
4. Appuyez sur **Start**.

**Options** contient le reste : jingles, effets sonores et son des vidéos, limite de pistes,
fréquence et bits, et l'exécution SNES (durée d'exécution du jeu, et appui sur Start pendant
l'exécution). La fenêtre les mémorise.

**Android.** L'application rommusic (Android 9 ou plus récent) est la même fenêtre. Au premier
lancement, elle demande l'accès à tous les fichiers pour lire vos ROM et écrire la musique ; les
fichiers et dossiers se choisissent dans son propre navigateur, et l'écriture continue écran éteint.

**macOS et Linux.** `rommusic.AppImage --install` ajoute la fenêtre au menu du bureau. L'app
Mac n'est pas notarisée : au premier lancement, faites clic droit > Ouvrir.

## Jeux pris en charge

| Plateforme | Fichiers | Musique |
|---|---|---|
| Jeux sur CD : PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast, etc. | CHD, CUE + BIN, GDI + BIN, ISO | Pistes audio CD ; fichiers musicaux du disque (ADX, XA, AT3, etc., aussi dans les archives CPK et AFS) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360, etc. | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, dossiers de jeu extraits | Fichiers musicaux du disque (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA, etc., aussi dans les archives CPK, AFS, ARC et SZS) |
| Wii U | WUA ; WUD, WUX ou dossier de titre avec sa clé à côté | Fichiers musicaux du disque (BFSTM, etc.) |
| Nintendo DS | NDS | Archive sonore SDAT (flux STRM, séquences SSEQ) |
| Nintendo 3DS | 3DS, CCI (déchiffrés uniquement) | Fichiers musicaux de l'archive sonore (BCSTM, etc.) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive / Genesis, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, aussi dans un zip ou un 7z | Code binaire de la BGM |
| SNES / Super Famicom | ROM, aussi dans un zip ou un 7z | Code binaire de la BGM |
| Nintendo 64 | ROM, aussi dans un zip ou un 7z | Code binaire de la BGM |
| PC-98 | Images de disquette (FDI, HDM, FDD, etc.), aussi dans un zip ou un 7z | Fichiers musicaux PMD et FMP de la disquette |

Même sur une plateforme prise en charge, tous les jeux ne sont pas pris en charge, pour des raisons
techniques. Les images disque dans un zip ou un 7z, les images chiffrées, la PS3, la Vita et les
paquets téléchargés ne sont pas lus.

## Sortie

Pour chaque fichier, rommusic crée un dossier `<nom> (music)` à côté (ou dans le dossier de
sortie). Les fichiers d'une exécution précédente y sont remplacés.

| Sortie | Contenu |
|---|---|
| **FLAC** (par défaut) | `01 - <titre>.flac`, `02 - ...` : la musique de fond. Une piste qui boucle joue sa boucle deux fois puis s'estompe. `index.txt` liste les pistes. |
| **Original audio** | Les fichiers musicaux tels que le jeu les stocke, dans un dossier `original`. Les pistes audio CD et la musique des cartouches n'en ont pas ; utilisez FLAC pour celles-ci. |
| **Both** | FLAC et les fichiers d'origine. |

Les fichiers d'origine contiennent le code et les données du jeu. Gardez-les avec vos ROM et ne
les partagez pas.

## Glisser-déposer

Déposez un ou plusieurs fichiers ou dossiers sur `rommusic.exe` (ou `rommusic` sous Linux et
macOS). Il écrit du FLAC avec les réglages par défaut, comme **Start** dans la fenêtre sans rien
modifier.

## Ligne de commande

L'outil en ligne de commande est `rommusic` (dans les apps : `rommusic.app/Contents/MacOS/rommusic`,
ou `rommusic.AppImage rommusic ...`). Chaque réglage de la fenêtre existe en option.

```
rommusic [options] <ROM, image disque ou dossier>...
```

Exemples :

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Option | Description |
|---|---|
| `--all` | Ajoute aussi les jingles, effets sonores et le son des vidéos. Par défaut, seule la musique de fond est écrite. |
| `--max N` | Au plus N pistes par fichier. |
| `--out DIR` | Place les dossiers `<nom> (music)` dans `DIR` au lieu d'à côté de chaque fichier. |
| `--rate native` | Par défaut : chaque piste à la fréquence de décodage. |
| `--cd` / `--rate cd` | 44.1 kHz, 16 bits. |
| `--dvd` / `--rate dvd` | 48 kHz, 24 bits. |
| `--hires` / `--rate hires` | 96 kHz, 24 bits. |
| `--rate <Hz>` | Toute fréquence de 8000 à 192000. |
| `--bits 16\|24` | Bits par échantillon (16 par défaut, 24 avec `--dvd` et `--hires`). |
| `--format flac` | Par défaut : FLAC seulement. |
| `--format original` | La musique telle que le jeu la stocke, dans un dossier `original`, sans FLAC. |
| `--format both` | FLAC et les fichiers d'origine. |
| `--seconds N` | SNES : durée d'exécution du jeu avant de prendre sa musique (20 par défaut). |
| `--start` | SNES : appuie de temps en temps sur Start pendant l'exécution, pour les jeux dont la musique commence après l'écran titre. |
| `--list` | Liste seulement les pistes, sans rien écrire. Une piste qui boucle affiche un passage et « loop » ; son fichier joue la boucle deux fois puis s'estompe. |
| `--verbose` | Indique pourquoi un fichier ou une piste a été ignoré. |
| `--no-pause` | N'attend pas Entrée à la fin. L'attente n'a lieu que dans la fenêtre ouverte pour un fichier déposé. |
| `-h`, `--help` / `-V`, `--version` | Aide et version. |

Une fréquence autre que `native` est rééchantillonnée. Cela n'ajoute pas de détail ; c'est pour les
lecteurs qui veulent tous les fichiers à la même fréquence, ou qui gèrent mal des fréquences rares
comme 37,8 kHz. Un fichier rééchantillonné l'indique dans son tag de commentaire.

## Bibliothèques incluses

| Bibliothèque | Licence |
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

## Licence

[AGPL-3.0](../LICENSE), comme romexplorer.
