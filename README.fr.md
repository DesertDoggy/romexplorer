# romexplorer

[English](README.md) · [日本語](README.ja.md) · **Français** · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Ceci est une traduction. En cas de divergence, c'est le README anglais qui fait foi.*

> **romexplorer ne distribue ni ne télécharge de ROM, de BIOS ou de clés, et ne soutient pas
> le piratage.** Utilisez-le avec vos propres copies des jeux que vous possédez.

Un gestionnaire de collections de ROM : il analyse vos dossiers, identifie chaque fichier par son
hash grâce aux DAT de référence, récupère métadonnées et visuels, puis convertit, vérifie et
copie les fichiers. Il fonctionne comme serveur avec une console et une interface graphique web
accessibles depuis le navigateur, ou comme application de bureau, sous Windows, macOS et Linux.
Consultez [docs/release_notes.fr.md](docs/release_notes.fr.md) pour savoir ce qui est testé et où.

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
- **Copier vers un appareil.** Copie des jeux et leurs images depuis le serveur ou cette
  machine vers un dossier ou une carte, en les convertissant au passage, avec les noms et
  l'arborescence qu'attend un frontend ; les images sont rangées pour RetroArch ou
  PCSX2 / DuckStation.
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

Déposez une image disque, obtenez un CHD ou un RVZ. `chdlite` (rapide) et `chdcomp` (le plus
compact) fonctionnent dans les deux sens : une image non compressée est compressée, un `.chd` ou un
`.rvz` est extrait. `chdread` affiche les métadonnées d'un fichier et `chdhash` calcule son hash.
Chaque outil fait une seule chose : un fichier déposé sur l'exécutable déclenche la bonne action.
`chdlite-gui` fait la même chose dans une fenêtre. Chaque réglage existe aussi sous forme d'option
de ligne de commande (`--help`). chdlite est publié séparément : voir
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Outil en ligne de commande (rommusic)

Déposez une ROM, une image disque, un dossier extrait ou un set de rips musicaux sur `rommusic` : il
écrit la musique de fond du jeu en FLAC dans un dossier à côté. Chaque réglage existe aussi sous
forme d'option (`--help`). Les systèmes pris en charge sont listés dans
[Audio des ROM](#audio-des-rom). rommusic est publié séparément : voir
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md).

### Clés des fournisseurs

Facultatif. Saisissez les clés d'API dans Configuration › Identifiants des fournisseurs, en tant
qu'administrateur. Un fournisseur sans clé est simplement indisponible.

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

## Scan et correspondance par hash

Le **scan de fichiers** parcourt un dossier et calcule le hash de chaque fichier (CRC32, MD5,
SHA-1, SHA-256, XXH3 et le hash RA). Les archives (CHD, RVZ/WIA/GCZ, zip, 7z) sont hachées de
l'intérieur. À la fin du scan, une correspondance par hash du même dossier est mise en file
d'attente avec les cases que vous avez cochées. Si toutes les cases sont décochées, aucune
correspondance n'est lancée.

| case | ce qu'elle fait |
|---|---|
| Correspondance DAT | Relie chaque jeu à la release DAT dont les hashes correspondent à ses fichiers (un CHD aussi par ses pistes), puis par numéro de série pour ce qu'aucun hash n'a trouvé. Hors ligne. |
| RetroAchievements | Relie les jeux aux jeux RetroAchievements par leur hash RA, à partir des données RA déjà synchronisées. Hors ligne, toujours active. |
| Hasheous | Recherche chaque fichier par hash sur hasheous.org (sans clé). Renvoie la release et ses identifiants chez les autres fournisseurs. |
| PlayMatch | Recherche les fichiers qui n'ont toujours pas d'identifiant IGDB (sans clé), 100 par requête. Renvoie le même type d'identifiants. |

Hasheous et PlayMatch ne se connectent à Internet que si les données ne sont pas en cache local.

**Fournisseurs auxquels mènent Hasheous et PlayMatch :** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper et MobyGames (données et images, par les identifiants trouvés), plus
une recherche par nom sur IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady et ScreenScraper
pour les titres encore sans lien. Les fournisseurs sans clé sont ignorés.

## Copie

**Copiez des jeux et leurs illustrations vers une autre destination.** En mode client de
bureau, vous pouvez aussi copier depuis le serveur principal : ajoutez le dossier monté en
réseau (SMB, NFS, ...) et la racine de la collection du serveur dans les réglages du client.

**Vous pouvez copier un set, ou des titres, releases, Crossplats et Defs individuels.**

Modes de copie (choisis au moment de copier) :

| mode | pour | règles |
|---|---|---|
| Vers cette collection | serveur → un dossier de collection sur cette machine | ignore ce que cette machine possède déjà ; les copies comptent aussitôt comme possédées ici |
| Vers une carte SD / USB / un dossier de téléphone | n'importe quel dossier hors de la collection | rien n'est ignoré parce qu'il est possédé ailleurs ; rien n'est enregistré |
| Dans cette collection | conversion sur place (ISO → RVZ, ...) | garde les deux copies |
| Dans la collection du serveur | depuis l'interface du serveur lui-même | chaque fichier est vérifié ; jamais d'écrasement |
| Envoi | cette machine → le serveur | seulement ce qui manque au serveur ; jamais d'écrasement ; le serveur le rescanne |

- Les copies vont dans un sous-dossier par plateforme. Vous choisissez le style de nom des
  sous-dossiers au moment de copier.
- Les fichiers peuvent être convertis au passage, pour les archiver et gagner de la place, ou
  dans un autre format qu'exige un appareil.
- Si le fichier existe déjà à destination, il est ignoré.

**Les illustrations peuvent aussi être exportées.** Cochez « exporter les images ensuite » dans
la fenêtre de copie, ou utilisez **exporter les images...** seul. Les images sont renommées et
rangées pour les émulateurs :

- **RetroArch :** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, l'arborescence de libretro-thumbnails.
- **PCSX2 / DuckStation :** un seul dossier covers, `<serial>.<ext>`, ou nommé d'après le
  fichier du jeu.

L'image de quel fournisseur remplit chaque emplacement suit `media_export_priority` (les vrais
scans de boîte d'abord).

## Onglet Collection

La collection est la base de données de tous les jeux que vous possédez, et des autres jeux
présents dans des bases comme Redump, No-Intro, TOSEC et IGDB (téléchargement des DAT / des
métadonnées requis).

| sous-onglet | une ligne par |
|---|---|
| Titres\* | Un Titre est un seul jeu sur une seule plateforme. |
| Releases\*\* | Une Release est un seul jeu où chaque région, langue, révision, édition, démo, etc. compte séparément. |
| Crossplats\*\*\* | Un Crossplat réunit tous les Titres d'un même jeu sur différentes plateformes. |
| Defs\*\*\*\* | Un Def sert à choisir une version d'un jeu parmi toutes ses versions (une famille de jeux) : l'original et ses remasters, portages, remakes, éditions GOTY, etc. Une ligne par famille de jeux. |
| Jeux | Un Jeu est un ensemble complet (ou incomplet) de roms pour un jeu. Si vous en possédez plusieurs copies, chacune compte comme un Jeu. Un jeu peut contenir plusieurs roms (Disc 1, Disc 2, ...). |
| Roms | Une Rom est une unité de support de jeu. Des disques séparés comptent comme des roms séparées. Une rom peut contenir plusieurs fichiers (Disc 1 = cue + track 1.bin + track 2.bin). |
| Fichiers | Les fichiers individuels sur le disque. |
| Contenus | Un Contenu est une suite d'octets, identifiée par son hash. Ce peut être un fichier, ou une seule entrée dans une archive. Un contenu peut aussi contenir des contenus. |
| Sets | Un Set est votre propre liste ordonnée de Titres / Releases / Crossplats / Defs. |
| Multimédia, Documents, Code source, Divers | Disques vidéo et musique ; manuels, magazines et scans ; code source ; outils, trainers, sauvegardes. |
| Patches | Patches de compatibilité RetroAchievements (avec téléchargement), patches listés dans les DAT, vos dossiers de patches. |
| BIOS / firmware | Les fichiers système de la machine : BIOS, firmware, images NAND et de carte SD. |
| Audio | Vos fichiers audio dans le dossier audio. Ils peuvent (comme les titres) être étiquetés, pour qu'une bande-son joue pendant que vous consultez la page du Titre. |

\* Le même jeu sur une autre plateforme (Hoge Quest (PS2), Hoge Quest (Dreamcast)) compte
comme un Titre distinct. Les révisions (Hoge Quest, Hoge Quest (Rev A)), régions
(Hoge Quest (Japan), Adventure of Hoge (USA)) ou langues (Hoge Quest (ja), Hoge Quest (fr))
différentes sont regroupées dans le même Titre.

\*\* À la copie, si plusieurs copies de la même release sont trouvées, la complète (aucun
fichier manquant) l'emporte, puis la priorité définie dans les réglages (avec en-tête > sans
en-tête, z64 > n64, etc.). À la copie, une seule release est copiée depuis le serveur ou le
client, choisie selon vos priorités de région / langue / édition (normale, économique,
spéciale). Les releases que vous ne possédez pas sont sautées au profit de la priorité suivante.

\*\*\* Hoge Quest inclut Hoge Quest (PS2) et Hoge Quest (Dreamcast).

\*\*\*\* Pour quand vous voulez jouer seulement à la dernière version, ou à celle qui a le plus
de contenu jouable. Par défaut, un Def choisit le Crossplat le plus récent, mais certains Defs
portent des notes classées pour les cas où le dernier Crossplat manque de contenu des versions
précédentes (ex. Hoge Quest 3D Remake (PC) n'a pas le donjon bonus de Hoge Quest (GBA)). Vous
pouvez faire glisser les versions dans l'ordre que vous préférez. Comme la version définitive
peut n'exister que sur des plateformes actuelles, vous pouvez choisir de copier le Crossplat /
Titre prioritaire de votre collection, ou de ne rien copier quand la version la plus définitive
n'est pas dans votre collection (autrement dit : allez y jouer sur Steam).

Note : les Crossplats et les Defs sont construits à partir des métadonnées IGDB, qui doivent
donc être téléchargées. Un réglage peut télécharger automatiquement les données liées (désactivé
par défaut, car la grande base de données ralentit tout).

## Audio des ROM

Les pages de titre peuvent jouer la musique contenue dans vos ROM, directement depuis le fichier :
rien n'est extrait ni copié. Lancez d'abord « Index ROM audio » dans l'onglet Audio. `rommusic` lit
les mêmes.

| musique tirée de | plateformes | fichiers |
|---|---|---|
| Pistes audio CD | Tout jeu sur CD : PC Engine CD, Mega-CD, Saturn, PlayStation, Dreamcast, etc. | CHD, cue/bin, gdi |
| Fichiers musicaux du disque | PlayStation, PlayStation 2, PSP, Saturn, Dreamcast, GameCube, Wii, Xbox, Xbox 360 | CHD, cue/bin, gdi, ISO, CSO/ZSO, RVZ/WIA/GCZ/WBFS, XISO |
| | Wii U | .wua ; .wud/.wux et titres téléchargés avec leur clé à côté |
| | PS3, PS Vita | dossiers extraits et déchiffrés |
| L'archive sonore | Nintendo DS (musique en flux et séquencée), Nintendo 3DS (images déchiffrées uniquement) | .nds, .3ds/.cci |
| Le moteur sonore du jeu | Game Boy Advance : jeux faits avec le moteur standard de Nintendo (environ la moitié) | .gba |
| Rips musicaux | NES et Famicom Disk System (NSF, NSFE), Super Nintendo (SPC), Game Boy (GBS), PC Engine (HES), MSX (KSS), Master System et Game Gear (SGC, VGM), Mega Drive (VGM, GYM), ZX Spectrum et Amstrad CPC (AY), Atari 8 bits (SAP), Nintendo 64 (USF), PlayStation (PSF), PlayStation 2 (PSF2), Saturn (SSF), Dreamcast (DSF), Game Boy Advance (GSF), WonderSwan (WSR), Neo Geo Pocket et Virtual Boy (VGM) | dans le dossier du jeu ou la bibliothèque de rips, seuls ou en zip |

- La bibliothèque de rips (Paramètres › Audio) associe les sets NSF, NSFE, GBS, SPC, HES, KSS et
  SGC à vos ROM par leur contenu : un même set joue pour toutes les régions d'un jeu, quel que soit
  son nom.
- Les rips PlayStation et PlayStation 2 ont besoin d'un BIOS PS2 dans votre collection ; sans lui,
  ils sont sautés.
- Les images disque dans un zip ou un 7z ne sont pas encore lues. Les images chiffrées ne sont pas
  lues.
- `rommusic` produit aussi la musique de titre d'un jeu Super Nintendo sans rip.

## Onglet RetroAchievements

Votre compte RetroAchievements et les jeux qui ont des succès, d'après la dernière
synchronisation du compte (nécessite `ra_username` et `ra_api_key`). Le bloc du compte en haut
montre votre profil et votre progression. La page d'un jeu liste ses succès et ses patches de
compatibilité, avec un bouton de téléchargement.

Si vous copiez un Titre ou l'ajoutez à un Set depuis l'onglet RA, **seules** les releases qui
ont des succès sont choisies. (Ex. : si Hoge Quest (USA) a des succès et Hoge Quest (Japan)
n'en a pas, Hoge Quest (USA) est copié même si votre priorité de région est Japan > USA.)

| sous-onglet | affiche |
|---|---|
| Titres | Les titres qui ont des succès RetroAchievements. |
| Maîtrise | Les titres dont vous avez débloqué tous les succès en mode hardcore. |
| Terminés | Les titres dont vous avez débloqué tous les succès en mode softcore. |
| Battus | Les jeux que vous avez battus sans les terminer. |
| En cours | Commencés mais ni maîtrisés ni terminés (les battus aussi, sauf si le réglage `ra_include_beaten` est désactivé). |
| À jouer | Les titres avec des succès que vous n'avez pas commencés. |

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
