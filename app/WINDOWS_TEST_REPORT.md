# Raport z testów na Windows — 2026-11-09/10

Sprawdzano, czy Ferrous Commander bezpiecznie obchodzi się z danymi na Windows 11
(NTFS, 8 wątków, 40 GB RAM), oraz wydajność i zużycie zasobów.

Wniosek: kampania znalazła i usunęła kilka błędów, które mogły zniszczyć pliki. W końcowym
kodzie żaden scenariusz nie stracił danych. Testy nie dają absolutnej pewności: sekcja
„Czego nie sprawdzono” wymienia, czego tu nie dało się uruchomić.

Testowano kod `main` w profilu release, a instalator osobno w wersji v0.1.5 z Releases.
**Zainstalowana wersja 0.1.5 nie zawiera poprawek z tego raportu.** Dostanie je dopiero
następne wydanie.

Linux: `cargo check --target x86_64-unknown-linux-gnu` przechodzi, ale to tylko kontrola
kompilacji. Testów linuksowych nie dało się tu uruchomić. Poprawki we wspólnym kodzie (limit
dekodowania obrazu w `preview/picture.rs`, ostrzeżenie o nieudanym zapisie ustawień w `app.rs`)
trzeba sprawdzić na komputerze z Linuksem.

## Środowisko

| Cel | Czym jest |
| --- | --- |
| ten sam wolumin | `C:\FerrousTest` (NTFS) |
| udział sieciowy | `\\localhost\C$\FerrousTest`, czyli SMB na tym samym komputerze |
| FAT32 | VHD 2 GB, widziany jako dysk stały |
| exFAT | VHD 8 GB, widziany jako dysk stały |
| mały NTFS | VHD 1 GB, do testu pełnego dysku |

Tryb dewelopera włączony (dowiązania). Sesja testowa działała bez podniesienia uprawnień, a
obsługa długich ścieżek w systemie była wyłączona. Testy pisały tylko w miejscach z tabeli. Kosz
dostawał wyłącznie elementy z prefiksem `ferrous-campaign-`.

## Hipotezy utraty danych

| | Hipoteza | Wynik | Jak sprawdzono |
| --- | --- | --- | --- |
| H1 | Plik za dowiązaniem do udziału ginie, a program mówi „w Koszu” | **Potwierdzona, naprawiona** | Uruchomiono. Teraz: odmowa „network locations have no Recycle Bin”. Dowiązania do D:, E:, F: trafiają do Kosza |
| H2 | Kontrola Kosza przepuszcza, gdy czegoś nie wie | **Potwierdzona, naprawiona** | Uruchomiono. Folder ponad limit z nieczytelną częścią przechodził. Teraz nieznany rozmiar lub brak skonfigurowanego limitu liczy się jak przekroczenie (brak limitu: bezpieczne 5% wolumenu, tylko test jednostkowy). Limity 102/203/818 MB działały już wcześniej. `subst` bezpieczny |
| H3 | Błąd zapisu na nośnik jest ignorowany, a źródło usuwane | **Naprawiona po przeczytaniu kodu** | Błąd zapisu nie jest już ignorowany. Ścieżki awarii nie uruchomiono, bo nie da się wstrzyknąć błędu `FlushFileBuffers`. Sonda: tylko folder na udziale zwraca kod 1 („funkcja nieobsługiwana”), który jest tolerowany |
| H4 | Przeniesienie gubi strumienie ADS na FAT/exFAT | **Potwierdzona, naprawiona** | Uruchomiono. Teraz odmowa i źródło zostaje. Udział i NTFS strumienie zachowują |
| H5 | `report.` zamieniane na `report`: akcja na sąsiednim pliku | **Potwierdzona, naprawiona** | Uruchomiono. Wycinanie, przeciąganie, otwieranie i „Otwórz za pomocą” odmawiają takich nazw |
| H6 | Kosz folderu z dzieckiem `report.` lub `nul` | Obalona | Uruchomiono. Odmowa, nic się nie zmienia |
| H7 | Zmiana źródła po weryfikacji przy otwartym uchwycie | Obalona | Uruchomiono na 4 celach. Źródło zostaje, komunikat „File changed” |
| H8 | Drugie wklejenie po wycięciu przenosi plik odtworzony pod starą ścieżką | **Potwierdzona w kodzie, naprawiona** | Po udanym wklejeniu program czyści schowek. Test 25 rund z innym właścicielem schowka |
| H9 | `destroy` przerwany w połowie, tylko do odczytu na FAT/SMB | **Potwierdzona, naprawiona** | Uruchomiono. Na udziale była odmowa dostępu. Zablokowane dziecko jest zgłaszane |
| H10 | `--uninstall` usuwa pliki użytkownika z folderu programu | **Potwierdzona w kodzie, naprawiona** | Uruchomiono po naprawie: „my notes.txt” zostaje, folder bez dodatków znika |
| H11 | Ścieżka ponad 260 znaków prowadzi do propozycji trwałego usunięcia | Obalona | Uruchomiono. Trafia do Kosza |

Inne błędy znalezione i naprawione: zmiana samej wielkości liter na FAT32 (`rename_as_asked`),
błąd kopiowania nie mówił, którego pliku dotyczy, przerwana instalacja mogła uszkodzić
istniejącą kopię programu, TIFF o wymiarach
wymagających 219 GB alokacji (teraz limit 2 GiB), nieudany zapis ustawień był niewidoczny (teraz
jednorazowy komunikat).

## Macierz operacji na plikach

Niezmiennik: żaden bajt źródła nie ginie, częściowy plik nie dostaje nazwy docelowej, błąd jest
zgłoszony, nie zostają foldery `.ferrous-copy-*`.

| Sprawdzone | Wynik |
| --- | --- |
| Kształty danych, granice 4095/4096/4097 B, plik 0 B | Poprawne na wszystkich celach |
| Nazwy: Unicode, emoji, `CON`, `nul`, `report.`, spacja na końcu | Poprawne albo jasna odmowa, także na 5 celach |
| Plik ponad 4 GiB | FAT32: odmowa. exFAT: 4,18 s |
| Pełny dysk | Błąd, źródło zostaje, brak śladów |
| Anulowanie w trakcie | Poprawne na wszystkich celach |
| Plik zablokowany, zmiana w trakcie, kolizje, folder do samego siebie | Poprawne |
| Dowiązania i junction, ścieżki ponad 260 znaków | Poprawne |
| Pliki rzadkie i skompresowane | Treść zachowana. Kopia przestaje być rzadka i zajmuje pełny rozmiar |
| Zabicie procesu w dowolnym momencie (`killed_at_any_moment`) | Dane całe |
| Chaos: 40 rund na udziale, FAT32/exFAT i NTFS | Bez utraty danych |

## Bezpieczeństwo

| Sprawdzone | Wynik |
| --- | --- |
| Archiwa: ucieczka ze ścieżki, nazwy absolutne, zastrzeżone, `a:evil.txt` | Odmowa lub pominięcie |
| Bomba 3 GiB na dysku 1 GiB | Błąd, nic nie zostaje, prawie zero dodatkowej pamięci |
| Mark of the Web | Zachowany przy rozpakowaniu i kopiowaniu |
| Fuzzing archiwów: 6 formatów po 3000 rund | 0 awarii |
| Fuzzing podglądu: 20 rodzajów plików po 3000 rund, w osobnych procesach | 0 awarii |
| `cargo audit` | 0 podatności, 2 ostrzeżenia o nieutrzymywanych zależnościach (`bincode`, `paste`) |

Tylko przeczytane w kodzie: DLL są ładowane wyłącznie z System32 (`SetDefaultDllDirectories`),
narzędzia systemowe są wołane pełną ścieżką (`system_program`), ostrzeżenie przed uruchomieniem
dotyczy tylko programów z sieci i nośników wymiennych. Ręcznie pisane wywołania COM przejrzano
przy okazji napraw, ale nie był to pełny audyt.

Znane ograniczenia:

- Program nie ma osobnej obsługi nazw ze znakiem odwrotnego kierunku tekstu (U+202E). Nazwa
  podszywająca się pod inne rozszerzenie nie jest wykrywana. Nie uruchamiano tego scenariusza.
- Hasło do RAR jest przekazywane w wierszu poleceń narzędzia, celowo, bo narzędzia czytają je
  z konsoli. Inny program tego użytkownika mógłby je odczytać w czasie działania.
- Przeniesienie między woluminami nadaje plikowi uprawnienia folderu docelowego, tak jak
  Eksplorator. Test: plik z prywatnym ACL po przeniesieniu był czytelny dla grupy Użytkownicy.
- Dowiązania twarde stają się niezależnymi kopiami.

## Wydajność

Mierzono program w profilu release, bez profilowania funkcji: WPR wymaga uprawnień administratora,
a `xperf` nie jest zainstalowany. Wartości to mediana.

| Pliki w folderze | `read_dir` | otwarcie w programie | pamięć | klatka (mediana / p99) | filtr | szukanie |
| --- | --- | --- | --- | --- | --- | --- |
| 1 000 | 1,6 ms | 15 ms | +2 MB | 0,37 / 0,72 ms | 0,8 ms | 2,8 ms |
| 10 000 | 15 ms | 53 ms | +3 MB | 0,58 / 1,17 ms | 2,7 ms | 17 ms |
| 100 000 | 147 ms | 559 ms | +22 MB | 1,22 / 1,92 ms | 21 ms | 148 ms |
| 500 000 | 768 ms | 3,5 s | +300 MB | 3,99 / 5,57 ms | 103 ms | 747 ms |

Prawdziwe okno, 5 prób: okno pojawia się po 40–54 ms (pierwsze uruchomienie 593 ms). Czas do
ustabilizowania listy to około 1 s przy folderach do 10 tys., 2 s przy 100 tys. i 4 s przy
500 tys. Mierzone próbkowaniem co sekundę, więc to górne oszacowanie. CPU w bezczynności: 0,8–1,2%
jednego rdzenia. Pamięć prywatna: 42 MB dla pustego folderu, 49 MB dla 1 tys., 90 MB dla 100 tys.,
235 MB dla 500 tys.

Kopiowanie 100 tys. małych plików:

| Cel | Ferrous Commander | robocopy |
| --- | --- | --- |
| ten sam wolumin | 77 s | 43 s |
| mały NTFS | 34 s | 28 s |
| udział sieciowy | 1453 s | 333 s |

Pomiary robocopy to pojedyncze przebiegi, więc to orientacyjne porównanie. Różnica na udziale jest
ceną świadomego zabezpieczenia: program wymusza zapis każdego pliku i odczytuje kopię z powrotem,
zanim nada jej nazwę docelową. Robocopy tego nie robi. Nie zmieniano tego. Przy tysiącach małych
plików na udziale kopiowanie będzie zauważalnie wolniejsze.

## Pamięć i zasoby

| Test | Wynik |
| --- | --- |
| 1500 cykli nawigacji, filtrowania, zakładek, kopiowania i usuwania (bez podglądu) | Pamięć 9 → 9 MB, uchwyty 191 → 191 po rozgrzewce |
| Godzina prawdziwego okna z folderem 100 tys. plików | Pamięć 88,4–89,5 MB, uchwyty 293–303, GDI 27, USER 25–27, łącznie 30 s CPU, bez awarii |
| Podgląd PNG 12000×12000 | Szczyt 562 MB, 428 ms |
| Podgląd tekstu 183 MB | Szczyt 0 MB dodatkowo, 11 ms |

## Instalacja i okno

| Scenariusz | Wynik |
| --- | --- |
| `--install` na czystym profilu | Program, skrót i wpis w rejestrze poprawne |
| Aktualizacja starszej kopii, program zamknięty | Plik podmieniony |
| Aktualizacja, gdy program działa | Błąd (kod 1), działająca kopia nietknięta, brak plików tymczasowych |
| Odinstalowanie przy otwartym oknie | Błąd (kod 1), nic nie usunięte |
| Odinstalowanie z własnym plikiem w folderze | Plik zostaje, program, skrót i klucz znikają |
| Odinstalowanie bez dodatków | Folder znika, ustawienia zostają |
| Setup.exe v0.1.5 cicho | Instalacja poprawna |
| Setup.exe, gdy program działa | Setup bez pytania zamyka program i kończy instalację. Program zapisał ustawienia przy zamknięciu |
| Deinstalator Setup z własnym plikiem | Plik zostaje, program, skrót i klucz znikają |
| `--screenshot` (Glow i wgpu) | Oba renderery rysują poprawnie |

Setup.exe nie jest podpisany cyfrowo, więc SmartScreen może go ostrzegać. Tylko przeczytane
w kodzie: `--uninstall --silent` przy instalacji z Setup uruchamia deinstalator w trybie z oknem,
więc tryb cichy nie jest wtedy naprawdę cichy.

## Czego nie sprawdzono

- Fizyczny pendrive i jego wyciągnięcie w trakcie pracy, wysuwanie nośnika z programu. VHD są
  dyskami stałymi, więc odczyt kopii z powrotem działał tu tylko na udziale. Test
  `real_mount_damaged_copy_is_never_published` na VHD kończy się komunikatem, że kontrola nie
  ruszyła, a ten sam scenariusz na udziale przechodzi.
- Prawdziwy serwer SMB i zerwanie połączenia, ReFS, prawdziwy OneDrive (pliki online i offline),
  EFS, foldery z włączoną czułością na wielkość liter.
- Skalowanie DPI, przeciąganie do i z Eksploratora, schowek z Eksploratorem w prawdziwym oknie.
  Mechanizm schowka ma test z innym programem, ale wymaga krótkiej próby ręcznej.
- Testy pod Linuksem (patrz początek raportu).

## Odstępstwa od planu

- Brak profilu z symbolami (powód w sekcji Wydajność). Mierzono program w postaci wydawanej.
- Poprawki trafiły do kilku zgrupowanych commitów zamiast wielu małych.
- Pomiary kopiowania FC i robocopy nie były powtarzane po 5 razy.

## Weryfikacja końcowego kodu

Na kodzie po wszystkich poprawkach:

- `cargo test --release campaign_ -- --ignored --test-threads=1`: 32 testy przeszły, 0 nie
  przeszło, 57 minut.
- `cargo test --locked`: 241 przeszło, 0 nie przeszło, 41 pominiętych (to testy ręczne i testy
  kampanii, uruchamiane osobno).
- `cargo check --locked --all-targets` na Windows i dla `x86_64-unknown-linux-gnu`: bez błędów
  i ostrzeżeń. Dla Linuksa to tylko kontrola kompilacji, bez linkowania i testów.

Testy schowka, zabicia procesu i chaosu uruchamiano wcześniej w trakcie kampanii. Kod, którego
dotyczą, nie zmienił się potem. Pomiary wydajności samego okna i godzinny test wykonano przed
ostatnią poprawką kontroli Kosza, która ich nie dotyczy.
