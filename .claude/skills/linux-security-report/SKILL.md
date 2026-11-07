---
name: linux-security-report
description: Update app/LINUX_SECURITY_REPORT.md, the Linux security assessment of Ferrous Commander. Use when the user asks to update, refresh or rerun the security report ("zaktualizuj raport bezpieczeństwa", "odśwież raport"). Only on request; never run it after ordinary changes.
---

# Aktualizacja raportu bezpieczeństwa Linux

Raport: `app/LINUX_SECURITY_REPORT.md`, po polsku. Wykonuj na Linuksie. Na innym systemie powiedz,
że raport wymaga Linuksa, i przerwij. Nie naprawiaj znalezisk w ramach tego polecenia, chyba że użytkownik
o to poprosi.

Zasada: w raporcie stoi tylko to, co zostało sprawdzone przy tym przebiegu. Wynik z wcześniejszego commita
oznacz jako wcześniejszy. Rozróżniaj „uruchomione” od „wynika z kodu”. Nie obniżaj wagi bez dowodu.

## 1. Stan

```sh
git fetch origin && git status -sb
git log --oneline -1
grep -n 'Oceniany commit' app/LINUX_SECURITY_REPORT.md
git diff --stat <oceniany commit>..HEAD -- app/src app/Cargo.toml app/Cargo.lock app/packaging
```

Jeśli `main` jest za `origin/main`, zapytaj, czy pobrać zmiany. Jeśli drzewo jest brudne, powiedz, że wynik
dotyczy niezacommitowanego kodu.

## 2. Sprawdzenia automatyczne (z `app/`)

```sh
cargo test --locked
cargo check --locked --all-targets
```

`cargo audit` instaluj do scratchpada, nie do systemu, chyba że jest już w `PATH`:

```sh
cargo install cargo-audit --locked --root "$SCRATCHPAD/audit"
"$SCRATCHPAD/audit/bin/cargo-audit" audit
```

Jeśli instalacja jest zablokowana, powiedz o tym i zapytaj użytkownika. Nie obchodź blokady.

Sprawdź też:

- Sieć: `grep -E '^name = "(reqwest|hyper|ureq|h2|rustls|native-tls|openssl|curl)"' Cargo.lock`. Nowe wyniki opisz.
- Utwardzenie: `readelf -lW` i `readelf -dW` na `dist/ferrous-commander/ferrous-commander`, jeśli jest świeży
  względem HEAD. W przeciwnym razie napisz, że nie sprawdzono, albo najpierw zapytaj o przebudowę.

## 3. Powtórz test W1

Tylko nieszkodliwy JAR ze znacznikiem, w scratchpadzie, nigdy w repozytorium. Bez Javy zapisz „nie sprawdzono”.

```sh
E="$SCRATCHPAD/e2e"; rm -rf "$E"; mkdir -p "$E/src" "$E/stick"; cd "$E/src"
cat > Hello.java <<EOF
public class Hello { public static void main(String[] a) throws Exception { new java.io.File("$E/MARKER").createNewFile(); } }
EOF
javac Hello.java && printf 'Main-Class: Hello\n' > m.txt && jar cfm "$E/stick/payload.jar" m.txt Hello.class
cd "$E/stick" && ln -s payload.jar notes.txt
xdg-mime query filetype "$E/stick/notes.txt"
timeout 20 xdg-open "$E/stick/notes.txt"
ls "$E/MARKER"
```

Znacznik pokazuje tylko zachowanie systemu. Czy W1 jest naprawione, rozstrzyga kod `run_warning` w
`app/src/app.rs` i test sprawdzający pytanie dla dowiązania. Sam test `xdg-open` zawsze utworzy znacznik.

## 4. Przejrzyj zmiany

Dla diffu z kroku 1 sprawdź, czy kod dotyka: otwierania plików, `run_warning`, `foreign_place`, `is_runnable`,
kopiowania i przenoszenia (`ops.rs`, `linux_fs.rs`), podglądu, wywołań `Command::new`, nowych zależności,
`unsafe`, instalacji. Sprawdź każde otwarte znalezisko: czy nadal istnieje i czy numery linii są aktualne.
Przeczytaj sekcję „Pozostaje” w `app/SECURITY_REVIEW.md`, bo W5 z niej korzysta.

Jeśli podłączony jest nośnik, zapytaj, czy uruchomić testy `real_mount_*`. Przed `real_mount_pulled` przypomnij,
że wymaga wyciągnięcia nośnika, i obejrzyj jego zawartość.

## 5. Zaktualizuj raport

- Tabela na górze: data, commit, system.
- Ocena i tabela obszarów: zmień, jeśli wyniki się zmieniły.
- Znaleziska: zachowaj numery. Naprawione oznacz statusem z commitem, nowe dostają kolejny numer.
- „Jak korzystać bezpiecznie”: usuń zalecenia, które przestały obowiązywać.
- „Co sprawdzono”: tylko bieżący przebieg i uczciwa lista tego, czego nie sprawdzono.
- „Historia”: jeden nowy wiersz na końcu.

## 6. Zakończ

Commit tylko raportu i wypchnięcie na `origin/main`, zgodnie z `CLAUDE.md`. Podaj użytkownikowi ocenę,
co się zmieniło od poprzedniej wersji i czego nie sprawdzono.
