use super::*;
use std::sync::atomic::AtomicBool;

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fc_archive_{name}_{}_{n}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn run(archive: &Path, dest: &Path) -> Result<Extracted, String> {
    extract(archive, dest, &AtomicBool::new(false), &mut |_| {})
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn tar_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(1_000_000_000);
        header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        builder.append(&header, *data).unwrap();
    }
    builder.into_inner().unwrap()
}

#[cfg(unix)]
fn link_entry(builder: &mut tar::Builder<Vec<u8>>, kind: tar::EntryType, name: &str, target: &str) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(0);
    header.set_mode(0o777);
    header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
    header.as_old_mut().linkname[..target.len()].copy_from_slice(target.as_bytes());
    header.set_cksum();
    builder.append(&header, io::empty()).unwrap();
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn recognises_archives_by_name_only() {
    for name in ["a.zip", "a.ZIP", "a.b.tar.gz", "a.tgz", "a.tar", "a.7z", "a.tar.zst", "a.tar.xz", "a.tar.bz2", "a.txt.gz", "a.xz"] {
        assert!(is_archive(Path::new(name)), "{name}");
    }
    for name in ["a.txt", "a.jar", "a.docx", "a.apk", "a.rar", ".zip", "zip", "a.tar.gz.part"] {
        assert!(!is_archive(Path::new(name)), "{name}");
    }
    assert_eq!(identify("x.y.tar.gz"), Some((Format::Tar(Packing::Gzip), "x.y")));
    assert_eq!(identify("notes.txt.GZ"), Some((Format::Single(Packing::Gzip), "notes.txt")));
    assert_eq!(identify("trailing. .zip"), Some((Format::Zip, "trailing")));
}

#[test]
fn every_supported_format_unpacks() {
    // Compressed by the real tools. These formats have no writer among our dependencies.
    let made_by_tools: &[(&str, &[u8])] = &[
        ("sample.tar.xz", include_bytes!("testdata/sample.tar.xz")),
        ("sample.tar.zst", include_bytes!("testdata/sample.tar.zst")),
        ("sample.tar.bz2", include_bytes!("testdata/sample.tar.bz2")),
        ("sample.tar.gz", include_bytes!("testdata/sample.tar.gz")),
        ("sample.zip", include_bytes!("testdata/sample.zip")),
        ("sample.7z", include_bytes!("testdata/sample.7z")),
    ];
    for (name, bytes) in made_by_tools {
        let folder = Folder::new("formats");
        let archive = folder.0.join(name);
        fs::write(&archive, bytes).unwrap();
        let done = run(&archive, &folder.0).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(done.path, folder.0.join("sample"), "{name}");
        assert_eq!(fs::read(done.path.join("top.txt")).unwrap(), b"top\n", "{name}");
        assert_eq!(fs::read(done.path.join("inner/żółć.txt")).unwrap(), b"inner\n", "{name}");
        assert_eq!(names(&folder.0).len(), 2, "{name}: {:?}", names(&folder.0));
    }
}

#[test]
fn single_compressed_files_keep_their_name_including_concatenated_streams() {
    let folder = Folder::new("single");
    let archive = folder.0.join("notes.txt.gz");
    let mut packed = Vec::new();
    for part in [&b"one "[..], b"two"] {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(part).unwrap();
        packed.extend(encoder.finish().unwrap());
    }
    fs::write(&archive, packed).unwrap();
    let done = run(&archive, &folder.0).unwrap();
    assert_eq!(done.path, folder.0.join("notes.txt"));
    assert_eq!(fs::read(done.path).unwrap(), b"one two");

    let archive = folder.0.join("data.zst");
    let mut packed = ruzstd::encoding::compress_to_vec(&b"first "[..], ruzstd::encoding::CompressionLevel::Fastest);
    packed.extend(ruzstd::encoding::compress_to_vec(&b"second"[..], ruzstd::encoding::CompressionLevel::Fastest));
    fs::write(&archive, packed).unwrap();
    assert_eq!(fs::read(run(&archive, &folder.0).unwrap().path).unwrap(), b"first second");
}

#[test]
fn a_single_top_folder_is_not_wrapped_and_loose_files_are() {
    let folder = Folder::new("layout");
    let wrapped = folder.0.join("wrapped.tar");
    fs::write(&wrapped, tar_of(&[("project/a.txt", b"a"), ("project/b.txt", b"b")])).unwrap();
    assert_eq!(run(&wrapped, &folder.0).unwrap().path, folder.0.join("project"));

    let loose = folder.0.join("loose.zip");
    fs::write(&loose, zip_of(&[("a.txt", b"a"), ("b.txt", b"b")])).unwrap();
    let done = run(&loose, &folder.0).unwrap();
    assert_eq!(done.path, folder.0.join("loose"));
    assert_eq!(names(&done.path), ["a.txt", "b.txt"]);
}

#[test]
fn existing_items_are_never_touched() {
    let folder = Folder::new("existing");
    fs::write(folder.0.join("a.txt"), b"mine").unwrap();
    let archive = folder.0.join("a.tar");
    fs::write(&archive, tar_of(&[("a.txt", b"theirs")])).unwrap();
    let done = run(&archive, &folder.0).unwrap();
    assert_eq!(fs::read(folder.0.join("a.txt")).unwrap(), b"mine");
    assert_eq!(fs::read(done.path).unwrap(), b"theirs");
}

#[test]
fn entries_leaving_the_archive_fail_it_and_leave_nothing() {
    for raw in ["../escaped.txt", "a/../../escaped.txt", "/tmp/fc_archive_absolute.txt"] {
        let folder = Folder::new("traversal");
        let dest = folder.0.join("dest");
        fs::create_dir(&dest).unwrap();
        let archive = folder.0.join("evil.zip");
        fs::write(&archive, zip_of(&[("good.txt", b"x"), (raw, b"evil")])).unwrap();
        let error = run(&archive, &dest).err().unwrap_or_else(|| panic!("{raw} was accepted"));
        assert!(error.contains("outside"), "{raw}: {error}");
        assert!(names(&dest).is_empty(), "{raw}: {:?}", names(&dest));
        assert_eq!(names(&folder.0), ["dest", "evil.zip"], "{raw}");
    }
}

#[test]
fn backslash_traversal_in_zip_is_refused_on_every_system() {
    let folder = Folder::new("backslash");
    let dest = folder.0.join("dest");
    fs::create_dir(&dest).unwrap();
    let archive = folder.0.join("evil.zip");
    fs::write(&archive, zip_of(&[("..\\..\\escaped.txt", b"evil")])).unwrap();
    assert!(run(&archive, &dest).is_err());
    assert!(names(&dest).is_empty());
}

#[test]
fn damaged_and_cancelled_archives_leave_nothing() {
    let folder = Folder::new("damaged");
    let dest = folder.0.join("dest");
    fs::create_dir(&dest).unwrap();
    let archive = folder.0.join("broken.tar.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_of(&[("a.txt", &[7; 100_000]), ("b.txt", &[8; 100_000])])).unwrap();
    let packed = encoder.finish().unwrap();
    fs::write(&archive, &packed[..packed.len() / 2]).unwrap();
    assert!(run(&archive, &dest).is_err());
    assert!(names(&dest).is_empty(), "{:?}", names(&dest));

    fs::write(&archive, &packed).unwrap();
    let cancel = AtomicBool::new(true);
    assert_eq!(extract(&archive, &dest, &cancel, &mut |_| {}).err().as_deref(), Some("Cancelled"));
    assert!(names(&dest).is_empty(), "{:?}", names(&dest));

    let junk = folder.0.join("junk.zip");
    fs::write(&junk, b"this is not an archive").unwrap();
    assert!(run(&junk, &dest).is_err());
    assert!(names(&dest).is_empty(), "{:?}", names(&dest));
}

#[test]
fn password_protected_zip_says_so() {
    let folder = Folder::new("password");
    let archive = folder.0.join("secret.zip");
    let mut bytes = zip_of(&[("a.txt", b"a")]);
    // Set the "encrypted" flag in the local and the central header.
    for signature in [&b"PK\x03\x04"[..], b"PK\x01\x02"] {
        let start = bytes.windows(4).position(|w| w == signature).unwrap();
        let flags = start + if signature[2] == 3 { 6 } else { 8 };
        bytes[flags] |= 1;
    }
    fs::write(&archive, bytes).unwrap();
    assert!(run(&archive, &folder.0).err().unwrap().contains("password"));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn links_inside_the_archive_survive_and_those_leaving_it_do_not() {
        let folder = Folder::new("links");
        let dest = folder.0.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(folder.0.join("secret"), b"outside").unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        builder.append_data(&mut header, "top/real.txt", &b"real"[..]).unwrap();
        link_entry(&mut builder, tar::EntryType::Symlink, "top/good", "real.txt");
        link_entry(&mut builder, tar::EntryType::Link, "top/hard", "top/real.txt");
        link_entry(&mut builder, tar::EntryType::Symlink, "top/up", "../../secret");
        link_entry(&mut builder, tar::EntryType::Symlink, "top/absolute", "/etc/passwd");
        // Each step alone looks inside; together they climb out of the stage.
        link_entry(&mut builder, tar::EntryType::Symlink, "top/d/here", "..");
        link_entry(&mut builder, tar::EntryType::Symlink, "top/chain", "d/here/../..");
        let archive = folder.0.join("links.tar");
        fs::write(&archive, builder.into_inner().unwrap()).unwrap();
        let done = run(&archive, &dest).unwrap();
        assert_eq!(done.path, dest.join("top"));
        assert_eq!(fs::read(done.path.join("good")).unwrap(), b"real");
        assert_eq!(fs::read(done.path.join("hard")).unwrap(), b"real");
        assert!(fs::symlink_metadata(done.path.join("up")).is_err());
        assert!(fs::symlink_metadata(done.path.join("absolute")).is_err());
        assert_eq!(done.skipped, 3, "{:?}", names(&done.path));
    }

    #[test]
    fn nothing_is_written_through_a_link_from_the_same_archive() {
        let folder = Folder::new("through");
        let dest = folder.0.join("dest");
        let victim = folder.0.join("victim");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(&victim).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        link_entry(&mut builder, tar::EntryType::Symlink, "way", victim.to_str().unwrap());
        link_entry(&mut builder, tar::EntryType::Symlink, "up", "../../..");
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        builder.append_data(&mut header, "way/planted.txt", &b"evil"[..]).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o644);
        builder.append_data(&mut header, "up/planted.txt", &b"evil"[..]).unwrap();
        let archive = folder.0.join("through.tar");
        fs::write(&archive, builder.into_inner().unwrap()).unwrap();
        let _ = run(&archive, &dest);
        assert!(names(&victim).is_empty());
        assert_eq!(names(&folder.0), ["dest", "through.tar", "victim"]);
    }

    #[test]
    fn permissions_keep_execute_but_lose_setuid() {
        let folder = Folder::new("modes");
        let mut builder = tar::Builder::new(Vec::new());
        for (name, mode) in [("top/run.sh", 0o755), ("top/suid", 0o4755), ("top/none", 0)] {
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(mode);
            header.set_mtime(1_000_000_000);
            builder.append_data(&mut header, name, &b"x"[..]).unwrap();
        }
        let archive = folder.0.join("modes.tar");
        fs::write(&archive, builder.into_inner().unwrap()).unwrap();
        let done = run(&archive, &folder.0).unwrap();
        let mode = |name: &str| fs::metadata(done.path.join(name)).unwrap().permissions().mode() & 0o7777;
        assert_ne!(mode("run.sh") & 0o100, 0);
        assert_eq!(mode("suid") & 0o7000, 0);
        assert_eq!(mode("none") & 0o600, 0o600);
        let modified = fs::metadata(done.path.join("run.sh")).unwrap().modified().unwrap();
        assert_eq!(modified, UNIX_EPOCH + Duration::from_secs(1_000_000_000));
    }

    #[test]
    fn special_files_are_left_out() {
        let folder = Folder::new("special");
        let mut builder = tar::Builder::new(Vec::new());
        link_entry(&mut builder, tar::EntryType::Fifo, "pipe", "");
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        builder.append_data(&mut header, "kept.txt", &b"x"[..]).unwrap();
        let archive = folder.0.join("special.tar");
        fs::write(&archive, builder.into_inner().unwrap()).unwrap();
        let done = run(&archive, &folder.0).unwrap();
        assert_eq!(done.path, folder.0.join("kept.txt"));
        assert_eq!(done.skipped, 1);
    }

    #[test]
    fn a_folder_without_write_permission_reports_an_error() {
        if unsafe { libc::geteuid() } == 0 { return; }
        let folder = Folder::new("readonly");
        let dest = folder.0.join("dest");
        fs::create_dir(&dest).unwrap();
        let archive = folder.0.join("a.tar");
        fs::write(&archive, tar_of(&[("a.txt", b"a")])).unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o500)).unwrap();
        let result = run(&archive, &dest);
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
    }
}
