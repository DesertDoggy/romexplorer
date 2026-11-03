# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · **Italiano** · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Questa è una traduzione. In caso di differenze, fa fede il README in inglese.*

Un gestore di collezioni di ROM: scansiona le cartelle, identifica ogni file tramite hash
confrontandolo con i DAT di riferimento, recupera metadati e immagini, e converte, verifica e copia
i file. Funziona come server con una console e un'interfaccia grafica web nel browser, oppure come
applicazione desktop, su Windows, macOS e Linux. Consultare
[docs/release_notes.md](docs/release_notes.md) per sapere che cosa è stato testato e dove.

## Che cosa fa

- **Scansione e identificazione.** Percorre una directory, rileva la piattaforma di ogni file dal suo
  contenuto, ne calcola gli hash (CRC32, MD5, SHA-1, SHA-256, XXH3, RetroAchievements) e lo
  confronta con il catalogo, prima per hash e poi per nome.
- **Lettura dentro i contenitori.** CHD, RVZ/WIA/GCZ, zip e 7z vengono decodificati come flussi:
  così di un'immagine disco si calcolano gli hash e la si identifica senza doverla prima estrarre.
- **Conversione e ricompressione.** Creazione ed estrazione di CHD (CD, GD-ROM, DVD) e RVZ; zip e
  7z; aggiunta o rimozione delle intestazioni NES/SNES; ricostruzione di un `.cue`, `.gdi` o `.m3u`
  mancante.
- **Catalogo.** Importa i DAT di Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements e
  RetroBIOS in un MariaDB incluso, e unisce le uscite regionali e i titoli.
- **Metadati e immagini.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady e RetroAchievements, ciascuno con il proprio limite di richieste.
- **Copia su un dispositivo.** Copia i giochi selezionati in una cartella o su una scheda,
  convertendoli durante la copia, con i nomi e la struttura che un frontend si aspetta.
- **Verifica e backup.** Controlli di integrità rispetto agli hash registrati; backup zstd del
  database e del catalogo; ripristino.
- **Condivisione tra macchine.** Il catalogo e le immagini di un server principale possono essere
  replicati da client e peer; un'esportazione SQLite alimenta uno smartphone.

Tutto è accessibile tramite l'API REST (`/api/v1/...`), la console nel browser (`/`) e
l'interfaccia grafica web (`/app/`).

## Configurazione iniziale

Esistono tre pacchetti. Sceglierne uno:

| pacchetto | che cos'è | installazione |
|---|---|---|
| **server** | server, worker, MariaDB incluso, interfaccia grafica web | eseguire l'installer |
| **desktop** | applicazione grafica con il proprio MariaDB | basta avviarla |
| **cli** | chdlite: strumenti autonomi per conversione / estrazione / hash, senza database | basta eseguirlo |

### Server

1. Estrarre il pacchetto.
2. Facoltativo: copiare `romexplorer.conf.example` in `romexplorer.conf` e modificarlo. Ogni
   valore è facoltativo; se la password è vuota, ne viene generata una. Questo file viene letto
   solo alla prima installazione.
3. Eseguire l'installer:
   - Linux / macOS: `./install.sh` -- da root installa in `/opt/romexplorer` come servizio di
     sistema; da utente normale, in `~/.local/opt/romexplorer` come servizio utente.
   - Windows: `.\install.ps1`, da un PowerShell avviato come amministratore.
4. Al primo avvio vengono creati il database, i suoi account e l'accesso amministratore, poi le
   credenziali della console vengono mostrate **una sola volta**. Conservarle.
5. Aprire `http://127.0.0.1:7337/` e accedere.

Valori predefiniti: il server è in ascolto su `127.0.0.1:7337` e il MariaDB incluso su `3307`.
Impostare `server_bind=0.0.0.0` per servire la rete locale; le altre macchine vengono allora
servite tramite TLS, con un certificato che il server crea al primo avvio.

In seguito:

- Cambiare la password dell'amministratore nella console o con `romexplorer_server reset-admin`.
- Controllare un'installazione con `romexplorer_dbtool check`: elenca le librerie native e il
  database trovati.
- Disinstallare con `./install.sh --uninstall` (i dati restano) o `--uninstall --purge`.

### Aggiornare il server

Eseguire l'installer della nuova versione sopra l'installazione esistente. Rileva il database, ferma
il servizio, sostituisce gli eseguibili, `lib/` e le risorse incluse, quindi lo riavvia. Database,
impostazioni, credenziali, contenuti multimediali e tutto il resto in `data/` e `assets/` vengono
conservati.

- Linux / macOS, direttamente dall'ultima versione:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- Oppure estrarre il nuovo pacchetto ed eseguire il suo `./install.sh`, o indicarlo a quello già
  installato:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: estrarre il nuovo pacchetto ed eseguire il suo `.\install.ps1` da un PowerShell avviato
  come amministratore, oppure `.\install.ps1 -From <package>.zip`.

Eseguirlo con lo stesso utente della prima installazione (root o no) e passare lo stesso
`--prefix` / `-Prefix`, se ne era stato scelto uno. Finché romexplorer è in fase alpha, lo schema
del database può cambiare da una versione all'altra: fare prima un backup (Configurazione › Backup).

### Desktop

Estrarre ed eseguire `romexplorer` (su macOS, `romexplorer.app`; non è notarizzata, quindi al primo
avvio serve clic destro > Apri). Il primo avvio crea e avvia il proprio database.

### Strumenti da riga di comando (chdlite)

Si trascina un'immagine disco e si ottiene un CHD o un RVZ. `chdlite` (veloce), `chdcomp` (il più
compatto), `chdread` (estrazione) e `chdhash` (hash) sono uno strumento per modalità, così un file
trascinato sull'eseguibile fa la cosa giusta; `chdlite-gui` fa lo stesso in una finestra. Ogni
opzione è disponibile anche come flag (`--help`). chdlite viene pubblicato separatamente: vedere
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Chiavi dei provider

Facoltative. Inserire le chiavi API in Configurazione › Credenziali dei provider come
amministratore. Vengono salvate cifrate sul server e non vengono più mostrate; un provider senza
chiavi semplicemente non è disponibile.

### Primo utilizzo

1. Aggiungere una directory e indicare quale piattaforma contiene, oppure lasciar decidere al
   rilevamento.
2. Eseguire una sincronizzazione DAT, così da avere un catalogo con cui confrontare.
3. Eseguire una scansione dei file sulla directory.
4. Eseguire una corrispondenza hash, poi le interrogazioni ai provider per metadati e immagini.

Ognuna di queste operazioni è un'attività; vedere sotto.

### Dai sorgenti

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` segnala gli strumenti mancanti senza compilare. Su macOS servono
`bash`, `bison` e `ninja` di Homebrew.

## Attività

Tutto ciò che è lento viene eseguito come **attività** in background, quindi continua anche senza
un browser aperto. Ogni attività mostra il proprio avanzamento, può essere annullata e può essere
pianificata (scheda Attività pianificate) per ripetersi ogni tot minuti, ore, giorni, settimane o mesi.
Le principali:

- **Sincronizzazione DAT** -- scarica e importa i DAT di riferimento (Redump, No-Intro, TOSEC, ...).
- **Download di metadati e media dai provider** -- recupera informazioni sui giochi e immagini da
  IGDB, ScreenScraper, SteamGridDB e gli altri.
- **Scansione dei file** -- percorre una directory, calcola gli hash dei file e li confronta con il
  catalogo.
- **Controllo di integrità** -- rilegge i file posseduti e li confronta con gli hash registrati.
- **Controllo e creazione di fogli cue / playlist** -- verifica che i file `.cue`, `.gdi` e `.m3u`
  puntino ancora a file validi e ricostruisce quelli mancanti.

## Codice di terze parti

romexplorer si basa su questi progetti. La maggior parte è usata come libreria, fissata come
sottomodulo in `external/`. «Con patch» significa che alcune righe del codice del progetto vengono
modificate in fase di compilazione; «wrapper» significa che aggiungiamo intorno una nostra piccola
libreria senza modificarne il codice. Entrambi si trovano in `deps/<name>/`, mai nel sottomodulo.
Grazie a tutti.

| progetto | usato per | nostre modifiche |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | con patch, wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | creazione / estrazione di CHD | con patch, wrapper |
| [MAME](https://github.com/mamedev/mame) | i sorgenti da cui viene compilato chdman-simd | nessuna |
| [libchdr](https://github.com/rtissera/libchdr) | il suo decoder LZMA; riferimento per il lettore CHD | nessuna |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | scrittura di .wua / .zar | nessuna |
| [zstd](https://github.com/facebook/zstd) | compressione | nessuna |
| [fmt](https://github.com/fmtlib/fmt) | usato dal codice di Cemu | nessuna |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | hash RetroAchievements | con patch, wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | il database incluso | file di build con patch |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | client del database | nessuna |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | nessuna |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | nessuna |
| [libxaac](https://github.com/ittiam-systems/libxaac) | audio AAC | wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | client dei provider; portato il suo confronto per nome | codice copiato e modificato |
| [RomM](https://github.com/rommapp/romm) | riferimento, icone delle piattaforme | non usato come codice |
| [RAChecker](https://github.com/x3kim/RAChecker) | riferimento per il client RetroAchievements | non usato come codice |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | copertine PS1 / PS2, per numero di serie | scaricate quando servono, non incluse |

Dettagli di compilazione, versioni fissate e patch si trovano in [deps/README.md](deps/README.md).
La licenza di ogni progetto è distribuita con i binari in `lib/LICENSES/`.
