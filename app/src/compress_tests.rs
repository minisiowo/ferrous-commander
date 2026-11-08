use super::*;
use crate::archive::extract;

struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fc_pack_{name}_{}_{n}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn run(sources: &[PathBuf], dest: &Path, name: &str, kind: Kind) -> Result<Packed, String> {
    pack(sources, dest, &file_name(name, kind), kind, &AtomicBool::new(false), &mut |_| {})
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Every format, through the app's own extractor: what goes in must come back out.
#[test]
fn every_format_round_trips_through_extract() {
    for kind in Kind::ALL {
        let root = Folder::new("round");
        let (src, out) = (root.0.join("src"), root.0.join("out"));
        fs::create_dir_all(src.join("folder/sub")).unwrap();
        fs::create_dir(src.join("folder/empty")).unwrap();
        fs::create_dir(&out).unwrap();
        let big: Vec<u8> = (0..300_000u32).flat_map(|n| n.to_le_bytes()).collect();
        fs::write(src.join("folder/sub/big.bin"), &big).unwrap();
        fs::write(src.join("folder/Zażółć 日本語.txt"), "hello").unwrap();
        fs::write(src.join("folder/nothing.txt"), "").unwrap();
        fs::write(src.join("top.txt"), "top").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            fs::write(src.join("folder/run.sh"), "#!/bin/sh").unwrap();
            fs::set_permissions(src.join("folder/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
            symlink("nothing.txt", src.join("folder/link")).unwrap();
        }

        let sources = [src.join("folder"), src.join("top.txt")];
        let packed = run(&sources, &src, "bundle", kind).unwrap();
        assert_eq!(packed.skipped, 0, "{kind:?}");
        let archive = src.join(&packed.name);
        assert_eq!(archive.file_name().unwrap().to_string_lossy(), format!("bundle{}", kind.suffix()));

        let done = extract(&archive, &out, None, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(done.skipped, 0, "{kind:?}");
        assert_eq!(done.path, out.join("bundle"), "{kind:?}");
        let back = done.path;
        assert_eq!(fs::read(back.join("folder/sub/big.bin")).unwrap(), big, "{kind:?}");
        assert_eq!(fs::read(back.join("folder/Zażółć 日本語.txt")).unwrap(), b"hello", "{kind:?}");
        assert_eq!(fs::read(back.join("folder/nothing.txt")).unwrap(), b"", "{kind:?}");
        assert_eq!(fs::read(back.join("top.txt")).unwrap(), b"top", "{kind:?}");
        assert!(back.join("folder/empty").is_dir(), "{kind:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(back.join("folder/run.sh")).unwrap().permissions().mode() & 0o111, 0o111, "{kind:?}");
            assert_eq!(fs::read_link(back.join("folder/link")).unwrap(), Path::new("nothing.txt"), "{kind:?}");
        }
    }
}

#[test]
fn an_empty_file_gets_no_data_block_in_7z() {
    let root = Folder::new("empty7z");
    let dir = root.0.join("dir");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("empty.txt"), "").unwrap();
    let packed = run(std::slice::from_ref(&dir), &root.0, "out", Kind::SevenZ).unwrap();
    let path = root.0.join(packed.name);
    let reader = sevenz_rust2::ArchiveReader::new(File::open(path).unwrap(), sevenz_rust2::Password::empty()).unwrap();
    let entry = |name: &str| reader.archive().files.iter().find(|f| f.name() == name).unwrap().clone();
    assert!(entry("dir/a.txt").has_stream);
    assert!(!entry("dir/empty.txt").has_stream && !entry("dir/empty.txt").is_directory());
}

#[test]
fn an_existing_archive_is_never_replaced() {
    let root = Folder::new("taken");
    let file = root.0.join("a.txt");
    fs::write(&file, "new").unwrap();
    for kind in [Kind::Zip, Kind::TarGz] {
        let taken = root.0.join(file_name("a", kind));
        fs::write(&taken, "precious").unwrap();
        let packed = run(std::slice::from_ref(&file), &root.0, "a", kind).unwrap();
        assert_eq!(packed.name.to_string_lossy(), format!("a (2){}", kind.suffix()));
        assert_eq!(fs::read(&taken).unwrap(), b"precious");
    }
}

#[test]
fn cancelling_or_failing_leaves_nothing_behind() {
    let root = Folder::new("cancel");
    let file = root.0.join("a.txt");
    fs::write(&file, "x").unwrap();
    let cancelled = pack(std::slice::from_ref(&file), &root.0, "a.zip", Kind::Zip, &AtomicBool::new(true), &mut |_| {});
    assert_eq!(cancelled.err().as_deref(), Some("Cancelled"));
    let missing = run(&[file.clone(), root.0.join("gone.txt")], &root.0, "b", Kind::Zip);
    assert!(missing.err().unwrap().contains("gone.txt"), "names what is missing");
    assert_eq!(names(&root.0), ["a.txt"]);
}

#[test]
fn unsafe_selections_are_refused() {
    let root = Folder::new("refuse");
    fs::create_dir_all(root.0.join("one/same")).unwrap();
    fs::create_dir_all(root.0.join("two/same")).unwrap();
    let doubled = [root.0.join("one/same"), root.0.join("two/same")];
    assert!(run(&doubled, &root.0, "x", Kind::Zip).err().unwrap().contains("both named"));
    let inside = [root.0.join("one")];
    assert!(run(&inside, &root.0.join("one/same"), "x", Kind::Zip).err().unwrap().contains("its own archive"));
    assert_eq!(names(&root.0), ["one", "two"]);
    assert!(fs::read_dir(root.0.join("one/same")).unwrap().next().is_none());
}

#[test]
fn names_and_selections_are_tidied() {
    assert_eq!(file_name("photos", Kind::TarGz), "photos.tar.gz");
    assert_eq!(file_name(" photos.TAR.GZ ", Kind::TarGz), "photos.TAR.GZ");
    assert_eq!(file_name(".zip", Kind::Zip), ".zip.zip");
    let folder = Path::new("/data/work");
    assert_eq!(suggested_name(&[PathBuf::from("/data/work/notes.txt")], folder), "notes");
    assert_eq!(suggested_name(&[PathBuf::from("/a"), PathBuf::from("/b")], folder), "work");
    let picked = outermost(vec![PathBuf::from("/a/b/c"), PathBuf::from("/a/b"), PathBuf::from("/a/bb"), PathBuf::from("/a/b")]);
    assert_eq!(picked, [PathBuf::from("/a/b"), PathBuf::from("/a/bb")]);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    /// A pipe would block forever if it were read, so getting here at all is half the test.
    #[test]
    fn special_files_are_left_out_and_counted() {
        let root = Folder::new("fifo");
        let dir = root.0.join("dir");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        let pipe = std::ffi::CString::new(dir.join("pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
        for kind in Kind::ALL {
            let packed = run(std::slice::from_ref(&dir), &root.0, "out", kind).unwrap();
            assert_eq!(packed.skipped, 1, "{kind:?}");
        }
    }

    /// What a link points to stays out of the archive, wherever it is.
    #[test]
    fn links_are_never_followed() {
        let root = Folder::new("links");
        let (dir, outside) = (root.0.join("dir"), root.0.join("outside"));
        fs::create_dir(&dir).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("away")).unwrap();
        let packed = run(std::slice::from_ref(&dir), &root.0, "out", Kind::Tar).unwrap();
        let mut archive = tar::Archive::new(File::open(root.0.join(packed.name)).unwrap());
        let listed: Vec<_> = archive.entries().unwrap().map(|e| {
            let e = e.unwrap();
            (e.path().unwrap().display().to_string().trim_end_matches('/').to_string(), e.header().entry_type())
        }).collect();
        assert_eq!(listed, [("dir".to_string(), tar::EntryType::Directory), ("dir/away".to_string(), tar::EntryType::Symlink)]);
    }

    #[test]
    fn names_a_format_cannot_store_are_left_out_only_there() {
        let root = Folder::new("bytes");
        let dir = root.0.join("dir");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("ok.txt"), "ok").unwrap();
        fs::write(dir.join(std::ffi::OsStr::from_bytes(b"bad-\xff.txt")), "bad").unwrap();
        assert_eq!(run(std::slice::from_ref(&dir), &root.0, "z", Kind::Zip).unwrap().skipped, 1);
        assert_eq!(run(std::slice::from_ref(&dir), &root.0, "s", Kind::SevenZ).unwrap().skipped, 1);
        assert_eq!(run(std::slice::from_ref(&dir), &root.0, "t", Kind::Tar).unwrap().skipped, 0);
    }
}
