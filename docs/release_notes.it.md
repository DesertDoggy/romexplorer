*Questa è una traduzione delle note di rilascio 0.2.1, 0.2.0 e 0.1.8. Le versioni precedenti sono in
[release_notes.md](release_notes.md) (inglese), che fa fede in caso di differenze.*

# romexplorer 0.3.0 (alpha)

Alpha: lo schema del database può cambiare tra una versione e l'altra senza possibilità di
aggiornamento. Fai un backup prima di sostituire una build.

## Modifiche

**Schermate**
- Gli elenchi con molti filtri ne mostrano quattro; gli altri sono in «Altri filtri», e quelli in uso compaiono come etichette da togliere.
- La pagina dei processi mostra prima l'elenco dei processi. I nuovi si avviano da «Nuovo processo».
- Le sottoschede della collezione per i file locali e per il materiale non di gioco sono ora due menu a tendina.
- Le impostazioni mostrano un nome nella lingua dell'interfaccia, con la chiave sotto. Salva e ripristina compaiono solo quando cambiano qualcosa, e le impostazioni di un'altra schermata restano lì.
- Le pagine di titolo e di release mostrano come pulsanti solo le azioni principali. I link ai titoli correlati sono nella riga del gioco base e gli id interni in «Dettagli». L’abbinamento manuale mostra prima i link attuali.
- Big Picture: ricerca e filtro sono nella barra in alto, lingua e disposizione nelle impostazioni. Un gioco con più dischi ha una sola voce «Dischi…» nel menu. Le impostazioni dell’app per telefono si aprono una parte alla volta.

**Scansione**
- Aggiunta «Scansione rapida (modificati)»: legge anche i file con dimensione o data cambiate.
- Gli hash della scansione si impostano separatamente per file normali, file d'archivio e contenuto degli archivi. I file d'archivio usano sha1 e xxh3_128 come predefinito.

**Copia**
- Una copia riavviata controlla più file già presenti nella destinazione insieme.

**Elenchi**
- Le viste a schede con molte voci si aprono e scorrono più velocemente.
- Maiusc+clic seleziona un intervallo, come su Windows e macOS. Un clic accanto a una casella la spunta invece di aprire la voce.
- Selezione multipla in altri elenchi: titoli uniti, job della schermata Scansione, fonti DAT, file ROM, BIOS, ROM patchate, elenchi di materiale, versioni definitive e dispositivi connessi.

**Avanzamento**
- Ogni processo mostra cosa sta facendo, il totale e a che punto è, anche durante l'esplorazione delle cartelle, i controlli e i passaggi del database.
- I processi in più passaggi mostrano l'elenco dei passaggi.
- I processi hanno il pulsante «dettagli».
- I processi in corso mostrano il tempo trascorso e quello rimanente.
- I download grandi e i controlli dei file mostrano quanto è stato letto.
- Svuotare il catalogo e importare un file di raccolte vengono eseguiti come processi.
- L'app desktop mostra cosa sta facendo durante l'avvio.

**Audio**
- Più brani possono essere convertiti insieme per la riproduzione.

## Correzioni

- Parte del testo dell'app per telefono era mostrata in inglese nelle altre lingue.
- L'avanzamento della scansione audio si fermava durante la misura del volume.
- L'avanzamento dell'indice musicale delle ROM si muoveva solo ogni 500 ROM.

# romexplorer 0.2.2 (alpha)

Alpha: lo schema del database può cambiare tra una versione e l'altra senza possibilità di
aggiornamento. Fai un backup prima di sostituire una build.

## Modifiche

**Audio delle ROM**
- L'indice della musica delle ROM può leggere più ROM insieme (4 come predefinito, modificabile nelle impostazioni audio).
- L'indice della musica delle ROM legge solo la copia meglio classificata di ogni titolo, e le altre solo se quella non ha musica.
- Nei giochi su più dischi, i file musicali già presenti su un disco precedente non vengono riletti.
- I giochi su cartuccia di cui l'app conosce già la musica la riproducono appena si apre la pagina del titolo, senza attendere l'indice.
- Aggiunto «Cancella l'indice audio delle ROM» (tutte o per piattaforma).

# romexplorer 0.2.1 (alpha)

Alpha: lo schema del database può cambiare tra una versione e l'altra senza possibilità di
aggiornamento. Fai un backup prima di sostituire una build.

## Modifiche

**3DS**
- Le ROM 3DS possono essere convertite in altri formati.
- Le chiavi non sono incluse. Metti aes_keys.txt (e seeddb.bin se un titolo lo richiede) accanto ai
  file, accanto a romexplorer o nella sua cartella assets.
- Corrispondenza per seriale per le ROM 3DS.

**Audio delle ROM**
- Aprendo la pagina di un titolo ora viene indicizzata per prima la musica delle ROM di quel gioco.
- L'indice della musica delle ROM ora legge prima le ROM più piccole e salta quelle non supportate.
- Sono supportati i giochi PS1 con un motore sonoro proprio.

## Correzioni

- L'indice della musica delle ROM sembrava fermo mentre una ROM richiedeva molto tempo.
- L'indice della musica delle ROM saltava le ROM identificate solo tramite un DAT.
- Una scansione lasciava senza piattaforma le ROM giapponesi del Super Famicom con il titolo interno in katakana.
- La pagina di un titolo ripeteva la stessa musica per ogni copia del gioco trovata.
- L'indicizzazione della musica delle ROM non finiva mai su alcune immagini disco.
- Convertendo dump SNES in una copia, un file con dimensione e header interno in disaccordo
  sull'header del copiatore veniva convertito comunque.

**Scansione**
- Su Linux una scansione ignorava le voci cue, gdi e m3u scritte con maiuscole diverse o con «\»
  come separatore, e prendeva due file che differiscono solo per le maiuscole per una sola ROM.
- La scansione di una cartella ignorava tipo e piattaforma impostati sulle sue sottocartelle.
- Le riscansioni rileggevano i giochi in cartella, separavano le tracce e perdevano le
  corrispondenze per seriale. Raggruppare i dischi in cartelle grandi è più veloce.
- I file che non sono ROM (testo, immagini) ora diventano giochi solo dopo essere identificati.
- Ora uno tra crc32, md5 e sha1 deve restare attivo negli hash della scansione. Con solo xxh3_128 si
  perdevano le corrispondenze DAT.

**Copia**
- Sovrascrivendo si cancellava il vecchio file prima di scrivere il nuovo, una copia riscriveva
  playlist da conservare e due file che differiscono solo per le maiuscole venivano scritti in uno.

**Unione dei titoli**
- L'unione univa versioni di un set di versioni, o titoli commerciali con titoli senza licenza.
  «Accetta tutto» ripristinava unioni annullate e tornavano link IGDB rimossi dall'audit.
- Una corrispondenza Hasheous sull'altro nome di un gioco unito creava un titolo doppio.

**Server e processi**
- Il recupero di immagini per un client poteva raggiungere la rete interna del server. È stato
  aggiunto anche un limite di dimensione.
- Scaricare le cache SQLite «database» e «all» ora richiede un amministratore.
- Una sincronizzazione completa interrotta lasciava una tabella a metà, e dopo l'annullamento di un
  backup il successivo partiva mentre si stava ancora scrivendo.
- Le copie su un'altra unità e le sincronizzazioni da un altro server aspettavano processi estranei,
  e dopo un riavvio del database un processo poteva partire due volte.

**Altro**
- La musica delle ROM andava in crash con un set PSF2.
- Parti della console web e della schermata delle impostazioni restavano in inglese.
- MariaDB precedente alla 10.6 ora mostra un errore alla connessione.

# romexplorer 0.2.0 (alpha)

## Modifiche

**IA in Revisione unioni**
- «Chiedi a Google» è ora un menu Ask. Apre la modalità IA di Google, Claude, ChatGPT o Perplexity
  nel browser con la domanda già scritta.
- Le risposte nell'app possono arrivare da Gemini, Claude, OpenAI o Perplexity, ognuno con la sua
  chiave API e il suo modello.
- Le risposte ora vengono salvate. «Chiedi di nuovo» ne ottiene una nuova; quelle fissate non
  vengono mai richieste di nuovo.
- Le risposte salvate sono incluse nei backup del database.

**Config › AI**
- È stata aggiunta una scheda delle impostazioni IA.
- Mostra il numero di risposte salvate e il costo stimato del mese e totale. Le risposte si possono
  esportare e importare in JSON.

**IA nella scheda Definitiva**
- La pagina di ogni gioco può chiedere a un'IA in cosa differiscono le sue versioni, per scegliere
  la definitiva. Le risposte ricevono gli stessi gradi delle note.
- Prima della prima domanda compare un avviso sui costi. Config › AI rimanda alla pagina dei limiti
  di utilizzo di ogni fornitore.
- «Valuta in blocco con l'IA» chiede di più giochi in un solo processo. Mostra prima il numero di
  domande e il costo stimato, e salta i giochi già valutati.
- I giochi con una nuova versione uscita dopo la valutazione sono segnati «Nuova versione dopo la
  valutazione» e si possono escludere con il filtro.

**Titoli: versioni minori e gruppi di versioni**
- È stato aggiunto «Nascondi versioni minori».
- È stato aggiunto «Una per ogni set di versioni».
- È stata aggiunta una scheda «Versioni» alle pagine dei titoli.

**Config › Avanzate**
- È stata aggiunta una scheda che raccoglie le azioni che possono danneggiare il database o far
  perdere file. L'ottimizzazione DB è stata spostata qui da Attività e dati › DB.

**Audio delle ROM**
- Le pagine dei titoli possono riprodurre la musica contenuta nelle ROM (tracce audio CD dei giochi
  su disco e archivi sonori di Nintendo DS e 3DS).
- I criteri delle tracce riprodotte si regolano nelle impostazioni.
- Al lettore della pagina del titolo è stato aggiunto un pulsante «non riprodurre questa traccia».
- La musica delle ROM decodificata per la riproduzione viene messa in cache in memoria
  (Impostazioni › Audio, 256 MiB di default).
- La normalizzazione del lettore è stata regolata.
- Il lettore può fare la dissolvenza incrociata (2000 ms di default, 0 la disattiva).
- Il silenzio alla fine delle tracce ora viene tagliato.
- La musica delle pagine dei titoli è ora attiva di default. Al primo avvio, dopo la lingua, l'app
  chiede se riprodurre la musica. I titoli senza audio proprio riproducono una traccia casuale di
  default.

**Revisione unioni e Titoli uniti**
- Un'uscita su una scheda si può trascinare sul titolo giusto. Rilasciarla su «+» crea un nuovo
  titolo.
- Le schede accettate e rifiutate si possono mostrare con il filtro di stato. Di default sono
  nascoste.
- Anche le separazioni vengono registrate in Modifica › Titoli uniti e si possono annullare da lì.
  Compaiono anche quelle fatte in Revisione unioni prima di questo aggiornamento.

**Uscite unite**
- I dischi di un gioco registrati come uscite separate si possono selezionare e unire con «Unisci in
  un'unica uscita».

**Scansione**
- Alle pagine dei file è stato aggiunto «Riscansiona questo file». Rilegge il file insieme agli
  altri file della ROM e associa subito il gioco alla sua uscita.
- Le pagine dei file ora mostrano l'uscita e il titolo collegati.

## Correzioni

- I nomi con un trattino dentro una parola non corrispondevano alla grafia di IGDB, così alcuni
  giochi non trovavano corrispondenza su IGDB.
- Le sincronizzazioni IGDB di SNES e NES tralasciavano i giochi usciti solo in Giappone.
- Scollegare IGDB lasciava sul titolo i nomi alternativi di IGDB, senza modo di cancellarli. Ora se
  ne vanno con il link e ognuno si può cancellare con ✕.
- I link IGDB rimossi a mano venivano ricollegati da «Aggiorna metadati». Ora restano scollegati
  finché non vengono ricollegati a mano.
- I sottoinsiemi di RetroAchievements (sfide, speedrun ecc.) finivano nel titolo del gioco e
  potevano unire versioni diverse in un titolo.
- Annullare un'unione fatta in Revisione unioni non riportava il suggerimento.
- I suggerimenti per titoli già uniti a mano restavano in Revisione unioni.
- I giochi pirata e senza licenza venivano uniti ai titoli commerciali.
- Le schede «un altro gioco all'interno» lasciavano nel titolo giochi pirata e senza licenza di
  altre regioni.
- Le schede «un altro gioco all'interno» prendevano per giochi diversi le uscite con un numero
  davanti al nome («0743 - ...»).
- Processi come l'audit delle unioni ora mostrano un avanzamento dettagliato.
- Su Windows arm64 e Linux arm64 non si potevano convertire immagini Wii U, Xbox e Xbox 360, leggere
  file PBP né riprodurre la musica delle ROM.
- I file elencati in un DAT restavano senza collegamento a uscite o titoli.

# romexplorer 0.1.8 (alpha)

## Modifiche

**Media**
- Ripartire con un nuovo database ora riutilizza le immagini già presenti nella cartella dei media.

**Attività**
- I dettagli dell'elenco dei processi sono stati regolati.
- Ai processi in attesa è stato aggiunto un pulsante «Esegui dopo».

**Corrispondenza hash**
- La corrispondenza hash con il solo Hasheous ora recupera anche dati e immagini IGDB.
- serial_match ora può scegliere per piattaforma se usare i seriali nei nomi dei file.

**Revisione unioni**
- Le risposte di Gemini e «Chiedi a Google» ora usano la lingua dell'interfaccia.
- Una richiesta non riuscita ora mostra il motivo.
- Gemini ora viene interrogato anche se il suo elenco di modelli non si può leggere.

**Definitiva e Crossplat**
- Ora sono mostrati solo i titoli con un ID IGDB.

## Correzioni

**Collezione**
- La ricerca nella scheda Definitiva trovava solo il nome del titolo principale.
