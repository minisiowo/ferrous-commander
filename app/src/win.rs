//! Windows calls.

#![cfg(windows)]



#[cfg(test)]
#[link(name = "shlwapi")]
unsafe extern "system" {
    fn StrCmpLogicalW(a: *const u16, b: *const u16) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LCMapStringEx(locale: *const u16, flags: u32, src: *const u16, src_len: i32, dst: *mut u8, dst_len: i32, version: *const u8, reserved: *const u8, param: isize) -> i32;
    fn QueryDosDeviceW(name: *const u16, target: *mut u16, max: u32) -> u32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn SetDefaultDllDirectories(flags: u32) -> i32;
    fn GetCurrentThread() -> isize;
    fn SetThreadPriority(thread: isize, priority: i32) -> i32;
}

/// Lowers the calling thread's CPU and disk priority, for long work nobody waits for.
pub fn background_thread() {
    const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;
    // SAFETY: the pseudo handle of the current thread is always valid.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN); }
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
#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(window: isize, attribute: u32, value: *const core::ffi::c_void, size: u32) -> i32;
    fn DwmExtendFrameIntoClientArea(window: isize, margins: *const [i32; 4]) -> i32;
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

/// How the main window's frame is dressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WindowLook {
    pub dark: bool,
    /// The title bar and its text, as red, green, blue.
    pub bar: [u8; 3],
    pub text: [u8; 3],
    /// Mica behind the whole window, title bar included, in place of the colours above.
    pub mica: bool,
}

/// Dresses the window; true when Mica is now behind it. Mica needs Windows 11 22H2,
/// and the title bar colours need Windows 11: older systems keep their own title bar.
pub fn dress_window(window: isize, look: WindowLook) -> bool {
    const USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const CAPTION_COLOR: u32 = 35;
    const TEXT_COLOR: u32 = 36;
    const SYSTEMBACKDROP_TYPE: u32 = 38;
    const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
    const BACKDROP_AUTO: u32 = 0;
    const BACKDROP_MICA: u32 = 2;
    // SAFETY: every value is a live u32 of the size passed, and a window that is gone is an error code.
    let set = |attribute: u32, value: u32| unsafe {
        DwmSetWindowAttribute(window, attribute, (&raw const value).cast(), size_of::<u32>() as u32) >= 0
    };
    // SAFETY: the margins are four live integers.
    let extend = |margin: i32| unsafe { DwmExtendFrameIntoClientArea(window, &[margin; 4]) >= 0 };
    let color = |[r, g, b]: [u8; 3]| u32::from_le_bytes([r, g, b, 0]);
    set(USE_IMMERSIVE_DARK_MODE, look.dark as u32);
    // The material only shows where the frame reaches, so the frame takes the whole window.
    let mica = look.mica && set(SYSTEMBACKDROP_TYPE, BACKDROP_MICA) && extend(-1);
    if mica {
        set(CAPTION_COLOR, COLOR_DEFAULT);
        set(TEXT_COLOR, COLOR_DEFAULT);
    } else {
        set(SYSTEMBACKDROP_TYPE, BACKDROP_AUTO);
        extend(0);
        set(CAPTION_COLOR, color(look.bar));
        set(TEXT_COLOR, color(look.text));
    }
    mica
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
    if !crate::windows_fs::shell_can_address(&shown) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Windows cannot safely open an item with this name; rename it first"));
    }
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
/// Only tests call it, to check that `sort_key` keeps this order.
#[cfg(test)]
pub fn logical_cmp(a: &[u16], b: &[u16]) -> std::cmp::Ordering {
    // SAFETY: both slices end with 0.
    let r = unsafe { StrCmpLogicalW(a.as_ptr(), b.as_ptr()) };
    r.cmp(&0)
}

/// Bytes whose order is Explorer's name order: digits compare as numbers and case is ignored,
/// as StrCmpLogicalW does. A key is made once per name and compares in a few nanoseconds,
/// where StrCmpLogicalW costs about 250 ns a call, which a folder of 500 000 names makes
/// eight million times.
pub fn sort_key(name: &str) -> Box<[u8]> {
    const NORM_IGNORECASE: u32 = 0x1;
    const SORT_DIGITSASNUMBERS: u32 = 0x8;
    const LCMAP_SORTKEY: u32 = 0x400;
    let wide: Vec<u16> = name.encode_utf16().collect();
    if wide.is_empty() { return Box::default(); }
    let map = |out: *mut u8, room: usize| {
        // SAFETY: `wide` is live for its stated length, and `out` has room for `room` bytes
        // (none when it is null). A null locale is the user's own, as StrCmpLogicalW uses.
        unsafe {
            LCMapStringEx(std::ptr::null(), LCMAP_SORTKEY | SORT_DIGITSASNUMBERS | NORM_IGNORECASE, wide.as_ptr(), wide.len() as i32,
                out, room as i32, std::ptr::null(), std::ptr::null(), 0)
        }
    };
    let mut short = [0u8; 256];
    let made = map(short.as_mut_ptr(), short.len());
    if made > 0 { return short[..made as usize].into(); }
    // A long name with many marks needs more room than most.
    let needed = map(std::ptr::null_mut(), 0);
    if needed > 0 {
        let mut long = vec![0u8; needed as usize];
        let made = map(long.as_mut_ptr(), long.len());
        if made > 0 { long.truncate(made as usize); return long.into(); }
    }
    // Not seen in practice. The lower-case name still gives a consistent order.
    name.to_lowercase().into_bytes().into()
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

/// What a test can read about this process's memory and handles.
#[cfg(test)]
pub mod measure {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn K32GetProcessMemoryInfo(process: *mut core::ffi::c_void, counters: *mut Counters, size: u32) -> i32;
        fn GetProcessHandleCount(process: *mut core::ffi::c_void, count: *mut u32) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    struct Counters { size: u32, faults: u32, peak_set: usize, set: usize, a: usize, b: usize, c: usize, d: usize, private: usize, peak_private: usize }

    pub fn private_bytes() -> usize {
        let mut counters = Counters { size: size_of::<Counters>() as u32, ..Default::default() };
        // SAFETY: `counters` is valid for writing and its size is passed.
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size_of::<Counters>() as u32); }
        counters.private
    }

    pub fn handles() -> u32 {
        let mut count = 0;
        // SAFETY: `count` is valid for writing.
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count); }
        count
    }

    /// How much private memory `work` needed at its worst, above what was in use before it.
    pub fn memory_used_by<T>(work: impl FnOnce() -> T) -> (T, usize) {
        let (start, stop) = (private_bytes(), Arc::new(AtomicBool::new(false)));
        let flag = stop.clone();
        let sampler = std::thread::spawn(move || {
            let mut most = 0;
            while !flag.load(Ordering::Relaxed) { most = most.max(private_bytes()); std::thread::sleep(std::time::Duration::from_millis(10)); }
            most
        });
        let out = work();
        stop.store(true, Ordering::Relaxed);
        (out, sampler.join().unwrap().saturating_sub(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `run.cmd.` and `run.cmd` are one name to the Shell, so opening the first ran the second.
    #[test]
    fn open_never_runs_a_neighbour_of_a_name_the_shell_cannot_address() {
        let dir = std::env::temp_dir().join(format!("fc_open_dot_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ran");
        std::fs::write(dir.join("run.cmd"), format!("@echo done> \"{}\"\r\n", marker.display())).unwrap();
        let dotted = crate::windows_fs::exact(&dir.join("run.cmd.")).unwrap();
        std::fs::write(&dotted, "harmless").unwrap();
        assert!(open(&crate::windows_fs::shown(&dotted)).is_err(), "the Shell cannot open this name");
        std::thread::sleep(std::time::Duration::from_secs(1));
        assert!(!marker.exists(), "opening run.cmd. ran run.cmd");
        std::fs::remove_file(dotted).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

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


