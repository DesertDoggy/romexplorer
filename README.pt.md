# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · **Português** · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Isto é uma tradução. Em caso de diferença, a referência é o README em inglês.*

> **O romexplorer não distribui nem descarrega ROM, BIOS ou chaves, e não apoia a pirataria.**
> Use-o com as suas próprias cópias de jogos que possui.

Um gestor de coleções de ROM: analisa as suas pastas, identifica cada ficheiro pelo hash face aos
DAT de referência, obtém metadados e imagens, e converte, verifica e copia os ficheiros. Funciona
como servidor, com uma consola no navegador e uma interface gráfica web, ou como aplicação de
computador, em Windows, macOS e Linux. Consulte
[docs/release_notes.pt.md](docs/release_notes.pt.md) para saber o que foi testado e onde.

## O que faz

- **Analisar e identificar.** Percorre um diretório, deteta a plataforma de cada ficheiro a partir
  do conteúdo, calcula os hashes (CRC32, MD5, SHA-1, SHA-256, XXH3, RetroAchievements) e compara-o
  com o catálogo, primeiro pelo hash e depois pelo nome.
- **Ler dentro de contentores.** CHD, RVZ/WIA/GCZ, zip e 7z são descodificados em fluxo, pelo que
  o hash de uma imagem de disco é calculado e a imagem identificada sem ter de a extrair primeiro
  para o disco.
- **Converter e reempacotar.** Criar e extrair CHD (CD, GD-ROM, DVD) e RVZ; zip e 7z; adicionar ou
  remover cabeçalhos NES/SNES; reconstruir um `.cue`, `.gdi` ou `.m3u` em falta.
- **Catálogo.** Importa os DAT de Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements e
  RetroBIOS para um MariaDB incluído, e funde os lançamentos regionais e os títulos.
- **Metadados e imagens.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady e RetroAchievements, cada um com o seu próprio limite de pedidos.
- **Copiar para um dispositivo.** Copia jogos e as suas imagens do servidor ou desta máquina
  para uma pasta ou cartão, convertendo-os pelo caminho, com os nomes e a estrutura que um
  frontend espera; as imagens são organizadas para RetroArch ou PCSX2 / DuckStation.
- **Verificar e fazer cópias de segurança.** Verificações de integridade face aos hashes
  registados; cópias de segurança zstd da base de dados e do catálogo; restauro.
- **Partilhar entre máquinas.** Clientes e pares podem espelhar o catálogo e as imagens de um
  servidor principal; uma exportação SQLite alimenta um telemóvel.

Tudo está acessível através da API REST (`/api/v1/...`), da consola no navegador (`/`) e da
interface gráfica web (`/app/`).

## Configuração inicial

Há três pacotes. Escolha um:

| pacote | o que é | instalação |
|---|---|---|
| **server** | servidor, worker, MariaDB incluído, interface gráfica web | executar o instalador |
| **desktop** | aplicação gráfica com o seu próprio MariaDB | basta abri-la |
| **cli** | chdlite: ferramentas autónomas para converter / extrair / calcular hashes, sem base de dados | basta executá-lo |

### Servidor

1. Descompacte o pacote.
2. Opcional: copie `romexplorer.conf.example` para `romexplorer.conf` e edite-o. Todos os
   valores são opcionais; se a palavra-passe ficar em branco, é gerada uma. Este ficheiro só é
   lido na primeira instalação.
3. Execute o instalador:
   - Linux / macOS: `./install.sh` -- como root, instala em `/opt/romexplorer` como serviço do
     sistema; como utilizador comum, em `~/.local/opt/romexplorer` como serviço de utilizador.
   - Windows: `.\install.ps1`, a partir de um PowerShell com privilégios de administrador.
4. O primeiro arranque cria a base de dados, as respetivas contas e o acesso de administrador, e
   mostra os dados de acesso à consola **uma única vez**. Guarde-os.
5. Abra `http://127.0.0.1:7337/` e inicie sessão.

Predefinições: o servidor escuta em `127.0.0.1:7337` e o MariaDB incluído em `3307`. Defina
`server_bind=0.0.0.0` para servir a rede local; as outras máquinas passam então a ligar-se por TLS,
com um certificado que o servidor cria no primeiro arranque.

Depois:

- Altere a palavra-passe de administrador na consola ou com `romexplorer_server reset-admin`.
- Verifique uma instalação com `romexplorer_dbtool check`: lista as bibliotecas nativas e a base
  de dados que encontrou.
- Desinstale com `./install.sh --uninstall` (mantém os dados) ou `--uninstall --purge`.

### Atualizar o servidor

Execute o instalador da nova versão sobre a instalação existente. Este deteta a base de dados,
para o serviço, substitui os executáveis, `lib/` e os recursos incluídos, e volta a iniciá-lo. A
base de dados, as definições, as credenciais, os ficheiros de média e tudo o resto em `data/` e
`assets/` são mantidos.

- Linux / macOS, diretamente a partir da versão mais recente:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- Ou descompacte o novo pacote e execute o respetivo `./install.sh`, ou indique-o ao que já está
  instalado:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: descompacte o novo pacote e execute o respetivo `.\install.ps1` a partir de um
  PowerShell com privilégios de administrador, ou `.\install.ps1 -From <package>.zip`.

Execute-o com o mesmo utilizador da primeira instalação (root ou não) e indique o mesmo
`--prefix` / `-Prefix`, se tiver escolhido um. Enquanto o romexplorer estiver em fase alfa, o
esquema da base de dados pode mudar entre versões, por isso faça primeiro uma cópia de
segurança (Configuração › Cópia de segurança).

### Aplicação de computador

Descompacte e execute `romexplorer` (no macOS, `romexplorer.app`; não está notarizada, por isso a
primeira abertura tem de ser feita com o botão direito > Abrir). A primeira abertura cria e inicia a
sua própria base de dados.

### Ferramentas de linha de comandos (chdlite)

Largue uma imagem de disco e obtenha um CHD ou um RVZ. `chdlite` (rápido) e `chdcomp` (mais
pequeno) funcionam nos dois sentidos: uma imagem não comprimida é comprimida e um `.chd` ou `.rvz` é
extraído. `chdread` mostra os metadados de um ficheiro e `chdhash` calcula o seu hash. Cada
ferramenta faz uma só coisa, pelo que um ficheiro largado sobre o executável faz o que deve;
`chdlite-gui` faz o mesmo numa janela. Todas as opções existem também como parâmetro (`--help`). O
chdlite é publicado em separado: consulte [docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Ferramenta de linha de comandos (rommusic)

Largue no `rommusic` uma ROM, uma imagem de disco, uma pasta extraída ou um conjunto de rips de
música e ele escreve a música de fundo do jogo em FLAC numa pasta ao lado. Todas as opções existem
também como parâmetro (`--help`). Os sistemas suportados estão em
[Áudio das ROM](#áudio-das-rom). O rommusic é publicado em separado: consulte
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md).

### Chaves dos fornecedores

Opcional. Introduza as chaves de API em Configuração › Credenciais dos fornecedores, como
administrador. Um fornecedor sem chaves fica simplesmente indisponível.

### Primeira utilização

1. Adicione um diretório e indique que plataforma contém, ou deixe a deteção decidir.
2. Execute uma sincronização de DAT, para haver um catálogo com que comparar.
3. Execute uma análise de ficheiros no diretório.
4. Execute uma correspondência por hash e, depois, as consultas aos fornecedores para obter
   metadados e imagens.

Cada uma destas operações é uma tarefa; ver abaixo.

### A partir do código-fonte

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` indica as ferramentas em falta sem compilar nada. No macOS, são
necessários o `bash`, o `bison` e o `ninja` do Homebrew.

## Tarefas

Tudo o que é demorado é executado como **tarefa** em segundo plano, pelo que continua a correr sem
nenhum navegador aberto. Cada tarefa mostra o seu progresso, pode ser cancelada e pode ser
agendada (separador Tarefas agendadas) para se repetir a cada tantos minutos, horas, dias, semanas
ou meses. As principais:

- **Sincronização de DAT** -- transfere e importa os DAT de referência (Redump, No-Intro,
  TOSEC, ...).
- **Transferência de metadados e média dos fornecedores** -- obtém informações de jogos e
  imagens do IGDB, ScreenScraper, SteamGridDB e dos restantes.
- **Análise de ficheiros** -- percorre um diretório, calcula os hashes dos ficheiros e compara-os
  com o catálogo.
- **Verificação de integridade** -- relê os ficheiros presentes e compara-os com os hashes
  registados.
- **Verificação e criação de folhas / listas de reprodução** -- verifica se os ficheiros `.cue`,
  `.gdi` e `.m3u` continuam a apontar para ficheiros válidos, e reconstrói os que faltam.

## Análise e correspondência por hash

A **análise de ficheiros** percorre uma pasta e calcula o hash de cada ficheiro (CRC32, MD5,
SHA-1, SHA-256, XXH3 e o hash RA). Os arquivos (CHD, RVZ/WIA/GCZ, zip, 7z) são lidos por
dentro. Quando a análise termina, é posta em fila uma correspondência por hash da mesma pasta
com as caixas que assinalou. Com todas as caixas desligadas, não é feita nenhuma
correspondência.

| caixa | o que faz |
|---|---|
| Correspondência DAT | Liga cada jogo à release do DAT cujos hashes correspondem aos seus ficheiros (um CHD também pelas suas faixas) e depois, pelo número de série, o que nenhum hash encontrou. Offline. |
| RetroAchievements | Liga os jogos aos jogos do RetroAchievements pelo seu hash RA, a partir dos dados RA já sincronizados. Offline, sempre ligada. |
| Hasheous | Procura cada ficheiro por hash em hasheous.org (sem chave). Devolve a release e os seus ids noutros fornecedores. |
| PlayMatch | Procura os ficheiros que ainda não têm id do IGDB (sem chave), 100 por pedido. Devolve o mesmo tipo de ids. |

O Hasheous e o PlayMatch só acedem à Internet quando os dados não estão em cache local.

**Fornecedores a que o Hasheous e o PlayMatch levam:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper e MobyGames (dados e imagens, pelos ids encontrados), mais uma
pesquisa por nome no IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady e ScreenScraper para os
títulos ainda sem ligação. Os fornecedores sem chaves são ignorados.

## Cópia

**Copie jogos e as suas ilustrações para outro destino.** No modo cliente de computador também
pode copiar a partir do servidor principal: adicione a pasta montada em rede (SMB, NFS, ...) e a
raiz da coleção do servidor nas definições do cliente.

**Pode copiar um set, ou títulos, releases, Crossplats e Defs individuais.**

Modos de cópia (escolhidos ao copiar):

| modo | para | regras |
|---|---|---|
| Para esta coleção | servidor → uma pasta da coleção nesta máquina | ignora o que esta máquina já tem; as cópias contam logo como tidas aqui |
| Para um cartão SD / USB / pasta do telemóvel | qualquer pasta fora da coleção | nada é ignorado por existir noutro lado; nada é registado |
| Dentro desta coleção | conversão no local (ISO → RVZ, ...) | mantém as duas cópias |
| Dentro da coleção do servidor | a partir da própria interface do servidor | cada ficheiro é verificado; nunca substitui |
| Enviar | esta máquina → o servidor | só o que falta ao servidor; nunca substitui; o servidor volta a analisá-lo |

- As cópias vão para uma subpasta por plataforma. O estilo de nome das subpastas escolhe-se ao
  copiar.
- Os ficheiros podem ser convertidos pelo caminho, para os arquivar e poupar espaço, ou para
  outro formato de que um dispositivo precise.
- Se o ficheiro já existir no destino, é ignorado.

**As ilustrações também podem ser exportadas.** Assinale «exportar imagens depois» no diálogo de
cópia, ou use **exportar imagens...** por si só. As imagens são renomeadas e organizadas para os
emuladores:

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, a estrutura do libretro-thumbnails.
- **PCSX2 / DuckStation:** uma única pasta covers, `<serial>.<ext>`, ou com o nome do ficheiro
  do jogo.

A imagem de que fornecedor preenche cada espaço segue `media_export_priority` (primeiro as
digitalizações reais das caixas).

## Separador Coleção

A coleção é a base de dados de todos os jogos que possui, e dos outros jogos que existem em
bases de dados como Redump, No-Intro, TOSEC e IGDB (requer a transferência dos DAT / dos
metadados).

| subseparador | uma linha por |
|---|---|
| Títulos\* | Um Título é um único jogo numa única plataforma. |
| Releases\*\* | Uma Release é um único jogo em que cada região, idioma, revisão, edição, demo, etc. conta em separado. |
| Crossplats\*\*\* | Um Crossplat junta todos os Títulos do mesmo jogo em plataformas diferentes. |
| Defs\*\*\*\* | Um Def serve para escolher uma versão de um jogo entre todas as suas versões (uma família de jogos): o original mais as suas remasterizações, ports, remakes, edições GOTY, etc. Uma linha por família de jogos. |
| Jogos | Um Jogo é um conjunto completo (ou incompleto) de roms de um jogo. Se tiver várias cópias, cada uma conta como um Jogo. Um jogo pode conter várias roms (Disc 1, Disc 2, ...). |
| Roms | Uma Rom é uma única unidade de suporte do jogo. Discos separados contam como roms separadas. Uma rom pode conter vários ficheiros (Disc 1 = cue + track 1.bin + track 2.bin). |
| Ficheiros | Os ficheiros individuais no disco. |
| Conteúdos | Um Conteúdo é uma sequência de bytes, identificada pelo seu hash. Pode ser um ficheiro, ou uma única entrada dentro de um arquivo. Um conteúdo também pode conter conteúdos. |
| Sets | Um Set é a sua própria lista ordenada de Títulos / Releases / Crossplats / Defs. |
| Multimédia, Documentos, Código-fonte, Outros | Discos de vídeo e música; manuais, revistas e digitalizações; código-fonte; ferramentas, trainers, gravações. |
| Patches | Patches de compatibilidade do RetroAchievements (com transferência), patches listados nos DAT, as suas pastas de patches. |
| BIOS / firmware | Os ficheiros de sistema da máquina: BIOS, firmware, imagens de NAND e de cartão SD. |
| Áudio | Os seus ficheiros de áudio na pasta audio. Podem ser etiquetados (tal como os títulos), para que uma banda sonora toque enquanto vê a página do Título. |

\* O mesmo jogo noutra plataforma (Hoge Quest (PS2), Hoge Quest (Dreamcast)) conta como um
Título separado. Revisões (Hoge Quest, Hoge Quest (Rev A)), regiões (Hoge Quest (Japan),
Adventure of Hoge (USA)) ou idiomas (Hoge Quest (ja), Hoge Quest (fr)) diferentes são agrupados
no mesmo Título.

\*\* Ao copiar, se forem encontradas várias cópias da mesma release, ganha a completa (sem
ficheiros em falta) e depois a prioridade definida nas definições (com cabeçalho > sem
cabeçalho, z64 > n64, etc.). Ao copiar, é copiada uma única release do servidor ou do cliente,
escolhida pelas suas prioridades de região / idioma / edição (normal, económica, especial). As
releases que não possui são saltadas em favor da prioridade seguinte.

\*\*\* Hoge Quest inclui Hoge Quest (PS2) e Hoge Quest (Dreamcast).

\*\*\*\* Para quando quer jogar apenas a versão mais recente, ou a que tem mais conteúdo jogável.
Por predefinição um Def escolhe o Crossplat mais recente, mas alguns Defs têm notas
classificadas para os casos em que o Crossplat mais recente não tem conteúdo de versões
anteriores (p. ex., ao Hoge Quest 3D Remake (PC) falta a masmorra extra do Hoge Quest (GBA)).
Pode arrastar as versões para a ordem que preferir. Como a versão definitiva pode existir apenas
em plataformas atuais, pode escolher copiar o Crossplat / Título de maior prioridade da sua
coleção, ou não copiar nada quando a versão mais definitiva não está na sua coleção (ou seja,
vá jogá-la no Steam).

Nota: os Crossplats e os Defs são construídos a partir dos metadados do IGDB, que por isso têm
de ser transferidos. Uma definição pode transferir automaticamente os dados relacionados
(desligada por predefinição, porque a base de dados grande torna tudo mais lento).

## Áudio das ROM

As páginas de título podem tocar a música que está dentro das suas ROM, diretamente do ficheiro:
nada é extraído nem copiado. Execute primeiro «Indexar o áudio das ROM» no separador Áudio. O `rommusic` lê
o mesmo.

| música de | plataformas | ficheiros |
|---|---|---|
| Faixas de áudio CD | Qualquer jogo em CD: PC Engine CD, Mega-CD, Saturn, PlayStation, Dreamcast e outros | CHD, cue/bin, gdi |
| Ficheiros de música no disco | PlayStation, PlayStation 2, PSP, Saturn, Dreamcast, GameCube, Wii, Xbox, Xbox 360 | CHD, cue/bin, gdi, ISO, CSO/ZSO, RVZ/WIA/GCZ/WBFS, XISO |
| | Wii U | .wua; .wud/.wux e títulos descarregados com a chave ao lado |
| | PS3, PS Vita | pastas extraídas e desencriptadas |
| O arquivo de som | Nintendo DS (música em streaming e sequenciada), Nintendo 3DS (só imagens desencriptadas) | .nds, .3ds/.cci |
| O motor de som do jogo | Game Boy Advance: jogos feitos com o motor padrão da Nintendo (cerca de metade) | .gba |
| Rips de música | NES e Famicom Disk System (NSF, NSFE), Super Nintendo (SPC), Game Boy (GBS), PC Engine (HES), MSX (KSS), Master System e Game Gear (SGC, VGM), Mega Drive (VGM, GYM), ZX Spectrum e Amstrad CPC (AY), Atari de 8 bits (SAP), Nintendo 64 (USF), PlayStation (PSF), PlayStation 2 (PSF2), Saturn (SSF), Dreamcast (DSF), Game Boy Advance (GSF), WonderSwan (WSR), Neo Geo Pocket e Virtual Boy (VGM) | na pasta do jogo ou na biblioteca de rips, soltos ou em zip |

- A biblioteca de rips (Definições › Áudio) associa os conjuntos NSF, NSFE, GBS, SPC, HES, KSS e
  SGC às suas ROM pelo conteúdo: o mesmo conjunto toca em todas as regiões de um jogo, seja qual
  for o nome.
- Os rips de PlayStation e PlayStation 2 precisam de uma BIOS da PS2 na sua coleção; sem ela são
  saltados.
- As imagens de disco dentro de zip ou 7z ainda não são lidas. As imagens encriptadas não são
  lidas.
- O `rommusic` também gera a música de título de um jogo da Super Nintendo sem rip.

## Separador RetroAchievements

A sua conta RetroAchievements e os jogos com conquistas, segundo a última sincronização da
conta (requer `ra_username` e `ra_api_key`). O bloco da conta, em cima, mostra o seu perfil e
progresso. A página de um jogo lista as suas conquistas e os seus patches de compatibilidade,
com um botão de transferência.

Se copiar um Título ou o adicionar a um Set a partir do separador RA, são escolhidas **apenas**
as releases que têm conquistas. (P. ex., se Hoge Quest (USA) tem conquistas e Hoge Quest (Japan)
não, é copiado Hoge Quest (USA) mesmo que a sua prioridade de região seja Japan > USA.)

| subseparador | mostra |
|---|---|
| Títulos | Títulos com conquistas RetroAchievements. |
| Mestria | Títulos em que desbloqueou todas as conquistas em modo hardcore. |
| Completos | Títulos em que desbloqueou todas as conquistas em modo softcore. |
| Vencidos | Jogos que venceu mas não completou. |
| Em curso | Começados mas sem mestria nem completos (também os vencidos, a não ser que a definição `ra_include_beaten` esteja desligada). |
| Por jogar | Títulos com conquistas que ainda não começou. |

## Código de terceiros

O romexplorer assenta nestes projetos. A maioria é usada como biblioteca, fixada como submódulo em
`external/`. «Com patch» significa que algumas linhas do código do próprio projeto são alteradas
durante a compilação; «wrapper» significa que acrescentamos à sua volta uma pequena biblioteca
nossa, sem alterar o seu código. Ambos ficam em `deps/<name>/`, nunca no submódulo. Obrigado a
todos.

| projeto | usado para | as nossas alterações |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | com patch, wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | criar / extrair CHD | com patch, wrapper |
| [MAME](https://github.com/mamedev/mame) | o código-fonte a partir do qual o chdman-simd é compilado | nenhuma |
| [libchdr](https://github.com/rtissera/libchdr) | o seu descodificador LZMA; referência para o leitor de CHD | nenhuma |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | escrita de .wua / .zar | nenhuma |
| [zstd](https://github.com/facebook/zstd) | compressão | nenhuma |
| [fmt](https://github.com/fmtlib/fmt) | usado pelo código do Cemu | nenhuma |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | hashes RetroAchievements | com patch, wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | a base de dados incluída | ficheiro de compilação com patch |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | cliente de base de dados | nenhuma |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | nenhuma |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | nenhuma |
| [libxaac](https://github.com/ittiam-systems/libxaac) | áudio AAC | wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | clientes dos fornecedores; adaptação da sua correspondência por nome | código copiado e modificado |
| [RomM](https://github.com/rommapp/romm) | referência, ícones de plataformas | não usado como código |
| [RAChecker](https://github.com/x3kim/RAChecker) | referência para o cliente RetroAchievements | não usado como código |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | capas de PS1 / PS2, por número de série | transferidas quando usadas, não incluídas |

Os detalhes de compilação, as versões fixadas e os patches estão em
[deps/README.md](deps/README.md). A licença de cada projeto é distribuída com os binários em
`lib/LICENSES/`.
