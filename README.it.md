# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · **Italiano** · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

*Questa è una traduzione. In caso di differenze, fa fede il README in inglese.*

> **romexplorer non distribuisce né scarica ROM, BIOS o chiavi, e non sostiene la
> pirateria.** Usalo con le tue copie dei giochi che possiedi.

Un gestore di collezioni di ROM: scansiona le cartelle, identifica ogni file tramite hash
confrontandolo con i DAT di riferimento, recupera metadati e immagini, e converte, verifica e copia
i file. Funziona come server con una console e un'interfaccia grafica web nel browser, oppure come
applicazione desktop, su Windows, macOS e Linux. Consultare
[docs/release_notes.it.md](docs/release_notes.it.md) per sapere che cosa è stato testato e dove.

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
- **Copia su un dispositivo.** Copia i giochi e le loro immagini dal server o da questa macchina
  in una cartella o su una scheda, convertendoli durante la copia, con i nomi e la struttura che
  un frontend si aspetta; le immagini sono organizzate per RetroArch o PCSX2 / DuckStation.
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

Si trascina un'immagine disco e si ottiene un CHD o un RVZ. `chdlite` (veloce) e `chdcomp` (il più
compatto) funzionano in entrambi i sensi: un'immagine non compressa viene compressa, un `.chd` o un
`.rvz` viene estratto. `chdread` mostra i metadati di un file e `chdhash` ne calcola l'hash. Ogni
strumento fa una sola cosa, così un file trascinato sull'eseguibile fa la cosa giusta;
`chdlite-gui` fa lo stesso in una finestra. Ogni opzione è disponibile anche come flag (`--help`).
chdlite viene pubblicato separatamente: vedere
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Strumento da riga di comando (rommusic)

Si trascina su `rommusic` una ROM, un'immagine disco o una cartella estratta
e scrive la musica di sottofondo del gioco in FLAC in una cartella accanto. Ogni opzione è
disponibile anche come flag (`--help`). I sistemi supportati sono elencati in
[Audio delle ROM](#audio-delle-rom). rommusic è rilasciato separatamente: vedi
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md).

### Chiavi dei provider

Facoltative. Inserire le chiavi API in Configurazione › Credenziali dei provider come
amministratore. Un provider senza chiavi semplicemente non è disponibile.

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

## Scansione e corrispondenza per hash

La **scansione dei file** percorre una cartella e calcola l'hash di ogni file (CRC32, MD5,
SHA-1, SHA-256, XXH3 e l'hash RA). Gli archivi (CHD, RVZ/WIA/GCZ, zip, 7z) vengono letti
dall'interno. Al termine della scansione viene messa in coda una corrispondenza per hash della
stessa cartella con le caselle che hai spuntato. Con tutte le caselle disattivate non viene
eseguita alcuna corrispondenza.

| casella | cosa fa |
|---|---|
| Corrispondenza DAT | Collega ogni gioco alla release DAT i cui hash corrispondono ai suoi file (un CHD anche tramite le sue tracce), poi tramite numero di serie per ciò che nessun hash ha trovato. Offline. |
| RetroAchievements | Collega i giochi ai giochi di RetroAchievements tramite il loro hash RA, dai dati RA già sincronizzati. Offline, sempre attiva. |
| Hasheous | Cerca ogni file per hash su hasheous.org (senza chiave). Restituisce la release e i suoi id presso gli altri provider. |
| PlayMatch | Cerca i file che ancora non hanno un id IGDB (senza chiave), 100 per richiesta. Restituisce lo stesso tipo di id. |

Hasheous e PlayMatch vanno online solo quando i dati non sono nella cache locale.

**Provider a cui portano Hasheous e PlayMatch:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper e MobyGames (dati e immagini, tramite gli id trovati), più una
ricerca per nome su IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady e ScreenScraper per i
titoli ancora senza collegamento. I provider senza chiavi vengono saltati.

## Copia

**Copia i giochi e le loro illustrazioni in un'altra destinazione.** In modalità client desktop
puoi copiare anche dal server principale: aggiungi la cartella montata in rete (SMB, NFS, ...) e
la radice della collezione del server nelle impostazioni del client.

**Puoi copiare un set, oppure singoli titoli, release, Crossplat e Def.**

Modalità di copia (si sceglie al momento della copia):

| modalità | per | regole |
|---|---|---|
| In questa collezione | server → una cartella della collezione su questa macchina | salta ciò che questa macchina possiede già; le copie contano subito come possedute qui |
| Su scheda SD / USB / cartella del telefono | qualsiasi cartella fuori dalla collezione | nulla viene saltato perché posseduto altrove; nulla viene registrato |
| All'interno di questa collezione | conversione sul posto (ISO → RVZ, ...) | mantiene entrambe le copie |
| All'interno della collezione del server | dall'interfaccia del server stesso | ogni file verificato; non sovrascrive mai |
| Caricamento | questa macchina → il server | solo ciò che manca al server; non sovrascrive mai; il server lo riscansiona |

- Le copie vanno in una sottocartella per piattaforma. Lo stile del nome delle sottocartelle si
  sceglie al momento della copia.
- I file possono essere convertiti durante la copia, per archiviarli e risparmiare spazio, o in
  un altro formato richiesto da un dispositivo.
- Se il file esiste già nella destinazione, viene saltato.

**Anche le illustrazioni possono essere esportate.** Spunta «esporta immagini dopo» nella
finestra di copia, oppure usa **esporta immagini...** da solo. Le immagini vengono rinominate e
organizzate per gli emulatori:

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, la struttura di libretro-thumbnails.
- **PCSX2 / DuckStation:** un'unica cartella covers, `<serial>.<ext>`, oppure con il nome del
  file del gioco.

L'immagine di quale provider riempie ogni slot segue `media_export_priority` (prima le vere
scansioni delle scatole).

## Scheda Collezione

La collezione è il database di tutti i giochi che possiedi, e degli altri giochi presenti in
database come Redump, No-Intro, TOSEC e IGDB (richiede il download dei DAT / dei metadati).

| sotto-scheda | una riga per |
|---|---|
| Titoli\* | Un Titolo è un singolo gioco su una singola piattaforma. |
| Release\*\* | Una Release è un singolo gioco in cui ogni regione, lingua, revisione, edizione, demo, ecc. conta separatamente. |
| Crossplat\*\*\* | Un Crossplat riunisce tutti i Titoli dello stesso gioco su piattaforme diverse. |
| Def\*\*\*\* | Un Def serve a scegliere una versione di un gioco fra tutte le sue versioni (una famiglia di giochi): l'originale più i suoi remaster, port, remake, edizioni GOTY, ecc. Una riga per famiglia di giochi. |
| Giochi | Un Gioco è un set completo (o incompleto) di rom per un gioco. Se ne possiedi più copie, ognuna conta come un Gioco. Un gioco può contenere più rom (Disc 1, Disc 2, ...). |
| Rom | Una Rom è una singola unità di supporto del gioco. Dischi separati contano come rom separate. Una rom può contenere più file (Disc 1 = cue + track 1.bin + track 2.bin). |
| File | I singoli file sul disco. |
| Contenuti | Un Contenuto è una sequenza di byte, identificata dal suo hash. Può essere un file, oppure una singola voce dentro un archivio. Un contenuto può anche contenere altri contenuti. |
| Set | Un Set è la tua lista ordinata di Titoli / Release / Crossplat / Def. |
| Multimedia, Documenti, Codice sorgente, Altro | Dischi video e musicali; manuali, riviste e scansioni; codice sorgente; strumenti, trainer, salvataggi. |
| Patch | Patch di compatibilità RetroAchievements (con download), patch elencate nei DAT, le tue cartelle di patch. |
| BIOS / firmware | I file di sistema della macchina: BIOS, firmware, immagini NAND e di schede SD. |
| Audio | I tuoi file audio nella cartella audio. Si possono etichettare (come i titoli), così una colonna sonora suona mentre guardi la pagina del Titolo. |

\* Lo stesso gioco su un'altra piattaforma (Hoge Quest (PS2), Hoge Quest (Dreamcast)) conta
come un Titolo separato. Revisioni (Hoge Quest, Hoge Quest (Rev A)), regioni
(Hoge Quest (Japan), Adventure of Hoge (USA)) o lingue (Hoge Quest (ja), Hoge Quest (fr))
diverse sono raggruppate nello stesso Titolo.

\*\* Nella copia, se si trovano più copie della stessa release, vince quella completa (nessun
file mancante), poi la priorità impostata nelle impostazioni (con header > senza header,
z64 > n64, ecc.). Nella copia viene copiata una sola release dal server o dal client, scelta in
base alle tue priorità di regione / lingua / edizione (normale, economica, speciale). Le release
che non possiedi vengono saltate a favore della priorità successiva.

\*\*\* Hoge Quest include Hoge Quest (PS2) e Hoge Quest (Dreamcast).

\*\*\*\* Per quando vuoi giocare solo l'ultima versione, o quella con più contenuti giocabili.
Per impostazione predefinita un Def sceglie il Crossplat più recente, ma alcuni Def hanno note
classificate per i casi in cui l'ultimo Crossplat manca di contenuti delle versioni precedenti
(es. a Hoge Quest 3D Remake (PC) manca il dungeon extra di Hoge Quest (GBA)). Puoi trascinare le
versioni nell'ordine che preferisci. Poiché la versione definitiva può esistere solo sulle
piattaforme attuali, puoi scegliere di copiare il Crossplat / Titolo con la priorità più alta
della tua collezione, oppure di non copiare nulla quando la versione più definitiva non è nella
tua collezione (cioè: vai a giocarla su Steam).

Nota: Crossplat e Def sono costruiti dai metadati IGDB, che quindi vanno scaricati.
Un'impostazione può scaricare automaticamente i dati correlati (disattivata per impostazione
predefinita, perché il database grande rallenta tutto).

## Audio delle ROM

Le pagine dei titoli possono riprodurre la musica contenuta nelle tue ROM, direttamente dal file:
non viene estratto né copiato nulla. Esegui prima «Indicizza l'audio delle ROM» nella scheda Audio. `rommusic`
legge le stesse cose. Per ora la musica di una ROM si sente solo dopo che la ROM è stata indicizzata;
nel frattempo la sua pagina può riprodurre la musica di un altro gioco.

| musica da | piattaforme | file |
|---|---|---|
| Tracce audio CD | Qualsiasi gioco su CD: PC Engine CD, Mega-CD, Saturn, PlayStation, Dreamcast e altri | CHD, cue/bin, gdi |
| L'archivio sonoro | Nintendo DS (musica in streaming e sequenziata), Nintendo 3DS (solo immagini decifrate) | .nds, .3ds/.cci |

- Le immagini disco dentro zip o 7z non vengono ancora lette. Le immagini cifrate non vengono lette.
- La musica dei giochi su cartuccia e dei file musicali su disco è ancora in lavorazione.

## Scheda RetroAchievements

Il tuo account RetroAchievements e i giochi con obiettivi, secondo l'ultima sincronizzazione
dell'account (servono `ra_username` e `ra_api_key`). Il riquadro dell'account in alto mostra il
tuo profilo e i tuoi progressi. La pagina di un gioco elenca i suoi obiettivi e le sue patch di
compatibilità, con un pulsante di download.

Se copi un Titolo o lo aggiungi a un Set dalla scheda RA, vengono scelte **solo** le release che
hanno obiettivi. (Es.: se Hoge Quest (USA) ha obiettivi e Hoge Quest (Japan) no, viene copiato
Hoge Quest (USA) anche se la tua priorità di regione è Japan > USA.)

| sotto-scheda | mostra |
|---|---|
| Titoli | Titoli con obiettivi RetroAchievements. |
| Maestria | Titoli in cui hai sbloccato tutti gli obiettivi in modalità hardcore. |
| Completati | Titoli in cui hai sbloccato tutti gli obiettivi in modalità softcore. |
| Battuti | Giochi che hai battuto ma non completato. |
| In corso | Iniziati ma non padroneggiati né completati (anche quelli battuti, a meno che l'impostazione `ra_include_beaten` sia disattivata). |
| Da giocare | Titoli con obiettivi che non hai ancora iniziato. |

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
