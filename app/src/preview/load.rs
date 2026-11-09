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
    Text { text: String, trimmed: bool, colours: Colours },
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
    if md.len() == 0 { return plain(Loaded::Text { text: String::new(), trimmed: false, colours: Colours::default() }); }
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
                Loaded::Text { text, trimmed, colours } => format!("text {} chars, trimmed {trimmed}, {} lines coloured", text.chars().count(), colours.ends.len()),
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

    fn zip_with(path: &std::path::Path, entries: &[(&str, &str)]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in entries { zip.start_file(*name, options).unwrap(); zip.write_all(text.as_bytes()).unwrap(); }
        zip.finish().unwrap();
    }

    /// Damaged files of every previewed kind. Every round must finish in time and without a
    /// panic. `FERROUS_CAMPAIGN_FUZZ` sets the rounds per file.
    #[test]
    #[ignore = "long run; writes damaged files under the temporary folder"]
    fn damaged_files_never_crash_or_hang_the_preview() {
        let rounds: u64 = std::env::var("FERROUS_CAMPAIGN_FUZZ").ok().and_then(|n| n.parse().ok()).unwrap_or(400);
        let root = scratch("fuzz");
        let seeds = root.join("seeds");
        std::fs::create_dir(&seeds).unwrap();
        let mut picture = image::RgbaImage::new(48, 48);
        for (x, y, pixel) in picture.enumerate_pixels_mut() { *pixel = image::Rgba([x as u8 * 5, y as u8 * 5, (x ^ y) as u8, 255]); }
        for (name, format) in [("a.png", image::ImageFormat::Png), ("a.bmp", image::ImageFormat::Bmp), ("a.gif", image::ImageFormat::Gif), ("a.tiff", image::ImageFormat::Tiff), ("a.webp", image::ImageFormat::WebP), ("a.ico", image::ImageFormat::Ico)] {
            picture.save_with_format(seeds.join(name), format).unwrap();
        }
        image::DynamicImage::ImageRgba8(picture).to_rgb8().save_with_format(seeds.join("a.jpg"), image::ImageFormat::Jpeg).unwrap();
        std::fs::write(seeds.join("a.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs><rect width="40" height="40" fill="url(#g)"/><text x="2" y="20">x</text></svg>"##).unwrap();
        std::fs::write(seeds.join("a.pdf"), "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n4 0 obj<</Length 21>>stream\n0 0 m 100 100 l S\nendstream endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n").unwrap();
        std::fs::write(seeds.join("a.stl"), "solid a\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid a\n").unwrap();
        std::fs::write(seeds.join("a.md"), "# T\n\n*a* `b` [c](d)\n\n| x | y |\n|---|---|\n| 1 | 2 |\n\n```rust\nfn x() {}\n```\n").unwrap();
        std::fs::write(seeds.join("a.csv"), "a,b,\"c\"\n1,2,3\n").unwrap();
        std::fs::write(seeds.join("a.rs"), "fn main() { println!(\"x\"); }\n").unwrap();
        zip_with(&seeds.join("a.zip"), &[("x.txt", "hello"), ("y/z.txt", "world")]);
        zip_with(&seeds.join("a.3mf"), &[("3D/3dmodel.model", r#"<model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources><object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object></resources><build><item objectid="1"/></build></model>"#)]);
        zip_with(&seeds.join("a.docx"), &[("word/document.xml", r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#)]);
        zip_with(&seeds.join("a.odt"), &[("content.xml", r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:text><text:p>x</text:p></office:text></office:body></office:document-content>"#)]);
        zip_with(&seeds.join("a.pptx"), &[("ppt/slides/slide1.xml", r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:t>x</a:t></p:sld>"#)]);
        zip_with(&seeds.join("a.xlsx"), &[("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#), ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets><sheet name="S" sheetId="1"/></sheets></workbook>"#)]);
        zip_with(&seeds.join("a.epub"), &[("mimetype", "application/epub+zip"), ("OEBPS/a.xhtml", "<html><body><p>x</p></body></html>")]);

        let mut files: Vec<_> = std::fs::read_dir(&seeds).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        // Each kind runs in a copy of this test program. In the app such a parser dies in the
        // helper process; here a death is written down and the next round goes on.
        let (exe, mut failures) = (std::env::current_exe().unwrap(), Vec::new());
        let progress = root.join("progress");
        for seed in &files {
            let name = seed.file_name().unwrap().to_string_lossy().into_owned();
            let mut start = 0;
            while start < rounds {
                let output = std::process::Command::new(&exe)
                    .args(["--exact", "preview::load::tests::damaged_files_worker", "--ignored", "--nocapture"])
                    .env("FERROUS_FUZZ_SEED", seed).env("FERROUS_FUZZ_START", start.to_string()).env("FERROUS_FUZZ_ROUNDS", rounds.to_string())
                    .env("FERROUS_FUZZ_PROGRESS", &progress).output().unwrap();
                if output.status.success() { break; }
                let round: u64 = std::fs::read_to_string(&progress).ok().and_then(|t| t.parse().ok()).unwrap_or(start);
                let text = String::from_utf8_lossy(&output.stderr).into_owned();
                let line = text.lines().find(|l| !l.trim().is_empty() && !l.starts_with("note")).unwrap_or("").to_owned();
                eprintln!("[fuzz preview] {name} round {round}: {:?}: {line}", output.status.code());
                failures.push(format!("{name} round {round}: {line}"));
                start = round + 1;
            }
            eprintln!("[fuzz preview] {name} done");
        }
        eprintln!("[fuzz preview] {} kinds x {rounds} rounds, {} failures", files.len(), failures.len());
        assert!(failures.is_empty(), "{failures:#?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// `cargo test --release campaign_preview -- --ignored --nocapture`
    #[cfg(windows)]
    #[test]
    #[ignore = "measures memory for a very large picture"]
    fn campaign_preview_peak_memory() {
        let root = scratch("peak");
        let huge = root.join("huge.png");
        image::RgbaImage::new(12000, 12000).save_with_format(&huge, image::ImageFormat::Png).unwrap();
        let text = root.join("large.txt");
        std::fs::write(&text, "a line of ordinary text\n".repeat(8_000_000)).unwrap();
        for file in [&huge, &text] {
            let started = std::time::Instant::now();
            let (_, memory) = crate::win::measure::memory_used_by(|| load(file, whole(crate::preview::MAX_SIDE)));
            eprintln!("[preview peak] {} on disk {} MB: {:?}, peak {} MB", fsx::display_name(file), std::fs::metadata(file).unwrap().len() >> 20, started.elapsed(), memory >> 20);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "worker of damaged_files_never_crash_or_hang_the_preview"]
    fn damaged_files_worker() {
        let Some(seed) = std::env::var_os("FERROUS_FUZZ_SEED").map(std::path::PathBuf::from) else { return };
        let number = |key: &str| std::env::var(key).unwrap().parse::<u64>().unwrap();
        let (start, rounds, progress) = (number("FERROUS_FUZZ_START"), number("FERROUS_FUZZ_ROUNDS"), std::env::var_os("FERROUS_FUZZ_PROGRESS").unwrap());
        let original = std::fs::read(&seed).unwrap();
        let name = seed.file_name().unwrap().to_string_lossy().into_owned();
        let root = seed.parent().unwrap().parent().unwrap();
        for round in start..rounds {
            std::fs::write(&progress, round.to_string()).unwrap();
            let mut state = (round + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ original.len() as u64 ^ name.bytes().fold(7u64, |h, b| h.wrapping_mul(31) + b as u64) | 1;
            let mut next = move || { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state };
            let mut bytes = original.clone();
            if next() % 3 == 0 { bytes.truncate((next() % bytes.len() as u64) as usize); }
            else { for _ in 0..1 + next() % 32 { let at = (next() % bytes.len() as u64) as usize; bytes[at] = next() as u8; } }
            let target = root.join(format!("{round}-{name}"));
            std::fs::write(&target, &bytes).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let path = target.clone();
            let worker = std::thread::spawn(move || { let _ = load(&path, whole(256)); let _ = tx.send(()); });
            match rx.recv_timeout(std::time::Duration::from_secs(30)) {
                Ok(()) => { let _ = worker.join(); }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("hung for 30 s"),
                Err(_) => panic!("the parser panicked"),
            }
            let _ = std::fs::remove_file(&target);
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
