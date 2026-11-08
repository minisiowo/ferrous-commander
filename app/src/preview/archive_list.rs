//! What an archive holds, read without unpacking it.

use std::io::{BufReader, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use super::load::{ArchiveEntry, Loaded};

const MAX_ENTRIES: usize = 5000;
/// A compressed tar has no index: every entry costs decompression up to it.
const SCAN_LIMIT: Duration = Duration::from_secs(4);

type Listing = (Vec<ArchiveEntry>, u64, bool);

fn zip(p: &Path) -> Result<Listing, String> {
    let file = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let total = archive.len();
    let mut entries = Vec::with_capacity(total.min(MAX_ENTRIES));
    for i in 0..total.min(MAX_ENTRIES) {
        let entry = archive.by_index_raw(i).map_err(|e| e.to_string())?;
        entries.push(ArchiveEntry { name: entry.name().map(|n| n.into_owned()).unwrap_or_default(), size: entry.size(), folder: entry.is_dir() });
    }
    Ok((entries, total as u64, total > MAX_ENTRIES))
}

fn seven(p: &Path) -> Result<Listing, String> {
    let archive = sevenz_rust2::Archive::open(p).map_err(|e| e.to_string())?;
    let total = archive.files.len();
    let entries = archive.files.iter().take(MAX_ENTRIES)
        .map(|f| ArchiveEntry { name: f.name.clone(), size: f.size, folder: f.is_directory }).collect();
    Ok((entries, total as u64, total > MAX_ENTRIES))
}

fn tar(reader: impl Read) -> Result<Listing, String> {
    let mut archive = ::tar::Archive::new(reader);
    let started = Instant::now();
    let mut entries = Vec::new();
    let mut trimmed = false;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        if entries.len() == MAX_ENTRIES || started.elapsed() > SCAN_LIMIT {
            trimmed = true;
            break;
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let header = entry.header();
        entries.push(ArchiveEntry {
            name: entry.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
            size: header.size().unwrap_or(0),
            folder: header.entry_type().is_dir(),
        });
    }
    let total = entries.len() as u64;
    Ok((entries, total, trimmed))
}

fn listing(p: &Path, name: &str, zip_signature: bool) -> Option<Result<Listing, String>> {
    let open = || std::fs::File::open(p).map(BufReader::new).map_err(|e| e.to_string());
    let tarred = |run: &dyn Fn() -> Result<Listing, String>| Some(run());
    if zip_signature || name.ends_with(".zip") { return Some(zip(p)); }
    if name.ends_with(".7z") { return Some(seven(p)); }
    if name.ends_with(".tar") { return tarred(&|| tar(open()?)); }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") { return tarred(&|| tar(flate2::bufread::MultiGzDecoder::new(open()?))); }
    if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") { return tarred(&|| tar(bzip2::bufread::MultiBzDecoder::new(open()?))); }
    if name.ends_with(".tar.xz") || name.ends_with(".txz") { return tarred(&|| tar(lzma_rust2::XzReader::new(open()?, true))); }
    if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        return tarred(&|| tar(ruzstd::decoding::StreamingDecoder::new(open()?).map_err(|e| e.to_string())?));
    }
    None
}

/// `None` when this is not a kind of archive that can be listed.
pub fn load(p: &Path, name: &str, zip_signature: bool) -> Option<Loaded> {
    Some(match listing(p, &name.to_ascii_lowercase(), zip_signature)? {
        Ok((entries, total, trimmed)) => Loaded::Archive { entries, total, trimmed },
        Err(_) => Loaded::Error("This archive could not be read. It may be damaged or protected with a password.".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::super::load::{load as load_file, scratch, whole};
    use super::*;

    const SAMPLES: &[&str] = &["sample.zip", "sample.7z", "sample.tar.gz", "sample.tar.bz2", "sample.tar.xz", "sample.tar.zst"];

    #[test]
    fn every_supported_archive_lists_its_contents() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testdata");
        for name in SAMPLES {
            let Loaded::Archive { entries, total, trimmed: false } = load_file(&data.join(name), whole(64)).0 else { panic!("{name} did not list") };
            assert!(!entries.is_empty(), "{name}");
            assert_eq!(total as usize, entries.len(), "{name}");
        }
    }

    #[test]
    fn a_locked_or_broken_archive_says_so() {
        let root = scratch("archive_broken");
        for name in SAMPLES {
            let file = root.join(name);
            std::fs::write(&file, b"this is no archive at all, only words").unwrap();
            assert!(matches!(load(&file, name, false), Some(Loaded::Error(_))), "{name}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
