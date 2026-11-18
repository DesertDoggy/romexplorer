# rommusic

Arrastra una ROM o una imagen de disco a `rommusic` y escribe la música del juego en FLAC.
Usa los mismos lectores que la aplicación romexplorer para la música de las ROM, y se publica
por separado (versión 0.2.1).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · **Español** · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Descarga:** [versiones de Rom Music](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Cambios:** [notas de la versión](../docs/rommusic_release_notes.md) · Parte de [romexplorer](../README.es.md)

> **rommusic no distribuye ni descarga ROM, BIOS ni claves, y no apoya la piratería.** Úsalo con
> juegos que tengas y hayas volcado tú mismo.

## Ventana

Abre `rommusic-gui` (en macOS `rommusic.app`, en Linux el AppImage de rommusic):

1. Elige archivos o una carpeta, o arrástralos a la ventana. Sirven ROM, imágenes de disco y
   carpetas de juego volcadas.
2. Elige **FLAC**, **Original audio** o **Both**.
3. Elige una carpeta de salida, o déjala vacía para escribir junto a cada archivo.
4. Pulsa **Start**.

**Options** contiene el resto: jingles, efectos de sonido y sonido de vídeos, un límite de pistas,
la frecuencia y los bits, y la ejecución de SNES (cuánto tiempo se ejecuta el juego y si se pulsa
Start mientras tanto). La ventana los recuerda.

**Android.** La app rommusic (Android 9 o posterior) es la misma ventana. Al iniciarse por primera
vez pide acceso a todos los archivos para leer tus ROM y escribir la música; los archivos y carpetas
se eligen en su propio navegador, y la escritura sigue con la pantalla apagada.

**macOS y Linux.** `rommusic.AppImage --install` añade la ventana al menú del escritorio. La app
de Mac no está notarizada: la primera vez, haz clic derecho > Abrir.

## Juegos compatibles

| Plataforma | Archivos | Música |
|---|---|---|
| Juegos en CD: PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast y otros | CHD, CUE + BIN, GDI + BIN, ISO | Pistas de audio CD; archivos de música del disco (ADX, XA, AT3 y otros, también dentro de archivos CPK y AFS) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360 y otros | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, carpetas de juego volcadas | Archivos de música del disco (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA y otros, también dentro de archivos CPK, AFS, ARC y SZS) |
| Wii U | WUA; WUD, WUX o una carpeta de título con su clave al lado | Archivos de música del disco (BFSTM y otros) |
| Nintendo DS | NDS | Archivo de sonido SDAT (streams STRM, secuencias SSEQ) |
| Nintendo 3DS | 3DS, CCI (solo descifrados) | Archivos de música del archivo de sonido (BCSTM y otros) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive / Genesis, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, también dentro de zip o 7z | Código binario de la BGM |
| SNES / Super Famicom | ROM, también dentro de zip o 7z | Código binario de la BGM |
| Nintendo 64 | ROM, también dentro de zip o 7z | Código binario de la BGM |
| PC-98 | Imágenes de disquete (FDI, HDM, FDD y otras), también dentro de zip o 7z | Archivos de música PMD y FMP del disquete |

Incluso en una plataforma compatible, no todos los juegos son compatibles, por motivos técnicos.
No se leen imágenes de disco dentro de zip o 7z, imágenes cifradas, PS3, Vita ni paquetes
descargados.

## Salida

Por cada archivo, rommusic crea una carpeta `<nombre> (music)` a su lado (o en la carpeta de
salida). Los archivos de una ejecución anterior se sustituyen.

| Salida | Contenido |
|---|---|
| **FLAC** (por defecto) | `01 - <título>.flac`, `02 - ...`: la música de fondo. Una pista en bucle repite el bucle dos veces y se desvanece. `index.txt` lista las pistas. |
| **Original audio** | Los archivos de música tal como los guarda el juego, en una carpeta `original`. Las pistas de audio CD y la música de cartucho no tienen; usa FLAC para ellas. |
| **Both** | FLAC y los archivos originales. |

Los archivos originales contienen el código y los datos del juego. Guárdalos con tus ROM y no los
compartas.

## Arrastrar y soltar

Arrastra uno o varios archivos o carpetas a `rommusic.exe` (o `rommusic` en Linux y macOS).
Escribe FLAC con los ajustes por defecto, igual que **Start** en la ventana sin cambiar nada.

## Línea de comandos

La herramienta de línea de comandos es `rommusic` (dentro de las apps: `rommusic.app/Contents/MacOS/rommusic`,
o `rommusic.AppImage rommusic ...`). Cada ajuste de la ventana existe como opción.

```
rommusic [opciones] <ROM, imagen de disco o carpeta>...
```

Ejemplos:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Opción | Descripción |
|---|---|
| `--all` | También jingles, efectos de sonido y el sonido de los vídeos. Por defecto solo se escribe la música de fondo. |
| `--max N` | Como máximo N pistas por archivo. |
| `--out DIR` | Pone las carpetas `<nombre> (music)` en `DIR` en lugar de junto a cada archivo. |
| `--rate native` | Por defecto: cada pista a la frecuencia con que se decodifica. |
| `--cd` / `--rate cd` | 44.1 kHz, 16 bits. |
| `--dvd` / `--rate dvd` | 48 kHz, 24 bits. |
| `--hires` / `--rate hires` | 96 kHz, 24 bits. |
| `--rate <Hz>` | Cualquier frecuencia de 8000 a 192000. |
| `--bits 16\|24` | Bits por muestra (16 por defecto, 24 con `--dvd` y `--hires`). |
| `--format flac` | Por defecto: solo FLAC. |
| `--format original` | La música tal como la guarda el juego, en una carpeta `original`, sin FLAC. |
| `--format both` | FLAC y los archivos originales. |
| `--seconds N` | SNES: cuánto tiempo se ejecuta el juego antes de tomar su música (20 por defecto). |
| `--start` | SNES: pulsa Start de vez en cuando mientras se ejecuta, para juegos cuya música empieza tras la pantalla de título. |
| `--list` | Solo lista las pistas; no escribe nada. Una pista en bucle muestra una pasada y «loop»; su archivo repite el bucle dos veces y se desvanece. |
| `--verbose` | Indica por qué se omitió un archivo o una pista. |
| `--no-pause` | No espera Intro al final. Solo espera en la ventana que se abre al arrastrar un archivo. |
| `-h`, `--help` / `-V`, `--version` | Ayuda y versión. |

Una frecuencia distinta de `native` se remuestrea. Remuestrear no añade detalle; es para
reproductores que quieren todos los archivos a la misma frecuencia o que llevan mal frecuencias
poco comunes como 37,8 kHz. Un archivo remuestreado lo indica en su etiqueta de comentario.

## Bibliotecas incluidas

| Biblioteca | Licencia |
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

## Licencia

[AGPL-3.0](../LICENSE), como romexplorer.
