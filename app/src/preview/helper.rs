//! The helper process that parses files for previews.
//!
//! The release build aborts on panic, and a parser can also overflow its stack, loop forever
//! or ask for all the memory. In a separate process each of those ends one preview. Its
//! replies are checked here like any other untrusted input.

use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::load::{self, Loaded, Picture, Reply};

pub const FLAG: &str = "--preview-helper";
const MAX_PATH_BYTES: usize = 256 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024 * 1024;
const MAX_SIDE_ACCEPTED: u32 = 16384;
/// The helper ends itself if one file takes this long, even with nobody left to kill it.
const SELF_LIMIT_SECONDS: u64 = 30;

const FAILED: &str = "This file could not be previewed";

fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect()
    }
}

fn path_from(bytes: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let wide: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        PathBuf::from(std::ffi::OsString::from_wide(&wide))
    }
}

fn read_u32(from: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    from.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_bytes(from: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    from.take(len as u64).read_to_end(&mut bytes)?;
    if bytes.len() != len { return Err(io::ErrorKind::UnexpectedEof.into()); }
    Ok(bytes)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

fn write_request(to: &mut impl Write, path: &Path, max_side: u32) -> io::Result<()> {
    let bytes = path_bytes(path);
    to.write_all(&max_side.to_le_bytes())?;
    to.write_all(&(bytes.len() as u32).to_le_bytes())?;
    to.write_all(&bytes)?;
    to.flush()
}

fn read_request(from: &mut impl Read) -> io::Result<(PathBuf, u32)> {
    let max_side = read_u32(from)?;
    let len = read_u32(from)? as usize;
    if len > MAX_PATH_BYTES { return Err(invalid("path too long")); }
    Ok((path_from(read_bytes(from, len)?), max_side))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Header {
    loaded: Loaded,
    size: Option<[u32; 2]>,
}

fn write_reply(to: &mut impl Write, (loaded, picture): &Reply) -> io::Result<()> {
    let header = serde_json::to_vec(&serde_json::json!({ "loaded": loaded, "size": picture.as_ref().map(|p| p.size) }))?;
    to.write_all(&(header.len() as u32).to_le_bytes())?;
    to.write_all(&header)?;
    if let Some(p) = picture { to.write_all(&p.rgba)?; }
    to.flush()
}

fn read_reply(from: &mut impl Read) -> io::Result<Reply> {
    let len = read_u32(from)? as usize;
    if len > MAX_HEADER_BYTES { return Err(invalid("header too long")); }
    let header: Header = serde_json::from_slice(&read_bytes(from, len)?)?;
    let picture = match header.size {
        Some([w, h]) => {
            if w == 0 || h == 0 || w > MAX_SIDE_ACCEPTED || h > MAX_SIDE_ACCEPTED { return Err(invalid("bad picture size")); }
            Some(Picture { size: [w, h], rgba: read_bytes(from, w as usize * h as usize * 4)? })
        }
        None => None,
    };
    Ok((header.loaded, picture))
}

/// Keeps a broken file's damage small: no huge allocation, no crash report, no orphan.
#[cfg(target_os = "linux")]
fn limit_this_process() {
    let limit = |what, value: u64| {
        let value = libc::rlimit { rlim_cur: value, rlim_max: value };
        // SAFETY: passes a valid struct; on failure the process merely stays unlimited.
        unsafe { libc::setrlimit(what, &value); }
    };
    limit(libc::RLIMIT_AS, 4 << 30);
    limit(libc::RLIMIT_CORE, 0);
    // SAFETY: takes plain integers.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong); }
}

#[cfg(not(target_os = "linux"))]
fn limit_this_process() {}

/// The body of `ferrous-commander --preview-helper`: answers until stdin closes.
pub fn serve() {
    use std::sync::atomic::{AtomicU64, Ordering};
    limit_this_process();
    static WORKING_SINCE: AtomicU64 = AtomicU64::new(0);
    let start = Instant::now();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let since = WORKING_SINCE.load(Ordering::Relaxed);
        if since != 0 && start.elapsed().as_secs() + 1 - since > SELF_LIMIT_SECONDS { std::process::exit(1); }
    });
    let (stdin, stdout) = (io::stdin(), io::stdout());
    let (mut input, mut output) = (stdin.lock(), BufWriter::with_capacity(1 << 20, stdout.lock()));
    while let Ok((path, max_side)) = read_request(&mut input) {
        WORKING_SINCE.store(start.elapsed().as_secs() + 1, Ordering::Relaxed);
        let reply = load::load(&path, max_side);
        WORKING_SINCE.store(0, Ordering::Relaxed);
        if write_reply(&mut output, &reply).is_err() { return; }
    }
}

pub struct Helper {
    child: Child,
    stdin: ChildStdin,
    replies: mpsc::Receiver<io::Result<Reply>>,
}

#[cfg_attr(test, allow(dead_code))]
impl Helper {
    /// The running program's own file: an update replacing it on disk changes nothing here.
    fn program() -> io::Result<PathBuf> {
        #[cfg(target_os = "linux")]
        return Ok(PathBuf::from("/proc/self/exe"));
        #[cfg(not(target_os = "linux"))]
        std::env::current_exe()
    }

    pub fn start() -> io::Result<Helper> {
        let mut command = Command::new(Self::program()?);
        command.arg(FLAG);
        Helper::from_command(command)
    }

    fn from_command(mut command: Command) -> io::Result<Helper> {
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x08000000);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| invalid("no stdin"))?;
        let mut stdout = child.stdout.take().ok_or_else(|| invalid("no stdout"))?;
        let (tx, replies) = mpsc::sync_channel(1);
        std::thread::spawn(move || loop {
            let reply = read_reply(&mut stdout);
            let ended = reply.is_err();
            if tx.send(reply).is_err() || ended { return; }
        });
        Ok(Helper { child, stdin, replies })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Asks the helper, starting one if needed. `None` means the job was given up for a newer
/// one. A helper that fails, or takes too long, is ended and the next job gets a new one.
pub fn ask(
    running: &mut Option<Helper>,
    start: fn() -> io::Result<Helper>,
    path: &Path,
    max_side: u32,
    limit: Duration,
    stale: &dyn Fn() -> bool,
) -> Option<Reply> {
    let failed = |message: &str| Some((Loaded::Error(message.to_string()), None));
    if running.is_none() {
        match start() {
            Ok(helper) => *running = Some(helper),
            Err(_) => return failed("The preview helper could not start"),
        }
    }
    let helper = running.as_mut()?;
    if write_request(&mut helper.stdin, path, max_side).is_err() {
        *running = None;
        return failed(FAILED);
    }
    let started = Instant::now();
    loop {
        match helper.replies.recv_timeout(Duration::from_millis(30)) {
            Ok(Ok(reply)) => return Some(reply),
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                *running = None;
                return failed(FAILED);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stale() {
                    *running = None;
                    return None;
                }
                if started.elapsed() > limit {
                    *running = None;
                    return failed("This file takes too long to preview");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(reply: &Reply) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_reply(&mut bytes, reply).unwrap();
        bytes
    }

    #[test]
    fn requests_and_replies_survive_the_pipe() {
        let path = Path::new("/tmp/zażółć file.pdf");
        let mut bytes = Vec::new();
        write_request(&mut bytes, path, 2048).unwrap();
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), (path.to_path_buf(), 2048));
        let reply: Reply = (Loaded::Image([9, 9]), Some(Picture { size: [2, 1], rgba: vec![7; 8] }));
        let (loaded, picture) = read_reply(&mut frame(&reply).as_slice()).unwrap();
        assert!(matches!(loaded, Loaded::Image([9, 9])));
        assert_eq!(picture.unwrap().rgba, vec![7; 8]);
        let text: Reply = (Loaded::Text("żółw".into(), true), None);
        assert!(matches!(read_reply(&mut frame(&text).as_slice()).unwrap(), (Loaded::Text(t, true), None) if t == "żółw"));
    }

    #[test]
    fn replies_that_lie_are_refused() {
        let reply: Reply = (Loaded::Binary, Some(Picture { size: [100000, 100000], rgba: Vec::new() }));
        assert!(read_reply(&mut frame(&reply).as_slice()).is_err(), "impossible picture size");
        let short: Reply = (Loaded::Binary, Some(Picture { size: [2, 2], rgba: vec![0; 4] }));
        assert!(read_reply(&mut frame(&short).as_slice()).is_err(), "fewer pixels than promised");
        assert!(read_reply(&mut u32::MAX.to_le_bytes().as_slice()).is_err(), "oversized header");
        let mut garbage = 3u32.to_le_bytes().to_vec();
        garbage.extend(b"{{{");
        assert!(read_reply(&mut garbage.as_slice()).is_err(), "not a header at all");
    }

    #[cfg(target_os = "linux")]
    fn shell(script: &'static str) -> io::Result<Helper> {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        Helper::from_command(command)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_helper_that_dies_costs_one_preview_and_is_replaced() {
        let mut running = None;
        let dying = || shell("exit 3");
        let never_stale = || false;
        let reply = ask(&mut running, dying, Path::new("/x"), 64, Duration::from_secs(5), &never_stale).unwrap();
        assert!(matches!(reply.0, Loaded::Error(ref m) if m == FAILED));
        assert!(running.is_none(), "the next file starts a new helper");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_stuck_helper_is_ended_at_the_limit_or_when_replaced() {
        let stuck = || shell("sleep 30");
        let never_stale = || false;
        let mut running = None;
        let reply = ask(&mut running, stuck, Path::new("/x"), 64, Duration::from_millis(50), &never_stale).unwrap();
        assert!(matches!(reply.0, Loaded::Error(ref m) if m.contains("too long")));
        assert!(running.is_none());
        let stale = || true;
        let started = Instant::now();
        assert!(ask(&mut running, stuck, Path::new("/x"), 64, Duration::from_secs(20), &stale).is_none());
        assert!(running.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
