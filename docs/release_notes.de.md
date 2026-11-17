*Dies ist eine Übersetzung der Versionshinweise zu 0.2.1, 0.2.0 und 0.1.8. Ältere Versionen stehen in
[release_notes.md](release_notes.md) (Englisch), die bei Abweichungen maßgeblich ist.*

# romexplorer 0.2.2 (alpha)

Alpha: Das Datenbankschema kann sich zwischen Versionen ohne Upgrade-Möglichkeit ändern. Erstellen
Sie ein Backup, bevor Sie einen Build ersetzen.

## Änderungen

**ROM-Audio**
- Der ROM-Musikindex kann mehrere ROMs gleichzeitig lesen (Standard 4, in den Audio-Einstellungen änderbar).
- Der ROM-Musikindex liest pro Titel nur die am höchsten eingestufte Kopie, andere Kopien nur, wenn sie keine Musik hat.
- Bei Spielen mit mehreren Discs werden Musikdateien, die schon auf einer früheren Disc liegen, nicht erneut gelesen.

# romexplorer 0.2.1 (alpha)

Alpha: Das Datenbankschema kann sich zwischen Versionen ohne Upgrade-Möglichkeit ändern. Erstellen
Sie ein Backup, bevor Sie einen Build ersetzen.

## Änderungen

**3DS**
- 3DS-ROMs lassen sich in andere Formate umwandeln.
- Schlüssel liegen nicht bei. Legen Sie aes_keys.txt (und seeddb.bin, wenn ein Titel sie braucht)
  neben die Dateien, neben romexplorer oder in dessen assets-Ordner.
- Serienabgleich für 3DS-ROMs.

**ROM-Audio**
- Beim Öffnen einer Titelseite wird die ROM-Musik dieses Spiels jetzt zuerst indexiert.
- Der ROM-Musikindex liest jetzt die kleinsten ROMs zuerst und überspringt nicht unterstützte ROMs.
- PS1-Spiele mit eigener Sound-Engine werden unterstützt.

## Fehlerbehebungen

- Der ROM-Musikindex schien stehen zu bleiben, während eine ROM lange gelesen wurde.
- Der ROM-Musikindex übersprang ROMs, die nur über einen DAT-Abgleich erkannt wurden.
- Ein Scan erkannte japanische Super-Famicom-ROMs mit einem internen Titel in Katakana keiner Plattform zu.
- Eine Titelseite listete dieselbe Musik für jede gefundene Kopie des Spiels erneut auf.
- Die Indexierung der ROM-Musik wurde bei manchen Disc-Images nie fertig.
- Beim Umwandeln von SNES-Dumps wurde eine Datei, deren Größe und interner Header sich beim
  Copier-Header widersprachen, trotzdem umgewandelt.

**Scan**
- Unter Linux übersah ein Scan cue-, gdi- und m3u-Einträge mit anderer Groß-/Kleinschreibung oder
  „\“ als Trenner und hielt zwei Dateien, die sich nur in der Schreibung unterscheiden, für ein ROM.
- Ein Scan des übergeordneten Ordners ignorierte Typ und Plattform der Ordner darin.
- Neuscans lasen Ordnerspiele neu ein, trennten Tracks und lösten Serienabgleiche. Das Gruppieren
  von Discs in großen Ordnern ist schneller.
- Dateien, die keine ROMs sind (Text, Bilder), werden jetzt erst nach einem Abgleich zu Spielen.
- Einer von crc32, md5 und sha1 muss jetzt bei den Scan-Hashes aktiv bleiben. Mit nur xxh3_128
  gingen DAT-Abgleiche verloren.

**Kopie**
- Beim Überschreiben wurde die alte Datei vor der neuen gelöscht, eine Kopie schrieb Playlists um,
  die erhalten bleiben sollten, und zwei Dateien, die sich nur in der Schreibung unterscheiden,
  wurden in eine geschrieben.

**Titel zusammenführen**
- Zusammenführen verband Versionen einer Versionsgruppe oder lizenzierte mit nicht lizenzierten
  Titeln. „Alle annehmen“ stellte aufgelöste Zusammenführungen wieder her, und vom Audit entfernte
  IGDB-Links kamen zurück.
- Ein Hasheous-Abgleich über den Zweitnamen eines zusammengeführten Spiels erzeugte einen doppelten
  Titel.

**Server und Jobs**
- Das Abrufen von Bildern für einen Client konnte das interne Netzwerk des Servers erreichen. Eine
  Größengrenze kam ebenfalls hinzu.
- Das Herunterladen der SQLite-Caches „database“ und „all“ erfordert jetzt einen Administrator.
- Eine abgebrochene vollständige Synchronisierung ließ eine Tabelle halb gefüllt, und nach dem
  Abbrechen eines Backups startete das nächste, während noch geschrieben wurde.
- Kopien auf ein anderes Laufwerk und Synchronisierungen von einem anderen Server warteten auf
  fremde Jobs, und nach einem Datenbank-Neustart konnte ein Job doppelt starten.

**Sonstiges**
- ROM-Musik stürzte bei einem PSF2-Set ab.
- Teile der Webkonsole und des Einstellungsbildschirms blieben englisch.
- MariaDB älter als 10.6 zeigt jetzt beim Verbinden einen Fehler.

# romexplorer 0.2.0 (alpha)

## Änderungen

**KI in „Zusammenführungen prüfen“**
- „Google fragen“ ist jetzt ein Ask-Menü. Es öffnet Googles KI-Modus, Claude, ChatGPT oder
  Perplexity im Browser mit eingetragener Frage.
- Antworten in der App können von Gemini, Claude, OpenAI oder Perplexity kommen, jeweils mit
  eigenem API-Schlüssel und Modell.
- Antworten werden jetzt gespeichert. „Erneut fragen“ holt eine neue; angeheftete Antworten werden
  nie neu abgefragt.
- Gespeicherte Antworten sind im Datenbank-Backup enthalten.

**Config › AI**
- Ein Tab für die KI-Einstellungen wurde hinzugefügt.
- Er zeigt die Zahl der gespeicherten Antworten und die geschätzten Kosten für diesen Monat und
  insgesamt. Antworten lassen sich als JSON exportieren und importieren.

**KI im Definitiv-Tab**
- Auf der Seite jedes Spiels kann man eine KI fragen, wie sich seine Versionen unterscheiden, um die
  definitive zu wählen. Antworten erhalten dieselben Ränge wie die Hinweise.
- Vor der ersten Frage erscheint ein Kostenhinweis. Config › AI verlinkt die Seite mit den
  Nutzungsgrenzen jedes Anbieters.
- „Mit KI gesammelt bewerten“ fragt zu mehreren Spielen in einem Job. Vorher werden Zahl der Fragen
  und geschätzte Kosten gezeigt; bereits beantwortete Spiele werden übersprungen.
- Spiele, für die nach der Bewertung eine neue Version erschien, sind als „Neue Version seit
  Bewertung“ markiert und lassen sich herausfiltern.

**Titel: Nebenversionen und Versionsgruppen**
- „Nebenversionen ausblenden“ wurde hinzugefügt.
- „Eine je Versionsset“ wurde hinzugefügt.
- Titelseiten haben eine Karte „Versionen“.

**Config › Erweitert**
- Ein Tab sammelt Aktionen, die die Datenbank beschädigen oder Dateien verlieren können. Die
  Datenbankoptimierung ist von Jobs und Daten › DB hierher umgezogen.

**ROM-Audio**
- Titelseiten können die Musik in ROMs abspielen (CD-Audiotracks von Disc-Spielen sowie
  Soundarchive von Nintendo DS und 3DS).
- Die Kriterien, welche Tracks spielen, lassen sich in den Einstellungen anpassen.
- Der Player der Titelseite hat eine Schaltfläche „diesen Track nicht spielen“.
- Für die Wiedergabe dekodierte ROM-Musik wird im Speicher zwischengespeichert (Einstellungen ›
  Audio, standardmäßig 256 MiB).
- Die Normalisierung des Players wurde abgestimmt.
- Der Player kann überblenden (standardmäßig 2000 ms, 0 schaltet ab).
- Stille am Ende von Tracks wird jetzt abgeschnitten.
- Musik auf Titelseiten ist jetzt standardmäßig an. Beim ersten Start fragt die App nach der Sprache,
  ob Musik spielen soll. Titel ohne eigenes Audio spielen standardmäßig einen zufälligen Track.

**Zusammenführungen prüfen und zusammengeführte Titel**
- Eine Veröffentlichung auf einer Karte lässt sich auf den richtigen Titel ziehen. Auf „+“
  abgelegt entsteht ein neuer Titel.
- Angenommene und abgelehnte Karten lassen sich mit dem Statusfilter zeigen. Standardmäßig sind
  sie ausgeblendet.
- Trennungen werden auch in Bearbeiten › Zusammengeführte Titel erfasst und lassen sich dort
  rückgängig machen. Trennungen aus „Zusammenführungen prüfen“ vor diesem Update sind ebenfalls
  aufgeführt.

**Zusammengefasste Veröffentlichungen**
- Discs eines Spiels, die als getrennte Veröffentlichungen geführt sind, lassen sich auswählen und
  mit „Zu einer Veröffentlichung zusammenfassen“ verbinden.

**Scan**
- Dateiseiten haben „Diese Datei neu scannen“. Die Datei wird zusammen mit den übrigen Dateien des
  ROMs neu gelesen und das Spiel sofort seiner Veröffentlichung zugeordnet.
- Dateiseiten zeigen jetzt die verknüpfte Veröffentlichung und den Titel.

## Fehlerbehebungen

- Namen mit einem Bindestrich im Wort passten nicht zur IGDB-Schreibweise, sodass manche Spiele
  keinen IGDB-Treffer fanden.
- Die IGDB-Synchronisierung für SNES und NES ließ Spiele aus, die nur in Japan erschienen.
- Das Lösen eines IGDB-Links ließ die IGDB-Alternativnamen am Titel, ohne Möglichkeit zum Löschen.
  Sie verschwinden jetzt mit dem Link, und jeder lässt sich mit ✕ löschen.
- Von Hand gelöste IGDB-Links wurden durch „Metadaten aktualisieren“ wieder verknüpft. Sie bleiben
  jetzt gelöst, bis sie von Hand neu verknüpft werden.
- RetroAchievements-Subsets (Challenges, Speedruns usw.) landeten im Titel des Spiels und konnten
  verschiedene Versionen zu einem Titel zusammenziehen.
- Das Rückgängigmachen einer Zusammenführung aus „Zusammenführungen prüfen“ brachte den Vorschlag
  nicht zurück.
- Vorschläge für bereits von Hand zusammengeführte Titel blieben in „Zusammenführungen prüfen“.
- Raubkopien und nicht lizenzierte Spiele wurden mit Titeln aus dem Handel zusammengeführt.
- Karten „ein anderes Spiel darin“ ließen Raubkopien und nicht lizenzierte Spiele anderer Regionen
  im Titel.
- Karten „ein anderes Spiel darin“ hielten Veröffentlichungen mit einer Nummer vor dem Namen
  („0743 - ...“) für andere Spiele.
- Jobs wie das Zusammenführungs-Audit zeigen jetzt einen detaillierten Fortschritt.
- Unter Windows arm64 und Linux arm64 ließen sich Wii-U-, Xbox- und Xbox-360-Images nicht
  umwandeln, PBP-Dateien nicht lesen und ROM-Musik nicht abspielen.
- In einem DAT aufgeführte Dateien blieben mit keiner Veröffentlichung und keinem Titel verknüpft.

# romexplorer 0.1.8 (alpha)

## Änderungen

**Medien**
- Ein Neustart mit einer neuen Datenbank verwendet jetzt die Bilder, die schon im Medienordner
  liegen.

**Jobs**
- Die Details der Jobliste wurden angepasst.
- Wartende Jobs haben eine Schaltfläche „Als Nächstes ausführen“.

**Hash-Abgleich**
- Ein Hash-Abgleich nur mit Hasheous holt jetzt auch IGDB-Daten und Bilder.
- serial_match kann jetzt je Plattform wählen, ob Seriennummern aus Dateinamen genutzt werden.

**Zusammenführungen prüfen**
- Antworten von Gemini und „Google fragen“ verwenden jetzt die UI-Sprache.
- Eine fehlgeschlagene Anfrage zeigt jetzt den Grund.
- Gemini wird jetzt auch gefragt, wenn die Modellliste nicht gelesen werden kann.

**Definitiv und Crossplat**
- Es werden jetzt nur Titel mit IGDB-ID gezeigt.

## Fehlerbehebungen

**Sammlung**
- Die Suche im Definitiv-Tab fand nur den Namen des führenden Titels.
