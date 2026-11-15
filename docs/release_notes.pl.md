*To jest tłumaczenie informacji o wydaniach 0.2.1, 0.2.0 i 0.1.8. Starsze wersje są w
[release_notes.md](release_notes.md) (po angielsku), która rozstrzyga w razie różnic.*

# romexplorer 0.2.2 (alpha)

Alfa: schemat bazy danych może się zmienić między wydaniami bez możliwości aktualizacji. Zrób kopię
zapasową przed podmianą wersji.

## Poprawki błędów

- Indeks muzyki ROM wyglądał na zatrzymany, gdy odczyt jednej ROM trwał długo.

# romexplorer 0.2.1 (alpha)

Alfa: schemat bazy danych może się zmienić między wydaniami bez możliwości aktualizacji. Zrób kopię
zapasową przed podmianą wersji.

## Zmiany

**3DS**
- ROM-y 3DS można konwertować do innych formatów.
- Klucze nie są dołączone. Umieść aes_keys.txt (i seeddb.bin, gdy tytuł go potrzebuje) obok plików,
  obok romexplorer albo w jego folderze assets.
- Dopasowanie po numerze seryjnym dla ROM-ów 3DS.

**Dźwięk z ROM-ów**
- Otwarcie strony tytułu najpierw indeksuje teraz muzykę z ROM-ów tej gry.
- Indeks muzyki z ROM-ów czyta teraz najpierw najmniejsze ROM-y i pomija nieobsługiwane.
- Obsługa gier PS1 z własnym silnikiem dźwięku.

## Poprawki błędów

- Indeksowanie muzyki z ROM-ów nigdy się nie kończyło na niektórych obrazach płyt.
- Przy konwersji zrzutów SNES w kopii plik, którego rozmiar i wewnętrzny nagłówek nie zgadzały się
  co do nagłówka kopiarki, był mimo to konwertowany.

**Skanowanie**
- Na Linuksie skanowanie pomijało wpisy cue, gdi i m3u zapisane inną wielkością liter lub z „\” jako
  separatorem i brało dwa pliki różniące się tylko wielkością liter za jeden ROM.
- Skanowanie folderu ignorowało typ i platformę ustawione dla folderów w środku.
- Ponowne skanowania znów czytały gry w folderach, rozdzielały ścieżki i gubiły dopasowania po
  numerze seryjnym. Grupowanie płyt w dużych folderach jest szybsze.
- Pliki, które nie są ROM-ami (tekst, obrazy), stają się teraz grami dopiero po rozpoznaniu.
- Jeden z crc32, md5 i sha1 musi teraz pozostać włączony w hashach skanowania. Przy samym xxh3_128
  ginęły dopasowania DAT.

**Kopiowanie**
- Nadpisywanie usuwało stary plik przed zapisaniem nowego, kopia przepisywała playlisty, które miały
  zostać, a dwa pliki różniące się tylko wielkością liter były zapisywane w jeden.

**Łączenie tytułów**
- Łączenie scalało wersje z jednej grupy wersji albo tytuły sklepowe z nielicencjonowanymi.
  „Akceptuj wszystkie” przywracało cofnięte scalenia, a linki IGDB usunięte przez audyt wracały.
- Dopasowanie Hasheous po innej nazwie połączonej gry tworzyło zdublowany tytuł.

**Serwer i zadania**
- Pobieranie grafik dla klienta mogło sięgać do wewnętrznej sieci serwera. Dodano też limit
  rozmiaru.
- Pobieranie pamięci podręcznych SQLite „database” i „all” wymaga teraz administratora.
- Przerwana pełna synchronizacja zostawiała tabelę w połowie, a po anulowaniu kopii zapasowej
  następna zaczynała się, gdy trwał jeszcze zapis.
- Kopie na inny dysk i synchronizacje z innego serwera czekały na niezwiązane zadania, a po
  restarcie bazy danych zadanie mogło wystartować dwa razy.

**Inne**
- Muzyka z ROM-ów zawieszała się na zestawie PSF2.
- Części konsoli webowej i ekranu ustawień zostawały po angielsku.
- MariaDB starsze niż 10.6 pokazuje teraz błąd przy połączeniu.

# romexplorer 0.2.0 (alpha)

## Zmiany

**AI w Przeglądzie scaleń**
- „Zapytaj Google” jest teraz menu Ask. Otwiera tryb AI Google, Claude, ChatGPT lub Perplexity w
  przeglądarce z wpisanym pytaniem.
- Odpowiedzi w aplikacji mogą pochodzić z Gemini, Claude, OpenAI lub Perplexity, każde z własnym
  kluczem API i modelem.
- Odpowiedzi są teraz zapisywane. „Zapytaj ponownie” pobiera nową; przypięte nigdy nie są pobierane
  ponownie.
- Zapisane odpowiedzi są w kopiach zapasowych bazy danych.

**Config › AI**
- Dodano kartę ustawień AI.
- Pokazuje liczbę zapisanych odpowiedzi i szacowany koszt w tym miesiącu i łącznie. Odpowiedzi
  można eksportować i importować jako JSON.

**AI na karcie Definitywne**
- Na stronie każdej gry można zapytać AI, czym różnią się jej wersje, by wybrać definitywną.
  Odpowiedzi dostają te same rangi co notatki.
- Przed pierwszym pytaniem pojawia się uwaga o kosztach. Config › AI linkuje stronę limitów użycia
  każdego dostawcy.
- „Oceń zbiorczo z AI” pyta o kilka gier w jednym zadaniu. Najpierw pokazuje liczbę pytań i
  szacowany koszt, a gry z odpowiedzią pomija.
- Gry, które po ocenie dostały nową wersję, są oznaczone „Nowa wersja od oceny” i można je
  odfiltrować.

**Tytuły: wersje pomniejsze i grupy wersji**
- Dodano „Ukryj wersje pomniejsze”.
- Dodano „Jedna z każdego zestawu wersji”.
- Na stronach tytułów dodano kartę Wersje.

**Config › Zaawansowane**
- Dodano kartę zbierającą działania, które mogą uszkodzić bazę danych lub spowodować utratę plików.
  Optymalizacja DB została tu przeniesiona z Zadania i dane › DB.

**Dźwięk z ROM-ów**
- Strony tytułów mogą odtwarzać muzykę z ROM-ów (ścieżki audio CD gier na płytach oraz archiwa
  dźwięku Nintendo DS i 3DS).
- Kryteria odtwarzanych utworów można dostosować w ustawieniach.
- Do odtwarzacza na stronie tytułu dodano przycisk „nie odtwarzaj tego utworu”.
- Muzyka z ROM-ów zdekodowana do odtwarzania trafia do pamięci podręcznej w RAM (Ustawienia › Audio,
  domyślnie 256 MiB).
- Dostrojono normalizację odtwarzacza.
- Odtwarzacz może płynnie przechodzić między utworami (domyślnie 2000 ms, 0 wyłącza).
- Cisza na końcu utworów jest teraz ucinana.
- Muzyka na stronach tytułów jest teraz domyślnie włączona. Przy pierwszym uruchomieniu, po
  wyborze języka, aplikacja pyta, czy odtwarzać muzykę. Tytuły bez własnego dźwięku domyślnie
  odtwarzają losowy utwór.

**Przegląd scaleń i Połączone tytuły**
- Wydanie z karty można przeciągnąć do właściwego tytułu. Upuszczenie go na „+” tworzy nowy tytuł.
- Przyjęte i odrzucone karty można pokazać filtrem stanu. Domyślnie są ukryte.
- Podziały też są zapisywane w Edycja › Połączone tytuły i można je tam cofnąć. Widać też podziały
  z Przeglądu scaleń sprzed tej aktualizacji.

**Połączone wydania**
- Płyty jednej gry wymienione jako osobne wydania można zaznaczyć i połączyć poleceniem „Połącz w
  jedno wydanie”.

**Skanowanie**
- Na stronach plików dodano „Przeskanuj ten plik ponownie”. Czyta plik ponownie razem z pozostałymi
  plikami ROM-u i od razu dopasowuje grę do jej wydania.
- Strony plików pokazują teraz połączone wydanie i tytuł.

## Poprawki błędów

- Nazwy z łącznikiem w środku słowa nie pasowały do pisowni IGDB, więc niektóre gry nie znajdowały
  dopasowania w IGDB.
- Synchronizacje IGDB dla SNES i NES pomijały gry wydane tylko w Japonii.
- Odłączenie IGDB zostawiało alternatywne nazwy IGDB przy tytule, bez możliwości ich usunięcia.
  Teraz znikają razem z linkiem, a każdą można usunąć przyciskiem ✕.
- Linki IGDB usunięte ręcznie wracały po „Aktualizuj metadane”. Teraz pozostają odłączone, dopóki
  nie zostaną ręcznie połączone ponownie.
- Podzestawy RetroAchievements (wyzwania, speedruny itd.) trafiały do tytułu gry i mogły łączyć
  różne wersje w jeden tytuł.
- Cofnięcie scalenia z Przeglądu scaleń nie przywracało propozycji.
- Propozycje dla tytułów już scalonych ręcznie zostawały w Przeglądzie scaleń.
- Gry pirackie i nielicencjonowane były scalane z tytułami sklepowymi.
- Karty „inna gra w środku” zostawiały w tytule gry pirackie i nielicencjonowane z innych regionów.
- Karty „inna gra w środku” brały wydania z numerem przed nazwą („0743 - ...”) za inne gry.
- Zadania takie jak audyt scaleń pokazują teraz szczegółowy postęp.
- Na Windows arm64 i Linux arm64 nie dało się konwertować obrazów Wii U, Xbox i Xbox 360, czytać
  plików PBP ani odtwarzać muzyki z ROM-ów.
- Pliki wymienione w DAT pozostawały niepołączone z żadnym wydaniem ani tytułem.

# romexplorer 0.1.8 (alpha)

## Zmiany

**Media**
- Start od nowa z nową bazą danych używa teraz grafik, które już są w folderze mediów.

**Zadania**
- Dostosowano szczegóły na liście zadań.
- Do oczekujących zadań dodano przycisk „Uruchom jako następne”.

**Dopasowanie po hashu**
- Dopasowanie po hashu z samym Hasheous pobiera teraz też dane i grafiki IGDB.
- serial_match może teraz dla każdej platformy wybrać, czy używać numerów seryjnych z nazw plików.

**Przegląd scaleń**
- Odpowiedzi Gemini i „Zapytaj Google” używają teraz języka interfejsu.
- Nieudane zapytanie pokazuje teraz przyczynę.
- Gemini jest teraz pytane nawet wtedy, gdy nie da się odczytać listy modeli.

**Definitywne i Crossplat**
- Pokazywane są teraz tylko tytuły z identyfikatorem IGDB.

## Poprawki błędów

**Kolekcja**
- Wyszukiwanie na karcie Definitywne znajdowało tylko nazwę głównego tytułu.
