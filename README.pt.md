# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · **Português** · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Isto é uma tradução. Em caso de diferença, a referência é o README em inglês.*

Um gestor de coleções de ROM: analisa as suas pastas, identifica cada ficheiro pelo hash face aos
DAT de referência, obtém metadados e imagens, e converte, verifica e copia os ficheiros. Funciona
como servidor, com uma consola no navegador e uma interface gráfica web, ou como aplicação de
computador, em Windows, macOS e Linux. Consulte
[docs/release_notes.md](docs/release_notes.md) para saber o que foi testado e onde.

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
- **Copiar para um dispositivo.** Copia os jogos selecionados para uma pasta ou cartão,
  convertendo-os pelo caminho, com os nomes e a estrutura que um frontend espera.
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

Largue uma imagem de disco e obtenha um CHD ou um RVZ. `chdlite` (rápido), `chdcomp` (mais
pequeno), `chdread` (extrair) e `chdhash` (hash) são uma ferramenta por modo, pelo que um ficheiro
largado sobre o executável faz o que deve; `chdlite-gui` faz o mesmo numa janela. Todas as opções
existem também como parâmetro (`--help`). O chdlite é publicado em separado: consulte
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Chaves dos fornecedores

Opcional. Introduza as chaves de API em Configuração › Credenciais dos fornecedores, como
administrador. São guardadas cifradas no servidor e nunca voltam a ser mostradas; um fornecedor
sem chaves fica simplesmente indisponível.

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
