# chdlite

Arrastra una imagen de disco (CUE + BIN, GDI + BIN, ISO) a chdlite y se convierte en CHD o RVZ;
arrastra un `.chd` o `.rvz` y recuperas el CUE + BIN, GDI + BIN o ISO. También lee los metadatos y
detecta la plataforma, calcula el hash del contenido de CHD, RVZ, CSO y PBP, pone los volcados de NES,
N64 y SNES en el formato que elijas y descifra títulos de 3DS. No necesita base de datos ni
configuración. Usa el núcleo de romexplorer y se publica por separado (versión 0.3.4).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · **Español** · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Descarga:** [versiones de CHD Lite](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Cambios:** [notas de la versión](../docs/chdlite_release_notes.md) · Parte de [romexplorer](../README.es.md)

> **chdlite no distribuye ni descarga ROM, BIOS ni claves, y no apoya la piratería.** Úsalo con
> juegos que tengas y hayas volcado tú mismo.

## Ventana

Abre `chdlite-gui` (en macOS `chdlite.app`, en Linux el AppImage de chdlite). Tiene cuatro modos:

| Modo | Descripción |
|---|---|
| **Lite** | Comprime rápido. Un `.chd` o `.rvz` se extrae a CUE + BIN, GDI + BIN o ISO. |
| **Comp** | Comprime despacio, para el archivo más pequeño. Un `.chd` o `.rvz` se extrae igual. |
| **Read** | Lee los metadatos y detecta la plataforma. |
| **Hash** | Calcula el hash del contenido de un archivo. |

Elige un modo y arrastra archivos o carpetas a la ventana (o elígelos). **Settings** contiene el
resto: hilos y archivos a la vez, carpeta de salida y sustitución de salidas existentes, preajuste
Lite y códecs Comp, códec, nivel y tamaño de bloque RVZ, hashes, estilo de cue y hoja GD-ROM,
formato de los volcados de cartucho y títulos de 3DS, y si preguntar antes de una salida con
pérdida (WUA, ZAR, XISO). La ventana los recuerda y la línea de comandos usa los mismos ajustes.

**Android.** La app chdlite es la misma ventana. Los archivos y carpetas se eligen en su propio
navegador.

**macOS y Linux.** La app de Mac no está notarizada: la primera vez, haz clic derecho > Abrir.

## Conversión

El formato de salida se elige según la imagen.

**Comprimir**

| Entrada | Salida |
|---|---|
| CD: CUE + BIN, TOC + BIN, o una pista BIN/IMG suelta | CHD de CD |
| GD-ROM (Dreamcast): GDI + BIN/RAW | CHD de GD-ROM |
| DVD (PS2, PSP y otros): ISO | CHD de DVD |
| PSP: CSO | CHD de DVD |
| PS1: PBP | CHD de CD |
| GameCube, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**Extraer**

| Entrada | Salida |
|---|---|
| CHD de CD | CUE + BIN (un BIN por pista) |
| CHD de GD-ROM | GDI + BIN, o CUE + BIN con `--gdrom-sheet cue` |
| CHD de DVD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR y XISO conservan los archivos del juego pero no el disco en sí, así que no pueden volver
a la imagen original. Solo se aceptan entradas sin pérdida: no WUA, ZAR ni XISO. Un disco de Wii U
necesita su clave al lado.

**Volcados de cartucho** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`): no se
comprimen. Se escriben en el formato elegido para su plataforma: NES con o sin cabecera (o como
`.unh`), N64 en uno de sus tres órdenes de bytes, SNES con o sin cabecera de copiadora. Si un volcado
de NES necesita cabecera se consulta por su hash, así que solo se convierten juegos conocidos.

**Títulos de 3DS** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` y los comprimidos `.zcci`, `.zcia`, `.zcxi`,
`.z3dsx`): se descifran. Un juego pasa a `.cci`/`.3ds`, `.cxi`, o comprimido `.zcci`/`.zcxi`;
actualizaciones, DLC y demos quedan como `.cia` (`.zcia` comprimido). chdlite no incluye claves.

**Hashes.** SHA-1, MD5, CRC32, SHA-256, XXH3 y el hash de RetroAchievements, que también indica si
RetroAchievements tiene logros para el juego (según la lista incluida).

## Read

Read detecta la plataforma a partir de los datos del disco y lee de ellos el número de serie y el
título del juego. También muestra:

| Archivo | Se muestra |
|---|---|
| CHD | Versión y tipo de CHD (CD, GD-ROM, DVD, disco duro); plataforma, número de serie y título; tamaño completo, tamaño del archivo y ratio de compresión; tamaño y número de hunks; tamaño de unidad; códecs; SHA-1 del total y de los datos; SHA-1 del padre (si lo hay); tipo y duración de cada pista |
| RVZ | GameCube o Wii; ID y título del juego; tamaño del disco, tamaño del archivo y ratio; tamaño de bloque |
| CSO | Versión y bloques del CSO; plataforma, número de serie y título; tamaño del ISO, tamaño del archivo y ratio |
| PBP | Número de discos; plataforma, número de serie y título; pistas y tamaño de cada disco |
| CUE + BIN, GDI + BIN, ISO | Plataforma, número de serie y título; formato de salida |
| Wii U WUD, WUX | Tamaño del disco; ID y versión del título |
| Títulos de 3DS | Contenedor (CIA, imagen de cartucho, NCCH, 3DSX); comprimido o no; tipo de título; cifrado o no; formato de salida |
| Volcados de cartucho | Plataforma y tamaño; formato actual (cabecera, orden de bytes); formato de salida |

## Arrastrar y soltar

Cada herramienta tiene una sola tarea: al arrastrar archivos a su ejecutable la realiza, con los
ajustes guardados.

| Herramienta | Acción al soltar |
|---|---|
| `chdlite` | Comprime rápido. Un `.chd` o `.rvz` se extrae a CUE + BIN, GDI + BIN o ISO. |
| `chdcomp` | Comprime para el archivo más pequeño. Un `.chd` o `.rvz` se extrae a CUE + BIN, GDI + BIN o ISO. |
| `chdread` | Lee los metadatos y detecta la plataforma. |
| `chdhash` | Calcula el hash del contenido de un `.chd`, `.rvz`, `.cso` o `.pbp`, o del archivo en sí. |

## Línea de comandos

Cada herramienta acepta todos los comandos y opciones. Dentro de las apps, las herramientas están en
`chdlite.app/Contents/MacOS/`, o se ejecutan con `chdlite.AppImage chdlite ...`.

```
chdlite [comando] <archivo|carpeta>... [opciones]
```

| Comando | Descripción |
|---|---|
| `auto` | Los `.chd`/`.rvz` se extraen; lo demás se crea con el preajuste rápido. |
| `create` | Crea con el preajuste rápido. |
| `comp` | Crea el archivo más pequeño. |
| `extract` | Extrae `.chd`, `.rvz`, `.cso` y `.pbp` a CUE + BIN, GDI + BIN o ISO, y los títulos de 3DS comprimidos. |
| `read` | Lee los metadatos y detecta la plataforma. |
| `hash` | Calcula el hash del contenido. |

| Opción | Descripción |
|---|---|
| `-o`, `--output <dir>` | Escribe aquí en lugar de junto a cada archivo. |
| `-f`, `--force` | Sustituye las salidas que ya existen. |
| `-np`, `--threads <n>` | Hilos en total (por defecto: núcleos físicos menos 4). |
| `-jf`, `--concurrent-files <n>` | Archivos convertidos a la vez. |
| `--preset pc\|mobile` | Preajuste rápido que usa `create`. |
| `--comp best\|chdman\|custom` | Modo de `comp`. |
| `-c`, `--compression <list>` | Códecs CHD, hasta 4. |
| `-hs`, `--hunksize <bytes>` | Tamaño de hunk CHD. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | Ajustes RVZ. `--rvz-scrub` elimina datos basura y no se puede deshacer. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (por defecto `sha1,rahash`). |
| `--cue-style <style>` | Estilo de la hoja cue extraída: `redump`, `redump_catalog` o `chdman`. |
| `--gdrom-sheet gdi\|cue` | Hoja de un GD-ROM extraído. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | Formato en que se escriben los volcados de cartucho y los títulos de 3DS. |
| `-h`, `--help` / `-V`, `--version` | Ayuda y versión. |

Los valores por defecto vienen de `chdlite.toml`, junto al programa o en tu carpeta de ajustes
(`--help` indica dónde).

## Bibliotecas incluidas

| Biblioteca | Licencia |
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

## Licencia

[AGPL-3.0](../LICENSE), como romexplorer.
