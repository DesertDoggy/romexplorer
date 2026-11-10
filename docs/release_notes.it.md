*Questa è una traduzione delle note di rilascio 0.2.1, 0.2.0 e 0.1.8. Le versioni precedenti sono in
[release_notes.md](release_notes.md) (inglese), che fa fede in caso di differenze.*

# romexplorer 0.2.1 (alpha)

Alpha: lo schema del database può cambiare tra una versione e l'altra senza possibilità di
aggiornamento. Fai un backup prima di sostituire una build.

## Modifiche

**3DS**
- Una copia può decifrare i titoli 3DS in .cci, .3ds o .cxi, oppure nei compressi .zcci e .zcxi
  che legge Azahar. Aggiornamenti e DLC restano .cia (.zcia), l'homebrew .3dsx (.z3dsx).
- romexplorer non include chiavi. Metti il tuo aes_keys.txt (e seeddb.bin se un titolo lo
  richiede) accanto ai file, accanto a romexplorer o nella sua cartella assets.
- Le scansioni trovano .cci, .cxi e i compressi .zcci, .zcia e .zcxi, quindi una copia può anche
  trasformare un .zcci in .cci e viceversa. La corrispondenza per seriale legge codice prodotto e
  ID titolo, con una propria casella per le immagini di cartuccia.

**Audio delle ROM**
- Aprendo la pagina di un titolo si indicizza prima la musica delle ROM di quel gioco. Nel
  frattempo la pagina mostra l'avanzamento invece di riprodurre la musica di un altro gioco.
- Una ROM con lo stesso contenuto di una già indicizzata ne riprende le tracce invece di essere
  riletta.

## Correzioni

- L'indicizzazione della musica delle ROM non finiva mai con alcune immagini disco, quelle con
  un «;» all'interno di un nome di file. Ora una ROM che richiede più di 20 minuti viene saltata.
- Convertendo dump SNES in una copia, un file veniva convertito anche se dimensione e header
  interno non concordavano sull'header del copier. Ora viene segnalato come errore. Un dump NES
  assente dal database degli header viene segnalato con il suo hash.

# romexplorer 0.2.0 (alpha)

Alpha: lo schema del database può cambiare tra una versione e l'altra senza possibilità di
aggiornamento. Fai un backup prima di sostituire una build.

## Modifiche

**IA in Revisione unioni**
- «Chiedi a Google» è ora un menu Ask: la modalità IA di Google, Claude, ChatGPT e Perplexity si
  aprono nel browser con la domanda già scritta.
- La risposta nell'app può arrivare da Gemini, Claude, OpenAI o Perplexity, ognuno con la propria
  chiave API e il proprio modello.
- Le risposte vengono salvate. «Chiedi di nuovo» ne ottiene una nuova; le risposte fissate non
  vengono richieste di nuovo.
- Le risposte salvate sono incluse nel backup del database.

**Config › AI**
- Una nuova scheda per le impostazioni dell'IA.
- Mostra quante risposte sono salvate e il loro costo approssimativo questo mese e in totale. Le
  risposte si possono esportare e importare in JSON.

**IA nella scheda Definitiva**
- La pagina di ogni gioco può chiedere all'IA quale versione ha più contenuti e la grafica, il
  sonoro e la comodità migliori. La risposta ha un grado, come le note revisionate.
- Una risposta ottenuta da un link del browser si può incollare, o salvare dagli appunti con un
  clic.
- La prima domanda mostra un avviso sui costi. Config › AI rimanda alla pagina dei limiti di spesa
  di ogni fornitore.
- «Valuta in blocco con l'IA» chiede di più giochi in un'unica attività: i giochi scelti nella
  scheda Definitiva, oppure le righe scelte di un set o l'intero set. Prima mostra il numero di
  domande e un costo approssimativo, salta i giochi già valutati e si può pianificare per un set.
- Un grado assegnato prima dell'uscita di una versione più recente del gioco è segnato «Nuova
  versione dopo la valutazione», e il filtro può escludere questi gradi.

**Titoli: versioni minori e gruppi di versioni**
- «Nascondi versioni minori» nasconde un gioco quando esiste un'edizione più completa sulla stessa
  piattaforma e regione (International, Director's Cut, Final Mix e simili; una Easy Type cede al
  gioco normale). In Titoli, Uscite e nell'elenco delle uscite di un titolo.
- «Una per ogni set di versioni» mostra una sola versione dei giochi usciti in più versioni (Pokémon,
  Yo-kai Watch e altri): quella che possiedi, altrimenti la tua scelta, altrimenti una a caso, con
  🎲 per vederne un'altra. Le versioni di uno stesso gruppo non formano più un'unica famiglia nella
  scheda Definitiva.
- La nuova scheda Versioni nella pagina di un titolo mostra entrambe le cose e permette di
  correggerle.

**Config › Avanzate**
- Una nuova scheda per le operazioni che possono danneggiare il database o far perdere file.
  L'ottimizzazione DB è stata spostata qui da Attività e dati › DB.

**Audio delle ROM**
- Le pagine dei titoli possono riprodurre la musica contenuta nelle tue ROM (tracce audio CD dei giochi su disco e archivi sonori Nintendo DS e 3DS).
- Nelle impostazioni si sceglie quali tracce riprodurre.
- Il lettore delle pagine dei titoli ha un pulsante «non riprodurre questa traccia». Le tracce
  nascoste sono elencate sotto la playlist: un clic ne riproduce una e Ripristina la rimette.
- La musica delle ROM decodificata per la riproduzione viene tenuta in cache in memoria
  (Impostazioni › Audio, 256 MiB di default). Quando è piena, le tracce passano alla cache audio su
  disco, che usa la cartella della cache di livello 1 se impostata.
- La normalizzazione del lettore è stata regolata.
- Il lettore può fare la dissolvenza incrociata (2000 ms di default, 0 per disattivarla).
- Il silenzio alla fine di una traccia viene tagliato: le tracce delle ROM vengono accorciate, e per
  i file audio la scansione audio misura dove finisce il suono (ripetila per i file già
  scansionati).
- La musica delle pagine dei titoli è attiva di default, e il primo avvio chiede se la vuoi, subito
  dopo la lingua. Un titolo senza audio proprio riproduce tracce a caso di default.

**Revisione unioni e Titoli uniti**
- Le uscite di una scheda si possono trascinare sul titolo a cui appartengono, oppure su «+» per un
  nuovo titolo con il nome della prima. Funziona anche toccare un'uscita e poi un titolo.
- Le schede già accettate o rifiutate si possono mostrare di nuovo con il nuovo filtro di stato.
  Sono nascoste di default.
- Le separazioni sono ora elencate in Modifica › Titoli uniti e si possono annullare da lì. Quelle
  fatte da Revisione unioni prima di questo aggiornamento vengono aggiunte all'elenco.

**Uscite unite**
- I dischi di un gioco elencati separatamente (quelli di un bundle, un disco bonus) si possono unire con «Unisci in un'unica uscita». Viene elencata un'uscita per tutti e copiandola si copiano tutti i dischi. Dalla pagina dell'uscita se ne può separare uno.

**Scansione**
- La pagina di un file ha «Riscansiona questo file». Rilegge il file con il resto della sua ROM e
  abbina subito il suo gioco a un'uscita.
- La pagina di un file mostra l'uscita e il titolo a cui è collegato.

## Correzioni

- Alcuni giochi non trovavano corrispondenza su IGDB: un nome con un trattino dentro una parola non corrispondeva alla grafia di IGDB, e la sincronizzazione IGDB di SNES e NES saltava i giochi usciti solo in Giappone.
- Un titolo conservava i titoli alternativi di IGDB dopo la rimozione del collegamento a IGDB, e non si potevano eliminare. Ora vengono rimossi con il collegamento, e ogni titolo alternativo si può rimuovere con ✕.
- «Aggiorna metadati» ricollegava a IGDB un titolo il cui collegamento era stato rimosso a mano. Ora resta scollegato finché non lo colleghi tu.
- I subset di RetroAchievements (sfida, speedrun e simili) finivano nel titolo del loro gioco e potevano unire due versioni di un gioco in un solo titolo. Ogni subset è ora un titolo a sé; la verifica delle unioni separa quelli già presenti.
- Annullando un'unione accettata nella revisione delle unioni, il suggerimento non tornava. Ora torna da rivedere.
- Un suggerimento di unione restava nella revisione delle unioni dopo aver unito i suoi titoli a mano.
- Giochi pirata e senza licenza venivano uniti ai titoli dei giochi commerciali.
- La scheda «un altro gioco all'interno» lasciava nel titolo giochi pirata e senza licenza di altre
  regioni.
- La scheda «un altro gioco all'interno» considerava un altro gioco un'uscita con un numero davanti
  al nome («0743 - ...»).
- Attività come l'audit delle unioni mostravano poco del loro avanzamento. Ora lo mostrano in
  dettaglio.
- Un file elencato da un DAT poteva restare senza uscita né titolo, se il DAT veniva aggiunto dopo la
  scansione o una sottocartella veniva scansionata dopo la cartella superiore.

# romexplorer 0.1.8 (alpha)

Alpha: lo schema del database può cambiare tra una versione e l'altra senza possibilità di
aggiornamento. Fai un backup prima di sostituire una build.

## Modifiche

**Media**
- Ripartendo con un nuovo database, le immagini già presenti nella cartella dei media vengono
  usate invece di scaricarle di nuovo.

**Attività**
- Le colonne di dettaglio dell'elenco delle attività sono state regolate.
- Un'attività in attesa ha un pulsante «Esegui dopo», anche per una selezione: parte appena finisce
  l'attività in corso nella sua coda. Le altre code non cambiano.

**Corrispondenza hash**
- Una corrispondenza hash con solo Hasheous attivo recupera anche i dati IGDB e le immagini di ciò
  che ha trovato, come con PlayMatch.
- L'impostazione serial_match sceglie per piattaforma se conta il seriale nel nome del file. Una
  piattaforma lasciata com'è segue la casella del nome del file.

**Revisione unioni**
- La risposta di Gemini e «Chiedi a Google» arrivano nella lingua dell'interfaccia.
- Quando la richiesta fallisce, il messaggio dice perché: un certificato o un timeout, il modello e
  la quota esaurita. Una chiave senza quota gratuita per il modello lo segnala.
- Se l'elenco dei modelli non si può leggere, Gemini viene interpellato comunque; con un errore di
  quota si prova una volta il modello Flash successivo.

**Definitiva e Crossplat**
- Sono elencati solo i titoli con un ID IGDB. I titoli Wii U chiamati con il loro ID non compaiono
  più in Definitiva.

## Correzioni

**Collezione**
- La ricerca della scheda Definitiva trovava solo il nome del titolo principale. Ora trova una
  famiglia dal nome di qualsiasi versione, compresi i nomi localizzati e alternativi.
