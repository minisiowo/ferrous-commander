//! Windows calls.

#![cfg(windows)]

use std::cmp::Ordering;


#[link(name = "shlwapi")]
unsafe extern "system" {
    fn StrCmpLogicalW(a: *const u16, b: *const u16) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryDosDeviceW(name: *const u16, target: *mut u16, max: u32) -> u32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn SetDefaultDllDirectories(flags: u32) -> i32;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn MonitorFromPoint(point: Point, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> i32;
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(reserved: *mut core::ffi::c_void, flags: u32) -> i32;
    fn CoUninitialize();
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

/// MONITORINFO.
#[repr(C)]
#[derive(Default)]
struct MonitorInfo {
    size: u32,
    monitor: [i32; 4],
    work: [i32; 4],
    flags: u32,
}

/// The usable part of the monitor at this physical point, as left, top, right, bottom.
pub fn work_area_at(x: f32, y: f32) -> Option<[f32; 4]> {
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    let mut info = MonitorInfo { size: size_of::<MonitorInfo>() as u32, ..Default::default() };
    // SAFETY: `info` is valid for writing and announces its own size.
    let ok = unsafe { GetMonitorInfoW(MonitorFromPoint(Point { x: x as i32, y: y as i32 }, MONITOR_DEFAULTTONEAREST), &mut info) };
    (ok != 0).then(|| info.work.map(|v| v as f32))
}

/// SHELLEXECUTEINFOW.
#[repr(C)]
struct ShellExecuteInfo {
    size: u32,
    mask: u32,
    window: isize,
    verb: *const u16,
    file: *const u16,
    parameters: *const u16,
    directory: *const u16,
    show: i32,
    instance: isize,
    id_list: *mut core::ffi::c_void,
    class: *const u16,
    class_key: isize,
    hot_key: u32,
    icon: isize,
    process: isize,
}

/// Later library loads search System32 only, so a DLL dropped beside this program,
/// in Downloads or on a pendrive, never runs inside it.
pub fn load_libraries_from_system_only() {
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
    // SAFETY: takes a flag and nothing else. On failure the old search order stays.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32); }
}

/// How long the system gets to report a problem before the file counts as opening.
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Open a file the way Explorer does. No helper program is involved, so nothing can be
/// planted next to this one in its place. A slow share or a slow program must not freeze
/// the window, so past `OPEN_WAIT` the call goes on alone.
pub fn open(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    const SEE_MASK_NOASYNC: u32 = 0x100;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const SW_SHOWNORMAL: i32 = 1;
    let shown = crate::windows_fs::shown(path);
    let file: Vec<u16> = shown.as_os_str().encode_wide().chain(Some(0)).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The Shell answers a missing file with a dialog of its own, and no error to show.
        if let Err(e) = std::fs::metadata(&shown) { let _ = tx.send(Err(e)); return; }
        // SAFETY: `info` declares its own size, `file` outlives the call and ends in NUL,
        // and each successful CoInitializeEx on this thread is paired with CoUninitialize.
        let result = unsafe {
            let com = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let mut info: ShellExecuteInfo = std::mem::zeroed();
            info.size = size_of::<ShellExecuteInfo>() as u32;
            info.mask = SEE_MASK_NOASYNC;
            info.file = file.as_ptr();
            info.show = SW_SHOWNORMAL;
            let done = ShellExecuteExW(&mut info) != 0;
            let result = if done { Ok(()) } else { Err(std::io::Error::last_os_error()) };
            if com >= 0 { CoUninitialize(); }
            result
        };
        let _ = tx.send(result);
    });
    rx.recv_timeout(OPEN_WAIT).unwrap_or(Ok(()))
}

/// A Windows tool by its full path. A bare name is searched for next to this program
/// first, so a `reg.exe` dropped beside it in Downloads would run instead.
pub fn system_program(name: &str) -> std::path::PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = [0u16; 260];
    // SAFETY: the size passed is the size of the live buffer.
    let len = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if len == 0 || len >= buffer.len() { return name.into(); }
    std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])).join(name)
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: NUL-terminated string returned by the system.
    unsafe {
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }
}

/// Explorer's "logical" name order (Shlwapi StrCmpLogicalW). Both strings must be NUL-terminated UTF-16.
pub fn logical_cmp(a: &[u16], b: &[u16]) -> Ordering {
    // SAFETY: both slices end with 0.
    let r = unsafe { StrCmpLogicalW(a.as_ptr(), b.as_ptr()) };
    r.cmp(&0)
}

/// Folder behind a SUBST drive ("Mount folder as new drive letter"), via QueryDosDevice
/// (original: cNetworkHelper 0x0600075f). `None` for ordinary drives.
pub fn subst_target(letter: char) -> Option<String> {
    let name = wide(&format!("{letter}:"));
    let mut buf = vec![0u16; 1024];
    // SAFETY: buffer we own, length passed.
    let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    let s = from_wide_ptr(buf.as_ptr());
    s.strip_prefix("\\??\\").map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_runs_a_file_and_reports_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("fc_open_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(open(&dir.join("missing.txt")).is_err());
        let (script, marker) = (dir.join("spaced name ż.cmd"), dir.join("ran"));
        std::fs::write(&script, format!("@echo done> \"{}\"\r\n", marker.display())).unwrap();
        open(&script).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(marker.exists(), "the script never ran");
        let _ = std::fs::remove_dir_all(&dir);
    }
}


