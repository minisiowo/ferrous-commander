# Raport wydajności

## 14 listopada 2026: Windows przed wydaniem po 0.2.1

Ten sam laptop co niżej, zasilanie sieciowe, plan „Zrównoważony”, build release. Kod z `main` po
v0.2.1 (42 commity) z nowym sortowaniem nazw opisanym dalej.

### Zasoby prawdziwego programu

| Pomiar | Wynik |
| --- | --- |
| Okno po uruchomieniu | 87 ms |
| Pamięć prywatna / zestaw roboczy w spoczynku | 48 MB / 91 MB |
| CPU w spoczynku | 0 ms przez 10 s, wszystkie wątki czekają |
| CPU przez pierwsze 30 s po starcie | 2,6% jednego rdzenia (dyski i ikony), potem 0 |
| Karta graficzna | Iris Xe, bez RTX: 0 MB pamięci dedykowanej, 41 MB współdzielonej, 3D 0,13% |
| Wątki / uchwyty | 18 / 304 |
| Rozmiar exe | 28,9 MB |
| Folder 100 tys. plików | gotowy po ok. 2 s, 88 MB prywatnej |
| Folder 500 tys. plików | gotowy po ok. 4 s, 234 MB prywatnej, chwilowo 493 MB w trakcie wczytywania |
| Przecieki, 1500 cykli nawigacji, kart i zadań | 11 → 11 MB, 199 → 199 uchwytów |

Te dwa foldery mierzono przed zmianą sortowania.

### Sortowanie nazw na Windowsie

Każde porównanie nazw wołało `StrCmpLogicalW`, około 250 ns na wywołanie. 500 tys. nazw to 8,4 mln
porównań, około 2 s na jednym rdzeniu i ok. 790 ms na ośmiu wątkach. Teraz każda nazwa dostaje raz
klucz z `LCMapStringEx` (`LCMAP_SORTKEY | SORT_DIGITSASNUMBERS | NORM_IGNORECASE`), a porównuje się
bajty kluczy, ok. 20 ns. W dużym folderze klucze liczy każdy wątek dla swojej części.

Kolejność się nie zmienia. Na 157 tys. nazw z folderu użytkownika, `Program Files` i `System32` nie
było żadnej niezgodności z `StrCmpLogicalW`. Test `sort_keys_keep_explorers_order` porównuje każdą
parę z ok. 70 trudnych nazw (zera wiodące, `plik (2)` i `plik (10)`, `ß`, polskie litery, szerokie
znaki, kana, emoji). `sort_keys_keep_explorers_order_on_this_disk` powtarza sprawdzenie na dysku.

| Pomiar | Przed | Po |
| --- | --- | --- |
| Sortowanie, 100 tys. | 187 ms | 46 ms |
| Sortowanie, 500 tys. | 1032 ms | 233 ms |
| Otwarcie folderu w programie, 10 tys. | 69 ms | 24 ms |
| Otwarcie folderu w programie, 100 tys. | 381 ms | 195 ms |
| Otwarcie folderu w programie, 500 tys. | 2,12 s | 1,25 s |
| Klatka bez zaznaczenia, 500 tys., mediana / p99 | 1,02 / 14,18 ms | 0,87 / 1,36 ms |

Wartości „przed” pochodzą z tego samego dnia. Różnica 148 → 187 ms przy 100 tys. względem
pomiaru z 10 listopada to rozrzut między przebiegami, bo kod sortowania był wtedy ten sam.

Filtr przy 500 tys. plików: 36 ms (10 listopada 109–226 ms). Czasy szukania z kampanii nie
zachowały się, bo PowerShell uciął długie linie logu.

### Testy na Windowsie

- Pełen zestaw: 359 zaliczonych, 0 błędów.
- Dwa testy zakładały zachowanie Linuksa i zostały poprawione. Kropka na początku nazwy nie ukrywa
  pliku na Windowsie. Podpowiedź ścieżki wychodzi poza pole przy długim folderze tymczasowym.
- Odtwarzanie dźwięku przez prawdziwy proces pomocniczy działa także na Windowsie: gra po 525 ms,
  pauza, przewijanie i koniec bez opóźnień.
- Uszkodzone pliki wszystkich podglądanych formatów, 400 rund: bez awarii i zawieszeń, 66 s.
- Kod pod Linuksa nie został sprawdzony z Windowsa, bo `alsa-sys` wymaga biblioteki ALSA.

## 10 listopada 2026: wersja 0.1.5

Stan na 10 listopada 2026, wersja 0.1.5 z poprawkami wydajności, build release.

Windows zmierzono na laptopie ASUS TUF Dash F15: i7-11370H (4 rdzenie, 8 wątków), obraz na Iris Xe,
40 GB RAM, zasilanie sieciowe. **Linux nie został jeszcze zmierzony.** Lista kontrolna jest na końcu.
Z Windowsa kod pod Linuksa został tylko sprawdzony kompilacyjnie (`cargo check`), bez linkowania i
bez uruchamiania testów.

## Obracanie i zoom modeli 3D

Model jest rysowany przez procesor w procesie pomocniczym. Karta graficzna tylko wyświetla gotowy obraz.

Prawdziwe okno, przeciąganie skryptem przez 8 s:

| Model | Wersja | Obrazy na sekundę | Oczekiwanie mediana / p95 |
| --- | --- | --- | --- |
| 1 mln trójkątów, STL | przed | 5,3 | 163 / 400 ms |
| 1 mln trójkątów, STL | po | 30,9 | 19 / 31 ms |
| 4 mln trójkątów, STL | po | 29,5 | 25 / 34 ms |
| 300 tys. trójkątów, 3MF | po | 28,0 | 24 / 34 ms |

Wersja przed poprawkami nie dała pomiaru dla 4 mln trójkątów, bo podgląd utykał na wczytywaniu
(opis niżej). Oczekiwanie liczone jest od pierwszego niezrealizowanego ruchu do pokazania kolejnego
obrazu. Czas obsługi jednego obrazu najlepiej oddaje liczba obrazów na sekundę.

Sam proces pomocniczy przez prawdziwy potok, okno 1076×1076, mediana:

| Model | Ruch przed | Ruch po | Obraz ostry przed | Obraz ostry po |
| --- | --- | --- | --- | --- |
| 100 tys. | 17,6 ms | 13,1 ms | 43,9 ms | 28,3 ms |
| 1 mln | 64,6 ms | 29,6 ms | 94,7 ms | 53,4 ms |
| 4 mln | 125,4 ms | 25,9 ms | 321,3 ms | 95,6 ms |
| 8 mln | nie mierzono | 37,9 ms | nie mierzono | 144,1 ms |

W ruchu duże modele są rysowane w mniejszym rozmiarze, dobieranym tak, by obraz powstawał w około
33 ms. Na tym laptopie model 1 mln trójkątów w ruchu miewa około 355×356 pikseli, więc jest wtedy
wyraźnie miękki. Po zatrzymaniu, po 150 ms, pojawia się pełna jakość.

Co zmieniono:
- Ramkowanie modelu liczone jest raz, a nie przy każdym obrazie. Zabierało 16 ms przy 1 mln trójkątów
  i 59 ms przy 4 mln.
- Ruch idzie bezpośrednio między oknem podglądu a procesem pomocniczym, bez klatek listy plików.
- Plik jest sprawdzany na dysku najwyżej raz na sekundę w trakcie ruchu.
- Bufory rysowania są używane ponownie.
- Trójkąty są przypisane do pasów obrazu, a pasy rozdzielane między wątki z kolejki.

Na Windowsie największe były koszty każdego pojedynczego obrazu w procesie pomocniczym. `vsync`,
karta graficzna ani przesyłanie obrazu nie okazały się główną przyczyną: wyłączenie vsync dało
6,1 zamiast 5,3 obrazu na sekundę, a tworzenie tekstury zajmuje poniżej 5 ms. Tego, ile różnicy
względem Linuksa wynika z mocniejszego procesora, a ile z systemu, nie zmierzono.

### Znaleziony błąd: podgląd utykał na wczytywaniu

Gdy wczytywanie trwało dłużej niż 200 ms albo przechodziło się strzałkami po plikach, okno podglądu
pokazywało spinner, który co klatkę prosił o następną. Na Windowsie okno listy plików przestawało
wtedy dostawać klatki, a przez nie przechodzą wynik wczytywania i klawisz Esc. Podgląd zostawał na
spinnerze, nie dawał się zamknąć klawiszem i zużywał około 37% rdzenia. Spinner zostawia teraz
przerwy między klatkami.

| Po przejściu strzałkami przez modele | Przed | Po |
| --- | --- | --- |
| CPU | 37% rdzenia | 0,6% rdzenia |
| Esc zamyka podgląd | nie | tak |
| Odwiedzone pliki (6 modeli) | zatrzymanie po 2–4 | wszystkie |

## Listy plików

Kampania `campaign_perf_selection_columns_and_sorting`, klatka bez GPU, mediana:

| Pomiar | 100 tys. przed | 100 tys. po | 500 tys. przed | 500 tys. po |
| --- | --- | --- | --- | --- |
| Sortowanie | 374 ms | 148 ms | 2129 ms | 882 ms |
| Klatka bez zaznaczenia | 0,90 ms | 0,52 ms | 3,87 ms | 0,59 ms |
| Klatka, wszystko zaznaczone | 20,33 ms | 0,54 ms | 123,69 ms | 1,02 ms |
| Klatka z kolumnami | 1,11 ms | 0,73 ms | 6,54 ms | 3,11 ms |

Otwarcie folderu w programie, po poprawkach: 100 tys. plików 277–292 ms (wcześniej 559 ms), 500 tys.
1,6–2,3 s (wcześniej 3,5 s).

Filtr przy 500 tys. plików waha się między 109 a 226 ms w kolejnych przebiegach tego samego kodu.
W poprzednim raporcie było 103 ms, co mieści się blisko dolnej granicy tego rozrzutu.

Co zmieniono:
- Widok kolumnowy i okno szukania nie kopiują całej listy w każdej klatce.
- Liczniki paska stanu dużego folderu są odświeżane co 250 ms.
- Zaznaczanie myszą przebudowuje wybór tylko po zmianie zakresu wierszy.
- Folder z setkami tysięcy plików, który się zmienia, jest czytany rzadziej: po pięciokrotności czasu
  ostatniego odczytu, najwyżej co 10 s. Odświeżenie przez użytkownika działa od razu.
- Sortowanie od 20 tys. wpisów korzysta z wielu rdzeni, z tą samą kolejnością co wcześniej.
- Stara lista dużego folderu jest zwalniana poza wątkiem interfejsu.

## Wytrzymałość i limity

| Próba | Wynik |
| --- | --- |
| 1500 obrazów modelu 1 mln trójkątów | pamięć procesu pomocniczego 130 → 132 MB |
| 80 obrazów modelu 8 mln trójkątów | 830 → 830 MB, pierwszy obraz 679 ms |
| Proces pomocniczy po zamknięciu podglądu | kończy się w ciągu 38 s |
| Pełen zestaw testów | 276 zaliczonych, 0 błędów, 44 pominięte (kampanie uruchamiane ręcznie) |

Plik blisko limitu 500 MB był sprawdzony tylko przy 400 MB (8 mln trójkątów). Limit czasu 15 s nie
został osiągnięty w żadnej próbie. Uszkodzone pliki STL i 3MF sprawdzają istniejące testy jednostkowe.

## Nie zmieniano w tym wydaniu

- Kopiowanie małych plików: 100 tys. plików zajmuje 77 s, a robocopy 43 s. Na udziałach sieciowych
  główny koszt to świadoma weryfikacja kopii.
- Linux: osobny `cp` na element i ponowne czytanie `/proc/self/mounts`.
- Paleta poleceń: ponowne szeregowanie przy nowych wynikach głębokiego szukania.
- PDF jest parsowany od nowa dla każdej strony, a duże obrazy dekodowane od nowa przy powiększaniu.
- Przebieg ostry nie jest przerywany przez nowy ruch. Trwa do około 144 ms przy 8 mln trójkątów.
- Rysowanie modelu przez GPU.

## Lista kontrolna na Linuksie

Z katalogu `app/` po `git pull`:

```sh
sh packaging/build-linux.sh
setsid -f ./dist/ferrous-commander/ferrous-commander
```

Ten sam pomiar co na Windowsie:

```sh
cargo build --release --locked
FERROUS_HELPER_EXE=target/release/ferrous-commander cargo test --release --locked model_look_baseline -- --ignored --nocapture
```

Do sprawdzenia ręcznie:

1. Obracanie i zoom modeli STL i 3MF: płynność oraz jakość po zatrzymaniu.
2. Model 8 mln trójkątów: brak błędu pamięci przy limicie 4 GB procesu pomocniczego.
   `FERROUS_LOOK_TRIANGLES=8000000 FERROUS_LOOK_ROUNDS=80` z testem `model_look_endurance`.
3. Duży plik i szybkie strzałki: podgląd nie utyka, Esc go zamyka.
4. Widok kolumnowy, szukanie, zaznaczanie myszą i kolejność sortowania w dużym folderze.
5. Regresje operacji na plikach.

Zmienna `FERROUS_PREVIEW_TRACE=1` wypisuje na stderr nazwę renderera i czasy każdego obrazu.
