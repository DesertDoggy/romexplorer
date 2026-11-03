# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · **Deutsch** · [한국어](README.ko.md) · [Polski](README.pl.md)

*Dies ist eine Übersetzung. Bei Abweichungen gilt die englische README.*

Ein Verwaltungsprogramm für ROM-Sammlungen: Es scannt Ihre Ordner, identifiziert jede Datei per
Hash anhand von Referenz-DATs, lädt Metadaten und Artwork und konvertiert, prüft und kopiert die
Dateien. Es läuft als Server mit Browser-Konsole und Web-GUI oder als Desktop-App, unter Windows,
macOS und Linux. Was wo getestet ist, steht in [docs/release_notes.md](docs/release_notes.md).

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
- **Auf ein Gerät kopieren.** Kopiert ausgewählte Spiele in einen Ordner oder auf eine
  Speicherkarte, konvertiert sie dabei und verwendet die Namen und die Ordnerstruktur, die ein
  Frontend erwartet.
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

Disc-Image darauf ziehen, CHD oder RVZ erhalten. `chdlite` (schnell), `chdcomp` (kleinste Datei),
`chdread` (entpacken) und `chdhash` (Hash) sind je ein Werkzeug pro Modus, sodass eine auf die
ausführbare Datei gezogene Datei das Richtige bewirkt; `chdlite-gui` bietet dasselbe in einem
Fenster. Jede Option gibt es auch als Kommandozeilenparameter (`--help`). chdlite erscheint
als eigenes Release: siehe [docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Anbieter-Schlüssel

Optional. Geben Sie API-Schlüssel als Administrator unter Konfiguration › Anbieter-Zugangsdaten
ein. Sie werden verschlüsselt auf dem Server gespeichert und nie wieder angezeigt; ein Anbieter
ohne Schlüssel ist einfach nicht verfügbar.

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
