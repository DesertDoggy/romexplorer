# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · **Español** · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Esto es una traducción. Si hay diferencias, la referencia es el README en inglés.*

Un gestor de colecciones de ROM: escanea tus carpetas, identifica cada archivo por su hash frente a
DAT de referencia, obtiene metadatos e imágenes, y convierte, verifica y copia los archivos.
Funciona como servidor con una consola y una interfaz gráfica web en el navegador, o como
aplicación de escritorio, en Windows, macOS y Linux. Consulta
[docs/release_notes.md](docs/release_notes.md) para ver qué se ha probado y dónde.

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
- **Copiar a un dispositivo.** Copia los juegos seleccionados a una carpeta o tarjeta, con
  conversión sobre la marcha, y con los nombres y la estructura que espera un frontend.
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

Arrastra una imagen de disco y obtén un CHD o un RVZ. `chdlite` (rápido), `chdcomp` (el más
pequeño), `chdread` (extraer) y `chdhash` (hash) son una herramienta por modo, así que un
archivo arrastrado sobre el ejecutable hace lo correcto; `chdlite-gui` hace lo mismo en una ventana.
Cada opción también está disponible como parámetro (`--help`). chdlite se publica por separado:
consulta [docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Claves de proveedores

Opcional. Introduce las claves de API en Configuración › Credenciales de proveedores como
administrador. Se guardan cifradas en el servidor y no vuelven a mostrarse; un proveedor sin claves
simplemente no está disponible.

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
