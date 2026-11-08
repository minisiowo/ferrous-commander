//! Turns a file into something to show. Runs inside the helper process.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{archive_list, media, model, office, pdf, picture, text};
use crate::fsx::{self, Kind};

/// What part of a file to prepare, and how large.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    pub max_side: u32,
    /// Page of a document.
    pub page: u32,
    /// Part of the picture, as left, top, right, bottom out of 65535.
    pub crop: Option<[u16; 4]>,
    /// Another look at a 3D model, instead of the first picture of it.
    pub view: Option<ModelView>,
}

/// How to show a 3D model. Whole numbers, so that equal views compare equal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ModelView {
    /// Tenths of a degree away from the first view.
    pub turn: i16,
    pub tilt: i16,
    /// Percent of the first view's size.
    pub zoom: u16,
    /// Pixels the model is moved by.
    pub pan: [i16; 2],
    /// Pixels of the picture wanted.
    pub size: [u16; 2],
    /// Pixels the first picture is wide on the screen, which sets the scale.
    pub fit: u16,
    /// Full quality, after the movement has stopped.
    pub fine: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind { H1, H2, H3, Paragraph, Quote, Code, Item, Rule }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub folder: bool,
}

/// Colours of a text, in one piece so that a long file is not thousands of small allocations.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Colours {
    /// Length in bytes and colour class of each stretch, line after line.
    pub spans: Vec<(u32, u8)>,
    /// Where in `spans` each line ends. Lines beyond the end of this have no colours.
    pub ends: Vec<u32>,
}

impl Colours {
    /// `None` too when the numbers make no sense, as they came from the helper.
    pub fn line(&self, n: usize) -> Option<&[(u32, u8)]> {
        let start = if n == 0 { 0 } else { *self.ends.get(n - 1)? as usize };
        self.spans.get(start..*self.ends.get(n)? as usize)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Loaded {
    Text { text: String, trimmed: bool, spans: Colours },
    /// Original width and height; the pixels travel beside it as a `Picture`.
    Image([u32; 2]),
    /// A drawn 3D model, with nothing behind it; its size in points, as for `Image`.
    Model([u32; 2]),
    Page { orig: [u32; 2], page: u32, pages: u32 },
    /// A sharper copy of part of a picture that is already shown.
    Detail,
    Doc { blocks: Vec<Block>, source: Option<String>, trimmed: bool },
    Hex { bytes: Vec<u8>, total: u64 },
    /// Spreadsheet rows; `sheets` lists the workbook's sheets and `page` the one shown.
    Table { rows: Vec<Vec<String>>, sheets: Vec<String>, page: u32, trimmed: bool },
    Archive { entries: Vec<ArchiveEntry>, total: u64, trimmed: bool },
    /// Facts about a file, with a cover or video frames as the `Picture` when there are any.
    /// `more` says that asking again for the film page gives a moving preview.
    Info { rows: Vec<(String, String)>, more: bool },
    Folder { folders: usize, files: usize, size: u64 },
    Error(String),
}

pub struct Picture {
    pub size: [u32; 2],
    /// Milliseconds per frame of an animation; empty for one still picture.
    pub delays: Vec<u32>,
    pub rgba: Vec<u8>,
}

pub type Reply = (Loaded, Option<Picture>);

pub(super) fn plain(loaded: Loaded) -> Reply {
    (loaded, None)
}

pub(super) fn failed(message: impl Into<String>) -> Reply {
    plain(Loaded::Error(message.into()))
}

fn head(p: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(1024);
    std::fs::File::open(p)?.take(1024).read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn load(p: &Path, options: Options) -> Reply {
    let md = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(e) => return failed(fsx::friendly_err(&e)),
    };
    if md.is_dir() { return plain(folder(p)); }
    if !md.is_file() { return failed("Preview is unavailable for pipes, sockets and device files"); }
    if md.len() == 0 { return plain(Loaded::Text { text: String::new(), trimmed: false, spans: Colours::default() }); }
    let head = match head(p) {
        Ok(head) => head,
        Err(e) => return failed(fsx::friendly_err(&e)),
    };
    let ext = fsx::split_ext(&fsx::display_name(p), false).to_ascii_lowercase();
    // Contents go first: a PDF or a picture with the wrong name is still that.
    if head.windows(5).any(|w| w == b"%PDF-") { return pdf::load(p, options); }
    if ext == "svg" || ext == "svgz" { return picture::svg(p, options); }
    if picture::is_picture(&head, &ext) { return picture::load(p, options); }
    let name = fsx::display_name(p);
    match ext.as_str() {
        "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => return office::workbook(p, options),
        "csv" | "tsv" => return office::separated(p, &ext, md.len()),
        "docx" | "docm" => return office::word(p),
        "odt" => return office::opendocument(p),
        "pptx" | "pptm" => return office::slides(p),
        "epub" => return office::epub(p),
        "stl" => return model::stl(p, options),
        "3mf" => return model::three_mf(p, options),
        "rar" => return plain(Loaded::Info { rows: vec![("Archive".into(), "RAR contents are shown after unpacking".into())], more: false }),
        _ => {}
    }
    if let Some(listed) = archive_list::load(p, &name, head.starts_with(b"PK\x03\x04")) { return plain(listed); }
    match Kind::from_ext(&ext) {
        Kind::Audio => return media::audio(p),
        Kind::Video => return media::video(p, options.page),
        _ => {}
    }
    plain(text::load(p, &ext, md.len()))
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

#[cfg(test)]
mod tests {
    use super::*;

    pub(in crate::preview) fn whole(max_side: u32) -> Options {
        Options { max_side, page: 0, crop: None, view: None }
    }

    pub(in crate::preview) fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("fc_preview_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

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
            let (loaded, picture) = load(&file, whole(crate::preview::MAX_SIDE));
            let summary = match loaded {
                Loaded::Text { text, trimmed, spans } => format!("text {} chars, trimmed {trimmed}, {} lines coloured", text.chars().count(), spans.ends.len()),
                Loaded::Doc { blocks, .. } => format!("doc of {} blocks", blocks.len()),
                Loaded::Hex { bytes, total } => format!("hex {} of {total}", bytes.len()),
                Loaded::Table { rows, sheets, .. } => format!("table {} rows, {} sheets", rows.len(), sheets.len()),
                Loaded::Archive { entries, .. } => format!("archive {} entries", entries.len()),
                other => format!("{other:?}"),
            };
            let size = picture.map(|p| (p.size, p.delays.len()));
            println!("{:>7.1} ms  {}  ->  {summary} {size:?}", started.elapsed().as_secs_f64() * 1000.0, fsx::display_name(&file));
        }
    }

    #[test]
    fn folders_missing_files_and_contents_over_names() {
        let root = scratch("load");
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let picture = root.join("really_a_picture.txt");
        image::RgbaImage::new(4, 4).save_with_format(&picture, image::ImageFormat::Png).unwrap();
        assert!(matches!(load(&picture, whole(64)).0, Loaded::Image([4, 4])));
        let pdf = root.join("report.bin");
        std::fs::write(&pdf, b"%PDF-1.7 not really").unwrap();
        assert!(matches!(load(&pdf, whole(64)).0, Loaded::Error(_)), "goes to the PDF reader, which refuses it");
        std::fs::write(root.join("bm.txt"), b"BM is how this text starts").unwrap();
        assert!(matches!(load(&root.join("bm.txt"), whole(64)).0, Loaded::Text { .. }), "weak signatures need the name too");
        assert!(matches!(load(&root, whole(64)).0, Loaded::Folder { folders: 1, files: 3, .. }));
        assert!(matches!(load(&root.join("missing"), whole(64)).0, Loaded::Error(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inaccessible_folder_reports_error_and_pipe_is_not_read() {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let root = scratch("access");
        let pipe = root.join("pipe.txt");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(matches!(load(&pipe, whole(64)).0, Loaded::Error(_)));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = load(&root, whole(64)).0;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(matches!(result, Loaded::Error(message) if message == "No access"));
        }
    }
}

#[cfg(test)]
pub(super) use tests::{scratch, whole};
