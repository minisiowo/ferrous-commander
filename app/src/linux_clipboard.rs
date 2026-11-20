//! File clipboard formats shared by Linux file managers.
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const URIS: &str = "text/uri-list";
const GNOME: &str = "x-special/gnome-copied-files";
// A cut belongs to this app. Moving requires a matching, still-pending local selection.
const LOCAL_CUT: &str = "application/x-ferrous-pending-cut";

#[derive(Clone)]
pub struct Files(Arc<Mutex<ClipboardContext>>);

pub type Selection = Result<Option<(Vec<PathBuf>, bool)>, String>;

impl Files {
    pub fn connect() -> Result<Self, String> {
        ClipboardContext::new()
            .map(|cb| Self(Arc::new(Mutex::new(cb))))
            .map_err(|e| e.to_string())
    }

    fn context(&self) -> std::sync::MutexGuard<'_, ClipboardContext> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn text(&self) -> String {
        self.context()
            .get_text()
            .unwrap_or_default()
            .replace("\r\n", "\n")
    }

    pub fn write(&self, paths: &[PathBuf], cut: bool) -> Result<(), String> {
        let list = crate::linux_drag::uri_list(paths).map_err(|e| e.to_string())?;
        let text = paths
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        let mut formats = vec![ClipboardContent::Text(text)];
        if cut {
            formats.push(ClipboardContent::Other(LOCAL_CUT.into(), list));
        } else {
            let uris = std::str::from_utf8(&list).expect("URI encoder emits ASCII");
            let gnome = format!("copy\n{}", uris.lines().collect::<Vec<_>>().join("\n"));
            formats.push(ClipboardContent::Other(GNOME.into(), gnome.into_bytes()));
            formats.push(ClipboardContent::Other(URIS.into(), list));
        }
        self.context().set(formats).map_err(|e| e.to_string())
    }

    pub fn read(&self) -> Selection {
        let cb = self.context();
        // Wayland reports an empty clipboard as an error. For a paste it just holds no files.
        let Ok(formats) = cb.available_formats() else { return Ok(None) };
        let Some(format) = [LOCAL_CUT, GNOME, URIS]
            .into_iter()
            .find(|f| formats.iter().any(|s| s == f))
        else {
            return Ok(None);
        };
        let bytes = cb.get_buffer(format).map_err(|e| e.to_string())?;
        let paths = decode(format, &bytes)?;
        Ok(Some((paths, format == LOCAL_CUT)))
    }
}

fn decode(format: &str, bytes: &[u8]) -> Result<Vec<PathBuf>, String> {
    let mut lines = bytes
        .split(|&c| c == b'\n')
        .map(|s| s.strip_suffix(b"\r").unwrap_or(s));
    if format == GNOME && !matches!(lines.next(), Some(b"copy" | b"cut")) {
        return Err("Invalid file clipboard action".into());
    }
    let mut paths = Vec::new();
    for line in lines.filter(|s| !s.is_empty() && !s.starts_with(b"#")) {
        let uri = line
            .strip_prefix(b"file://")
            .ok_or("Only local file paths can be pasted")?;
        let path = uri
            .strip_prefix(b"localhost")
            .filter(|p| p.starts_with(b"/"))
            .unwrap_or(uri);
        if !path.starts_with(b"/") {
            return Err("Only local file paths can be pasted".into());
        }
        let mut decoded = Vec::new();
        let mut bytes = path.iter().copied();
        while let Some(c) = bytes.next() {
            let c = if c == b'%' {
                let mut hex = || {
                    bytes
                        .next()
                        .and_then(|b| char::from(b).to_digit(16))
                        .ok_or("Invalid escaped file path")
                };
                (hex()? * 16 + hex()?) as u8
            } else {
                c
            };
            if c == 0 {
                return Err("Invalid file path on clipboard".into());
            }
            decoded.push(c);
        }
        paths.push(PathBuf::from(OsString::from_vec(decoded)));
    }
    if paths.is_empty() {
        return Err("Clipboard contains no local files".into());
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    #[test]
    fn reads_local_uris_without_losing_filename_bytes() {
        let expected = vec![
            PathBuf::from("/tmp/a #%.txt"),
            PathBuf::from(OsString::from_vec(b"/tmp/\xff\n".to_vec())),
        ];
        let list = b"# a comment\r\nfile:///tmp/a%20%23%25.txt\r\nfile://localhost/tmp/%FF%0A\r\n";
        assert_eq!(decode(URIS, list).unwrap(), expected);
        assert_eq!(
            decode(GNOME, &[b"copy\n", &list[..]].concat()).unwrap(),
            expected
        );
        for bad in [
            b"file://remote/tmp/a".as_slice(),
            b"https://example.org/file",
            b"file:///tmp/%0",
            b"file:///tmp/%00",
            b"/tmp/plain-text",
        ] {
            assert!(decode(URIS, &[&list[..], bad].concat()).is_err());
        }
    }

    #[test]
    #[ignore = "Uses a disposable graphical session and wl-clipboard or xclip"]
    fn exchanges_files_with_an_independent_clipboard_client() {
        assert_eq!(
            std::env::var("FC_TEST_CLIPBOARD").as_deref(),
            Ok("isolated")
        );
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let read = |mime: &str| {
            let result = if wayland {
                Command::new("wl-paste")
                    .args(["--no-newline", "--type", mime])
                    .output()
            } else {
                Command::new("xclip")
                    .args(["-selection", "clipboard", "-o", "-t", mime])
                    .output()
            }
            .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            result.stdout
        };
        let write = |mime: &str, bytes: &[u8]| {
            let mut command = if wayland {
                Command::new("wl-copy")
            } else {
                Command::new("xclip")
            };
            if wayland {
                command.args(["--type", mime]);
            } else {
                command.args(["-selection", "clipboard", "-i", "-t", mime]);
            }
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(bytes).unwrap();
            assert!(child.wait().unwrap().success());
        };
        let files = Files::connect().unwrap();
        let paths = vec![std::env::current_exe().unwrap(), std::env::temp_dir()];
        files.write(&paths, false).unwrap();
        assert_eq!(decode(URIS, &read(URIS)).unwrap(), paths);
        assert_eq!(decode(GNOME, &read(GNOME)).unwrap(), paths);
        for (format, input) in [
            (GNOME, b"copy\nfile:///tmp/a%20b".as_slice()),
            (URIS, b"file:///tmp/a%20b\r\n"),
        ] {
            write(format, input);
            assert_eq!(
                files.read().unwrap(),
                Some((vec![PathBuf::from("/tmp/a b")], false))
            );
        }
        files.write(&paths, true).unwrap();
        assert_eq!(files.read().unwrap(), Some((paths, true)));
        write(
            if wayland { "text/plain" } else { "UTF8_STRING" },
            b"/tmp/a b",
        );
        assert_eq!(files.read().unwrap(), None);
        assert_eq!(files.text(), "/tmp/a b");
    }
}
