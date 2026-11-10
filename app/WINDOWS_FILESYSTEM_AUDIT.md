# Przegląd niezawodności Windows 11 — 2026-11-05

Celem była ochrona przed utratą plików. Problemy potwierdzono eksperymentem albo testem.

Wyniki pełnej kampanii testów z 2026-11-09 i aktualne liczby testów są w `WINDOWS_TEST_REPORT.md`.
Liczby testów poniżej opisują stan z dnia danego przeglądu.

| Problem przed zmianą | Skutek | Obecne zachowanie i dowód |
| --- | --- | --- |
| Kosz na lokalizacji bez Kosza: `trash` używa `FOF_NO_UI`, a Shell zwraca sukces | **Trwałe usunięcie bez pytania** i komunikat „Moved to Recycle Bin”. Potwierdzone na `\\localhost\C$` | `recycle_blocker` sprawdza typ dysku, `SHQueryRecycleBinW`, politykę, `NukeOnDelete` i `MaxCapacity`. Takie elementy zostają nietknięte, a dialog pyta o trwałe usunięcie. Test na prawdziwej ścieżce UNC. |
| Nazwy normalizowane przez Win32 (`raport.`, `nul`) | **Do Kosza trafił sąsiedni plik `raport`**. Potwierdzone eksperymentem | Operacje używają ścieżek `\\?\`. Shell ich nie rozumie, więc takie nazwy są odrzucane przy kasowaniu z informacją o zmianie nazwy; działa kopiowanie, przenoszenie i zmiana nazwy. |
| Przenoszenie między woluminami: `remove_dir_all` po kopii | Plik dodany do folderu w trakcie operacji był usuwany bez kopii; dane mogły zostać w cache zapisu | Kopia jest zrzucana na nośnik i porównywana bajt po bajcie (odczyt bez cache) jeszcze w katalogu pośrednim. Usuwane jest tylko to, co sprawdzono i co się nie zmieniło. Folder z nowymi elementami zostaje. |
| Przytrzymany `Delete`, `Ctrl+D`, `Ctrl+V`, `Shift+Delete` | Autorepeat działał na kolejnych plikach | Jedno naciśnięcie to jedna operacja. |
| Zamknięcie okna w trakcie operacji | Wątek ginął, reszta elementów po cichu zostawała nieprzetworzona | Dialog: kontynuuj albo anuluj i zamknij; okno zamyka się dopiero po zatrzymaniu operacji. |
| Błędy wyłącznie w toaście (6 s, maks. 5) | Informację o nieudanej operacji łatwo było przegapić | Okno z listą błędów zostaje do potwierdzenia, tekst można skopiować. |
| Junction w kopiowaniu | W trybie dewelopera odtwarzany jako symlink wbrew dokumentacji | Rozpoznawany po tagu reparse i zawsze odrzucany. |
| Placeholdery OneDrive traktowane jak linki | Kopiowanie folderów z OneDrive kończyło się błędem, wyszukiwanie je pomijało | Linkiem są tylko symlinki, junctiony i mount pointy. Nieprzetestowane na prawdziwym OneDrive. |
| Zapis ustawień bez `sync` i nadpisywanie nieczytelnego pliku | Pusty plik po awarii zasilania; utrata ustawień | Zapis na dysk przed podmianą; plik, którego nie da się odczytać, nie jest nadpisywany. |

Weryfikacja: `cargo test --locked --offline`: 55 testów przeszło, 1 pominięty (schowek,
uruchamiany ręcznie; zmieniono w nim tylko ścieżkę). `cargo check --tests` dla Linuksa.
Trzy celowe uszkodzenia zabezpieczeń (porównanie treści, usuwanie folderów, autorepeat)
zostały wykryte przez testy.

Nadal nieprzetestowane: fizyczny pendrive, wyciągnięcie nośnika podczas pracy, prawdziwy
OneDrive. Pełny dysk, FAT32, exFAT i Kosz ponad `MaxCapacity` sprawdzono później na dyskach VHD
(zob. `WINDOWS_TEST_REPORT.md`). Kopiowanie bez przenoszenia na dysku lokalnym nie jest
weryfikowane bajt po bajcie; na udziale sieciowym i nośniku wymiennym kopia jest odczytywana
z powrotem. Linux: zob. `SECURITY_REVIEW.md`.

# Przegląd systemu plików Windows — 2026-11-04

Przejrzano operacje w `ops.rs`, `fsx.rs`, `app.rs`, `main.rs`, `settings.rs`,
`drives.rs`, `openwith*.rs` i `install.rs`. Testy wykonano na Windowsie,
na lokalnym woluminie obsługującym strumienie NTFS, w katalogach tymczasowych.
Nie oznacza to 100% zgodności ze wszystkimi systemami plików i dostawcami Windows.

## Naprawione i sprawdzone

| Obszar                               | Zachowanie i dowód                                                                                                                                                                                                                                                                                                                           |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Tworzenie i zmiana nazw              | Walidacja nazw urządzeń DOS również przed rozszerzeniem, COM/LPT z cyframi indeksowymi, znaków sterujących, końcowych spacji/kropek i limitu 255 jednostek UTF-16. Polskie, japońskie i emoji mają testy. Istniejące pliki nie są obcinane.                                                                                                  |
| Kolizje i zmiana wielkości liter     | `MoveFileExW` bez zastępowania istniejącego celu. Test sprawdza `report.txt` → `REPORT.txt` oraz zachowanie obu plików przy kolizji. Usunięto blokującą tę zmianę kontrolę istnienia wewnątrz funkcji zmiany nazwy.                                                                                                                          |
| Kopiowanie plików                    | `CopyFileExW` zamiast kopiowania wyłącznie głównego strumienia bajtów. Test odczytuje dane i dodatkowy strumień NTFS w kopii oraz sprawdza atrybuty hidden/read-only i daty utworzenia/modyfikacji. Daty są ustawiane jawnie po kopiowaniu. Nie zezwalamy na ciche odszyfrowanie EFS.                                                        |
| Długie ścieżki                       | Natywne wywołania otrzymują absolutne ścieżki UTF-16 z prefiksem rozszerzonym, także dla UNC. Test kopiuje plik w zagnieżdżeniu przekraczającym 260 znaków i sprawdza konwersję prefiksu UNC. Nie łączy się z serwerem.                                                                                                                      |
| Kopiowanie folderów                  | Rekurencja zachowuje nazwy OS; daty i zwykłe atrybuty folderu są ustawiane po dzieciach. Test sprawdza zawartość długiej ścieżki i atrybut folderu.                                                                                                                                                                                          |
| Niepełne kopie i anulowanie          | Kopia powstaje w zarezerwowanym katalogu pośrednim i trafia pod nazwę docelową dopiero po sukcesie. Anulowanie sprawdzane przed elementami i przez natywny callback podczas transferu pliku. Test sprawdza anulowanie przed operacją i brak publikacji celu; przerwanie trwającego transferu nie ma deterministycznego testu integracyjnego. |
| Sprzątanie                           | Nieudane kopie są usuwane również przy skopiowanym atrybucie read-only. Sprzątanie nie podąża za dowiązaniami i nie zmienia atrybutów źródła. Błędy sprzątania pokazują ścieżkę pozostałości.                                                                                                                                                |
| Blokady i przenoszenie               | Test otwiera źródło z wyłączonym współdzieleniem: kopiowanie i przenoszenie zgłaszają błąd, źródło zostaje, katalog pośredni znika. Sprawdzono zwykłe przeniesienie, ten sam katalog i kolizje nazw. Błędy inne niż zmiana woluminu nie uruchamiają kopiowania z kasowaniem.                                                                 |
| Dowiązania i punkty ponownej analizy | Dowiązania symboliczne są odtwarzane przez API Windows, a nie kopiowane jako zawartość celu. Junctiony, mount pointy i pozostałe punkty reparse są odrzucane z widocznym błędem przed wejściem w ich zawartość. Test tworzy prawdziwy junction do własnego folderu, sprawdza odmowę kopiowania i zachowanie pliku docelowego.                |
| Wyszukiwanie                         | Nie wchodzi rekurencyjnie w żaden folder z atrybutem reparse. Test z cyklicznym junctionem kończy wyszukiwanie i zwraca jeden wynik.                                                                                                                                                                                                         |
| Cel wewnątrz źródła                  | Kanonikalizacja rozwiązuje aliasy; na Windows porównujemy komponenty ścieżki bez rozróżniania wielkości liter przez `CompareStringOrdinal`. Sprawdzono odmowę kopiowania do siebie i odróżnienie folderu o podobnym prefiksie. W folderach z włączoną czułością na wielkość liter kontrola może być zachowawcza.                             |
| Uruchamianie                         | Argumenty programu używają `args_os`, aby ścieżka Windows nie musiała być poprawnym UTF-8.                                                                                                                                                                                                                                                   |
| Konfiguracja i integracja            | Istniejące testy obejmują Lua/JSON, ścieżki UNC i Unicode, migrację ustawień, powłokowe dane pliku oraz listę zalecanych aplikacji. Test przeciągania grup ustawia grupy przed dynamicznie wykrywanymi dyskami, żeby odkrycie dysku nie przesuwało współrzędnych podczas testu.                                                              |

## Granice zgodności

- Kopiowanie junctionów i mount pointów jest celowo blokowane; komunikat odsyła do
  Eksploratora Windows. Placeholdery OneDrive są kopiowane jak zwykła zawartość.
  Odtwarzanie symlinków wymaga uprawnień przyznanych przez Windows / trybu dewelopera;
  przetestowano je w trybie dewelopera w kampanii z 2026-11-09.
- Nie ma edytora Windows ACL/właściciela/audytu ani edytora strumieni ADS,
  kompresji NTFS czy EFS. Kopie otrzymują uprawnienia według reguł Windows;
  nie obiecujemy zachowania właściciela, DACL/SACL ani pełnych metadanych folderów.
  Hardlinki w kopiowanym drzewie stają się niezależnymi kopiami danych. Pliki sparse
  zachowują treść, ale w kopii przestają być rzadkie i zajmują pełny rozmiar.
- Nie przetestowano ReFS, prawdziwego serwera SMB, utraty połączenia, EFS, trybu
  case-sensitive ani plików chmurowych offline. Przenoszenie między woluminami, FAT32,
  exFAT, udział `\\localhost`, pełny dysk i pliki sparse/compressed sprawdzono w kampanii
  z 2026-11-09 na dyskach VHD i udziale lokalnym.
- Kopia nie jest migawką. Równoczesna podmiana plików/dowiązań przez inny proces
  nie jest objęta gwarancją. Błędy po zakończonej kopii, ale przed usunięciem
  źródła, pozostawiają kopię i raportują problem; źródło nie jest celowo nadpisywane.
- Kosz nadal korzysta z biblioteki `trash`; nie usuwano rzeczywistych plików
  użytkownika ani nie testowano opróżniania kosza. Instalator, aktualizację
  uruchomionego EXE i odinstalowanie sprawdzono w kampanii z 2026-11-09.
- Wyświetlanie nazw i edycja/config używają tekstu Unicode. Operacje zachowują
  `PathBuf`, lecz nazwy zawierające niepoprawne sekwencje UTF-16 nie mają gwarancji
  edycji i zapisu sesji bez utraty informacji. Bardzo długi komponent z dodanym
  przyrostkiem „Copy” może przekroczyć limit woluminu i zwrócić błąd.

## Weryfikacja

`cargo test --locked --offline`: 33 testy przeszły, 0 pominiętych.
Obejmuje 6 nowych testów Windows (w tym test integracyjny operacji) oraz
dotychczasowe testy GUI, konfiguracji, podglądu i skojarzeń aplikacji.
Wyniki kompilacji wydaniowej, `rebuild_check` i nowy zrzut ekranu znajdują się
w katalogu projektu `checks`. Wygląd interfejsu nie był zmieniany w tym przeglądzie.

Dokumentacja API użyta do weryfikacji flag i granic zachowania:
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-copyfileexw
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw
