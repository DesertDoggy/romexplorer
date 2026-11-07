# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · **Deutsch** · [한국어](README.ko.md) · [Polski](README.pl.md)

*Dies ist eine Übersetzung. Bei Abweichungen gilt die englische README.*

> **romexplorer verbreitet und lädt keine ROMs, BIOS-Dateien oder Schlüssel herunter und
> unterstützt keine Piraterie.** Verwenden Sie es mit eigenen Dumps von Spielen, die Sie besitzen.

Ein Verwaltungsprogramm für ROM-Sammlungen: Es scannt Ihre Ordner, identifiziert jede Datei per
Hash anhand von Referenz-DATs, lädt Metadaten und Artwork und konvertiert, prüft und kopiert die
Dateien. Es läuft als Server mit Browser-Konsole und Web-GUI oder als Desktop-App, unter Windows,
macOS und Linux. Was wo getestet ist, steht in [docs/release_notes.de.md](docs/release_notes.de.md).

## Funktionen

- **Scannen und identifizieren.** Durchläuft ein Verzeichnis, erkennt die Plattform jeder Datei
  anhand ihres Inhalts, berechnet ihre Hashes (CRC32, MD5, SHA-1, SHA-256, XXH3,
  RetroAchievements) und gleicht sie mit dem Katalog ab – zuerst per Hash, dann per Name.
- **In Container hineinlesen.** CHD, RVZ/WIA/GCZ, zip und 7z werden als Stream dekodiert. Ein
  Disc-Image wird so gehasht und identifiziert, ohne vorher auf die Festplatte entpackt zu werden.
- **Konvertieren und neu packen.** CHD (CD, GD-ROM, DVD) und RVZ erstellen und entpacken; zip und
  7z; NES/SNES-Header hinzufügen oder entfernen; fehlende `.cue`-, `.gdi`- oder `.m3u`-Dateien
  neu erstellen.
- **Katalog.** Importiert DATs von Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements und
  RetroBIOS in ein mitgeliefertes MariaDB und führt regionale Releases und Titel zusammen.
- **Metadaten und Artwork.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady und RetroAchievements, jeweils mit eigenem Ratenlimit.
- **Auf ein Gerät kopieren.** Kopiert Spiele und ihre Bilder vom Server oder von diesem
  Rechner in einen Ordner oder auf eine Speicherkarte, konvertiert sie dabei und verwendet die
  Namen und die Ordnerstruktur, die ein Frontend erwartet; Bilder werden für RetroArch oder
  PCSX2 / DuckStation angeordnet.
- **Prüfen und sichern.** Integritätsprüfungen gegen gespeicherte Hashes; zstd-Sicherungen von
  Datenbank und Katalog; Wiederherstellung.
- **Zwischen Rechnern teilen.** Clients und Peers können Katalog und Artwork eines Hauptservers
  spiegeln; ein SQLite-Export versorgt ein Handy.

Alles ist über die REST-API (`/api/v1/...`), die Browser-Konsole (`/`) und die Web-GUI (`/app/`)
erreichbar.

## Ersteinrichtung

Es gibt drei Pakete. Wählen Sie eines:

| Paket | Inhalt | Einrichtung |
|---|---|---|
| **server** | Server, Worker, mitgeliefertes MariaDB, Web-GUI | Installer ausführen |
| **desktop** | GUI-App mit eigenem MariaDB | einfach starten |
| **cli** | chdlite: eigenständige Werkzeuge zum Konvertieren / Entpacken / Hashen, ohne Datenbank | einfach ausführen |

### Server

1. Entpacken Sie das Paket.
2. Optional: Kopieren Sie `romexplorer.conf.example` nach `romexplorer.conf` und bearbeiten Sie
   die Datei. Alle Werte sind optional; für ein leeres Passwort wird eines generiert. Die Datei wird nur bei
   der ersten Installation gelesen.
3. Führen Sie den Installer aus:
   - Linux / macOS: `./install.sh` -- als root wird nach `/opt/romexplorer` als Systemdienst
     installiert, als normaler Benutzer nach `~/.local/opt/romexplorer` als Benutzerdienst.
   - Windows: `.\install.ps1` in einer PowerShell mit Administratorrechten.
4. Beim ersten Start werden die Datenbank, ihre Konten und der Admin-Zugang angelegt; danach
   werden die Zugangsdaten für die Konsole **einmalig** angezeigt. Bewahren Sie sie auf.
5. Öffnen Sie `http://127.0.0.1:7337/` und melden Sie sich an.

Standardwerte: Der Server lauscht auf `127.0.0.1:7337`, das mitgelieferte MariaDB auf `3307`.
Setzen Sie `server_bind=0.0.0.0`, um im LAN erreichbar zu sein; andere Rechner werden dann über TLS
bedient, mit einem Zertifikat, das der Server beim ersten Start erzeugt.

Danach:

- Ändern Sie das Admin-Passwort in der Konsole oder mit `romexplorer_server reset-admin`.
- Prüfen Sie eine Installation mit `romexplorer_dbtool check`: Es listet die gefundenen nativen
  Bibliotheken und die Datenbank auf.
- Entfernen Sie sie mit `./install.sh --uninstall` (Daten bleiben erhalten) oder
  `--uninstall --purge`.

### Server aktualisieren

Führen Sie den Installer des neuen Release über die bestehende Installation aus. Er erkennt die
Datenbank, stoppt den Dienst, ersetzt die ausführbaren Dateien, `lib/` und die mitgelieferten
Assets und startet den Dienst wieder. Ihre Datenbank, Einstellungen, Zugangsdaten, Medien und
alles andere unter `data/` und `assets/` bleiben erhalten.

- Linux / macOS, direkt aus dem neuesten Release:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- Oder entpacken Sie das neue Paket und führen Sie dessen `./install.sh` aus, oder übergeben Sie
  es dem bereits installierten:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: Entpacken Sie das neue Paket und führen Sie dessen `.\install.ps1` in einer PowerShell
  mit Administratorrechten aus, oder `.\install.ps1 -From <package>.zip`.

Führen Sie ihn als derselbe Benutzer wie bei der ersten Installation aus (root oder nicht) und
übergeben Sie dasselbe `--prefix` / `-Prefix`, falls Sie eines gewählt haben. Solange romexplorer
im Alpha-Stadium ist, kann sich das Datenbankschema zwischen Releases ändern. Legen Sie daher
vorher eine Sicherung an (Konfiguration › Sicherung).

### Desktop

Entpacken Sie das Paket und starten Sie `romexplorer` (unter macOS `romexplorer.app`; die App ist
nicht notarisiert, daher beim ersten Start Rechtsklick > Öffnen). Beim ersten Start wird eine
eigene Datenbank angelegt und gestartet.

### Kommandozeilenwerkzeuge (chdlite)

Disc-Image darauf ziehen, CHD oder RVZ erhalten. `chdlite` (schnell) und `chdcomp` (kleinste
Datei) arbeiten in beide Richtungen: Ein unkomprimiertes Image wird komprimiert, eine `.chd` oder
`.rvz` entpackt. `chdread` zeigt die Metadaten einer Datei, `chdhash` berechnet ihren Hash. Jedes
Werkzeug hat eine Aufgabe, sodass eine auf die ausführbare Datei gezogene Datei das Richtige
bewirkt; `chdlite-gui` bietet dasselbe in einem Fenster. Jede Option gibt es auch als
Kommandozeilenparameter (`--help`). chdlite erscheint als eigenes Release: siehe
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Kommandozeilenwerkzeug (rommusic)

Ziehen Sie ein ROM, ein Disc-Image, einen gedumpten Ordner oder ein Set von Musikdateien im Original-Audioformat auf `rommusic`,
und es schreibt die Hintergrundmusik des Spiels als FLAC in einen Ordner daneben. Jede Option gibt
es auch als Flag (`--help`). Die unterstützten Systeme stehen unter [ROM-Audio](#rom-audio).
rommusic wird separat veröffentlicht: siehe
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md).

### Anbieter-Schlüssel

Optional. Geben Sie API-Schlüssel als Administrator unter Konfiguration › Anbieter-Zugangsdaten
ein. Ein Anbieter ohne Schlüssel ist einfach nicht verfügbar.

### Erste Schritte

1. Fügen Sie ein Verzeichnis hinzu und geben Sie an, welche Plattform es enthält – oder überlassen
   Sie das der Erkennung.
2. Führen Sie eine DAT-Synchronisierung aus, damit ein Katalog für den Abgleich vorhanden ist.
3. Führen Sie einen Datei-Scan für das Verzeichnis aus.
4. Führen Sie einen Hash-Abgleich aus und danach Anbieter-Abfragen für Metadaten und Artwork.

Jeder dieser Schritte ist ein Job; siehe unten.

### Aus dem Quellcode

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` meldet fehlende Werkzeuge, ohne etwas zu bauen. Unter macOS werden
`bash`, `bison` und `ninja` aus Homebrew benötigt.

## Jobs

Alles, was länger dauert, läuft als **Job** im Hintergrund und damit auch ohne geöffneten Browser
weiter. Jeder Job zeigt seinen Fortschritt, lässt sich abbrechen und kann zeitgesteuert werden
(Tab „Geplante Jobs“), um sich alle paar Minuten, Stunden, Tage, Wochen oder Monate zu wiederholen.
Die wichtigsten:

- **DAT-Synchronisierung** -- lädt die Referenz-DATs herunter und importiert sie (Redump,
  No-Intro, TOSEC, ...).
- **Download von Anbieter-Metadaten und -Medien** -- holt Spielinformationen und Artwork von IGDB,
  ScreenScraper, SteamGridDB und den anderen.
- **Datei-Scan** -- durchläuft ein Verzeichnis, hasht seine Dateien und gleicht sie mit dem
  Katalog ab.
- **Integritätsprüfung** -- liest vorhandene Dateien erneut ein und vergleicht sie mit ihren
  gespeicherten Hashes.
- **Prüfung und Erstellung von Cue-Sheets / Playlists** -- prüft, ob `.cue`-, `.gdi`- und
  `.m3u`-Dateien noch auf vorhandene Dateien verweisen, und erstellt fehlende neu.

## Scan und Hash-Abgleich

Der **Datei-Scan** durchläuft einen Ordner und berechnet für jede Datei die Hashes (CRC32, MD5,
SHA-1, SHA-256, XXH3 und den RA-Hash). Archive (CHD, RVZ/WIA/GCZ, zip, 7z) werden von innen
gehasht. Wenn der Scan endet, wird ein Hash-Abgleich desselben Ordners mit den angehakten Boxen
in die Warteschlange gestellt. Sind alle Boxen aus, läuft kein Abgleich.

| Box | was sie tut |
|---|---|
| DAT-Abgleich | Verknüpft jedes Spiel mit dem DAT-Release, dessen Hashes zu seinen Dateien passen (ein CHD auch über seine Tracks), danach über die Seriennummer, was kein Hash gefunden hat. Offline. |
| RetroAchievements | Verknüpft Spiele über ihren RA-Hash mit RetroAchievements-Spielen, aus den bereits synchronisierten RA-Daten. Offline, immer an. |
| Hasheous | Sucht jede Datei per Hash auf hasheous.org (ohne Schlüssel). Liefert das Release und seine IDs bei anderen Anbietern. |
| PlayMatch | Sucht Dateien, die noch keine IGDB-ID haben (ohne Schlüssel), 100 pro Anfrage. Liefert dieselbe Art von IDs. |

Hasheous und PlayMatch gehen nur online, wenn die Daten nicht lokal zwischengespeichert sind.

**Anbieter, zu denen Hasheous und PlayMatch führen:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper und MobyGames (Daten und Bilder, über die gefundenen IDs), dazu eine
Namenssuche bei IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady und ScreenScraper für Titel,
die noch keine Verknüpfung haben. Anbieter ohne Schlüssel werden übersprungen.

## Kopieren

**Kopieren Sie Spiele und ihre Bilder an ein anderes Ziel.** Im Desktop-Client-Modus können Sie
auch vom Hauptserver kopieren: Fügen Sie in den Client-Einstellungen das per Netzwerk
eingebundene Verzeichnis (SMB, NFS, ...) und das Sammlungs-Stammverzeichnis des Servers hinzu.

**Sie können ein Set oder einzelne Titel, Releases, Crossplats und Defs kopieren.**

Kopiermodi (beim Kopieren gewählt):

| Modus | für | Regeln |
|---|---|---|
| In diese Sammlung | Server → ein Sammlungsordner auf diesem Rechner | überspringt, was dieser Rechner schon hat; die Kopien zählen sofort als hier vorhanden |
| Auf SD-Karte / USB / Handy-Ordner | jeder Ordner außerhalb der Sammlung | nichts wird übersprungen, weil es anderswo vorhanden ist; nichts wird erfasst |
| Innerhalb dieser Sammlung | Konvertieren an Ort und Stelle (ISO → RVZ, ...) | behält beide Kopien |
| Innerhalb der Sammlung des Servers | aus der Oberfläche des Servers selbst | jede Datei wird geprüft; überschreibt nie |
| Hochladen | dieser Rechner → der Server | nur, was dem Server fehlt; überschreibt nie; der Server scannt es neu |

- Kopien landen in einem Unterordner pro Plattform. Den Namensstil der Unterordner wählen Sie
  beim Kopieren.
- Dateien können unterwegs konvertiert werden, um sie platzsparend zu archivieren oder in ein
  anderes Format zu bringen, das ein Gerät braucht.
- Existiert die Zieldatei bereits, wird sie übersprungen.

**Bilder lassen sich auch exportieren.** Haken Sie im Kopierdialog „Bilder danach exportieren“
an, oder verwenden Sie **Bilder exportieren...** für sich allein. Die Bilddateien werden für
Emulatoren umbenannt und angeordnet:

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, die Struktur von libretro-thumbnails.
- **PCSX2 / DuckStation:** ein einziger covers-Ordner, `<serial>.<ext>`, oder nach der
  Spieldatei benannt.

Welches Bild welches Anbieters einen Platz füllt, folgt `media_export_priority` (echte
Hüllen-Scans zuerst).

## Tab Sammlung

Die Sammlung ist die Datenbank aller Spiele, die Sie besitzen, und der übrigen Spiele, die es
in Datenbanken wie Redump, No-Intro, TOSEC und IGDB gibt (Download der DATs / Metadaten nötig).

| Unter-Tab | eine Zeile pro |
|---|---|
| Titel\* | Ein Titel ist ein einzelnes Spiel auf einer einzelnen Plattform. |
| Releases\*\* | Ein Release ist ein einzelnes Spiel, bei dem jede Region, Sprache, Revision, Edition, Demo usw. einzeln zählt. |
| Crossplats\*\*\* | Ein Crossplat fasst alle Titel desselben Spiels auf verschiedenen Plattformen zusammen. |
| Defs\*\*\*\* | Ein Def dient dazu, eine Version eines Spiels aus all seinen Versionen (einer Spielfamilie) zu wählen: das Original samt Remasters, Portierungen, Remakes, GOTY-Editionen usw. Eine Zeile pro Spielfamilie. |
| Spiele | Ein Spiel ist ein vollständiger (oder unvollständiger) Satz Roms für ein Spiel. Besitzen Sie mehrere Kopien, zählt jede als ein Spiel. Ein Spiel kann mehrere Roms enthalten (Disc 1, Disc 2, ...). |
| Roms | Eine Rom ist eine einzelne Einheit eines Spielmediums. Getrennte Discs zählen als getrennte Roms. Eine Rom kann mehrere Dateien enthalten (Disc 1 = cue + track 1.bin + track 2.bin). |
| Dateien | Die einzelnen Dateien auf dem Datenträger. |
| Inhalte | Ein Inhalt ist eine Bytefolge, erkannt an ihrem Hash. Das kann eine Datei sein oder ein einzelner Eintrag in einem Archiv. Ein Inhalt kann auch Inhalte enthalten. |
| Sets | Ein Set ist Ihre eigene geordnete Liste von Titeln / Releases / Crossplats / Defs. |
| Multimedia, Dokumente, Quellcode, Sonstiges | Video- und Musikdiscs; Handbücher, Magazine und Scans; Quellcode; Tools, Trainer, Spielstände. |
| Patches | RetroAchievements-Kompatibilitätspatches (mit Download), in DATs gelistete Patches, Ihre Patch-Ordner. |
| BIOS / Firmware | Die Systemdateien des Geräts: BIOS, Firmware, NAND- und SD-Karten-Images. |
| Audio | Ihre Audiodateien im audio-Verzeichnis. Sie (und Titel) lassen sich taggen, sodass ein Soundtrack läuft, während Sie die Titelseite ansehen. |

\* Dasselbe Spiel auf einer anderen Plattform (Hoge Quest (PS2), Hoge Quest (Dreamcast)) zählt
als eigener Titel. Unterschiedliche Revisionen (Hoge Quest, Hoge Quest (Rev A)), Regionen
(Hoge Quest (Japan), Adventure of Hoge (USA)) oder Sprachen (Hoge Quest (ja), Hoge Quest (fr))
werden im selben Titel gebündelt.

\*\* Werden beim Kopieren mehrere Kopien desselben Releases gefunden, gewinnt die vollständige
(keine fehlenden Dateien), danach die in den Einstellungen festgelegte Priorität (mit Header >
ohne Header, z64 > n64 usw.). Beim Kopieren wird ein einziges Release vom Server oder vom
Client kopiert, gewählt nach Ihren Prioritäten für Region / Sprache / Edition (normal, Budget,
Sonderausgabe). Releases, die Sie nicht besitzen, werden zugunsten der nächsten Priorität
übersprungen.

\*\*\* Hoge Quest umfasst Hoge Quest (PS2) und Hoge Quest (Dreamcast).

\*\*\*\* Für den Fall, dass Sie nur die neueste Version spielen wollen, oder die mit den meisten
spielbaren Inhalten. Standardmäßig wählt ein Def den neuesten Crossplat, aber manche Defs tragen
eingestufte Hinweise für Fälle, in denen dem neuesten Crossplat Inhalte früherer Versionen
fehlen (z. B. fehlt Hoge Quest 3D Remake (PC) der Extra-Dungeon von Hoge Quest (GBA)). Sie
können die Versionen in die gewünschte Reihenfolge ziehen. Da es die definitive Version
womöglich nur auf aktuellen Plattformen gibt, können Sie einstellen, dass der Crossplat / Titel
mit der höchsten Priorität in Ihrer Sammlung kopiert wird, oder gar nichts, wenn die
definitivste Version nicht in Ihrer Sammlung ist (sprich: Spielen Sie sie auf Steam).

Hinweis: Crossplats und Defs werden aus IGDB-Metadaten aufgebaut, diese müssen also
heruntergeladen sein. Eine Einstellung kann die zugehörigen Daten automatisch herunterladen
(standardmäßig aus, weil die große Datenbank alles verlangsamt).

## ROM-Audio

Titelseiten können die Musik in Ihren ROMs direkt aus der Datei abspielen: Nichts wird entpackt
oder kopiert. Führen Sie zuerst „ROM-Audio indizieren“ im Audio-Tab aus. `rommusic` liest dasselbe.

| Musik aus | Plattformen | Dateien |
|---|---|---|
| CD-Audiotracks | Jedes CD-Spiel: PC Engine CD, Mega-CD, Saturn, PlayStation, Dreamcast und andere | CHD, cue/bin, gdi |
| Musikdateien auf der Disc | PlayStation, PlayStation 2, PSP, Saturn, Dreamcast, GameCube, Wii, Xbox, Xbox 360 | CHD, cue/bin, gdi, ISO, CSO/ZSO, RVZ/WIA/GCZ/WBFS, XISO |
| | Wii U | .wua; .wud/.wux und heruntergeladene Titel mit ihrem Schlüssel daneben |
| | PS3, PS Vita | entschlüsselte Ordner-Dumps |
| Das Soundarchiv | Nintendo DS (gestreamte und sequenzierte Musik), Nintendo 3DS (nur entschlüsselte Images) | .nds, .3ds/.cci |
| Die Sound-Engine des Spiels | Game Boy Advance: Spiele mit Nintendos Standard-Engine (etwa die Hälfte) | .gba |
| Original-Audioformate | NES und Famicom Disk System (NSF, NSFE), Super Nintendo (SPC), Game Boy (GBS), PC Engine (HES), MSX (KSS), Master System und Game Gear (SGC, VGM), Mega Drive (VGM, GYM), ZX Spectrum und Amstrad CPC (AY), Atari 8-Bit (SAP), Nintendo 64 (USF), PlayStation (PSF), PlayStation 2 (PSF2), Saturn (SSF), Dreamcast (DSF), Game Boy Advance (GSF), WonderSwan (WSR), Neo Geo Pocket und Virtual Boy (VGM) | im Ordner des Spiels oder in der Original-Audio-Bibliothek, einzeln oder gezippt |

- Die Original-Audio-Bibliothek (Einstellungen › Audio) ordnet NSF-, NSFE-, GBS-, SPC-, HES-, KSS- und
  SGC-Sets anhand des Inhalts Ihren ROMs zu: Ein Set spielt für jede Region eines Spiels, egal wie
  es heißt.
- PlayStation- und PlayStation-2-Dateien brauchen ein PS2-BIOS in Ihrer Sammlung; ohne werden sie
  übersprungen.
- Disc-Images in zip oder 7z werden noch nicht gelesen. Verschlüsselte Images werden nicht gelesen.

## Tab RetroAchievements

Ihr RetroAchievements-Konto und die Spiele mit Erfolgen, Stand der letzten Kontosynchronisierung
(braucht `ra_username` und `ra_api_key`). Der Kontobereich oben zeigt Ihr Profil und Ihren
Fortschritt. Die Seite eines Spiels listet seine Erfolge und seine Kompatibilitätspatches, mit
einem Download-Knopf.

Wenn Sie einen Titel aus dem RA-Tab kopieren oder zu einem Set hinzufügen, werden **nur**
Releases mit Erfolgen gewählt. (Beispiel: Hat Hoge Quest (USA) Erfolge und Hoge Quest (Japan)
nicht, wird Hoge Quest (USA) kopiert, auch wenn Ihre Regionspriorität Japan > USA ist.)

| Unter-Tab | zeigt |
|---|---|
| Titel | Titel mit RetroAchievements-Erfolgen. |
| Gemeistert | Titel, bei denen Sie im Hardcore-Modus alle Erfolge freigeschaltet haben. |
| Abgeschlossen | Titel, bei denen Sie im Softcore-Modus alle Erfolge freigeschaltet haben. |
| Durchgespielt | Spiele, die Sie durchgespielt, aber nicht abgeschlossen haben. |
| In Arbeit | Begonnen, aber weder gemeistert noch abgeschlossen (durchgespielte auch, außer die Einstellung `ra_include_beaten` ist aus). |
| Noch zu spielen | Titel mit Erfolgen, die Sie noch nicht begonnen haben. |

## Code von Drittanbietern

romexplorer baut auf diesen Projekten auf. Die meisten werden als Bibliotheken verwendet und sind
als Submodule unter `external/` fest eingebunden. „Gepatcht“ bedeutet, dass beim Build einige
Zeilen im Code des Projekts selbst geändert werden; „Wrapper“ bedeutet, dass wir eine eigene kleine
Bibliothek darum herum ergänzen, ohne seinen Code zu ändern. Beides liegt in `deps/<name>/`, nie im
Submodul. Vielen Dank an alle.

| Projekt | verwendet für | unsere Änderungen |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | gepatcht, Wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | CHD erstellen / entpacken | gepatcht, Wrapper |
| [MAME](https://github.com/mamedev/mame) | der Quellcode, aus dem chdman-simd gebaut wird | keine |
| [libchdr](https://github.com/rtissera/libchdr) | sein LZMA-Decoder; Referenz für den CHD-Reader | keine |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | Wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | Wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | Schreiben von .wua / .zar | keine |
| [zstd](https://github.com/facebook/zstd) | Kompression | keine |
| [fmt](https://github.com/fmtlib/fmt) | von Cemus Code verwendet | keine |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | Wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | Wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | RetroAchievements-Hashes | gepatcht, Wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | die mitgelieferte Datenbank | Build-Datei gepatcht |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | Datenbank-Client | keine |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | keine |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | keine |
| [libxaac](https://github.com/ittiam-systems/libxaac) | AAC-Audio | Wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | Anbieter-Clients; sein Namensabgleich portiert | Code übernommen und angepasst |
| [RomM](https://github.com/rommapp/romm) | Referenz, Plattform-Icons | nicht als Code verwendet |
| [RAChecker](https://github.com/x3kim/RAChecker) | Referenz für den RetroAchievements-Client | nicht als Code verwendet |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | PS1-/PS2-Cover, nach Seriennummer | bei Bedarf heruntergeladen, nicht mitgeliefert |

Build-Details, Versions-Pins und Patches stehen in [deps/README.md](deps/README.md). Die Lizenz
jedes Projekts liegt den Binärdateien in `lib/LICENSES/` bei.
