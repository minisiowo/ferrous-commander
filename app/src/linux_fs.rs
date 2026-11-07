//! Linux safety rules for data leaving one filesystem for another, and for media that
//! may have been prepared by someone else (a pendrive is not trusted input).
use std::{fs, io::{self, Read}, path::{Path, PathBuf}, sync::atomic::{AtomicBool, Ordering}, time::SystemTime};
use std::os::{fd::AsRawFd, unix::fs::{FileTypeExt, MetadataExt, PermissionsExt}};

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
}

fn differs(what: &str, path: &Path) -> io::Error {
    io::Error::other(format!("{what}: {}", path.display()))
}

/// Push everything written to the filesystem holding `dir` onto the device. Until this
/// returns, a "finished" copy may exist only in memory: unplugging the drive loses it.
pub fn sync(dir: &Path) -> io::Result<()> {
    let folder = fs::File::open(dir)?;
    // SAFETY: a plain call on a descriptor that stays open for its duration.
    if unsafe { libc::syncfs(folder.as_raw_fd()) } == 0 { return Ok(()); }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOSYS) { Ok(()) } else { Err(error) }
}

/// Call after removing a folder from `dir`. On exFAT (seen on Linux 7.2.15, reproducible with
/// `mkdir d; rmdir d; cp big .; sync`) a removed folder's unwritten blocks still go to disk
/// after its cluster was reused, and `cp` reports success over a zeroed part of the new
/// file. Writing them out while the cluster is free prevents it. Elsewhere this costs
/// nothing, and failures are not reported here: the sync ending every job reports them.
pub fn settle_freed_folders(dir: &Path) {
    if is_exfat(dir) { let _ = sync(dir); }
}

pub fn is_exfat(dir: &Path) -> bool {
    const EXFAT_SUPER_MAGIC: i64 = 0x2011BAB0;
    let Ok(folder) = fs::File::open(dir) else { return false };
    // SAFETY: zeroed plain data, filled by the call on a descriptor that stays open.
    unsafe {
        let mut info: libc::statfs = std::mem::zeroed();
        libc::fstatfs(folder.as_raw_fd(), &mut info) == 0 && info.f_type as i64 == EXFAT_SUPER_MAGIC
    }
}

/// One source entry whose copy was proven good.
pub struct Verified { path: PathBuf, kept: Kept }
enum Kept { File { len: u64, modified: Option<SystemTime> }, Dir, Other }

/// Prove `dst` is a durable, byte-identical copy of `src` before the source may go.
/// Lists exactly what was checked, so anything that appears later is never removed.
pub fn verify(src: &Path, dst: &Path, cancel: &AtomicBool) -> io::Result<Vec<Verified>> {
    sync(dst.parent().unwrap_or(dst))?;
    let mut proof = Vec::new();
    let mut pending = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((src, dst)) = pending.pop() {
        if cancel.load(Ordering::Relaxed) { return Err(cancelled()); }
        let s = fs::symlink_metadata(&src)?;
        let d = fs::symlink_metadata(&dst).map_err(|_| differs("Copied item is missing", &dst))?;
        let (from, to) = (s.file_type(), d.file_type());
        let kept = if from.is_symlink() {
            if !to.is_symlink() || fs::read_link(&src)? != fs::read_link(&dst)? { return Err(differs("Copied link differs", &dst)); }
            Kept::Other
        } else if from.is_dir() {
            if !to.is_dir() { return Err(differs("Copied folder is missing", &dst)); }
            let mut children = 0;
            for child in fs::read_dir(&src)? {
                let child = child?;
                pending.push((child.path(), dst.join(child.file_name())));
                children += 1;
            }
            // `A` and `a` collapse into one file on FAT/exFAT, and names can be lost to
            // the destination's rules. A file created in the source meanwhile lands here too.
            if fs::read_dir(&dst)?.count() != children { return Err(differs("Copied folder has different items", &dst)); }
            Kept::Dir
        } else if from.is_file() {
            if !to.is_file() || s.len() != d.len() { return Err(differs("Copied file has a different size", &dst)); }
            same_contents(&src, &dst, cancel)?;
            let after = fs::symlink_metadata(&src)?;
            if after.len() != s.len() || after.modified().ok() != s.modified().ok() { return Err(differs("File changed while it was being moved", &src)); }
            Kept::File { len: s.len(), modified: s.modified().ok() }
        } else {
            let same = from.is_fifo() == to.is_fifo() && from.is_socket() == to.is_socket()
                && from.is_block_device() == to.is_block_device() && from.is_char_device() == to.is_char_device();
            if !same || s.rdev() != d.rdev() { return Err(differs("Copied special file differs", &dst)); }
            Kept::Other
        };
        proof.push(Verified { path: src, kept });
    }
    Ok(proof)
}

fn read_full(file: &mut fs::File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Compare contents, asking for the copy from the device and not from memory.
fn same_contents(src: &Path, dst: &Path, cancel: &AtomicBool) -> io::Result<()> {
    const CHUNK: usize = 1 << 20;
    let mut original = fs::File::open(src)?;
    let mut copy = fs::File::open(dst)?;
    // SAFETY: a plain call on an open descriptor. Advisory, so its result does not matter.
    unsafe { libc::posix_fadvise(copy.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED); }
    let (mut expected, mut actual) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(cancelled()); }
        let (a, b) = (read_full(&mut original, &mut expected)?, read_full(&mut copy, &mut actual)?);
        if a != b || expected[..a] != actual[..b] { return Err(differs("Copy does not match the original", dst)); }
        if a == 0 { return Ok(()); }
    }
}

/// Remove only what `verify` proved, and only if it still looks the same. Folders go
/// with `remove_dir`, so one holding anything new stays with its contents.
pub fn remove_verified(proof: &[Verified]) -> io::Result<()> {
    // `verify` lists folders before their contents.
    for item in proof.iter().rev() {
        let md = match fs::symlink_metadata(&item.path) {
            Ok(md) => md,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        match item.kept {
            Kept::File { len, modified } => {
                if md.len() != len || md.modified().ok() != modified {
                    return Err(differs("File changed after it was copied and was kept", &item.path));
                }
                fs::remove_file(&item.path)?;
            }
            Kept::Other => fs::remove_file(&item.path)?,
            Kept::Dir => if let Err(e) = fs::remove_dir(&item.path) {
                if e.kind() == io::ErrorKind::DirectoryNotEmpty { return Err(differs("Folder received new items and was kept", &item.path)); }
                return Err(e);
            },
        }
    }
    Ok(())
}

/// A copy belongs to the user who made it, so a set-UID bit from foreign media would make
/// the file run with that user's rights for anyone. Only regular files lose it: on a
/// folder, set-GID merely picks the group of new items.
pub fn drop_setid(root: &Path) -> io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let md = fs::symlink_metadata(&path)?;
        if md.is_dir() {
            // A folder this user may not list stays as copied.
            let Ok(children) = fs::read_dir(&path) else { continue };
            for child in children { pending.push(child?.path()); }
        } else if md.is_file() && md.mode() & 0o6000 != 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(md.mode() & 0o1777))?;
        }
    }
    Ok(())
}

/// A link inside the per-volume trash of `path`. The `trash` crate follows such links, so a
/// prepared drive could turn "delete this file" into "put it in ~/.config/autostart".
fn redirected_trash(path: &Path, uid: u32) -> Option<PathBuf> {
    let is_link = |p: &Path| fs::symlink_metadata(p).is_ok_and(|md| md.file_type().is_symlink());
    let parent = fs::canonicalize(path.parent()?).ok()?;
    // The crate picks the mount point, which is always one of these folders. Asking
    // about all of them costs a few lookups and needs no parsing of the mount table.
    parent.ancestors().find_map(|top| {
        [top.join(".Trash").join(uid.to_string()), top.join(format!(".Trash-{uid}"))].into_iter()
            .flat_map(|trash| [trash.join("files"), trash.join("info"), trash])
            .find(|candidate| is_link(candidate))
    })
}

/// Why `path` must not go through the trash, if it must not.
pub fn trash_blocker(path: &Path) -> Option<String> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    redirected_trash(path, uid).map(|link| format!("this drive's trash folder is a link to another place ({})", link.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sandbox(PathBuf);
    impl Sandbox {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("ferrous-linux-{name}-{}-{nanos}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Sandbox { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    fn link(target: &Path, at: &Path) { std::os::unix::fs::symlink(target, at).unwrap(); }

    #[test]
    fn every_link_inside_a_volume_trash_is_refused() {
        const UID: u32 = 4242;
        let root = Sandbox::new("trash");
        let (usb, elsewhere) = (root.0.join("usb"), root.0.join("elsewhere"));
        fs::create_dir_all(usb.join("folder")).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let file = usb.join("folder/file.txt");
        fs::write(&file, "x").unwrap();
        assert_eq!(redirected_trash(&file, UID), None, "an ordinary drive");

        let private = usb.join(".Trash-4242");
        fs::create_dir(&private).unwrap();
        fs::create_dir(private.join("files")).unwrap();
        assert_eq!(redirected_trash(&file, UID), None, "a real trash folder");
        fs::remove_dir(private.join("files")).unwrap();
        for part in ["files", "info"] {
            link(&elsewhere, &private.join(part));
            assert_eq!(redirected_trash(&file, UID), Some(private.join(part)));
            fs::remove_file(private.join(part)).unwrap();
        }
        fs::remove_dir(&private).unwrap();
        link(&elsewhere, &private);
        assert_eq!(redirected_trash(&file, UID), Some(private.clone()));
        fs::remove_file(&private).unwrap();

        let shared = usb.join(".Trash");
        fs::create_dir(&shared).unwrap();
        link(&elsewhere, &shared.join("4242"));
        assert_eq!(redirected_trash(&file, UID), Some(shared.join("4242")));
    }

    #[test]
    fn only_regular_files_lose_setid_bits() {
        let root = Sandbox::new("setid");
        let (folder, program) = (root.0.join("folder"), root.0.join("folder/program"));
        fs::create_dir(&folder).unwrap();
        fs::write(&program, "x").unwrap();
        link(Path::new("/nonexistent"), &root.0.join("dangling"));
        fs::set_permissions(&program, fs::Permissions::from_mode(0o6755)).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o2755)).unwrap();
        drop_setid(&root.0).unwrap();
        assert_eq!(fs::metadata(&program).unwrap().mode() & 0o7777, 0o755);
        assert_eq!(fs::metadata(&folder).unwrap().mode() & 0o7777, 0o2755);
    }
}
