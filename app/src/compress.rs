//! Packing files and folders into a new archive, with no outside programs.
//!
//! The archive is written in a private stage beside the destination and gets its name only
//! when it is complete, so a failed or cancelled job leaves nothing behind and no existing
//! file is ever replaced. Links are stored as links and never followed. An item that cannot
//! be read stops the job, because an archive that silently lacks a file is worse than none.

use std::fs::{self, File, Metadata};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{fsx, ops};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind { Zip, SevenZ, Tar, TarGz, TarBz2, TarXz }

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Zip, Kind::SevenZ, Kind::Tar, Kind::TarGz, Kind::TarBz2, Kind::TarXz];

    pub fn suffix(self) -> &'static str {
        match self {
            Kind::Zip => ".zip",
            Kind::SevenZ => ".7z",
            Kind::Tar => ".tar",
            Kind::TarGz => ".tar.gz",
            Kind::TarBz2 => ".tar.bz2",
            Kind::TarXz => ".tar.xz",
        }
    }
}

/// The archive's name for what was typed. Typing the suffix as well does not double it.
pub fn file_name(typed: &str, kind: Kind) -> String {
    let typed = typed.trim();
    let suffix = kind.suffix();
    let has_suffix = typed.len() > suffix.len()
        && typed.get(typed.len() - suffix.len()..).is_some_and(|end| end.eq_ignore_ascii_case(suffix));
    if has_suffix { typed.to_string() } else { format!("{typed}{suffix}") }
}

/// The name to offer: the item's own for one item, the folder's for several.
pub fn suggested_name(sources: &[PathBuf], folder: &Path) -> String {
    let name = match sources {
        [one] if one.is_dir() => one.file_name(),
        [one] => one.file_stem(),
        _ => folder.file_name(),
    };
    name.map_or_else(|| "Archive".to_string(), |n| n.to_string_lossy().into_owned())
}

/// Drops items already inside another selected folder, which the tree view allows.
pub fn outermost(sources: Vec<PathBuf>) -> Vec<PathBuf> {
    crate::fsx::outermost_paths(sources)
}

pub struct Packed {
    pub name: std::ffi::OsString,
    /// Items an archive cannot hold, such as sockets, or names this format cannot store.
    pub skipped: usize,
}

/// Pack `sources` as the top level of a new archive in `dest_dir`.
pub fn pack(sources: &[PathBuf], dest_dir: &Path, name: &str, kind: Kind, cancel: &AtomicBool, report: &mut dyn FnMut(String)) -> Result<Packed, String> {
    let mut seen = std::collections::HashSet::new();
    for source in sources {
        let item = source.file_name().ok_or("A drive cannot be packed")?;
        if dest_dir.starts_with(source) {
            return Err(format!("{} would have to contain its own archive", item.to_string_lossy()));
        }
        let key = if cfg!(windows) { item.to_string_lossy().to_lowercase().into() } else { item.to_os_string() };
        if !seen.insert(key) {
            return Err(format!("Two selected items are both named {}. Nothing was packed", item.to_string_lossy()));
        }
    }
    ops::staged(dest_dir, |stage| {
        let draft = stage.join("archive");
        let file = fs::OpenOptions::new().write(true).create_new(true).open(&draft).map_err(|e| fsx::friendly_err(&e))?;
        let mut out = open(BufWriter::with_capacity(1 << 16, file), kind)?;
        let mut walk = Walk { cancel, report, skipped: 0, stored: 0 };
        for source in sources {
            walk.add(source, Path::new(source.file_name().unwrap_or_default()), out.as_mut())
                .map_err(|e| if cancel.load(Ordering::Relaxed) { "Cancelled".to_string() } else { e })?;
        }
        if walk.stored == 0 { return Err("Nothing here can be packed".into()); }
        let skipped = walk.skipped;
        out.finish().map_err(|e| fsx::friendly_err(&e))?;
        if cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
        let target = free_name(dest_dir, name, kind);
        ops::publish(&draft, &target, cancel).map_err(|e| fsx::friendly_err(&e))?;
        Ok(Packed { name: target.file_name().unwrap_or_default().to_os_string(), skipped })
    })
}

/// `name` if it is free, else `stem (2).tar.gz`, keeping the whole suffix together.
fn free_name(dir: &Path, name: &str, kind: Kind) -> PathBuf {
    let stem = &name[..name.len() - kind.suffix().len()];
    let suffix = &name[name.len() - kind.suffix().len()..];
    let free = |candidate: &Path| fs::symlink_metadata(candidate).is_err_and(|e| e.kind() == io::ErrorKind::NotFound);
    let first = dir.join(name);
    if free(&first) { return first; }
    (2..).map(|n| dir.join(format!("{stem} ({n}){suffix}"))).find(|c| free(c)).unwrap_or(first)
}

struct Walk<'a> {
    cancel: &'a AtomicBool,
    report: &'a mut dyn FnMut(String),
    skipped: usize,
    stored: usize,
}

impl Walk<'_> {
    fn add(&mut self, path: &Path, rel: &Path, out: &mut dyn Output) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
        (self.report)(fsx::display_name(path));
        let failed = |e: io::Error| format!("{}: {}", rel.display(), fsx::friendly_err(&e));
        let md = fs::symlink_metadata(path).map_err(failed)?;
        if !out.holds(rel) { self.skipped += 1; return Ok(()); }
        let entry = Entry { name: rel, md: &md };
        let kept = if md.file_type().is_symlink() {
            self.link(path, &entry, out).map_err(failed)?
        } else if md.is_dir() {
            out.dir(&entry).map_err(failed)?;
            let mut children = fs::read_dir(path).and_then(|items| items.collect::<Result<Vec<_>, _>>()).map_err(failed)?;
            children.sort_by_key(|child| child.file_name());
            for child in children {
                self.add(&child.path(), &rel.join(child.file_name()), out)?;
            }
            true
        } else if md.is_file() {
            let file = File::open(path).map_err(failed)?;
            out.file(&entry, &mut Exact { file, left: md.len(), cancel: self.cancel }).map_err(failed)?;
            true
        } else {
            // Reading a pipe or a device would wait forever or never end.
            false
        };
        if kept { self.stored += 1 } else { self.skipped += 1 }
        Ok(())
    }

    #[cfg(unix)]
    fn link(&mut self, path: &Path, entry: &Entry, out: &mut dyn Output) -> io::Result<bool> {
        out.link(entry, &fs::read_link(path)?)
    }

    /// A Windows link holds an absolute path that means nothing on another computer.
    #[cfg(not(unix))]
    fn link(&mut self, _path: &Path, _entry: &Entry, _out: &mut dyn Output) -> io::Result<bool> { Ok(false) }
}

/// One file's content: exactly the size already recorded for it. A tar header announces the
/// size first, so a file that shrinks while being read would corrupt everything after it.
struct Exact<'a> {
    file: File,
    left: u64,
    cancel: &'a AtomicBool,
}

impl Read for Exact<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) { return Err(io::Error::other("Cancelled")); }
        let room = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if room == 0 { return Ok(0); }
        let n = self.file.read(&mut buf[..room])?;
        if n == 0 { return Err(io::Error::other("the file changed while it was being packed")); }
        self.left -= n as u64;
        Ok(n)
    }
}

struct Entry<'a> {
    name: &'a Path,
    md: &'a Metadata,
}

impl Entry<'_> {
    #[cfg(unix)]
    fn unix_mode(&self) -> u32 { std::os::unix::fs::MetadataExt::mode(self.md) }
}

trait Output {
    /// Whether this format can store the name at all.
    fn holds(&self, _name: &Path) -> bool { true }
    fn dir(&mut self, entry: &Entry) -> io::Result<()>;
    fn file(&mut self, entry: &Entry, content: &mut dyn Read) -> io::Result<()>;
    /// False when the link cannot be stored.
    #[cfg(unix)]
    fn link(&mut self, entry: &Entry, target: &Path) -> io::Result<bool>;
    fn finish(self: Box<Self>) -> io::Result<()>;
}

type Disk = BufWriter<File>;

fn open(file: Disk, kind: Kind) -> Result<Box<dyn Output>, String> {
    let tar = |stream| -> Result<Box<dyn Output>, String> { Ok(Box::new(Tar(tar::Builder::new(stream)))) };
    match kind {
        Kind::Zip => Ok(Box::new(Zip(zip::ZipWriter::new(file)))),
        Kind::SevenZ => Ok(Box::new(SevenZ(sevenz_rust2::ArchiveWriter::new(file).map_err(|e| e.to_string())?))),
        Kind::Tar => tar(Stream::Plain(file)),
        Kind::TarGz => tar(Stream::Gzip(flate2::write::GzEncoder::new(file, flate2::Compression::default()))),
        Kind::TarBz2 => tar(Stream::Bzip2(bzip2::write::BzEncoder::new(file, bzip2::Compression::default()))),
        Kind::TarXz => tar(Stream::Xz(lzma_rust2::XzWriter::new(file, lzma_rust2::XzOptions::with_preset(6)).map_err(|e| e.to_string())?)),
    }
}

/// `a/b` whatever the system's separator is, for the formats that store text.
fn text_name(name: &Path) -> Option<String> {
    let parts: Option<Vec<&str>> = name.iter().map(|part| part.to_str()).collect();
    Some(parts?.join("/"))
}

enum Stream {
    Plain(Disk),
    Gzip(flate2::write::GzEncoder<Disk>),
    Bzip2(bzip2::write::BzEncoder<Disk>),
    Xz(lzma_rust2::XzWriter<Disk>),
}

impl Stream {
    fn finish(self) -> io::Result<()> {
        let mut disk = match self {
            Stream::Plain(disk) => disk,
            Stream::Gzip(encoder) => encoder.finish()?,
            Stream::Bzip2(encoder) => encoder.finish()?,
            Stream::Xz(encoder) => encoder.finish()?,
        };
        disk.flush()
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(w) => w.write(buf),
            Stream::Gzip(w) => w.write(buf),
            Stream::Bzip2(w) => w.write(buf),
            Stream::Xz(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(w) => w.flush(),
            Stream::Gzip(w) => w.flush(),
            Stream::Bzip2(w) => w.flush(),
            Stream::Xz(w) => w.flush(),
        }
    }
}

struct Tar(tar::Builder<Stream>);

fn tar_header(md: &Metadata) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_metadata_in_mode(md, tar::HeaderMode::Complete);
    header
}

impl Output for Tar {
    fn dir(&mut self, entry: &Entry) -> io::Result<()> {
        self.0.append_data(&mut tar_header(entry.md), entry.name, io::empty())
    }

    fn file(&mut self, entry: &Entry, content: &mut dyn Read) -> io::Result<()> {
        self.0.append_data(&mut tar_header(entry.md), entry.name, content)
    }

    #[cfg(unix)]
    fn link(&mut self, entry: &Entry, target: &Path) -> io::Result<bool> {
        self.0.append_link(&mut tar_header(entry.md), entry.name, target)?;
        Ok(true)
    }

    fn finish(self: Box<Self>) -> io::Result<()> { self.0.into_inner()?.finish() }
}

struct Zip(zip::ZipWriter<Disk>);

fn zip_options(md: &Metadata) -> zip::write::SimpleFileOptions {
    let mut options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        // Decided before the data is seen, and only needed near the 4 GB limit.
        .large_file(md.len() >= 1 << 31);
    if let Some(time) = md.modified().ok()
        .map(chrono::DateTime::<chrono::Local>::from)
        .and_then(|time| zip::DateTime::try_from(time.naive_local()).ok()) {
        options = options.last_modified_time(time);
    }
    #[cfg(unix)]
    { options = options.unix_permissions(std::os::unix::fs::MetadataExt::mode(md)); }
    options
}

impl Output for Zip {
    fn holds(&self, name: &Path) -> bool { name.to_str().is_some() }

    fn dir(&mut self, entry: &Entry) -> io::Result<()> {
        let name = text_name(entry.name).ok_or(io::ErrorKind::InvalidFilename)?;
        self.0.add_directory(name, zip_options(entry.md)).map_err(io::Error::other)
    }

    fn file(&mut self, entry: &Entry, content: &mut dyn Read) -> io::Result<()> {
        let name = text_name(entry.name).ok_or(io::ErrorKind::InvalidFilename)?;
        self.0.start_file(name, zip_options(entry.md)).map_err(io::Error::other)?;
        io::copy(content, &mut self.0)?;
        Ok(())
    }

    #[cfg(unix)]
    fn link(&mut self, entry: &Entry, target: &Path) -> io::Result<bool> {
        let (Some(name), Some(target)) = (text_name(entry.name), target.to_str()) else { return Ok(false) };
        self.0.add_symlink(name, target, zip_options(entry.md)).map_err(io::Error::other)?;
        Ok(true)
    }

    fn finish(self: Box<Self>) -> io::Result<()> {
        self.0.finish().map_err(io::Error::other)?.flush()
    }
}

struct SevenZ(sevenz_rust2::ArchiveWriter<Disk>);

fn seven_entry(entry: &Entry, mut made: sevenz_rust2::ArchiveEntry) -> sevenz_rust2::ArchiveEntry {
    if let Some(time) = entry.md.modified().ok().and_then(|time| sevenz_rust2::NtTime::try_from(time).ok()) {
        made.last_modified_date = time;
        made.has_last_modified_date = true;
    }
    #[cfg(unix)]
    {
        // Attributes keep FILE_ATTRIBUTE_UNIX_EXTENSION's layout, as 7-Zip itself writes them.
        made.has_windows_attributes = true;
        made.windows_attributes = 0x8000 | (entry.unix_mode() << 16);
    }
    made
}

impl Output for SevenZ {
    fn holds(&self, name: &Path) -> bool { name.to_str().is_some() }

    fn dir(&mut self, entry: &Entry) -> io::Result<()> {
        let name = text_name(entry.name).ok_or(io::ErrorKind::InvalidFilename)?;
        let made = seven_entry(entry, sevenz_rust2::ArchiveEntry::new_directory(&name));
        self.0.push_archive_entry::<io::Empty>(made, None).map_err(io::Error::other)?;
        Ok(())
    }

    fn file(&mut self, entry: &Entry, content: &mut dyn Read) -> io::Result<()> {
        let name = text_name(entry.name).ok_or(io::ErrorKind::InvalidFilename)?;
        let made = seven_entry(entry, sevenz_rust2::ArchiveEntry::new_file(&name));
        // An empty file has no data block in 7z. libarchive, behind many Linux archive
        // managers, reports a zero-length block as a truncated archive.
        self.0.push_archive_entry(made, (entry.md.len() > 0).then_some(content)).map_err(io::Error::other)?;
        Ok(())
    }

    #[cfg(unix)]
    fn link(&mut self, entry: &Entry, target: &Path) -> io::Result<bool> {
        let Some(name) = text_name(entry.name) else { return Ok(false) };
        let made = seven_entry(entry, sevenz_rust2::ArchiveEntry::new_file(&name));
        let target = std::os::unix::ffi::OsStrExt::as_bytes(target.as_os_str());
        self.0.push_archive_entry(made, Some(target)).map_err(io::Error::other)?;
        Ok(true)
    }

    fn finish(self: Box<Self>) -> io::Result<()> {
        self.0.finish()?.flush()
    }
}

#[cfg(test)]
#[path = "compress_tests.rs"]
mod tests;
