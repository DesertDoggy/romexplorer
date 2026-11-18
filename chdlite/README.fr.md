# chdlite

Déposez une image disque (CUE + BIN, GDI + BIN, ISO) sur chdlite et elle devient un CHD ou un RVZ ;
déposez un `.chd` ou un `.rvz` et vous récupérez le CUE + BIN, GDI + BIN ou ISO. Il lit aussi les
métadonnées et détecte la plateforme, calcule le hash du contenu des CHD, RVZ, CSO et PBP, met les
dumps NES, N64 et SNES dans le format choisi et déchiffre les titres 3DS. Aucune base de données ni
configuration n'est nécessaire. Il utilise le cœur de romexplorer et est publié séparément (version 0.3.4).

[English](README.md) · [日本語](README.ja.md) · **Français** · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Téléchargement :** [versions de CHD Lite](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Changements :** [notes de version](../docs/chdlite_release_notes.md) · Fait partie de [romexplorer](../README.fr.md)

> **chdlite ne distribue ni ne télécharge de ROM, de BIOS ou de clés, et ne soutient pas le
> piratage.** Utilisez-le avec des jeux que vous possédez et que vous avez extraits vous-même.

## Fenêtre

Ouvrez `chdlite-gui` (sur macOS `chdlite.app`, sur Linux l'AppImage de chdlite). Il a quatre modes :

| Mode | Description |
|---|---|
| **Lite** | Compresse rapidement. Un `.chd` ou `.rvz` est extrait en CUE + BIN, GDI + BIN ou ISO. |
| **Comp** | Compresse lentement, pour le fichier le plus petit. Un `.chd` ou `.rvz` est extrait de la même façon. |
| **Read** | Lit les métadonnées et détecte la plateforme. |
| **Hash** | Calcule le hash du contenu d'un fichier. |

Choisissez un mode, puis déposez des fichiers ou dossiers sur la fenêtre (ou choisissez-les).
**Settings** contient le reste : threads et fichiers simultanés, dossier de sortie et remplacement
des sorties existantes, préréglage Lite et codecs Comp, codec, niveau et taille de bloc RVZ, hashs,
style de cue et feuille GD-ROM, format des dumps de cartouche et des titres 3DS, et confirmation
avant une sortie avec perte (WUA, ZAR, XISO). La fenêtre les mémorise, et la ligne de commande
utilise les mêmes réglages.

**Android.** L'application chdlite est la même fenêtre. Les fichiers et dossiers se choisissent
dans son propre navigateur.

**macOS et Linux.** L'app Mac n'est pas notarisée : au premier lancement, faites clic droit > Ouvrir.

## Conversion

Le format de sortie est choisi selon l'image.

**Compresser**

| Entrée | Sortie |
|---|---|
| CD : CUE + BIN, TOC + BIN, ou une piste BIN/IMG seule | CHD CD |
| GD-ROM (Dreamcast) : GDI + BIN/RAW | CHD GD-ROM |
| DVD (PS2, PSP, etc.) : ISO | CHD DVD |
| PSP : CSO | CHD DVD |
| PS1 : PBP | CHD CD |
| GameCube, Wii : ISO, GCM | RVZ |
| Wii U : WUD, WUX | WUA |
| Xbox 360 : ISO | ZAR |
| Xbox : ISO | XISO |

**Extraire**

| Entrée | Sortie |
|---|---|
| CHD CD | CUE + BIN (un BIN par piste) |
| CHD GD-ROM | GDI + BIN, ou CUE + BIN avec `--gdrom-sheet cue` |
| CHD DVD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR et XISO gardent les fichiers du jeu mais pas le disque lui-même : ils ne peuvent pas
redevenir l'image d'origine. Seules les entrées sans perte sont acceptées : pas de WUA, ZAR ni XISO.
Un disque Wii U a besoin de sa clé à côté.

**Dumps de cartouche** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`) : ils ne sont
pas compressés. Ils sont écrits dans le format choisi pour leur plateforme : NES avec ou sans
en-tête (ou en `.unh`), N64 dans l'un de ses trois ordres d'octets, SNES avec ou sans en-tête de
copieur. La présence d'un en-tête NES se vérifie par hash, donc seuls les jeux connus sont convertis.

**Titres 3DS** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` et les compressés `.zcci`, `.zcia`, `.zcxi`,
`.z3dsx`) : ils sont déchiffrés. Un jeu devient `.cci`/`.3ds`, `.cxi`, ou compressé `.zcci`/`.zcxi` ;
mises à jour, DLC et démos restent en `.cia` (`.zcia` compressé). Aucune clé n'est fournie avec chdlite.

**Hashs.** SHA-1, MD5, CRC32, SHA-256, XXH3 et le hash RetroAchievements, qui indique aussi si
RetroAchievements a des succès pour le jeu (d'après la liste incluse).

## Read

Read détecte la plateforme à partir des données du disque et y lit le numéro de série et le titre
du jeu. Il affiche aussi :

| Fichier | Affiché |
|---|---|
| CHD | Version et type de CHD (CD, GD-ROM, DVD, disque dur) ; plateforme, numéro de série et titre ; taille complète, taille du fichier et taux de compression ; taille et nombre de hunks ; taille d'unité ; codecs ; SHA-1 global et des données ; SHA-1 du parent (s'il y en a un) ; type et durée de chaque piste |
| RVZ | GameCube ou Wii ; ID et titre du jeu ; taille du disque, taille du fichier et taux ; taille de bloc |
| CSO | Version et blocs CSO ; plateforme, numéro de série et titre ; taille de l'ISO, taille du fichier et taux |
| PBP | Nombre de disques ; plateforme, numéro de série et titre ; pistes et taille de chaque disque |
| CUE + BIN, GDI + BIN, ISO | Plateforme, numéro de série et titre ; format de sortie |
| Wii U WUD, WUX | Taille du disque ; ID et version du titre |
| Titres 3DS | Conteneur (CIA, image de cartouche, NCCH, 3DSX) ; compressé ou non ; type de titre ; chiffré ou non ; format de sortie |
| Dumps de cartouche | Plateforme et taille ; format actuel (en-tête, ordre des octets) ; format de sortie |

## Glisser-déposer

Chaque outil a une seule tâche : déposer des fichiers sur son exécutable l'effectue, avec les
réglages enregistrés.

| Outil | Action au dépôt |
|---|---|
| `chdlite` | Compresse rapidement. Un `.chd` ou `.rvz` est extrait en CUE + BIN, GDI + BIN ou ISO. |
| `chdcomp` | Compresse pour le fichier le plus petit. Un `.chd` ou `.rvz` est extrait en CUE + BIN, GDI + BIN ou ISO. |
| `chdread` | Lit les métadonnées et détecte la plateforme. |
| `chdhash` | Calcule le hash du contenu d'un `.chd`, `.rvz`, `.cso` ou `.pbp`, ou du fichier lui-même. |

## Ligne de commande

Chaque outil accepte toutes les commandes et options. Dans les apps, les outils sont dans
`chdlite.app/Contents/MacOS/`, ou se lancent avec `chdlite.AppImage chdlite ...`.

```
chdlite [commande] <fichier|dossier>... [options]
```

| Commande | Description |
|---|---|
| `auto` | Les `.chd`/`.rvz` sont extraits, le reste est créé avec le préréglage rapide. |
| `create` | Crée avec le préréglage rapide. |
| `comp` | Crée le fichier le plus petit. |
| `extract` | Extrait les `.chd`, `.rvz`, `.cso` et `.pbp` en CUE + BIN, GDI + BIN ou ISO, ainsi que les titres 3DS compressés. |
| `read` | Lit les métadonnées et détecte la plateforme. |
| `hash` | Calcule le hash du contenu. |

| Option | Description |
|---|---|
| `-o`, `--output <dir>` | Écrit ici au lieu d'à côté de chaque fichier. |
| `-f`, `--force` | Remplace les sorties existantes. |
| `-np`, `--threads <n>` | Threads au total (par défaut : cœurs physiques moins 4). |
| `-jf`, `--concurrent-files <n>` | Fichiers convertis en même temps. |
| `--preset pc\|mobile` | Préréglage rapide utilisé par `create`. |
| `--comp best\|chdman\|custom` | Mode de `comp`. |
| `-c`, `--compression <list>` | Codecs CHD, jusqu'à 4. |
| `-hs`, `--hunksize <bytes>` | Taille de hunk CHD. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | Réglages RVZ. `--rvz-scrub` supprime les données inutiles, de façon irréversible. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (par défaut `sha1,rahash`). |
| `--cue-style <style>` | Style de la feuille cue extraite : `redump`, `redump_catalog` ou `chdman`. |
| `--gdrom-sheet gdi\|cue` | Feuille d'un GD-ROM extrait. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | Format d'écriture des dumps de cartouche et des titres 3DS. |
| `-h`, `--help` / `-V`, `--version` | Aide et version. |

Les valeurs par défaut viennent de `chdlite.toml`, à côté du programme ou dans votre dossier de
réglages (`--help` indique où).

## Bibliothèques incluses

| Bibliothèque | Licence |
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

## Licence

[AGPL-3.0](../LICENSE), comme romexplorer.
