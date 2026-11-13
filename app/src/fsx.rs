//! Folder listing, sorting, hidden-file rules and value formatting.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Local};
use egui::Color32;
use serde::{Deserialize, Serialize};

use crate::defaults::{HIDDEN_NAMES, HIDDEN_EXTENSIONS, HIDDEN_PREFIXES};
/// Additional preview formats supported by the rebuild.
const EXTRA_TEXT_EXTS: &[&str] = &[
    "log", "csv", "toml", "yaml", "yml",
    "rs", "py", "js", "ts", "c", "h", "cpp", "cs", "java", "go", "sh", "bat", "ps1", "css",
];

pub fn is_text_extension(ext: &str) -> bool {
    crate::defaults::TEXT_EXTENSIONS.contains(&ext) || EXTRA_TEXT_EXTS.contains(&ext)
}
/// Decoder allowlist for preview; classification can recognize more formats.
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff", "tga", "pnm", "ppm", "pgm", "pbm", "pam", "qoi", "hdr", "dds", "exr",
];
const VIDEO_EXTS: &[&str] = &["mkv", "avi", "mp4", "mov", "webm", "m4v", "wmv", "mpeg", "mpg", "3gp"];
/// Audio formats decoded by the preview player; also used for file classification.
pub const AUDIO_PLAYBACK_EXTS: &[&str] = &["mp3", "wav", "flac", "ogg", "oga", "m4a", "aac", "aiff", "aif", "mka"];
const AUDIO_EXTS: &[&str] = &["ape", "opus", "wma"];
const DOC_EXTS: &[&str] = &[
    "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "pdf", "rtf", "epub",
];
const ARCHIVE_EXTS: &[&str] = &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "iso", "cab", "tgz"];
const EXEC_EXTS: &[&str] = &["exe", "msi", "bat", "cmd", "com", "ps1", "sh", "appimage", "deb"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Folder,
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Program,
    Text,
    Other,
}

impl Kind {
    pub fn from_ext(ext: &str) -> Kind {
        let e = ext.to_ascii_lowercase();
        let e = e.as_str();
        // Category precedence and extra portable formats are rebuild choices.
        // Shipped membership lists are exact configuration observations.
        use crate::defaults as original;
        if IMAGE_EXTS.contains(&e)
            || original::IMAGE_EXTENSIONS.contains(&e)
            || original::PHOTO_EXTENSIONS.contains(&e)
        {
            Kind::Image
        } else if VIDEO_EXTS.contains(&e) || original::VIDEO_EXTENSIONS.contains(&e) {
            Kind::Video
        } else if AUDIO_PLAYBACK_EXTS.contains(&e) || AUDIO_EXTS.contains(&e) || original::AUDIO_EXTENSIONS.contains(&e) {
            Kind::Audio
        } else if DOC_EXTS.contains(&e) || original::DOCUMENT_EXTENSIONS.contains(&e) {
            Kind::Document
        } else if ARCHIVE_EXTS.contains(&e) || original::ARCHIVE_EXTENSIONS.contains(&e) {
            Kind::Archive
        } else if EXEC_EXTS.contains(&e) || original::EXECUTABLE_EXTENSIONS.contains(&e) {
            Kind::Program
        } else if is_text_extension(e) {
            Kind::Text
        } else {
            Kind::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => "Folder",
            Kind::Image => "Image",
            Kind::Video => "Video",
            Kind::Audio => "Audio",
            Kind::Document => "Document",
            Kind::Archive => "Archive",
            Kind::Program => "Program",
            Kind::Text => "Text",
            Kind::Other => "File",
        }
    }

    /// Accent colour of the little file glyph.
    pub fn color(self) -> Color32 {
        match self {
            Kind::Folder => Color32::from_rgb(0xF2, 0xC2, 0x4F),
            Kind::Image => Color32::from_rgb(0x88, 0xC8, 0x66),
            Kind::Video => Color32::from_rgb(0xC7, 0x8E, 0xDD),
            Kind::Audio => Color32::from_rgb(0xED, 0xA9, 0x5D),
            Kind::Document => Color32::from_rgb(0x6F, 0xB8, 0xEF),
            Kind::Archive => Color32::from_rgb(0xEF, 0x70, 0x63),
            Kind::Program => Color32::from_rgb(0x5E, 0xD0, 0xC0),
            Kind::Text => Color32::from_rgb(0xB8, 0xB8, 0xC0),
            Kind::Other => Color32::from_rgb(0x9A, 0x98, 0xA0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    /// Only when the name is not simply the end of the path.
    custom_name: Option<Box<str>>,
    /// Lower-case name for filtering and sorting, only when it differs from the name.
    lower: Option<Box<str>>,
    /// Where the name starts in the path.
    name_at: u32,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub hidden: bool,
    pub readonly: bool,
    pub kind: Kind,
}

impl Entry {
    /// A row called `name`, with nothing else known about it yet.
    pub fn named(mut path: PathBuf, name: String, is_dir: bool, kind: Kind) -> Entry {
        // `DirEntry::path` leaves room for growing, and listings hold many of these.
        path.shrink_to_fit();
        let bytes = path.as_os_str().as_encoded_bytes();
        let name_at = bytes.len().checked_sub(name.len())
            .filter(|&at| at <= u32::MAX as usize && &bytes[at..] == name.as_bytes());
        let already_lower = name.is_ascii() && !name.bytes().any(|b| b.is_ascii_uppercase());
        let lowered = (!already_lower).then(|| name.to_lowercase()).filter(|lowered| *lowered != name);
        Entry {
            lower: lowered.map(String::into_boxed_str),
            name_at: name_at.unwrap_or(0) as u32,
            custom_name: name_at.is_none().then(|| name.into_boxed_str()),
            path,
            is_dir,
            size: 0,
            modified: None,
            created: None,
            hidden: false,
            readonly: false,
            kind,
        }
    }

    pub fn name(&self) -> &str {
        if let Some(name) = &self.custom_name { return name; }
        let tail = self.path.as_os_str().as_encoded_bytes().get(self.name_at as usize..).unwrap_or_default();
        std::str::from_utf8(tail).unwrap_or_default()
    }

    pub fn lname(&self) -> &str {
        self.lower.as_deref().unwrap_or_else(|| self.name())
    }

    pub fn ext(&self) -> &str {
        split_ext(self.name(), self.is_dir)
    }

    /// The name without its extension.
    pub fn stem(&self) -> &str {
        let (name, ext) = (self.name(), self.ext());
        if ext.is_empty() { name } else { &name[..name.len() - ext.len() - 1] }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum SortBy {
    #[default]
    Name,
    Date,
    Size,
    Type,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Sorting {
    pub by: SortBy,
    pub ascending: bool,
    pub folders_on_top: bool,
}

impl Default for Sorting {
    fn default() -> Self {
        Sorting {
            by: crate::defaults::SORT_BY,
            ascending: crate::defaults::SORT_ASCENDING,
            folders_on_top: crate::defaults::SORT_FOLDERS_ON_TOP,
        }
    }
}

/// Whether a name alone makes an item hidden, as `make_entry` decides it before attributes.
pub fn hidden_by_name(name: &str, is_dir: bool) -> bool {
    is_hidden_by_rule(name, split_ext(name, is_dir))
}

/// The hidden attribute Windows keeps apart from the name.
#[cfg(windows)]
pub fn hidden_by_attribute(md: &std::fs::Metadata) -> bool {
    attr_flags(md).0
}

fn is_hidden_by_rule(name: &str, ext: &str) -> bool {
    let l = name.to_lowercase();
    if HIDDEN_NAMES.iter().any(|n| n.eq_ignore_ascii_case(&l))
        || HIDDEN_PREFIXES.iter().any(|p| l.starts_with(p)) {
        return true;
    }
    let e = ext.to_ascii_lowercase();
    if HIDDEN_EXTENSIONS.contains(&e.as_str()) {
        return true;
    }
    // Dot-files: not hidden on Windows,
    // but they are the hiding convention on Linux.
    (crate::defaults::TREAT_DOT_FILES_AS_HIDDEN || cfg!(not(windows)))
        && name.starts_with('.')
}

#[cfg(windows)]
fn attr_flags(md: &std::fs::Metadata) -> (bool, bool) {
    use std::os::windows::fs::MetadataExt;
    let a = md.file_attributes();
    (a & 0x2 != 0 || a & 0x4 != 0, a & 0x1 != 0)
}
#[cfg(not(windows))]
fn attr_flags(md: &std::fs::Metadata) -> (bool, bool) {
    (false, md.permissions().readonly())
}

pub fn split_ext(name: &str, is_dir: bool) -> &str {
    if is_dir {
        return "";
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i + 1..],
        _ => "",
    }
}

pub fn make_entry(path: PathBuf, md: &std::fs::Metadata) -> Entry {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let is_dir = md.is_dir();
    let ext = split_ext(&name, is_dir);
    let (attr_hidden, readonly) = attr_flags(md);
    let hidden = attr_hidden || is_hidden_by_rule(&name, ext);
    let kind = if is_dir { Kind::Folder } else { Kind::from_ext(ext) };
    let mut entry = Entry::named(path, name, is_dir, kind);
    entry.size = if is_dir { 0 } else { md.len() };
    entry.modified = md.modified().ok();
    entry.created = md.created().ok();
    entry.hidden = hidden;
    entry.readonly = readonly;
    entry
}

/// Read one folder. Fast path: one directory walk, metadata comes with the entry on Windows.
pub fn read_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| friendly_err(&e))?;
    let mut out = Vec::with_capacity(64);
    // Stop at the first error: on a drive that was pulled out it can repeat forever.
    for de in rd.map_while(Result::ok) {
        let p = de.path();
        let md = match de.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        // Follow links to folders so they behave like folders.
        let md = if md.file_type().is_symlink() {
            std::fs::metadata(&p).unwrap_or(md)
        } else {
            md
        };
        out.push(make_entry(p, &md));
    }
    out.shrink_to_fit();
    Ok(out)
}

/// Selected parents already include their children. Sorted paths make descendants
/// contiguous, avoiding a quadratic scan of a large search/tree selection.
pub fn outermost_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    let mut kept: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if kept.last().is_none_or(|parent| !path.starts_with(parent)) { kept.push(path); }
    }
    kept
}

pub fn friendly_err(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "No access".into(),
        std::io::ErrorKind::NotFound => "Not available".into(),
        _ => e.to_string(),
    }
}

/// Number-aware comparison ("file2" < "file10"), case-insensitive.
#[cfg(any(not(windows), test))]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < ab.len() && j < bb.len() {
        let (ca, cb) = (ab[i], bb[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let si = i;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let na = a[si..i].trim_start_matches('0');
            let nb = b[sj..j].trim_start_matches('0');
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            let o = ca.to_ascii_lowercase().cmp(&cb.to_ascii_lowercase());
            if o != Ordering::Equal {
                return o;
            }
            i += 1;
            j += 1;
        }
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}

/// Prepared name for comparing, kept only while sorting: most folders hold far
/// more entries than they are ever re-sorted, so entries do not carry it.
#[cfg(windows)]
type NameKey = Vec<u16>;
#[cfg(not(windows))]
type NameKey = ();

fn name_key(_e: &Entry) -> NameKey {
    #[cfg(windows)]
    { crate::win::wide(_e.name()) }
}

fn compare_entries(keyed_a: &(NameKey, Entry), keyed_b: &(NameKey, Entry), s: Sorting) -> Ordering {
    let (a, b) = (&keyed_a.1, &keyed_b.1);
    if s.folders_on_top && a.is_dir != b.is_dir {
        return b.is_dir.cmp(&a.is_dir);
    }
    let by_name = || {
        #[cfg(windows)]
        { crate::win::logical_cmp(&keyed_a.0, &keyed_b.0) }
        #[cfg(not(windows))]
        { natural_cmp(a.lname(), b.lname()) }
    };
    // "Folders can't be sorted by size" (resource SortBySize_LargestFirst).
    if s.by == SortBy::Size && a.is_dir && b.is_dir {
        return by_name();
    }
    fn lower(text: &str) -> impl Iterator<Item = char> + '_ {
        text.chars().flat_map(char::to_lowercase)
    }
    let o = match s.by {
        SortBy::Name => by_name(),
        SortBy::Date => a.modified.cmp(&b.modified).then_with(by_name),
        // Folders stay one group next to the files, even empty ones. Comparing them
        // with files by size and name would make the order inconsistent, and the
        // standard sort may panic on an inconsistent order.
        SortBy::Size if a.is_dir != b.is_dir => b.is_dir.cmp(&a.is_dir),
        SortBy::Size => a.size.cmp(&b.size).then_with(by_name),
        SortBy::Type => lower(a.ext()).cmp(lower(b.ext())).then_with(by_name),
    };
    if s.ascending { o } else { o.reverse() }
}

pub fn sort_entries(v: &mut Vec<Entry>, s: Sorting) {
    let keyed: Vec<(NameKey, Entry)> = std::mem::take(v).into_iter().map(|e| (name_key(&e), e)).collect();
    *v = sort_in_parts(keyed, |a, b| compare_entries(a, b, s)).into_iter().map(|(_, e)| e).collect();
}

/// From this many items a sort is worth spreading over the processor's cores.
const SORT_IN_PARTS_FROM: usize = 20_000;

/// A stable sort like `sort_by`, with the same result, on several cores when there is much to do.
fn sort_in_parts<T: Send>(mut items: Vec<T>, compare: impl Fn(&T, &T) -> Ordering + Sync) -> Vec<T> {
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    if items.len() < SORT_IN_PARTS_FROM || workers < 2 {
        items.sort_by(compare);
        return items;
    }
    let size = items.len().div_ceil(workers);
    let mut rest = items.into_iter();
    let mut parts: Vec<Vec<T>> = (0..workers).map(|_| rest.by_ref().take(size).collect()).collect();
    let compare = &compare;
    std::thread::scope(|scope| {
        for part in &mut parts { scope.spawn(move || part.sort_by(compare)); }
    });
    while parts.len() > 1 {
        let mut waiting = parts.into_iter();
        parts = std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            while let Some(left) = waiting.next() {
                let right = waiting.next();
                jobs.push(scope.spawn(move || match right {
                    Some(right) => merged(left, right, compare),
                    None => left,
                }));
            }
            jobs.into_iter().map(|job| job.join().unwrap()).collect()
        });
    }
    parts.pop().unwrap_or_default()
}

/// Of two equal items the one from `left` comes first, which keeps the sort stable.
fn merged<T>(left: Vec<T>, right: Vec<T>, compare: &impl Fn(&T, &T) -> Ordering) -> Vec<T> {
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut left, mut right) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
        let next = if compare(b, a) == Ordering::Less { right.next() } else { left.next() };
        out.extend(next);
    }
    out.extend(left);
    out.extend(right);
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    Folder,
    File,
    Missing,
}

/// Checks paths without letting a sleeping network share freeze the caller:
/// an answer that does not arrive within `limit` stays `None`.
pub fn probe_paths(paths: &[PathBuf], limit: std::time::Duration) -> Vec<Option<PathKind>> {
    let (tx, rx) = std::sync::mpsc::channel();
    for (k, path) in paths.iter().cloned().enumerate() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let kind = match std::fs::metadata(&path) {
                Ok(md) if md.is_dir() => PathKind::Folder,
                Ok(md) if md.is_file() => PathKind::File,
                _ => PathKind::Missing,
            };
            let _ = tx.send((k, kind));
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + limit;
    let mut kinds = vec![None; paths.len()];
    for _ in paths {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok((k, kind)) = rx.recv_timeout(left) else { break };
        kinds[k] = Some(kind);
    }
    kinds
}

/// Size with decimal units.
pub fn fmt_size(bytes: u64, mebibytes: bool) -> String {
    let k = if mebibytes { 1024.0 } else { 1000.0 };
    let units: [&str; 5] = if mebibytes {
        ["B", "KiB", "MiB", "GiB", "TiB"]
    } else {
        ["B", "KB", "MB", "GB", "TB"]
    };
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= k && u < units.len() - 1 {
        v /= k;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", units[u])
    } else {
        format!("{v:.1} {}", units[u])
    }
}

pub fn age_secs(t: Option<SystemTime>) -> Option<f64> {
    let t = t?;
    Some(SystemTime::now().duration_since(t).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

/// Short relative age as in the Age column: `40'`, `1 h 44'`, `4 d`, `3 M`, `1 y`
/// (such as `16'`, `1 h 44'`, `2 h 2'`).
pub fn fmt_age(t: Option<SystemTime>) -> String {
    let Some(s) = age_secs(t) else { return String::new() };
    let min = s / 60.0;
    let h = min / 60.0;
    let d = h / 24.0;
    if min < 60.0 {
        format!("{}'", min.floor() as i64)
    } else if h < 24.0 {
        format!("{} h {}'", h.floor() as i64, (min % 60.0).floor() as i64)
    } else if d < 60.0 {
        format!("{} d", d.floor() as i64)
    } else if d < 365.0 {
        format!("{} M", (d / 30.44).floor() as i64)
    } else {
        format!("{} y", (d / 365.25).floor() as i64)
    }
}

/// Windows-style HLS (0..240 scale) to RGB.
fn hls240(h: f32, l: f32, s: f32) -> Color32 {
    let (h, l, s) = (h / 240.0, l / 240.0, s / 240.0);
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(c(f(h + 1.0 / 3.0)), c(f(h)), c(f(h - 1.0 / 3.0)))
}

/// Saturation and luminosity come from the defaults.
pub const AGE_SATURATION: f32 = crate::defaults::AGE_SATURATION;
pub const AGE_LUMA: f32 = crate::defaults::AGE_LUMA;

/// Age colour: red (now) → yellow (1 day) → green (7 days) → cyan (60 days) → blue (330 days),
/// grey after a year. The curve points are estimated from the program's own age legend picture.
pub fn age_color(t: Option<SystemTime>) -> Option<Color32> {
    let s = age_secs(t)?;
    let days = s / 86400.0;
    if days >= 365.0 {
        return None;
    }
    // (days, hue in degrees)
    const PTS: [(f64, f64); 5] = [(1.0 / 24.0, 0.0), (1.0, 60.0), (7.0, 120.0), (60.0, 180.0), (330.0, 240.0)];
    let d = days.max(PTS[0].0);
    let mut hue = 240.0;
    for w in PTS.windows(2) {
        let ((d0, h0), (d1, h1)) = (w[0], w[1]);
        if d <= d1 {
            let t = (d.ln() - d0.ln()) / (d1.ln() - d0.ln());
            hue = h0 + (h1 - h0) * t.clamp(0.0, 1.0);
            break;
        }
    }
    Some(hls240(hue as f32 / 360.0 * 240.0, AGE_LUMA, AGE_SATURATION))
}

pub fn fmt_date(t: Option<SystemTime>) -> String {
    t.map(|t| DateTime::<Local>::from(t).format(crate::defaults::DATE_FORMAT).to_string()).unwrap_or_default()
}
pub fn fmt_time(t: Option<SystemTime>) -> String {
    t.map(|t| DateTime::<Local>::from(t).format(crate::defaults::TIME_FORMAT).to_string()).unwrap_or_default()
}
pub fn fmt_datetime(t: Option<SystemTime>) -> String {
    t.map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// Folder name for tabs and titles.
pub fn display_name(p: &Path) -> String {
    match p.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => {
            let s = p.to_string_lossy().into_owned();
            if s.is_empty() { "/".into() } else { s.trim_end_matches(['\\']).to_string() }
        }
    }
}

/// The full form of a folder named on the command line, so `.` shows as the folder it is
/// and keeps working as a tab. A link stays a link, as when walking into it; only `..`,
/// which POSIX `absolute` leaves in place, needs the real location.
pub fn full_folder(given: &Path) -> std::io::Result<PathBuf> {
    let full = std::path::absolute(given)?;
    let full = if full.components().any(|c| c == std::path::Component::ParentDir) { std::fs::canonicalize(&full)? } else { full };
    Ok(full.components().collect())
}

/// Ancestor chain from the root down to `p` (inclusive).
pub fn ancestors(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = p.ancestors().map(|a| a.to_path_buf()).collect();
    v.reverse();
    v.retain(|a| !a.as_os_str().is_empty());
    v
}

/// Collision-safe candidate retaining the original OS filename bytes.
pub fn unique_os_name(dir: &Path, name: &std::ffi::OsStr, is_dir: bool) -> PathBuf {
    let candidate = dir.join(name);
    if std::fs::symlink_metadata(&candidate).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return candidate;
    }
    let path = Path::new(name);
    let ext = if is_dir { None } else { path.extension() };
    let stem = if is_dir { name } else { path.file_stem().unwrap_or(name) };
    for n in 1.. {
        let mut next = stem.to_os_string();
        next.push(if n == 1 { " - Copy".into() } else { format!(" - Copy ({n})") });
        if let Some(ext) = ext { next.push("."); next.push(ext); }
        let candidate = dir.join(next);
        match std::fs::symlink_metadata(&candidate) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return candidate,
            Err(_) => return candidate, // The operation reports the actual access error.
            Ok(_) => {}
        }
    }
    unreachable!()
}

/// Writes `bytes` to `file` all at once: readers see the old contents or the new, never
/// half of them. Missing folders are made. `private` keeps it from other users on Unix.
pub fn write_atomic(file: &Path, bytes: &[u8], private: bool) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Per-process name: two running windows must not write one temporary file.
    let mut tmp_name = file.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = file.with_file_name(tmp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let written = options.open(&tmp).and_then(|mut out| {
        use std::io::Write;
        out.write_all(bytes)?;
        // On disk before the rename, or a power cut can leave an empty file.
        out.sync_all()
    });
    let result = written.and_then(|()| std::fs::rename(&tmp, file));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn valid_name(name: &str) -> bool {
    let bad: &[char] = if cfg!(windows) { &['\\', '/', ':', '*', '?', '"', '<', '>', '|'] } else { &['/'] };
    if name.is_empty() || name.contains('\0') || name.contains(bad) || name == "." || name == ".." { return false; }
    #[cfg(windows)]
    {
        if name.encode_utf16().count() > 255 || name.ends_with([' ', '.']) || name.chars().any(|c| c < ' ') { return false; }
        let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ').to_uppercase();
        if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str()) { return false; }
        if let Some(n) = stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
            && ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&n) { return false; }
        true
    }
    #[cfg(not(windows))]
    { name.len() <= 255 }
}

/// Names the system runs, or hands to something that runs them, instead of showing them.
const RUNNABLE_EXTS: &[&str] = &[
    "exe", "com", "scr", "pif", "cpl", "msi", "msp", "msc", "appx", "msix", "application",
    "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "jar", "reg", "inf",
    "lnk", "url", "scf", "appref-ms", "library-ms", "search-ms",
    "msu", "mst", "msixbundle", "appxbundle", "appinstaller", "vb", "ws", "wsc", "psd1", "ps1xml",
    "chm", "hlp", "settingcontent-ms", "diagcab", "gadget", "shb", "shs", "sct", "xbap",
    "sh", "bash", "run", "desktop", "appimage", "deb", "rpm",
];

/// Hand a file or folder to the system, which runs it if it is a program.
pub fn open_with_system(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    { crate::win::open(path) }
    #[cfg(not(windows))]
    { open::that_detached(path) }
}

pub fn is_runnable(name: &str) -> bool {
    // Windows drops trailing dots and spaces, so `setup.exe.` still runs as `setup.exe`.
    let ext = split_ext(name.trim_end_matches(['.', ' ']), false).to_ascii_lowercase();
    RUNNABLE_EXTS.contains(&ext.as_str())
}

/// Types `xdg-open` can hand to something that runs them. A file gets its type from its
/// contents or a link's target, so a harmless name proves nothing.
#[cfg(not(windows))]
const RUNNABLE_TYPES: &[&str] = &[
    "application/x-executable", "application/x-pie-executable", "application/x-sharedlib",
    "application/x-dosexec", "application/x-ms-dos-executable", "application/x-msdownload",
    "application/vnd.microsoft.portable-executable", "application/x-msi",
    "application/x-shellscript", "application/x-sh", "text/x-shellscript", "application/x-desktop",
    "application/java-archive", "application/x-java-archive", "application/x-jar",
    "application/vnd.appimage", "application/x-iso9660-appimage",
    "text/x-python", "text/x-script.python", "application/x-perl", "text/x-perl", "application/x-ruby",
    "application/vnd.debian.binary-package", "application/x-rpm", "application/vnd.flatpak.ref",
];

#[cfg(not(windows))]
pub fn is_runnable_type(mime: &str) -> bool {
    RUNNABLE_TYPES.contains(&mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str())
}

/// Links lead elsewhere and must never be walked recursively. On Windows this covers
/// symbolic links, junctions and mount points, but not placeholders such as OneDrive's,
/// which are real folders and files.
pub fn is_link_like(md: &std::fs::Metadata) -> bool {
    md.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sorting_in_parts_gives_what_a_stable_sort_gives() {
        for len in [0, 1, super::SORT_IN_PARTS_FROM, super::SORT_IN_PARTS_FROM + 7, 200_003] {
            let items: Vec<(u32, u32)> = (0..len as u32).map(|k| (k.wrapping_mul(2_654_435_761) >> 22, k)).collect();
            let mut expected = items.clone();
            expected.sort_by_key(|item| item.0);
            assert_eq!(super::sort_in_parts(items, |a, b| a.0.cmp(&b.0)), expected, "{len} items");
        }
    }

    use std::path::PathBuf;

    #[test]
    fn an_entry_keeps_only_what_the_path_does_not_say() {
        let plain = super::Entry::named(PathBuf::from("/x/notes.txt"), "notes.txt".into(), false, super::Kind::Other);
        assert_eq!((plain.name(), plain.lname(), plain.ext()), ("notes.txt", "notes.txt", "txt"));
        assert!(plain.custom_name.is_none() && plain.lower.is_none());
        let mixed = super::Entry::named(PathBuf::from("/x/Żółw.TAR"), "Żółw.TAR".into(), false, super::Kind::Other);
        assert_eq!((mixed.name(), mixed.lname(), mixed.ext()), ("Żółw.TAR", "żółw.tar", "TAR"));
        let drive = super::Entry::named(PathBuf::from("D:\\"), "Data (D:)".into(), true, super::Kind::Folder);
        assert_eq!((drive.name(), drive.ext()), ("Data (D:)", ""));
        let folder = super::Entry::named(PathBuf::from("/x/a.d"), "a.d".into(), true, super::Kind::Folder);
        assert_eq!(folder.ext(), "", "a folder has no extension");
        assert_eq!((plain.stem(), folder.stem()), ("notes", "a.d"));
    }

    #[test]
    fn every_sorting_is_a_consistent_order() {
        use std::cmp::Ordering::Greater;
        let root = std::env::temp_dir().join(format!("fc_sort_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in ["a", "c", "e"] { std::fs::create_dir_all(root.join(name)).unwrap(); }
        for (name, size) in [("b", 0), ("d", 0), ("f", 1), ("g.TXT", 0), ("h.txt", 2), ("Ż.Żx", 0), ("i.żx", 0)] {
            std::fs::write(root.join(name), vec![0; size]).unwrap();
        }
        let entries: Vec<_> = read_dir(&root).unwrap().into_iter().map(|e| (super::name_key(&e), e)).collect();
        assert_eq!(entries.len(), 10);
        for by in [SortBy::Name, SortBy::Date, SortBy::Size, SortBy::Type] {
            for ascending in [true, false] {
                for folders_on_top in [true, false] {
                    let s = Sorting { by, ascending, folders_on_top };
                    for a in &entries { for b in &entries { for c in &entries {
                        if super::compare_entries(a, b, s) != Greater && super::compare_entries(b, c, s) != Greater {
                            assert_ne!(super::compare_entries(a, c, s), Greater, "{s:?}: {} {} {}", a.1.name(), b.1.name(), c.1.name());
                        }
                    }}}
                }
            }
        }
        let by_type = Sorting { by: SortBy::Type, ascending: true, folders_on_top: false };
        let (mut upper, mut lower) = (None, None);
        for e in &entries {
            if e.1.name() == "g.TXT" { upper = Some(e); }
            if e.1.name() == "h.txt" { lower = Some(e); }
        }
        assert_eq!(super::compare_entries(upper.unwrap(), lower.unwrap(), by_type), std::cmp::Ordering::Less, "extensions ignore case");

        let file = root.join("f");
        let missing = root.join("missing");
        let kinds = super::probe_paths(&[root.clone(), file, missing], std::time::Duration::from_secs(5));
        assert_eq!(kinds, [Some(super::PathKind::Folder), Some(super::PathKind::File), Some(super::PathKind::Missing)]);
        std::fs::remove_dir_all(root).unwrap();
    }

    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_creation_names() {
        for bad in ["CON", "nul.txt", "COM1", "LPT9.log", "COM¹.txt", "con .txt", "CONIN$", "trailing.", "trailing ", "a:b", "line\nbreak", "..", ""] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for good in ["Zażółć 日本語.txt", "COM10.txt", "report", ".gitignore", " leading", "a..b"] { assert!(valid_name(good), "{good}"); }
        assert!(valid_name(&"ą".repeat(255))); assert!(!valid_name(&"ą".repeat(256)));
        assert!(valid_name(&"🙂".repeat(127))); assert!(!valid_name(&"🙂".repeat(128)));
    }
    #[test]
    fn runnable_names_cannot_hide_behind_case_or_trailing_characters() {
        for name in ["setup.exe", "SETUP.EXE", "start.bat", "photo.jpg.lnk", "setup.exe.", "setup.exe . ", "Install.desktop", "app.AppImage", "help.chm", "update.msu", "open.settingcontent-ms"] {
            assert!(is_runnable(name), "{name}");
        }
        for name in ["photo.jpg", "notes.txt", "exe", ".exe.txt", "archive.zip"] {
            assert!(!is_runnable(name), "{name}");
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn runnable_types_ignore_case_and_parameters() {
        for mime in ["application/java-archive", "APPLICATION/X-PIE-EXECUTABLE", "text/x-shellscript; charset=utf-8"] {
            assert!(is_runnable_type(mime), "{mime}");
        }
        for mime in ["text/plain", "image/jpeg", "application/zip", "application/pdf", ""] {
            assert!(!is_runnable_type(mime), "{mime}");
        }
    }

    #[test]
    fn natural() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("a", "B"), Ordering::Less);
    }
    #[cfg(windows)]
    #[test]
    fn windows_logical_sort() {
        let cmp = |a: &str, b: &str| crate::win::logical_cmp(&crate::win::wide(a), &crate::win::wide(b));
        assert_eq!(cmp("file2.txt", "file10.txt"), Ordering::Less);
        assert_eq!(cmp("REPORT.txt", "report.txt"), Ordering::Equal);
        assert_eq!(cmp("zażółć2.txt", "zażółć10.txt"), Ordering::Less);
    }
    #[test]
    fn a_folder_from_the_command_line_becomes_a_full_path() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(full_folder(Path::new(".")).unwrap(), here);
        assert_eq!(full_folder(Path::new("src/..")).unwrap(), here);
        // Path equality ignores a trailing separator, the text shown does not.
        assert_eq!(full_folder(Path::new("./src/")).unwrap().as_os_str(), here.join("src").as_os_str());
    }
    #[cfg(unix)]
    #[test]
    fn a_link_named_on_the_command_line_stays_a_link() {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("fc_full_folder_{}", std::process::id()));
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let full = full_folder(&root.join("link"));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(full.unwrap(), root.join("link"));
    }
    #[test]
    fn sizes() {
        assert_eq!(fmt_size(10_500_000, false), "10.5 MB");
        assert_eq!(fmt_size(512, false), "512 B");
    }
    #[test]
    fn shipped_formats_classify_without_expanding_preview_decoders() {
        for (ext, expected) in [
            ("CR2", Kind::Image), ("avif", Kind::Image), ("heic", Kind::Image),
            ("m2ts", Kind::Video), ("mka", Kind::Audio), ("docm", Kind::Document),
            ("qcow2", Kind::Archive), ("py", Kind::Program),
            ("gif", Kind::Image), ("m2v", Kind::Image), ("!!!", Kind::Text),
        ] {
            assert_eq!(Kind::from_ext(ext), expected, "{ext}");
        }
        for ext in ["cr2", "heic", "avif"] {
            assert!(!IMAGE_EXTS.contains(&ext), "classification must not enable unsupported preview: {ext}");
        }
    }
}
