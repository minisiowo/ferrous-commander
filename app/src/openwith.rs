//! "Open with": Windows lists system association handlers; Linux lists installed apps that
//! declare the file's type in their .desktop launcher.

use std::path::Path;
#[cfg(windows)]
#[path = "openwith_windows.rs"]
mod windows;
#[cfg(not(windows))]
#[path = "openwith_linux.rs"]
mod linux;
#[cfg(not(windows))]
pub use linux::{Chooser, file_type};

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppEntry {
    pub name: String,
    pub exec: String,
}

#[cfg(windows)]
pub fn run_handler_helper() -> bool {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|a| a == "--choose-open-with-app") {
        let result = args.get(2).ok_or_else(|| "Missing file".to_string())
            .and_then(|p| windows::choose_app(Path::new(p)));
        if let Err(e) = result { eprintln!("{e}"); std::process::exit(1); }
        return true;
    }
    if args.get(1).is_some_and(|a| a == "--list-open-with-handlers") {
        let apps = args.get(2).map(|p| windows::apps_for(Path::new(p))).unwrap_or_default();
        println!("{}", serde_json::to_string(&apps).unwrap_or_else(|_| "[]".into()));
        return true;
    }
    if args.get(1).is_none_or(|a| a != "--open-with-handler") { return false; }
    let result = if args.len() == 4 {
        windows::launch(Path::new(&args[2]), &AppEntry {
            name: "selected application".into(), exec: args[3].to_string_lossy().into_owned(),
        })
    } else { Err("Missing file or application handler".into()) };
    if let Err(e) = result { eprintln!("{e}"); std::process::exit(1); }
    true
}

#[cfg(windows)]
fn helper_output(path: &Path, app: Option<&AppEntry>) -> Result<std::process::Output, String> {
    use std::{os::windows::process::CommandExt, process::{Command, Stdio}, time::{Duration, Instant}};
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.arg(if app.is_some() { "--open-with-handler" } else { "--list-open-with-handlers" }).arg(path);
    if let Some(app) = app { command.arg(&app.exec); }
    // Launched applications must not inherit capture pipes and hold them open.
    command.creation_flags(0x08000000).stdin(Stdio::null()).stderr(Stdio::null())
        .stdout(if app.is_some() { Stdio::null() } else { Stdio::piped() });
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) if started.elapsed() < Duration::from_secs(30) => std::thread::sleep(Duration::from_millis(25)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(e) => e.to_string(),
                    _ => "The application handler timed out. Try Choose another app.".into(),
                });
            }
        }
    }
}

/// Applications registered for this file type.
pub fn apps_for(path: &Path) -> Vec<AppEntry> {
    #[cfg(windows)]
    {
        let output = helper_output(path, None).ok();
        output.filter(|o| o.status.success())
            .and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        linux::apps_for(path)
    }
}

/// Open `path` with `app`; with no app, show the system's app chooser (Windows).
pub fn launch(path: &Path, app: Option<&AppEntry>) -> Result<(), String> {
    match app {
        #[cfg(windows)]
        Some(a) => {
            // Shell extensions run outside the file manager: a faulty handler cannot
            // take down the UI, and the helper owns a fresh STA COM apartment.
            if !path.is_file() { return Err("The selected file no longer exists".into()); }
            let output = helper_output(path, Some(a))?;
            if output.status.success() { Ok(()) } else {
                Err("The application handler failed or is no longer available. Try Choose another app.".into())
            }
        },
        #[cfg(not(windows))]
        Some(a) => {
            linux::launch(path, a)
        }
        None => {
            #[cfg(windows)]
            {
                use std::{os::windows::process::CommandExt, process::{Command, Stdio}};
                if !path.is_file() { return Err("The selected file no longer exists".into()); }
                // A separate STA owns the native chooser. Unlike handler discovery,
                // this interactive operation must not expire while the user chooses.
                let status = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                    .arg("--choose-open-with-app").arg(path).creation_flags(0x08000000)
                    .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                    .status().map_err(|e| format!("Could not show the application chooser: {e}"))?;
                if status.success() { Ok(()) } else {
                    Err("Windows could not show the application chooser for this file.".into())
                }
            }
            #[cfg(not(windows))]
            {
                Err("Choose an application first".into())
            }
        }
    }
}

