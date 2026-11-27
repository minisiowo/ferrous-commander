//! Ferrous Commander as the desktop's file manager on Linux.
//!
//! Two things make a program the file manager there:
//! - the default application for folders (`inode/directory`), which `xdg-open` and most
//!   programs use to open a folder, and
//! - the `org.freedesktop.FileManager1` D-Bus service, which browsers and other programs
//!   call to show a file in its folder ("Show in folder" after a download). D-Bus starts
//!   the program that a service file names when nobody owns that name.
//!
//! Both are set together, only when the user asks for it. While it is on, a running
//! Ferrous Commander owns the service, and every request opens a tab in its window.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const SERVICE: &str = "org.freedesktop.FileManager1";
const OBJECT: &str = "/org/freedesktop/FileManager1";
/// The same interface under a name only Ferrous Commander uses: a second start with a
/// folder hands it to the running window, and never to another file manager.
const OWN_NAME: &str = "io.github.FerrousCommander";
const DESKTOP_ID: &str = "ferrous-commander.desktop";
const FOLDER_TYPE: &str = "inode/directory";
/// Written into the service file, to give the folder default back when this is turned off.
const PREVIOUS: &str = "# Previous default for folders: ";

/// A folder to open in a tab, with items to select in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Show {
    pub folder: PathBuf,
    pub select: Vec<PathBuf>,
    /// Open the Properties of the first selected item.
    pub properties: bool,
}

/// Requests not yet taken by the window, and how to wake it for them.
static INBOX: Mutex<Vec<Show>> = Mutex::new(Vec::new());
static WAKE: Mutex<Option<crate::background::Wake>> = Mutex::new(None);
/// The bus connection holding the names while the service runs.
static CONNECTION: Mutex<Option<zbus::blocking::Connection>> = Mutex::new(None);

/// Requests that arrived since the last call. The first call also lets the service wake
/// the window for later ones.
pub fn take(wake: &crate::background::Wake) -> Vec<Show> {
    let mut slot = WAKE.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_none() { *slot = Some(wake.clone()); }
    drop(slot);
    std::mem::take(&mut *INBOX.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Requests from the command line, for the window about to open.
pub fn queue(shows: Vec<Show>) {
    deliver(shows);
}

fn deliver(shows: Vec<Show>) {
    if shows.is_empty() { return; }
    INBOX.lock().unwrap_or_else(|e| e.into_inner()).extend(shows);
    if let Some(wake) = WAKE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() { wake.repaint(); }
}

/// A `file://` URI as a path. Other schemes have no folder here.
fn path_of(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x` are both a local file.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') { return None; }
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(rest).collect();
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// Folders open as they are. Anything else opens its folder with it selected; items in
/// one folder share a tab. A folder asked to be shown as an item is selected in its parent.
pub fn plan(paths: impl IntoIterator<Item = PathBuf>, as_items: bool, properties: bool) -> Vec<Show> {
    let mut out: Vec<Show> = Vec::new();
    for path in paths {
        let Ok(path) = std::path::absolute(&path) else { continue };
        let parent = path.parent().map(Path::to_path_buf);
        match parent {
            Some(parent) if as_items || !path.is_dir() => match out.iter_mut().find(|s| s.folder == parent && !s.select.is_empty()) {
                Some(show) => show.select.push(path),
                None => out.push(Show { folder: parent, select: vec![path], properties }),
            },
            _ => out.push(Show { folder: path, select: Vec::new(), properties: false }),
        }
    }
    out
}

fn plan_uris(uris: &[String], as_items: bool, properties: bool) -> Vec<Show> {
    plan(uris.iter().filter_map(|u| path_of(u)), as_items, properties)
}

struct FileManager;

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_folders(&self, uris: Vec<String>, _startup_id: &str) {
        deliver(plan_uris(&uris, false, false));
    }
    fn show_items(&self, uris: Vec<String>, _startup_id: &str) {
        deliver(plan_uris(&uris, true, false));
    }
    fn show_item_properties(&self, uris: Vec<String>, _startup_id: &str) {
        deliver(plan_uris(&uris, true, true));
    }
}

/// Answer file manager requests while this is the default file manager. Does not wait
/// for the bus, so the window opens just as fast. A request D-Bus started this program
/// for arrives once the names are taken.
pub fn start_if_default() {
    if is_default() { std::thread::spawn(|| { let _ = start(); }); }
}

fn start() -> Result<(), String> {
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    let mut slot = CONNECTION.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() { return Ok(()); }
    let connection = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    connection.object_server().at(OBJECT, FileManager).map_err(|e| e.to_string())?;
    // Not queued: a second window must not take over requests after this one closes, as
    // D-Bus then starts a new one. Another file manager running already keeps the name.
    for name in [SERVICE, OWN_NAME] {
        match connection.request_name_with_flags(name, RequestNameFlags::DoNotQueue.into()) {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
            // zbus reports a name held by someone else as an error.
            Ok(_) | Err(zbus::Error::NameTaken) if name == SERVICE => return Err("Another file manager is running and answers file manager requests. Close it, then try again.".into()),
            Ok(_) | Err(zbus::Error::NameTaken) => return Err("Another Ferrous Commander window answers file manager requests".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
    *slot = Some(connection);
    Ok(())
}

fn stop() {
    if let Some(connection) = CONNECTION.lock().unwrap_or_else(|e| e.into_inner()).take() {
        for name in [SERVICE, OWN_NAME] { let _ = connection.release_name(name); }
    }
}

/// Follow the setting changed elsewhere: from the command line, or in another window,
/// which may also have closed and left the requests to this one.
pub fn follow_setting() {
    let serving = CONNECTION.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    match (is_default(), serving) {
        (true, false) => { std::thread::spawn(|| { let _ = start(); }); }
        (false, true) => stop(),
        _ => {}
    }
}

/// Hand folders and files from the command line to the window that is already open,
/// as `xdg-open` does when it opens a folder. False if there is none to take them.
pub fn hand_over(paths: &[PathBuf]) -> bool {
    if paths.is_empty() || !is_default() { return false; }
    let send = || -> zbus::Result<()> {
        let connection = zbus::blocking::connection::Builder::session()?.method_timeout(Duration::from_secs(5)).build()?;
        let uris: Vec<String> = paths.iter().filter_map(|p| std::path::absolute(p).ok()).map(|p| uri_of(&p)).collect();
        let (folders, items): (Vec<String>, Vec<String>) = uris.into_iter().partition(|u| path_of(u).is_some_and(|p| p.is_dir()));
        let proxy = zbus::blocking::Proxy::new(&connection, OWN_NAME, OBJECT, SERVICE)?;
        // Only a name someone holds: asking a missing one could make D-Bus start another copy.
        if !zbus::blocking::fdo::DBusProxy::new(&connection)?.name_has_owner(OWN_NAME.try_into()?)? {
            return Err(zbus::Error::Failure("no window to take them".into()));
        }
        if !folders.is_empty() { proxy.call_method("ShowFolders", &(folders, ""))?; }
        if !items.is_empty() { proxy.call_method("ShowItems", &(items, ""))?; }
        Ok(())
    };
    send().is_ok()
}

fn uri_of(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'/').remove(b'-').remove(b'_').remove(b'.').remove(b'~');
    format!("file://{}", percent_encoding::percent_encode(path.as_os_str().as_bytes(), KEEP))
}

fn service_file() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("dbus-1/services").join(format!("{SERVICE}.service")))
}

/// What D-Bus runs to start this program: the copy the user turned the setting on in.
/// The installed copy can be an older one, without this service.
fn program() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("The program's location is unknown: {e}"))?;
    // A program rebuilt while it runs: Linux reports the replaced file as deleted.
    let shown = exe.to_string_lossy();
    Ok(shown.strip_suffix(" (deleted)").map(PathBuf::from).unwrap_or(exe))
}

/// After installing, the service starts the installed copy.
pub fn follow_install(exe: &Path) {
    let Some(file) = service_file() else { return };
    let Ok(text) = std::fs::read_to_string(&file) else { return };
    if !is_default() { return; }
    let _ = std::fs::write(&file, service_text(exe, previous_default(&text)));
    reload_bus();
}

/// On when the user's service file starts Ferrous Commander. It overrides the one a
/// file manager installed for the whole system.
pub fn is_default() -> bool {
    service_file().and_then(|f| std::fs::read_to_string(f).ok())
        .is_some_and(|text| text.lines().any(|l| l.starts_with("Exec=") && l.contains("ferrous-commander")))
}

fn folder_default() -> Option<String> {
    let out = std::process::Command::new("xdg-mime").args(["query", "default", FOLDER_TYPE])
        .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|s| !s.is_empty())
}

fn set_folder_default(desktop: &str) -> Result<(), String> {
    let run = |program: &str, args: &[&str]| std::process::Command::new(program).args(args)
        .stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())
        .and_then(|o| if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()) });
    run("xdg-mime", &["default", desktop, FOLDER_TYPE])
        .or_else(|e| run("gio", &["mime", FOLDER_TYPE, desktop]).map_err(|_| format!("Setting the default needs xdg-utils (xdg-mime): {e}")))
}

/// The service file's text: D-Bus reads `Exec` like a shell, so the path is quoted.
fn service_text(program: &Path, previous: Option<&str>) -> String {
    let exec = program.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
    let previous = previous.map(|p| format!("{PREVIOUS}{p}\n")).unwrap_or_default();
    format!("# Written by Ferrous Commander: Settings, \"Default file manager\".\n{previous}[D-BUS Service]\nName={SERVICE}\nExec=\"{exec}\"\n")
}

fn previous_default(text: &str) -> Option<&str> {
    text.lines().find_map(|l| l.strip_prefix(PREVIOUS)).map(str::trim).filter(|p| !p.is_empty() && *p != DESKTOP_ID)
}

/// dbus-broker, the usual session bus, reads service files only when asked to. Without
/// this, D-Bus would go on starting the previous file manager until the next login.
fn reload_bus() {
    if let Ok(connection) = zbus::blocking::connection::Builder::session().and_then(|b| b.method_timeout(Duration::from_secs(5)).build())
        && let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(&connection) {
            let _ = bus.reload_config();
        }
}

/// Make Ferrous Commander the file manager, or give the role back to the one before.
pub fn set_default(on: bool) -> Result<String, String> {
    let file = service_file().ok_or("No data folder")?;
    if on {
        let desktop = dirs::data_dir().map(|d| d.join("applications").join(DESKTOP_ID));
        let system = Path::new("/usr/share/applications").join(DESKTOP_ID);
        if !desktop.is_some_and(|d| d.exists()) && !system.exists() {
            return Err("Install Ferrous Commander first (Application, Install for this user), so the desktop can start it".into());
        }
        let previous = folder_default();
        let previous = previous.as_deref().filter(|p| *p != DESKTOP_ID);
        if let Some(dir) = file.parent() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; }
        std::fs::write(&file, service_text(&program()?, previous)).map_err(|e| e.to_string())?;
        reload_bus();
        set_folder_default(DESKTOP_ID)?;
        match start() {
            Ok(()) => Ok("Ferrous Commander is now the default file manager".into()),
            Err(e) => Ok(format!("Ferrous Commander is now the default file manager. {e}")),
        }
    } else {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        stop();
        if file.exists() { std::fs::remove_file(&file).map_err(|e| e.to_string())?; }
        reload_bus();
        if folder_default().as_deref() == Some(DESKTOP_ID) && let Some(previous) = previous_default(&text) {
            set_folder_default(previous)?;
        }
        Ok("Ferrous Commander is no longer the default file manager".into())
    }
}

/// Called by Uninstall: what this module set up must not outlive the program.
pub fn forget() {
    if is_default() { let _ = set_default(false); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_from_browsers_become_paths() {
        assert_eq!(path_of("file:///home/me/Pobrane/Za%C5%BC%C3%B3%C5%82%C4%87%20plik.pdf"), Some(PathBuf::from("/home/me/Pobrane/Zażółć plik.pdf")));
        assert_eq!(path_of("file://localhost/tmp/a%23b"), Some(PathBuf::from("/tmp/a#b")));
        assert_eq!(path_of("sftp://server/home"), None);
        assert_eq!(path_of("file://server/share"), None, "a file on another host");
        let odd = Path::new("/tmp/50% off #1 ?.txt");
        assert_eq!(path_of(&uri_of(odd)).as_deref(), Some(odd), "round trip");
    }

    #[test]
    fn items_open_their_folder_selected_and_folders_open_as_they_are() {
        let root = std::env::temp_dir().join(format!("fc_file_manager_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for f in ["a.pdf", "b.pdf"] { std::fs::write(root.join(f), "x").unwrap(); }
        let items = plan([root.join("a.pdf"), root.join("b.pdf"), root.join("sub")], true, false);
        assert_eq!(items, vec![Show { folder: root.clone(), select: vec![root.join("a.pdf"), root.join("b.pdf"), root.join("sub")], properties: false }]);
        let folders = plan([root.join("sub"), root.join("a.pdf")], false, false);
        assert_eq!(folders, vec![
            Show { folder: root.join("sub"), select: vec![], properties: false },
            Show { folder: root.clone(), select: vec![root.join("a.pdf")], properties: false },
        ], "a file among folders is shown selected");
        assert!(plan([root.join("a.pdf")], true, true)[0].properties);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// On a private bus, so the desktop's own file manager is not touched:
    /// FERROUS_PRIVATE_BUS=1 XDG_DATA_HOME=$(mktemp -d) dbus-run-session -- cargo test file_manager_bus -- --ignored
    #[test]
    #[ignore = "Needs a private session bus from dbus-run-session"]
    fn file_manager_bus_requests_reach_the_window() {
        assert_eq!(std::env::var("FERROUS_PRIVATE_BUS").as_deref(), Ok("1"), "run on a private bus");
        let data = dirs::data_dir().unwrap();
        assert!(data.starts_with(std::env::temp_dir()), "XDG_DATA_HOME must be a temporary folder");
        let wake = crate::background::Wake::default();
        assert!(!is_default() && !hand_over(&[std::env::temp_dir()]), "off: nothing is handed over");
        start().unwrap();
        let caller = zbus::blocking::Connection::session().unwrap();
        let proxy = zbus::blocking::Proxy::new(&caller, SERVICE, OBJECT, SERVICE).unwrap();
        let file = std::env::temp_dir().join("Faktura nr 1.pdf");
        proxy.call_method("ShowItems", &(vec![uri_of(&file)], "")).unwrap();
        proxy.call_method("ShowFolders", &(vec![uri_of(&data)], "")).unwrap();
        assert_eq!(take(&wake), vec![
            Show { folder: std::env::temp_dir(), select: vec![file.clone()], properties: false },
            Show { folder: data.clone(), select: vec![], properties: false },
        ]);
        // A second start with a folder hands it to this one once it is the default.
        let file = service_file().unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, service_text(Path::new("/opt/ferrous-commander"), None)).unwrap();
        assert!(hand_over(std::slice::from_ref(&data)));
        assert_eq!(take(&wake), vec![Show { folder: data.clone(), select: vec![], properties: false }]);
        // Another file manager keeps the name it holds.
        stop();
        let other = zbus::blocking::Connection::session().unwrap();
        other.request_name(SERVICE).unwrap();
        let taken = start();
        assert!(taken.as_ref().is_err_and(|e| e.contains("Another file manager")), "{taken:?}");
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn the_service_file_names_this_program_and_remembers_the_previous_default() {
        let text = service_text(Path::new("/home/me/My Apps/ferrous-commander"), Some("org.gnome.Nautilus.desktop"));
        assert!(text.contains("Name=org.freedesktop.FileManager1\n"));
        assert!(text.contains("Exec=\"/home/me/My Apps/ferrous-commander\"\n"));
        assert_eq!(previous_default(&text), Some("org.gnome.Nautilus.desktop"));
        assert_eq!(previous_default(&service_text(Path::new("/x/ferrous-commander"), None)), None);
    }
}
