# romexplorer

[English](README.md) · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · **Polski**

*To jest tłumaczenie. W razie rozbieżności rozstrzyga angielska wersja README.*

Menedżer kolekcji ROM-ów: skanuje foldery, identyfikuje każdy plik po hashu na podstawie
referencyjnych plików DAT, pobiera metadane i grafiki, a także konwertuje, weryfikuje i kopiuje
pliki. Działa jako serwer z konsolą w przeglądarce i webowym GUI albo jako aplikacja desktopowa,
w systemach Windows, macOS i Linux. Informacje o tym, co i gdzie zostało przetestowane, znajdziesz
w [docs/release_notes.md](docs/release_notes.md).

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
- **Kopiowanie na urządzenie.** Kopiuje wybrane gry do folderu lub na kartę, konwertując je po
  drodze, z nazwami i układem, jakich oczekuje frontend.
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

Upuść obraz płyty, a otrzymasz CHD lub RVZ. `chdlite` (szybki), `chdcomp` (najmniejszy plik),
`chdread` (rozpakowywanie) i `chdhash` (hash) to osobne narzędzia dla każdego trybu, więc plik
upuszczony na plik wykonywalny zostanie obsłużony właściwie; `chdlite-gui` robi to samo w oknie.
Każda opcja jest też dostępna jako flaga (`--help`). chdlite jest wydawany osobno: zobacz
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Klucze dostawców

Opcjonalne. Wprowadź klucze API jako administrator w Konfiguracja › Dane logowania dostawców. Są
przechowywane na serwerze w postaci zaszyfrowanej i nigdy więcej nie są wyświetlane; dostawca bez
kluczy jest po prostu niedostępny.

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
