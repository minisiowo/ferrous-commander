//! Drives (local, removable and network) and cloud folders such as OneDrive.
//! Runs on a background thread: a sleeping network server must never freeze the window.

use std::path::{Path, PathBuf};

use crate::app::DriveInfo;

/// How long one drive may take to answer before it is listed without its size.
#[cfg(windows)]
const DRIVE_TIMEOUT_MS: u64 = 2500;

#[cfg(windows)]
mod win {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetLogicalDrives() -> u32;
        pub fn GetDriveTypeW(root: *const u16) -> u32;
        pub fn GetDiskFreeSpaceExW(root: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
        pub fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_len: *mut u32,
            flags: *mut u32,
            fs: *mut u16,
            fs_len: u32,
        ) -> i32;
        pub fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
        pub fn GetVolumeNameForVolumeMountPointW(mount: *const u16, volume: *mut u16, len: u32) -> i32;
        pub fn SetErrorMode(mode: u32) -> u32;
        pub fn DeviceIoControl(
            device: *mut core::ffi::c_void,
            code: u32,
            input: *const core::ffi::c_void,
            input_len: u32,
            output: *mut core::ffi::c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }
    #[link(name = "mpr")]
    unsafe extern "system" {
        pub fn WNetGetConnectionW(local: *const u16, remote: *mut u16, len: *mut u32) -> u32;
    }
    pub const DRIVE_REMOTE: u32 = 4;
    pub const DRIVE_NO_ROOT_DIR: u32 = 1;

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    pub fn from_wide(b: &[u16]) -> String {
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf16_lossy(&b[..n])
    }
}

#[cfg(windows)]
fn one_drive(letter: char) -> Option<DriveInfo> {
    use win::*;
    let root = format!("{letter}:\\");
    let w = wide(&root);
    // SAFETY: plain Win32 calls with NUL-terminated buffers we own.
    unsafe {
        // no "insert a disk" pop-ups for empty card readers
        SetErrorMode(0x0001 | 0x8000);
        let kind = GetDriveTypeW(w.as_ptr());
        if kind == DRIVE_NO_ROOT_DIR {
            return None;
        }
        let network = kind == DRIVE_REMOTE;
        let mut remote = String::new();
        if network {
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let local = wide(&format!("{letter}:"));
            if WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut len) == 0 {
                remote = from_wide(&buf);
            }
        }
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free);
        let mut name = vec![0u16; 261];
        let mut fs = vec![0u16; 261];
        let (mut serial, mut maxlen, mut flags) = (0u32, 0u32, 0u32);
        let label = if GetVolumeInformationW(w.as_ptr(), name.as_mut_ptr(), 261, &mut serial, &mut maxlen, &mut flags, fs.as_mut_ptr(), 261) != 0 {
            from_wide(&name)
        } else {
            String::new()
        };
        let label = if network && !remote.is_empty() {
            format!("({remote})")
        } else if let Some(target) = crate::win::subst_target(letter) {
            format!("({target})")
        } else { label };
        // Only removable media: Windows wants admin rights to take a fixed volume offline.
        let eject = matches!(kind, 2 | 5).then(|| format!("{letter}:"));
        Some(DriveInfo { path: PathBuf::from(&root), name: format!("{letter}:"), label, total, free: avail, network, remote, eject })
    }
}

#[cfg(windows)]
pub fn list() -> Vec<DriveInfo> {
    // A timed-out OS call keeps running. Do not start another for the same
    // drive on every refresh while that call is still blocked.
    static IN_FLIGHT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    struct Probe(u32);
    impl Drop for Probe {
        fn drop(&mut self) {
            IN_FLIGHT.fetch_and(!self.0, std::sync::atomic::Ordering::Relaxed);
        }
    }
    // SAFETY: no arguments.
    let mask = unsafe { win::GetLogicalDrives() };
    let (tx, rx) = std::sync::mpsc::channel();
    let mut letters = Vec::new();
    for k in 0..26u32 {
        if mask & (1 << k) != 0 {
            let letter = (b'A' + k as u8) as char;
            letters.push(letter);
            let bit = 1 << k;
            if IN_FLIGHT.fetch_or(bit, std::sync::atomic::Ordering::Relaxed) & bit != 0 {
                continue;
            }
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _probe = Probe(bit);
                let _ = tx.send((letter, one_drive(letter)));
            });
        }
    }
    drop(tx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(DRIVE_TIMEOUT_MS);
    let mut got: Vec<(char, Option<DriveInfo>)> = Vec::new();
    while got.len() < letters.len() {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(x) => got.push(x),
            Err(_) => break,
        }
    }
    let mut out: Vec<DriveInfo> = letters
        .iter()
        .filter_map(|&l| match got.iter().find(|(g, _)| *g == l) {
            Some((_, d)) => d.clone(),
            // still waking up: list it anyway, without sizes
            None => Some(DriveInfo {
                path: PathBuf::from(format!("{l}:\\")),
                name: format!("{l}:"),
                label: "(not responding)".into(),
                total: 0,
                free: 0,
                network: true,
                remote: String::new(),
                eject: None,
            }),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(not(windows))]
fn is_network_fs(fs: &str) -> bool {
    ["nfs", "cifs", "smb", "sshfs", "9p", "davfs", "fuse.rclone", "gvfs"].iter().any(|n| fs.contains(n))
}

/// Take a drive offline so it can be pulled out without losing data. `id` is
/// `DriveInfo::eject`. Blocks until the drive's data is written, so never call this on the UI
/// thread. On success, says what the user may do next.
#[cfg(target_os = "linux")]
pub fn eject(mount: &Path, id: &str) -> Result<String, String> {
    fn udisks(action: &str, device: &str) -> Result<(), String> {
        let output = std::process::Command::new("udisksctl")
            .args([action, "--no-user-interaction", "-b", device])
            .env("LC_ALL", "C").stdin(std::process::Stdio::null()).output()
            .map_err(|e| format!("Ejecting needs udisks2 (udisksctl): {e}"))?;
        if output.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()) }
    }
    crate::linux_fs::sync(mount).map_err(|e| format!("Data could not be written to the drive, so it stays connected: {e}"))?;
    // Our own short reads of this folder can collide with the first attempt.
    let mut result = udisks("unmount", id);
    for _ in 0..3 {
        if !result.as_ref().is_err_and(|e| linux_names::is_busy(e)) { break; }
        std::thread::sleep(std::time::Duration::from_millis(300));
        result = udisks("unmount", id);
    }
    result.map_err(|e| if linux_names::is_busy(&e) {
        "The drive is in use. Close the programs and terminals using it, then try again.".to_owned()
    } else { e })?;
    match udisks("power-off", id) {
        Ok(()) => Ok("It is safe to remove the drive".into()),
        // Other partitions can keep it powered, so "safe" would not be true here.
        Err(e) => Ok(format!("Unmounted, but the drive stays on ({e}). Before removing it, make sure nothing else on it is in use")),
    }
}

#[cfg(windows)]
pub fn eject(_mount: &Path, id: &str) -> Result<String, String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    const LOCK_VOLUME: u32 = 0x0009_0018;
    const DISMOUNT_VOLUME: u32 = 0x0009_0020;
    const MEDIA_REMOVAL: u32 = 0x002D_4804;
    const EJECT_MEDIA: u32 = 0x002D_4808;
    let volume = std::fs::OpenOptions::new().read(true).write(true).share_mode(3)
        .open(format!(r"\\.\{id}")).map_err(|e| format!("Could not reach the drive: {e}"))?;
    volume.sync_all().map_err(|e| format!("Data could not be written to the drive, so it stays connected: {e}"))?;
    let control = |code: u32, input: &[u8]| {
        let mut returned = 0u32;
        // SAFETY: the handle lives as long as `volume`; the input is a live buffer of the
        // given length, and these codes write no output.
        let done = unsafe { win::DeviceIoControl(volume.as_raw_handle(), code, input.as_ptr().cast(), input.len() as u32,
            std::ptr::null_mut(), 0, &mut returned, std::ptr::null_mut()) } != 0;
        if done { Ok(()) } else { Err(std::io::Error::last_os_error()) }
    };
    let mut locked = control(LOCK_VOLUME, &[]);
    for _ in 0..10 {
        if locked.is_ok() { break; }
        std::thread::sleep(std::time::Duration::from_millis(200));
        locked = control(LOCK_VOLUME, &[]);
    }
    locked.map_err(|_| "The drive is in use. Close the programs and windows using it, then try again.".to_owned())?;
    control(DISMOUNT_VOLUME, &[]).map_err(|e| format!("Could not take the drive offline: {e}"))?;
    // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }. Many drives have no such lock.
    let _ = control(MEDIA_REMOVAL, &[0]);
    control(EJECT_MEDIA, &[]).map_err(|e| format!("The drive is offline, but Windows could not eject it: {e}"))?;
    Ok("It is safe to remove the drive".into())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn eject(_mount: &Path, _id: &str) -> Result<String, String> {
    Err("Ejecting is not available on this system".into())
}

/// Where a file lives, if that is somewhere its contents should not be trusted by default.
/// Asks the system at that moment, because the drive list lags behind a stick just plugged in.
#[cfg(windows)]
pub fn foreign_place(path: &Path) -> Option<&'static str> {
    // A link on the stick may lead to the local disk, and a local link to the stick.
    windows_place(path).or_else(|| windows_place(&std::fs::canonicalize(path).ok()?))
}

/// A share is answered from the path alone, so a sleeping server is never woken.
#[cfg(windows)]
fn windows_place(path: &Path) -> Option<&'static str> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    let of_kind = |kind| match kind {
        2 | 5 => Some("a removable drive"),
        win::DRIVE_REMOTE => Some("a network location"),
        _ => None,
    };
    let Some(Component::Prefix(prefix)) = path.components().next() else { return None };
    let letter = match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter as char,
        _ => return Some("a network location"),
    };
    let root = win::wide(&format!("{letter}:\\"));
    // SAFETY: a NUL-terminated buffer we own.
    let found = of_kind(unsafe { win::GetDriveTypeW(root.as_ptr()) });
    if found.is_some() { return found; }
    // A drive can also be mounted on a folder of the local disk.
    let name: Vec<u16> = crate::windows_fs::shown(path).as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume = vec![0u16; name.len().max(261) + 1];
    // SAFETY: both buffers are live and NUL-terminated, and the size passed is the real one.
    if unsafe { win::GetVolumePathNameW(name.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0 { return None; }
    // SAFETY: GetVolumePathNameW left a NUL-terminated root.
    let kind = unsafe { win::GetDriveTypeW(volume.as_ptr()) };
    of_kind(kind).or_else(|| bus_type(&volume).is_some_and(bus_can_be_unplugged).then_some("a removable drive"))
}

/// External disks usually report themselves as fixed, so the drive type alone misses them.
#[cfg(windows)]
fn bus_can_be_unplugged(bus: u32) -> bool {
    const IEEE_1394: u32 = 0x4;
    const USB: u32 = 0x7;
    const SD: u32 = 0xC;
    const MMC: u32 = 0xD;
    matches!(bus, IEEE_1394 | USB | SD | MMC)
}

/// STORAGE_DEVICE_DESCRIPTOR.BusType of the volume mounted at `root`. Needs no rights
/// and no read from the disk: the driver answers from what it already knows.
#[cfg(windows)]
fn bus_type(root: &[u16]) -> Option<u32> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    const BUS_TYPE_OFFSET: usize = 28;
    let mut volume = [0u16; 64];
    // SAFETY: `root` ends in NUL and the length matches the buffer. Subst drives fail here.
    if unsafe { win::GetVolumeNameForVolumeMountPointW(root.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) } == 0 { return None; }
    let device = win::from_wide(&volume);
    let file = std::fs::OpenOptions::new().access_mode(0).share_mode(3).open(device.trim_end_matches('\\')).ok()?;
    // STORAGE_PROPERTY_QUERY: StorageDeviceProperty, PropertyStandardQuery.
    let query = [0u32; 3];
    let mut answer = [0u32; 256];
    let mut returned = 0u32;
    // SAFETY: both buffers are live for the call and the sizes passed are their real sizes.
    let done = unsafe { win::DeviceIoControl(file.as_raw_handle(), IOCTL_STORAGE_QUERY_PROPERTY, query.as_ptr().cast(), size_of_val(&query) as u32,
        answer.as_mut_ptr().cast(), size_of_val(&answer) as u32, &mut returned, std::ptr::null_mut()) } != 0;
    (done && returned as usize >= BUS_TYPE_OFFSET + 4).then(|| answer[BUS_TYPE_OFFSET / 4])
}

#[cfg(all(test, windows))]
mod place_tests {
    use super::*;

    #[test]
    fn the_system_disk_is_on_a_bus_that_stays() {
        let bus = bus_type(&win::wide("C:\\")).expect("the system disk answers");
        // SCSI, ATA, RAID, SAS, SATA, virtual disk, NVMe: anything else misreads the structure.
        assert!(matches!(bus, 0x1 | 0x3 | 0x8 | 0xA | 0xB | 0xE | 0x11), "bus {bus:#x}");
        assert!(!bus_can_be_unplugged(bus));
        for external in [0x4, 0x7, 0xC, 0xD] { assert!(bus_can_be_unplugged(external)); }
    }

    #[test]
    fn places_are_told_apart_however_the_path_is_written() {
        let local = std::env::temp_dir();
        assert_eq!(foreign_place(&local), None);
        assert_eq!(foreign_place(&std::fs::canonicalize(&local).unwrap()), None, "verbatim form");
        for share in [r"\\localhost\C$\Windows", r"\\?\UNC\localhost\C$\Windows", r"\\nobody.invalid\share\setup.exe"] {
            assert_eq!(foreign_place(Path::new(share)), Some("a network location"), "{share}");
        }
    }

    /// A drive letter over a share is as foreign as the share.
    #[test]
    fn a_subst_letter_over_a_share_is_a_network_location() {
        // SAFETY: no arguments.
        let used = unsafe { win::GetLogicalDrives() };
        let Some(letter) = ('M'..='W').rev().find(|l| used & (1 << (*l as u8 - b'A')) == 0) else { return };
        let (drive, subst) = (format!("{letter}:"), crate::win::system_program("subst.exe"));
        let added = std::process::Command::new(&subst).args([&drive, r"\\localhost\C$\Windows"]).status();
        if !added.is_ok_and(|s| s.success()) { eprintln!("Skipped: subst refused"); return; }
        let place = foreign_place(Path::new(&format!(r"{drive}\system32\notepad.exe")));
        let _ = std::process::Command::new(&subst).args([&drive, "/D"]).status();
        assert_eq!(place, Some("a network location"));
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn foreign_place(_path: &Path) -> Option<&'static str> { None }

#[cfg(target_os = "linux")]
pub fn foreign_place(path: &Path) -> Option<&'static str> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let place = |path: &Path| {
        let (source, mount, fs) = linux_names::mount_of(&mounts, path)?;
        if is_network_fs(&fs) { return Some("a network location"); }
        let device = std::fs::canonicalize(&source).unwrap_or_else(|_| PathBuf::from(&source));
        let removable = mount.starts_with("/run/media") || mount.starts_with("/media") || linux_names::is_removable(&device);
        removable.then_some("a removable drive")
    };
    // A link on the stick may lead to the local disk, and a local link to the stick.
    place(path).or_else(|| place(&std::fs::canonicalize(path).ok()?))
}

/// Read Linux's existing device metadata without launching utilities or probing disks.
#[cfg(target_os = "linux")]
mod linux_names {
    use std::path::{Path, PathBuf};

    fn decode_label(s: &str) -> String {
        let mut bytes = Vec::new();
        let mut i = 0;
        while i < s.len() {
            let b = s.as_bytes();
            if b[i] == b'\\' && b.get(i + 1) == Some(&b'x') && i + 3 < b.len() {
                // Bytes, not a string slice: the two after `\x` may split a character.
                let hex = std::str::from_utf8(&b[i + 2..i + 4]).ok();
                if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    bytes.push(v);
                    i += 4;
                    continue;
                }
            }
            bytes.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn labels() -> std::collections::HashMap<PathBuf, String> {
        std::fs::read_dir("/dev/disk/by-label").into_iter().flatten().flatten()
            .filter_map(|e| Some((std::fs::canonicalize(e.path()).ok()?,
                decode_label(&e.file_name().to_string_lossy()))))
            .collect()
    }

    pub fn device_info(device: &Path) -> (bool, String) {
        let Some(name) = device.file_name() else { return (false, String::new()) };
        let Ok(sys) = std::fs::canonicalize(Path::new("/sys/class/block").join(name)) else {
            return (false, String::new());
        };
        let usb = sys.components().any(|c| c.as_os_str().to_string_lossy().starts_with("usb"));
        let mut model = String::new();
        for dir in sys.ancestors().take_while(|p| p.starts_with("/sys/devices")) {
            if model.is_empty() {
                model = std::fs::read_to_string(dir.join("device/model")).unwrap_or_default().trim().to_owned();
            }
        }
        (usb, model)
    }

    /// Added drives only. A disk holding the running system never offers this, however
    /// the kernel describes it.
    pub fn can_eject(mount: &Path, kernel_removable: bool, network: bool) -> bool {
        const SYSTEM: [&str; 5] = ["/", "/home", "/boot", "/usr", "/var"];
        !network && !SYSTEM.iter().any(|s| mount == Path::new(s))
            && (kernel_removable || mount.starts_with("/run/media") || mount.starts_with("/media"))
    }

    pub fn is_busy(error: &str) -> bool {
        let error = error.to_lowercase();
        error.contains("devicebusy") || error.contains("target is busy") || error.contains("device is busy")
    }

    /// True for USB devices and for anything the kernel calls removable, such as SD cards.
    pub fn is_removable(device: &Path) -> bool {
        let Some(name) = device.file_name() else { return false };
        let Ok(sys) = std::fs::canonicalize(Path::new("/sys/class/block").join(name)) else { return false };
        // The flag lives on the whole disk, one level above its partitions.
        device_info(device).0 || sys.ancestors().take(2).any(|dir|
            std::fs::read_to_string(dir.join("removable")).is_ok_and(|v| v.trim() == "1"))
    }

    /// Source, mount point and filesystem of the mount holding `path`, from the text of
    /// /proc/self/mounts. The last of equal mount points is the one in effect.
    pub fn mount_of(mounts: &str, path: &Path) -> Option<(String, PathBuf, String)> {
        // That file writes spaces and a few other characters as \ooo.
        let unescape = |field: &str| {
            let (b, mut out, mut i) = (field.as_bytes(), Vec::new(), 0);
            while i < b.len() {
                let octal = (b[i] == b'\\' && i + 3 < b.len()).then(|| std::str::from_utf8(&b[i + 1..i + 4]).ok()
                    .and_then(|o| u8::from_str_radix(o, 8).ok())).flatten();
                match octal {
                    Some(byte) => { out.push(byte); i += 4; }
                    None => { out.push(b[i]); i += 1; }
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        };
        mounts.lines().filter_map(|line| {
            let mut fields = line.split(' ');
            let (source, mount, fs) = (fields.next()?, fields.next()?, fields.next()?);
            let mount = PathBuf::from(unescape(mount));
            path.starts_with(&mount).then(|| (unescape(source), mount, fs.to_owned()))
        }).max_by_key(|(_, mount, _)| mount.components().count())
    }

    pub fn title(mount: &Path, volume: Option<&str>, model: &str, removable: bool, network: bool) -> String {
        if mount == Path::new("/") { return "System disk".into(); }
        if mount == Path::new("/home") { return "Home".into(); }
        let leaf = mount.file_name().unwrap_or_default().to_string_lossy();
        // UUID mount directories are useful as paths, but make poor display names.
        let uuid = leaf.len() >= 32 && leaf.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        let name = volume.filter(|v| !v.trim().is_empty()).unwrap_or_else(|| {
            if !leaf.is_empty() && !uuid { &leaf }
            else if !model.is_empty() { model }
            else if network { "Network drive" }
            else { "External disk" }
        });
        if removable { format!("USB · {name}") } else { name.to_owned() }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn readable_mounted_device_names() {
            assert_eq!(title(Path::new("/"), None, "", false, false), "System disk");
            assert_eq!(title(Path::new("/home"), None, "", false, false), "Home");
            assert_eq!(title(Path::new("/run/media/user/12345678-abcd-1234-abcd-123456789abc"), Some("Photos"), "Kingston", true, false), "USB · Photos");
            assert_eq!(title(Path::new("/media/12345678-abcd-1234-abcd-123456789abc"), None, "Samsung", true, false), "USB · Samsung");
            assert_eq!(title(Path::new("/mnt/Backup"), None, "", false, false), "Backup");
            assert_eq!(decode_label(r"Family\x20Photos"), "Family Photos");
            assert_eq!(decode_label(r"Zażółć\x20USB"), "Zażółć USB");
            assert_eq!(decode_label(r"\xaż"), r"\xaż", "a volume label must not crash the app");
        }
        #[test]
        fn only_added_drives_can_be_ejected() {
            assert!(can_eject(Path::new("/run/media/me/Stick"), true, false));
            assert!(can_eject(Path::new("/run/media/me/Backup"), false, false), "a second disk mounted by the desktop");
            assert!(can_eject(Path::new("/mnt/card"), true, false));
            assert!(!can_eject(Path::new("/mnt/data"), false, false), "fixed disk from fstab");
            assert!(!can_eject(Path::new("/run/media/me/share"), false, true), "network share");
            for system in ["/", "/home", "/boot"] {
                assert!(!can_eject(Path::new(system), true, false), "{system} runs from a USB disk");
            }
        }
        #[test]
        fn a_busy_drive_is_recognised() {
            assert!(is_busy("Error unmounting /dev/sda: GDBus.Error:org.freedesktop.UDisks2.Error.DeviceBusy: target is busy"));
            assert!(!is_busy("Error unmounting /dev/sda: Not authorized"));
        }
        #[test]
        fn the_mount_holding_a_path_is_the_deepest_and_latest() {
            let mounts = "/dev/root / btrfs rw 0 0\n/dev/old /run/media/me/My\\040Stick ext4 rw 0 0\n/dev/sda1 /run/media/me/My\\040Stick vfat rw 0 0\n//nas/share /mnt/nas cifs rw 0 0\n";
            let of = |p: &str| mount_of(mounts, Path::new(p)).map(|(source, _, fs)| (source, fs));
            assert_eq!(of("/run/media/me/My Stick/setup.exe"), Some(("/dev/sda1".into(), "vfat".into())));
            assert_eq!(of("/mnt/nas/run.sh"), Some(("//nas/share".into(), "cifs".into())));
            assert_eq!(of("/mnt/nasty/run.sh"), Some(("/dev/root".into(), "btrfs".into())), "a whole path component");
        }
    }
}

#[cfg(not(windows))]
pub fn list() -> Vec<DriveInfo> {
    let mut out: Vec<DriveInfo> = Vec::new();
    #[cfg(target_os = "linux")]
    let labels = linux_names::labels();
    if let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() {
        // Keep useful entry points, but suppress repeated Btrfs subvolumes/bind mounts.
        #[cfg(target_os = "linux")]
        let primary_sources: std::collections::HashSet<_> = disks.iter()
            .filter(|d| matches!(d.mount_point().to_str(), Some("/" | "/home")))
            .map(|d| d.name().to_os_string()).collect();
        for d in disks.iter() {
            let mp = d.mount_point().to_path_buf();
            let s = mp.to_string_lossy().to_string();
            let hidden_mount = ["/proc", "/sys", "/dev", "/snap", "/boot", "/var/lib", "/tmp"].iter().any(|p| mp.starts_with(p))
                || (mp.starts_with("/run") && !mp.starts_with("/run/media"));
            if hidden_mount || out.iter().any(|o| o.path == mp) {
                continue;
            }
            let fs = d.file_system().to_string_lossy().to_lowercase();
            let network = is_network_fs(&fs);
            #[cfg(target_os = "linux")]
            if fs == "btrfs" && primary_sources.contains(d.name()) && s != "/" && s != "/home" {
                continue;
            }
            let source = d.name().to_string_lossy().into_owned();
            #[cfg(target_os = "linux")]
            let (name, label) = {
                let device = std::fs::canonicalize(&source).unwrap_or_else(|_| PathBuf::from(&source));
                let (usb, model) = linux_names::device_info(&device);
                let volume = labels.get(&device).map(String::as_str);
                let name = linux_names::title(&mp, volume, &model, usb, network);
                let name = if d.is_removable() && !usb { format!("Removable · {name}") } else { name };
                let kind = if network { "Network drive" } else if usb { "USB drive" }
                    else if d.is_removable() { "Removable drive" } else { "Local disk" };
                (name, format!("{kind}\n{model}\n{source}\n{fs}").replace("\n\n", "\n"))
            };
            #[cfg(not(target_os = "linux"))]
            let (name, label) = (s.clone(), source);
            #[cfg(target_os = "linux")]
            let eject = linux_names::can_eject(&mp, d.is_removable(), network).then_some(source);
            #[cfg(not(target_os = "linux"))]
            let eject = None;
            out.push(DriveInfo {
                path: mp,
                name,
                label,
                total: d.total_space(),
                free: d.available_space(),
                network,
                remote: if network { d.name().to_string_lossy().into_owned() } else { String::new() },
                eject,
            });
        }
    }
    if !out.iter().any(|d| d.path == std::path::Path::new("/")) {
        out.push(DriveInfo { path: PathBuf::from("/"), name: "System disk".into(), label: "File system".into(),
            total: 0, free: 0, network: false, remote: String::new(), eject: None });
    }
    // GNOME/GIO network mounts (smb://, sftp://, Google Drive…) live under $XDG_RUNTIME_DIR/gvfs
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        if let Ok(rd) = std::fs::read_dir(PathBuf::from(rt).join("gvfs")) {
            for de in rd.flatten() {
                let name = de.file_name().to_string_lossy().into_owned();
                out.push(DriveInfo { path: de.path(), name: name.clone(), label: String::new(), total: 0, free: 0, network: true, remote: name, eject: None });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Cloud-synced folders: OneDrive (personal and work), Dropbox, Google Drive, iCloud, Nextcloud…
pub fn cloud_folders() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for var in ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"] {
        if let Some(p) = std::env::var_os(var) {
            v.push(PathBuf::from(p));
        }
    }
    let prefixes = ["onedrive", "dropbox", "google drive", "googledrive", "icloud", "icloud drive", "iclouddrive", "nextcloud", "owncloud", "pcloud", "box", "mega", "proton drive"];
    if let Some(home) = dirs::home_dir()
        && let Ok(rd) = std::fs::read_dir(&home) {
            for de in rd.flatten() {
                let n = de.file_name().to_string_lossy().to_lowercase();
                if prefixes.iter().any(|p| n == *p || n.starts_with(&format!("{p} ")) || (*p == "onedrive" && n.starts_with("onedrive"))) && de.path().is_dir() {
                    v.push(de.path());
                }
            }
        }
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| p.is_dir() && seen.insert(p.to_string_lossy().to_lowercase()));
    v.sort();
    v
}
