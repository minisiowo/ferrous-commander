//! The helper process that parses files for previews.
//!
//! The release build aborts on panic, and a parser can also overflow its stack, loop forever
//! or ask for all the memory. In a separate process each of those ends one preview. Its
//! replies are checked here like any other untrusted input.

use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use super::load::{self, Loaded, ModelView, Options, Picture, Reply};

pub const FLAG: &str = "--preview-helper";
const MAX_PATH_BYTES: usize = 256 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024 * 1024;
const MAX_SIDE_ACCEPTED: u32 = 16384;
const MAX_FRAMES_ACCEPTED: usize = 1024;
const MAX_PIXEL_BYTES: usize = 1 << 31;
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
    // Whole at once, not doubled as it arrives. Untouched pages cost nothing if the sender lied.
    bytes.try_reserve_exact(len).map_err(|_| invalid("too large"))?;
    from.take(len as u64).read_to_end(&mut bytes)?;
    if bytes.len() != len { return Err(io::ErrorKind::UnexpectedEof.into()); }
    Ok(bytes)
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

fn write_request(to: &mut impl Write, path: &Path, options: Options) -> io::Result<()> {
    let bytes = path_bytes(path);
    to.write_all(&options.max_side.to_le_bytes())?;
    to.write_all(&options.page.to_le_bytes())?;
    let [l, t, r, b] = options.crop.unwrap_or([0; 4]);
    for v in [l, t, r, b] { to.write_all(&v.to_le_bytes())?; }
    to.write_all(&[options.view.map_or(0, |view| 1 + view.fine as u8)])?;
    let view = options.view.unwrap_or(ModelView { turn: 0, tilt: 0, zoom: 0, pan: [0; 2], size: [0; 2], fit: 0, fine: false });
    for v in [view.turn as u16, view.tilt as u16, view.zoom, view.pan[0] as u16, view.pan[1] as u16, view.size[0], view.size[1], view.fit] { to.write_all(&v.to_le_bytes())?; }
    to.write_all(&(bytes.len() as u32).to_le_bytes())?;
    to.write_all(&bytes)?;
    to.flush()
}

fn read_request(from: &mut impl Read) -> io::Result<(PathBuf, Options)> {
    let (max_side, page) = (read_u32(from)?, read_u32(from)?);
    let mut raw = [0; 8];
    from.read_exact(&mut raw)?;
    let crop: [u16; 4] = std::array::from_fn(|i| u16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]]));
    let mut raw_view = [0; 17];
    from.read_exact(&mut raw_view)?;
    let v: [u16; 8] = std::array::from_fn(|i| u16::from_le_bytes([raw_view[1 + i * 2], raw_view[2 + i * 2]]));
    let view = (raw_view[0] != 0).then_some(ModelView { turn: v[0] as i16, tilt: v[1] as i16, zoom: v[2], pan: [v[3] as i16, v[4] as i16], size: [v[5], v[6]], fit: v[7], fine: raw_view[0] == 2 });
    let len = read_u32(from)? as usize;
    if len > MAX_PATH_BYTES { return Err(invalid("path too long")); }
    let options = Options { max_side, page, crop: (crop != [0; 4]).then_some(crop), view };
    Ok((path_from(read_bytes(from, len)?), options))
}

/// Written with borrowed parts, read into owned ones.
#[derive(serde::Serialize, serde::Deserialize)]
struct Header<L, D> {
    loaded: L,
    size: Option<[u32; 2]>,
    #[serde(default)]
    delays: D,
    /// Microseconds in the helper: all of it, turning the model, filling it in.
    #[serde(default)]
    times: Option<[u32; 3]>,
}

/// What the last reply said about its own timing. Only one job is out at a time.
static LAST_TIMES: Mutex<Option<[u32; 3]>> = Mutex::new(None);

pub fn take_times() -> Option<[u32; 3]> {
    LAST_TIMES.lock().unwrap().take()
}

fn write_reply(to: &mut impl Write, (loaded, picture): &Reply, times: Option<[u32; 3]>) -> io::Result<()> {
    let header = serde_json::to_vec(&Header::<&Loaded, &[u32]> {
        loaded,
        size: picture.as_ref().map(|p| p.size),
        delays: picture.as_ref().map_or(&[], |p| &p.delays),
        times,
    })?;
    to.write_all(&(header.len() as u32).to_le_bytes())?;
    to.write_all(&header)?;
    if let Some(p) = picture { to.write_all(&p.rgba)?; }
    to.flush()
}

fn read_reply(from: &mut impl Read) -> io::Result<Reply> {
    let len = read_u32(from)? as usize;
    if len > MAX_HEADER_BYTES { return Err(invalid("header too long")); }
    let header: Header<Loaded, Vec<u32>> = serde_json::from_slice(&read_bytes(from, len)?)?;
    *LAST_TIMES.lock().unwrap() = header.times;
    let picture = match header.size {
        Some([w, h]) => {
            if w == 0 || h == 0 || w > MAX_SIDE_ACCEPTED || h > MAX_SIDE_ACCEPTED { return Err(invalid("bad picture size")); }
            if header.delays.len() > MAX_FRAMES_ACCEPTED { return Err(invalid("too many frames")); }
            let bytes = w as usize * h as usize * 4 * header.delays.len().max(1);
            if bytes > MAX_PIXEL_BYTES { return Err(invalid("too many pixels")); }
            Some(Picture { size: [w, h], delays: header.delays, rgba: read_bytes(from, bytes)? })
        }
        None => None,
    };
    Ok((header.loaded, picture))
}

/// Keeps a broken file's damage small: no huge allocation, no crash report, no orphan.
#[cfg(target_os = "linux")]
pub(super) fn limit_this_process() {
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
pub(super) fn limit_this_process() {}

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
    while let Ok((path, options)) = read_request(&mut input) {
        WORKING_SINCE.store(start.elapsed().as_secs() + 1, Ordering::Relaxed);
        let working = Instant::now();
        let reply = load::load(&path, options);
        let times = super::model::take_paint_times().map(|[turning, filling]| [working.elapsed().as_micros() as u32, turning, filling]);
        WORKING_SINCE.store(0, Ordering::Relaxed);
        if write_reply(&mut output, &reply, times).is_err() { return; }
        if options.view.is_some() && let (_, Some(picture)) = reply { super::model::recycle(picture.rgba); }
    }
}

pub struct Helper {
    child: Option<Child>,
    stdin: ChildStdin,
    replies: mpsc::Receiver<io::Result<Reply>>,
}

#[cfg_attr(test, allow(dead_code))]
impl Helper {
    /// The running program's own file: an update replacing it on disk changes nothing here.
    pub(super) fn program() -> io::Result<PathBuf> {
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
        Ok(Helper { child: Some(child), stdin, replies })
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let _ = child.kill();
        // A process stuck on a dead network share dies only when the share answers,
        // and the lane must not wait for that.
        std::thread::spawn(move || { let _ = child.wait(); });
    }
}

/// Asks the helper, starting one if needed. `None` means the job was given up for a newer
/// one. A helper that fails, or takes too long, is ended and the next job gets a new one.
pub fn ask(
    running: &mut Option<Helper>,
    start: fn() -> io::Result<Helper>,
    path: &Path,
    options: Options,
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
    if write_request(&mut helper.stdin, path, options).is_err() {
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

    const ANY: Options = Options { max_side: 64, page: 0, crop: None, view: None };

    fn frame(reply: &Reply) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_reply(&mut bytes, reply, None).unwrap();
        bytes
    }

    /// A wavy ball, so that the triangles differ in size and some of them hide others.
    fn write_ball(path: &Path, wanted: usize) -> usize {
        use std::f32::consts::{PI, TAU};
        let columns = ((wanted as f64).sqrt() as usize).max(4);
        let rows = (wanted / (2 * columns)).max(2);
        let at = |row: usize, column: usize| {
            let (a, b) = (PI * row as f32 / rows as f32, TAU * column as f32 / columns as f32);
            let reach = 40.0 * (1.0 + 0.08 * (7.0 * a).sin() * (11.0 * b).cos());
            [reach * a.sin() * b.cos(), reach * a.sin() * b.sin(), reach * a.cos()]
        };
        let mut file = BufWriter::with_capacity(1 << 20, std::fs::File::create(path).unwrap());
        file.write_all(&[0; 80]).unwrap();
        file.write_all(&((2 * rows * columns) as u32).to_le_bytes()).unwrap();
        for row in 0..rows {
            for column in 0..columns {
                for triangle in [[at(row, column), at(row + 1, column), at(row, column + 1)], [at(row + 1, column), at(row + 1, column + 1), at(row, column + 1)]] {
                    file.write_all(&[0; 12]).unwrap();
                    for number in triangle.iter().flatten() { file.write_all(&number.to_le_bytes()).unwrap(); }
                    file.write_all(&[0; 2]).unwrap();
                }
            }
        }
        2 * rows * columns
    }

    fn middle_and_p95(mut values: Vec<f64>) -> (f64, f64) {
        values.sort_by(f64::total_cmp);
        (values[values.len() / 2], values[(values.len() * 95 / 100).min(values.len() - 1)])
    }

    #[cfg(windows)]
    fn memory_of(child: u32) -> u64 {
        let script = format!("(Get-Process -Id {child}).PrivateMemorySize64");
        let output = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().parse().unwrap()
    }

    #[cfg(not(windows))]
    fn memory_of(child: u32) -> u64 {
        let status = std::fs::read_to_string(format!("/proc/{child}/status")).unwrap();
        let line = status.lines().find(|line| line.starts_with("VmRSS:")).unwrap();
        line.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() * 1024
    }

    /// Many looks at one model must not make the helper grow. Same set-up as the baseline below,
    /// with `FERROUS_LOOK_TRIANGLES` (one number) and `FERROUS_LOOK_ROUNDS`.
    #[test]
    #[ignore = "needs the release program, named by FERROUS_HELPER_EXE"]
    fn model_look_endurance() {
        let program = std::env::var_os("FERROUS_HELPER_EXE").expect("FERROUS_HELPER_EXE");
        let number = |name, default| std::env::var(name).map_or(default, |n: String| n.parse().unwrap());
        let (wanted, rounds): (usize, i16) = (number("FERROUS_LOOK_TRIANGLES", 1_000_000), number("FERROUS_LOOK_ROUNDS", 1500) as i16);
        let root = load::scratch("endurance");
        let file = root.join("ball.stl");
        let triangles = write_ball(&file, wanted);
        let mut command = Command::new(&program);
        command.arg(FLAG);
        let mut running = Some(Helper::from_command(command).unwrap());
        let child = running.as_ref().unwrap().child.as_ref().unwrap().id();
        let no_second_helper = || Err(io::ErrorKind::NotFound.into());
        let never_stale = || false;
        let mut ask_for = |view| ask(&mut running, no_second_helper, &file, Options { max_side: 2048, view, ..ANY }, Duration::from_secs(120), &never_stale).unwrap();
        assert!(matches!(ask_for(None).0, Loaded::Model(_)));
        let mut seen = Vec::new();
        for look in 0..rounds {
            let view = ModelView { turn: look.wrapping_mul(30), tilt: look % 300, zoom: 100 + (look % 50) as u16 * 20, pan: [0; 2], size: [1600, 900], fit: 850, fine: look % 20 == 0 };
            assert!(ask_for(Some(view)).1.is_some(), "look {look}");
            if look + 1 == rounds / 8 || look + 1 == rounds { seen.push(memory_of(child)); }
        }
        println!("{triangles} triangles, {rounds} looks: helper {} MB after the first eighth, {} MB at the end", seen[0] >> 20, seen[1] >> 20);
        assert!(seen[1] < seen[0] + (64 << 20), "the helper keeps growing");
        drop(ask_for);
        drop(running);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Looks at the same models on any machine, through the real helper and its pipe:
    /// `cargo build --release`, then with `FERROUS_HELPER_EXE=target/release/ferrous-commander`
    /// `cargo test --release model_look_baseline -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the release program, named by FERROUS_HELPER_EXE"]
    fn model_look_baseline() {
        let program = std::env::var_os("FERROUS_HELPER_EXE").expect("FERROUS_HELPER_EXE");
        let sizes: Vec<usize> = std::env::var("FERROUS_LOOK_TRIANGLES").map_or(vec![100_000, 1_000_000, 4_000_000], |list| list.split(',').map(|n| n.parse().unwrap()).collect());
        let root = load::scratch("look");
        let no_second_helper = || Err(io::ErrorKind::NotFound.into());
        let never_stale = || false;
        for wanted in sizes {
            let file = root.join(format!("ball_{wanted}.stl"));
            let triangles = write_ball(&file, wanted);
            let mut command = Command::new(&program);
            command.arg(FLAG);
            let mut running = Some(Helper::from_command(command).unwrap());
            let started = Instant::now();
            let first = ask(&mut running, no_second_helper, &file, Options { max_side: 2048, ..ANY }, Duration::from_secs(120), &never_stale).unwrap();
            assert!(matches!(first.0, Loaded::Model(_)), "{:?}", first.0);
            println!("{triangles} triangles: first picture {:.0} ms", started.elapsed().as_secs_f64() * 1000.0);
            for (name, zoom, fine, looks, size_asked, fit) in [("moving 100%", 100, false, 60, [1600, 900], 850), ("moving 400%", 400, false, 60, [1600, 900], 850), ("sharp 100%", 100, true, 10, [1600, 900], 850), ("moving win", 100, false, 60, [1076, 1076], 1050), ("sharp win", 100, true, 10, [1076, 1076], 1050)] {
                let (mut round, mut helper, mut turning, mut filling, mut pipe) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
                let mut size = [0, 0];
                for look in 0..looks {
                    let view = ModelView { turn: look * 30, tilt: (look % 20) * 15, zoom, pan: [0; 2], size: size_asked, fit, fine };
                    let started = Instant::now();
                    let reply = ask(&mut running, no_second_helper, &file, Options { max_side: 2048, view: Some(view), ..ANY }, Duration::from_secs(120), &never_stale).unwrap();
                    let took = started.elapsed().as_secs_f64() * 1000.0;
                    size = reply.1.as_ref().expect("a picture").size;
                    let [all, turned, filled] = take_times().expect("the helper's own times").map(|us| us as f64 / 1000.0);
                    round.push(took);
                    helper.push(all);
                    turning.push(turned);
                    filling.push(filled);
                    pipe.push(took - all);
                }
                let (middle, p95) = middle_and_p95(round);
                let show = |values: Vec<f64>| middle_and_p95(values).0;
                println!("  {name:12} {size:?}: {middle:7.1} ms median, {p95:7.1} ms p95; helper {:7.1}, turn {:7.1}, fill {:7.1}, pipe {:6.1}", show(helper), show(turning), show(filling), show(pipe));
            }
            drop(running);
            std::fs::remove_file(&file).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn requests_and_replies_survive_the_pipe() {
        let path = Path::new("/tmp/zażółć file.pdf");
        let mut bytes = Vec::new();
        let view = Some(ModelView { turn: -1800, tilt: 45, zoom: 250, pan: [-3, 7], size: [640, 480], fit: 512, fine: true });
        let options = Options { max_side: 2048, page: 3, crop: Some([1, 2, 3, 4]), view };
        write_request(&mut bytes, path, options).unwrap();
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), (path.to_path_buf(), options));
        let mut bytes = Vec::new();
        write_request(&mut bytes, path, ANY).unwrap();
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap().1, ANY);
        let reply: Reply = (Loaded::Image([9, 9]), Some(Picture { size: [2, 1], delays: Vec::new(), rgba: vec![7; 8] }));
        let (loaded, picture) = read_reply(&mut frame(&reply).as_slice()).unwrap();
        assert!(matches!(loaded, Loaded::Image([9, 9])));
        assert_eq!(picture.unwrap().rgba, vec![7; 8]);
        let animation: Reply = (Loaded::Image([2, 1]), Some(Picture { size: [2, 1], delays: vec![50, 60], rgba: vec![1; 16] }));
        let picture = read_reply(&mut frame(&animation).as_slice()).unwrap().1.unwrap();
        assert_eq!((picture.delays, picture.rgba.len()), (vec![50, 60], 16));
        let text: Reply = (Loaded::Text { text: "żółw".into(), trimmed: true, colours: load::Colours { spans: vec![(7, 1)], ends: vec![1] } }, None);
        assert!(matches!(read_reply(&mut frame(&text).as_slice()).unwrap(), (Loaded::Text { text, trimmed: true, .. }, None) if text == "żółw"));
    }

    #[test]
    fn replies_that_lie_are_refused() {
        let reply: Reply = (Loaded::Detail, Some(Picture { size: [100000, 100000], delays: Vec::new(), rgba: Vec::new() }));
        assert!(read_reply(&mut frame(&reply).as_slice()).is_err(), "impossible picture size");
        let many: Reply = (Loaded::Detail, Some(Picture { size: [16000, 16000], delays: vec![1; 1000], rgba: Vec::new() }));
        assert!(read_reply(&mut frame(&many).as_slice()).is_err(), "too much memory promised");
        let short: Reply = (Loaded::Detail, Some(Picture { size: [2, 2], delays: Vec::new(), rgba: vec![0; 4] }));
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
        let reply = ask(&mut running, dying, Path::new("/x"), ANY, Duration::from_secs(5), &never_stale).unwrap();
        assert!(matches!(reply.0, Loaded::Error(ref m) if m == FAILED));
        assert!(running.is_none(), "the next file starts a new helper");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_stuck_helper_is_ended_at_the_limit_or_when_replaced() {
        let stuck = || shell("sleep 30");
        let never_stale = || false;
        let mut running = None;
        let reply = ask(&mut running, stuck, Path::new("/x"), ANY, Duration::from_millis(50), &never_stale).unwrap();
        assert!(matches!(reply.0, Loaded::Error(ref m) if m.contains("too long")));
        assert!(running.is_none());
        let stale = || true;
        let started = Instant::now();
        assert!(ask(&mut running, stuck, Path::new("/x"), ANY, Duration::from_secs(20), &stale).is_none());
        assert!(running.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
