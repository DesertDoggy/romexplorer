# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · **Español** · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Esto es una traducción. Si hay diferencias, la referencia es el README en inglés.*

> **romexplorer no distribuye ni descarga ROM, BIOS ni claves, y no apoya la piratería.**
> Úsalo con tus propias copias de juegos que tengas.

Un gestor de colecciones de ROM: escanea tus carpetas, identifica cada archivo por su hash frente a
DAT de referencia, obtiene metadatos e imágenes, y convierte, verifica y copia los archivos.
Funciona como servidor con una consola y una interfaz gráfica web en el navegador, o como
aplicación de escritorio, en Windows, macOS y Linux. Consulta
[docs/release_notes.es.md](docs/release_notes.es.md) para ver qué se ha probado y dónde.

## Qué hace

- **Escanear e identificar.** Recorre un directorio, detecta la plataforma de cada archivo a partir
  de su contenido, calcula sus hashes (CRC32, MD5, SHA-1, SHA-256, XXH3, RetroAchievements) y lo
  busca en el catálogo, primero por hash y después por nombre.
- **Leer dentro de contenedores.** CHD, RVZ/WIA/GCZ, zip y 7z se decodifican como flujos, así que
  una imagen de disco se procesa e identifica sin tener que extraerla antes al disco.
- **Convertir y reempaquetar.** Crear y extraer CHD (CD, GD-ROM, DVD) y RVZ; zip y 7z; añadir o
  quitar cabeceras NES/SNES; reconstruir un `.cue`, `.gdi` o `.m3u` que falte.
- **Catálogo.** Importa los DAT de Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements y
  RetroBIOS en un MariaDB incluido, y fusiona los lanzamientos regionales y los títulos.
- **Metadatos e imágenes.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady y RetroAchievements, cada uno con su propio límite de peticiones.
- **Copiar a un dispositivo.** Copia juegos y sus imágenes desde el servidor o esta máquina a
  una carpeta o tarjeta, con conversión sobre la marcha, y con los nombres y la estructura que
  espera un frontend; las imágenes se organizan para RetroArch o PCSX2 / DuckStation.
- **Verificar y hacer copias de seguridad.** Comprobaciones de integridad frente a los hashes
  registrados; copias de seguridad zstd de la base de datos y del catálogo; restauración.
- **Compartir entre equipos.** Clientes y pares pueden replicar el catálogo y las imágenes de un
  servidor principal; una exportación SQLite alimenta un móvil.

Todo es accesible desde la API REST (`/api/v1/...`), la consola del navegador (`/`) y la interfaz
gráfica web (`/app/`).

## Configuración inicial

Hay tres paquetes. Elige uno:

| paquete | qué es | instalación |
|---|---|---|
| **server** | servidor, proceso en segundo plano, MariaDB incluido, interfaz gráfica web | ejecutar el instalador |
| **desktop** | aplicación gráfica con su propio MariaDB | basta con abrirla |
| **cli** | chdlite: herramientas independientes para convertir / extraer / calcular hashes, sin base de datos | basta con ejecutarlo |

### Servidor

1. Descomprime el paquete.
2. Opcional: copia `romexplorer.conf.example` como `romexplorer.conf` y edítalo. Todos los
   valores son opcionales; si la contraseña se deja en blanco, se genera una. Este archivo solo se
   lee en la primera instalación.
3. Ejecuta el instalador:
   - Linux / macOS: `./install.sh` -- como root se instala en `/opt/romexplorer` como servicio
     del sistema; como usuario normal, en `~/.local/opt/romexplorer` como servicio de usuario.
   - Windows: `.\install.ps1`, desde un PowerShell con permisos de administrador.
4. El primer arranque crea la base de datos, sus cuentas y el inicio de sesión de administrador,
   y muestra los datos de acceso a la consola **una sola vez**. Guárdalos.
5. Abre `http://127.0.0.1:7337/` e inicia sesión.

Valores predeterminados: el servidor escucha en `127.0.0.1:7337` y el MariaDB incluido en `3307`.
Define `server_bind=0.0.0.0` para dar servicio a la red local; los demás equipos se conectan
entonces por TLS, con un certificado que el servidor crea en el primer arranque.

Después:

- Cambia la contraseña de administrador en la consola o con `romexplorer_server reset-admin`.
- Comprueba una instalación con `romexplorer_dbtool check`: muestra las bibliotecas nativas y la
  base de datos que ha encontrado.
- Desinstala con `./install.sh --uninstall` (conserva los datos) o `--uninstall --purge`.

### Actualizar el servidor

Ejecuta el instalador de la nueva versión sobre la instalación existente. Detecta la base de
datos, detiene el servicio, sustituye los ejecutables, `lib/` y los recursos incluidos, y lo vuelve
a iniciar. Se conservan la base de datos, la configuración, las credenciales, los medios y todo lo
demás que haya en `data/` y `assets/`.

- Linux / macOS, directamente desde la versión más reciente:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- O descomprime el nuevo paquete y ejecuta su `./install.sh`, o indícaselo al ya instalado:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: descomprime el nuevo paquete y ejecuta su `.\install.ps1` desde un PowerShell con
  permisos de administrador, o `.\install.ps1 -From <package>.zip`.

Ejecútalo con el mismo usuario que la primera instalación (root o no) y pasa el mismo
`--prefix` / `-Prefix` si elegiste uno. Mientras romexplorer esté en fase alfa, el esquema de la
base de datos puede cambiar entre versiones, así que haz antes una copia de seguridad
(Configuración › Copia de seguridad).

### Escritorio

Descomprime y ejecuta `romexplorer` (en macOS, `romexplorer.app`; no está notarizada, así que la
primera vez hay que abrirla con clic derecho > Abrir). El primer arranque crea e inicia su propia
base de datos.

### Herramientas de línea de comandos (chdlite)

Arrastra una imagen de disco y obtén un CHD o un RVZ. `chdlite` (rápido) y `chdcomp` (el más
pequeño) funcionan en ambos sentidos: una imagen sin comprimir se comprime y un `.chd` o `.rvz` se
extrae. `chdread` muestra los metadatos de un archivo y `chdhash` calcula su hash. Cada herramienta
hace una sola cosa, así que un archivo arrastrado sobre el ejecutable hace lo correcto;
`chdlite-gui` hace lo mismo en una ventana. Cada opción también está disponible como parámetro
(`--help`). chdlite se publica por separado: consulta
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Herramienta de línea de comandos (rommusic)

Arrastra a `rommusic` una ROM, una imagen de disco, una carpeta volcada o un set de archivos de música en formato de audio original y
escribe la música de fondo del juego en FLAC en una carpeta a su lado. Cada opción también está
disponible como parámetro (`--help`). Los sistemas compatibles están en
[Audio de las ROM](#audio-de-las-rom). rommusic se publica por separado: consulta
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md).

### Claves de proveedores

Opcional. Introduce las claves de API en Configuración › Credenciales de proveedores como
administrador. Un proveedor sin claves simplemente no está disponible.

### Primeros pasos

1. Añade un directorio e indica qué plataforma contiene, o deja que la detección lo decida.
2. Ejecuta una sincronización de DAT para tener un catálogo con el que comparar.
3. Ejecuta un escaneo de archivos en el directorio.
4. Ejecuta una coincidencia de hash y, después, las consultas a proveedores para obtener metadatos
   e imágenes.

Cada uno de estos pasos es una tarea (ver más abajo).

### Desde el código fuente

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` informa de las herramientas que faltan sin compilar nada. En macOS
necesita `bash`, `bison` y `ninja` de Homebrew.

## Tareas

Todo lo que es lento se ejecuta como **tarea** en segundo plano, así que sigue funcionando sin
ningún navegador abierto. Cada tarea muestra su progreso, se puede cancelar y se puede programar
(pestaña Tareas programadas) para que se repita cada cierto número de minutos, horas, días, semanas
o meses. Las principales:

- **Sincronización de DAT** -- descarga e importa los DAT de referencia (Redump, No-Intro,
  TOSEC, ...).
- **Descarga de metadatos y medios de proveedores** -- obtiene información de juegos e imágenes de
  IGDB, ScreenScraper, SteamGridDB y los demás.
- **Escaneo de archivos** -- recorre un directorio, calcula los hashes de sus archivos y los
  compara con el catálogo.
- **Comprobación de integridad** -- vuelve a leer los archivos presentes y los compara con sus
  hashes registrados.
- **Comprobación y creación de hojas cue / listas de reproducción** -- comprueba que los archivos
  `.cue`, `.gdi` y `.m3u` siguen apuntando a archivos válidos, y reconstruye los que faltan.

## Escaneo y coincidencia por hash

El **escaneo de archivos** recorre una carpeta y calcula el hash de cada archivo (CRC32, MD5,
SHA-1, SHA-256, XXH3 y el hash RA). Los archivos comprimidos (CHD, RVZ/WIA/GCZ, zip, 7z) se
procesan por dentro. Al terminar el escaneo, se pone en cola una coincidencia por hash de la
misma carpeta con las casillas que hayas marcado. Con todas las casillas desmarcadas, no se
ejecuta ninguna coincidencia.

| casilla | qué hace |
|---|---|
| Coincidencia DAT | Vincula cada juego con la release del DAT cuyos hashes coinciden con sus archivos (un CHD también por sus pistas), y luego por número de serie lo que ningún hash encontró. Sin conexión. |
| RetroAchievements | Vincula los juegos con juegos de RetroAchievements por su hash RA, a partir de los datos de RA ya sincronizados. Sin conexión, siempre activa. |
| Hasheous | Busca cada archivo por hash en hasheous.org (sin clave). Devuelve la release y sus identificadores en otros proveedores. |
| PlayMatch | Busca los archivos que aún no tienen identificador de IGDB (sin clave), 100 por petición. Devuelve el mismo tipo de identificadores. |

Hasheous y PlayMatch solo se conectan a Internet cuando los datos no están en la caché local.

**Proveedores a los que llevan Hasheous y PlayMatch:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper y MobyGames (datos e imágenes, por los identificadores encontrados),
además de una búsqueda por nombre en IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady y
ScreenScraper para los títulos que aún no tienen vínculo. Los proveedores sin claves se omiten.

## Copia

**Copia juegos y sus ilustraciones a otro destino.** En modo cliente de escritorio también
puedes copiar desde el servidor principal: añade la carpeta montada en red (SMB, NFS, ...) y la
raíz de la colección del servidor en los ajustes del cliente.

**Puedes copiar un set, o títulos, releases, Crossplats y Defs sueltos.**

Modos de copia (se eligen al copiar):

| modo | para | reglas |
|---|---|---|
| A esta colección | servidor → una carpeta de la colección en esta máquina | omite lo que esta máquina ya tiene; las copias cuentan como tenidas aquí al momento |
| A una tarjeta SD / USB / carpeta del teléfono | cualquier carpeta fuera de la colección | no se omite nada por tenerlo en otro sitio; no se registra nada |
| Dentro de esta colección | conversión en el sitio (ISO → RVZ, ...) | conserva ambas copias |
| Dentro de la colección del servidor | desde la propia interfaz del servidor | se verifica cada archivo; nunca sobrescribe |
| Subir | esta máquina → el servidor | solo lo que le falta al servidor; nunca sobrescribe; el servidor lo vuelve a escanear |

- Las copias van a una subcarpeta por plataforma. El estilo de nombre de las subcarpetas se
  elige al copiar.
- Los archivos se pueden convertir por el camino, para archivarlos y ahorrar espacio, o a otro
  formato que necesite un dispositivo.
- Si el archivo ya existe en el destino, se omite.

**También se pueden exportar las ilustraciones.** Marca «exportar imágenes después» en el
diálogo de copia, o usa **exportar imágenes...** por separado. Las imágenes se renombran y se
organizan para los emuladores:

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, la estructura de libretro-thumbnails.
- **PCSX2 / DuckStation:** una sola carpeta covers, `<serial>.<ext>`, o con el nombre del
  archivo del juego.

Qué imagen de qué proveedor ocupa cada hueco sigue `media_export_priority` (primero los
escaneos reales de cajas).

## Pestaña Colección

La colección es la base de datos de todos los juegos que tienes, y de los demás juegos que
existen en bases de datos como Redump, No-Intro, TOSEC e IGDB (requiere descargar los DAT / los
metadatos).

| subpestaña | una fila por |
|---|---|
| Títulos\* | Un Título es un solo juego en una sola plataforma. |
| Releases\*\* | Una Release es un solo juego en el que cada región, idioma, revisión, edición, demo, etc. cuenta por separado. |
| Crossplats\*\*\* | Un Crossplat reúne todos los Títulos del mismo juego en distintas plataformas. |
| Defs\*\*\*\* | Un Def sirve para elegir una versión de un juego entre todas sus versiones (una familia de juegos): el original y sus remasters, ports, remakes, ediciones GOTY, etc. Una fila por familia de juegos. |
| Juegos | Un Juego es un conjunto completo (o incompleto) de roms de un juego. Si tienes varias copias, cada una cuenta como un Juego. Un juego puede contener varias roms (Disc 1, Disc 2, ...). |
| Roms | Una Rom es una unidad de soporte del juego. Los discos separados cuentan como roms separadas. Una rom puede contener varios archivos (Disc 1 = cue + track 1.bin + track 2.bin). |
| Archivos | Los archivos individuales en disco. |
| Contenidos | Un Contenido es una secuencia de bytes, identificada por su hash. Puede ser un archivo, o una sola entrada dentro de un archivo comprimido. Un contenido también puede contener contenidos. |
| Sets | Un Set es tu propia lista ordenada de Títulos / Releases / Crossplats / Defs. |
| Multimedia, Documentos, Código fuente, Otros | Discos de vídeo y música; manuales, revistas y escaneos; código fuente; herramientas, trainers, partidas guardadas. |
| Parches | Parches de compatibilidad de RetroAchievements (con descarga), parches listados en los DAT, tus carpetas de parches. |
| BIOS / firmware | Los archivos de sistema de la máquina: BIOS, firmware, imágenes de NAND y de tarjeta SD. |
| Audio | Tus archivos de audio en la carpeta audio. Se pueden etiquetar (igual que los títulos) para que suene una banda sonora mientras ves la página del Título. |

\* El mismo juego en otra plataforma (Hoge Quest (PS2), Hoge Quest (Dreamcast)) cuenta como un
Título aparte. Las distintas revisiones (Hoge Quest, Hoge Quest (Rev A)), regiones
(Hoge Quest (Japan), Adventure of Hoge (USA)) o idiomas (Hoge Quest (ja), Hoge Quest (fr)) se
agrupan en el mismo Título.

\*\* Al copiar, si se encuentran varias copias de la misma release, gana la completa (sin
archivos que falten) y luego la prioridad fijada en los ajustes (con cabecera > sin cabecera,
z64 > n64, etc.). Al copiar, se copia una sola release del servidor o del cliente, elegida
según tus prioridades de región / idioma / edición (normal, económica, especial). Las releases
que no tienes se saltan y se pasa a la siguiente prioridad.

\*\*\* Hoge Quest incluye Hoge Quest (PS2) y Hoge Quest (Dreamcast).

\*\*\*\* Para cuando quieres jugar solo la última versión, o la que tiene más contenido jugable.
Por defecto un Def elige el Crossplat más reciente, pero algunos Defs llevan notas clasificadas
para los casos en que el último Crossplat no tiene contenido de versiones anteriores (p. ej.,
a Hoge Quest 3D Remake (PC) le falta la mazmorra extra de Hoge Quest (GBA)). Puedes arrastrar
las versiones al orden que prefieras. Como la versión definitiva puede existir solo en
plataformas actuales, puedes elegir copiar el Crossplat / Título de mayor prioridad de tu
colección, o no copiar nada cuando la versión más definitiva no está en tu colección (es decir,
ve a jugarla en Steam).

Nota: los Crossplats y los Defs se construyen a partir de los metadatos de IGDB, así que hay que
descargarlos. Un ajuste puede descargar automáticamente los datos relacionados (desactivado por
defecto, porque la gran base de datos lo ralentiza todo).

## Audio de las ROM

Las páginas de título pueden reproducir la música que hay dentro de tus ROM, directamente desde el
archivo: no se extrae ni se copia nada. Ejecuta primero «Indexar el audio de las ROM» en la pestaña Audio.
`rommusic` lee lo mismo.

| música de | plataformas | archivos |
|---|---|---|
| Pistas de audio CD | Cualquier juego en CD: PC Engine CD, Mega-CD, Saturn, PlayStation, Dreamcast y otros | CHD, cue/bin, gdi |
| Archivos de música del disco | PlayStation, PlayStation 2, PSP, Saturn, Dreamcast, GameCube, Wii, Xbox, Xbox 360 | CHD, cue/bin, gdi, ISO, CSO/ZSO, RVZ/WIA/GCZ/WBFS, XISO |
| | Wii U | .wua; .wud/.wux y títulos descargados con su clave al lado |
| | PS3, PS Vita | carpetas volcadas y descifradas |
| El archivo de sonido | Nintendo DS (música en streaming y secuenciada), Nintendo 3DS (solo imágenes descifradas) | .nds, .3ds/.cci |
| El motor de sonido del juego | Game Boy Advance: juegos hechos con el motor estándar de Nintendo (aproximadamente la mitad) | .gba |
| Formatos de audio originales | NES y Famicom Disk System (NSF, NSFE), Super Nintendo (SPC), Game Boy (GBS), PC Engine (HES), MSX (KSS), Master System y Game Gear (SGC, VGM), Mega Drive (VGM, GYM), ZX Spectrum y Amstrad CPC (AY), Atari de 8 bits (SAP), Nintendo 64 (USF), PlayStation (PSF), PlayStation 2 (PSF2), Saturn (SSF), Dreamcast (DSF), Game Boy Advance (GSF), WonderSwan (WSR), Neo Geo Pocket y Virtual Boy (VGM) | en la carpeta del juego o en la biblioteca de audio original, sueltos o en zip |

- La biblioteca de audio original (Ajustes › Audio) empareja los sets NSF, NSFE, GBS, SPC, HES, KSS y SGC con
  tus ROM por su contenido: un mismo set suena en todas las regiones de un juego, se llame como se
  llame.
- Los archivos de PlayStation y PlayStation 2 necesitan una BIOS de PS2 en tu colección; sin ella se
  saltan.
- Las imágenes de disco dentro de un zip o 7z todavía no se leen. Las imágenes cifradas no se leen.

## Pestaña RetroAchievements

Tu cuenta de RetroAchievements y los juegos con logros, según la última sincronización de la
cuenta (necesita `ra_username` y `ra_api_key`). El bloque de la cuenta, arriba, muestra tu
perfil y tu progreso. La página de un juego lista sus logros y sus parches de compatibilidad,
con un botón de descarga.

Si copias un Título o lo añades a un Set desde la pestaña RA, **solo** se eligen las releases
que tienen logros. (P. ej., si Hoge Quest (USA) tiene logros y Hoge Quest (Japan) no, se copia
Hoge Quest (USA) aunque tu prioridad de región sea Japan > USA.)

| subpestaña | muestra |
|---|---|
| Títulos | Títulos con logros de RetroAchievements. |
| Maestría | Títulos en los que desbloqueaste todos los logros en modo hardcore. |
| Completados | Títulos en los que desbloqueaste todos los logros en modo softcore. |
| Superados | Juegos que superaste pero no completaste. |
| En curso | Empezados pero sin maestría ni completados (también los superados, salvo que el ajuste `ra_include_beaten` esté desactivado). |
| Por jugar | Títulos con logros que no has empezado. |

## Código de terceros

romexplorer se basa en estos proyectos. La mayoría se usan como bibliotecas, fijados como
submódulos en `external/`. «Parcheado» significa que se cambian unas pocas líneas del código del
propio proyecto al compilar; «wrapper» significa que añadimos nuestra propia pequeña biblioteca a
su alrededor sin tocar su código. Ambos están en `deps/<name>/`, nunca en el submódulo. Gracias a
todos ellos.

| proyecto | se usa para | nuestros cambios |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | parcheado, wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | crear / extraer CHD | parcheado, wrapper |
| [MAME](https://github.com/mamedev/mame) | el código fuente a partir del cual se compila chdman-simd | ninguno |
| [libchdr](https://github.com/rtissera/libchdr) | su decodificador LZMA; referencia para el lector de CHD | ninguno |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | escritura de .wua / .zar | ninguno |
| [zstd](https://github.com/facebook/zstd) | compresión | ninguno |
| [fmt](https://github.com/fmtlib/fmt) | lo usa el código de Cemu | ninguno |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | hashes de RetroAchievements | parcheado, wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | la base de datos incluida | archivo de compilación parcheado |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | cliente de base de datos | ninguno |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | ninguno |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | ninguno |
| [libxaac](https://github.com/ittiam-systems/libxaac) | audio AAC | wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | clientes de proveedores; adaptación de su coincidencia por nombre | código copiado y modificado |
| [RomM](https://github.com/rommapp/romm) | referencia, iconos de plataformas | no se usa como código |
| [RAChecker](https://github.com/x3kim/RAChecker) | referencia para el cliente de RetroAchievements | no se usa como código |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | carátulas de PS1 / PS2, por número de serie | se descargan al usarse, no se incluyen |

Los detalles de compilación, las versiones fijadas y los parches están en
[deps/README.md](deps/README.md). La licencia de cada proyecto se distribuye con los binarios en
`lib/LICENSES/`.
