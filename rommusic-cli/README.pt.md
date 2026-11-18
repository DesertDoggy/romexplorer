# rommusic

Largue uma ROM ou uma imagem de disco no `rommusic` e ele escreve a música do jogo em FLAC.
Usa os mesmos leitores que a aplicação romexplorer usa para a música das ROM, e é publicado
em separado (versão 0.2.1).

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · **Português** · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Transferência:** [versões do Rom Music](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Alterações:** [notas de versão](../docs/rommusic_release_notes.md) · Parte do [romexplorer](../README.pt.md)

> **O rommusic não distribui nem transfere ROM, BIOS ou chaves, e não apoia a pirataria.** Use-o
> com jogos que possui e que extraiu você mesmo.

## Janela

Abra o `rommusic-gui` (no macOS `rommusic.app`, no Linux o AppImage do rommusic):

1. Escolha ficheiros ou uma pasta, ou largue-os na janela. Servem ROM, imagens de disco e pastas
   de jogo extraídas.
2. Escolha **FLAC**, **Original audio** ou **Both**.
3. Escolha uma pasta de saída, ou deixe-a vazia para escrever ao lado de cada ficheiro.
4. Prima **Start**.

**Options** tem o resto: jingles, efeitos sonoros e som dos vídeos, um limite de faixas, a
frequência e os bits, e a execução SNES (quanto tempo o jogo corre e se prime Start entretanto).
A janela memoriza-os.

**Android.** A app rommusic (Android 9 ou posterior) é a mesma janela. No primeiro arranque pede
acesso a todos os ficheiros para ler as ROM e escrever a música; ficheiros e pastas escolhem-se no
seu próprio navegador, e a escrita continua com o ecrã desligado.

**macOS e Linux.** `rommusic.AppImage --install` adiciona a janela ao menu do ambiente de
trabalho. A app Mac não está notarizada: no primeiro arranque, clique com o botão direito > Abrir.

## Jogos suportados

| Plataforma | Ficheiros | Música |
|---|---|---|
| Jogos em CD: PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast e outros | CHD, CUE + BIN, GDI + BIN, ISO | Faixas de áudio CD; ficheiros de música do disco (ADX, XA, AT3 e outros, também em arquivos CPK e AFS) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360 e outros | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, pastas de jogo extraídas | Ficheiros de música do disco (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA e outros, também em arquivos CPK, AFS, ARC e SZS) |
| Wii U | WUA; WUD, WUX ou uma pasta de título com a chave ao lado | Ficheiros de música do disco (BFSTM e outros) |
| Nintendo DS | NDS | Arquivo de som SDAT (streams STRM, sequências SSEQ) |
| Nintendo 3DS | 3DS, CCI (só desencriptados) | Ficheiros de música do arquivo de som (BCSTM e outros) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive / Genesis, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, também dentro de zip ou 7z | Código binário da BGM |
| SNES / Super Famicom | ROM, também dentro de zip ou 7z | Código binário da BGM |
| Nintendo 64 | ROM, também dentro de zip ou 7z | Código binário da BGM |
| PC-98 | Imagens de disquete (FDI, HDM, FDD e outras), também dentro de zip ou 7z | Ficheiros de música PMD e FMP da disquete |

Mesmo numa plataforma suportada, nem todos os jogos são suportados, por motivos técnicos. Não são
lidas imagens de disco dentro de zip ou 7z, imagens encriptadas, PS3, Vita nem pacotes transferidos.

## Saída

Para cada ficheiro, o rommusic cria uma pasta `<nome> (music)` ao lado (ou na pasta de saída).
Os ficheiros de uma execução anterior são substituídos.

| Saída | Conteúdo |
|---|---|
| **FLAC** (predefinição) | `01 - <título>.flac`, `02 - ...`: a música de fundo. Uma faixa em ciclo toca o ciclo duas vezes e desvanece. `index.txt` lista as faixas. |
| **Original audio** | Os ficheiros de música tal como o jogo os guarda, numa pasta `original`. As faixas de áudio CD e a música dos cartuchos não os têm; use FLAC para essas. |
| **Both** | FLAC e os ficheiros originais. |

Os ficheiros originais contêm o código e os dados do jogo. Guarde-os com as suas ROM e não os
partilhe.

## Arrastar e largar

Largue um ou mais ficheiros ou pastas no `rommusic.exe` (ou `rommusic` no Linux e macOS). Escreve
FLAC com as predefinições, tal como **Start** na janela sem alterar nada.

## Linha de comandos

A ferramenta de linha de comandos é o `rommusic` (dentro das apps: `rommusic.app/Contents/MacOS/rommusic`,
ou `rommusic.AppImage rommusic ...`). Cada definição da janela existe como opção.

```
rommusic [opções] <ROM, imagem de disco ou pasta>...
```

Exemplos:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Opção | Descrição |
|---|---|
| `--all` | Também jingles, efeitos sonoros e o som dos vídeos. Por predefinição só a música de fundo é escrita. |
| `--max N` | No máximo N faixas por ficheiro. |
| `--out DIR` | Põe as pastas `<nome> (music)` em `DIR` em vez de ao lado de cada ficheiro. |
| `--rate native` | Predefinição: cada faixa à frequência a que é descodificada. |
| `--cd` / `--rate cd` | 44.1 kHz, 16 bits. |
| `--dvd` / `--rate dvd` | 48 kHz, 24 bits. |
| `--hires` / `--rate hires` | 96 kHz, 24 bits. |
| `--rate <Hz>` | Qualquer frequência de 8000 a 192000. |
| `--bits 16\|24` | Bits por amostra (16 por predefinição, 24 com `--dvd` e `--hires`). |
| `--format flac` | Predefinição: só FLAC. |
| `--format original` | A música tal como o jogo a guarda, numa pasta `original`, sem FLAC. |
| `--format both` | FLAC e os ficheiros originais. |
| `--seconds N` | SNES: quanto tempo o jogo corre antes de a música ser recolhida (20 por predefinição). |
| `--start` | SNES: prime Start de vez em quando enquanto o jogo corre, para jogos cuja música começa depois do ecrã de título. |
| `--list` | Só lista as faixas, sem escrever nada. Uma faixa em ciclo mostra uma passagem e "loop"; o seu ficheiro toca o ciclo duas vezes e desvanece. |
| `--verbose` | Diz porque é que um ficheiro ou faixa foi ignorado. |
| `--no-pause` | Não espera por Enter no fim. Só espera na janela que abre para um ficheiro largado. |
| `-h`, `--help` / `-V`, `--version` | Ajuda e versão. |

Uma frequência diferente de `native` é reamostrada. A reamostragem não acrescenta detalhe; serve
para leitores que querem todos os ficheiros à mesma frequência ou que lidam mal com frequências
invulgares como 37,8 kHz. Um ficheiro reamostrado indica-o na etiqueta de comentário.

## Bibliotecas incluídas

| Biblioteca | Licença |
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

## Licença

[AGPL-3.0](../LICENSE), como o romexplorer.
