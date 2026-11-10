*Dies ist eine Übersetzung der Versionshinweise zu 0.2.1, 0.2.0 und 0.1.8. Ältere Versionen stehen in
[release_notes.md](release_notes.md) (Englisch), die bei Abweichungen maßgeblich ist.*

# romexplorer 0.2.1 (alpha)

Alpha: Das Datenbankschema kann sich zwischen Versionen ohne Upgrade-Möglichkeit ändern. Erstellen
Sie ein Backup, bevor Sie einen Build ersetzen.

## Änderungen

**ROM-Audio**
- Beim Öffnen einer Titelseite wird zuerst die ROM-Musik dieses Spiels indiziert. Bis dahin zeigt
  die Seite den Fortschritt, statt Musik eines anderen Spiels abzuspielen.
- Ein ROM mit demselben Inhalt wie ein bereits indiziertes übernimmt dessen Titel, statt erneut
  gelesen zu werden.

## Fehlerbehebungen

- Die Indizierung der ROM-Musik wurde bei einigen Disc-Images nie fertig, und zwar bei solchen
  mit einem „;“ mitten im Dateinamen. Ein ROM, das länger als 20 Minuten braucht, wird jetzt
  übersprungen.

# romexplorer 0.2.0 (alpha)

Alpha: Das Datenbankschema kann sich zwischen Versionen ohne Upgrade-Möglichkeit ändern. Erstellen
Sie ein Backup, bevor Sie einen Build ersetzen.

## Änderungen

**KI in „Zusammenführungen prüfen“**
- „Google fragen“ ist jetzt ein Ask-Menü: Googles KI-Modus, Claude, ChatGPT und Perplexity öffnen
  sich im Browser mit bereits eingetragener Frage.
- Die Antwort in der App kann von Gemini, Claude, OpenAI oder Perplexity kommen, jeweils mit
  eigenem API-Schlüssel und Modell.
- Antworten werden gespeichert. „Erneut fragen“ holt eine neue; angeheftete Antworten werden nicht
  neu abgefragt.
- Gespeicherte Antworten sind im Datenbank-Backup enthalten.

**Config › AI**
- Ein neuer Tab für die KI-Einstellungen.
- Zeigt, wie viele Antworten gespeichert sind und was sie diesen Monat und insgesamt ungefähr
  gekostet haben. Antworten lassen sich als JSON exportieren und importieren.

**KI im Definitiv-Tab**
- Auf der Seite jedes Spiels lässt sich die KI fragen, welche Version den meisten Inhalt und die
  beste Grafik, den besten Sound und den meisten Komfort hat. Die Antwort trägt einen Rang wie die
  geprüften Notizen.
- Eine Antwort aus einem Browser-Link kann eingefügt oder mit einem Klick aus der Zwischenablage
  gespeichert werden.
- Vor der ersten Frage erscheint ein Kostenhinweis. Config › AI verlinkt die Ausgabenlimits jedes
  Anbieters.
- „Mit KI gesammelt bewerten“ fragt mehrere Spiele in einem Job ab: die im Definitiv-Tab
  gewählten Spiele oder die gewählten Zeilen eines Sets oder das ganze Set. Vorher werden die Zahl
  der Fragen und die ungefähren Kosten angezeigt, schon beantwortete Spiele werden übersprungen, und
  für ein Set lässt es sich planen.
- Ein Rang, der vor dem Erscheinen einer neueren Version des Spiels vergeben wurde, ist mit „Neue
  Version seit der Bewertung“ markiert, und der Filter kann solche Ränge auslassen.

**Titel: Nebenversionen und Versionsgruppen**
- „Nebenversionen ausblenden“ blendet ein Spiel aus, wenn es auf derselben Plattform und in derselben
  Region eine vollständigere Ausgabe gibt (International, Director's Cut, Final Mix und ähnliche;
  eine Easy Type weicht dem normalen Spiel). In Titel, Releases und der Release-Liste eines Titels.
- „Eine je Versionsset“ zeigt nur eine Version von Spielen, die in mehreren Versionen
  erschienen sind (Pokémon, Yo-kai Watch und andere): die, die Sie besitzen, sonst Ihre Wahl, sonst
  eine zufällige, mit 🎲 für eine andere. Versionen derselben Gruppe bilden im Definitiv-Tab keine
  gemeinsame Familie mehr.
- Die neue Karte „Versionen“ auf der Seite eines Titels zeigt beides und lässt es korrigieren.

**Config › Erweitert**
- Ein neuer Tab für Vorgänge, die die Datenbank beschädigen oder Dateien verlieren können. „DB
  optimieren“ ist von Jobs und Daten › DB hierher umgezogen.

**ROM-Audio**
- Titelseiten können die Musik in Ihren ROMs abspielen (CD-Audiotracks von Disc-Spielen sowie Soundarchive von Nintendo DS und 3DS).
- In den Einstellungen wird festgelegt, welche Tracks spielen.
- Der Player auf Titelseiten hat eine Schaltfläche „diesen Track nicht spielen“. Ausgeblendete
  Tracks stehen unter der Playlist: Ein Klick spielt einen ab, „Wiederherstellen“ holt ihn zurück.
- Zum Abspielen dekodierte ROM-Musik wird im Arbeitsspeicher zwischengespeichert (Einstellungen ›
  Audio, standardmäßig 256 MiB). Ist er voll, wandern Tracks in den Audio-Cache auf der Festplatte,
  der den Ordner des Tier-1-Caches nutzt, wenn einer eingestellt ist.
- Die Normalisierung im Player wurde angepasst.
- Der Player kann überblenden (standardmäßig 2000 ms, 0 schaltet es aus).
- Stille am Ende eines Tracks wird abgeschnitten: ROM-Tracks werden gekürzt, und bei Audiodateien
  misst der Audio-Scan, wo der Ton endet (für früher gescannte Dateien erneut ausführen).
- Musik auf Titelseiten ist standardmäßig an, und der erste Start fragt direkt nach der Sprache, ob
  Sie sie möchten. Ein Titel ohne eigenes Audio spielt standardmäßig zufällige Tracks.

**Zusammenführungen prüfen und zusammengeführte Titel**
- Releases auf einer Karte lassen sich auf den Titel ziehen, zu dem sie gehören, oder auf „+“ für
  einen neuen Titel mit dem Namen des ersten. Ein Release antippen und dann einen Titel geht auch.
- Bereits übernommene oder abgelehnte Karten lassen sich mit dem neuen Statusfilter wieder
  anzeigen. Standardmäßig sind sie ausgeblendet.
- Trennungen stehen jetzt unter Bearbeiten › Zusammengeführte Titel und lassen sich dort rückgängig
  machen. Trennungen aus „Zusammenführungen prüfen“ vor diesem Update werden der Liste hinzugefügt.

**Zusammengefasste Veröffentlichungen**
- Getrennt gelistete Discs eines Spiels (die Discs eines Bundles, eine Bonus-Disc) lassen sich mit „Zu einer Veröffentlichung zusammenfassen“ zusammenlegen. Gelistet wird eine Veröffentlichung für alle, und beim Kopieren werden alle Discs kopiert. Auf der Seite der Veröffentlichung lässt sich eine wieder abtrennen.

**Scan**
- Die Seite einer Datei hat „Diese Datei neu scannen“. Die Datei wird mit dem Rest ihres ROMs neu
  gelesen und ihr Spiel sofort einem Release zugeordnet.
- Die Seite einer Datei zeigt das Release und den Titel, mit denen die Datei verknüpft ist.

## Fehlerbehebungen

- Manche Spiele fanden keinen IGDB-Treffer: Ein Name mit Bindestrich innerhalb eines Wortes passte nicht zur Schreibweise bei IGDB, und die IGDB-Synchronisierung für SNES und NES ließ Spiele aus, die nur in Japan erschienen.
- Ein Titel behielt die alternativen Titel von IGDB, nachdem seine IGDB-Verknüpfung entfernt wurde, und sie ließen sich nicht löschen. Sie werden jetzt mit der Verknüpfung entfernt, und jeder alternative Titel lässt sich mit ✕ löschen.
- „Metadaten aktualisieren“ verknüpfte einen Titel wieder mit IGDB, obwohl die Verknüpfung von Hand entfernt worden war. Sie bleibt jetzt entfernt, bis Sie selbst verknüpfen.
- RetroAchievements-Subsets (Challenge, Speedrun usw.) landeten im Titel ihres Spiels und konnten zwei Versionen eines Spiels in einem Titel vereinen. Jedes Subset ist jetzt ein eigener Titel; die Zusammenführungsprüfung trennt die vorhandenen ab.
- Wurde eine Zusammenführung aus der Zusammenführungsprüfung rückgängig gemacht, kam der Vorschlag nicht zurück. Er steht jetzt wieder zur Prüfung an.
- Ein Zusammenführungsvorschlag blieb in der Zusammenführungsprüfung, nachdem seine Titel von Hand zusammengeführt wurden.
- Raubkopien und unlizenzierte Spiele wurden mit den Titeln kommerzieller Spiele zusammengeführt.
- Die Karte „ein anderes Spiel darin“ ließ Raubkopien und unlizenzierte Spiele anderer Regionen im
  Titel.
- Die Karte „ein anderes Spiel darin“ hielt ein Release mit einer Nummer vor dem Namen
  („0743 - ...“) für ein anderes Spiel.
- Jobs wie die Zusammenführungsprüfung zeigten wenig von ihrem Fortschritt. Jetzt zeigen sie ihn
  im Detail.
- Eine Datei, die ein DAT listet, konnte ohne Release und Titel bleiben, wenn das DAT nach dem Scan
  hinzukam oder ein Unterordner nach dem Ordner darüber gescannt wurde.

# romexplorer 0.1.8 (alpha)

Alpha: Das Datenbankschema kann sich zwischen Versionen ohne Upgrade-Möglichkeit ändern. Erstellen
Sie ein Backup, bevor Sie einen Build ersetzen.

## Änderungen

**Medien**
- Nach einem Neustart mit einer neuen Datenbank werden die Bilder, die schon im Medienordner
  liegen, verwendet statt neu heruntergeladen.

**Jobs**
- Die Detailspalten der Jobliste wurden angepasst.
- Ein wartender Job hat eine Schaltfläche „Als Nächstes ausführen“, auch für eine Auswahl: Er startet, sobald der
  laufende Job in seiner Spur fertig ist. Andere Spuren sind nicht betroffen.

**Hash-Abgleich**
- Ein Hash-Abgleich, bei dem nur Hasheous aktiv ist, holt für die Treffer auch IGDB-Daten und
  Bilder, wie mit PlayMatch.
- Die Einstellung serial_match legt pro Plattform fest, ob eine Seriennummer im Dateinamen zählt.
  Eine unveränderte Plattform folgt dem Kästchen für den Dateinamen.

**Zusammenführungen prüfen**
- Die Gemini-Antwort und „Google fragen“ kommen in der Sprache der Oberfläche.
- Schlägt die Anfrage fehl, sagt die Meldung warum: ein Zertifikat oder eine Zeitüberschreitung, das
  Modell und das aufgebrauchte Kontingent. Ein Schlüssel ohne Gratiskontingent für das Modell sagt
  das.
- Lässt sich die Modellliste nicht lesen, wird Gemini trotzdem gefragt; bei einem
  Kontingentfehler wird das nächste Flash-Modell einmal versucht.

**Definitiv und Crossplat**
- Nur Titel mit einer IGDB-ID werden aufgeführt. Wii-U-Titel, die nach ihrer Titel-ID benannt
  sind, erscheinen nicht mehr in Definitiv.

## Fehlerbehebungen

**Sammlung**
- Die Suche im Definitiv-Tab fand nur den Namen des führenden Titels. Jetzt findet sie eine
  Familie über den Namen jeder Version, auch über lokalisierte und alternative Namen.
