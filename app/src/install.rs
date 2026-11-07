//! Per-user install and uninstall, no admin rights needed.
//! Windows: %LOCALAPPDATA%\Programs\Ferrous Commander, a Start-menu shortcut and an entry in
//! "Installed apps". Linux: ~/.local/bin, a .desktop launcher and an icon.

use std::path::PathBuf;

const NAME: &str = "Ferrous Commander";
const BIN: &str = "ferrous-commander";

fn icon_rgba() -> (Vec<u8>, u32) {
    let ic = crate::icons::app_icon();
    (ic.rgba, ic.width)
}

#[cfg(windows)]
fn install_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("Programs").join(NAME))
}
#[cfg(windows)]
fn shortcut() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("Microsoft\\Windows\\Start Menu\\Programs").join(format!("{NAME}.lnk")))
}
#[cfg(windows)]
const POWERSHELL: &str = "WindowsPowerShell\\v1.0\\powershell.exe";
#[cfg(windows)]
const UNINSTALL_KEY: &str = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\FerrousCommander";

#[cfg(not(windows))]
fn bin_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local/bin").join(BIN))
}
#[cfg(not(windows))]
fn desktop_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("applications").join(format!("{BIN}.desktop")))
}
#[cfg(not(windows))]
fn icon_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("icons/hicolor/64x64/apps").join(format!("{BIN}.png")))
}

pub fn installed_exe() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        Some(install_dir()?.join(format!("{BIN}.exe")))
    }
    #[cfg(not(windows))]
    {
        bin_path()
    }
}

pub fn is_installed() -> bool {
    installed_exe().is_some_and(|p| p.exists())
}

fn copy_self(to: &std::path::Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if let (Ok(a), Ok(b)) = (me.canonicalize(), to.canonicalize())
        && a == b {
            return Ok(());
        }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    std::fs::copy(&me, to).map_err(|e| format!("copy failed: {e}"))?;
    #[cfg(not(windows))]
    {
        // Replace the inode; Linux cannot overwrite a running executable.
        use std::os::unix::fs::PermissionsExt;
        let staged = to.with_extension(format!("install-{}", std::process::id()));
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&staged)
            .map_err(|e| format!("create installer staging file: {e}"))?;
        let result = (|| -> std::io::Result<()> {
            let mut input = std::fs::File::open(&me)?;
            std::io::copy(&mut input, &mut output)?;
            output.set_permissions(std::fs::Permissions::from_mode(0o755))?;
            output.sync_all()?;
            drop(output);
            std::fs::rename(&staged, to)
        })();
        if result.is_err() { let _ = std::fs::remove_file(&staged); }
        result.map_err(|e| format!("copy failed: {e}"))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn desktop_exec(path: &std::path::Path) -> Result<String, String> {
    let path = path.to_str().ok_or("Install path must be UTF-8 for the app-menu launcher")?;
    let mut quoted = String::from("\"");
    for c in path.chars() {
        // Desktop Entry string escaping follows Exec argument quoting.
        match c {
            '\\' => quoted.push_str(r"\\\\"),
            '"' | '$' | '`' => { quoted.push_str(r"\\"); quoted.push(c); },
            '%' => quoted.push_str("%%"),
            '\n' | '\r' => return Err("Install path cannot contain a line break".into()),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    // GIO checks the executable before expanding %% field escapes. Keep a
    // literal percent in an argument to env instead of in the executable token.
    if path.contains('%') { Ok(format!("/usr/bin/env {quoted}")) } else { Ok(quoted) }
}

pub fn install() -> Result<String, String> {
    let exe = installed_exe().ok_or("no home folder")?;
    copy_self(&exe)?;
    #[cfg(windows)]
    {
        let dir = install_dir().ok_or("no install folder")?;
        // The icon lives inside the executable; earlier versions kept a separate file.
        let _ = std::fs::remove_file(dir.join(format!("{BIN}.ico")));
        let lnk = shortcut().ok_or("no Start menu folder")?;
        if let Some(d) = lnk.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // Paths travel in variables: PowerShell also ends a string at typographic quotes.
        const SHORTCUT: &str = "$s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:FERROUS_LINK);$s.TargetPath=$env:FERROUS_EXE;$s.IconLocation=$env:FERROUS_EXE;$s.WorkingDirectory=$env:FERROUS_DIR;$s.Description='Fast file manager';$s.Save()";
        let mut powershell = std::process::Command::new(crate::win::system_program(POWERSHELL));
        powershell.args(["-NoProfile", "-NonInteractive", "-Command", SHORTCUT])
            .env("FERROUS_LINK", &lnk).env("FERROUS_EXE", &exe).env("FERROUS_DIR", &dir);
        finish(powershell)?;
        let ver = env!("CARGO_PKG_VERSION");
        let uninstall = format!("\"{}\" --uninstall", exe.display());
        let quiet_uninstall = format!("{uninstall} --silent");
        let entries: [(&str, &str, String); 9] = [
            ("DisplayName", "REG_SZ", NAME.to_string()),
            ("DisplayVersion", "REG_SZ", ver.to_string()),
            ("Publisher", "REG_SZ", "Ferrous Commander contributors".to_string()),
            ("DisplayIcon", "REG_SZ", exe.display().to_string()),
            ("InstallLocation", "REG_SZ", dir.display().to_string()),
            ("UninstallString", "REG_SZ", uninstall),
            ("QuietUninstallString", "REG_SZ", quiet_uninstall),
            ("NoModify", "REG_DWORD", "1".to_string()),
            ("NoRepair", "REG_DWORD", "1".to_string()),
        ];
        for (k, t, v) in entries {
            reg(&["add", UNINSTALL_KEY, "/v", k, "/t", t, "/d", &v, "/f"])?;
        }
        Ok(format!("Installed to {} with a Start menu shortcut", dir.display()))
    }
    #[cfg(not(windows))]
    {
        let command = desktop_exec(&exe)?;
        let (rgba, n) = icon_rgba();
        let icon = icon_path().ok_or("no data folder")?;
        if let Some(d) = icon.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        image::save_buffer(&icon, &rgba, n, n, image::ColorType::Rgba8).map_err(|e| e.to_string())?;
        let desk = desktop_path().ok_or("no data folder")?;
        if let Some(d) = desk.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = format!(
            "[Desktop Entry]\nType=Application\nName={NAME}\nGenericName=File Manager\nComment=Fast file manager with column navigation, tabs and two panes\nExec={} %F\nIcon={BIN}\nTerminal=false\nCategories=System;FileTools;FileManager;Utility;\nMimeType=inode/directory;\nStartupWMClass={BIN}\n",
            command
        );
        std::fs::write(&desk, text).map_err(|e| e.to_string())?;
        let _ = std::process::Command::new("update-desktop-database").arg(desk.parent().unwrap()).status();
        Ok(format!("Installed to {} with an app-menu launcher", exe.display()))
    }
}

pub fn uninstall() -> Result<String, String> {
    #[cfg(windows)]
    {
        let dir = install_dir().ok_or("no install folder")?;
        let exe = dir.join(format!("{BIN}.exe"));
        if other_instance_running(&exe) {
            return Err("Close all other Ferrous Commander windows, then uninstall again.".into());
        }
        // Setup.exe installs into the same folder and owns its own "Installed apps" entry.
        let setup_uninstaller = dir.join("unins000.exe");
        if setup_uninstaller.exists() {
            std::process::Command::new(&setup_uninstaller)
                .current_dir(std::env::temp_dir())
                .spawn()
                .map_err(|e| format!("Could not start {}: {e}", setup_uninstaller.display()))?;
            return Ok("Windows uninstaller started.".into());
        }
        if let Some(l) = shortcut() {
            match std::fs::remove_file(&l) {
                Ok(()) => {},
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(e) => return Err(format!("Could not remove {}: {e}", l.display())),
            }
        }
        if reg(&["query", UNINSTALL_KEY]).is_ok() {
            reg(&["delete", UNINSTALL_KEY, "/f"])?;
        }
        remove_folder_when_free(&dir).map_err(|e| format!("Could not remove {}: {e}", dir.display()))?;
        Ok("Uninstalled. Settings are kept in your profile.".into())
    }
    #[cfg(not(windows))]
    {
        for p in [desktop_path(), icon_path(), bin_path()].into_iter().flatten() {
            match std::fs::remove_file(&p) {
                Ok(()) => {},
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                Err(e) => return Err(format!("Could not remove {}: {e}", p.display())),
            }
        }
        if let Some(desk) = desktop_path() {
            let _ = std::process::Command::new("update-desktop-database").arg(desk.parent().unwrap()).status();
        }
        Ok("Uninstalled. Settings are kept in your configuration folder (ferrous).".into())
    }
}

/// True when the app must close for uninstall to finish: Windows cannot delete a running program.
pub fn must_exit_after_uninstall() -> bool {
    #[cfg(windows)]
    {
        let me = std::env::current_exe().and_then(|p| p.canonicalize());
        let dir = install_dir().and_then(|d| d.canonicalize().ok());
        matches!((me, dir), (Ok(me), Some(dir)) if me.starts_with(&dir))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Windows has no console for a GUI program, so a result would otherwise be invisible.
#[cfg(windows)]
pub fn show_message(text: &str, is_error: bool) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(owner: *mut core::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    const MB_ICONERROR: u32 = 0x10;
    const MB_ICONINFORMATION: u32 = 0x40;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, caption) = (wide(text), wide(NAME));
    let kind = if is_error { MB_ICONERROR } else { MB_ICONINFORMATION };
    // SAFETY: both strings are NUL-terminated and outlive the blocking call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), kind) };
}

#[cfg(windows)]
fn other_instance_running(exe: &std::path::Path) -> bool {
    use std::os::windows::process::CommandExt;
    let script = format!(
        "@(Get-Process -Name {BIN} -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq $env:FERROUS_INSTALLED_EXE -and $_.Id -ne {} }}).Count",
        std::process::id()
    );
    let output = std::process::Command::new(crate::win::system_program(POWERSHELL))
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("FERROUS_INSTALLED_EXE", exe)
        .creation_flags(0x0800_0000)
        .output();
    // A failed check must not block uninstalling; the removal below still retries.
    output.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().is_ok_and(|n| n > 0))
}

/// Remove `dir` in a background process, retrying for about a minute until files are released.
#[cfg(windows)]
fn remove_folder_when_free(dir: &std::path::Path) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    // cmd does not understand the \" escaping of Command::arg, so the line must be passed raw. The
    // path travels in a variable, which keeps characters such as & and % out of the script.
    const SCRIPT: &str = "for /L %i in (1,1,60) do @if exist \"%FERROUS_REMOVE_DIR%\" (rmdir /s /q \"%FERROUS_REMOVE_DIR%\" 2>nul & if exist \"%FERROUS_REMOVE_DIR%\" ping -n 2 127.0.0.1 >nul)";
    std::process::Command::new(crate::win::system_program("cmd.exe"))
        .arg("/C")
        .raw_arg(SCRIPT)
        .env("FERROUS_REMOVE_DIR", dir)
        // A working directory inside `dir` would keep the folder itself from being deleted.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
}

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<(), String> {
    let mut command = std::process::Command::new(crate::win::system_program("reg.exe"));
    command.args(args);
    finish(command)
}

#[cfg(windows)]
fn finish(mut command: std::process::Command) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let out = command.creation_flags(0x0800_0000).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Save the app icon (PNG, or ICO when the name ends in .ico) for packaging.
pub fn export_icon(to: &std::path::Path) -> Result<(), String> {
    let (rgba, n) = icon_rgba();
    image::save_buffer(to, &rgba, n, n, image::ColorType::Rgba8).map_err(|e| e.to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::remove_folder_when_free;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn folder_removal_handles_special_characters_and_waits_for_locked_files() {
        let root = std::env::temp_dir().join(format!("ferrous-install-test-{}", std::process::id()));
        let dir = root.join("Ferrous & Commander 100%");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ferrous-commander.exe");
        std::fs::write(&file, b"test").unwrap();
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(&file).unwrap();

        let mut removal = remove_folder_when_free(&dir).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(file.exists(), "a locked file must survive until it is released");
        drop(lock);
        removal.wait().unwrap();

        assert!(!dir.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
