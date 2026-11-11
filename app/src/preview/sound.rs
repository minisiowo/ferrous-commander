//! Playing a music file from its preview.
//!
//! The sound comes from a process of its own (`--sound-helper <file>`), one for each file
//! played. A decoder that breaks on a damaged file then ends the music and not the
//! application, and nothing of it stays in memory once the music stops.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use egui::{Align2, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::app::App;
use crate::fsx::{Entry, Kind};
use crate::theme::{Palette, blend};

pub const FLAG: &str = "--sound-helper";
/// How often the helper says where in the music it is.
const TICK: Duration = Duration::from_millis(200);
/// Opening a file or an audio device must not leave a helper stuck forever.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How far the arrow keys move in the music.
pub const STEP: Duration = Duration::from_secs(5);
pub const TRANSPORT_H: f32 = 28.0;

pub fn playable(kind: Kind, ext: &str) -> bool {
    kind == Kind::Audio && crate::fsx::AUDIO_PLAYBACK_EXTS.iter().any(|played| ext.eq_ignore_ascii_case(played))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Idle,
    Starting,
    Playing,
    Paused,
    Ended,
    NoDevice,
    Unreadable,
    /// The helper ended without saying why.
    Failed,
}

impl State {
    fn from_byte(byte: u8) -> Option<State> {
        [State::Playing, State::Paused, State::Ended, State::NoDevice, State::Unreadable].into_iter().find(|s| *s as u8 == byte)
    }

    /// Music that can still be paused, continued or moved in.
    pub fn running(self) -> bool {
        matches!(self, State::Starting | State::Playing | State::Paused)
    }

    fn trouble(self) -> Option<&'static str> {
        match self {
            State::NoDevice => Some("No sound device was found"),
            State::Unreadable => Some("This file cannot be played here"),
            State::Failed => Some("Playing this file failed"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CommandKind {
    Pause,
    Resume,
    Seek(Duration),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Order {
    command: CommandKind,
    revision: u64,
}

fn write_order(to: &mut impl Write, order: Order) -> io::Result<()> {
    let (tag, at) = match order.command {
        CommandKind::Pause => (1u8, 0),
        CommandKind::Resume => (2, 0),
        CommandKind::Seek(at) => (3, at.as_millis() as u64),
    };
    to.write_all(&[tag])?;
    to.write_all(&at.to_le_bytes())?;
    to.write_all(&order.revision.to_le_bytes())?;
    to.flush()
}

fn read_order(from: &mut impl Read) -> io::Result<Order> {
    let mut raw = [0; 17];
    from.read_exact(&mut raw)?;
    let at = Duration::from_millis(u64::from_le_bytes(raw[1..9].try_into().unwrap()));
    let command = match raw[0] {
        1 => CommandKind::Pause,
        2 => CommandKind::Resume,
        3 => CommandKind::Seek(at),
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    Ok(Order { command, revision: u64::from_le_bytes(raw[9..].try_into().unwrap()) })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Report {
    state: State,
    at: Duration,
    length: Duration,
    /// All commands, including pause and resume, are acknowledged in order.
    revision: u64,
}

impl Report {
    fn new(state: State, at: Duration, length: Duration, revision: u64) -> Self {
        Self { state, at, length, revision }
    }
}

fn write_report(to: &mut impl Write, report: Report) -> io::Result<()> {
    to.write_all(&[report.state as u8])?;
    to.write_all(&(report.at.as_millis() as u64).to_le_bytes())?;
    to.write_all(&(report.length.as_millis() as u64).to_le_bytes())?;
    to.write_all(&report.revision.to_le_bytes())?;
    to.flush()
}

fn read_report(from: &mut impl Read) -> io::Result<Report> {
    let mut raw = [0; 25];
    from.read_exact(&mut raw)?;
    let number = |at: usize| Duration::from_millis(u64::from_le_bytes(raw[at..at + 8].try_into().unwrap()));
    let state = State::from_byte(raw[0]).ok_or(io::ErrorKind::InvalidData)?;
    Ok(Report::new(state, number(1), number(9), u64::from_le_bytes(raw[17..].try_into().unwrap())))
}

type Music = rodio::Decoder<io::BufReader<std::fs::File>>;

/// The music in a file and how long it is; zero when nothing says.
fn open(path: &Path) -> Option<(Music, Duration)> {
    use rodio::Source;
    if !std::fs::metadata(path).ok()?.is_file() { return None; }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // A regular file may be replaced by a FIFO between metadata and open.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() { return None; }
    let bytes = metadata.len();
    let mut builder = rodio::Decoder::builder().with_data(io::BufReader::new(file)).with_byte_len(bytes).with_seekable(true);
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) { builder = builder.with_hint(&ext.to_ascii_lowercase()); }
    let music = builder.build().ok()?;
    let length = music.total_duration().or_else(|| {
        use lofty::prelude::AudioFile;
        Some(lofty::read_from_path(path).ok()?.properties().duration())
    });
    Some((music, length.unwrap_or_default()))
}

/// Obeys orders and reports until the music ends or nobody listens any more.
fn run(player: &rodio::Player, length: Duration, orders: &mpsc::Receiver<Order>, mut report: impl FnMut(Report) -> bool) {
    let mut revision = 0;
    loop {
        if player.empty() {
            report(Report::new(State::Ended, length, length, revision));
            return;
        }
        let state = if player.is_paused() { State::Paused } else { State::Playing };
        if !report(Report::new(state, player.get_pos(), length, revision)) { return; }
        match orders.recv_timeout(TICK) {
            Ok(order) => {
                match order.command {
                    CommandKind::Pause => player.pause(),
                    CommandKind::Resume => player.play(),
                    CommandKind::Seek(to) => {
                        let to = if length.is_zero() { to } else { to.min(length) };
                        // A rejected seek is acknowledged with the actual position,
                        // rather than leaving the UI waiting for an impossible move.
                        let _ = player.try_seek(to);
                    }
                }
                revision = order.revision;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The body of `ferrous-commander --sound-helper <file>`: plays until told otherwise.
pub fn serve(path: &Path) {
    super::helper::limit_this_process();
    let (ready, startup) = mpsc::channel();
    std::thread::spawn(move || {
        if matches!(startup.recv_timeout(START_TIMEOUT), Err(mpsc::RecvTimeoutError::Timeout)) {
            std::process::exit(1);
        }
    });
    let mut output = io::stdout().lock();
    let mut report = move |report: Report| write_report(&mut output, report).is_ok();
    let Some((music, length)) = open(path) else {
        report(Report::new(State::Unreadable, Duration::ZERO, Duration::ZERO, 0));
        return;
    };
    let Ok(mut device) = rodio::DeviceSinkBuilder::open_default_sink() else {
        report(Report::new(State::NoDevice, Duration::ZERO, length, 0));
        return;
    };
    device.log_on_drop(false);
    let player = rodio::Player::connect_new(device.mixer());
    player.append(music);
    let _ = ready.send(());
    let (tx, orders) = mpsc::channel();
    // Ends with the application's end of the pipe, and takes the music along.
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        while let Ok(order) = read_order(&mut input) {
            if tx.send(order).is_err() { return; }
        }
    });
    run(&player, length, &orders, report);
}

/// What is known about the music, in the application.
#[derive(Clone, Debug)]
pub struct Status {
    pub track: Option<PathBuf>,
    pub state: State,
    pub length: Duration,
    at: Duration,
    since: Instant,
    /// Which helper this is about; a late word from an older one is ignored.
    run: u64,
    /// Latest requested command; older reports must not undo it.
    revision: u64,
}

impl Status {
    fn idle() -> Status {
        Status { track: None, state: State::Idle, length: Duration::ZERO, at: Duration::ZERO, since: Instant::now(), run: 0, revision: 0 }
    }

    /// Where in the music it is now, between two reports too.
    pub fn position(&self) -> Duration {
        let at = if self.state == State::Playing { self.at + self.since.elapsed() } else { self.at };
        if self.length.is_zero() { at } else { at.min(self.length) }
    }

    fn report(&mut self, report: Report) {
        if report.revision != self.revision && report.state != State::Ended && report.state.trouble().is_none() { return; }
        self.length = report.length;
        self.set(report.state, report.at);
    }

    fn set(&mut self, state: State, at: Duration) {
        (self.state, self.at, self.since) = (state, at, Instant::now());
    }
}

struct Link {
    child: Option<Child>,
    stdin: ChildStdin,
}

impl Drop for Link {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let _ = child.kill();
        std::thread::spawn(move || { let _ = child.wait(); });
    }
}

type Start = fn(&Path) -> io::Result<Command>;

#[cfg_attr(test, allow(dead_code))]
fn helper_command(path: &Path) -> io::Result<Command> {
    let mut command = Command::new(super::helper::Helper::program()?);
    command.arg(FLAG).arg(path);
    Ok(command)
}

/// Tests have no helper binary to start: there the music is only said to play.
#[cfg(not(test))]
const START: Option<Start> = Some(helper_command);
#[cfg(test)]
const START: Option<Start> = None;

pub struct Sound {
    status: Arc<Mutex<Status>>,
    link: Option<Link>,
    start: Option<Start>,
    runs: u64,
}

impl Sound {
    pub fn new() -> Sound {
        Sound { status: Arc::new(Mutex::new(Status::idle())), link: None, start: START, runs: 0 }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// For the preview window, which is drawn without the application.
    pub fn shared(&self) -> Arc<Mutex<Status>> {
        self.status.clone()
    }

    fn connect(&self, path: &Path, run: u64, ctx: &egui::Context) -> io::Result<Link> {
        let Some(start) = self.start else { return Err(io::ErrorKind::Unsupported.into()) };
        let mut command = start(path)?;
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut command, 0x08000000);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let mut stdout = child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let (status, ctx) = (self.status.clone(), ctx.clone());
        std::thread::spawn(move || {
            loop {
                let report = read_report(&mut stdout);
                let mut status = status.lock().unwrap();
                if status.run != run { return; }
                let before = status.state;
                match report {
                    Ok(report) => status.report(report),
                    Err(_) if before.running() => status.set(State::Failed, Duration::ZERO),
                    Err(_) => {}
                }
                let changed = status.state != before;
                drop(status);
                if changed {
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                    ctx.request_repaint_of(super::window::viewport());
                }
                if report.is_err() { return; }
            }
        });
        Ok(Link { child: Some(child), stdin })
    }

    pub fn play(&mut self, path: &Path, ctx: &egui::Context) {
        self.link = None;
        self.runs += 1;
        let state = if self.start.is_some() { State::Starting } else { State::Playing };
        *self.status.lock().unwrap() = Status { track: Some(path.to_path_buf()), state, run: self.runs, ..Status::idle() };
        if self.start.is_none() { return; }
        match self.connect(path, self.runs, ctx) {
            Ok(link) => self.link = Some(link),
            Err(_) => self.status.lock().unwrap().set(State::Failed, Duration::ZERO),
        }
    }

    fn order(&mut self, order: Order) {
        let Some(link) = &mut self.link else { return };
        if write_order(&mut link.stdin, order).is_err() { self.link = None; }
    }

    /// What the play button does: starts this file, or pauses and continues it.
    pub fn toggle(&mut self, path: &Path, ctx: &egui::Context) {
        let order = {
            let mut status = self.status.lock().unwrap();
            if status.track.as_deref() != Some(path) || !status.state.running() {
                drop(status);
                return self.play(path, ctx);
            }
            let (state, command) = if status.state == State::Paused { (State::Playing, CommandKind::Resume) } else { (State::Paused, CommandKind::Pause) };
            let at = status.position();
            status.revision += 1;
            status.set(state, at);
            Order { command, revision: status.revision }
        };
        self.order(order);
    }

    pub fn seek(&mut self, to: Duration) {
        let order = {
            let mut status = self.status.lock().unwrap();
            if !status.state.running() { return; }
            let to = if status.length.is_zero() { to } else { to.min(status.length) };
            status.revision += 1;
            let state = status.state;
            status.set(state, to);
            Order { command: CommandKind::Seek(to), revision: status.revision }
        };
        self.order(order);
    }

    pub fn stop(&mut self) {
        self.link = None;
        self.runs += 1;
        *self.status.lock().unwrap() = Status { run: self.runs, ..Status::idle() };
    }
}

/// What the user asked of the transport.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ask {
    Toggle,
    Seek(Duration),
}

fn clock(time: Duration) -> String {
    super::media::duration(time.as_secs())
}

/// The play button and the line that shows and sets the place in the music.
/// `status` is that of this very file, and `None` while something else plays or nothing.
pub fn transport(ui: &Ui, r: Rect, pal: Palette, status: Option<&Status>, id: egui::Id) -> Option<Ask> {
    let p = ui.painter_at(r.expand(4.0));
    let mut ask = None;
    let state = status.map_or(State::Idle, |s| s.state);
    let side = r.height().min(TRANSPORT_H);
    let button = Rect::from_center_size(pos2(r.min.x + side / 2.0, r.center().y), vec2(side, side));
    let pressed = ui.interact(button, id.with("play"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    if pressed.clicked() { ask = Some(Ask::Toggle); }
    p.rect_filled(button, pal.corner(), if pressed.hovered() { pal.hover } else { blend(pal.panel, pal.text, 0.06) });
    let (c, half) = (button.center(), side * 0.19);
    if matches!(state, State::Starting | State::Playing) {
        for x in [-half * 0.75, half * 0.75] {
            p.rect_filled(Rect::from_center_size(c + vec2(x, 0.0), vec2(half * 0.7, half * 2.0)), 0.0, pal.text);
        }
    } else {
        let points = vec![c + vec2(-half * 0.8, -half), c + vec2(half, 0.0), c + vec2(-half * 0.8, half)];
        p.add(egui::Shape::convex_polygon(points, pal.text, Stroke::NONE));
    }
    let font = FontId::proportional(11.0);
    let rest = Rect::from_min_max(pos2(button.max.x + 10.0, r.min.y), r.max);
    if let Some(trouble) = state.trouble() {
        p.text(rest.left_center(), Align2::LEFT_CENTER, crate::ui::fit_text(&p, trouble, &font, rest.width()), font, pal.text_weak);
        return ask;
    }
    let Some(status) = status.filter(|s| s.state != State::Idle) else {
        p.line_segment([rest.left_center(), rest.right_center()], Stroke::new(3.0, blend(pal.panel, pal.text, 0.12)));
        return ask;
    };
    let times = rest.width() > 150.0;
    let line = if times { rest.shrink2(vec2(40.0, 0.0)) } else { rest };
    let known = !status.length.is_zero();
    let sense = if known && status.state.running() { Sense::click_and_drag() } else { Sense::hover() };
    let held = ui.interact(line.expand2(vec2(4.0, 0.0)), id.with("line"), sense);
    let pointed = held.interact_pointer_pos().map(|at| ((at.x - line.min.x) / line.width().max(1.0)).clamp(0.0, 1.0));
    let playing = if known { status.position().as_secs_f32() / status.length.as_secs_f32() } else { 0.0 };
    // While the line is held it shows the hand, and the music moves once on letting go.
    let dropped = ui.data_mut(|data| {
        let last = data.get_temp::<f32>(held.id);
        match pointed {
            Some(at) => { data.insert_temp(held.id, at); }
            None => data.remove::<f32>(held.id),
        }
        last.filter(|_| pointed.is_none())
    });
    // The deferred preview queues the seek for the main window. Keep drawing the
    // released position until that window has applied it to the shared status.
    let pending_id = held.id.with("pending_seek");
    let pending = ui.data_mut(|data| {
        if let Some(at) = dropped.filter(|_| known) {
            data.insert_temp(pending_id, (status.run, status.revision, at));
        }
        let pending = data.get_temp::<(u64, u64, f32)>(pending_id);
        if let Some((run, seek, at)) = pending {
            if run == status.run && seek == status.revision && status.state.running() { return Some(at); }
            data.remove::<(u64, u64, f32)>(pending_id);
        }
        None
    });
    if let Some(at) = dropped.filter(|_| known) { ask = Some(Ask::Seek(status.length.mul_f32(at))); }
    let shown = pointed.or(pending).unwrap_or(playing);
    let y = line.center().y;
    p.line_segment([pos2(line.min.x, y), pos2(line.max.x, y)], Stroke::new(3.0, blend(pal.panel, pal.text, 0.12)));
    let x = line.min.x + line.width() * shown;
    p.line_segment([pos2(line.min.x, y), pos2(x, y)], Stroke::new(3.0, pal.accent));
    if held.hovered() || held.dragged() { p.circle_filled(pos2(x, y), 5.0, pal.accent); }
    if times {
        let at = if known { status.length.mul_f32(shown) } else { status.position() };
        p.text(pos2(line.min.x - 8.0, y), Align2::RIGHT_CENTER, clock(at), font.clone(), pal.text_weak);
        if known { p.text(pos2(line.max.x + 8.0, y), Align2::LEFT_CENTER, clock(status.length), font, pal.text_weak); }
    }
    if status.state == State::Playing || status.state == State::Starting {
        ui.ctx().request_repaint_after(Duration::from_millis(if held.dragged() { 16 } else { 250 }));
    }
    ask
}

impl App {
    pub(crate) fn sound_ask(&mut self, path: &Path, ask: Ask, ctx: &egui::Context) {
        match ask {
            Ask::Toggle => self.sound.toggle(path, ctx),
            Ask::Seek(to) => self.sound.seek(to),
        }
    }

    /// Start a newly shown track once; paused, ended and failed tracks keep their state.
    pub(crate) fn sound_preview(&mut self, e: &Entry, ctx: &egui::Context) {
        self.sound_shown = Some(e.path.clone());
        if playable(e.kind, e.ext()) && self.sound.status().track.as_ref() != Some(&e.path) {
            self.sound.play(&e.path, ctx);
        }
    }

    /// The transport inside the file list's own panels. Says that this file is on show, which
    /// is what lets its music go on.
    pub fn sound_strip(&mut self, ui: &Ui, r: Rect, e: &Entry) {
        if !playable(e.kind, e.ext()) { return; }
        let status = self.sound.status();
        let mine = (status.track.as_deref() == Some(&*e.path)).then_some(&status);
        if let Some(ask) = transport(ui, r, self.pal, mine, egui::Id::unique("sound_strip")) {
            self.sound_ask(&e.path, ask, ui.ctx());
        }
    }

    /// Music belongs to the file whose preview is on show, and ends when that one is left.
    pub(crate) fn sound_follows(&mut self) {
        let shown = self.sound_shown.take();
        let status = self.sound.status();
        if status.track.is_some() && status.track != shown { self.sound.stop(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Silence as a WAV file: one channel, 16 bits.
    fn write_wav(path: &Path, seconds: u32, rate: u32) {
        let bytes = seconds * rate * 2;
        let mut wav = Vec::with_capacity(44 + bytes as usize);
        wav.extend(b"RIFF");
        wav.extend((36 + bytes).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 2).to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(bytes.to_le_bytes());
        wav.resize(44 + bytes as usize, 0);
        std::fs::write(path, wav).unwrap();
    }

    /// Stands in for the sound device: takes the samples as fast as they come.
    fn listen(output: rodio::queue::SourcesQueueOutput) {
        std::thread::spawn(move || for _ in output {});
    }

    #[test]
    fn orders_and_reports_survive_the_pipe() {
        for command in [CommandKind::Pause, CommandKind::Resume, CommandKind::Seek(Duration::from_millis(90_500))] {
            let order = Order { command, revision: 42 };
            let mut bytes = Vec::new();
            write_order(&mut bytes, order).unwrap();
            assert_eq!(read_order(&mut bytes.as_slice()).unwrap(), order);
        }
        for state in [State::Playing, State::Paused, State::Ended, State::NoDevice, State::Unreadable] {
            let report = Report::new(state, Duration::from_millis(1500), Duration::from_secs(200), 42);
            let mut bytes = Vec::new();
            write_report(&mut bytes, report).unwrap();
            assert_eq!(read_report(&mut bytes.as_slice()).unwrap(), report);
        }
        assert!(read_order(&mut [9u8; 17].as_slice()).is_err(), "an unknown order");
        assert!(read_report(&mut [0u8; 25].as_slice()).is_err(), "a state the helper never reports");
        assert!(read_report(&mut [State::Playing as u8; 5].as_slice()).is_err(), "cut short");
    }

    #[test]
    fn music_is_played_to_its_end_and_reported_on_the_way() {
        let root = crate::preview::load::scratch("sound_end");
        let file = root.join("a.wav");
        write_wav(&file, 3, 8000);
        let (music, length) = open(&file).unwrap();
        assert_eq!(length, Duration::from_secs(3));
        let (player, output) = rodio::Player::new();
        player.append(music);
        listen(output);
        let (_tx, orders) = mpsc::channel();
        let mut reports = Vec::new();
        run(&player, length, &orders, |report| { reports.push(report); true });
        assert_eq!(reports.last(), Some(&Report::new(State::Ended, length, length, 0)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn music_pauses_moves_and_goes_on_when_told() {
        let root = crate::preview::load::scratch("sound_orders");
        let file = root.join("a.wav");
        write_wav(&file, 600, 8000);
        let (music, length) = open(&file).unwrap();
        let (player, output) = rodio::Player::new();
        player.append(music);
        let (tx, orders) = mpsc::channel();
        let mut output = Some(output);
        let mut seen = Vec::new();
        run(&player, length, &orders, |Report { state, at, revision, .. }| {
            seen.push((state, at));
            match seen.len() {
                1 => {
                    assert_eq!((state, at), (State::Playing, Duration::ZERO));
                    tx.send(Order { command: CommandKind::Pause, revision: 1 }).unwrap();
                }
                // Nothing was taken yet, as with a device that has not asked for sound.
                2 => {
                    assert_eq!((state, at), (State::Paused, Duration::ZERO));
                    listen(output.take().unwrap());
                    tx.send(Order { command: CommandKind::Seek(Duration::from_secs(599)), revision: 2 }).unwrap();
                }
                3 => {
                    assert_eq!((state, at), (State::Paused, Duration::from_secs(599)), "moved while paused");
                    assert_eq!(revision, 2, "the report acknowledges the applied seek");
                    tx.send(Order { command: CommandKind::Resume, revision: 3 }).unwrap();
                }
                _ => {}
            }
            true
        });
        assert_eq!(seen.last(), Some(&(State::Ended, length)), "{seen:?}");
        assert!(seen.len() < 20, "the last second is over quickly: {seen:?}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_that_is_not_music_is_refused_and_the_helper_stops_when_unheard() {
        let root = crate::preview::load::scratch("sound_bad");
        let file = root.join("a.mp3");
        std::fs::write(&file, b"not audio at all, only a name that says so").unwrap();
        assert!(open(&file).is_none());
        assert!(open(&root.join("missing.mp3")).is_none());
        let good = root.join("a.wav");
        write_wav(&good, 600, 8000);
        let (music, length) = open(&good).unwrap();
        let (player, _output) = rodio::Player::new();
        player.append(music);
        let (_tx, orders) = mpsc::channel();
        let mut reports = 0;
        run(&player, length, &orders, |_| { reports += 1; false });
        assert_eq!(reports, 1, "a closed pipe ends the helper");
        let (tx, orders) = mpsc::channel::<Order>();
        drop(tx);
        let mut reports = 0;
        run(&player, length, &orders, |_| { reports += 1; true });
        assert_eq!(reports, 1, "and so does the end of the orders");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Every format that gets a play button must really open. Needs ffmpeg to make the files.
    #[test]
    fn every_format_with_a_play_button_opens() {
        let Some(tool) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX))).find(|p| p.is_file()) else { return };
        let root = crate::preview::load::scratch("sound_formats");
        let mut made = 0;
        for ext in crate::fsx::AUDIO_PLAYBACK_EXTS {
            let file = root.join(format!("tone.{ext}"));
            let done = Command::new(&tool).args(["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"]).arg(&file).status();
            // This ffmpeg may lack an encoder; the others still count.
            if !done.is_ok_and(|status| status.success()) { continue; }
            made += 1;
            let (music, length) = open(&file).unwrap_or_else(|| panic!("{ext} does not open"));
            assert!((1900..2200).contains(&length.as_millis()), "{ext}: {length:?}");
            assert!(music.take(1000).count() == 1000, "{ext} gives no sound");
        }
        assert!(made >= 5, "only {made} formats could be made");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_music_the_decoders_know_gets_a_play_button() {
        assert!(playable(Kind::Audio, "MP3") && playable(Kind::Audio, "flac"));
        assert!(!playable(Kind::Audio, "opus") && !playable(Kind::Audio, "ape"));
        assert!(!playable(Kind::Video, "mp4") && !playable(Kind::Other, "wav"));
    }

    fn silent() -> (Sound, egui::Context) {
        (Sound::new(), egui::Context::default())
    }

    #[test]
    fn the_button_starts_pauses_continues_and_starts_another_file() {
        let (mut sound, ctx) = silent();
        let (a, b) = (Path::new("/music/a.mp3"), Path::new("/music/b.mp3"));
        assert_eq!(sound.status().state, State::Idle);
        sound.toggle(a, &ctx);
        assert_eq!((sound.status().track.as_deref(), sound.status().state), (Some(a), State::Playing));
        sound.toggle(a, &ctx);
        assert_eq!(sound.status().state, State::Paused);
        let paused_at = sound.status().position();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sound.status().position(), paused_at, "paused music stays where it is");
        sound.toggle(a, &ctx);
        assert_eq!(sound.status().state, State::Playing);
        sound.toggle(b, &ctx);
        assert_eq!((sound.status().track.as_deref(), sound.status().state), (Some(b), State::Playing));
        assert!(sound.status().position() < Duration::from_secs(1), "the other file starts from its beginning");
        sound.stop();
        assert_eq!((sound.status().track, sound.status().state), (None, State::Idle));
    }

    #[test]
    fn stale_reports_cannot_undo_pause_resume_or_startup_toggles() {
        let (mut sound, ctx) = silent();
        let file = Path::new("/music/a.mp3");
        sound.play(file, &ctx);
        sound.status.lock().unwrap().set(State::Starting, Duration::ZERO);
        sound.toggle(file, &ctx);
        assert_eq!(sound.status().state, State::Paused, "pause is remembered while the device opens");
        sound.status.lock().unwrap().report(Report::new(State::Playing, Duration::ZERO, Duration::from_secs(100), 0));
        assert_eq!(sound.status().state, State::Paused);
        sound.toggle(file, &ctx);
        sound.status.lock().unwrap().report(Report::new(State::Paused, Duration::ZERO, Duration::from_secs(100), 1));
        assert_eq!(sound.status().state, State::Playing, "a late pause does not undo resume");
        sound.status.lock().unwrap().report(Report::new(State::Playing, Duration::from_secs(2), Duration::from_secs(100), 2));
        assert!(sound.status().position() >= Duration::from_secs(2));
        sound.toggle(file, &ctx);
        sound.status.lock().unwrap().report(Report::new(State::Playing, Duration::from_secs(3), Duration::from_secs(100), 2));
        assert_eq!(sound.status().state, State::Paused);
        sound.status.lock().unwrap().report(Report::new(State::Ended, Duration::from_secs(100), Duration::from_secs(100), 2));
        assert_eq!(sound.status().state, State::Ended, "the helper may finish before a final pause reaches it");
    }

    #[test]
    fn every_playable_extension_is_classified_as_audio() {
        for ext in crate::fsx::AUDIO_PLAYBACK_EXTS {
            assert!(playable(Kind::from_ext(ext), ext), "{ext}");
            assert!(playable(Kind::from_ext(&ext.to_uppercase()), ext), "{ext}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_refused_without_opening_or_reading_them() {
        use std::os::unix::ffi::OsStrExt;
        let root = crate::preview::load::scratch("sound_special");
        let pipe = root.join("pipe.mp3");
        let raw = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid pathname, creating a FIFO in this test's directory.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || { let _ = tx.send(open(&pipe).is_none()); });
        assert!(rx.recv_timeout(Duration::from_secs(1)).expect("opening a FIFO must not block"));
        assert!(open(Path::new("/dev/zero")).is_none());
        assert!(open(&root).is_none());
        let file = root.join("a.wav");
        let link = root.join("link.wav");
        write_wav(&file, 1, 8000);
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(open(&link).is_some(), "links to regular music files still work");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_place_moves_between_reports_and_stays_within_the_music() {
        let (mut sound, ctx) = silent();
        sound.play(Path::new("/music/a.mp3"), &ctx);
        sound.status.lock().unwrap().length = Duration::from_secs(100);
        sound.seek(Duration::from_secs(40));
        std::thread::sleep(Duration::from_millis(30));
        let at = sound.status().position();
        assert!(at > Duration::from_secs(40) && at < Duration::from_secs(41), "{at:?}");
        sound.seek(Duration::from_secs(500));
        assert_eq!(sound.status().position(), Duration::from_secs(100));
        sound.stop();
        sound.seek(Duration::from_secs(5));
        assert_eq!(sound.status().state, State::Idle, "nothing to move in");
    }

    #[test]
    fn the_button_asks_to_play_and_the_line_asks_for_a_place_once_let_go() {
        use egui::{Event, Modifiers, PointerButton, RawInput};
        let (mut sound, ctx) = silent();
        let file = Path::new("/music/a.mp3");
        let pal = Palette::get(crate::theme::ThemeName::Dark, egui::Color32::WHITE);
        let r = Rect::from_min_size(pos2(100.0, 100.0), vec2(400.0, TRANSPORT_H));
        let run = |sound: &Sound, events: Vec<Event>| {
            let status = sound.status();
            let mine = (status.track.as_deref() == Some(file)).then_some(&status);
            let mut ask = None;
            let input = RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| ask = transport(ui, r, pal, mine, egui::Id::unique("test_transport")));
            out.textures_delta.clear();
            ask
        };
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let click = |sound: &Sound, pos| {
            let frames = [vec![Event::PointerMoved(pos)], vec![button(pos, true)], vec![button(pos, false)], vec![], vec![]];
            frames.into_iter().filter_map(|events| run(sound, events)).collect::<Vec<_>>()
        };
        let (play, line) = (pos2(114.0, 114.0), |part: f32| pos2(178.0 + 282.0 * part, 114.0));
        assert!(click(&sound, line(0.5)).is_empty(), "there is no place in music that does not play");
        assert_eq!(click(&sound, play), [Ask::Toggle]);
        sound.toggle(file, &ctx);
        sound.status.lock().unwrap().length = Duration::from_secs(100);
        let asked = click(&sound, line(0.75));
        let [Ask::Seek(to)] = asked[..] else { panic!("{asked:?}") };
        assert!((74_000..76_000).contains(&to.as_millis()), "{to:?}");
        // Held and moved: nothing is asked until the button is let go, and then once.
        let mut asks = Vec::new();
        for events in [vec![Event::PointerMoved(line(0.1))], vec![button(line(0.1), true)], vec![Event::PointerMoved(line(0.2))], vec![Event::PointerMoved(line(0.3))]] {
            asks.extend(run(&sound, events));
        }
        assert!(asks.is_empty(), "{asks:?}");
        for events in [vec![button(line(0.3), false)], vec![], vec![]] { asks.extend(run(&sound, events)); }
        let [Ask::Seek(to)] = asks[..] else { panic!("{asks:?}") };
        assert!((29_000..31_000).contains(&to.as_millis()), "{to:?}");
        assert_eq!(click(&sound, play), [Ask::Toggle], "the button stays a button while music plays");
    }

    #[test]
    fn releasing_the_slider_keeps_its_position_until_the_seek_is_applied() {
        use egui::{Event, Modifiers, PointerButton, RawInput, Shape};
        for state in [State::Playing, State::Paused] {
            let (mut sound, ctx) = silent();
            sound.play(Path::new("/music/a.mp3"), &ctx);
            {
                let mut status = sound.status.lock().unwrap();
                status.length = Duration::from_secs(100);
                status.set(state, Duration::from_secs(10));
            }
            let pal = Palette::get(crate::theme::ThemeName::Dark, egui::Color32::WHITE);
            let r = Rect::from_min_size(pos2(100.0, 100.0), vec2(400.0, TRANSPORT_H));
            let line = |part: f32| pos2(178.0 + 282.0 * part, 114.0);
            let button = |part, pressed| Event::PointerButton { pos: line(part), button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            let frame = |sound: &Sound, events| {
                let status = sound.status();
                let mut ask = None;
                let mut out = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| {
                    ask = transport(ui, r, pal, Some(&status), egui::Id::unique("seek_regression"));
                });
                out.textures_delta.clear();
                let x = out.shapes.iter().find_map(|shape| match &shape.shape {
                    Shape::LineSegment { points, stroke } if stroke.color == pal.accent => Some(points[1].x),
                    _ => None,
                }).expect("the played part of the timeline is drawn");
                (ask, (x - 178.0) / 282.0)
            };
            for target in [0.8, 0.2] {
                frame(&sound, vec![Event::PointerMoved(line(0.4))]);
                frame(&sound, vec![button(0.4, true)]);
                let (_, shown) = frame(&sound, vec![Event::PointerMoved(line(target))]);
                assert!((shown - target).abs() < 0.001);
                let mut asks = Vec::new();
                // Delay the main window processing the command, as in a deferred preview.
                for events in [vec![button(target, false)], vec![], vec![], vec![]] {
                    let (ask, shown) = frame(&sound, events);
                    asks.extend(ask);
                    assert!((shown - target).abs() < 0.001, "release jumped to {shown}, wanted {target}");
                }
                let [Ask::Seek(to)] = asks[..] else { panic!("{asks:?}") };
                let old_seek = sound.status().revision;
                sound.seek(to);
                // A report already in the pipe must not restore the pre-seek position.
                sound.status.lock().unwrap().report(Report::new(state, Duration::from_secs(40), Duration::from_secs(100), old_seek));
                let (_, shown) = frame(&sound, vec![]);
                assert!((shown - target).abs() < 0.01, "stale report jumped to {shown}");
                let seek = sound.status().revision;
                sound.status.lock().unwrap().report(Report::new(state, to + Duration::from_millis(250), Duration::from_secs(100), seek));
                let (_, shown) = frame(&sound, vec![]);
                assert!((shown - (target + 0.0025)).abs() < 0.001, "acknowledged playback resumes at {shown}");
            }
        }
    }

    #[test]
    fn only_the_latest_seek_acknowledgement_can_update_the_position() {
        let (mut sound, ctx) = silent();
        sound.play(Path::new("/music/a.mp3"), &ctx);
        sound.toggle(Path::new("/music/a.mp3"), &ctx);
        sound.status.lock().unwrap().length = Duration::from_secs(100);
        sound.seek(Duration::from_secs(80));
        sound.seek(Duration::from_secs(20));
        let mut status = sound.status.lock().unwrap();
        status.report(Report::new(State::Playing, Duration::from_secs(80), Duration::from_secs(100), 2));
        assert_eq!(status.position(), Duration::from_secs(20));
        assert_eq!(status.state, State::Paused);
        status.report(Report::new(State::Paused, Duration::from_millis(20_050), Duration::from_secs(100), 3));
        assert_eq!(status.position(), Duration::from_millis(20_050));
        status.report(Report::new(State::NoDevice, Duration::ZERO, Duration::ZERO, 0));
        assert_eq!(status.state, State::NoDevice, "startup failures are reported even if a seek was queued");
    }

    #[cfg(target_os = "linux")]
    fn with_helper(script: &'static str) -> (Sound, egui::Context) {
        let (mut sound, ctx) = silent();
        sound.start = Some(match script {
            "dies" => |_: &Path| { let mut c = Command::new("/bin/sh"); c.args(["-c", "exit 3"]); Ok(c) },
            "no device" => |_: &Path| { let mut c = Command::new("/bin/sh"); c.args(["-c", r"printf '\005\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000\000'"]); Ok(c) },
            "plays" => |_: &Path| { let mut c = Command::new("/bin/sh"); c.args(["-c", r"printf '\002\350\003\000\000\000\000\000\000\020\047\000\000\000\000\000\000\000\000\000\000\000\000\000\000'; exec sleep 30"]); Ok(c) },
            _ => |_: &Path| Err(io::ErrorKind::NotFound.into()),
        });
        (sound, ctx)
    }

    #[cfg(target_os = "linux")]
    fn wait_for(sound: &Sound, state: State) {
        let until = Instant::now() + Duration::from_secs(10);
        while sound.status().state != state {
            assert!(Instant::now() < until, "still {:?}", sound.status().state);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_helper_that_dies_or_has_no_device_is_reported_and_can_be_tried_again() {
        let file = Path::new("/music/a.mp3");
        let (mut sound, ctx) = with_helper("dies");
        sound.play(file, &ctx);
        wait_for(&sound, State::Failed);
        let (mut sound, ctx) = with_helper("no device");
        sound.play(file, &ctx);
        wait_for(&sound, State::NoDevice);
        let (mut sound, ctx) = with_helper("missing");
        sound.play(file, &ctx);
        assert_eq!(sound.status().state, State::Failed);
        let (mut sound, ctx) = with_helper("plays");
        sound.toggle(file, &ctx);
        wait_for(&sound, State::Playing);
        let status = sound.status();
        assert_eq!(status.length, Duration::from_secs(10));
        assert!(status.position() >= Duration::from_secs(1) && status.position() < Duration::from_secs(3));
        sound.stop();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(sound.status().state, State::Idle, "the ended helper's last word changes nothing");
    }

    /// The real helper on the real sound device, three seconds of silence:
    /// `cargo build --release`, then with `FERROUS_HELPER_EXE=target/release/ferrous-commander`
    /// `cargo test --release sound_through_the_real_helper -- --ignored --nocapture`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs the release program, named by FERROUS_HELPER_EXE, and a sound device"]
    fn sound_through_the_real_helper() {
        let root = crate::preview::load::scratch("sound_real");
        let file = root.join("a.wav");
        write_wav(&file, 3, 44100);
        let (mut sound, ctx) = silent();
        sound.start = Some(|path: &Path| {
            let mut command = Command::new(std::env::var_os("FERROUS_HELPER_EXE").expect("FERROUS_HELPER_EXE"));
            command.arg(FLAG).arg(path);
            Ok(command)
        });
        let started = Instant::now();
        sound.play(&file, &ctx);
        wait_for(&sound, State::Playing);
        println!("playing after {} ms", started.elapsed().as_millis());
        let child = sound.link.as_ref().unwrap().child.as_ref().unwrap().id();
        std::thread::sleep(Duration::from_millis(1000));
        let rollup = std::fs::read_to_string(format!("/proc/{child}/smaps_rollup")).unwrap();
        for line in rollup.lines().filter(|line| line.starts_with("Pss") || line.starts_with("Rss")) { println!("helper {line}"); }
        sound.toggle(&file, &ctx);
        wait_for(&sound, State::Paused);
        // The helper's own word on where it stopped replaces the estimate.
        std::thread::sleep(Duration::from_millis(500));
        let paused = sound.status().position();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(sound.status().position(), paused);
        sound.seek(Duration::from_millis(2500));
        sound.toggle(&file, &ctx);
        wait_for(&sound, State::Ended);
        assert!(started.elapsed() < Duration::from_millis(3500), "moving ahead shortened it: {:?}", started.elapsed());
        std::fs::remove_dir_all(root).unwrap();
    }
}
