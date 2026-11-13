//! Starting what the user opens: files the system runs, and a terminal in a folder.

use std::path::Path;

use crate::fsx;

/// Asking only here keeps the question meaningful: one shown for every local program
/// would soon be dismissed without being read. "Open with" needs no question, because
/// choosing an application shows the file is meant to be read, not run.
pub fn run_warning(p: &Path) -> Option<&'static str> {
    let place = crate::drives::foreign_place(p)?;
    might_run(p).then_some(place)
}

/// The system goes by a link's target and by contents, so the name shown is not enough:
/// `notes.txt` can be a link to a program. A type that cannot be told counts as a program.
fn might_run(p: &Path) -> bool {
    let target = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if fsx::is_runnable(&fsx::display_name(p)) || fsx::is_runnable(&fsx::display_name(&target)) { return true; }
    #[cfg(not(windows))]
    return crate::openwith::file_type(p).is_none_or(|mime| fsx::is_runnable_type(&mime));
    #[cfg(windows)]
    false
}

/// The name a link leads to, for showing what would really start.
pub fn link_target(p: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(p).ok()?.file_type().is_symlink() { return None; }
    Some(fsx::display_name(&std::fs::canonicalize(p).ok()?))
}

pub fn open_terminal(dir: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // config CommandShell = CMD
        std::process::Command::new(crate::win::system_program("cmd.exe"))
            .arg("/K")
            .current_dir(dir)
            .creation_flags(0x0000_0010)
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut last = std::io::Error::other("no terminal found");
        for (terminal, flag) in terminal_choices(default_terminal_is_ghostty()) {
            match terminal_command(terminal, flag, dir).spawn() {
                Ok(_) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

#[cfg(not(windows))]
const GHOSTTY: (&str, &str) = ("ghostty", "--working-directory");

#[cfg(not(windows))]
fn default_terminal_is_ghostty() -> bool {
    std::process::Command::new("xdg-terminal-exec").arg("--print-id").stderr(std::process::Stdio::null()).output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim().starts_with("com.mitchellh.ghostty"))
}

/// Terminals to try, in order. `xdg-terminal-exec` starts Ghostty through its desktop entry,
/// which forces single-instance mode, so a Ghostty that is the default is started directly.
#[cfg(not(windows))]
fn terminal_choices(ghostty_first: bool) -> impl Iterator<Item = (&'static str, &'static str)> {
    ghostty_first.then_some(GHOSTTY).into_iter().chain([
        ("xdg-terminal-exec", "--dir"),
        ("x-terminal-emulator", ""),
        GHOSTTY,
        ("foot", "--working-directory"),
        ("gnome-terminal", "--working-directory"),
        ("konsole", "--workdir"),
        ("xfce4-terminal", "--working-directory"),
        ("kitty", "--directory"),
        ("alacritty", "--working-directory"),
        ("xterm", ""),
    ])
}

#[cfg(not(windows))]
fn terminal_command(terminal: &str, flag: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(terminal);
    command.current_dir(dir);
    if terminal == GHOSTTY.0 {
        // A running single instance opens the window in the folder it was started from.
        command.arg("--gtk-single-instance=false");
    }
    if !flag.is_empty() {
        let mut argument = std::ffi::OsString::from(flag);
        argument.push("=");
        argument.push(dir.as_os_str());
        command.arg(argument);
    }
    command
}

#[cfg(all(test, target_os = "linux"))]
mod terminal_tests {
    use super::*;

    #[test]
    fn ghostty_as_the_default_is_started_directly_and_on_its_own() {
        assert_eq!(terminal_choices(true).next(), Some(GHOSTTY));
        assert_eq!(terminal_choices(false).next().map(|t| t.0), Some("xdg-terminal-exec"));
        let dir = Path::new("/some/folder with space");
        let command = terminal_command(GHOSTTY.0, GHOSTTY.1, dir);
        assert_eq!(command.get_current_dir(), Some(dir));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--gtk-single-instance=false", "--working-directory=/some/folder with space"]);
    }

    #[test]
    fn other_terminals_only_get_the_folder() {
        let args: Vec<_> = terminal_command("foot", "--working-directory", Path::new("/x")).get_args().map(|a| a.to_owned()).collect();
        assert_eq!(args, ["--working-directory=/x"]);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod run_tests {
    use super::*;

    #[test]
    fn a_harmless_name_does_not_hide_a_program() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("fc_might_run_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (jar, text) = (root.join("payload.jar"), root.join("plain.txt"));
        std::fs::write(&jar, "stub").unwrap();
        std::fs::write(&text, "words").unwrap();
        symlink(&jar, root.join("notes.txt")).unwrap();
        symlink(&text, root.join("letter.txt")).unwrap();
        std::fs::copy("/usr/bin/true", root.join("photo.jpg")).unwrap();
        let told = crate::openwith::file_type(&text).is_some();
        assert!(might_run(&jar));
        assert!(might_run(&root.join("notes.txt")), "a link is judged by where it leads");
        assert_eq!(link_target(&root.join("notes.txt")).as_deref(), Some("payload.jar"));
        assert_eq!(link_target(&text), None);
        if told {
            assert!(might_run(&root.join("photo.jpg")), "contents decide, not the extension");
            assert!(!might_run(&text));
            assert!(!might_run(&root.join("letter.txt")));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
