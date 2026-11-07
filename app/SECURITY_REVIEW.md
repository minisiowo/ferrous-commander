# Przegląd bezpieczeństwa — 2026-11-05

Model zagrożeń: obcy nośnik (pendrive, spakowany folder, udział sieciowy) jest danymi
niezaufanymi, a pliki użytkownika nie mogą zginąć. Program działa z prawami użytkownika
i nie łączy się z siecią.

| Problem przed zmianą | Skutek | Obecne zachowanie i dowód |
| --- | --- | --- |
| Biblioteka `trash` podąża za dowiązaniami w `.Trash-<uid>` na nośniku | **Spreparowany nośnik kieruje „usuwany” plik do wybranego folderu poza nośnikiem**, np. `~/.config/autostart`. Potwierdzone eksperymentem na osobnym punkcie montowania | `linux_fs::trash_blocker` wykrywa dowiązanie, plik zostaje nietknięty, a dialog pyta o trwałe usunięcie. Test przez prawdziwe zadanie oraz na `/dev/shm` |
| Linux: trwałe usuwanie szło przez `trash` | Po potwierdzeniu problem by wracał | `ops::destroy` usuwa bez kosza; dowiązanie znika, cel zostaje |
| Linux: przenoszenie między dyskami kończyło się `remove_dir_all` po `cp` | Plik dodany w trakcie ginął bez kopii; nazwy `A` i `a` łączą się na FAT; źródło znikało, gdy kopia była jeszcze tylko w pamięci, więc wyciągnięcie pendrive'a traciło dane | `linux_fs::verify`: `syncfs`, zrzut pamięci podręcznej, porównanie bajt po bajcie, liczba elementów. Usuwane jest tylko to, co sprawdzono. Odpowiednik mechanizmu z Windows |
| Linux: kopiowanie kończyło się przed zapisem na nośnik | Komunikat „gotowe” przed faktycznym zapisem | Zadanie trwa do końca `syncfs` i zgłasza błąd zapisu |
| `cp --archive` zachowywał set-UID/set-GID | Kopia z obcego nośnika stawała się programem uruchamianym z prawami użytkownika | `linux_fs::drop_setid` czyści te bity w plikach zwykłych |
| Katalog pośredni dostawał `chmod` po utworzeniu | Krótka chwila z domyślnymi prawami; błąd `chmod` zostawiłby katalog `.ferrous-copy-*`. FAT32 i exFAT ignorują `chmod` bez błędu, więc na nośnikach nie wystąpił | Tryb 0700 od chwili utworzenia |
| **exFAT, jądro 7.2.15: po usunięciu świeżego folderu klaster nowego pliku zostaje wyzerowany** | **Uszkodzenie danych bez żadnego błędu.** `mkdir d; rmdir d; cp plik .; sync` odtwarza to bez aplikacji. W aplikacji kopia po nieudanym lub anulowanym kopiowaniu mogła wyjść uszkodzona. Wykryte przez porównanie bajt po bajcie na prawdziwym pendrivie | `linux_fs::settle_freed_folders` zrzuca bufory po każdym usunięciu folderu na exFAT. Test `real_mount_limits`: 5 z 5 biegów czystych, po wyłączeniu poprawki pada |
| Zwykłe kopiowanie na nośnik nie było sprawdzane | Uszkodzenie z wiersza exFAT przechodziło niezauważone | Kopie na nośniki wymienne, do sieci i na exFAT są czytane z powrotem przed nadaniem nazwy. Test `real_mount_damaged`. Plik zmieniany w trakcie takiej kopii kończy ją błędem |
| Jeden folder o nazwie spoza UTF-8 w historii lub kartach | Zapis ustawień po cichu przestawał działać, a zmiany ginęły przy zamknięciu | Takie ścieżki nie trafiają do pliku, karta przechodzi do najbliższego folderu nadrzędnego. Test |
| `settings.lua` w trybie domyślnym (zwykle 0644) | Inni użytkownicy widzieli historię folderów | Tryb 0600 |
| `decode_label` ciął tekst po bajtach | Możliwa awaria przy nietypowej etykiecie | Dekodowanie na bajtach, test |
| Programy z nośnika i sieci uruchamiały się od razu po Enter | Plik `.exe`, `.bat`, `.lnk` lub `.desktop` podłożony na pendrive'ie startował bez ostrzeżenia | Pytanie „Run this program?” tylko dla plików uruchamialnych z nośnika wymiennego i sieci, z pełną nazwą. Enter nie potwierdza, Escape nic nie robi. Dysk lokalny bez zmian. Miejsce sprawdzane w chwili otwarcia, bo lista dysków nie nadąża za świeżo włożonym nośnikiem |
| Linux: pytanie o uruchomienie patrzyło tylko na nazwę, a `xdg-open` na cel dowiązania i zawartość | **Dowiązanie `notes.txt` do pliku `.jar` na nośniku uruchomiło program po Enter, bez pytania.** Potwierdzone nieszkodliwym JAR-em. Program bez rozszerzenia z listy też nie budził pytania | Liczy się nazwa, nazwa celu dowiązania i typ z `xdg-mime`, a typ nieznany oznacza pytanie. Okno pokazuje cel dowiązania. Sprawdzane tylko na nośnikach i w sieci. Testy `a_harmless_name_does_not_hide_a_program` i `real_drive_asks` |
| Windows: narzędzia uruchamiane po samej nazwie (`reg`, `powershell`, `cmd`) | Plik `reg.exe` obok programu, np. w Pobranych, uruchomiłby się zamiast systemowego | `win::system_program` podaje pełną ścieżkę System32 |
| Windows: ścieżki w skrypcie PowerShell przy instalacji | Cudzysłów typograficzny w ścieżce kończył napis | Ścieżki w zmiennych środowiskowych |
| `read_dir(...).flatten()` | Powtarzający się błąd po wyciągnięciu nośnika mógłby zapętlić wątek (środek ostrożności, nie odtworzono) | Odczyt kończy się na pierwszym błędzie |

Sprawdzone bez zastrzeżeń: `cargo audit` (350 paczek, baza RustSec 1409 wpisów), brak sekretów
w repozytorium, brak kodu sieciowego, wstrzykiwanie przez nazwy plików (argumenty bez powłoki,
`open` na Windows używa zmiennej środowiskowej), konfiguracja bez wykonywania kodu, otwarcie
pliku na Linuksie (`.sh` i `.desktop` nie wykonują się), limit pamięci dekodera obrazów (512 MiB),
dowiązania przy kopiowaniu i usuwaniu, nieprzekazywanie urządzeń i FIFO do podglądu.

Weryfikacja: `cargo test --locked` (68 testów, 11 ignorowanych, z czego większość wymaga nośnika).
Testy nośników przeszły na obrazach exFAT, FAT32, ext4 i btrfs oraz na pendrivie USB z FAT32. Na exFAT
z pendrive'a przeszły we wcześniejszej wersji, przed ostatnimi zmianami. Pułapka z koszem: ext4 i btrfs.

Testy chaosu (`chaos_tests.rs`) budują losowe drzewa z trudnymi nazwami, dowiązaniami, FIFO i hardlinkami,
a potem anulują operacje, dopisują pliki w trakcie i zabijają proces przez SIGKILL. Sprawdzają, że każdy
wpis istnieje bajt w bajt w źródle albo w kompletnej kopii. Pierwsza wersja nie wykrywała starego kodu,
więc została przebudowana. Teraz wykrywa rekurencyjne usuwanie źródła, usuwanie edytowanych plików
i publikację niepełnej kopii. Uszkodzenie kopii wykrywają testy deterministyczne, nie chaos.

Błąd exFAT odtwarza się 12 na 12 razy na obrazie, więc dotyczy sterownika jądra, a nie nośnika.
Sprawdzono też kolejkowanie zadań na exFAT i usunięto je: nie udało się wywołać wyścigu, który miało
blokować, nawet przy wyłączonych zabezpieczeniach. Ochroną jest odczyt kopii po synchronizacji.

Wyniki z pendrive'a (ten sam nośnik sformatowany na FAT32): błąd z exFAT nie występuje (12 prób
czystych). Kolizje `Note.txt`/`note.txt`, `report`/`report.`, nazwa z końcową spacją, dowiązanie,
pełny nośnik i plik ponad 4 GiB kończą się błędem i pełnym źródłem. Pojedynczy `solo.` traci kropkę,
dane zostają. FAT nie przechowuje praw: plik 0600 staje się na nośniku 0644, `cp` nie zgłasza błędu.
Pliki sprzed 1980 r. dostają datę 1980-01-01. Komunikat o pełnym nośniku był pusty, bo log `cp` leżał
na tym nośniku; teraz idzie przez potok.

Wyniki z pendrive'a (exFAT):

- `Note.txt` i `note.txt` w jednym folderze: `cp` zgłasza błąd, kopia i przenoszenie są odrzucane,
  źródło zostaje.
- Dowiązanie w drzewie: odmowa, źródło zostaje.
- Pełny nośnik (16 MB wolne, plik 64 MB): kopia i przenoszenie zgłaszają błąd, źródło całe,
  bez pozostałości.
- Pułapka z koszem: exFAT nie ma dowiązań, więc na tym nośniku jest niewykonalna. Sprawdzona tylko
  na `/dev/shm`.

Wyciągnięcie pendrive'a w trakcie przenoszenia (FAT32, plik 3 GiB, test `real_mount_pulled`, 2026-11-06):

- Po jednym razie podczas `cp` i tuż po nim, podczas sprawdzania kopii. Rozłączenia USB potwierdzone w logu jądra.
  Oba razy błąd, a źródło zgadza się w całości.
- Test przenosił wcześniej 6 GiB, więc na FAT32 zaliczałby się bez wyciągania, bo limit 4 GiB też daje
  błąd. Teraz plik ma 3 GiB, a test wymaga komunikatu `Source kept:`.
- Komunikat twierdził, że kopia „remains at” na nośniku, którego już nie było, albo był gołym
  `Input/output error`. Teraz każdy błąd przenoszenia zaczyna się od `Source kept:`.
- Po wyciągnięciu podczas `cp` folder pośredni nie trafił na nośnik, a `fsck.fat -n` zgłosił tylko
  flagę brudnego odmontowania. Stanu po drugim wyciągnięciu nie sprawdzono.

Wysuwanie dysków (Linux: `udisksctl`, Windows: blokada i odmontowanie woluminu): odmowa w trakcie
operacji na plikach, a „można wyjąć” pada tylko po odłączeniu urządzenia. Dysk systemowy, `/home`
i udziały sieciowe nie mają tej opcji. Windows: tylko nośniki wymienne.

## Windows, 2026-11-06

Sprawdzone na Windows 11 bez nośnika USB i bez praw administratora. Drugim woluminem był
`\\localhost\C$`: to prawdziwa lokalizacja sieciowa, a dla Windows inny wolumin. Testy chaosu
działają teraz także tutaj (losowe drzewa, nazwy spoza Unicode, atrybut tylko do odczytu, hardlinki,
anulowanie, zapis w trakcie, `TerminateProcess`). Bez dowiązań symbolicznych, bo wymagają uprawnień,
których to konto nie ma. Domyślnie 8 rund; pełne 40 rund (`FERROUS_CHAOS_ROUNDS=40`, około 5 minut) przeszło.

| Problem przed zmianą | Skutek | Obecne zachowanie i dowód |
| --- | --- | --- |
| Zmiana nazwy gotowej kopii sporadycznie dawała „Access is denied” | **Poprawna kopia kończyła się błędem i była usuwana.** Windows nie zmienia nazwy folderu, gdy coś trzyma otwarty plik w środku (antywirus, indekser, cache SMB). Znalezione przez test chaosu w niecałą minutę | `ops::publish` ponawia przez 3 s. W 40 rundach 2 ponowienia, każde udane |
| `foreign_place` patrzył tylko na literę dysku | Folder montowany i dowiązanie prowadzące na nośnik omijały pytanie o uruchomienie i odczyt kopii | Sprawdzany jest też wolumin ścieżki i cel dowiązania. Test z `subst` na udział. **Sam folder montowany i dowiązanie nie zostały uruchomione**: wymagają administratora |
| Dysk USB zgłaszający się jako stały był „lokalny” | Brak pytania o uruchomienie i brak odczytu kopii | Rodzaj magistrali (USB, 1394, SD, MMC) z `IOCTL_STORAGE_QUERY_PROPERTY`. Odczyt sprawdzony na dysku systemowym. **Prawdziwy dysk USB nie był podłączony** |
| Biblioteki DLL ładowane z folderu programu | Plik DLL obok programu, np. w Pobranych, uruchamiał się wewnątrz niego | `/DEPENDENTLOADFLAG:0x800` i `SetDefaultDllDirectories`. Atak: 17 podłożonych DLL. Stary program wczytał 4 (`opengl32`, `dwmapi`, `uxtheme`, `dxgi`), nowy żadnej, a okno się rysuje |
| `open` uruchamiał `powershell.exe` po samej nazwie | Plik `powershell.exe` obok programu uruchomiłby się zamiast systemowego | `win::open` woła `ShellExecuteExW`, bez programu pośredniego. Test uruchamia prawdziwy `.cmd` |
| Lista plików uruchamialnych bez m.in. `.chm`, `.msu`, `.settingcontent-ms` | Takie pliki z nośnika startowały bez pytania | Lista uzupełniona, test |
| Nazwa z połową pary zastępczej UTF-16 w ustawieniach | Ten sam problem co spoza UTF-8 na Linuksie | Kod był wspólny, test działał tylko na Linuksie. Działa na obu |

Na Windows potwierdzone pod atakiem: plik zmieniony lub dodany po sprawdzeniu nie znika, a uszkodzona
kopia (zera w środku, ta sama długość) nie dostaje nazwy.

## Pozostaje

- Wyciągnięcie nośnika podczas pracy: na Windows nie sprawdzone. Na Linuksie nie trafiono ręcznie
  w końcowy zrzut po odczycie kontrolnym ani w samo usuwanie źródła.
- NTFS nie był sprawdzany. Zabezpieczenie po usunięciu folderu działa tylko na exFAT, a inne
  programy i system nadal mogą wywołać ten błąd jądra. Warto go zgłosić do opiekunów sterownika.
- Kopie między dyskami lokalnymi nie są porównywane bajt po bajcie.
- Kosz: gdy katalog główny nośnika jest niezapisywalny, `trash` kopiuje element do kosza domowego bez
  weryfikacji, a FIFO w folderze zawiesi taką operację.
- Dwa okna programu nie widzą nawzajem swoich operacji, także przy wysuwaniu dysku.
- Windows: wysuwanie nie było uruchomione ani razu. Otwiera wolumin do zapisu, co może wymagać praw,
  których zwykły użytkownik nie ma, i nie obejmuje dysków USB widzianych jako stałe.
  `CM_Request_Device_EjectW` robi to samo co „Bezpieczne usuwanie sprzętu”. Zmiana wymaga nośnika.
- Windows: FAT32 i exFAT nie były sprawdzane: pełny dysk, plik ponad 4 GiB, strumienie NTFS przy
  przenoszeniu, wyciągnięcie nośnika. Testy `real_mounts_*`, `real_mount_limits` i `real_mount_pulled`
  są tylko dla Linuksa.
- Windows: czy spreparowany `$RECYCLE.BIN` na zewnętrznym dysku NTFS przekierowuje kosz.
- `panic = "abort"`: błąd w dekoderze podglądu zamyka cały program wraz z trwającą operacją.
  Dane źródłowe zostają, kopia pośrednia może zostać na nośniku.
