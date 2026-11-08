//! Audio tags and covers, and a frame from a video when ffmpeg is installed.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::load::{Loaded, Picture, Reply};

const MAX_SIDE: u32 = 1600;
/// Frames of the moving preview of a video, and how long each one stays.
const FRAMES: usize = 10;
const FRAME_MS: u32 = 700;
const FRAME_WIDTH: u32 = 960;
/// How many copies of ffmpeg may seek at once.
const WORKERS: usize = 4;
const LONG_VIDEO_SECONDS: f64 = 120.0;
const LARGE_VIDEO_PIXELS: u64 = 1920 * 1080;
/// The page of a video that means its moving preview; page 0 is one frame, ready sooner.
pub const FILM_PAGE: u32 = 1;

fn to_picture(bytes: &[u8]) -> Option<Picture> {
    let img = image::load_from_memory(bytes).ok()?;
    // `thumbnail` also enlarges, which only blurs a small cover.
    let img = if img.width().max(img.height()) > MAX_SIDE { img.thumbnail(MAX_SIDE, MAX_SIDE) } else { img }.to_rgba8();
    Some(Picture { size: [img.width(), img.height()], delays: Vec::new(), rgba: img.into_raw() })
}

fn note(key: &str, value: &str) -> Reply {
    (Loaded::Info { rows: vec![(key.to_string(), value.to_string())], more: false }, None)
}

fn duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

pub fn audio(p: &Path) -> Reply {
    use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
    let Ok(file) = lofty::read_from_path(p) else {
        return note("Audio", "No details could be read from this file");
    };
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut picture = None;
    if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
        let mut add = |key: &str, value: Option<String>| {
            if let Some(value) = value.filter(|v| !v.is_empty()) { rows.push((key.to_string(), value)); }
        };
        add("Title", tag.title().map(|v| v.into_owned()));
        add("Artist", tag.artist().map(|v| v.into_owned()));
        add("Album", tag.album().map(|v| v.into_owned()));
        add("Genre", tag.genre().map(|v| v.into_owned()));
        picture = tag.pictures().first().and_then(|cover| to_picture(cover.data()));
    }
    let properties = file.properties();
    rows.push(("Length".into(), duration(properties.duration().as_secs())));
    if let Some(rate) = properties.overall_bitrate() { rows.push(("Bitrate".into(), format!("{rate} kbps"))); }
    if let Some(rate) = properties.sample_rate() { rows.push(("Sample rate".into(), format!("{rate} Hz"))); }
    if let Some(channels) = properties.channels() { rows.push(("Channels".into(), channels.to_string())); }
    (Loaded::Info { rows, more: false }, picture)
}

/// A relative PATH entry would run whatever sits in the folder the app started from.
fn ffmpeg() -> Option<PathBuf> {
    let file = format!("ffmpeg{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?).filter(|d| d.is_absolute()).map(|d| d.join(&file)).find(|p| p.is_file())
}

/// A "video" can really be a playlist that tells ffmpeg to fetch addresses from the network,
/// and merely selecting a downloaded file must not do that.
fn before_input() -> [&'static str; 6] {
    ["-hide_banner", "-nostdin", "-timelimit", "10", "-protocol_whitelist", "file"]
}

fn command(tool: &Path) -> Command {
    let mut command = Command::new(tool);
    command.args(before_input()).stdin(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x08000000);
    command
}

/// What ffmpeg says about a file when given nothing to do with it.
fn probe(tool: &Path, input: &OsStr) -> Option<String> {
    let output = command(tool).arg("-i").arg(input).output().ok()?;
    Some(String::from_utf8_lossy(&output.stderr).into_owned())
}

fn grab(tool: &Path, input: &OsStr, seconds: f64, seeking: &[&str]) -> Option<Picture> {
    let output = command(tool).args(seeking).args(["-ss", &format!("{seconds:.3}"), "-i"]).arg(input)
        .args(["-frames:v", "1", "-vf", &format!("scale='min({FRAME_WIDTH},iw)':-2"), "-f", "image2pipe", "-vcodec", "mjpeg", "-q:v", "4", "-"])
        .output().ok()?;
    to_picture(&output.stdout)
}

/// In a short clip every position could land on one key frame, so `quick` is for the rest.
/// Some encoders mark no key frames that ffmpeg can skip to, hence the second try.
fn frame_at(tool: &Path, input: &OsStr, seconds: f64, quick: bool) -> Option<Picture> {
    if !quick { return grab(tool, input, seconds, &[]); }
    grab(tool, input, seconds, &["-noaccurate_seek", "-skip_frame", "nokey"])
        .or_else(|| grab(tool, input, seconds, &["-noaccurate_seek"]))
}

/// Where to look: evenly through the film, staying off the black start and end.
fn sample_times(duration: Option<f64>) -> Vec<f64> {
    let Some(duration) = duration.filter(|d| *d > 0.0) else { return vec![0.0] };
    let count = ((duration * 2.0).ceil() as usize).clamp(1, FRAMES);
    (0..count).map(|i| duration * (i as f64 + 0.5) / count as f64).collect()
}

fn frames_at(tool: &Path, input: &OsStr, times: &[f64], quick: bool) -> Vec<Picture> {
    let next = AtomicUsize::new(0);
    let found: Mutex<Vec<Option<Picture>>> = Mutex::new(times.iter().map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(times.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&seconds) = times.get(index) else { return };
                    let frame = frame_at(tool, input, seconds, quick);
                    found.lock().unwrap()[index] = frame;
                }
            });
        }
    });
    found.into_inner().unwrap().into_iter().flatten().collect()
}

/// One picture holding every frame, or the only frame there was.
fn film(frames: Vec<Picture>) -> Option<Picture> {
    let size = frames.first()?.size;
    let mut same: Vec<Picture> = frames.into_iter().filter(|f| f.size == size).collect();
    // Positions that landed on one key frame would only make the film stand still.
    same.dedup_by(|a, b| a.rgba == b.rgba);
    let delays = if same.len() > 1 { vec![FRAME_MS; same.len()] } else { Vec::new() };
    Some(Picture { size, delays, rgba: same.into_iter().flat_map(|f| f.rgba).collect() })
}

/// The film's length, from ffmpeg's "Duration: 00:01:23.45".
fn seconds_in(log: &str) -> Option<f64> {
    let value = log.split("Duration: ").nth(1)?.split(',').next()?.trim();
    let mut parts = value.split(':').map(|part| part.parse::<f64>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    Some(h * 3600.0 + m * 60.0 + s)
}

/// The picture size on ffmpeg's "Video:" line.
fn picture_size(log: &str) -> Option<(u64, u64)> {
    let line = log.lines().find(|l| l.contains("Video: "))?;
    line.split("Video: ").nth(1)?.split([',', ' ']).find_map(|part| {
        let (w, h) = part.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    })
}

/// Reads the lines ffmpeg prints about its input.
fn details(log: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    if let Some(at) = log.find("Duration: ") {
        let value = log[at + 10..].split(',').next().unwrap_or("").trim();
        let value = value.split('.').next().unwrap_or(value);
        if !value.is_empty() && value != "N/A" { rows.push(("Length".to_string(), value.to_string())); }
    }
    if let Some(line) = log.lines().find(|l| l.contains("Video: ")) {
        let video = line.split("Video: ").nth(1).unwrap_or("");
        rows.push(("Codec".to_string(), video.split([' ', ',']).next().unwrap_or("").to_string()));
        if let Some((w, h)) = picture_size(log) { rows.push(("Resolution".to_string(), format!("{w} × {h}"))); }
    }
    rows
}

/// Seeking to a key frame saves decoding, which matters for long and for large videos.
fn quick_seek(log: &str, duration: Option<f64>) -> bool {
    let large = picture_size(log).is_some_and(|(w, h)| w * h > LARGE_VIDEO_PIXELS);
    large || duration.is_some_and(|d| d > LONG_VIDEO_SECONDS)
}

pub fn video(p: &Path, page: u32) -> Reply {
    let Some(tool) = ffmpeg() else { return note("Video", "Install ffmpeg to see this video move") };
    // The prefix keeps a file named like "concat:…" or "http:…" from being taken for a protocol.
    let mut input = OsString::from("file:");
    input.push(p.as_os_str());
    let Some(log) = probe(&tool, &input) else { return note("Video", "ffmpeg could not read this file") };
    let duration = seconds_in(&log);
    let times = sample_times(duration);
    let quick = quick_seek(&log, duration);
    let picture = if page == FILM_PAGE {
        film(frames_at(&tool, &input, &times, quick))
    } else {
        frame_at(&tool, &input, times[times.len() / 2], quick)
    };
    let rows = details(&log);
    if picture.is_none() && rows.is_empty() { return note("Video", "No frame could be read from this file"); }
    let more = page != FILM_PAGE && times.len() > 1 && picture.is_some();
    (Loaded::Info { rows, more }, picture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_or_large_videos_seek_quickly() {
        let log = |size: &str| format!("  Stream #0:0: Video: h264 (High), yuv420p, {size} [SAR 1:1]\n");
        assert!(quick_seek(&log("3840x2160"), Some(10.0)));
        assert!(quick_seek(&log("640x480"), Some(600.0)));
        assert!(!quick_seek(&log("1920x1080"), Some(10.0)));
    }

    #[test]
    fn details_come_from_the_ffmpeg_log() {
        let log = "Input #0, mov, from 'file:a.mp4':\n  Duration: 00:01:23.45, start: 0.0, bitrate: 900 kb/s\n  Stream #0:0: Video: h264 (High), yuv420p(tv), 1920x1080 [SAR 1:1], 24 fps\n";
        assert_eq!(details(log), [("Length".to_string(), "00:01:23".to_string()), ("Codec".to_string(), "h264".to_string()), ("Resolution".to_string(), "1920 × 1080".to_string())]);
        assert!(details("nothing useful").is_empty());
    }

    #[test]
    fn only_local_files_are_allowed() {
        let arguments = before_input();
        let at = arguments.iter().position(|a| *a == "-protocol_whitelist").unwrap();
        assert_eq!(arguments[at + 1], "file");
    }

    #[test]
    fn frames_spread_over_the_film() {
        assert_eq!(sample_times(None), [0.0]);
        assert_eq!(sample_times(Some(0.0)), [0.0]);
        assert_eq!(sample_times(Some(0.4)).len(), 1, "too short to move");
        assert_eq!(sample_times(Some(1.0)), [0.25, 0.75]);
        let times = sample_times(Some(100.0));
        assert_eq!(times.len(), FRAMES);
        assert_eq!((times[0], times[9]), (5.0, 95.0));
    }

    #[test]
    fn the_film_length_is_read_from_the_log() {
        assert_eq!(seconds_in("  Duration: 01:02:03.50, start: 0.0"), Some(3723.5));
        assert_eq!(seconds_in("  Duration: N/A, start: 0.0"), None);
        assert_eq!(seconds_in("nothing"), None);
    }

    #[test]
    fn frames_join_into_one_film() {
        let frame = |w, shade| Picture { size: [w, 1], delays: Vec::new(), rgba: vec![shade; w as usize * 4] };
        let joined = film(vec![frame(2, 1), frame(2, 2)]).unwrap();
        assert_eq!((joined.delays, joined.rgba.len()), (vec![FRAME_MS, FRAME_MS], 16));
        assert!(film(vec![frame(2, 1)]).unwrap().delays.is_empty(), "one frame stays a still picture");
        assert_eq!(film(vec![frame(2, 1), frame(3, 2), frame(2, 3)]).unwrap().rgba.len(), 16, "a frame of another size is left out");
        assert!(film(vec![frame(2, 1), frame(2, 1), frame(2, 4)]).unwrap().delays.len() == 2, "repeated frames collapse");
        assert!(film(Vec::new()).is_none());
    }

    #[test]
    fn a_real_video_becomes_a_moving_preview() {
        let Some(tool) = ffmpeg() else { return };
        let root = crate::preview::load::scratch("video");
        let file = root.join("clip.mp4");
        let made = Command::new(tool).args(["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "testsrc=duration=6:size=160x120:rate=10", "-pix_fmt", "yuv420p"]).arg(&file).status();
        if made.is_ok_and(|status| status.success()) {
            let (loaded, still) = video(&file, 0);
            assert!(matches!(loaded, Loaded::Info { rows, more: true } if rows.iter().any(|(key, _)| key == "Length")));
            assert!(still.unwrap().delays.is_empty(), "one frame first");
            let (loaded, picture) = video(&file, FILM_PAGE);
            assert!(matches!(loaded, Loaded::Info { more: false, .. }));
            let picture = picture.unwrap();
            assert_eq!(picture.delays.len(), FRAMES);
            assert_eq!(picture.size, [160, 120]);
            let bytes = 160 * 120 * 4;
            assert_eq!(picture.rgba.len(), bytes * FRAMES);
            assert_ne!(&picture.rgba[..bytes], &picture.rgba[bytes..bytes * 2], "the frames differ");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lengths_read_naturally() {
        assert_eq!(duration(65), "1:05");
        assert_eq!(duration(3725), "1:02:05");
    }

    #[test]
    fn broken_audio_still_gets_a_card() {
        let root = crate::preview::load::scratch("audio");
        let file = root.join("a.mp3");
        std::fs::write(&file, b"not audio").unwrap();
        assert!(matches!(audio(&file).0, Loaded::Info { .. }));
        std::fs::remove_dir_all(root).unwrap();
    }
}
