//! Audio tags and covers, and a frame from a video when ffmpeg is installed.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::load::{Loaded, Picture, Reply, plain};

const MAX_SIDE: u32 = 1600;

fn to_picture(bytes: &[u8]) -> Option<Picture> {
    let img = image::load_from_memory(bytes).ok()?;
    // `thumbnail` also enlarges, which only blurs a small cover.
    let img = if img.width().max(img.height()) > MAX_SIDE { img.thumbnail(MAX_SIDE, MAX_SIDE) } else { img }.to_rgba8();
    Some(Picture { size: [img.width(), img.height()], delays: Vec::new(), rgba: img.into_raw() })
}

fn duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

pub fn audio(p: &Path) -> Reply {
    use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
    let Ok(file) = lofty::read_from_path(p) else {
        return plain(Loaded::Info { rows: vec![("Audio".into(), "No details could be read from this file".into())] });
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
    (Loaded::Info { rows }, picture)
}

/// A relative PATH entry would run whatever sits in the folder the app started from.
fn ffmpeg() -> Option<PathBuf> {
    let file = format!("ffmpeg{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?).filter(|d| d.is_absolute()).map(|d| d.join(&file)).find(|p| p.is_file())
}

fn frame_at(tool: &Path, input: &OsStr, seek: &str) -> Option<(Vec<u8>, String)> {
    let mut command = Command::new(tool);
    command.args(["-hide_banner", "-nostdin", "-timelimit", "10", "-ss", seek, "-i"]).arg(input)
        .args(["-frames:v", "1", "-vf", "scale='min(1600,iw)':-2", "-f", "image2pipe", "-vcodec", "png", "-"])
        .stdin(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut command, 0x08000000);
    let output = command.output().ok()?;
    Some((output.stdout, String::from_utf8_lossy(&output.stderr).into_owned()))
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
        let size = video.split([',', ' ']).find(|part| {
            let mut sides = part.split('x');
            let digits = |s: Option<&str>| s.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
            digits(sides.next()) && digits(sides.next()) && sides.next().is_none()
        });
        if let Some(size) = size { rows.push(("Resolution".to_string(), size.replace('x', " × "))); }
    }
    rows
}

pub fn video(p: &Path) -> Reply {
    let Some(tool) = ffmpeg() else {
        return plain(Loaded::Info { rows: vec![("Video".into(), "Install ffmpeg to see a frame from this video".into())] });
    };
    // The prefix keeps a file named like "concat:…" or "http:…" from being taken for a protocol.
    let mut input = OsString::from("file:");
    input.push(p.as_os_str());
    // A little way in, past black opening frames; very short clips have nothing there.
    let attempt = frame_at(&tool, &input, "5").filter(|(frame, _)| !frame.is_empty()).or_else(|| frame_at(&tool, &input, "0"));
    let Some((frame, log)) = attempt else {
        return plain(Loaded::Info { rows: vec![("Video".into(), "ffmpeg could not read this file".into())] });
    };
    let rows = details(&log);
    let picture = to_picture(&frame);
    if picture.is_none() && rows.is_empty() {
        return plain(Loaded::Info { rows: vec![("Video".into(), "No frame could be read from this file".into())] });
    }
    (Loaded::Info { rows }, picture)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_come_from_the_ffmpeg_log() {
        let log = "Input #0, mov, from 'file:a.mp4':\n  Duration: 00:01:23.45, start: 0.0, bitrate: 900 kb/s\n  Stream #0:0: Video: h264 (High), yuv420p(tv), 1920x1080 [SAR 1:1], 24 fps\n";
        assert_eq!(details(log), [("Length".to_string(), "00:01:23".to_string()), ("Codec".to_string(), "h264".to_string()), ("Resolution".to_string(), "1920 × 1080".to_string())]);
        assert!(details("nothing useful").is_empty());
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
