# chdlite

Largue uma imagem de disco (CUE + BIN, GDI + BIN, ISO) no chdlite e ela torna-se um CHD ou RVZ;
largue um `.chd` ou `.rvz` e recupera o CUE + BIN, GDI + BIN ou ISO. Também lê os metadados e deteta
a plataforma, calcula o hash do conteúdo de CHD, RVZ, CSO e PBP, põe os dumps de NES, N64 e SNES no
formato que escolher e desencripta títulos de 3DS. Não precisa de base de dados nem de configuração.
Usa o núcleo do romexplorer e é publicado em separado (versão 0.3.4).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · **Português** · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Transferência:** [versões do CHD Lite](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Alterações:** [notas de versão](../docs/chdlite_release_notes.md) · Parte do [romexplorer](../README.pt.md)

> **O chdlite não distribui nem transfere ROM, BIOS ou chaves, e não apoia a pirataria.** Use-o com
> jogos que possui e que extraiu você mesmo.

## Janela

Abra o `chdlite-gui` (no macOS `chdlite.app`, no Linux o AppImage do chdlite). Tem quatro modos:

| Modo | Descrição |
|---|---|
| **Lite** | Comprime depressa. Um `.chd` ou `.rvz` é extraído para CUE + BIN, GDI + BIN ou ISO. |
| **Comp** | Comprime devagar, para o ficheiro mais pequeno. Um `.chd` ou `.rvz` é extraído da mesma forma. |
| **Read** | Lê os metadados e deteta a plataforma. |
| **Hash** | Calcula o hash do conteúdo de um ficheiro. |

Escolha um modo e largue ficheiros ou pastas na janela (ou escolha-os). **Settings** tem o resto:
threads e ficheiros em simultâneo, pasta de saída e substituição de saídas existentes, predefinição
Lite e codecs Comp, codec, nível e tamanho de bloco RVZ, hashes, estilo de cue e folha GD-ROM,
formato dos dumps de cartucho e dos títulos de 3DS, e confirmação antes de uma saída com perdas
(WUA, ZAR, XISO). A janela memoriza-os, e a linha de comandos usa as mesmas definições.

**Android.** A app chdlite é a mesma janela. Ficheiros e pastas escolhem-se no seu próprio
navegador.

**macOS e Linux.** A app Mac não está notarizada: no primeiro arranque, clique com o botão direito > Abrir.

## Conversão

O formato de saída é escolhido conforme a imagem.

**Comprimir**

| Entrada | Saída |
|---|---|
| CD: CUE + BIN, TOC + BIN, ou uma faixa BIN/IMG solta | CHD de CD |
| GD-ROM (Dreamcast): GDI + BIN/RAW | CHD de GD-ROM |
| DVD (PS2, PSP e outros): ISO | CHD de DVD |
| PSP: CSO | CHD de DVD |
| PS1: PBP | CHD de CD |
| GameCube, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**Extrair**

| Entrada | Saída |
|---|---|
| CHD de CD | CUE + BIN (um BIN por faixa) |
| CHD de GD-ROM | GDI + BIN, ou CUE + BIN com `--gdrom-sheet cue` |
| CHD de DVD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR e XISO mantêm os ficheiros do jogo mas não o disco em si, por isso não podem voltar à
imagem original. Só são aceites entradas sem perdas: não WUA, ZAR nem XISO. Um disco de Wii U precisa
da sua chave ao lado.

**Dumps de cartucho** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`): não são
comprimidos. São escritos no formato definido para a plataforma: NES com ou sem cabeçalho (ou como
`.unh`), N64 numa das três ordens de bytes, SNES com ou sem cabeçalho de copiador. Se um dump de NES
precisa de cabeçalho consulta-se pelo hash, por isso só jogos conhecidos são convertidos.

**Títulos de 3DS** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` e os comprimidos `.zcci`, `.zcia`, `.zcxi`,
`.z3dsx`): são desencriptados. Um jogo passa a `.cci`/`.3ds`, `.cxi`, ou comprimido `.zcci`/`.zcxi`;
atualizações, DLC e demos ficam `.cia` (`.zcia` comprimido). O chdlite não inclui chaves.

**Hashes.** SHA-1, MD5, CRC32, SHA-256, XXH3 e o hash do RetroAchievements, que também indica se o
RetroAchievements tem conquistas para o jogo (pela lista incluída).

## Read

O Read deteta a plataforma a partir dos dados do disco e lê deles o número de série e o título do
jogo. Mostra também:

| Ficheiro | Mostrado |
|---|---|
| CHD | Versão e tipo de CHD (CD, GD-ROM, DVD, disco rígido); plataforma, número de série e título; tamanho completo, tamanho do ficheiro e taxa de compressão; tamanho e número de hunks; tamanho de unidade; codecs; SHA-1 do total e dos dados; SHA-1 do pai (se houver); tipo e duração de cada faixa |
| RVZ | GameCube ou Wii; ID e título do jogo; tamanho do disco, tamanho do ficheiro e taxa; tamanho de bloco |
| CSO | Versão e blocos do CSO; plataforma, número de série e título; tamanho do ISO, tamanho do ficheiro e taxa |
| PBP | Número de discos; plataforma, número de série e título; faixas e tamanho de cada disco |
| CUE + BIN, GDI + BIN, ISO | Plataforma, número de série e título; formato de saída |
| Wii U WUD, WUX | Tamanho do disco; ID e versão do título |
| Títulos de 3DS | Contentor (CIA, imagem de cartucho, NCCH, 3DSX); comprimido ou não; tipo de título; encriptado ou não; formato de saída |
| Dumps de cartucho | Plataforma e tamanho; formato atual (cabeçalho, ordem de bytes); formato de saída |

## Arrastar e largar

Cada ferramenta tem uma só tarefa: largar ficheiros no seu executável executa-a, com as definições
guardadas.

| Ferramenta | Ação ao largar |
|---|---|
| `chdlite` | Comprime depressa. Um `.chd` ou `.rvz` é extraído para CUE + BIN, GDI + BIN ou ISO. |
| `chdcomp` | Comprime para o ficheiro mais pequeno. Um `.chd` ou `.rvz` é extraído para CUE + BIN, GDI + BIN ou ISO. |
| `chdread` | Lê os metadados e deteta a plataforma. |
| `chdhash` | Calcula o hash do conteúdo de um `.chd`, `.rvz`, `.cso` ou `.pbp`, ou do próprio ficheiro. |

## Linha de comandos

Cada ferramenta aceita todos os comandos e opções. Dentro das apps, as ferramentas estão em
`chdlite.app/Contents/MacOS/`, ou correm com `chdlite.AppImage chdlite ...`.

```
chdlite [comando] <ficheiro|pasta>... [opções]
```

| Comando | Descrição |
|---|---|
| `auto` | Os `.chd`/`.rvz` são extraídos; o resto é criado com a predefinição rápida. |
| `create` | Cria com a predefinição rápida. |
| `comp` | Cria o ficheiro mais pequeno. |
| `extract` | Extrai `.chd`, `.rvz`, `.cso` e `.pbp` para CUE + BIN, GDI + BIN ou ISO, e os títulos de 3DS comprimidos. |
| `read` | Lê os metadados e deteta a plataforma. |
| `hash` | Calcula o hash do conteúdo. |

| Opção | Descrição |
|---|---|
| `-o`, `--output <dir>` | Escreve aqui em vez de ao lado de cada ficheiro. |
| `-f`, `--force` | Substitui as saídas que já existem. |
| `-np`, `--threads <n>` | Threads no total (predefinição: núcleos físicos menos 4). |
| `-jf`, `--concurrent-files <n>` | Ficheiros convertidos em simultâneo. |
| `--preset pc\|mobile` | Predefinição rápida usada pelo `create`. |
| `--comp best\|chdman\|custom` | Modo do `comp`. |
| `-c`, `--compression <list>` | Codecs CHD, até 4. |
| `-hs`, `--hunksize <bytes>` | Tamanho de hunk CHD. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | Definições RVZ. `--rvz-scrub` remove dados inúteis e não pode ser desfeito. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (predefinição `sha1,rahash`). |
| `--cue-style <style>` | Estilo da folha cue extraída: `redump`, `redump_catalog` ou `chdman`. |
| `--gdrom-sheet gdi\|cue` | Folha de um GD-ROM extraído. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | Formato em que são escritos os dumps de cartucho e os títulos de 3DS. |
| `-h`, `--help` / `-V`, `--version` | Ajuda e versão. |

As predefinições vêm do `chdlite.toml`, ao lado do programa ou na sua pasta de definições
(`--help` indica onde).

## Bibliotecas incluídas

| Biblioteca | Licença |
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

## Licença

[AGPL-3.0](../LICENSE), como o romexplorer.
