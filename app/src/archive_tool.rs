//! RAR's decoder cannot be shipped with the app, so RAR goes to a tool the user installed.
//!
//! The tool is trusted to unpack, not to judge what it unpacks: the names are checked before
//! it starts, and everything it left is checked once it ends.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{PASSWORD_NEEDED, PASSWORD_WRONG, Sink, refused};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind { SevenZip, Unrar }

#[derive(Clone, Debug)]
pub(super) struct Tool {
    pub(super) kind: Kind,
    pub(super) path: PathBuf,
}

static TOOL: OnceLock<Option<Tool>> = OnceLock::new();

/// Looking through PATH can stall on a sleeping network drive, so it never runs on the UI thread.
pub fn detect_in_background() {
    std::thread::spawn(|| { TOOL.get_or_init(|| installed().into_iter().next()); });
}

/// Until the search ends, a RAR file simply opens in its default application.
pub fn ready() -> bool {
    TOOL.get().is_some_and(Option::is_some)
}

/// Every working tool, best first. 7-Zip takes a password on stdin on every Linux build.
pub(super) fn installed() -> Vec<Tool> {
    // A relative PATH entry would run whatever sits in the folder the app started from.
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).filter(|dir| dir.is_absolute()).collect())
        .unwrap_or_default();
    #[cfg(windows)]
    if let Some(programs) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
        dirs.push(programs.join("7-Zip"));
        dirs.push(programs.join("WinRAR"));
    }
    let mut found = Vec::new();
    for (name, kind) in [("7z", Kind::SevenZip), ("7zz", Kind::SevenZip), ("unrar", Kind::Unrar)] {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let tool = dirs.iter().map(|dir| dir.join(&file)).filter(|path| path.is_file())
            .map(|path| Tool { kind, path }).find(works);
        if let Some(tool) = tool.filter(|t| !found.iter().any(|f: &Tool| f.kind == t.kind)) { found.push(tool); }
    }
    found
}

/// Some 7-Zip packages leave RAR out, and `unrar` can name a different program entirely.
fn works(tool: &Tool) -> bool {
    let mut command = base_command(&tool.path);
    if tool.kind == Kind::SevenZip { command.arg("i"); }
    let Ok(output) = command.stdin(Stdio::null()).output() else { return false };
    let text = String::from_utf8_lossy(&output.stdout);
    text.contains(if tool.kind == Kind::SevenZip { "Rar5" } else { "Alexander Roshal" })
}

fn base_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x08000000);
    #[cfg(unix)]
    {
        command.env("LC_ALL", "C.UTF-8");
        // Without a terminal of its own, a tool started from one cannot ask there instead of
        // reading stdin and wait for an answer nobody sees.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
            });
        }
    }
    command
}

struct Finished { success: bool, code: Option<i32>, text: String }

/// On Linux a password reaches the tool on stdin, because every process can read another's
/// command line. On Windows it goes on the command line: only the same user and
/// administrators can read that there, and the tools ask the console rather than stdin.
fn run(tool: &Tool, args: Vec<OsString>, password: Option<&str>, sink: &Sink) -> Result<Finished, String> {
    let mut child = base_command(&tool.path).args(args)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("Couldn't start {}: {e}", tool.path.display()))?;
    // Closing stdin right after is what makes a tool give up instead of waiting for an answer.
    if let Some(mut input) = child.stdin.take() && cfg!(unix) && let Some(password) = password {
        let _ = writeln!(input, "{password}");
    }
    let drain = |source: Option<Box<dyn Read + Send>>| std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut source) = source { let _ = source.read_to_end(&mut text); }
        text
    });
    let stdout = drain(child.stdout.take().map(|s| Box::new(s) as _));
    let stderr = drain(child.stderr.take().map(|s| Box::new(s) as _));
    let status = loop {
        if sink.cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut text = stdout.join().unwrap_or_default();
    text.extend(stderr.join().unwrap_or_default());
    Ok(Finished { success: status.success(), code: status.code(), text: String::from_utf8_lossy(&text).into_owned() })
}

fn password_args(kind: Kind, password: Option<&str>) -> Vec<OsString> {
    match (kind, password) {
        // "Don't ask": an encrypted archive then fails at once.
        (Kind::Unrar, None) => vec!["-p-".into()],
        #[cfg(windows)]
        (Kind::SevenZip, None) => vec!["-p-".into()],
        #[cfg(windows)]
        (_, Some(password)) => vec![format!("-p{password}").into()],
        #[cfg(not(windows))]
        _ => Vec::new(),
    }
}

/// Exit code 11 is unrar's own "wrong password".
fn failure(tool: &Tool, done: &Finished, password: Option<&str>) -> String {
    let lower = done.text.to_lowercase();
    let about_password = ["enter password", "wrong password", "incorrect password", "password is incorrect"]
        .iter().any(|phrase| lower.contains(phrase));
    if about_password || (tool.kind == Kind::Unrar && done.code == Some(11)) {
        return if password.is_some() { PASSWORD_WRONG } else { PASSWORD_NEEDED }.into();
    }
    let lines = || done.text.lines().map(str::trim).filter(|line| !line.is_empty());
    lines().find(|line| line.starts_with("ERROR")).or(lines().next_back()).unwrap_or("The archive could not be unpacked").to_string()
}

/// The names as the tool reports them, one per line.
fn names(kind: Kind, listing: &str) -> Vec<String> {
    match kind {
        Kind::Unrar => listing.lines().map(str::to_string).collect(),
        // The block above the first ruler describes the archive itself.
        Kind::SevenZip => listing.lines().skip_while(|line| !line.starts_with("----------"))
            .filter_map(|line| line.strip_prefix("Path = ").map(str::to_string)).collect(),
    }
}

/// Both tools refuse such names themselves. Old unrar versions had holes here, though.
fn escaping(name: &str) -> bool {
    Path::new(&name.replace('\\', "/")).components()
        .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
}

fn plain(path: &Path) -> OsString {
    #[cfg(windows)]
    { crate::windows_fs::shown(path).into_os_string() }
    #[cfg(not(windows))]
    { path.as_os_str().to_os_string() }
}

fn with(mut first: Vec<OsString>, more: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Vec<OsString> {
    first.extend(more.into_iter().map(|arg| arg.as_ref().to_os_string()));
    first
}

pub(super) fn unpack(archive: &Path, password: Option<&str>, sink: &mut Sink) -> Result<(), String> {
    let tool = TOOL.get_or_init(|| installed().into_iter().next()).as_ref()
        .ok_or("Unpacking RAR needs 7-Zip or unrar, and neither is installed")?;
    unpack_with(tool, archive, password, sink)
}

pub(super) fn unpack_with(tool: &Tool, archive: &Path, password: Option<&str>, sink: &mut Sink) -> Result<(), String> {
    let secret = password_args(tool.kind, password);
    (sink.report)("Reading the archive".into());
    let source = plain(archive);
    let target = plain(&sink.root);
    let listing = match tool.kind {
        Kind::SevenZip => with(vec!["l".into(), "-slt".into()], secret.iter()),
        Kind::Unrar => with(vec!["lb".into()], secret.iter()),
    };
    let listed = run(tool, with(listing, [OsStr::new("--"), &source]), password, sink)?;
    if !listed.success { return Err(failure(tool, &listed, password)); }
    if let Some(bad) = names(tool.kind, &listed.text).iter().find(|name| escaping(name)) {
        return Err(refused(bad));
    }
    (sink.report)("Unpacking".into());
    let args = match tool.kind {
        Kind::SevenZip => {
            let mut into = OsString::from("-o");
            into.push(&target);
            with(with(vec!["x".into(), "-y".into(), "-bd".into(), into], secret.iter()), [OsStr::new("--"), &source])
        }
        Kind::Unrar => {
            // The separator tells unrar this is a destination, not a name to extract.
            let mut into = target.clone();
            into.push(std::path::MAIN_SEPARATOR_STR);
            with(with(vec!["x".into(), "-y".into(), "-idq".into()], secret.iter()), [OsStr::new("--"), &source, &into])
        }
    };
    let done = run(tool, args, password, sink)?;
    if !done.success { return Err(failure(tool, &done, password)); }
    sink.settle()
}
