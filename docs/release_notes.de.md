*Dies ist eine Übersetzung der Versionshinweise zu 0.2.0 und 0.1.8. Ältere Versionen stehen in
[release_notes.md](release_notes.md) (Englisch), die bei Abweichungen maßgeblich ist.*

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
- Titelseiten können die Musik in Ihren ROMs abspielen (nur unterstützte Plattformen und
  Medientypen).
- In den Einstellungen wird festgelegt, welche Tracks spielen.
- NES- und Famicom-Musik spielt direkt aus dem ROM, über den eigenen Soundcode des Spiels: keine Original-Audiodateien nötig. Famicom-Soundchips (VRC6, Namco 163, Sunsoft 5B, MMC5) inklusive.
- Musik von Disk System, Game Boy, Game Boy Color, Master System, Game Gear, Mega Drive, PC Engine, MSX, WonderSwan, Virtual Boy und Game Boy Advance spielt ebenso direkt aus dem ROM, FM-Sound des japanischen Master System inklusive.
- Original-Audioformate (NSF, SPC, GBS, VGM, HES, KSS, AY, SAP und Game-Gear-SGC, einzeln
  oder gezippt) werden unterstützt.
- Eine Original-Audio-Bibliothek: Tragen Sie unter Einstellungen › Audio die Ordner Ihrer Sets ein, und
  „ROM-Audio indizieren“ ordnet jedes Set anhand des Inhalts Ihren ROMs zu. Ein Set spielt so für jede
  Region des Spiels, egal wie es heißt.
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
- PSF- und PSF2-Dateien nutzen ein PS2-BIOS aus Ihrer Sammlung und werden ohne eines übersprungen.

**Zusammenführungen prüfen und zusammengeführte Titel**
- Releases auf einer Karte lassen sich auf den Titel ziehen, zu dem sie gehören, oder auf „+“ für
  einen neuen Titel mit dem Namen des ersten. Ein Release antippen und dann einen Titel geht auch.
- Bereits übernommene oder abgelehnte Karten lassen sich mit dem neuen Statusfilter wieder
  anzeigen. Standardmäßig sind sie ausgeblendet.
- Trennungen stehen jetzt unter Bearbeiten › Zusammengeführte Titel und lassen sich dort rückgängig
  machen. Trennungen aus „Zusammenführungen prüfen“ vor diesem Update werden der Liste hinzugefügt.

## Fehlerbehebungen

- Raubkopien und unlizenzierte Spiele wurden mit den Titeln kommerzieller Spiele zusammengeführt.
- Die Karte „ein anderes Spiel darin“ ließ Raubkopien und unlizenzierte Spiele anderer Regionen im
  Titel.
- Die Karte „ein anderes Spiel darin“ hielt ein Release mit einer Nummer vor dem Namen
  („0743 - ...“) für ein anderes Spiel.
- Jobs wie die Zusammenführungsprüfung zeigten wenig von ihrem Fortschritt. Jetzt zeigen sie ihn
  im Detail.

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
