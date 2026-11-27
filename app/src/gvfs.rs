//! Phones, e-readers, cameras and iPhones reached through GVfs (MTP, PTP and AFC). They
//! have no block device, so the kernel's mount table never lists them. GIO finds them,
//! and its FUSE bridge under `$XDG_RUNTIME_DIR/gvfs` gives each one a folder, but only
//! once it is mounted. Nautilus mounts them through GIO, and so does this module, by
//! running `gio`, which ships with GLib on every desktop that has GVfs at all.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Devices plugged in by cable, as opposed to network shares GVfs also serves.
const DEVICE_SCHEMES: [&str; 3] = ["mtp", "gphoto2", "afc"];

/// A device GIO's volume monitors see.
#[derive(Debug, PartialEq)]
pub struct Device {
    pub name: String,
    /// What `gio mount` takes, such as `mtp://maker_model_serial/`.
    pub uri: String,
    pub mounted: bool,
}

/// Where GVfs shows mounted locations as folders.
pub fn root() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("gvfs"))
}

/// The folder of the GVfs mount holding `path`, if `path` is inside one.
fn mount_folder(path: &Path) -> Option<PathBuf> {
    let root = root()?;
    let first = path.strip_prefix(&root).ok()?.components().next()?;
    Some(root.join(first))
}

/// Inside any GVfs mount: copies there must not rely on what a real filesystem offers.
pub fn is_gvfs_path(path: &Path) -> bool {
    mount_folder(path).is_some()
}

/// Inside a phone, e-reader or camera mounted through GVfs.
pub fn is_device_path(path: &Path) -> bool {
    mount_folder(path).is_some_and(|folder| folder.file_name()
        .is_some_and(|name| scheme_of_folder(&name.to_string_lossy()).is_some()))
}

fn scheme(uri: &str) -> Option<&str> {
    uri.split_once("://").map(|(scheme, _)| scheme)
}

fn host(uri: &str) -> Option<&str> {
    uri.split_once("://").map(|(_, rest)| rest.split('/').next().unwrap_or_default()).filter(|h| !h.is_empty())
}

fn scheme_of_folder(folder: &str) -> Option<&'static str> {
    DEVICE_SCHEMES.into_iter().find(|s| folder.strip_prefix(s).is_some_and(|rest| rest.starts_with(":host=")))
}

/// The folder GVfs gives a device's mount, as in `mtp:host=maker_model_serial`. It is
/// only a guess until the device is mounted: `local_path` asks for the real one.
pub fn folder_name(uri: &str) -> Option<String> {
    let scheme = scheme(uri).filter(|s| DEVICE_SCHEMES.contains(s))?;
    Some(format!("{scheme}:host={}", host(uri)?))
}

/// The URI of a device folder found under `root()` that no volume monitor reported.
pub fn uri_of_folder(folder: &str) -> Option<String> {
    let scheme = scheme_of_folder(folder)?;
    let host = folder.split_once(":host=")?.1.split(',').next()?;
    Some(format!("{scheme}://{host}/"))
}

/// What kind of device the user plugged in, in their words.
pub fn kind(uri: &str) -> &'static str {
    match scheme(uri) {
        Some("gphoto2") => "Camera (PTP)",
        Some("afc") => "iPhone or iPad",
        _ => "Phone or e-reader (MTP)",
    }
}

/// Parse `gio mount --list --detail`. A volume's mount is indented below it.
pub fn parse(text: &str) -> Vec<Device> {
    let mut out: Vec<Device> = Vec::new();
    // The volume being read and the indentation of its heading.
    let mut current: Option<(usize, usize)> = None;
    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if current.is_some_and(|(_, at)| indent <= at) {
            current = None;
        }
        if let Some(rest) = line.strip_prefix("Volume(") {
            let name = rest.split_once("): ").map(|(_, name)| name).unwrap_or_default();
            out.push(Device { name: name.to_owned(), uri: String::new(), mounted: false });
            current = Some((out.len() - 1, indent));
        } else if let Some((at, _)) = current {
            if line.starts_with("Mount(") {
                out[at].mounted = true;
            } else if let Some(uri) = line.strip_prefix("activation_root=") {
                out[at].uri = uri.to_owned();
            }
        }
    }
    out.retain(|d| folder_name(&d.uri).is_some());
    for d in &mut out {
        if d.name.trim().is_empty() { d.name = host(&d.uri).unwrap_or("Device").replace('_', " "); }
    }
    out
}

/// Run `gio` with a time limit, so a stuck volume monitor cannot hold up the drive list.
fn gio(args: &[&str], limit: Duration) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    // UTF-8 for device names, and messages in a language this code can recognise.
    let mut child = Command::new("gio").args(args).env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
        .map_err(|e| format!("Devices like this need GIO (the gio command from GLib): {e}"))?;
    let (mut stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
    let reader = std::thread::spawn(move || {
        let (mut out, mut err) = (String::new(), String::new());
        if let Some(s) = stdout.as_mut() { let _ = s.read_to_string(&mut out); }
        if let Some(s) = stderr.as_mut() { let _ = s.read_to_string(&mut err); }
        (out, err)
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The device did not answer in time".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = reader.join().unwrap_or_default();
    if status.success() { Ok(out) } else { Err(err.trim().trim_start_matches("gio: ").to_owned()) }
}

/// Every device GIO sees, mounted or not. Empty where GIO is missing or does not answer.
pub fn devices() -> Vec<Device> {
    gio(&["mount", "--list", "--detail"], Duration::from_secs(3)).map(|text| parse(&text)).unwrap_or_default()
}

/// The folder of a mounted location, as GIO reports it.
pub fn local_path(uri: &str) -> Option<PathBuf> {
    let info = gio(&["info", "--attributes=standard::name", uri], Duration::from_secs(5)).ok()?;
    info.lines().find_map(|l| l.strip_prefix("local path: ")).map(PathBuf::from)
}

/// Mount the device, then return its folder. Blocks while the device lists its storage,
/// which can take a while on a full phone, so never call this on the UI thread.
pub fn mount(uri: &str) -> Result<PathBuf, String> {
    match gio(&["mount", uri], Duration::from_secs(90)) {
        Ok(_) => {}
        Err(e) if e.contains("already mounted") => {}
        Err(e) => return Err(e),
    }
    local_path(uri).ok_or_else(|| "The device was connected, but the system gave it no folder. GVfs's FUSE service (gvfsd-fuse) may not be running.".into())
}

/// Unmount, so the cable can be pulled. GVfs finishes the transfers it has started first.
pub fn unmount(uri: &str) -> Result<(), String> {
    gio(&["mount", "--unmount", uri], Duration::from_secs(60)).map(|_| ())
}

/// Size and free space, or nothing if the device does not answer within a moment.
pub fn space(folder: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(folder.as_os_str().as_bytes()).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // SAFETY: zeroed plain data, filled by the call; the path is NUL-terminated.
        let result = unsafe {
            let mut st: libc::statvfs = std::mem::zeroed();
            (libc::statvfs(path.as_ptr(), &mut st) == 0)
                .then(|| (st.f_blocks as u64 * st.f_frsize as u64, st.f_bavail as u64 * st.f_frsize as u64))
        };
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_millis(1500)).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "\
Drive(0): KBG6AZNV512G LA KIOXIA
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  ids:
   unix-device: '/dev/nvme0n1'
  can_eject=0
Volume(0): CalypsoPlus
  Type: GProxyVolume (GProxyVolumeMonitorMTP)
  ids:
   unix-device: '/dev/bus/usb/001/006'
  activation_root=mtp://rockchip_CalypsoPlus_IB-CAP-R-88013/
  can_mount=1
  should_automount=1
Volume(1): Canon Camera
  Type: GProxyVolume (GProxyVolumeMonitorGPhoto2)
  activation_root=gphoto2://Canon_Inc._Canon_Digital_Camera/
  Mount(0): Canon Camera -> gphoto2://Canon_Inc._Canon_Digital_Camera/
    Type: GProxyShadowMount (GProxyVolumeMonitorGPhoto2)
    default_location=gphoto2://Canon_Inc._Canon_Digital_Camera/
Drive(1): Kingston DataTraveler
  Volume(0): STICK
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
    ids:
     uuid: '1234-ABCD'
Mount(0): share on nas -> smb://nas/share/
  Type: GDaemonMount
";

    #[test]
    fn devices_are_read_from_gio_and_disks_and_shares_are_left_out() {
        assert_eq!(parse(LISTING), vec![
            Device { name: "CalypsoPlus".into(), uri: "mtp://rockchip_CalypsoPlus_IB-CAP-R-88013/".into(), mounted: false },
            Device { name: "Canon Camera".into(), uri: "gphoto2://Canon_Inc._Canon_Digital_Camera/".into(), mounted: true },
        ]);
        assert_eq!(parse(""), vec![]);
        assert_eq!(parse("Volume(0): \n  activation_root=mtp://Maker_Reader/\n")[0].name, "Maker Reader", "a nameless volume");
    }

    #[test]
    fn a_device_folder_matches_its_uri() {
        let uri = "mtp://rockchip_CalypsoPlus_IB-CAP-R-88013/";
        assert_eq!(folder_name(uri).as_deref(), Some("mtp:host=rockchip_CalypsoPlus_IB-CAP-R-88013"));
        assert_eq!(uri_of_folder("mtp:host=rockchip_CalypsoPlus_IB-CAP-R-88013").as_deref(), Some(uri));
        assert_eq!(folder_name("smb://nas/share/"), None, "a network share is not a device");
        assert_eq!(uri_of_folder("smb-share:server=nas,share=share"), None);
        assert_eq!(folder_name("mtp:///"), None);
    }

    #[test]
    fn device_paths_are_recognised_below_the_gvfs_folder() {
        let Some(root) = root() else { return };
        assert!(is_device_path(&root.join("mtp:host=Maker_Reader/Internal storage/Books")));
        assert!(is_device_path(&root.join("afc:host=00008030-001")));
        assert!(!is_device_path(&root.join("sftp:host=server/home")), "a network share");
        assert!(is_gvfs_path(&root.join("sftp:host=server/home")));
        assert!(!is_gvfs_path(&root), "the folder holding the mounts");
        assert!(!is_gvfs_path(Path::new("/run/media/me/mtp:host=x")));
    }
}
