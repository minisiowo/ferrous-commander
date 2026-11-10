//! File operations that run off the UI thread: copy, move, delete, search, folder size.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::fsx;
#[cfg(target_os = "linux")]
use crate::linux_fs as proven;
#[cfg(windows)]
use crate::windows_fs as proven;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Copy,
    Move,
    Trash,
    /// Delete for good, after the user confirmed it.
    Destroy,
    Extract,
    Compress,
    #[cfg(target_os = "linux")]
    Symlink,
    #[cfg(target_os = "linux")]
    Hardlink,
}

impl OpKind {
    pub fn verb(self) -> &'static str {
        match self {
            OpKind::Copy => "Copying",
            OpKind::Move => "Moving",
            OpKind::Trash | OpKind::Destroy => "Deleting",
            OpKind::Extract => "Extracting",
            OpKind::Compress => "Compressing",
            #[cfg(target_os = "linux")]
            OpKind::Symlink | OpKind::Hardlink => "Creating links",
        }
    }
}

#[derive(Default)]
pub struct Progress {
    pub done_items: usize,
    pub total_items: usize,
    pub current: String,
    pub finished: bool,
    pub errors: Vec<String>,
    /// Where the new items ended up, so they can be selected.
    pub created: Vec<PathBuf>,
    /// Left untouched because deleting them could not be undone, and why.
    pub unrecoverable: Vec<(PathBuf, String)>,
    /// Archives that want a password, and whether the one given was wrong.
    pub locked: Vec<(PathBuf, bool)>,
}

pub struct Job {
    pub kind: OpKind,
    pub dest: Option<PathBuf>,
    pub progress: Arc<Mutex<Progress>>,
    pub cancel: Arc<AtomicBool>,
}

pub fn start(kind: OpKind, sources: Vec<PathBuf>, dest: Option<PathBuf>, ctx: egui::Context) -> Job {
    start_with_password(kind, sources, dest, None, ctx)
}

pub fn start_with_password(kind: OpKind, sources: Vec<PathBuf>, dest: Option<PathBuf>, password: Option<String>, ctx: egui::Context) -> Job {
    let progress = Arc::new(Mutex::new(Progress { total_items: sources.len(), ..Default::default() }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c, d) = (progress.clone(), cancel.clone(), dest.clone());
    std::thread::spawn(move || {
        let exact_dest = d.as_deref().map(op_path);
        for listed in sources {
            if c.load(Ordering::Relaxed) {
                p.lock().unwrap().errors.push("Cancelled".into());
                break;
            }
            p.lock().unwrap().current = fsx::display_name(&listed);
            let src = op_path(&listed);
            let r = match (kind, &exact_dest) {
                (OpKind::Trash, _) => match recycle_blocker(&src) {
                    Some(reason) => { p.lock().unwrap().unrecoverable.push((listed.clone(), reason)); Ok(None) }
                    None => shell_delete(&src),
                },
                (OpKind::Destroy, _) => destroy(&src),
                (OpKind::Copy, Some(dir)) => copy_item(&src, dir, &c).map(Some),
                (OpKind::Move, Some(dir)) => move_item(&src, dir, &c).map(Some),
                (OpKind::Extract, Some(dir)) => {
                    let mut report = |entry: String| p.lock().unwrap().current = entry;
                    match crate::archive::extract(&src, dir, password.as_deref(), &c, &mut report).map(|done| {
                        if done.skipped > 0 {
                            p.lock().unwrap().errors.push(format!(
                                "{}: {} item(s) left out: links leading outside the archive, special files, or names this system can't use",
                                fsx::display_name(&listed), done.skipped));
                        }
                        Some(done.path)
                    }) {
                        Err(e) if e == crate::archive::PASSWORD_NEEDED || e == crate::archive::PASSWORD_WRONG => {
                            p.lock().unwrap().locked.push((listed.clone(), e == crate::archive::PASSWORD_WRONG));
                            Ok(None)
                        }
                        done => done,
                    }
                }
                #[cfg(target_os = "linux")]
                (OpKind::Symlink | OpKind::Hardlink, Some(dir)) => create_link(&src, dir, kind == OpKind::Hardlink).map(Some),
                _ => Err("No destination".into()),
            };
            let mut g = p.lock().unwrap();
            match r {
                // Report the path as listed: panes select new items by that spelling.
                Ok(Some(path)) => g.created.push(match (&d, path.file_name()) {
                    (Some(dir), Some(name)) => dir.join(name),
                    _ => path,
                }),
                Ok(None) => {}
                Err(e) => g.errors.push(format!("{}: {e}", fsx::display_name(&src))),
            }
            g.done_items += 1;
            drop(g);
            ctx.request_repaint();
        }
        // A copy counts as done once it is on the device: the job stays open until then.
        #[cfg(target_os = "linux")]
        if let (OpKind::Copy | OpKind::Extract, Some(dir)) = (kind, &exact_dest) {
            let wrote = !p.lock().unwrap().created.is_empty();
            if wrote {
                p.lock().unwrap().current = "Writing to the drive".into();
                ctx.request_repaint();
                if let Err(e) = proven::sync(dir) {
                    p.lock().unwrap().errors.push(format!("The copy may not have reached the drive: {e}"));
                }
            }
        }
        p.lock().unwrap().finished = true;
        ctx.request_repaint();
    });
    Job { kind, dest, progress, cancel }
}

/// Pack all of `sources` into one new archive named `name` in `dest`.
pub fn start_compress(sources: Vec<PathBuf>, dest: PathBuf, name: String, kind: crate::compress::Kind, ctx: egui::Context) -> Job {
    let progress = Arc::new(Mutex::new(Progress { total_items: sources.len(), ..Default::default() }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c, d) = (progress.clone(), cancel.clone(), dest.clone());
    std::thread::spawn(move || {
        let exact_dest = op_path(&d);
        let exact: Vec<PathBuf> = sources.iter().map(|source| op_path(source)).collect();
        let mut report = |entry: String| p.lock().unwrap().current = entry;
        let packed = crate::compress::pack(&exact, &exact_dest, &name, kind, &c, &mut report);
        match packed {
            Ok(done) => {
                let wrote = d.join(&done.name);
                let mut g = p.lock().unwrap();
                if done.skipped > 0 {
                    g.errors.push(format!("{}: {} item(s) left out: special files, or names this format can't store", fsx::display_name(&wrote), done.skipped));
                }
                g.created.push(wrote);
                g.done_items = g.total_items;
                drop(g);
                #[cfg(target_os = "linux")]
                {
                    p.lock().unwrap().current = "Writing to the drive".into();
                    ctx.request_repaint();
                    if let Err(e) = proven::sync(&exact_dest) {
                        p.lock().unwrap().errors.push(format!("The archive may not have reached the drive: {e}"));
                    }
                }
            }
            Err(e) => p.lock().unwrap().errors.push(if e == "Cancelled" { e } else { format!("{name}: {e}") }),
        }
        p.lock().unwrap().finished = true;
        ctx.request_repaint();
    });
    Job { kind: OpKind::Compress, dest: Some(dest), progress, cancel }
}

/// The path to operate on, immune to Windows path normalisation.
fn op_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    { crate::windows_fs::exact(path).unwrap_or_else(|_| path.to_path_buf()) }
    #[cfg(not(windows))]
    { path.to_path_buf() }
}

fn shown(path: &Path) -> String {
    #[cfg(windows)]
    { crate::windows_fs::shown(path).display().to_string() }
    #[cfg(not(windows))]
    { path.display().to_string() }
}

fn recycle_blocker(_path: &Path) -> Option<String> {
    #[cfg(windows)]
    { crate::windows_fs::recycle_blocker(_path) }
    #[cfg(target_os = "linux")]
    { crate::linux_fs::trash_blocker(_path) }
    #[cfg(not(any(windows, target_os = "linux")))]
    { None }
}

fn shell_delete(src: &Path) -> Result<Option<PathBuf>, String> {
    // Renaming works on such items, because it does not go through the Shell.
    #[cfg(windows)]
    if !proven::shell_can_address(src) {
        return Err("Windows cannot safely delete an item with this name. Nothing was deleted; rename it first".into());
    }
    trash::delete(src).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    if std::fs::symlink_metadata(src).is_ok() {
        return Err("Windows reported success, but the item is still there".into());
    }
    Ok(None)
}

/// Delete for good. The trash must not be involved, and the Windows Shell would use the
/// Recycle Bin wherever a drive has one. None of these calls follow links: a link is
/// removed, not what it points to.
fn destroy(src: &Path) -> Result<Option<PathBuf>, String> {
    let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    // Windows removes a link to a folder, or a junction, as a folder. `is_dir` is false for it.
    #[cfg(windows)]
    let folder_link = std::os::windows::fs::FileTypeExt::is_symlink_dir(&md.file_type());
    #[cfg(not(windows))]
    let folder_link = false;
    let removed = if md.is_dir() {
        std::fs::remove_dir_all(src)
    } else if folder_link {
        std::fs::remove_dir(src)
    } else {
        std::fs::remove_file(src)
    };
    // Shares and FAT honour the read-only attribute, which the user has just overruled.
    #[cfg(windows)]
    let removed = match removed {
        Err(e) if e.raw_os_error() == Some(5) => proven::remove_staged(src),
        other => other,
    };
    #[cfg(target_os = "linux")]
    if md.is_dir() && let Some(parent) = src.parent() { proven::settle_freed_folders(parent); }
    removed.map_err(|e| e.to_string())?;
    Ok(None)
}

#[cfg(target_os = "linux")]
fn create_link(src: &Path, dest_dir: &Path, hard: bool) -> Result<PathBuf, String> {
    let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if hard && !md.is_file() { return Err("Hard links require a regular file on the same filesystem".into()); }
    let name = src.file_name().ok_or("Cannot link a filesystem root")?;
    let target = fsx::unique_os_name(dest_dir, name, md.is_dir());
    if hard { std::fs::hard_link(src, &target) }
    else { std::os::unix::fs::symlink(std::path::absolute(src).map_err(|e| e.to_string())?, &target) }
        .map_err(|e| e.to_string())?;
    Ok(target)
}

fn check_destination(src: &Path, dest_dir: &Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if md.is_dir() {
        let source = std::fs::canonicalize(src).map_err(|e| e.to_string())?;
        let destination = std::fs::canonicalize(dest_dir).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let inside = crate::windows_fs::within(&destination, &source);
        #[cfg(not(windows))]
        let inside = destination.starts_with(source);
        if inside {
            return Err("Can't copy or move a folder into itself (including through a symbolic link)".into());
        }
    }
    Ok(())
}

/// Never replace an existing entry, including dangling symbolic links.
pub fn rename_no_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let c_src = std::ffi::CString::new(src.as_os_str().as_bytes())?;
        let c_dst = std::ffi::CString::new(dst.as_os_str().as_bytes())?;
        let result = unsafe { libc::renameat2(libc::AT_FDCWD, c_src.as_ptr(), libc::AT_FDCWD, c_dst.as_ptr(), libc::RENAME_NOREPLACE) };
        if result == 0 { return Ok(()); }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            // FAT/exFAT/NTFS report a case-only rename as a collision with itself.
            Some(libc::EEXIST) if same_entry(src, dst) => std::fs::rename(src, dst),
            // NFS and some FUSE filesystems lack RENAME_NOREPLACE. Check first;
            // the remaining race window is the same as other file managers'.
            Some(libc::EINVAL) if std::fs::symlink_metadata(dst).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {
                std::fs::rename(src, dst)
            }
            _ => Err(error),
        }
    }
    #[cfg(windows)]
    { crate::windows_fs::rename_as_asked(src, dst) }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        if std::fs::symlink_metadata(dst).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "Destination already exists"));
        }
        std::fs::rename(src, dst)
    }
}

/// The same directory entry under another letter case (case-insensitive filesystems).
#[cfg(unix)]
pub fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let names = a.file_name().map(|n| n.to_string_lossy().to_lowercase()) == b.file_name().map(|n| n.to_string_lossy().to_lowercase());
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => names && a.parent() == b.parent() && x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Local disks are trusted to keep what they accept. Pendrives and network shares earn no
/// such trust: testing found a real exFAT drive zeroing part of a copy while every call
/// reported success. There, a copy is read back before it gets its name.
fn copy_item(src: &Path, dest_dir: &Path, cancel: &AtomicBool) -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    let trusted = crate::drives::foreign_place(dest_dir).is_none() && !proven::is_exfat(dest_dir);
    #[cfg(windows)]
    let trusted = crate::drives::foreign_place(dest_dir).is_none();
    #[cfg(any(windows, target_os = "linux"))]
    if !trusted {
        return copy_checked(src, dest_dir, cancel, |payload| {
            #[cfg(test)]
            chaos::DAMAGE_COPY.with_borrow_mut(|hook| if let Some(hook) = hook.take() { hook(payload) });
            proven::verify(src, payload, cancel).map(|_| ()).map_err(|e| e.to_string())
        });
    }
    copy_checked(src, dest_dir, cancel, |_| Ok(()))
}

/// Private from the moment it exists, where the filesystem has permissions at all.
/// FAT and exFAT ignore the mode without an error.
fn reserve_stage(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    { use std::os::unix::fs::DirBuilderExt; std::fs::DirBuilder::new().mode(0o700).create(path) }
    #[cfg(not(unix))]
    { std::fs::create_dir(path) }
}

/// `check` sees the finished copy while it is still staged, so a copy that fails it is
/// discarded without ever appearing under the destination name.
fn copy_checked(src: &Path, dest_dir: &Path, cancel: &AtomicBool, check: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    check_destination(src, dest_dir)?;
    let name = src.file_name().ok_or("Cannot copy a filesystem root")?;
    let target = fsx::unique_os_name(dest_dir, name, src.is_dir());
    staged(dest_dir, |stage| {
        let payload = stage.join("item");
        copy_rec(src, &payload, cancel).and_then(|()| check(&payload)).and_then(|()| {
            if cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
            publish(&payload, &target, cancel).map_err(|e| e.to_string())
        })?;
        Ok(target)
    })
}

/// Stage beside the destination so cancellation/failure never exposes a partial result.
pub(crate) fn staged<T>(dest_dir: &Path, work: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    let mut stage = None;
    for n in 0..1000 {
        let candidate = dest_dir.join(format!(".ferrous-copy-{}-{n}", std::process::id()));
        match reserve_stage(&candidate) {
            Ok(()) => { stage = Some(candidate); break; }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let stage = stage.ok_or("Could not reserve a temporary copy directory")?;
    let result = work(&stage);
    // Keep cleanup errors visible: restricted filesystems may refuse removal.
    #[cfg(windows)]
    let cleanup = crate::windows_fs::remove_staged(&stage);
    #[cfg(not(windows))]
    let cleanup = std::fs::remove_dir_all(&stage);
    #[cfg(target_os = "linux")]
    proven::settle_freed_folders(dest_dir);
    match (result, cleanup) {
        (Ok(done), Ok(())) => Ok(done),
        (Err(e), Ok(())) => Err(e),
        // The drive was pulled out: what is left on it cannot be known from here.
        (Err(e), Err(gone)) if gone.kind() == std::io::ErrorKind::NotFound => Err(e),
        (result, Err(e)) => Err(format!("{}; temporary copy remains at {}: {e}", result.err().unwrap_or_else(|| "Copy completed".into()), shown(&stage))),
    }
}

/// Give a finished copy its name. Windows refuses to rename a folder while anything inside
/// is open, and antivirus, the search indexer and SMB all hold a new file for a moment, so
/// a good copy could fail at the very end. Testing against a share hit this within a minute.
pub(crate) fn publish(payload: &Path, target: &Path, _cancel: &AtomicBool) -> std::io::Result<()> {
    #[cfg(windows)]
    for _ in 0..120 {
        match rename_no_replace(payload, target) {
            // Access denied, sharing violation.
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32)) && !_cancel.load(Ordering::Relaxed) =>
                std::thread::sleep(std::time::Duration::from_millis(25)),
            done => return done,
        }
    }
    rename_no_replace(payload, target)
}

#[cfg(target_os = "linux")]
fn copy_rec(src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<(), String> {
    use std::process::{Command, Stdio};
    if cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
    // GNU cp preserves modes, ACLs, xattrs, timestamps, sparse extents and hard links
    // within a copied tree. Ownership is deliberately that of the current user.
    // Archive mode recreates special nodes instead of reading FIFOs/devices.
    // No explicit --preserve list: that makes unsupported xattrs fatal, so files
    // downloaded by browsers (user.xdg.origin.url) could not reach FAT/exFAT drives.
    let mut child = Command::new("cp")
        .args(["--archive", "--no-preserve=ownership", "--reflink=auto", "--no-target-directory", "--"])
        .arg(src).arg(dst).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()
        .map_err(|e| format!("Copy requires GNU coreutils cp: {e}"))?;
    // In memory: a log on the destination stays empty exactly when that drive is full.
    // The rest is drained, or a very talkative cp would block on a full pipe.
    let mut stderr = child.stderr.take().ok_or("Could not read the output of cp")?;
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut details = Vec::new();
        let _ = (&mut stderr).take(8192).read_to_end(&mut details);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        details
    });
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill(); let _ = child.wait();
            break Err("Cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(_)) => {
                let details = reader.join().unwrap_or_default();
                return Err(format!("Copy failed: {}", String::from_utf8_lossy(&details).trim()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(30)),
            Err(e) => { let _ = child.kill(); let _ = child.wait(); break Err(e.to_string()); }
        }
    };
    result.and_then(|()| crate::linux_fs::drop_setid(dst).map_err(|e| e.to_string()))
}

#[cfg(windows)]
fn copy_rec(src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<(), String> {
    crate::windows_fs::copy(src, dst, cancel)
        .map_err(|e| if cancel.load(Ordering::Relaxed) { "Cancelled".into() } else { e.to_string() })
}

#[cfg(not(any(target_os = "linux", windows)))]
fn copy_rec(src: &Path, dst: &Path, cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
    let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if md.file_type().is_symlink() {
        let target = std::fs::read_link(src).map_err(|e| e.to_string())?;
        return std::os::unix::fs::symlink(target, dst).map_err(|e| e.to_string());
    }
    if md.is_dir() {
        std::fs::create_dir(dst).map_err(|e| e.to_string())?;
        for de in std::fs::read_dir(src).map_err(|e| e.to_string())? {
            let de = de.map_err(|e| e.to_string())?;
            copy_rec(&de.path(), &dst.join(de.file_name()), cancel)?;
        }
        std::fs::set_permissions(dst, md.permissions()).map_err(|e| e.to_string())
    } else if md.is_file() {
        let mut input = std::fs::File::open(src).map_err(|e| e.to_string())?;
        let mut output = std::fs::File::create_new(dst).map_err(|e| e.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
        output.set_permissions(md.permissions()).map_err(|e| e.to_string())
    } else { Err("Copying this special file type is not supported on this platform".into()) }
}

fn move_item(src: &Path, dest_dir: &Path, cancel: &AtomicBool) -> Result<PathBuf, String> {
    if cancel.load(Ordering::Relaxed) { return Err("Cancelled".into()); }
    check_destination(src, dest_dir)?;
    if src.parent().and_then(|p| std::fs::canonicalize(p).ok()) == std::fs::canonicalize(dest_dir).ok() {
        return Err("Destination same as source".into());
    }
    let name = src.file_name().ok_or("Cannot move a filesystem root")?;
    let target = fsx::unique_os_name(dest_dir, name, src.is_dir());
    match rename_no_replace(src, &target) {
        Ok(()) => return Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {},
        Err(e) => return Err(e.to_string()),
    }
    move_by_copy(src, dest_dir, cancel)
}

/// The source goes only after its copy is proven: nothing unverified, and nothing
/// that appeared in the meantime, is ever removed.
fn move_by_copy(src: &Path, dest_dir: &Path, cancel: &AtomicBool) -> Result<PathBuf, String> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        let mut proof = Vec::new();
        let target = copy_checked(src, dest_dir, cancel, |payload| {
            proof = proven::verify(src, payload, cancel).map_err(|e| e.to_string())?;
            #[cfg(windows)]
            proven::streams_survive(src, payload).map_err(|e| e.to_string())?;
            Ok(())
        }).map_err(|e| if e == "Cancelled" { e } else { format!("Source kept: {e}") })?;
        #[cfg(windows)]
        proven::flush_entry(&target).and_then(|()| proven::flush_entry(dest_dir))
            .map_err(|e| format!("Source kept: the copy at {} may not have reached the drive: {e}", shown(&target)))?;
        // The verified data is on the device; its final name must be too.
        #[cfg(target_os = "linux")]
        proven::sync(dest_dir).map_err(|e| format!("Source kept: the copy at {} may not have reached the drive: {e}", shown(&target)))?;
        if cancel.load(Ordering::Relaxed) {
            return Err(format!("Cancelled; source retained, completed copy at {}", shown(&target)));
        }
        #[cfg(test)]
        chaos::AFTER_PROOF.with_borrow_mut(|hook| if let Some(hook) = hook.take() { hook() });
        let removed = proven::remove_verified(&proof);
        #[cfg(target_os = "linux")]
        if let Some(parent) = src.parent() { proven::settle_freed_folders(parent); }
        removed.map_err(|e| format!("Verified copy is at {}; the source was not fully removed: {e}", shown(&target)))?;
        Ok(target)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let target = copy_item(src, dest_dir, cancel)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(format!("Cancelled; source retained, completed copy at {}", target.display()));
        }
        let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
        let r = if md.is_dir() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) };
        r.map_err(|e| format!("Copy completed at {}; source could not be removed: {e}", target.display()))?;
        Ok(target)
    }
}

// ---------- search ----------

pub struct Search {
    pub results: Arc<Mutex<Vec<fsx::Entry>>>,
    pub done: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub scanned: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Search names under `root` containing every word of `query` (case-insensitive).
pub fn search(root: PathBuf, query: String, show_hidden: bool, ctx: egui::Context) -> Search {
    search_within(root, query, show_hidden, None, ctx)
}

/// With a limit the search ends at that many results, and takes the folders nearest the
/// root first, so that the few results kept are the ones closest to hand.
pub fn search_within(root: PathBuf, query: String, show_hidden: bool, limit: Option<usize>, ctx: egui::Context) -> Search {
    let s = Search {
        results: Arc::new(Mutex::new(Vec::new())),
        done: Arc::new(AtomicBool::new(false)),
        cancel: Arc::new(AtomicBool::new(false)),
        scanned: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let (res, done, cancel, scanned) = (s.results.clone(), s.done.clone(), s.cancel.clone(), s.scanned.clone());
    std::thread::spawn(move || {
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        let mut stack = std::collections::VecDeque::from([root]);
        let mut last = std::time::Instant::now();
        let mut full = false;
        while let Some(dir) = if limit.is_some() { stack.pop_front() } else { stack.pop_back() } {
            if full || cancel.load(Ordering::Relaxed) {
                break;
            }
            let Ok(list) = fsx::read_dir(&dir) else { continue };
            for e in list {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if e.hidden && !show_hidden {
                    continue;
                }
                scanned.fetch_add(1, Ordering::Relaxed);
                if e.is_dir {
                    let link = std::fs::symlink_metadata(&e.path).map(|m| fsx::is_link_like(&m)).unwrap_or(true);
                    if !link {
                        stack.push_back(e.path.clone());
                    }
                }
                if !words.is_empty() && words.iter().all(|w| e.lname().contains(w.as_str())) {
                    let mut found = res.lock().unwrap();
                    found.push(e);
                    if limit.is_some_and(|limit| found.len() >= limit) {
                        full = true;
                        break;
                    }
                }
            }
            if last.elapsed().as_millis() > 100 {
                last = std::time::Instant::now();
                ctx.request_repaint();
            }
        }
        done.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    });
    s
}

#[cfg(all(test, any(windows, target_os = "linux")))]
#[path = "chaos_tests.rs"]
mod chaos;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_staging_collisions_moves_and_lock_failures() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!("ferrous-staging-{}", std::process::id()));
        // A unique directory is reserved; never delete a preexisting test path.
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = crate::windows_fs::remove_staged(&self.0); } }
        let _cleanup = Cleanup(root.clone());
        let src = root.join("source"); let dst = root.join("destination");
        std::fs::create_dir(&src).unwrap(); std::fs::create_dir(&dst).unwrap();
        let file = src.join("Łódź.txt"); std::fs::write(&file, "original").unwrap();
        std::fs::write(dst.join("Łódź.txt"), "existing").unwrap();
        let cancel = AtomicBool::new(false);
        let copied = copy_item(&file, &dst, &cancel).unwrap();
        assert_ne!(copied, dst.join("Łódź.txt"));
        assert_eq!(std::fs::read(dst.join("Łódź.txt")).unwrap(), b"existing");
        assert_eq!(std::fs::read(&copied).unwrap(), b"original");
        assert!(copy_item(&src, &src, &cancel).is_err());
        assert!(move_item(&file, &src, &cancel).is_err());
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();
        assert!(copy_item(&file, &dst, &cancel).is_err());
        assert!(move_item(&file, &dst, &cancel).is_err());
        assert!(file.exists()); drop(lock);
        assert!(!std::fs::read_dir(&dst).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ferrous-copy-")));
        assert!(copy_item(&file, &dst, &AtomicBool::new(true)).is_err());
        let moved = move_item(&file, &dst, &cancel).unwrap();
        assert!(!file.exists()); assert_eq!(std::fs::read(moved).unwrap(), b"original");
    }

    #[cfg(windows)]
    mod windows_safety {
        use super::*;
        use crate::windows_fs::{exact, remove_staged, remove_verified, verify};

        struct Sandbox(PathBuf);
        impl Sandbox {
            fn new(name: &str) -> Self {
                let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
                let path = std::env::temp_dir().join(format!("ferrous-safety-{name}-{}-{nanos}", std::process::id()));
                let path = crate::windows_fs::shown(&std::fs::canonicalize({ std::fs::create_dir(&path).unwrap(); path }).unwrap());
                Self(path)
            }
            fn dirs(&self) -> (PathBuf, PathBuf) {
                let (src, dst) = (self.0.join("source"), self.0.join("destination"));
                std::fs::create_dir(&src).unwrap();
                std::fs::create_dir(&dst).unwrap();
                (src, dst)
            }
        }
        impl Drop for Sandbox { fn drop(&mut self) { let _ = remove_staged(&self.0); } }

        fn attribute(path: &Path, flag: &str) {
            assert!(std::process::Command::new("attrib").arg(flag).arg(path).status().unwrap().success());
        }
        fn staged(dir: &Path) -> bool {
            std::fs::read_dir(dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ferrous-copy-"))
        }
        fn bytes(n: usize) -> Vec<u8> { (0..n).map(|i| (i * 31 % 251) as u8).collect() }

        #[test]
        fn cross_volume_move_handles_big_read_only_and_nested_items() {
            let root = Sandbox::new("move"); let (src, dst) = root.dirs();
            let tree = src.join("Zażółć"); let nested = tree.join("sub");
            std::fs::create_dir_all(&nested).unwrap();
            let big = bytes(3 << 20 | 123);
            std::fs::write(nested.join("big.bin"), &big).unwrap();
            std::fs::write(tree.join("empty.txt"), "").unwrap();
            std::fs::write(tree.join("locked.txt"), "read only").unwrap();
            attribute(&tree.join("locked.txt"), "+R");
            attribute(&nested, "+R"); // Windows uses this flag on folders with custom icons.
            let moved = move_by_copy(&tree, &dst, &AtomicBool::new(false)).unwrap();
            assert!(!tree.exists());
            assert_eq!(std::fs::read(moved.join("sub/big.bin")).unwrap(), big);
            assert_eq!(std::fs::read(moved.join("empty.txt")).unwrap(), b"");
            assert_eq!(std::fs::read(moved.join("locked.txt")).unwrap(), b"read only");
            assert!(std::fs::metadata(moved.join("locked.txt")).unwrap().permissions().readonly(), "attribute survives the flush");
            assert!(!staged(&dst));
            attribute(&moved.join("locked.txt"), "-R"); attribute(&moved.join("sub"), "-R");
        }

        #[test]
        fn corrupt_copy_is_never_published_and_keeps_the_source() {
            let root = Sandbox::new("corrupt"); let (src, dst) = root.dirs();
            let file = src.join("data.bin"); let original = bytes(70_000);
            std::fs::write(&file, &original).unwrap();
            let cancel = AtomicBool::new(false);
            // Same size, one flipped byte: only a content comparison can see it.
            let result = copy_checked(&file, &dst, &cancel, |payload| {
                let mut damaged = original.clone(); damaged[69_999] ^= 1;
                std::fs::write(payload, &damaged).unwrap();
                verify(&file, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert!(result.unwrap_err().contains("does not match"));
            assert_eq!(std::fs::read(&file).unwrap(), original);
            assert!(!dst.join("data.bin").exists()); assert!(!staged(&dst));
        }

        #[test]
        fn file_added_during_copy_keeps_the_whole_source() {
            let root = Sandbox::new("added"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("old.txt"), "old").unwrap();
            let cancel = AtomicBool::new(false);
            let result = copy_checked(&tree, &dst, &cancel, |payload| {
                std::fs::write(tree.join("new.txt"), "new").unwrap();
                verify(&tree, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(tree.join("new.txt")).unwrap(), b"new");
            assert_eq!(std::fs::read(tree.join("old.txt")).unwrap(), b"old");
            assert!(!dst.join("tree").exists());
        }

        #[test]
        fn removal_spares_anything_new_or_changed_after_verification() {
            let root = Sandbox::new("removal"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("a.txt"), "a").unwrap();
            let cancel = AtomicBool::new(false);
            let copy = copy_item(&tree, &dst, &cancel).unwrap();
            let proof = verify(&tree, &copy, &cancel).unwrap();
            std::fs::write(tree.join("late.txt"), "written after the check").unwrap();
            let error = remove_verified(&proof).unwrap_err();
            assert!(error.to_string().contains("new items"), "{error}");
            assert_eq!(std::fs::read(tree.join("late.txt")).unwrap(), b"written after the check");

            let file = src.join("b.txt"); std::fs::write(&file, "before").unwrap();
            let copy = copy_item(&file, &dst, &cancel).unwrap();
            let proof = verify(&file, &copy, &cancel).unwrap();
            std::fs::write(&file, "edited, longer").unwrap();
            assert!(remove_verified(&proof).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), b"edited, longer");
        }

        #[test]
        fn cancelled_verification_leaves_the_source_and_no_copy() {
            let root = Sandbox::new("cancel"); let (src, dst) = root.dirs();
            let file = src.join("a.txt"); std::fs::write(&file, "a").unwrap();
            let cancel = AtomicBool::new(false);
            let result = copy_checked(&file, &dst, &cancel, |payload| {
                cancel.store(true, Ordering::Relaxed);
                verify(&file, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert_eq!(result.unwrap_err(), "Cancelled");
            assert!(file.exists()); assert!(!dst.join("a.txt").exists()); assert!(!staged(&dst));
        }

        #[test]
        fn names_windows_normalises_reach_the_listed_file_only() {
            let root = Sandbox::new("names"); let (src, dst) = root.dirs();
            let sibling = src.join("report");
            let dotted = PathBuf::from(format!(r"{}\report.", src.display()));
            let device = PathBuf::from(format!(r"{}\nul", src.display()));
            std::fs::write(&sibling, "sibling").unwrap();
            std::fs::write(exact(&dotted).unwrap(), "dotted").unwrap();
            std::fs::write(exact(&device).unwrap(), "not a device").unwrap();
            // Listings hand these names back as plain paths.
            let listed: Vec<_> = std::fs::read_dir(&src).unwrap().map(|e| e.unwrap().path()).collect();
            assert!(listed.contains(&dotted) && listed.contains(&device));
            // The Shell would have deleted `report` here.
            for path in [&dotted, &device] {
                assert!(shell_delete(&exact(path).unwrap()).unwrap_err().contains("Nothing was deleted"));
            }
            let moved = move_item(&exact(&dotted).unwrap(), &exact(&dst).unwrap(), &AtomicBool::new(false)).unwrap();
            assert_eq!(moved.file_name().unwrap(), "report.");
            assert_eq!(std::fs::read(&moved).unwrap(), b"dotted");
            assert_eq!(std::fs::read(&sibling).unwrap(), b"sibling");
            let renamed = src.join("renamed");
            rename_no_replace(&device, &renamed).unwrap();
            assert_eq!(std::fs::read(renamed).unwrap(), b"not a device");
            std::fs::remove_file(moved).unwrap();
        }

        #[test]
        fn folder_with_a_junction_refuses_to_move_across_volumes() {
            use std::os::windows::process::CommandExt;
            let root = Sandbox::new("junction"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(root.0.join("target.txt"), "outside").unwrap();
            std::fs::write(tree.join("keep.txt"), "keep").unwrap();
            let link = tree.join("loop");
            assert!(std::process::Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&link).arg(&root.0).creation_flags(0x08000000).output().unwrap().status.success());
            let error = move_by_copy(&tree, &dst, &AtomicBool::new(false)).unwrap_err();
            assert!(error.contains("Junction"), "{error}");
            assert_eq!(std::fs::read(tree.join("keep.txt")).unwrap(), b"keep");
            assert_eq!(std::fs::read(root.0.join("target.txt")).unwrap(), b"outside");
            assert!(!dst.join("tree").exists()); assert!(!staged(&dst));
            std::fs::remove_dir(link).unwrap();
        }

        fn wait(job: &Job) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !job.progress.lock().unwrap().finished {
                assert!(std::time::Instant::now() < deadline, "job did not finish");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        /// `C:\x` and `\\localhost\C$\x` differ in device, so this is a real cross-device
        /// move and a real location without a Recycle Bin, on any machine with admin shares.
        #[test]
        fn network_location_is_never_deleted_without_consent_and_moves_verified() {
            let root = Sandbox::new("unc"); let (src, dst) = root.dirs();
            let share = PathBuf::from(format!(r"\\localhost\{}", root.0.display().to_string().replacen(':', "$", 1)));
            if !share.exists() { eprintln!("Skipped: administrative share is not available"); return; }
            let ctx = egui::Context::default();

            let doomed = share.join("source").join("Zażółć.txt");
            std::fs::write(&doomed, "precious").unwrap();
            let job = start(OpKind::Trash, vec![doomed.clone()], None, ctx.clone());
            wait(&job);
            assert_eq!(std::fs::read(&doomed).unwrap(), b"precious", "the Shell would destroy this silently");
            let progress = job.progress.lock().unwrap();
            assert!(progress.errors.is_empty(), "{:?}", progress.errors);
            assert_eq!(progress.unrecoverable.len(), 1);
            assert_eq!(progress.unrecoverable[0].0, doomed);
            assert!(progress.unrecoverable[0].1.contains("network"));
            drop(progress);

            let file = src.join("over-the-wire.bin"); let data = bytes(2 << 20);
            std::fs::write(&file, &data).unwrap();
            let remote_dir = share.join("destination");
            let device = rename_no_replace(&file, &remote_dir.join("probe")).unwrap_err();
            assert_eq!(device.kind(), std::io::ErrorKind::CrossesDevices, "this must exercise the copy path");
            let job = start(OpKind::Move, vec![file.clone()], Some(remote_dir.clone()), ctx);
            wait(&job);
            let progress = job.progress.lock().unwrap();
            assert!(progress.errors.is_empty(), "{:?}", progress.errors);
            assert_eq!(progress.created, [remote_dir.join("over-the-wire.bin")], "reported as listed");
            assert!(!file.exists());
            assert_eq!(std::fs::read(dst.join("over-the-wire.bin")).unwrap(), data);
        }

        /// A local drive has a Recycle Bin, and a permanent delete must not use it.
        #[test]
        fn destroy_removes_files_folders_and_links_without_the_shell() {
            let root = Sandbox::new("destroy"); let (src, dst) = root.dirs();
            let file = src.join("read-only.txt"); std::fs::write(&file, "x").unwrap();
            let mut read_only = std::fs::metadata(&file).unwrap().permissions(); read_only.set_readonly(true);
            std::fs::set_permissions(&file, read_only.clone()).unwrap();
            let name = src.join("report."); std::fs::write(exact(&name).unwrap(), "x").unwrap();
            let folder = src.join("folder"); std::fs::create_dir(&folder).unwrap();
            std::fs::write(folder.join("inner.txt"), "x").unwrap();
            std::fs::set_permissions(folder.join("inner.txt"), read_only).unwrap();
            for item in [&file, &name, &folder] {
                destroy(&exact(item).unwrap()).unwrap();
                assert!(std::fs::symlink_metadata(exact(item).unwrap()).is_err(), "{}", item.display());
            }
            std::fs::write(dst.join("kept.txt"), "kept").unwrap();
            let link = src.join("link");
            if std::os::windows::fs::symlink_dir(&dst, &link).is_err() { eprintln!("Skipped links: no privilege"); return; }
            destroy(&exact(&link).unwrap()).unwrap();
            assert!(std::fs::symlink_metadata(&link).is_err());
            assert_eq!(std::fs::read(dst.join("kept.txt")).unwrap(), b"kept", "what the link points to stays");
        }

        #[test]
        fn local_drive_has_a_recycle_bin() {
            let root = Sandbox::new("local");
            assert_eq!(crate::windows_fs::recycle_blocker(&root.0), None);
        }

        #[test]
        fn verbatim_paths_round_trip() {
            for (plain, verbatim) in [(r"C:\a\b.", r"\\?\C:\a\b."), (r"\\server\share\x", r"\\?\UNC\server\share\x")] {
                let converted = exact(Path::new(plain)).unwrap();
                assert_eq!(converted, Path::new(verbatim));
                assert_eq!(crate::windows_fs::shown(&converted), Path::new(plain));
                assert_eq!(exact(&converted).unwrap(), converted);
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod linux_safety {
        use super::*;
        use crate::linux_fs::{remove_verified, verify};

        struct Sandbox(PathBuf);
        impl Sandbox {
            fn new(name: &str) -> Self {
                let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
                let path = std::env::temp_dir().join(format!("ferrous-move-{name}-{}-{nanos}", std::process::id()));
                std::fs::create_dir(&path).unwrap();
                Self(path)
            }
            fn dirs(&self) -> (PathBuf, PathBuf) {
                let (src, dst) = (self.0.join("source"), self.0.join("destination"));
                std::fs::create_dir(&src).unwrap();
                std::fs::create_dir(&dst).unwrap();
                (src, dst)
            }
        }
        impl Drop for Sandbox { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

        fn staged(dir: &Path) -> bool {
            std::fs::read_dir(dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ferrous-copy-"))
        }
        fn bytes(n: usize) -> Vec<u8> { (0..n).map(|i| (i * 31 % 251) as u8).collect() }
        fn wait(job: &Job) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !job.progress.lock().unwrap().finished {
                assert!(std::time::Instant::now() < deadline, "job did not finish");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        #[test]
        fn move_by_copy_takes_big_nested_special_and_linked_items() {
            let root = Sandbox::new("full"); let (src, dst) = root.dirs();
            let tree = src.join("Zażółć"); let nested = tree.join("sub");
            std::fs::create_dir_all(&nested).unwrap();
            let big = bytes(3 << 20 | 123);
            std::fs::write(nested.join("big.bin"), &big).unwrap();
            std::fs::write(tree.join("empty.txt"), "").unwrap();
            std::os::unix::fs::symlink("empty.txt", tree.join("link")).unwrap();
            std::os::unix::fs::symlink("missing", tree.join("dangling")).unwrap();
            std::fs::create_dir(tree.join("empty folder")).unwrap();
            use std::os::unix::ffi::OsStrExt;
            let fifo = std::ffi::CString::new(tree.join("pipe").as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            let moved = move_by_copy(&tree, &dst, &AtomicBool::new(false)).unwrap();
            assert!(std::fs::symlink_metadata(&tree).is_err(), "the source is gone");
            assert_eq!(std::fs::read(moved.join("sub/big.bin")).unwrap(), big);
            assert_eq!(std::fs::read_link(moved.join("dangling")).unwrap(), PathBuf::from("missing"));
            assert!(moved.join("empty folder").is_dir());
            assert!(!staged(&dst));
        }

        #[test]
        fn corrupt_copy_is_never_published_and_keeps_the_source() {
            let root = Sandbox::new("corrupt"); let (src, dst) = root.dirs();
            let file = src.join("data.bin"); let original = bytes(2 << 20);
            std::fs::write(&file, &original).unwrap();
            let cancel = AtomicBool::new(false);
            // Same size, one flipped byte in the second chunk: only comparing contents can see it.
            let result = copy_checked(&file, &dst, &cancel, |payload| {
                let mut damaged = original.clone(); damaged[(2 << 20) - 1] ^= 1;
                std::fs::write(payload, &damaged).unwrap();
                verify(&file, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert!(result.unwrap_err().contains("does not match"));
            assert_eq!(std::fs::read(&file).unwrap(), original);
            assert!(!dst.join("data.bin").exists()); assert!(!staged(&dst));
        }

        #[test]
        fn file_added_during_copy_keeps_the_whole_source() {
            let root = Sandbox::new("added"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("old.txt"), "old").unwrap();
            let cancel = AtomicBool::new(false);
            let result = copy_checked(&tree, &dst, &cancel, |payload| {
                std::fs::write(tree.join("new.txt"), "new").unwrap();
                verify(&tree, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(tree.join("new.txt")).unwrap(), b"new");
            assert_eq!(std::fs::read(tree.join("old.txt")).unwrap(), b"old");
            assert!(!dst.join("tree").exists());
        }

        #[test]
        fn a_name_lost_at_the_destination_keeps_the_source() {
            let root = Sandbox::new("lost"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("Note.txt"), "upper").unwrap();
            std::fs::write(tree.join("note.txt"), "lower").unwrap();
            let cancel = AtomicBool::new(false);
            // What FAT does: the second name lands on the first file.
            let result = copy_checked(&tree, &dst, &cancel, |payload| {
                std::fs::remove_file(payload.join("Note.txt")).unwrap();
                verify(&tree, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(tree.join("Note.txt")).unwrap(), b"upper");
        }

        /// Case-sensitive test filesystems cannot merge two names, and on one that can,
        /// every lookup succeeds: only the number of items tells. Same check, other direction.
        #[test]
        fn a_folder_with_a_different_number_of_items_keeps_the_source() {
            let root = Sandbox::new("count"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("a.txt"), "a").unwrap();
            let cancel = AtomicBool::new(false);
            let result = copy_checked(&tree, &dst, &cancel, |payload| {
                std::fs::write(payload.join("extra.txt"), "x").unwrap();
                verify(&tree, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert!(result.unwrap_err().contains("different items"));
            assert!(tree.join("a.txt").exists());
        }

        #[test]
        fn removal_spares_anything_new_or_changed_after_verification() {
            let root = Sandbox::new("removal"); let (src, dst) = root.dirs();
            let tree = src.join("tree"); std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("a.txt"), "a").unwrap();
            let cancel = AtomicBool::new(false);
            let copy = copy_item(&tree, &dst, &cancel).unwrap();
            let proof = verify(&tree, &copy, &cancel).unwrap();
            std::fs::write(tree.join("late.txt"), "written after the check").unwrap();
            let error = remove_verified(&proof).unwrap_err();
            assert!(error.to_string().contains("new items"), "{error}");
            assert_eq!(std::fs::read(tree.join("late.txt")).unwrap(), b"written after the check");

            let file = src.join("b.txt"); std::fs::write(&file, "before").unwrap();
            let copy = copy_item(&file, &dst, &cancel).unwrap();
            let proof = verify(&file, &copy, &cancel).unwrap();
            std::fs::write(&file, "edited, longer").unwrap();
            assert!(remove_verified(&proof).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), b"edited, longer");
        }

        #[test]
        fn cancelled_verification_leaves_the_source_and_no_copy() {
            let root = Sandbox::new("cancel"); let (src, dst) = root.dirs();
            let file = src.join("a.txt"); std::fs::write(&file, "a").unwrap();
            let cancel = AtomicBool::new(false);
            let result = copy_checked(&file, &dst, &cancel, |payload| {
                cancel.store(true, Ordering::Relaxed);
                verify(&file, payload, &cancel).map(|_| ()).map_err(|e| e.to_string())
            });
            assert_eq!(result.unwrap_err(), "Cancelled");
            assert!(file.exists()); assert!(!dst.join("a.txt").exists()); assert!(!staged(&dst));
        }

        #[test]
        fn copies_lose_setid_bits() {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let root = Sandbox::new("setid"); let (src, dst) = root.dirs();
            let program = src.join("program"); std::fs::write(&program, "x").unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o4755)).unwrap();
            let copy = copy_item(&program, &dst, &AtomicBool::new(false)).unwrap();
            assert_eq!(std::fs::metadata(copy).unwrap().mode() & 0o7777, 0o755);
            assert_eq!(std::fs::metadata(program).unwrap().mode() & 0o7777, 0o4755, "the source stays as it was");
        }

        /// `.Trash-<uid>` here is what a prepared drive carries. Every step runs through
        /// the real job, so this covers what pressing Delete and confirming do.
        #[test]
        fn a_trash_link_cannot_move_a_deleted_file_out_of_its_drive() {
            let root = Sandbox::new("trash"); let (usb, elsewhere) = root.dirs();
            std::os::unix::fs::symlink(&elsewhere, usb.join(format!(".Trash-{}", unsafe { libc::getuid() }))).unwrap();
            let name = format!("ferrous-destroy-test-{}.txt", std::process::id());
            let file = usb.join(&name); std::fs::write(&file, "payload").unwrap();
            let ctx = egui::Context::default();
            let job = start(OpKind::Trash, vec![file.clone()], None, ctx.clone());
            wait(&job);
            {
                let progress = job.progress.lock().unwrap();
                assert!(progress.errors.is_empty(), "{:?}", progress.errors);
                assert_eq!(progress.unrecoverable.len(), 1, "asks first");
                assert!(progress.unrecoverable[0].1.contains("link"));
            }
            assert_eq!(std::fs::read(&file).unwrap(), b"payload", "untouched until confirmed");
            let job = start(OpKind::Destroy, vec![file.clone()], None, ctx.clone());
            wait(&job);
            assert!(job.progress.lock().unwrap().errors.is_empty());
            assert!(!file.exists());
            assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing landed where the link points");
            let trashed = trash::os_limited::list().unwrap();
            assert!(!trashed.iter().any(|item| item.name.to_string_lossy() == name), "deleted for good, not trashed");

            // Deleting a link for good removes the link and leaves what it points to.
            std::fs::write(elsewhere.join("kept.txt"), "kept").unwrap();
            let link = usb.join("folder link"); std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
            let job = start(OpKind::Destroy, vec![link.clone()], None, ctx);
            wait(&job);
            assert!(std::fs::symlink_metadata(&link).is_err());
            assert_eq!(std::fs::read(elsewhere.join("kept.txt")).unwrap(), b"kept");
        }

        #[test]
        fn finished_copy_job_is_on_the_drive_and_reported() {
            let root = Sandbox::new("job"); let (src, dst) = root.dirs();
            let file = src.join("a.txt"); std::fs::write(&file, "a").unwrap();
            let job = start(OpKind::Copy, vec![file], Some(dst.clone()), egui::Context::default());
            wait(&job);
            let progress = job.progress.lock().unwrap();
            assert!(progress.errors.is_empty(), "{:?}", progress.errors);
            assert_eq!(progress.created, [dst.join("a.txt")]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn copies_symlinks_without_following_them_and_keeps_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!("fc_unix_copy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("source");
        let dst = root.join("copy");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("file"), "contents").unwrap();
        symlink("file", src.join("link")).unwrap();
        symlink("missing", src.join("dangling")).unwrap();
        symlink(".", src.join("loop")).unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o750)).unwrap();
        copy_rec(&src, &dst, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read_link(dst.join("link")).unwrap(), PathBuf::from("file"));
        assert_eq!(std::fs::read_link(dst.join("dangling")).unwrap(), PathBuf::from("missing"));
        assert_eq!(std::fs::read_link(dst.join("loop")).unwrap(), PathBuf::from("."));
        assert_eq!(std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777, 0o750);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_metadata_special_files_raw_names_and_collision_safety() {
        use std::os::unix::{ffi::OsStringExt, fs::{MetadataExt, FileTypeExt}};
        let root = std::env::temp_dir().join(format!("fc_linux_metadata_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("source"); let dst = root.join("dest");
        std::fs::create_dir_all(&src).unwrap(); std::fs::create_dir(&dst).unwrap();
        let file = src.join("file"); std::fs::write(&file, "payload").unwrap();
        std::fs::hard_link(&file, src.join("hardlink")).unwrap();
        std::fs::File::options().write(true).open(&file).unwrap().set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234567890))).unwrap();
        let fifo = src.join("pipe");
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let raw = std::ffi::OsString::from_vec(b"raw-\xff".to_vec());
        std::fs::write(src.join(&raw), "raw name").unwrap();
        let c = AtomicBool::new(false);
        let copied = copy_item(&src, &dst, &c).unwrap();
        assert_eq!(std::fs::read(copied.join(&raw)).unwrap(), b"raw name");
        let m = std::fs::metadata(copied.join("file")).unwrap();
        assert_eq!(m.ino(), std::fs::metadata(copied.join("hardlink")).unwrap().ino());
        assert_eq!(m.mtime(), 1234567890);
        assert!(std::fs::symlink_metadata(copied.join("pipe")).unwrap().file_type().is_fifo());
        std::os::unix::fs::symlink("missing", dst.join(&raw)).unwrap();
        let raw_copy = copy_item(&src.join(&raw), &dst, &c).unwrap();
        assert_ne!(raw_copy, dst.join(&raw));
        assert_eq!(std::fs::read_link(dst.join(&raw)).unwrap(), PathBuf::from("missing"));
        assert_eq!(std::fs::read(raw_copy).unwrap(), b"raw name");
        let alias = root.join("alias"); std::os::unix::fs::symlink(&src, &alias).unwrap();
        assert!(copy_item(&src, &alias, &c).is_err());
        assert!(rename_no_replace(&file, &src.join("hardlink")).is_err());
        assert!(file.exists());
        let hard = create_link(&file, &dst, true).unwrap();
        assert_eq!(std::fs::metadata(&hard).unwrap().ino(), std::fs::metadata(&file).unwrap().ino());
        let symbolic = create_link(&file, &dst, false).unwrap();
        assert_eq!(std::fs::read_link(symbolic).unwrap(), file);
        assert!(create_link(&src, &dst, true).is_err());
        std::fs::remove_file(&hard).unwrap();
        let cancelled = AtomicBool::new(true);
        assert!(copy_item(&file, &dst, &cancelled).is_err());
        assert!(!dst.join("file").exists());
        assert!(!std::fs::read_dir(&dst).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ferrous-copy-")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_copy_preserves_acl_and_extended_attributes() {
        use std::process::Command;
        let root = std::env::temp_dir().join(format!("fc_acl_copy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(&root).unwrap();
        let src = root.join("source"); let dst = root.join("dest");
        std::fs::write(&src, "metadata").unwrap();
        // These integration tests intentionally require the Linux inspection utilities.
        let acl_entry = format!("u:{}:r--", unsafe { libc::geteuid() });
        assert!(Command::new("setfacl").args(["-m", &acl_entry, "--"]).arg(&src).status().unwrap().success());
        assert!(Command::new("setfattr").args(["-n", "user.ferrous-test", "-v", "value", "--"]).arg(&src).status().unwrap().success());
        copy_rec(&src, &dst, &AtomicBool::new(false)).unwrap();
        let acl = |path: &Path| Command::new("getfacl").args(["--omit-header", "--numeric", "--"]).arg(path).output().unwrap().stdout;
        assert_eq!(acl(&src), acl(&dst));
        let attr = Command::new("getfattr").args(["--only-values", "-n", "user.ferrous-test", "--"]).arg(&dst).output().unwrap();
        assert!(attr.status.success()); assert_eq!(attr.stdout, b"value");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Real cross-filesystem behaviour (USB-style FAT/exFAT, tmpfs, network mounts).
    /// FERROUS_FS_TARGETS="/run/media/$USER/STICK:/tmp" cargo test real_mounts -- --ignored
    #[test]
    #[ignore = "Writes to the mounted filesystems named in FERROUS_FS_TARGETS"]
    #[cfg(target_os = "linux")]
    fn real_mounts_copy_move_rename_without_losing_data() {
        let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let c = AtomicBool::new(false);
        let tree = |root: &Path| {
            std::fs::create_dir_all(root.join("Zażółć/sub")).unwrap();
            std::fs::write(root.join("Zażółć/sub/a b.txt"), vec![7u8; 300_000]).unwrap();
            std::fs::write(root.join("Zażółć/plain.txt"), "plain").unwrap();
            let pdf = root.join("Zażółć/download.pdf");
            std::fs::write(&pdf, "pdf").unwrap();
            // Browsers tag downloads like this; FAT/exFAT cannot store it.
            assert!(std::process::Command::new("setfattr").args(["-n", "user.xdg.origin.url", "-v", "https://example.com", "--"]).arg(&pdf).status().unwrap().success());
            root.join("Zażółć")
        };
        let same = |a: &Path, b: &Path| {
            for rel in ["sub/a b.txt", "plain.txt", "download.pdf"] {
                assert_eq!(std::fs::read(a.join(rel)).unwrap(), std::fs::read(b.join(rel)).unwrap(), "{rel}");
            }
        };
        for target in targets.split(':') {
            let local = home.join(format!(".fc_mount_test_{}", std::process::id()));
            let remote = Path::new(target).join(format!("fc_mount_test_{}", std::process::id()));
            for d in [&local, &remote] { let _ = std::fs::remove_dir_all(d); std::fs::create_dir_all(d).unwrap(); }
            let (reference, original) = (tree(&local.join("ref")), tree(&local));
            // Copy out, then duplicate inside the target.
            let copied = copy_item(&original, &remote, &c).unwrap();
            same(&reference, &copied);
            let duplicate = copy_item(&copied, &remote, &c).unwrap();
            assert_ne!(duplicate, copied);
            same(&reference, &duplicate);
            // Cross-device move both ways keeps every byte and removes the source.
            let moved = move_item(&duplicate, &local.join("ref"), &c).unwrap();
            assert!(!duplicate.exists());
            same(&reference, &moved);
            let back = move_item(&moved, &remote, &c).unwrap();
            assert!(!moved.exists());
            same(&reference, &back);
            // Renames never replace, including a case-only rename.
            let (a, b) = (copied.join("plain.txt"), copied.join("download.pdf"));
            assert!(rename_no_replace(&a, &b).is_err());
            assert_eq!(std::fs::read(&a).unwrap(), b"plain");
            assert_eq!(std::fs::read(&b).unwrap(), b"pdf");
            let upper = copied.join("PLAIN.txt");
            rename_no_replace(&a, &upper).unwrap();
            assert_eq!(std::fs::read(&upper).unwrap(), b"plain");
            // A colliding copy picks a new name and leaves the existing file alone.
            std::fs::write(remote.join("download.pdf"), "existing").unwrap();
            let collided = copy_item(&original.join("download.pdf"), &remote, &c).unwrap();
            assert_eq!(std::fs::read(remote.join("download.pdf")).unwrap(), b"existing");
            assert_eq!(std::fs::read(collided).unwrap(), b"pdf");
            same(&reference, &original);
            // Failed cross-device moves keep the whole source and leave no partial copy.
            let free = unsafe {
                let mut st: libc::statvfs = std::mem::zeroed();
                let path = std::ffi::CString::new(target).unwrap();
                assert_eq!(libc::statvfs(path.as_ptr(), &mut st), 0);
                st.f_bavail as u64 * st.f_frsize as u64
            };
            let failing = local.join("failing");
            std::fs::create_dir(&failing).unwrap();
            std::fs::write(failing.join("ok.txt"), "kept").unwrap();
            if free < 256 << 20 {
                std::fs::write(failing.join("too big.bin"), vec![1u8; free as usize + (8 << 20)]).unwrap();
            }
            // FAT/exFAT reject these names; elsewhere an unreadable file fails the copy.
            std::fs::write(failing.join("what?.txt"), "name").unwrap();
            let locked = failing.join("locked.txt");
            std::fs::write(&locked, "secret").unwrap();
            { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap(); }
            assert!(move_item(&failing, &remote, &c).is_err());
            assert_eq!(std::fs::read(failing.join("ok.txt")).unwrap(), b"kept");
            assert_eq!(std::fs::read(failing.join("what?.txt")).unwrap(), b"name");
            { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap(); }
            assert_eq!(std::fs::read(&locked).unwrap(), b"secret");
            assert!(!remote.join("failing").exists());
            // Names differing only by case collapse on FAT/exFAT. Nothing may vanish quietly.
            let cases = local.join("cases");
            std::fs::create_dir(&cases).unwrap();
            std::fs::write(cases.join("Note.txt"), "upper").unwrap();
            std::fs::write(cases.join("note.txt"), "lower").unwrap();
            let both = |dir: &Path| std::fs::read(dir.join("Note.txt")).unwrap() == b"upper" && std::fs::read(dir.join("note.txt")).unwrap() == b"lower";
            if let Ok(copy) = copy_item(&cases, &remote, &c) { assert!(both(&copy), "copy reported success but lost a file"); }
            match move_item(&cases, &remote, &c) {
                Ok(moved) => assert!(both(&moved)),
                Err(_) => assert!(both(&cases), "a refused move keeps the source"),
            }
            // Delete only ever trashes: the bytes must be recoverable.
            let doomed = remote.join(format!("trash-me-{}.txt", std::process::id()));
            std::fs::write(&doomed, "recoverable").unwrap();
            trash::delete(&doomed).unwrap();
            assert!(!doomed.exists());
            let name = doomed.file_name().unwrap();
            let trashed = [Path::new(target).join(format!(".Trash-{}", unsafe { libc::getuid() })), home.join(".local/share/Trash")]
                .into_iter().map(|t| t.join("files").join(name)).find(|p| p.exists()).expect("trashed file");
            assert_eq!(std::fs::read(&trashed).unwrap(), b"recoverable");
            std::fs::remove_file(&trashed).unwrap();
            let _ = std::fs::remove_file(trashed.parent().unwrap().parent().unwrap().join("info").join(format!("{}.trashinfo", name.to_string_lossy())));
            assert!(!std::fs::read_dir(&remote).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ferrous-copy-")));
            std::fs::remove_dir_all(&remote).unwrap();
            std::fs::remove_dir_all(&local).unwrap();
        }
    }

    /// What a target cannot hold must leave the source whole and no leftovers behind.
    /// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_mount_limits -- --ignored --nocapture
    #[test]
    #[ignore = "Briefly fills the mounted filesystems named in FERROUS_FS_TARGETS"]
    #[cfg(target_os = "linux")]
    fn real_mount_limits_keep_the_source() {
        let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let c = AtomicBool::new(false);
        let free = |path: &str| unsafe {
            let mut st: libc::statvfs = std::mem::zeroed();
            let path = std::ffi::CString::new(path).unwrap();
            assert_eq!(libc::statvfs(path.as_ptr(), &mut st), 0);
            st.f_bavail as u64 * st.f_frsize as u64
        };
        for target in targets.split(':') {
            let local = home.join(format!(".fc_limits_test_{}", std::process::id()));
            let remote = Path::new(target).join(format!("fc_limits_test_{}", std::process::id()));
            for d in [&local, &remote] { let _ = std::fs::remove_dir_all(d); std::fs::create_dir_all(d).unwrap(); }
            let clean = |dir: &Path| !std::fs::read_dir(dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".ferrous-copy-"));

            // Links and names some filesystems refuse: every outcome keeps all the data.
            let tree = local.join("tree");
            std::fs::create_dir(&tree).unwrap();
            std::fs::write(tree.join("file.txt"), "file").unwrap();
            std::os::unix::fs::symlink("file.txt", tree.join("link")).unwrap();
            match move_item(&tree, &remote, &c) {
                Ok(moved) => assert_eq!(std::fs::read(moved.join("link")).unwrap(), b"file"),
                Err(e) => {
                    eprintln!("{target}: link refused: {e}");
                    assert_eq!(std::fs::read_link(tree.join("link")).unwrap(), PathBuf::from("file.txt"));
                    assert_eq!(std::fs::read(tree.join("file.txt")).unwrap(), b"file");
                }
            }
            assert!(clean(&remote));

            // A full drive. The filler reserves space without writing it, where that works.
            let big = local.join("big.bin");
            let data: Vec<u8> = (0..64usize << 20).map(|i| (i * 31 % 251) as u8).collect();
            std::fs::write(&big, &data).unwrap();
            let before = free(target);
            // FAT32 files stop at 4 GiB, so that takes several, kept open: closing can return
            // preallocated space. exFAT cannot preallocate but extends without writing.
            let mut fillers = Vec::new();
            let mut wanted = before - (16 << 20);
            while wanted > 0 {
                use std::os::fd::AsRawFd;
                let piece = wanted.min(0xFFFF_0000);
                let file = std::fs::File::create_new(remote.join(format!("filler{}", fillers.len()))).unwrap();
                let reserved = unsafe { libc::fallocate(file.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 0, piece as i64) } == 0;
                if !reserved { file.set_len(piece).unwrap(); }
                fillers.push(file);
                wanted -= piece;
            }
            let left = free(target);
            if left < 32 << 20 {
                let copied = copy_item(&big, &remote, &c);
                let moved = move_item(&big, &remote, &c);
                eprintln!("{target}: {} MB free: copy {copied:?}, move {moved:?}", left >> 20);
                assert!(copied.is_err() && moved.is_err());
                assert!(copied.unwrap_err().contains("No space"), "the reason must reach the user");
                assert_eq!(std::fs::read(&big).unwrap(), data, "the source survives a full drive");
                assert!(clean(&remote) && !remote.join("big.bin").exists());
            } else {
                eprintln!("{target}: full-drive check skipped: the filler took no space");
            }
            for k in 0..fillers.len() { std::fs::remove_file(remote.join(format!("filler{k}"))).unwrap(); }
            drop(fillers);

            // FAT32 refuses files of 4 GiB and more only after writing that much.
            let probe = remote.join("probe");
            let too_big = std::fs::File::create_new(&probe).unwrap().set_len(4 << 30).is_err();
            std::fs::remove_file(&probe).unwrap();
            if too_big {
                let huge = local.join("huge.bin");
                std::fs::File::create_new(&huge).unwrap().set_len((4 << 30) + 1).unwrap();
                let result = move_item(&huge, &remote, &c);
                eprintln!("{target}: 4 GiB file: {result:?}");
                assert!(result.is_err() && huge.exists() && clean(&remote) && !remote.join("huge.bin").exists());
                assert_eq!(std::fs::metadata(&huge).unwrap().len(), (4 << 30) + 1);
            }

            // A plain copy after a failed one is not verified by the app, so check it here:
            // the failure above freed a folder, and exFAT can zero this file over it.
            let copied = copy_item(&big, &remote, &c).unwrap();
            crate::linux_fs::verify(&big, &copied, &c).map_err(|e| e.to_string()).unwrap();
            std::fs::remove_file(&copied).unwrap();
            // The moved bytes are what the drive returns, and timing shows the wait is real.
            let started = std::time::Instant::now();
            let moved = move_item(&big, &remote, &c).unwrap();
            eprintln!("{target}: 64 MB move took {:?}", started.elapsed());
            assert!(!big.exists());
            assert_eq!(std::fs::read(&moved).unwrap(), data);
            std::fs::remove_dir_all(&remote).unwrap();
            std::fs::remove_dir_all(&local).unwrap();
        }
    }

    /// A drive pulled out mid-move: the source must survive. Pull it when asked.
    /// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_mount_pulled -- --ignored --nocapture
    #[test]
    #[ignore = "Needs a person to pull the drive out during the run"]
    #[cfg(target_os = "linux")]
    fn real_mount_pulled_out_during_a_move_keeps_the_source() {
        use std::io::Write;
        let target = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let local = home.join(format!(".fc_pull_test_{}", std::process::id()));
        std::fs::create_dir_all(&local).unwrap();
        let big = local.join("big.bin");
        // Hash-free check: every chunk carries its own index.
        let chunk = |k: u64| -> Vec<u8> { (0..1u64 << 20).map(|i| ((i ^ k).wrapping_mul(2654435761) >> 13) as u8).collect() };
        let mut out = std::fs::File::create_new(&big).unwrap();
        // Under FAT32's 4 GiB limit: a larger file fails with the drive still in, and passes.
        const CHUNKS: u64 = 3 << 10;
        for k in 0..CHUNKS { out.write_all(&chunk(k)).unwrap(); }
        out.sync_all().unwrap(); drop(out);
        eprintln!("MOVE STARTED: pull the drive out now");
        let result = move_item(&big, Path::new(&target), &AtomicBool::new(false));
        eprintln!("MOVE ENDED: {result:?}");
        assert!(result.as_ref().is_err_and(|e| e.starts_with("Source kept: ")), "the drive stayed in: nothing was tested");
        use std::io::Read;
        let mut input = std::fs::File::open(&big).expect("the source must still exist");
        let mut buffer = vec![0u8; 1 << 20];
        for k in 0..CHUNKS {
            input.read_exact(&mut buffer).unwrap();
            assert!(buffer == chunk(k), "chunk {k} changed");
        }
        eprintln!("SOURCE INTACT: all {CHUNKS} MiB match");
        std::fs::remove_dir_all(&local).unwrap();
    }

    /// A move between drives that fails must say the source is safe: a bare I/O error does not.
    #[test]
    #[cfg(target_os = "linux")]
    fn failed_move_between_drives_says_the_source_is_kept() {
        use std::os::unix::fs::PermissionsExt;
        let shm = Path::new("/dev/shm");
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        if !shm.is_dir() || unsafe { libc::geteuid() } == 0 { return; }
        let id = std::process::id();
        let (from, to) = (shm.join(format!("fc_kept_{id}")), home.join(format!(".fc_kept_{id}")));
        std::fs::create_dir_all(from.join("folder")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let unreadable = from.join("folder").join("secret.txt");
        std::fs::write(&unreadable, "stays").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = move_item(&from.join("folder"), &to, &AtomicBool::new(false));
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600)).unwrap();
        let kept = std::fs::read(&unreadable);
        let _ = std::fs::remove_dir_all(&from);
        let _ = std::fs::remove_dir_all(&to);
        assert!(result.as_ref().is_err_and(|e| e.starts_with("Source kept: ")), "{result:?}");
        assert_eq!(kept.unwrap(), b"stays");
    }

    /// A drive prepared to redirect its trash, on a real mount where the `trash` crate uses
    /// the per-volume folder. FAT has no links, so it is skipped there.
    /// FERROUS_FS_TARGETS="/run/media/$USER/STICK" cargo test real_mount_with -- --ignored
    #[test]
    #[ignore = "Writes to the mounted filesystems named in FERROUS_FS_TARGETS"]
    #[cfg(target_os = "linux")]
    fn real_mount_with_a_redirected_trash_keeps_the_file_in_place() {
        let targets = std::env::var("FERROUS_FS_TARGETS").expect("FERROUS_FS_TARGETS");
        for target in targets.split(':') {
            let trash = Path::new(target).join(format!(".Trash-{}", unsafe { libc::getuid() }));
            let root = std::env::temp_dir().join(format!("fc_redirect_{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            if std::fs::symlink_metadata(&trash).is_ok() || std::os::unix::fs::symlink(&root, &trash).is_err() {
                eprintln!("Skipped {target}: it has a trash folder or no symbolic links");
                continue;
            }
            let file = Path::new(target).join(format!("redirect-me-{}.txt", std::process::id()));
            std::fs::write(&file, "stays").unwrap();
            let job = start(OpKind::Trash, vec![file.clone()], None, egui::Context::default());
            while !job.progress.lock().unwrap().finished { std::thread::sleep(std::time::Duration::from_millis(10)); }
            let refused = job.progress.lock().unwrap().unrecoverable.len();
            let (kept, landed) = (std::fs::read(&file), std::fs::read_dir(&root).unwrap().count());
            let _ = std::fs::remove_file(&trash);
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_dir_all(&root);
            assert_eq!((refused, kept.unwrap(), landed), (1, b"stays".to_vec(), 0));
        }
    }

    #[test]
    fn dropping_search_cancels_its_worker() {
        let s = Search {
            results: Arc::new(Mutex::new(Vec::new())),
            done: Arc::new(AtomicBool::new(false)),
            cancel: Arc::new(AtomicBool::new(false)),
            scanned: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let cancel = s.cancel.clone();
        drop(s);
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn a_limited_search_takes_the_nearest_folders_first_and_stops() {
        let root = std::env::temp_dir().join(format!("fc_search_limit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a").join("b").join("c")).unwrap();
        for (folder, n) in [("a/b/c", 0), ("a/b", 1), ("a", 2), ("", 3)] {
            std::fs::write(root.join(folder).join(format!("note{n}.txt")), b"x").unwrap();
        }
        let wait = |s: &Search| {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !s.done.load(Ordering::Relaxed) {
                assert!(std::time::Instant::now() < until, "the search never ended");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            s.results.lock().unwrap().iter().map(|e| e.name().to_string()).collect::<Vec<_>>()
        };
        let limited = search_within(root.clone(), "note".into(), false, Some(2), egui::Context::default());
        assert_eq!(wait(&limited), ["note3.txt", "note2.txt"]);
        let all = search(root.clone(), "note".into(), false, egui::Context::default());
        assert_eq!(wait(&all).len(), 4);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_and_move() {
        let base = std::env::temp_dir().join(format!("fc_ops_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("sub").join("f.txt"), b"hello").unwrap();
        let c = AtomicBool::new(false);
        let t = copy_item(&a.join("sub"), &b, &c).unwrap();
        assert_eq!(std::fs::read(t.join("f.txt")).unwrap(), b"hello");
        // a second copy gets a new name instead of overwriting
        let t2 = copy_item(&a.join("sub"), &b, &c).unwrap();
        assert_eq!(fsx::display_name(&t2), "sub - Copy");
        let m = move_item(&a.join("sub").join("f.txt"), &b, &c).unwrap();
        assert!(m.exists() && !a.join("sub").join("f.txt").exists());
        assert!(copy_item(&a, &a.join("sub"), &c).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
