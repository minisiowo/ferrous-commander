# Project rules

The app lives in `app/` (Ferrous Commander, Rust). It is developed on two
computers, one Windows and one Linux, and both pull from `main`.

## Everything goes on `main`

- Work and commit directly on `main`. Do not create branches.
- Push finished work to `origin/main`, so the other computer can pull it.

## `main` must build on both Windows and Linux

A commit that builds on only one system breaks the other computer. Before every
commit that touches code, check both platforms from `app/`:

```sh
cargo check --locked --all-targets                                # the current system
cargo check --locked --all-targets --target x86_64-pc-windows-msvc  # from Linux
cargo check --locked --all-targets --target x86_64-unknown-linux-gnu # from Windows
cargo test --locked
```

- Fix any failure before committing, on either platform.
- If the other platform cannot be checked on this computer, say so plainly. Do
  not report the work as verified on that platform.
- A cross-target `cargo check` type-checks the other platform's code, but it does
  not link, run tests or run that platform's branch of `build.rs`. Report it as
  a compile check, not a full build.
- Be careful around `#[cfg(windows)]` and `#[cfg(target_os = "linux")]`. When
  removing a field or item, remove its `cfg` attribute too, because a leftover
  attribute silently applies to the next item and breaks the other platform.

## Linux security report

`app/LINUX_SECURITY_REPORT.md` is the Linux security assessment. Update it only
when the user asks, by following `.claude/skills/linux-security-report/SKILL.md`
(`/linux-security-report`).

## Releases

`app/Cargo.toml` holds the one version number. To publish a release, on `main`:

1. Set `version` in `app/Cargo.toml`, then run `cargo check` so `Cargo.lock` follows.
2. Commit and push.
3. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

The tag starts `.github/workflows/release.yml`, which tests and builds on Linux and
Windows and publishes a pre-release. A tag that differs from `Cargo.toml` fails.
Pushing a tag publishes to testers, so only do it when the user asks for a release.

## Packaging

Full packaging builds: `sh packaging/build-linux.sh` on Linux and
`powershell -File packaging\build-windows.ps1` on Windows.

On Linux, always launch the app for the user after a successful build, detached
so it keeps running: `setsid -f ./dist/ferrous-commander/ferrous-commander`
from `app/`. Confirm it is running before reporting the build as done.
