//! Native Windows copying: keep streams and metadata; never traverse name-surrogate
//! reparse points (symbolic links, junctions, mount points).
use std::{ffi::{c_void, OsString}, fs, io::{self, Read, Seek}, path::{Component, Path, PathBuf}, sync::atomic::{AtomicBool, Ordering}, time::SystemTime};
use std::os::windows::{ffi::{OsStrExt, OsStringExt}, fs::{MetadataExt, OpenOptionsExt, FileTypeExt, FileTimesExt}};

type Progress = unsafe extern "system" fn(i64, i64, i64, i64, u32, u32, *mut c_void, *mut c_void, *mut c_void) -> u32;
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CopyFileExW(src: *const u16, dst: *const u16, progress: Option<Progress>, data: *mut c_void, cancel: *mut i32, flags: u32) -> i32;
    fn MoveFileExW(src: *const u16, dst: *const u16, flags: u32) -> i32;
    fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32;
    fn CompareStringOrdinal(a: *const u16, a_len: i32, b: *const u16, b_len: i32, ignore_case: i32) -> i32;
    fn FindFirstFileW(name: *const u16, data: *mut FindData) -> *mut c_void;
    fn FindClose(find: *mut c_void) -> i32;
    fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
    fn GetVolumeNameForVolumeMountPointW(mount: *const u16, volume: *mut u16, len: u32) -> i32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn FindFirstStreamW(name: *const u16, level: i32, data: *mut StreamData, flags: u32) -> *mut c_void;
    fn FindNextStreamW(find: *mut c_void, data: *mut StreamData) -> i32;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHQueryRecycleBinW(root: *const u16, info: *mut RecycleInfo) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(key: isize, subkey: *const u16, value: *const u16, flags: u32, kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
}

#[repr(C)]
struct FindData { attributes: u32, times: [u32; 6], size: [u32; 2], reserved: [u32; 2], name: [u16; 260], short: [u16; 14] }
#[repr(C)]
struct RecycleInfo { size: u32, bytes: i64, items: i64 }
#[repr(C)]
struct StreamData { size: i64, name: [u16; 296] }

const REPARSE_SYMLINK: u32 = 0xA000000C;

/// The verbatim (`\\?\`) form of an absolute path, which Win32 passes to the filesystem
/// unchanged. Plain paths are normalised first: `report.` silently means `report` and
/// `nul` means a device, so an operation could hit a different file than the listed
/// one. Linux, NAS shares and archives do produce such names.
pub fn exact(path: &Path) -> io::Result<PathBuf> {
    const VERBATIM: [u16; 4] = [92, 92, 63, 92];
    const DEVICE: [u16; 4] = [92, 92, 46, 92];
    let clean = path.is_absolute() && path.components().all(|c| !matches!(c, Component::CurDir | Component::ParentDir));
    let absolute = if clean { path.to_path_buf() } else { std::path::absolute(path)? };
    let raw: Vec<u16> = absolute.as_os_str().encode_wide().map(|c| if c == 47 { 92 } else { c }).collect();
    if raw.contains(&0) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in path")); }
    let out: Vec<u16> = if raw.starts_with(&VERBATIM) || raw.starts_with(&DEVICE) { raw }
        else if raw.starts_with(&[92, 92]) { "\\\\?\\UNC\\".encode_utf16().chain(raw.into_iter().skip(2)).collect() }
        else { "\\\\?\\".encode_utf16().chain(raw).collect() };
    Ok(PathBuf::from(OsString::from_wide(&out)))
}

/// `path` as a person would type it, for messages and Shell calls.
pub fn shown(path: &Path) -> PathBuf {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
    let out: Vec<u16> = if raw.starts_with(&unc) { [92, 92].into_iter().chain(raw[unc.len()..].iter().copied()).collect() }
        else if raw.starts_with(&[92, 92, 63, 92]) { raw[4..].to_vec() }
        else { raw };
    PathBuf::from(OsString::from_wide(&out))
}

/// The Shell parses plain paths, so it would resolve `report.` to a neighbouring `report`
/// and `nul` to a device. Nothing handed to it may contain such a name.
pub fn shell_can_address(path: &Path) -> bool {
    path.components().all(|c| match c {
        Component::Normal(name) => name.to_str().is_some_and(crate::fsx::valid_name),
        _ => true,
    })
}

/// Extended absolute UTF-16 paths, including UNC. No lossy UTF-8 conversion.
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut out: Vec<u16> = exact(path)?.as_os_str().encode_wide().collect();
    out.push(0);
    Ok(out)
}

/// The reparse tag of the entry itself, or 0 for an ordinary entry.
fn reparse_tag(path: &Path) -> io::Result<u32> {
    let path = wide_path(path)?;
    // SAFETY: zeroed plain data, filled by the call; the handle is closed at once.
    unsafe {
        let mut data: FindData = std::mem::zeroed();
        let find = FindFirstFileW(path.as_ptr(), &mut data);
        if find as isize == -1 { return Err(io::Error::last_os_error()); }
        FindClose(find);
        Ok(if data.attributes & 0x400 != 0 { data.reserved[0] } else { 0 })
    }
}

pub fn within(destination: &Path, source: &Path) -> bool {
    let a: Vec<_> = destination.components().collect();
    let b: Vec<_> = source.components().collect();
    a.len() >= b.len() && a.iter().zip(&b).all(|(a, b)| {
        let a: Vec<u16> = a.as_os_str().encode_wide().collect();
        let b: Vec<u16> = b.as_os_str().encode_wide().collect();
        // SAFETY: explicit lengths match the live UTF-16 buffers.
        unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) == 2 }
    })
}

/// No REPLACE_EXISTING and no COPY_ALLOWED: atomic collision protection, including
/// case-only renames, and cross-volume errors handled by the staged copy path.
pub fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    let (src, dst) = (wide_path(src)?, wide_path(dst)?);
    // SAFETY: NUL-terminated buffers remain alive for the synchronous call.
    if unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), 0) } == 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}

unsafe extern "system" fn progress(_: i64, _: i64, _: i64, _: i64, _: u32, _: u32, _: *mut c_void, _: *mut c_void, data: *mut c_void) -> u32 {
    // SAFETY: CopyFileEx is synchronous; data points to its caller's AtomicBool.
    if unsafe { &*data.cast::<AtomicBool>() }.load(Ordering::Relaxed) { 1 } else { 0 }
}

fn attributes(path: &Path, attrs: u32) -> io::Result<()> {
    let path = wide_path(path)?;
    // SAFETY: a live NUL-terminated buffer; attributes are validated by Windows.
    if unsafe { SetFileAttributesW(path.as_ptr(), attrs) } == 0 { return Err(io::Error::last_os_error()); }
    Ok(())
}

fn copy_times(dst: &Path, md: &fs::Metadata, directory: bool) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).access_mode(0x100); // FILE_WRITE_ATTRIBUTES works on read-only files too.
    if directory { options.custom_flags(0x02000000); } // FILE_FLAG_BACKUP_SEMANTICS
    let file = options.open(dst)?;
    let mut times = fs::FileTimes::new();
    if let Ok(t) = md.created() { times = times.set_created(t); }
    if let Ok(t) = md.modified() { times = times.set_modified(t); }
    if let Ok(t) = md.accessed() { times = times.set_accessed(t); }
    file.set_times(times)
}

pub fn copy(src: &Path, dst: &Path, cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")); }
    let md = fs::symlink_metadata(src)?;
    let ft = md.file_type();
    if ft.is_symlink() {
        // Junctions and mount points count as links too, but recreating one as a
        // symbolic link would change what it is. Refuse instead of following a cycle.
        if reparse_tag(src)? != REPARSE_SYMLINK {
            return Err(io::Error::new(io::ErrorKind::Unsupported, format!("Junction cannot be copied safely: {}. Use Windows Explorer for this item", shown(src).display())));
        }
        let target = fs::read_link(src)?;
        return if ft.is_symlink_dir() { std::os::windows::fs::symlink_dir(target, dst) }
            else { std::os::windows::fs::symlink_file(target, dst) }.map_err(|e| naming(e, src));
    }
    // Any other reparse point (OneDrive placeholders, deduplication) is ordinary
    // content to read: Windows delivers the real data.
    if md.is_dir() {
        fs::create_dir(dst)?;
        for child in fs::read_dir(src)? {
            let child = child?;
            copy(&child.path(), &dst.join(child.file_name()), cancel)?;
        }
        copy_times(dst, &md, true)?;
        // Settable ordinary attributes; compression/encryption need dedicated APIs.
        let flags = md.file_attributes() & (0x1 | 0x2 | 0x4 | 0x20 | 0x100 | 0x2000);
        attributes(dst, if flags == 0 { 0x80 } else { flags })
    } else if md.is_file() {
        let (src_wide, dst_wide) = (wide_path(src)?, wide_path(dst)?);
        // FAIL_IF_EXISTS | COPY_SYMLINK. CopyFileEx retains alternate streams,
        // timestamps, extended attributes and encryption without silent decryption.
        let ok = unsafe { CopyFileExW(src_wide.as_ptr(), dst_wide.as_ptr(), Some(progress), (cancel as *const AtomicBool).cast_mut().cast(), std::ptr::null_mut(), 0x1 | 0x800) };
        if ok == 0 { Err(naming(io::Error::last_os_error(), src)) } else { copy_times(dst, &md, false).map_err(|e| naming(e, src)) }
    } else { Err(io::Error::new(io::ErrorKind::Unsupported, "Unsupported file type")) }
}

fn changed(what: &str, path: &Path) -> io::Error {
    io::Error::other(format!("{what}: {}", shown(path).display()))
}

/// One item failing stops a whole folder, so the message has to say which item it was.
fn naming(error: io::Error, path: &Path) -> io::Error {
    io::Error::new(error.kind(), format!("{error}: {}", shown(path).display()))
}

/// Named data streams and their sizes. Empty where the filesystem has no streams. A file's
/// download mark does not count: it never survives outside NTFS, and nothing is lost with it.
fn named_streams(path: &Path) -> io::Result<Vec<(Vec<u16>, i64)>> {
    let wide = wide_path(path)?;
    let plain: [Vec<u16>; 3] = ["::$DATA", ":Zone.Identifier:$DATA", ":SmartScreen:$DATA"].map(|n| n.encode_utf16().collect());
    let mut found = Vec::new();
    // SAFETY: zeroed plain data filled by the calls; the handle is closed before returning.
    unsafe {
        let mut data: StreamData = std::mem::zeroed();
        let find = FindFirstStreamW(wide.as_ptr(), 0, &mut data, 0);
        if find as isize == -1 {
            let error = io::Error::last_os_error();
            // No streams at all, or a filesystem that does not know the idea.
            return if matches!(error.raw_os_error(), Some(1 | 38 | 50 | 87)) { Ok(found) } else { Err(error) };
        }
        loop {
            let end = data.name.iter().position(|c| *c == 0).unwrap_or(data.name.len());
            if !plain.iter().any(|name| name.as_slice() == &data.name[..end]) { found.push((data.name[..end].to_vec(), data.size)); }
            if FindNextStreamW(find, &mut data) == 0 { break; }
        }
        let error = io::Error::last_os_error();
        FindClose(find);
        if error.raw_os_error() != Some(38) { return Err(error); }
    }
    Ok(found)
}

/// A move removes the original, so anything the destination could not store would be gone
/// for good. CopyFileEx reports success while dropping named streams on FAT and exFAT.
pub fn streams_survive(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() { return Ok(()); }
    let there = named_streams(dst)?;
    if named_streams(src)?.iter().any(|stream| !there.contains(stream)) {
        return Err(changed("This drive cannot store the extra data streams attached to the item. Copy it instead if they do not matter", src));
    }
    if md.is_dir() {
        for child in fs::read_dir(src)? {
            let child = child?;
            streams_survive(&child.path(), &dst.join(child.file_name()))?;
        }
    }
    Ok(())
}

/// One source entry whose copy was proven good, in removal order (children first).
pub struct Verified { path: PathBuf, kept: Kept }
enum Kept { File { len: u64, modified: Option<SystemTime> }, Dir, Link }

/// Prove `dst` is a durable, byte-identical copy of `src` before the source may go.
/// Lists exactly what was checked, so anything that appears later is never removed.
pub fn verify(src: &Path, dst: &Path, cancel: &AtomicBool) -> io::Result<Vec<Verified>> {
    let mut proof = Vec::new();
    verify_into(src, dst, cancel, &mut proof)?;
    Ok(proof)
}

fn verify_into(src: &Path, dst: &Path, cancel: &AtomicBool, proof: &mut Vec<Verified>) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")); }
    let (s, d) = (fs::symlink_metadata(src)?, fs::symlink_metadata(dst)?);
    let kept = if s.file_type().is_symlink() {
        if !d.file_type().is_symlink() || fs::read_link(src)? != fs::read_link(dst)? { return Err(changed("Copied link differs", dst)); }
        Kept::Link
    } else if s.is_dir() {
        if !d.is_dir() { return Err(changed("Copied folder is missing", dst)); }
        for child in fs::read_dir(src)? {
            let child = child?;
            // A file created in the source mid-copy has no twin here and fails the check.
            verify_into(&child.path(), &dst.join(child.file_name()), cancel, proof)?;
        }
        flush_entry(dst)?;
        Kept::Dir
    } else {
        if !d.is_file() || s.len() != d.len() { return Err(changed("Copied file has a different size", dst)); }
        flush(dst, &d)?;
        same_contents(src, dst, cancel)?;
        let after = fs::symlink_metadata(src)?;
        if after.len() != s.len() || after.modified().ok() != s.modified().ok() { return Err(changed("File changed while it was being moved", src)); }
        Kept::File { len: s.len(), modified: s.modified().ok() }
    };
    proof.push(Verified { path: src.to_path_buf(), kept });
    Ok(())
}

/// Force the copy out of Windows' write cache, so unplugging the drive cannot lose it.
fn flush(path: &Path, md: &fs::Metadata) -> io::Result<()> {
    let flags = md.file_attributes();
    let readonly = flags & 0x1 != 0;
    // Flushing needs write access, which the read-only attribute forbids.
    if readonly { clear_readonly(path, md)?; }
    // FILE_FLAG_BACKUP_SEMANTICS: folders can be opened only with it.
    let result = fs::OpenOptions::new().write(true).custom_flags(0x02000000).open(path).and_then(|file| file.sync_all());
    if readonly { attributes(path, flags)?; }
    match result {
        // ERROR_INVALID_FUNCTION: this filesystem has no flush to offer.
        Err(e) if e.raw_os_error() == Some(1) => Ok(()),
        other => other,
    }
}

/// Force an entry's name out to the device. The source is removed next, so a failure here
/// must stop the move. A folder we may add to but not open for writing cannot be flushed,
/// such as the root of a system drive: that is no sign of a failing device.
pub fn flush_entry(path: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(path)?;
    if md.file_type().is_symlink() { return Ok(()); }
    match flush(path, &md) {
        Err(e) if md.is_dir() && e.raw_os_error() == Some(5) => Ok(()),
        other => other,
    }
}

/// Compare contents, reading the copy from the device and not from Windows' cache.
fn same_contents(src: &Path, dst: &Path, cancel: &AtomicBool) -> io::Result<()> {
    const CHUNK: usize = 1 << 20;
    const SECTOR: usize = 4096;
    const NO_BUFFERING: u32 = 0x20000000;
    const SEQUENTIAL_SCAN: u32 = 0x08000000;
    let mut original = fs::File::open(src)?;
    let unbuffered = fs::OpenOptions::new().read(true).custom_flags(NO_BUFFERING | SEQUENTIAL_SCAN).open(dst);
    let mut direct = unbuffered.is_ok();
    let mut copy = match unbuffered { Ok(file) => file, Err(_) => fs::File::open(dst)? };
    // Unbuffered reads need a sector-aligned buffer.
    let mut block = vec![0u8; CHUNK + SECTOR];
    let start = block.as_ptr().align_offset(SECTOR);
    let block = &mut block[start..start + CHUNK];
    let mut expected = vec![0u8; CHUNK];
    let mut offset = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")); }
        let n = match copy.read(block) {
            Ok(n) => n,
            // ERROR_INVALID_PARAMETER: sectors larger than our alignment.
            Err(e) if direct && e.raw_os_error() == Some(87) => {
                copy = fs::File::open(dst)?;
                copy.seek(io::SeekFrom::Start(offset))?;
                direct = false;
                continue;
            }
            Err(e) => return Err(e),
        };
        if n == 0 {
            return if original.read(&mut expected[..1])? == 0 { Ok(()) } else { Err(changed("File changed while it was being moved", src)) };
        }
        original.read_exact(&mut expected[..n]).map_err(|_| changed("File changed while it was being moved", src))?;
        if block[..n] != expected[..n] { return Err(changed("Copy does not match the original", dst)); }
        offset += n as u64;
    }
}

fn clear_readonly(path: &Path, md: &fs::Metadata) -> io::Result<()> {
    let flags = md.file_attributes() & !0x1;
    attributes(path, if flags == 0 { 0x80 } else { flags })
}

/// Remove only what `verify` proved, and only if it still looks the same. Folders go
/// with `remove_dir`, so one holding anything new stays with its contents.
pub fn remove_verified(proof: &[Verified]) -> io::Result<()> {
    for item in proof {
        let md = match fs::symlink_metadata(&item.path) {
            Ok(md) => md,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let path = &item.path;
        match item.kept {
            Kept::File { len, modified } => {
                if md.len() != len || md.modified().ok() != modified { return Err(changed("File changed after it was copied and was kept", path)); }
                if let Err(e) = fs::remove_file(path) {
                    // Filesystems without POSIX delete honour the read-only attribute.
                    if !md.permissions().readonly() { return Err(e); }
                    clear_readonly(path, &md)?;
                    fs::remove_file(path).inspect_err(|_| { let _ = attributes(path, md.file_attributes()); })?;
                }
            }
            Kept::Link => if md.file_type().is_symlink_dir() { fs::remove_dir(path)? } else { fs::remove_file(path)? },
            Kept::Dir => if let Err(e) = fs::remove_dir(path) {
                if e.kind() == io::ErrorKind::DirectoryNotEmpty { return Err(changed("Folder received new items and was kept", path)); }
                if !md.permissions().readonly() { return Err(e); }
                clear_readonly(path, &md)?;
                fs::remove_dir(path).inspect_err(|_| { let _ = attributes(path, md.file_attributes()); })?;
            },
        }
    }
    Ok(())
}

fn registry_dword(root: isize, key: &str, name: &str) -> Option<u32> {
    let (key, name) = (crate::win::wide(key), crate::win::wide(name));
    let (mut value, mut size) = (0u32, 4u32);
    // SAFETY: NUL-terminated strings; `value` holds the 4 bytes RRF_RT_REG_DWORD returns.
    let status = unsafe { RegGetValueW(root, key.as_ptr(), name.as_ptr(), 0x10, std::ptr::null_mut(), (&mut value as *mut u32).cast(), &mut size) };
    (status == 0).then_some(value)
}

/// Whether the total size passes `limit`, stopping as soon as it does. Links count as nothing.
fn exceeds(path: &Path, limit: u64, total: &mut u64) -> bool {
    let Ok(md) = fs::symlink_metadata(path) else { return false };
    if md.file_type().is_symlink() { return false; }
    if md.is_dir() {
        let Ok(list) = fs::read_dir(path) else { return false };
        return list.map_while(Result::ok).any(|child| exceeds(&child.path(), limit, total));
    }
    *total = total.saturating_add(md.len());
    *total > limit
}

/// Why the Shell would destroy `path` instead of recycling it, if it would. It does that
/// silently here: the `trash` crate suppresses the warning and still reports success.
pub fn recycle_blocker(path: &Path) -> Option<String> {
    const HKCU: isize = 0x80000001u32 as i32 as isize;
    const HKLM: isize = 0x80000002u32 as i32 as isize;
    const POLICY: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    // The Shell is handed the real folder (the `trash` crate resolves links first), so that
    // is the place to judge: a link on C: can lead to a share that has no Recycle Bin.
    let Some(Ok(parent)) = path.parent().map(fs::canonicalize) else { return Some("its drive could not be identified".into()) };
    let name: Vec<u16> = shown(&parent).as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; name.len().max(261) + 1];
    // SAFETY: both buffers are live, NUL-terminated and sized for the call.
    if unsafe { GetVolumePathNameW(name.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Some("its drive could not be identified".into());
    }
    // SAFETY: GetVolumePathNameW left a NUL-terminated root.
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        3 => {}
        2 => return Some("removable drives have no Recycle Bin".into()),
        4 => return Some("network locations have no Recycle Bin".into()),
        _ => return Some("this drive has no Recycle Bin".into()),
    }
    let mut info = RecycleInfo { size: size_of::<RecycleInfo>() as u32, bytes: 0, items: 0 };
    // SAFETY: `root` is NUL-terminated and `info` declares its own size.
    if unsafe { SHQueryRecycleBinW(root.as_ptr(), &mut info) } < 0 {
        return Some("this drive has no Recycle Bin".into());
    }
    if [HKCU, HKLM].iter().any(|key| registry_dword(*key, POLICY, "NoRecycleFiles") == Some(1)) {
        return Some("the Recycle Bin is turned off by policy".into());
    }
    let mut volume = [0u16; 64];
    // SAFETY: the length matches the buffer. Subst drives fail here and keep the defaults.
    if unsafe { GetVolumeNameForVolumeMountPointW(root.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0 { return None; }
    let end = volume.iter().position(|c| *c == 0).unwrap_or(volume.len());
    let volume = String::from_utf16_lossy(&volume[..end]);
    let guid = volume.trim_start_matches(r"\\?\Volume").trim_end_matches('\\');
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    if registry_dword(HKCU, &key, "NukeOnDelete") == Some(1) {
        return Some("the Recycle Bin is turned off for this drive".into());
    }
    let megabytes = u64::from(registry_dword(HKCU, &key, "MaxCapacity")?);
    exceeds(path, megabytes << 20, &mut 0).then(|| format!("it is larger than this drive's Recycle Bin ({megabytes} MB)"))
}

/// For our reserved staging directory and for what the user confirmed deleting for good:
/// clears the read-only attribute and never descends through a link or junction.
pub fn remove_staged(path: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(path)?;
    if md.is_dir() && !crate::fsx::is_link_like(&md) {
        for child in fs::read_dir(path)? { remove_staged(&child?.path())?; }
    }
    if md.permissions().readonly() && !crate::fsx::is_link_like(&md) {
        let flags = md.file_attributes() & !0x1;
        attributes(path, if flags == 0 { 0x80 } else { flags })?;
    }
    if md.is_dir() || md.file_type().is_symlink_dir() { fs::remove_dir(path) } else { fs::remove_file(path) }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Sandbox(std::path::PathBuf);
    impl Sandbox {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("ferrous-windows-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            fs::create_dir(&path).unwrap(); Self(path)
        }
    }
    impl Drop for Sandbox { fn drop(&mut self) { let _ = remove_staged(&self.0); } }

    #[test]
    fn copy_retains_unicode_streams_times_and_attributes() {
        let root = Sandbox::new("metadata");
        let src = root.0.join("Zażółć 日本語.txt"); let dst = root.0.join("copy.txt");
        fs::write(&src, b"payload").unwrap();
        fs::write(src.with_file_name("Zażółć 日本語.txt:ferrous"), b"alternate stream").unwrap();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234567890);
        fs::File::options().write(true).open(&src).unwrap().set_times(fs::FileTimes::new().set_created(time).set_modified(time)).unwrap();
        attributes(&src, 0x1 | 0x2 | 0x20).unwrap();
        copy(&src, &dst, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"payload");
        assert_eq!(fs::read(dst.with_file_name("copy.txt:ferrous")).unwrap(), b"alternate stream");
        let md = fs::metadata(&dst).unwrap();
        assert_eq!(md.modified().unwrap(), time); assert_eq!(md.created().unwrap(), time);
        assert_eq!(md.file_attributes() & 0x3, 0x3);
        assert!(copy(&src, &dst, &AtomicBool::new(false)).is_err());
        assert!(fs::metadata(&src).unwrap().permissions().readonly());
    }

    #[test]
    fn rename_is_atomic_and_allows_case_changes_without_overwrite() {
        let root = Sandbox::new("rename");
        let a = root.0.join("report.txt"); let b = root.0.join("other.txt");
        fs::write(&a, "a").unwrap(); fs::write(&b, "b").unwrap();
        assert!(rename(&a, &b).is_err());
        assert_eq!(fs::read(&a).unwrap(), b"a"); assert_eq!(fs::read(&b).unwrap(), b"b");
        let upper = root.0.join("REPORT.txt"); rename(&a, &upper).unwrap();
        assert!(fs::read_dir(&root.0).unwrap().any(|e| e.unwrap().file_name() == "REPORT.txt"));
    }

    #[test]
    fn long_paths_and_directory_metadata_survive_copy() {
        let root = Sandbox::new("long");
        let src = root.0.join("source");
        let nested = src.join("a".repeat(100)).join("b".repeat(100)).join("c".repeat(100));
        fs::create_dir_all(&nested).unwrap(); fs::write(nested.join("Łódź.txt"), "long").unwrap();
        attributes(&src, 0x2).unwrap();
        let dst = root.0.join("copy"); copy(&src, &dst, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(dst.join(nested.strip_prefix(&src).unwrap()).join("Łódź.txt")).unwrap(), b"long");
        assert_ne!(fs::metadata(&dst).unwrap().file_attributes() & 0x2, 0);
        let cancelled = root.0.join("cancelled");
        assert!(copy(&src, &cancelled, &AtomicBool::new(true)).is_err()); assert!(!cancelled.exists());
        assert!(within(&src.join("child"), &src));
        assert!(!within(&root.0.join("source-other"), &src));
        let unc = wide_path(Path::new(r"\\server\share\Łódź")).unwrap();
        assert_eq!(String::from_utf16_lossy(&unc[..unc.len()-1]), r"\\?\UNC\server\share\Łódź");
    }

    #[test]
    fn junction_is_rejected_without_touching_target() {
        use std::os::windows::process::CommandExt;
        let root = Sandbox::new("junction");
        let src = root.0.join("source"); fs::create_dir(&src).unwrap();
        let link = src.join("loop");
        let result = std::process::Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&link).arg(&src).creation_flags(0x08000000).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        fs::write(src.join("keep.txt"), "keep").unwrap();
        assert!(crate::fsx::is_link_like(&fs::symlink_metadata(&link).unwrap()));
        let search = crate::ops::search(src.clone(), "keep".into(), true, egui::Context::default());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !search.done.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(search.done.load(Ordering::Relaxed), "search must not recurse through the junction");
        assert_eq!(search.results.lock().unwrap().len(), 1);
        assert!(copy(&src, &root.0.join("copy"), &AtomicBool::new(false)).is_err());
        remove_staged(&root.0.join("copy")).unwrap();
        assert_eq!(fs::read(src.join("keep.txt")).unwrap(), b"keep");
        fs::remove_dir(link).unwrap();
    }
}
