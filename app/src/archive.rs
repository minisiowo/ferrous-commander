//! Unpacking archives into the folder that holds them, with no outside programs.
//!
//! An archive is untrusted input. Everything lands in a private stage beside the destination
//! and gets its final name only after the whole archive read cleanly, so a damaged or
//! cancelled archive leaves nothing behind and no existing file is ever replaced.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{fsx, ops};

#[path = "archive_tool.rs"]
mod tool;

pub use tool::detect_in_background as detect_tools;

/// `extract` fails with these exactly when the archive should be tried again with a password.
pub const PASSWORD_NEEDED: &str = "Password needed";
pub const PASSWORD_WRONG: &str = "Wrong password";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Packing { None, Gzip, Bzip2, Xz, Zstd }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Zip,
    SevenZ,
    Rar,
    Tar(Packing),
    /// One compressed file, such as `notes.txt.gz`.
    Single(Packing),
}

/// Formats that only look like archives (.jar, .docx, .apk) are left out on purpose: they
/// are documents and programs, and opening them must keep starting their own application.
/// Longer suffixes come first, so `.tar.gz` is not read as a compressed file.
const SUFFIXES: &[(&str, Format)] = &[
    (".tar.gz", Format::Tar(Packing::Gzip)),
    (".tar.bz2", Format::Tar(Packing::Bzip2)),
    (".tar.xz", Format::Tar(Packing::Xz)),
    (".tar.zst", Format::Tar(Packing::Zstd)),
    (".tgz", Format::Tar(Packing::Gzip)),
    (".tbz2", Format::Tar(Packing::Bzip2)),
    (".tbz", Format::Tar(Packing::Bzip2)),
    (".txz", Format::Tar(Packing::Xz)),
    (".tzst", Format::Tar(Packing::Zstd)),
    (".tar", Format::Tar(Packing::None)),
    (".zip", Format::Zip),
    (".7z", Format::SevenZ),
    (".rar", Format::Rar),
    (".gz", Format::Single(Packing::Gzip)),
    (".bz2", Format::Single(Packing::Bzip2)),
    (".xz", Format::Single(Packing::Xz)),
    (".zst", Format::Single(Packing::Zstd)),
];

/// The format, and the name left once the archive's suffix is gone.
fn identify(name: &str) -> Option<(Format, &str)> {
    let lower = name.to_ascii_lowercase();
    let (suffix, format) = SUFFIXES.iter().find(|(suffix, _)| lower.ends_with(suffix))?;
    // A dot or space would end the name that Windows refuses to keep as written.
    let stem = name[..name.len() - suffix.len()].trim_end_matches(['.', ' ']);
    (!stem.is_empty()).then_some((*format, stem))
}

pub fn is_archive(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return false };
    match identify(&name.to_string_lossy()) {
        // Without a tool to unpack it, a RAR file stays with its default application.
        Some((Format::Rar, _)) => tool::ready(),
        Some(_) => true,
        None => false,
    }
}

pub struct Extracted {
    pub path: PathBuf,
    /// Entries that could not be kept here, such as links leading out of the archive.
    pub skipped: usize,
}

/// Unpack next to `dest_dir`'s other items. One top-level item keeps its own name; anything
/// else gets a folder named after the archive, so it cannot scatter across the destination.
pub fn extract(archive: &Path, dest_dir: &Path, password: Option<&str>, cancel: &AtomicBool, report: &mut dyn FnMut(String)) -> Result<Extracted, String> {
    let name = archive.file_name().ok_or("Not an archive")?.to_string_lossy();
    let (format, stem) = identify(&name).ok_or("This archive format is not supported")?;
    ops::staged(dest_dir, |stage| {
        let root = stage.join("item");
        fs::create_dir(&root).map_err(|e| e.to_string())?;
        let mut sink = Sink {
            root: root.clone(), cancel, report, skipped: 0, unreadable: false, links: Vec::new(),
            #[cfg(windows)]
            zone: zone_of(archive),
        };
        let opened = File::open(archive).map_err(|e| fsx::friendly_err(&e))?;
        match format {
            Format::Zip => unzip(opened, password, &mut sink)?,
            Format::SevenZ => un7z(opened, password, &mut sink)?,
            Format::Rar => tool::unpack(archive, password, &mut sink)?,
            Format::Tar(packing) => untar(unpacked(opened, packing)?, &mut sink)?,
            Format::Single(packing) => {
                sink.file(Path::new(stem), None, None, &mut unpacked(opened, packing)?)?;
            }
        }
        sink.place_links()?;
        let skipped = sink.skipped;
        let top = fs::read_dir(&root).and_then(|items| items.collect::<Result<Vec<_>, _>>()).map_err(|e| e.to_string())?;
        let (payload, target) = match top.as_slice() {
            [] if skipped > 0 => return Err("Nothing in this archive can be unpacked here".into()),
            [] => return Err("The archive is empty".into()),
            [only] => {
                let is_dir = fs::symlink_metadata(only.path()).is_ok_and(|md| md.is_dir());
                (only.path(), fsx::unique_os_name(dest_dir, &only.file_name(), is_dir))
            }
            _ => (root, fsx::unique_os_name(dest_dir, stem.as_ref(), true)),
        };
        if cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
        ops::publish(&payload, &target, cancel).map_err(|e| e.to_string())?;
        Ok(Extracted { path: target, skipped })
    })
}

fn unpacked(file: File, packing: Packing) -> Result<Box<dyn Read>, String> {
    let file = BufReader::with_capacity(1 << 16, file);
    Ok(match packing {
        Packing::None => Box::new(file),
        Packing::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(file)),
        Packing::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(file)),
        Packing::Xz => Box::new(lzma_rust2::XzReader::new(file, true)),
        Packing::Zstd => Box::new(ZstdFrames { frame: None, source: Some(file) }),
    })
}

/// `cat a.zst b.zst` is valid zstd, but the decoder stops after its first frame.
struct ZstdFrames<R: BufRead> {
    frame: Option<ruzstd::decoding::StreamingDecoder<R, ruzstd::decoding::FrameDecoder>>,
    source: Option<R>,
}

impl<R: BufRead> Read for ZstdFrames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() { return Ok(0); }
        loop {
            if let Some(frame) = &mut self.frame {
                let n = frame.read(buf)?;
                if n > 0 { return Ok(n); }
                let (source, decoder) = self.frame.take().expect("frame is set").into_parts();
                if let (Some(stored), Some(actual)) = (decoder.get_checksum_from_data(), decoder.get_calculated_checksum())
                    && stored != actual {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "The archive is damaged (checksum mismatch)"));
                }
                self.source = Some(source);
            }
            let Some(mut source) = self.source.take() else { return Ok(0) };
            if source.fill_buf()?.is_empty() { return Ok(0); }
            self.frame = Some(ruzstd::decoding::StreamingDecoder::new(source).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?);
        }
    }
}

/// Everything an archive writes passes through here, whatever its format.
struct Sink<'a> {
    root: PathBuf,
    cancel: &'a AtomicBool,
    report: &'a mut dyn FnMut(String),
    skipped: usize,
    /// The archive's data stopped making sense, which under a password means a wrong one.
    /// Failing to write is a different matter, and says so.
    unreadable: bool,
    /// Symbolic links wait until every file is written, so no entry can write through one.
    links: Vec<(PathBuf, PathBuf)>,
    #[cfg(windows)]
    zone: Option<Vec<u8>>,
}

impl Sink<'_> {
    /// The checked path of an entry. `None` means leave it out; an entry that tries to leave
    /// the archive fails the whole archive, because nothing about it can be trusted.
    fn resolve(&mut self, raw: &Path) -> Result<Option<PathBuf>, String> {
        if self.cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
        let mut rel = PathBuf::new();
        for part in raw.components() {
            match part {
                Component::CurDir => {}
                Component::Normal(name) if fsx::valid_name(&name.to_string_lossy()) => rel.push(name),
                // A name this system cannot hold: the rest of the archive is still good.
                Component::Normal(_) => { self.skipped += 1; return Ok(None); }
                _ => return Err(refused(&raw.display())),
            }
        }
        if rel.as_os_str().is_empty() { return Ok(None); }
        (self.report)(rel.display().to_string());
        Ok(Some(rel))
    }

    /// The folder holding `rel`, created as needed. Refuses to pass through anything that is
    /// not a real folder. `None` means the path is taken by a file or a link.
    fn parent_of(&self, rel: &Path) -> Result<Option<PathBuf>, String> {
        let mut dir = self.root.clone();
        let mut parts = rel.components().peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() { break; }
            dir.push(part);
            match fs::symlink_metadata(&dir) {
                Ok(md) if md.is_dir() => {}
                Ok(_) => return Ok(None),
                Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&dir).map_err(|e| fsx::friendly_err(&e))?,
                Err(e) => return Err(fsx::friendly_err(&e)),
            }
        }
        Ok(Some(dir))
    }

    fn dir(&mut self, raw: &Path) -> Result<(), String> {
        let Some(rel) = self.resolve(raw)? else { return Ok(()) };
        let Some(parent) = self.parent_of(&rel)? else { self.skipped += 1; return Ok(()) };
        let path = parent.join(rel.file_name().unwrap_or_default());
        match fs::create_dir(&path) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&path).is_ok_and(|md| md.is_dir()) { self.skipped += 1; }
                Ok(())
            }
            done => done.map_err(|e| fsx::friendly_err(&e)),
        }
    }

    fn file(&mut self, raw: &Path, mode: Option<u32>, modified: Option<SystemTime>, content: &mut dyn Read) -> Result<(), String> {
        let Some(rel) = self.resolve(raw)? else { return Ok(()) };
        let Some(parent) = self.parent_of(&rel)? else { self.skipped += 1; return Ok(()) };
        let path = parent.join(rel.file_name().unwrap_or_default());
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // Setuid and friends stay behind. The owner keeps the right to read and remove the
        // file, and the umask applies as for any other new file.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode.map_or(0o666, |m| (m & 0o777) | 0o600));
        #[cfg(not(unix))]
        let _ = mode;
        let mut output = match options.open(&path) {
            // Two entries share a name, which includes names differing only by letter case
            // on Windows. Replacing one would hide that the other ever existed.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => { self.skipped += 1; return Ok(()); }
            opened => opened.map_err(|e| fsx::friendly_err(&e))?,
        };
        let mut buffer = vec![0; 1 << 16];
        loop {
            if self.cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
            let n = match content.read(&mut buffer) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => { self.unreadable = true; return Err(e.to_string()); }
            };
            if n == 0 { break; }
            output.write_all(&buffer[..n]).map_err(|e| fsx::friendly_err(&e))?;
        }
        if let Some(modified) = modified { let _ = output.set_modified(modified); }
        drop(output);
        #[cfg(windows)]
        self.mark_zone(&path);
        Ok(())
    }

    fn hard_link(&mut self, raw: &Path, target: &Path) -> Result<(), String> {
        let Some(rel) = self.resolve(raw)? else { return Ok(()) };
        let Some(target) = self.resolve(target)? else { self.skipped += 1; return Ok(()) };
        let source = self.root.join(target);
        // No symbolic link exists yet, so this path can only be something the archive wrote.
        let usable = fs::symlink_metadata(&source).is_ok_and(|md| md.is_file());
        let Some(parent) = self.parent_of(&rel)?.filter(|_| usable) else { self.skipped += 1; return Ok(()) };
        let path = parent.join(rel.file_name().unwrap_or_default());
        if fs::symlink_metadata(&path).is_ok() { self.skipped += 1; return Ok(()); }
        fs::hard_link(&source, &path).or_else(|_| fs::copy(&source, &path).map(|_| ())).map_err(|e| fsx::friendly_err(&e))
    }

    fn symlink(&mut self, raw: &Path, target: PathBuf) -> Result<(), String> {
        if let Some(rel) = self.resolve(raw)? { self.links.push((rel, target)); }
        Ok(())
    }

    /// Keep only the links that stay inside the archive once all of them exist. Names alone
    /// cannot show that: `b -> a/..` looks harmless, yet leaves when `a` is itself a link.
    #[cfg(unix)]
    fn place_links(&mut self) -> Result<(), String> {
        let mut placed = Vec::new();
        for (rel, target) in std::mem::take(&mut self.links) {
            let parent = if target.is_absolute() { None } else { self.parent_of(&rel)? };
            let Some(parent) = parent else { self.skipped += 1; continue };
            let path = parent.join(rel.file_name().unwrap_or_default());
            // Refused on a filesystem without links, such as a FAT pendrive.
            match std::os::unix::fs::symlink(&target, &path) {
                Ok(()) => placed.push(path),
                Err(_) => self.skipped += 1,
            }
        }
        self.keep_inside(placed)
    }

    #[cfg(unix)]
    fn keep_inside(&mut self, links: Vec<PathBuf>) -> Result<(), String> {
        if links.is_empty() { return Ok(()); }
        let root = fs::canonicalize(&self.root).map_err(|e| e.to_string())?;
        for path in links {
            if !fs::canonicalize(&path).is_ok_and(|target| target.starts_with(&root)) {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
                self.skipped += 1;
            }
        }
        Ok(())
    }

    /// Making a link on Windows needs a privilege most accounts lack.
    #[cfg(not(unix))]
    fn place_links(&mut self) -> Result<(), String> {
        self.skipped += std::mem::take(&mut self.links).len();
        Ok(())
    }

    #[cfg(not(unix))]
    fn keep_inside(&mut self, links: Vec<PathBuf>) -> Result<(), String> {
        for path in links {
            fs::remove_file(&path).or_else(|_| fs::remove_dir(&path)).map_err(|e| e.to_string())?;
            self.skipped += 1;
        }
        Ok(())
    }

    /// Hold what an outside tool left to the rules every other format follows.
    fn settle(&mut self) -> Result<(), String> {
        let mut pending = vec![self.root.clone()];
        let mut links = Vec::new();
        while let Some(dir) = pending.pop() {
            for item in fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let path = item.map_err(|e| e.to_string())?.path();
                let md = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                if fsx::is_link_like(&md) {
                    links.push(path);
                } else if md.is_dir() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&path, fs::Permissions::from_mode(md.permissions().mode() & 0o777 | 0o700)).map_err(|e| e.to_string())?;
                    }
                    pending.push(path);
                } else if md.is_file() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = md.permissions().mode();
                        if mode & 0o7000 != 0 {
                            fs::set_permissions(&path, fs::Permissions::from_mode(mode & 0o777)).map_err(|e| e.to_string())?;
                        }
                    }
                    #[cfg(windows)]
                    self.mark_zone(&path);
                } else {
                    fs::remove_file(&path).map_err(|e| e.to_string())?;
                    self.skipped += 1;
                }
            }
        }
        self.keep_inside(links)
    }

    #[cfg(windows)]
    fn mark_zone(&self, path: &Path) {
        if let Some(zone) = &self.zone {
            let mut stream = path.as_os_str().to_os_string();
            stream.push(":Zone.Identifier");
            let _ = fs::write(stream, zone);
        }
    }
}

/// What Windows knows about where a download came from. Without passing it on, a program
/// inside a downloaded archive would run with no SmartScreen check.
#[cfg(windows)]
fn zone_of(archive: &Path) -> Option<Vec<u8>> {
    let mut stream = archive.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    fs::read(stream).ok()
}

/// A link's target is stored as the entry's content. Nothing legitimate needs more than this.
const LINK_TARGET_LIMIT: u64 = 1 << 16;

fn link_target(content: &mut dyn Read) -> Result<PathBuf, String> {
    let mut target = Vec::new();
    content.take(LINK_TARGET_LIMIT).read_to_end(&mut target).map_err(|e| e.to_string())?;
    Ok(PathBuf::from(String::from_utf8_lossy(&target).into_owned()))
}

fn refused(entry: &dyn std::fmt::Display) -> String {
    format!("Refused: this archive tries to write outside its folder ({entry})")
}

fn is_link_mode(mode: u32) -> bool { mode & 0o170000 == 0o120000 }

fn untar(reader: Box<dyn Read>, sink: &mut Sink) -> Result<(), String> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.header().entry_type();
        // `git archive` adds this record; it holds metadata, not a file.
        if kind.is_pax_global_extensions() { continue; }
        // Windows cannot hold a name that is not Unicode.
        let Ok(path) = entry.path().map(|p| p.into_owned()) else { sink.skipped += 1; continue };
        if kind.is_dir() {
            sink.dir(&path)?;
        } else if kind.is_symlink() || kind.is_hard_link() {
            let Ok(Some(target)) = entry.link_name().map(|t| t.map(|t| t.into_owned())) else { sink.skipped += 1; continue };
            if kind.is_symlink() { sink.symlink(&path, target)?; } else { sink.hard_link(&path, &target)?; }
        } else if kind.is_file() || kind.is_gnu_sparse() {
            let mode = entry.header().mode().ok();
            let modified = entry.header().mtime().ok().filter(|&t| t > 0).map(|t| UNIX_EPOCH + Duration::from_secs(t));
            sink.file(&path, mode, modified, &mut entry)?;
        } else {
            // Devices and named pipes have no place in an unpacked folder.
            sink.skipped += 1;
        }
    }
    Ok(())
}

fn unzip(file: File, password: Option<&str>, sink: &mut Sink) -> Result<(), String> {
    use chrono::TimeZone;
    let mut archive = zip::ZipArchive::new(BufReader::with_capacity(1 << 16, file)).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let encrypted = archive.by_index_raw(i).map_err(|e| e.to_string())?.encrypted();
        let entry = match (encrypted, password) {
            (true, None) => return Err(PASSWORD_NEEDED.into()),
            (true, Some(password)) => archive.by_index_decrypt(i, password.as_bytes()),
            (false, _) => archive.by_index(i),
        };
        let mut entry = match entry {
            Err(zip::result::ZipError::InvalidPassword) => return Err(PASSWORD_WRONG.into()),
            opened => opened.map_err(|e| e.to_string())?,
        };
        // ZIP paths use forward slashes, yet some Windows tools write backslashes.
        let path = PathBuf::from(entry.name().map_err(|e| e.to_string())?.replace('\\', "/"));
        if entry.is_dir() {
            sink.dir(&path)?;
        } else if entry.is_symlink() {
            let target = link_target(&mut entry)?;
            sink.symlink(&path, target)?;
        } else {
            let modified = entry.last_modified()
                .and_then(|t| chrono::NaiveDateTime::try_from(t).ok())
                .and_then(|t| chrono::Local.from_local_datetime(&t).earliest())
                .map(SystemTime::from);
            let mode = entry.unix_mode();
            // Old ZIP encryption lets one wrong password in 256 past its check, and only the
            // finished file's checksum shows it.
            sink.file(&path, mode, modified, &mut entry)
                .map_err(|e| if encrypted && sink.unreadable { PASSWORD_WRONG.into() } else { e })?;
        }
    }
    Ok(())
}

/// The bit saying that a 7z entry's attributes carry Unix permissions in their upper half.
const UNIX_EXTENSION: u32 = 0x8000;

fn un7z(file: File, password: Option<&str>, sink: &mut Sink) -> Result<(), String> {
    use sevenz_rust2::{ArchiveReader, Error, Password};
    let describe = |e: Error| match e {
        // A password is only ever given to an archive that asked for one, and a wrong one
        // turns its data into noise that can fail in any way.
        _ if password.is_some() => PASSWORD_WRONG.to_string(),
        Error::PasswordRequired | Error::MaybeBadPassword(_) => PASSWORD_NEEDED.to_string(),
        Error::Other(message) => message.into_owned(),
        other => other.to_string(),
    };
    let mut reader = ArchiveReader::new(BufReader::with_capacity(1 << 16, file), password.map_or_else(Password::empty, Password::new)).map_err(describe)?;
    let mut failure = None;
    let result = reader.for_each_entries(|entry, content| {
        let path = PathBuf::from(entry.name().replace('\\', "/"));
        let mode = (entry.has_windows_attributes && entry.windows_attributes & UNIX_EXTENSION != 0)
            .then_some(entry.windows_attributes >> 16);
        let done = if entry.is_anti_item() {
            Ok(())
        } else if entry.is_directory() {
            sink.dir(&path)
        } else if mode.is_some_and(is_link_mode) {
            link_target(content).and_then(|target| sink.symlink(&path, target))
        } else {
            let modified = entry.has_last_modified_date.then(|| entry.last_modified_date.into());
            sink.file(&path, mode, modified, content)
        };
        // The library's error type has no room for the message that matters here.
        done.map(|()| true).map_err(|e| { failure = Some(e); Error::Other("".into()) })
    });
    match (failure, result) {
        (Some(_), _) if sink.unreadable && password.is_some() => Err(PASSWORD_WRONG.into()),
        (Some(e), _) => Err(e),
        (None, result) => result.map_err(describe),
    }
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
