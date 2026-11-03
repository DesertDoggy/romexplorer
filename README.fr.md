# romexplorer

[English](README.md) · [日本語](README.ja.md) · **Français** · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Ceci est une traduction. En cas de divergence, c'est le README anglais qui fait foi.*

Un gestionnaire de collections de ROM : il analyse vos dossiers, identifie chaque fichier par son
hash grâce aux DAT de référence, récupère métadonnées et visuels, puis convertit, vérifie et
copie les fichiers. Il fonctionne comme serveur avec une console et une interface graphique web
accessibles depuis le navigateur, ou comme application de bureau, sous Windows, macOS et Linux.
Consultez [docs/release_notes.md](docs/release_notes.md) pour savoir ce qui est testé et où.

## Ce qu'il fait

- **Analyser et identifier.** Parcourt un répertoire, détecte la plateforme de chaque fichier
  d'après son contenu, calcule ses hashs (CRC32, MD5, SHA-1, SHA-256, XXH3, RetroAchievements) et
  le rapproche du catalogue, d'abord par hash, puis par nom.
- **Lire dans les conteneurs.** CHD, RVZ/WIA/GCZ, zip et 7z sont décodés en flux : une image
  disque est hachée et identifiée sans être d'abord extraite sur le disque.
- **Convertir et réempaqueter.** Créer et extraire des CHD (CD, GD-ROM, DVD) et des RVZ ; zip et
  7z ; ajouter ou retirer les en-têtes NES/SNES ; reconstruire un `.cue`, `.gdi` ou `.m3u` manquant.
- **Catalogue.** Importe les DAT Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements et
  RetroBIOS dans un MariaDB intégré, et fusionne les sorties régionales et les titres.
- **Métadonnées et visuels.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames,
  Hasheous, PlayMatch, EmuReady et RetroAchievements, chacun avec sa propre limite de requêtes.
- **Copier vers un appareil.** Copie les jeux sélectionnés vers un dossier ou une carte, en les
  convertissant au passage, avec les noms et l'arborescence qu'attend un frontend.
- **Vérifier et sauvegarder.** Vérifications d'intégrité par rapport aux hashs enregistrés ;
  sauvegardes zstd de la base de données et du catalogue ; restauration.
- **Partager entre machines.** Le catalogue et les visuels d'un serveur principal peuvent
  être répliqués par des clients et des pairs ; un export SQLite alimente un téléphone.

Tout est accessible via l'API REST (`/api/v1/...`), la console web (`/`) et l'interface graphique
web (`/app/`).

## Installation

Il existe trois paquets. Choisissez-en un :

| paquet | contenu | installation |
|---|---|---|
| **server** | serveur, processus d'arrière-plan, MariaDB intégré, interface graphique web | lancer l'installateur |
| **desktop** | application graphique avec son propre MariaDB | il suffit de la lancer |
| **cli** | chdlite : outils autonomes de conversion / extraction / hash, sans base de données | il suffit de l'exécuter |

### Serveur

1. Décompressez le paquet.
2. Facultatif : copiez `romexplorer.conf.example` en `romexplorer.conf` et modifiez-le. Toutes
   les valeurs sont facultatives ; un mot de passe laissé vide est généré. Ce fichier n'est lu
   qu'à la première installation.
3. Lancez l'installateur :
   - Linux / macOS : `./install.sh` -- en root, il s'installe dans `/opt/romexplorer` comme
     service système ; en utilisateur ordinaire, dans `~/.local/opt/romexplorer` comme service
     utilisateur.
   - Windows : `.\install.ps1`, depuis un PowerShell lancé en administrateur.
4. Le premier démarrage crée la base de données, ses comptes et le compte administrateur,
   puis affiche les identifiants de la console **une seule fois**. Conservez-les.
5. Ouvrez `http://127.0.0.1:7337/` et connectez-vous.

Par défaut, le serveur écoute sur `127.0.0.1:7337` et le MariaDB intégré sur `3307`. Définissez
`server_bind=0.0.0.0` pour servir le réseau local ; les autres machines sont alors servies en TLS,
avec un certificat que le serveur crée au premier démarrage.

Ensuite :

- Changez le mot de passe administrateur dans la console, ou avec `romexplorer_server reset-admin`.
- Vérifiez une installation avec `romexplorer_dbtool check` : la commande liste les bibliothèques
  natives et la base de données trouvées.
- Désinstallez avec `./install.sh --uninstall` (les données sont conservées) ou
  `--uninstall --purge`.

### Mettre à jour le serveur

Lancez l'installateur de la nouvelle version par-dessus l'installation existante. Il détecte la
base de données, arrête le service, remplace les exécutables, `lib/` et les ressources fournies,
puis le redémarre. Votre base de données, vos paramètres, vos identifiants, vos médias et tout le
reste sous `data/` et `assets/` sont conservés.

- Linux / macOS, directement depuis la version la plus récente :
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- Ou décompressez le nouveau paquet et lancez son `./install.sh`, ou indiquez-le à celui déjà
  installé :
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows : décompressez le nouveau paquet et lancez son `.\install.ps1` depuis un PowerShell
  lancé en administrateur, ou `.\install.ps1 -From <package>.zip`.

Lancez-le avec le même utilisateur que pour la première installation (root ou non), et passez le
même `--prefix` / `-Prefix` si vous en aviez choisi un. Tant que romexplorer est en alpha, le
schéma de la base de données peut changer d'une version à l'autre : faites d'abord une sauvegarde
(Configuration › Sauvegarde).

### Application de bureau

Décompressez et lancez `romexplorer` (sous macOS, `romexplorer.app` ; l'application n'est pas
notarisée, le premier lancement se fait donc par clic droit > Ouvrir). Le premier lancement crée
et démarre sa propre base de données.

### Outils en ligne de commande (chdlite)

Déposez une image disque, obtenez un CHD ou un RVZ. `chdlite` (rapide), `chdcomp` (le plus
compact), `chdread` (extraction) et `chdhash` (hash) sont un outil par mode : un fichier déposé sur
l'exécutable déclenche la bonne action. `chdlite-gui` fait la même chose dans une fenêtre. Chaque
réglage existe aussi sous forme d'option de ligne de commande (`--help`). chdlite est publié
séparément : voir [docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Clés des fournisseurs

Facultatif. Saisissez les clés d'API dans Configuration › Identifiants des fournisseurs, en tant
qu'administrateur. Elles sont stockées chiffrées sur le serveur et ne sont plus jamais affichées ;
un fournisseur sans clé est simplement indisponible.

### Premiers pas

1. Ajoutez un répertoire et indiquez la plateforme qu'il contient, ou laissez la détection décider.
2. Lancez une synchronisation des DAT pour disposer d'un catalogue de référence.
3. Lancez une analyse des fichiers du répertoire.
4. Lancez une correspondance par hash, puis les requêtes aux fournisseurs pour les métadonnées et
   les visuels.

Chacune de ces opérations est une tâche ; voir ci-dessous.

### Depuis les sources

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` signale les outils manquants sans rien compiler. Sous macOS, il
faut les paquets `bash`, `bison` et `ninja` de Homebrew.

## Tâches

Toute opération lente s'exécute en **tâche** de fond, et continue donc sans navigateur ouvert.
Chacune affiche sa progression, peut être annulée et peut être planifiée (onglet Tâches planifiées)
pour se répéter toutes les N minutes, heures, jours, semaines ou mois. Les principales :

- **Synchronisation des DAT** -- télécharge et importe les DAT de référence (Redump, No-Intro,
  TOSEC, ...).
- **Téléchargement des métadonnées et médias des fournisseurs** -- récupère les informations sur
  les jeux et les visuels depuis IGDB, ScreenScraper, SteamGridDB et les autres.
- **Analyse des fichiers** -- parcourt un répertoire, calcule les hashs de ses fichiers, puis les
  rapproche du catalogue.
- **Vérification d'intégrité** -- relit les fichiers possédés et les compare à leurs hashs enregistrés.
- **Vérification et création des feuilles cue / playlists** -- vérifie que les fichiers `.cue`, `.gdi`
  et `.m3u` pointent toujours vers des fichiers valides, et reconstruit ceux qui manquent.

## Code tiers

romexplorer repose sur les projets suivants. La plupart sont utilisés comme bibliothèques,
épinglés comme sous-modules dans `external/`. « Patché » signifie que quelques lignes du code du
projet sont modifiées à la compilation ; « wrapper » signifie que nous ajoutons autour notre propre
petite bibliothèque, sans toucher à son code. Les deux se trouvent dans `deps/<name>/`, jamais dans
le sous-module. Merci à tous ces projets.

| projet | utilisé pour | nos modifications |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | patché, wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | création / extraction de CHD | patché, wrapper |
| [MAME](https://github.com/mamedev/mame) | les sources à partir desquelles chdman-simd est compilé | aucune |
| [libchdr](https://github.com/rtissera/libchdr) | son décodeur LZMA ; référence pour le lecteur CHD | aucune |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | écriture .wua / .zar | aucune |
| [zstd](https://github.com/facebook/zstd) | compression | aucune |
| [fmt](https://github.com/fmtlib/fmt) | utilisé par le code de Cemu | aucune |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | hashs RetroAchievements | patché, wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | la base de données intégrée | fichier de build patché |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | client de base de données | aucune |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | aucune |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | aucune |
| [libxaac](https://github.com/ittiam-systems/libxaac) | audio AAC | wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | clients des fournisseurs ; portage de son rapprochement par nom | code copié et modifié |
| [RomM](https://github.com/rommapp/romm) | référence, icônes de plateformes | non utilisé comme code |
| [RAChecker](https://github.com/x3kim/RAChecker) | référence pour le client RetroAchievements | non utilisé comme code |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | jaquettes PS1 / PS2, par numéro de série | téléchargées à l'usage, non incluses |

Les détails de compilation, les versions épinglées et les patchs se trouvent dans
[deps/README.md](deps/README.md). La licence de chaque projet est fournie avec les binaires dans
`lib/LICENSES/`.
