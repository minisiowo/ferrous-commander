# Raport bezpieczeństwa: Ferrous Commander na Linuksie

| | |
| --- | --- |
| Data ostatniej aktualizacji | 2026-11-06 |
| Oceniany commit | `c2ccbfa` |
| System testowy | Omarchy (Hyprland), jądro 7.2.15, rustc 1.99.1 |
| Aktualizacja | polecenie `/linux-security-report` albo prośba o aktualizację raportu |

## Ocena

**Program nadaje się do codziennej pracy, także z obcymi nośnikami i udziałami. Nie ma otwartych znalezisk
o wysokiej ani średniej wadze. Zostają cztery niskie (W3–W6).**

Najmocniejsza strona to ochrona danych: przenoszenie usuwa źródło dopiero po sprawdzeniu kopii, a próby
wyciągnięcia pendrive'a w trakcie pracy nie naruszyły źródła. Pytanie „Run this program?” patrzy teraz na
nazwę, cel dowiązania i typ pliku, czyli na to samo co system. Działa jednak tylko tam, gdzie program rozpozna
obce miejsce (W4), i tylko dla typów ze swojej listy (W2).

| Obszar | Stan | Uzasadnienie |
| --- | --- | --- |
| Utrata danych przy kopiowaniu i przenoszeniu | Dobry | Testy chaosu, wcześniejsze próby na pendrivie, `Source kept:` przy błędzie |
| Uruchamianie plików z obcych nośników | Dobry, z zastrzeżeniem | W1 i W2 naprawione, zostaje W4 |
| Znane podatności w zależnościach | Dobry | `cargo audit`: 0 zgłoszeń |
| Komunikacja sieciowa | Dobry | Brak kodu i bibliotek sieciowych w wersji linuksowej |
| Podgląd plików niezaufanych | Dobry, z zastrzeżeniem | Biblioteki w Ruście z limitem pamięci, ale awaria zamyka program (W3) |
| Uruchamianie narzędzi systemowych | Dobry | Argumenty bez powłoki, ścieżki za `--` albo bezwzględne |
| Prywatność | Dobry | Ustawienia w trybie 0600, brak telemetrii |
| Utwardzenie pliku wykonywalnego | Dobry | PIE, pełne RELRO, stos bez wykonywania |
| Wiarygodność paczek | Do rozważenia | Brak podpisów i sum kontrolnych (W6) |

## Znaleziska

Numery są stałe. Naprawione znaleziska zostają w tabeli ze statusem, żeby było widać historię.

| Nr | Waga | Status | Znalezisko |
| --- | --- | --- | --- |
| W1 | Wysoka | Naprawione w `c2ccbfa` | Dowiązanie omijało pytanie o uruchomienie |
| W2 | Średnia | Naprawione w `c2ccbfa`, z ograniczeniem | Lista rozszerzeń zamiast typu pliku |
| W3 | Niska | Otwarte | Awaria podglądu zamyka program |
| W4 | Niska | Otwarte | Niepełne rozpoznawanie obcych miejsc |
| W5 | Niska | Otwarte | Pozostałe ograniczenia ochrony danych |
| W6 | Niska | Otwarte | Paczki bez podpisów |

### W1. Dowiązanie omijało pytanie o uruchomienie (wysoka, naprawione)

`run_warning` sprawdzał nazwę dowiązania, a `xdg-open` rozpoznaje typ po jego celu. Plik `notes.txt` będący
dowiązaniem do `payload.jar` nie wywoływał pytania, a po Enter uruchamiał program. Atak działał na nośnikach
z dowiązaniami (ext4, btrfs, NFS), gdy typ miał skojarzony program, na tym komputerze Javę.

**Naprawa:** `might_run` (`app/src/app.rs:2349`) sprawdza też nazwę celu, a pytanie pokazuje `Link to ...`.

**Dowód:** `real_drive_asks` na obrazie ext4 zamontowanym przez `udisks` w `/run/media` otwiera dowiązanie
przez `App::open` i dostaje pytanie z nazwą celu. Pytanie jest też zamknięte kliknięciem w interfejsie, bez
uruchamiania pliku.

**Granice dowodu:** test działa na wirtualnym oknie, nie w oknie programu na pulpicie, a nośnik to obraz, nie
fizyczny dysk. Samo `xdg-open` nadal uruchamia taki JAR i tak ma być: ochroną jest pytanie przed tym wywołaniem.

### W2. Lista rozszerzeń zamiast typu pliku (średnia, naprawione z ograniczeniem)

`fsx::is_runnable` porównywał tylko nazwę z listą, a `xdg-mime` patrzy też na zawartość: kopia `/usr/bin/true`
nazwana `photo.jpg` ma typ `application/x-pie-executable`.

**Naprawa:** dla plików z obcych miejsc `might_run` pyta `xdg-mime` o typ. Typy wykonywalne, skrypty i
instalatory z listy `RUNNABLE_TYPES` wywołują pytanie. Typ nieustalony w 2 s liczy się jak program.

**Dowód:** ten sam `real_drive_asks` dostaje pytanie dla pliku `photo.jpg` z zawartością programu.

**Co zostaje:**

- Lista typów jest skończona. Typ spoza niej, który na danym komputerze ma program uruchamiający, nie wywoła pytania.
- Program z Wine nie był sprawdzany, bo Wine nie ma na tym komputerze. Typy plików EXE są na liście.
- Otwarcie pliku z zawieszonego udziału może zablokować okno do 2 s. To kwestia wygody, nie bezpieczeństwa.

### W3. Awaria podglądu zamyka program (niska)

Dekodowanie obrazów działa w procesie programu, a `panic = "abort"` kończy go razem z trwającą operacją.
Podgląd działa po samym zaznaczeniu pliku, gdy panel podglądu jest włączony. Pliki źródłowe zostają całe,
a kopia pośrednia może zostać na nośniku. Skutkiem jest przerwana praca, nie utrata danych.

### W4. Niepełne rozpoznawanie obcych miejsc (niska)

Wynika z odczytu `foreign_place`, nie z testu. Program może uznać za lokalne:

- sieciowe systemy plików spoza listy `is_network_fs`, np. CephFS, GlusterFS, Lustre, AFS, s3fs,
- dysk USB z LUKS lub LVM zamontowany ręcznie poza `/run/media` i `/media`,
- dysk zewnętrzny inny niż USB, zamontowany ręcznie poza tymi folderami.

Skutek to brak pytania o uruchomienie i brak odczytu kontrolnego. Zwykłe montowanie przez `udisks` trafia
do `/run/media` i jest rozpoznawane.

### W5. Pozostałe ograniczenia ochrony danych (niska)

Szczegóły są w `SECURITY_REVIEW.md`, w sekcji „Pozostaje”:

- kopie między dyskami lokalnymi nie są porównywane bajt po bajcie,
- kosz: gdy katalog główny nośnika jest niezapisywalny, element trafia do kosza domowego bez weryfikacji,
- dwa okna programu nie widzą nawzajem swoich operacji,
- błąd sterownika exFAT w jądrze jest omijany tylko w tym programie.

### W6. Paczki bez podpisów (niska)

`tar.gz` i `.deb` nie mają podpisu ani sumy kontrolnej. Przy budowaniu na własny użytek nie ma to znaczenia.
Staje się ważne, gdy program trafi do innych ludzi.

## Co działa dobrze

- **Przenoszenie:** źródło znika dopiero po `syncfs`, zrzucie pamięci podręcznej i porównaniu bajt po bajcie.
- **Wyciągnięcie nośnika:** dwie próby na FAT32 przy `f4d38de`, podczas `cp` i podczas sprawdzania kopii.
  Rozłączenie USB potwierdzone w logu jądra, 3072 MiB źródła bez zmian.
- **Kopie z nośników:** bity set-UID i set-GID są czyszczone.
- **Nazwy plików:** bez powłoki. `cp` dostaje ścieżki za `--`, a `gio` i `xdg-open` ścieżki bezwzględne.
- **Podgląd:** pomija potoki i urządzenia, także przez dowiązania. Obrazy do 200 MB i z limitem pamięci
  dekodera, tekst do ustalonej długości.
- **Otwarcie folderu:** nie czyta zawartości plików ani nie uruchamia niczego z nośnika.
- **Konfiguracja:** `settings.lua` czyta własny parser, kod nie jest wykonywany.
- **Sieć:** brak bibliotek HTTP i TLS. `tokio` jest w `Cargo.lock`, ale tylko przez ścieżkę WebAssembly.
- **Kod `unsafe` używany na Linuksie:** nieliczne wywołania systemowe i most do Wayland.

## Jak korzystać bezpiecznie

1. Nie uruchamiaj programu jako root. Program tego nie blokuje, a ochrona zakłada zwykłego użytkownika.
2. Ważne pliki kopiowane między dyskami lokalnymi sprawdzaj osobno, bo nie mają odczytu kontrolnego (W5).
3. Na rzadkich systemach sieciowych i ręcznie montowanych dyskach nie licz na pytanie o uruchomienie (W4).

## Poza zakresem

- Montowanie spreparowanego systemu plików: to kod jądra, który działa przed programem.
- Luki w programach otwierających pliki, np. przeglądarce czy odtwarzaczu.
- Złośliwe oprogramowanie już działające na koncie użytkownika. Program nie ma piaskownicy i ma pełne
  prawa użytkownika.
- Zachowanie na Windows: osobno w `SECURITY_REVIEW.md` i `WINDOWS_FILESYSTEM_AUDIT.md`.

## Co sprawdzono

| Sprawdzenie | Wynik | Rodzaj |
| --- | --- | --- |
| `cargo audit` 0.22.2 | 0 zgłoszeń, 350 paczek, 1422 wpisy | Uruchomione |
| `cargo test --locked` | 73 zaliczone, 11 ignorowanych | Uruchomione |
| `cargo check --locked --all-targets` na Linuksie | Bez błędów | Uruchomione |
| `real_drive_asks` na obrazie ext4 (`udisks`, `/run/media`) | Zaliczony, pytanie dla dowiązania i dla `photo.jpg` | Uruchomione |
| Biblioteki sieciowe w `Cargo.lock` | Brak (reqwest, hyper, ureq, h2, rustls, native-tls, openssl, curl) | Uruchomione |
| Utwardzenie pliku (`readelf`, świeży build) | PIE, RELRO, BIND_NOW, stos RW | Uruchomione |
| `xdg-open` na dowiązaniu do JAR-a | Uruchamia JAR, zgodnie z oczekiwaniem | Uruchomione |
| Przegląd zmian od `f4d38de` | Dotyczą tylko pytania o uruchomienie, testów i `xdg-mime` w „Open with” | Czytanie |
| `real_mount_pulled` na FAT32 | 2 z 2, źródło całe | Wcześniejszy wynik, `f4d38de`, przed dodaniem wymogu `Source kept:` do testu |
| OSV (`api.osv.dev`) | 0 zgłoszeń, 349 paczek | Wcześniejszy wynik, `f4d38de` |

**Czego nie sprawdzono:** testów `real_mount_*` przy tym commicie, w tym nowego wymogu `Source kept:` w
`real_mount_pulled`, kliknięcia W1 w prawdziwym oknie programu, fizycznego nośnika z dowiązaniami, Wine (W2),
fuzzingu dekoderów, X11, stanu pendrive'a po drugim wyciągnięciu. Windows nie jest częścią tego raportu.
Przegląd kodu nie był pełnym audytem każdego pliku.

## Historia

| Data | Commit | Zmiana |
| --- | --- | --- |
| 2026-11-06 | `f4d38de` | Pierwszy raport. Znaleziono W1–W6 |
| 2026-11-06 | `c2ccbfa` | Naprawiono W1 i W2. Brak otwartych znalezisk o wysokiej i średniej wadze |
