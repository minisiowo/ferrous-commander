//! Freedesktop application discovery. Keep desktop files as launch identities:
//! GIO handles Exec quoting, field codes, Terminal and D-Bus activation for us.
use std::{collections::HashSet, path::{Path, PathBuf}, sync::mpsc};
use super::AppEntry;

pub struct Chooser {
    pub path: PathBuf,
    pub query: String,
    pub apps: Vec<AppEntry>,
    pub loading: bool,
    pub focus: bool,
    receiver: mpsc::Receiver<Vec<AppEntry>>,
}

impl Chooser {
    pub fn new(path: PathBuf, ctx: egui::Context) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(installed_apps());
            ctx.request_repaint();
        });
        Self { path, query: String::new(), apps: Vec::new(), loading: true, focus: true, receiver }
    }

    pub fn poll(&mut self) {
        match self.receiver.try_recv() {
            Ok(apps) => { self.apps = apps; self.loading = false; }
            Err(mpsc::TryRecvError::Disconnected) => self.loading = false,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

fn application_dirs() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("applications"));
        roots.push(data.join("flatpak/exports/share/applications"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    roots.extend(std::env::split_paths(&data_dirs).filter(|p| p.is_absolute()).map(|p| p.join("applications")));
    roots.extend(["/var/lib/flatpak/exports/share/applications", "/var/lib/snapd/desktop/applications"].map(PathBuf::from));
    roots
}

fn executable_exists(value: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| std::fs::metadata(path)
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if value.contains('/') { return executable(Path::new(value)); }
    std::env::var_os("PATH").is_some_and(|paths|
        std::env::split_paths(&paths).any(|dir| executable(&dir.join(value))))
}

fn parse_desktop(path: PathBuf, text: &str) -> Option<(AppEntry, Vec<String>)> {
    let mut fields = std::collections::HashMap::new();
    let mut in_entry = false;
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if line.starts_with('[') { in_entry = line == "[Desktop Entry]"; continue; }
        if in_entry {
            if let Some((key, value)) = line.split_once('=') { fields.insert(key, value); }
        }
    }
    if fields.get("Type") != Some(&"Application")
        || fields.get("Hidden") == Some(&"true") || fields.get("NoDisplay") == Some(&"true")
        || fields.get("TryExec").is_some_and(|v| !executable_exists(v))
        || (fields.get("Exec").is_none_or(|v| v.is_empty()) && fields.get("DBusActivatable") != Some(&"true")) {
        return None;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktops: Vec<_> = desktop.split(':').filter(|s| !s.is_empty()).collect();
    let matches = |list: &str| list.split(';').any(|d| desktops.contains(&d));
    if fields.get("OnlyShowIn").is_some_and(|v| !matches(v))
        || fields.get("NotShowIn").is_some_and(|v| matches(v)) { return None; }
    let mut name = *fields.get("Name")?;
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"].iter()
        .find_map(|key| std::env::var(key).ok().filter(|s| !s.is_empty())).unwrap_or_default();
    let locale = locale.split('.').next().unwrap_or("");
    for variant in [locale, locale.split('_').next().unwrap_or("")] {
        if let Some(value) = fields.get(format!("Name[{variant}]").as_str()) { name = value; break; }
    }
    if name.is_empty() { return None; }
    let mimes = fields.get("MimeType").unwrap_or(&"").split(';')
        .filter(|s| !s.is_empty()).map(String::from).collect();
    Some((AppEntry { name: name.into(), exec: path.to_string_lossy().into_owned() }, mimes))
}

fn discover(roots: &[PathBuf]) -> Vec<(AppEntry, Vec<String>)> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for root in roots {
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if entry.file_type().is_ok_and(|t| t.is_dir()) { pending.push(path); continue; }
                if path.extension().is_none_or(|e| e != "desktop") { continue; }
                let id = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('/', "-");
                // User overrides, including Hidden=true tombstones, shadow system launchers.
                if !seen.insert(id) { continue; }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Some(app) = parse_desktop(path, &text) { apps.push(app); }
                }
            }
        }
    }
    apps.sort_by(|(a, _), (b, _)| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

pub fn installed_apps() -> Vec<AppEntry> {
    discover(&application_dirs()).into_iter().map(|(app, _)| app).collect()
}

/// The type `xdg-open` acts on, or None if it cannot be told in time: a stalled share
/// must not freeze the window.
pub fn file_type(path: &Path) -> Option<String> {
    let output = std::process::Command::new("timeout").args(["2s", "xdg-mime", "query", "filetype"]).arg(path).output().ok()?;
    if !output.status.success() { return None; }
    let mime = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!mime.is_empty()).then_some(mime)
}

pub fn apps_for(path: &Path) -> Vec<AppEntry> {
    let Some(mime) = file_type(path) else { return Vec::new() };
    discover(&application_dirs()).into_iter().filter(|(_, types)| types.contains(&mime))
        .map(|(app, _)| app).take(24).collect()
}

pub fn launch(path: &Path, app: &AppEntry) -> Result<(), String> {
    if !path.is_file() { return Err("The selected file no longer exists".into()); }
    let desktop = Path::new(&app.exec);
    if !desktop.is_absolute() || desktop.extension().is_none_or(|e| e != "desktop") || !desktop.is_file() {
        return Err("The application is no longer installed. Choose another app.".into());
    }
    let status = std::process::Command::new("gio").arg("launch").arg(desktop).arg(path)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map_err(|e| format!("Could not launch application (GIO is required): {e}"))?;
    if status.success() { Ok(()) } else {
        Err(format!("Could not launch {}. Choose another app or check its installation.", app.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gio_launch_preserves_special_file_names() {
        use std::os::unix::fs::PermissionsExt;
        // GIO is a runtime dependency; allow pure unit tests on minimal builders.
        if std::process::Command::new("gio").arg("version").output().is_err() { return; }
        let root = std::env::temp_dir().join(format!("ferrous gio test {}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("capture args");
        let result = root.join("result");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\" > \"$(dirname \"$0\")/result\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let desktop = root.join("capture.desktop");
        std::fs::write(&desktop, format!("[Desktop Entry]\nType=Application\nName=Capture\nExec=\"{}\" %f\nTerminal=false\n", script.display())).unwrap();
        let file = root.join("zażółć ' $() ; file.txt");
        std::fs::write(&file, "test").unwrap();
        launch(&file, &AppEntry { name: "Capture".into(), exec: desktop.to_string_lossy().into_owned() }).unwrap();
        for _ in 0..100 {
            if std::fs::read_to_string(&result).is_ok_and(|v| v == file.to_string_lossy()) { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(result).unwrap(), file.to_string_lossy());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn user_overrides_nested_launchers_and_hidden_tombstones() {
        let base = std::env::temp_dir().join(format!("ferrous-desktop-test-{}", std::process::id()));
        let user = base.join("user");
        let system = base.join("system");
        std::fs::create_dir_all(user.join("vendor")).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        let desktop = "[Desktop Entry]\nType=Application\nName=Editor\nExec=editor %F\nMimeType=text/plain;\n";
        std::fs::write(user.join("vendor/editor.desktop"), desktop).unwrap();
        std::fs::write(system.join("vendor-editor.desktop"), desktop.replace("Editor", "Old editor")).unwrap();
        std::fs::write(user.join("hidden.desktop"), "[Desktop Entry]\nHidden=true\n").unwrap();
        std::fs::write(system.join("hidden.desktop"), desktop).unwrap();
        std::fs::write(system.join("missing.desktop"), format!("{desktop}TryExec=/no/such/ferrous-test-app\n")).unwrap();
        let apps = discover(&[user.clone(), system]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].0.exec, user.join("vendor/editor.desktop").to_string_lossy());
        assert_eq!(apps[0].1, vec!["text/plain"]);
        std::fs::remove_dir_all(base).unwrap();
    }
}
