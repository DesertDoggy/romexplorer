# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · **Polski**

*To jest tłumaczenie. W razie rozbieżności rozstrzyga angielska wersja README.*

> **romexplorer nie rozpowszechnia ani nie pobiera ROM-ów, BIOS-ów ani kluczy i nie wspiera
> piractwa.** Używaj go z własnymi zrzutami gier, które posiadasz.

Menedżer kolekcji ROM-ów: skanuje foldery, identyfikuje każdy plik po hashu na podstawie
referencyjnych plików DAT, pobiera metadane i grafiki, a także konwertuje, weryfikuje i kopiuje
pliki. Działa jako serwer z konsolą w przeglądarce i webowym GUI albo jako aplikacja desktopowa,
w systemach Windows, macOS i Linux. Informacje o tym, co i gdzie zostało przetestowane, znajdziesz
w [docs/release_notes.pl.md](docs/release_notes.pl.md).

## Co robi

- **Skanowanie i identyfikacja.** Przegląda katalog, rozpoznaje platformę każdego pliku na
  podstawie jego zawartości, oblicza hashe (CRC32, MD5, SHA-1, SHA-256, XXH3, RetroAchievements) i
  dopasowuje plik do katalogu – najpierw po hashu, potem po nazwie.
- **Odczyt wewnątrz kontenerów.** CHD, RVZ/WIA/GCZ, zip i 7z są dekodowane strumieniowo, więc obraz
  płyty jest hashowany i identyfikowany bez wcześniejszego rozpakowywania na dysk.
- **Konwersja i ponowne pakowanie.** Tworzenie i rozpakowywanie CHD (CD, GD-ROM, DVD) oraz RVZ; zip
  i 7z; dodawanie lub usuwanie nagłówków NES/SNES; odtwarzanie brakujących plików `.cue`, `.gdi`
  lub `.m3u`.
- **Katalog.** Importuje pliki DAT z Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements i
  RetroBIOS do dołączonej bazy MariaDB oraz scala regionalne wydania i tytuły.
- **Metadane i grafiki.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady i RetroAchievements, każdy z osobnym limitem zapytań.
- **Kopiowanie na urządzenie.** Kopiuje gry i ich grafiki z serwera lub z tego komputera do
  folderu lub na kartę, konwertując je po drodze, z nazwami i układem, jakich oczekuje frontend;
  grafiki są układane dla RetroArch lub PCSX2 / DuckStation.
- **Weryfikacja i kopie zapasowe.** Sprawdzanie integralności względem zapisanych hashy; kopie
  zapasowe bazy danych i katalogu w formacie zstd; przywracanie.
- **Współdzielenie między komputerami.** Klienci i peery mogą tworzyć lustrzane kopie
  katalogu i grafik z serwera głównego; eksport do SQLite zasila telefon.

Wszystko jest dostępne przez REST API (`/api/v1/...`), konsolę w przeglądarce (`/`) i webowe GUI
(`/app/`).

## Pierwsza konfiguracja

Dostępne są trzy pakiety. Wybierz jeden:

| pakiet | co zawiera | instalacja |
|---|---|---|
| **server** | serwer, proces roboczy, dołączona baza MariaDB, webowe GUI | uruchom instalator |
| **desktop** | aplikacja z GUI i własną bazą MariaDB | wystarczy ją uruchomić |
| **cli** | chdlite: samodzielne narzędzia do konwersji / rozpakowywania / hashowania, bez bazy danych | wystarczy go uruchomić |

### Serwer

1. Rozpakuj pakiet.
2. Opcjonalnie: skopiuj `romexplorer.conf.example` jako `romexplorer.conf` i zmodyfikuj go.
   Wszystkie wartości są opcjonalne; puste hasło zostanie wygenerowane. Ten plik jest odczytywany
   tylko przy pierwszej instalacji.
3. Uruchom instalator:
   - Linux / macOS: `./install.sh` -- uruchomiony jako root instaluje do `/opt/romexplorer` jako
     usługę systemową; jako zwykły użytkownik – do `~/.local/opt/romexplorer` jako usługę
     użytkownika.
   - Windows: `.\install.ps1` w PowerShellu uruchomionym jako administrator.
4. Pierwsze uruchomienie tworzy bazę danych, jej konta i login administratora, a następnie
   wyświetla dane logowania do konsoli **tylko raz**. Zachowaj je.
5. Otwórz `http://127.0.0.1:7337/` i zaloguj się.

Ustawienia domyślne: serwer nasłuchuje na `127.0.0.1:7337`, a dołączona baza MariaDB na `3307`.
Ustaw `server_bind=0.0.0.0`, aby udostępniać usługę w sieci lokalnej; inne komputery łączą się
wtedy przez TLS z certyfikatem, który serwer tworzy przy pierwszym uruchomieniu.

Następnie:

- Zmień hasło administratora w konsoli lub poleceniem `romexplorer_server reset-admin`.
- Sprawdź instalację poleceniem `romexplorer_dbtool check`: wyświetla znalezione biblioteki
  natywne i bazę danych.
- Odinstaluj poleceniem `./install.sh --uninstall` (dane zostają) lub `--uninstall --purge`.

### Aktualizacja serwera

Uruchom instalator z nowego wydania na istniejącej instalacji. Wykrywa on bazę danych, zatrzymuje
usługę, podmienia pliki wykonywalne, `lib/` i dostarczane zasoby, po czym uruchamia ją ponownie.
Baza danych, ustawienia, dane logowania, media i wszystko inne w `data/` i `assets/`
pozostają bez zmian.

- Linux / macOS, bezpośrednio z najnowszego wydania:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- Możesz też rozpakować nowy pakiet i uruchomić jego `./install.sh` albo wskazać pakiet już
  zainstalowanemu instalatorowi:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: rozpakuj nowy pakiet i uruchom jego `.\install.ps1` w PowerShellu uruchomionym jako
  administrator albo użyj `.\install.ps1 -From <package>.zip`.

Uruchom go jako ten sam użytkownik co przy pierwszej instalacji (root lub nie) i podaj ten sam
`--prefix` / `-Prefix`, jeśli go wybrano. Dopóki romexplorer jest w wersji alfa, schemat bazy
danych może się zmieniać między wydaniami, dlatego najpierw zrób kopię zapasową
(Konfiguracja › Kopia zapasowa).

### Aplikacja desktopowa

Rozpakuj i uruchom `romexplorer` (w macOS `romexplorer.app`; aplikacja nie jest notaryzowana, więc
przy pierwszym uruchomieniu użyj prawego przycisku myszy > Otwórz). Pierwsze uruchomienie tworzy i
uruchamia własną bazę danych.

### Narzędzia wiersza poleceń (chdlite)

Upuść obraz płyty, a otrzymasz CHD lub RVZ. `chdlite` (szybki) i `chdcomp` (najmniejszy plik)
działają w obie strony: nieskompresowany obraz jest kompresowany, a plik `.chd` lub `.rvz`
rozpakowywany. `chdread` pokazuje metadane pliku, a `chdhash` liczy jego hash. Każde narzędzie
robi jedną rzecz, więc plik upuszczony na plik wykonywalny zostanie obsłużony właściwie;
`chdlite-gui` robi to samo w oknie. Każda opcja jest też dostępna jako flaga (`--help`). chdlite
jest wydawany osobno: zobacz [docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Narzędzie wiersza poleceń (rommusic)

Upuść na `rommusic` ROM, obraz płyty, zrzucony folder lub zestaw ripów muzyki, a zapisze muzykę w
tle gry jako FLAC w folderze obok. Każda opcja jest też dostępna jako flaga (`--help`). Obsługiwane
systemy są wymienione w [Dźwięk z ROM-ów](#dźwięk-z-rom-ów). rommusic jest wydawany osobno: zobacz
[docs/rommusic_release_notes.md](docs/rommusic_release_notes.md).

### Klucze dostawców

Opcjonalne. Wprowadź klucze API jako administrator w Konfiguracja › Dane logowania dostawców.
Dostawca bez kluczy jest po prostu niedostępny.

### Pierwsze kroki

1. Dodaj katalog i wskaż, jakiej platformy dotyczy, albo pozostaw to wykrywaniu.
2. Uruchom synchronizację DAT, aby mieć katalog do dopasowywania.
3. Uruchom skanowanie plików w tym katalogu.
4. Uruchom dopasowanie po hashu, a następnie zapytania do dostawców o metadane i grafiki.

Każdy z tych kroków to zadanie; zobacz niżej.

### Ze źródeł

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` zgłasza brakujące narzędzia bez budowania. W macOS wymaga `bash`,
`bison` i `ninja` z Homebrew.

## Zadania

Wszystko, co trwa długo, działa jako **zadanie** w tle, więc trwa dalej nawet bez otwartej
przeglądarki. Każde zadanie pokazuje postęp, można je anulować i zaplanować (karta Zaplanowane
zadania) tak, aby powtarzało się co określoną liczbę minut, godzin, dni, tygodni lub miesięcy.
Najważniejsze:

- **Synchronizacja DAT** -- pobiera i importuje referencyjne pliki DAT (Redump, No-Intro,
  TOSEC, ...).
- **Pobieranie metadanych i mediów od dostawców** -- pobiera informacje o grach i grafiki z
  IGDB, ScreenScraper, SteamGridDB i pozostałych.
- **Skanowanie plików** -- przegląda katalog, oblicza hashe jego plików i dopasowuje je do
  katalogu.
- **Sprawdzanie integralności** -- ponownie odczytuje posiadane pliki i porównuje je z zapisanymi
  hashami.
- **Sprawdzanie i tworzenie arkuszy / playlist** -- sprawdza, czy pliki `.cue`, `.gdi` i `.m3u`
  nadal wskazują prawidłowe pliki, i odtwarza brakujące.

## Skanowanie i dopasowanie po hashu

**Skanowanie plików** przechodzi przez folder i liczy hash każdego pliku (CRC32, MD5, SHA-1,
SHA-256, XXH3 i hash RA). Archiwa (CHD, RVZ/WIA/GCZ, zip, 7z) są hashowane od środka. Po
zakończeniu skanowania do kolejki trafia dopasowanie po hashu tego samego folderu z polami,
które zaznaczysz. Gdy wszystkie pola są odznaczone, żadne dopasowanie nie jest uruchamiane.

| pole | co robi |
|---|---|
| Dopasowanie DAT | Łączy każdą grę z wydaniem z DAT, którego hashe pasują do jej plików (CHD także po ścieżkach), a potem po numerze seryjnym to, czego nie znalazł żaden hash. Offline. |
| RetroAchievements | Łączy gry z grami RetroAchievements po ich hashu RA, na podstawie już zsynchronizowanych danych RA. Offline, zawsze włączone. |
| Hasheous | Szuka każdego pliku po hashu w hasheous.org (bez klucza). Zwraca wydanie i jego identyfikatory u innych dostawców. |
| PlayMatch | Szuka plików, które wciąż nie mają identyfikatora IGDB (bez klucza), 100 na zapytanie. Zwraca ten sam rodzaj identyfikatorów. |

Hasheous i PlayMatch łączą się z internetem tylko wtedy, gdy danych nie ma w lokalnej pamięci
podręcznej.

**Dostawcy, do których prowadzą Hasheous i PlayMatch:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper i MobyGames (dane i grafiki, według znalezionych identyfikatorów),
a także wyszukiwanie po nazwie w IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady
i ScreenScraper dla tytułów, które wciąż nie mają powiązania. Dostawcy bez kluczy są pomijani.

## Kopiowanie

**Kopiuj gry i ich grafiki do innego miejsca.** W trybie klienta desktopowego możesz też
kopiować z serwera głównego: dodaj w ustawieniach klienta katalog zamontowany sieciowo
(SMB, NFS, ...) oraz katalog główny kolekcji serwera.

**Możesz skopiować set albo pojedyncze tytuły, wydania, Crossplaty i Defy.**

Tryby kopiowania (wybierane przy kopiowaniu):

| tryb | do czego | zasady |
|---|---|---|
| Do tej kolekcji | serwer → folder kolekcji na tym komputerze | pomija to, co ten komputer już ma; kopie od razu liczą się jako posiadane tutaj |
| Na kartę SD / USB / do folderu telefonu | dowolny folder poza kolekcją | nic nie jest pomijane z powodu posiadania gdzie indziej; nic nie jest zapisywane |
| W obrębie tej kolekcji | konwersja na miejscu (ISO → RVZ, ...) | zachowuje obie kopie |
| W obrębie kolekcji serwera | z interfejsu samego serwera | każdy plik weryfikowany; nigdy nie nadpisuje |
| Wysyłanie | ten komputer → serwer | tylko to, czego brakuje serwerowi; nigdy nie nadpisuje; serwer skanuje to ponownie |

- Kopie trafiają do podfolderu dla każdej platformy. Styl nazw podfolderów wybierasz przy
  kopiowaniu.
- Pliki można konwertować w trakcie kopiowania, aby je zarchiwizować i oszczędzić miejsce, albo
  do innego formatu, którego wymaga urządzenie.
- Jeśli plik docelowy już istnieje, jest pomijany.

**Grafiki można też eksportować.** Zaznacz „eksportuj grafiki potem” w oknie kopiowania albo
użyj osobno **eksportuj grafiki...**. Pliki obrazów są przemianowywane i układane dla emulatorów:

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, układ libretro-thumbnails.
- **PCSX2 / DuckStation:** jeden folder covers, `<serial>.<ext>` albo nazwa według pliku gry.

To, obraz którego dostawcy wypełnia dane miejsce, wynika z `media_export_priority` (najpierw
prawdziwe skany pudełek).

## Karta Kolekcja

Kolekcja to baza danych wszystkich gier, które posiadasz, oraz innych gier istniejących w bazach
takich jak Redump, No-Intro, TOSEC i IGDB (wymaga pobrania DAT-ów / metadanych).

| podkarta | jeden wiersz na |
|---|---|
| Tytuły\* | Tytuł to jedna gra na jednej platformie. |
| Wydania\*\* | Wydanie to jedna gra, w której każdy region, język, rewizja, edycja, demo itd. liczy się osobno. |
| Crossplaty\*\*\* | Crossplat łączy wszystkie Tytuły tej samej gry na różnych platformach. |
| Defy\*\*\*\* | Def służy do wyboru jednej wersji gry spośród wszystkich jej wersji (rodziny gry): oryginału oraz jego remasterów, portów, remake'ów, edycji GOTY itd. Jeden wiersz na rodzinę gry. |
| Gry | Gra to kompletny (lub niekompletny) zestaw romów jednej gry. Jeśli masz kilka kopii, każda liczy się jako jedna Gra. Gra może zawierać kilka romów (Disc 1, Disc 2, ...). |
| Romy | Rom to pojedyncza jednostka nośnika gry. Osobne płyty liczą się jako osobne romy. Rom może zawierać kilka plików (Disc 1 = cue + track 1.bin + track 2.bin). |
| Pliki | Pojedyncze pliki na dysku. |
| Zawartości | Zawartość to ciąg bajtów rozpoznawany po hashu. Może to być plik albo pojedynczy wpis w archiwum. Zawartość może też zawierać zawartości. |
| Sety | Set to twoja własna uporządkowana lista Tytułów / Wydań / Crossplatów / Defów. |
| Multimedia, Dokumenty, Kod źródłowy, Inne | Płyty wideo i muzyczne; instrukcje, czasopisma i skany; kod źródłowy; narzędzia, trainery, zapisy gier. |
| Łatki | Łatki zgodności RetroAchievements (z pobieraniem), łatki wymienione w DAT-ach, twoje foldery z łatkami. |
| BIOS / firmware | Pliki systemowe urządzenia: BIOS, firmware, obrazy NAND i kart SD. |
| Audio | Twoje pliki audio w katalogu audio. Można je (podobnie jak tytuły) otagować, aby ścieżka dźwiękowa grała podczas oglądania strony Tytułu. |

\* Ta sama gra na innej platformie (Hoge Quest (PS2), Hoge Quest (Dreamcast)) liczy się jako
osobny Tytuł. Różne rewizje (Hoge Quest, Hoge Quest (Rev A)), regiony (Hoge Quest (Japan),
Adventure of Hoge (USA)) lub języki (Hoge Quest (ja), Hoge Quest (fr)) są łączone w ten sam
Tytuł.

\*\* Jeśli przy kopiowaniu znaleziono kilka kopii tego samego wydania, wygrywa kompletna (bez
brakujących plików), a potem priorytet z ustawień (z nagłówkiem > bez nagłówka, z64 > n64 itd.).
Przy kopiowaniu kopiowane jest jedno wydanie z serwera lub klienta, wybrane według twoich
priorytetów regionu / języka / edycji (zwykła, budżetowa, specjalna). Wydania, których nie
posiadasz, są pomijane na rzecz kolejnego priorytetu.

\*\*\* Hoge Quest obejmuje Hoge Quest (PS2) i Hoge Quest (Dreamcast).

\*\*\*\* Na wypadek, gdy chcesz grać tylko w najnowszą wersję albo w tę z największą ilością
grywalnej zawartości. Domyślnie Def wybiera najnowszy Crossplat, ale niektóre Defy mają
uszeregowane notatki dla przypadków, w których najnowszemu Crossplatowi brakuje zawartości
z wcześniejszych wersji (np. w Hoge Quest 3D Remake (PC) brakuje dodatkowego lochu z Hoge Quest
(GBA)). Możesz przeciągnąć wersje w preferowanej kolejności. Ponieważ wersja ostateczna może
istnieć tylko na obecnych platformach, możesz ustawić kopiowanie Crossplatu / Tytułu
o najwyższym priorytecie w twojej kolekcji albo niekopiowanie niczego, gdy najbardziej
ostatecznej wersji nie ma w kolekcji (czyli: zagraj w nią na Steamie).

Uwaga: Crossplaty i Defy są budowane z metadanych IGDB, więc trzeba je pobrać. Ustawienie może
automatycznie pobierać powiązane dane (domyślnie wyłączone, bo duża baza danych spowalnia
wszystko).

## Dźwięk z ROM-ów

Strony tytułów mogą odtwarzać muzykę zapisaną w twoich ROM-ach prosto z pliku: nic nie jest
rozpakowywane ani kopiowane. Najpierw uruchom „Indeksuj dźwięk z ROM-ów” na karcie Audio. `rommusic` czyta
to samo.

| muzyka z | platformy | pliki |
|---|---|---|
| Ścieżki audio CD | Każda gra na CD: PC Engine CD, Mega-CD, Saturn, PlayStation, Dreamcast i inne | CHD, cue/bin, gdi |
| Pliki muzyczne na płycie | PlayStation, PlayStation 2, PSP, Saturn, Dreamcast, GameCube, Wii, Xbox, Xbox 360 | CHD, cue/bin, gdi, ISO, CSO/ZSO, RVZ/WIA/GCZ/WBFS, XISO |
| | Wii U | .wua; .wud/.wux i pobrane tytuły z kluczem obok |
| | PS3, PS Vita | odszyfrowane zrzuty folderów |
| Archiwum dźwięku | Nintendo DS (muzyka strumieniowa i sekwencyjna), Nintendo 3DS (tylko odszyfrowane obrazy) | .nds, .3ds/.cci |
| Silnik dźwięku gry | Game Boy Advance: gry zrobione na standardowym silniku Nintendo (około połowy) | .gba |
| Ripy muzyki | NES i Famicom Disk System (NSF, NSFE), SNES (SPC), Game Boy (GBS), PC Engine (HES), MSX (KSS), Master System i Game Gear (SGC, VGM), Mega Drive (VGM, GYM), ZX Spectrum i Amstrad CPC (AY), Atari 8-bit (SAP), Nintendo 64 (USF), PlayStation (PSF), PlayStation 2 (PSF2), Saturn (SSF), Dreamcast (DSF), Game Boy Advance (GSF), WonderSwan (WSR), Neo Geo Pocket i Virtual Boy (VGM) | w folderze gry lub w bibliotece ripów, luzem lub w zip |

- Biblioteka ripów (Ustawienia › Audio) dopasowuje zestawy NSF, NSFE, GBS, SPC, HES, KSS i SGC do
  twoich ROM-ów po zawartości: jeden zestaw gra dla każdego regionu gry, niezależnie od nazwy.
- Ripy z PlayStation i PlayStation 2 wymagają BIOS-u PS2 w twojej kolekcji; bez niego są
  pomijane.
- Obrazy płyt w zip lub 7z nie są jeszcze czytane. Zaszyfrowane obrazy nie są czytane.
- `rommusic` tworzy też muzykę tytułową gry na SNES bez ripu.

## Karta RetroAchievements

Twoje konto RetroAchievements i gry z osiągnięciami, według ostatniej synchronizacji konta
(wymaga `ra_username` i `ra_api_key`). Blok konta u góry pokazuje twój profil i postępy. Strona
gry wymienia jej osiągnięcia i łatki zgodności, z przyciskiem pobierania.

Jeśli kopiujesz Tytuł lub dodajesz go do Setu z karty RA, wybierane są **tylko** wydania
z osiągnięciami. (Np. jeśli Hoge Quest (USA) ma osiągnięcia, a Hoge Quest (Japan) nie,
kopiowany jest Hoge Quest (USA), nawet gdy twój priorytet regionów to Japan > USA.)

| podkarta | pokazuje |
|---|---|
| Tytuły | Tytuły z osiągnięciami RetroAchievements. |
| Mistrzostwo | Tytuły, w których odblokowałeś wszystkie osiągnięcia w trybie hardcore. |
| Ukończone | Tytuły, w których odblokowałeś wszystkie osiągnięcia w trybie softcore. |
| Pokonane | Gry, które przeszedłeś, ale nie ukończyłeś. |
| W toku | Rozpoczęte, ale bez mistrzostwa i nieukończone (także pokonane, chyba że ustawienie `ra_include_beaten` jest wyłączone). |
| Do zagrania | Tytuły z osiągnięciami, których jeszcze nie zacząłeś. |

## Kod zewnętrzny

romexplorer opiera się na tych projektach. Większość jest używana jako biblioteki, przypięte jako
submoduły w `external/`. „Z łatką” oznacza, że podczas budowania zmienia się kilka linii kodu
samego projektu; „wrapper” oznacza, że dodajemy wokół niego własną małą bibliotekę, nie zmieniając
jego kodu. Jedno i drugie znajduje się w `deps/<name>/`, nigdy w submodule. Dziękujemy im
wszystkim.

| projekt | do czego służy | nasze zmiany |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | z łatką, wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | tworzenie / rozpakowywanie CHD | z łatką, wrapper |
| [MAME](https://github.com/mamedev/mame) | źródła, z których budowany jest chdman-simd | brak |
| [libchdr](https://github.com/rtissera/libchdr) | jego dekoder LZMA; wzorzec dla czytnika CHD | brak |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | zapis .wua / .zar | brak |
| [zstd](https://github.com/facebook/zstd) | kompresja | brak |
| [fmt](https://github.com/fmtlib/fmt) | używany przez kod Cemu | brak |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | hashe RetroAchievements | z łatką, wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | dołączona baza danych | plik budowania z łatką |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | klient bazy danych | brak |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | brak |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | brak |
| [libxaac](https://github.com/ittiam-systems/libxaac) | dźwięk AAC | wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | klienci dostawców; przeniesione dopasowywanie nazw | kod skopiowany i zmodyfikowany |
| [RomM](https://github.com/rommapp/romm) | wzorzec, ikony platform | nieużywany jako kod |
| [RAChecker](https://github.com/x3kim/RAChecker) | wzorzec dla klienta RetroAchievements | nieużywany jako kod |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | okładki PS1 / PS2, według numeru seryjnego | pobierane w razie potrzeby, nie dołączane |

Szczegóły budowania, przypięte wersje i łatki opisano w [deps/README.md](deps/README.md). Licencja
każdego projektu jest dołączona do plików binarnych w `lib/LICENSES/`.
