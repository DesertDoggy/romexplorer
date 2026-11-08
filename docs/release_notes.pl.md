*To jest tłumaczenie informacji o wydaniach 0.2.0 i 0.1.8. Starsze wersje są w
[release_notes.md](release_notes.md) (po angielsku), która rozstrzyga w razie różnic.*

# romexplorer 0.2.0 (alpha)

Alfa: schemat bazy danych może się zmienić między wydaniami bez możliwości aktualizacji. Zrób kopię
zapasową przed podmianą wersji.

## Zmiany

**AI w Przeglądzie scaleń**
- „Zapytaj Google” jest teraz menu Ask: tryb AI Google, Claude, ChatGPT i Perplexity otwierają się
  w przeglądarce z wpisanym już pytaniem.
- Odpowiedź w aplikacji może pochodzić z Gemini, Claude, OpenAI lub Perplexity, każdy z własnym
  kluczem API i modelem.
- Odpowiedzi są zapisywane. „Zapytaj ponownie” pobiera nową; przypięte odpowiedzi nie są pobierane
  ponownie.
- Zapisane odpowiedzi są częścią kopii zapasowej bazy danych.

**Config › AI**
- Nowa karta z ustawieniami AI.
- Pokazuje, ile odpowiedzi zapisano i ich przybliżony koszt w tym miesiącu i łącznie. Odpowiedzi
  można eksportować i importować jako JSON.

**AI na karcie Definitywne**
- Na stronie każdej gry można zapytać AI, która wersja ma najwięcej treści oraz najlepszą grafikę,
  dźwięk i wygodę gry. Odpowiedź ma ocenę, taką jak sprawdzone notatki.
- Odpowiedź uzyskaną przez link w przeglądarce można wkleić albo zapisać ze schowka jednym
  kliknięciem.
- Przed pierwszym pytaniem pojawia się ostrzeżenie o kosztach. Config › AI zawiera linki do stron
  z limitami wydatków każdego dostawcy.
- „Oceń zbiorczo z AI” pyta o kilka gier w jednym zadaniu: o gry wybrane na karcie Definitywne albo
  o wybrane wiersze zestawu lub cały zestaw. Najpierw pokazuje liczbę pytań i przybliżony koszt,
  pomija gry, które mają już odpowiedź, i można je zaplanować dla zestawu.
- Ocena wystawiona przed wydaniem nowszej wersji gry jest oznaczona „Nowa wersja od oceny”, a filtr
  może takie oceny pominąć.

**Tytuły: wersje pomniejsze i grupy wersji**
- „Ukryj wersje pomniejsze” ukrywa grę, gdy na tej samej platformie i w tym samym regionie jest
  pełniejsze wydanie (International, Director's Cut, Final Mix i podobne; Easy Type ustępuje
  zwykłej grze). W Tytułach, Wydaniach i na liście wydań tytułu.
- „Jedna z każdego zestawu wersji” pokazuje tylko jedną wersję gier wydanych w kilku wersjach (Pokémon,
  Yo-kai Watch i inne): tę, którą masz, w przeciwnym razie wybraną przez ciebie, a jeśli nie, losową,
  z 🎲, by zobaczyć inną. Wersje z jednej grupy nie tworzą już jednej rodziny na karcie Definitywne.
- Nowa karta Wersje na stronie tytułu pokazuje oba i pozwala je poprawić.

**Config › Zaawansowane**
- Nowa karta z operacjami, które mogą uszkodzić bazę danych lub spowodować utratę plików.
  Optymalizacja DB została tu przeniesiona z Zadania i dane › DB.

**Dźwięk z ROM-ów**
- Strony tytułów mogą odtwarzać muzykę zapisaną w twoich ROM-ach (ścieżki audio CD z gier na płytach oraz archiwa dźwięku z Nintendo DS i 3DS).
- W ustawieniach wybiera się, które utwory są odtwarzane.
- Odtwarzacz na stronach tytułów ma przycisk „nie odtwarzaj tego utworu”. Ukryte utwory są pod
  listą odtwarzania: kliknięcie odtwarza utwór, a Przywróć go przywraca.
- Muzyka z ROM-ów zdekodowana do odtwarzania trafia do pamięci podręcznej w RAM (Ustawienia › Audio,
  domyślnie 256 MiB). Gdy się zapełni, utwory przechodzą do pamięci podręcznej audio na dysku, która
  używa folderu pamięci podręcznej poziomu 1, jeśli jest ustawiony.
- Dostrojono normalizację w odtwarzaczu.
- Odtwarzacz potrafi płynnie przechodzić między utworami (domyślnie 2000 ms, 0 wyłącza).
- Cisza na końcu utworu jest wycinana: utwory z ROM-ów są przycinane, a dla plików audio skan audio
  mierzy, gdzie kończy się dźwięk (uruchom go ponownie dla plików zeskanowanych wcześniej).
- Muzyka na stronach tytułów jest domyślnie włączona, a pierwsze uruchomienie pyta o nią zaraz po
  wyborze języka. Tytuł bez własnego dźwięku domyślnie odtwarza losowe utwory.

**Przegląd scaleń i Połączone tytuły**
- Wydania z karty można przeciągnąć do tytułu, do którego należą, albo na „+”, aby utworzyć nowy
  tytuł nazwany od pierwszego z nich. Działa też dotknięcie wydania, a potem tytułu.
- Karty już przyjęte lub odrzucone można znów pokazać nowym filtrem stanu. Domyślnie są ukryte.
- Podziały są teraz wymienione w Edycja › Połączone tytuły i można je tam cofnąć. Podziały z
  Przeglądu scaleń sprzed tej aktualizacji są dodawane do listy.

**Połączone wydania**
- Płyty jednej gry wymienione osobno (płyty z pakietu, płyta bonusowa) można połączyć poleceniem „Połącz w jedno wydanie”. Na liście jest jedno wydanie dla wszystkich, a skopiowanie go kopiuje wszystkie płyty. Na stronie wydania można jedno odłączyć.

**Skanowanie**
- Strona pliku ma „Przeskanuj ten plik ponownie”. Plik jest czytany ponownie z resztą swojego ROM-u,
  a jego gra od razu dopasowana do wydania.
- Strona pliku pokazuje wydanie i tytuł, z którymi plik jest powiązany.

## Poprawki błędów

- Niektóre gry nie znajdowały dopasowania w IGDB: nazwa z łącznikiem wewnątrz słowa nie pasowała do pisowni IGDB, a synchronizacja IGDB dla SNES i NES pomijała gry wydane tylko w Japonii.
- Tytuł zachowywał alternatywne tytuły z IGDB po usunięciu powiązania z IGDB i nie dało się ich usunąć. Teraz znikają razem z powiązaniem, a każdy alternatywny tytuł można usunąć przyciskiem ✕.
- „Aktualizuj metadane” ponownie wiązało z IGDB tytuł, którego powiązanie usunięto ręcznie. Teraz pozostaje bez powiązania, dopóki nie powiążesz go samodzielnie.
- Podzestawy RetroAchievements (wyzwania, speedrun itp.) trafiały do tytułu swojej gry i mogły połączyć dwie wersje gry w jeden tytuł. Każdy podzestaw jest teraz osobnym tytułem; audyt scaleń wydziela te już istniejące.
- Cofnięcie scalenia przyjętego w przeglądzie scaleń nie przywracało propozycji. Teraz wraca do przeglądu.
- Propozycja scalenia zostawała w przeglądzie scaleń po ręcznym scaleniu jej tytułów.
- Gry pirackie i nielicencjonowane były scalane z tytułami gier komercyjnych.
- Karta „inna gra w środku” zostawiała w tytule pirackie i nielicencjonowane gry z innych regionów.
- Karta „inna gra w środku” traktowała wydanie z numerem przed nazwą („0743 - ...”) jako inną grę.
- Zadania takie jak audyt scaleń pokazywały niewiele ze swojego postępu. Teraz pokazują go
  szczegółowo.
- Plik wymieniony w DAT mógł zostać bez wydania i tytułu, gdy DAT dodano po skanowaniu albo podfolder
  zeskanowano po folderze nad nim.

# romexplorer 0.1.8 (alpha)

Alfa: schemat bazy danych może się zmienić między wydaniami bez możliwości aktualizacji. Zrób kopię
zapasową przed podmianą wersji.

## Zmiany

**Media**
- Po rozpoczęciu od nowa z nową bazą danych grafiki, które są już w folderze mediów, są używane
  zamiast pobierania ich ponownie.

**Zadania**
- Dostosowano kolumny szczegółów na liście zadań.
- Oczekujące zadanie ma przycisk „Uruchom jako następne”, także dla zaznaczenia: rusza, gdy tylko skończy się
  zadanie działające w jego torze. Inne tory pozostają bez zmian.

**Dopasowanie po hashu**
- Dopasowanie po hashu z włączonym tylko Hasheous pobiera też dane IGDB i grafiki dla tego, co
  dopasowało, tak jak z PlayMatch.
- Ustawienie serial_match decyduje dla każdej platformy, czy liczy się numer seryjny w nazwie pliku.
  Platforma pozostawiona bez zmian korzysta z pola wyboru nazwy pliku.

**Przegląd scaleń**
- Odpowiedź Gemini i „Zapytaj Google” przychodzą w języku interfejsu.
- Gdy zapytanie się nie powiedzie, komunikat mówi dlaczego: certyfikat lub przekroczony czas, model
  i wyczerpany limit. Klucz bez darmowego limitu dla modelu to zgłasza.
- Jeśli listy modeli nie da się odczytać, Gemini i tak jest pytany; przy błędzie limitu raz
  próbowany jest następny model Flash.

**Definitywne i Crossplat**
- Wyświetlane są tylko tytuły z identyfikatorem IGDB. Tytuły Wii U nazwane swoim identyfikatorem
  tytułu nie pojawiają się już w Definitywne.

## Poprawki błędów

**Kolekcja**
- Wyszukiwanie na karcie Definitywne znajdowało tylko nazwę pierwszego tytułu. Teraz znajduje
  rodzinę po nazwie dowolnej wersji, także po nazwach zlokalizowanych i alternatywnych.
