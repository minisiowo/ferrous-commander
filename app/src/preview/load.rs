//! Turns a file into something to show. Runs inside the helper process.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fsx;

/// Text preview length.
pub const PREVIEW_TEXT_MAX: usize = crate::defaults::PREVIEW_TEXT_MAX;

#[derive(Serialize, Deserialize, Debug)]
pub enum Loaded {
    Text(String, bool),
    /// Original width and height; the pixels travel beside it as a `Picture`.
    Image([u32; 2]),
    Folder { folders: usize, files: usize, size: u64 },
    Binary,
    Error(String),
}

pub struct Picture {
    pub size: [u32; 2],
    pub rgba: Vec<u8>,
}

pub type Reply = (Loaded, Option<Picture>);

fn plain(loaded: Loaded) -> Reply {
    (loaded, None)
}

pub fn load(p: &Path, max_side: u32) -> Reply {
    let md = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) => return plain(Loaded::Error(fsx::friendly_err(&e))),
    };
    if md.is_dir() { return plain(folder(p)); }
    if !md.is_file() { return plain(Loaded::Error("Preview is unavailable for pipes, sockets and device files".into())); }
    if md.len() == 0 { return plain(Loaded::Text(String::new(), false)); }
    let ext = fsx::split_ext(&fsx::display_name(p), false).to_ascii_lowercase();
    if fsx::IMAGE_EXTS.contains(&ext.as_str()) && md.len() < 200_000_000 { return image(p, max_side); }
    plain(text(p, &ext, md.len()))
}

fn folder(p: &Path) -> Loaded {
    let (mut folders, mut files, mut size) = (0, 0, 0u64);
    let rd = match std::fs::read_dir(p) {
        Ok(rd) => rd,
        Err(e) => return Loaded::Error(fsx::friendly_err(&e)),
    };
    for de in rd.take(20000) {
        let de = match de {
            Ok(de) => de,
            Err(e) => return Loaded::Error(fsx::friendly_err(&e)),
        };
        match de.metadata() {
            Ok(m) if m.is_dir() => folders += 1,
            Ok(m) => {
                files += 1;
                size += m.len();
            }
            Err(_) => {}
        }
    }
    Loaded::Folder { folders, files, size }
}

fn image(p: &Path, max_side: u32) -> Reply {
    let decoded = image::ImageReader::open(p)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())
        .and_then(|r| r.decode().map_err(|e| e.to_string()));
    match decoded {
        Ok(img) => {
            let orig = [img.width(), img.height()];
            let small = if img.width() > max_side || img.height() > max_side { img.thumbnail(max_side, max_side) } else { img };
            let rgba = small.to_rgba8();
            let picture = Picture { size: [rgba.width(), rgba.height()], rgba: rgba.into_raw() };
            (Loaded::Image(orig), Some(picture))
        }
        Err(e) => plain(Loaded::Error(e)),
    }
}

fn text(p: &Path, ext: &str, len: u64) -> Loaded {
    use std::io::Read;
    let mut buf = vec![0u8; PREVIEW_TEXT_MAX * 4];
    let n = match std::fs::File::open(p).and_then(|mut f| f.read(&mut buf)) {
        Ok(n) => n,
        Err(e) => return Loaded::Error(fsx::friendly_err(&e)),
    };
    buf.truncate(n);
    let sniff = &buf[..buf.len().min(2048)];
    if !fsx::is_text_extension(ext) && sniff.contains(&0) { return Loaded::Binary; }
    let s = String::from_utf8_lossy(&buf);
    let trimmed = s.chars().count() > PREVIEW_TEXT_MAX || len as usize > n;
    Loaded::Text(s.chars().take(PREVIEW_TEXT_MAX).collect(), trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Timings on real files: `FERROUS_PREVIEW_FIXTURES=<folder> cargo test --release
    /// preview_baseline -- --ignored --nocapture`.
    #[test]
    #[ignore = "measures files named by FERROUS_PREVIEW_FIXTURES"]
    fn preview_baseline() {
        let folder = std::env::var_os("FERROUS_PREVIEW_FIXTURES").expect("FERROUS_PREVIEW_FIXTURES");
        let mut files: Vec<_> = std::fs::read_dir(folder).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        for file in files {
            let started = std::time::Instant::now();
            let (loaded, picture) = load(&file, crate::preview::MAX_SIDE);
            let summary = match loaded {
                Loaded::Text(t, trimmed) => format!("text {} chars, trimmed {trimmed}", t.chars().count()),
                other => format!("{other:?} {:?}", picture.map(|p| p.size)),
            };
            println!("{:>7.1} ms  {}  ->  {summary}", started.elapsed().as_secs_f64() * 1000.0, fsx::display_name(&file));
        }
    }

    #[test]
    fn previews_text_images_folders_and_binary_files() {
        let root = std::env::temp_dir().join(format!("fc_preview_{}", std::process::id()));
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let text = root.join("unicode.txt");
        std::fs::write(&text, "🦀".repeat(PREVIEW_TEXT_MAX + 1)).unwrap();
        assert!(matches!(load(&text, 1024).0, Loaded::Text(s, true) if s == "🦀".repeat(PREVIEW_TEXT_MAX)));
        let binary = root.join("binary.bin");
        std::fs::write(&binary, [0, 1, 2, 3]).unwrap();
        assert!(matches!(load(&binary, 1024).0, Loaded::Binary));
        let picture = root.join("image.png");
        image::RgbaImage::new(2048, 100).save(&picture).unwrap();
        let (loaded, pixels) = load(&picture, 1024);
        assert!(matches!(loaded, Loaded::Image([2048, 100])));
        let pixels = pixels.unwrap();
        assert!(pixels.size[0] == 1024 && pixels.size[1] <= 1024);
        assert_eq!(pixels.rgba.len(), (pixels.size[0] * pixels.size[1] * 4) as usize);
        assert!(matches!(load(&root, 1024).0, Loaded::Folder { folders: 1, files: 3, .. }));
        assert!(matches!(load(&root.join("missing"), 1024).0, Loaded::Error(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inaccessible_folder_reports_error_and_pipe_is_not_read() {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let root = std::env::temp_dir().join(format!("fc_preview_access_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let pipe = root.join("pipe.txt");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(matches!(load(&pipe, 1024).0, Loaded::Error(_)));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = load(&root, 1024).0;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(matches!(result, Loaded::Error(message) if message == "No access"));
        }
    }
}
