//! Following the Omarchy desktop theme. `omarchy theme set` replaces the folder
//! holding `colors.toml`, so a slow check of that file sees every change and no
//! hook has to be installed. Elsewhere the file does not exist and the theme is
//! not offered.

use crate::theme::{Palette, blend, contrast, hex};
use egui::Color32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// A desktop theme change shows up in the window within this time.
const POLL: Duration = Duration::from_secs(1);

/// `omarchy-theme-set` writes under `$HOME/.local/state`, whatever `XDG_STATE_HOME` says.
fn colors_file() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local/state/omarchy/current/theme/colors.toml"))
}

pub fn available() -> bool {
    cfg!(target_os = "linux") && colors_file().is_some_and(|f| f.is_file())
}

fn color(text: &str) -> Option<Color32> {
    let digits = text.strip_prefix('#')?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex(digits))
}

/// The colours every Omarchy theme has, also ones written before `colors.toml`
/// gained the extra background and foreground shades.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Colors {
    pub dark: bool,
    pub background: Color32,
    pub foreground: Color32,
    pub accent: Color32,
}

impl Colors {
    /// Reads the flat `key = "value"` lines of `colors.toml`. A theme can come
    /// from a git repository, so anything unexpected means no colours, not a crash.
    pub fn parse(text: &str) -> Option<Colors> {
        let value = |key: &str| {
            text.lines().find_map(|line| {
                let (name, value) = line.split_once('=')?;
                if name.trim() != key {
                    return None;
                }
                value.trim().strip_prefix('"')?.split('"').next()
            })
        };
        let background = color(value("background")?)?;
        let foreground = color(value("foreground")?)?;
        let accent = ["accent", "blue", "color4"]
            .into_iter()
            .find_map(|key| color(value(key)?))
            .unwrap_or(foreground);
        let lightness = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        let dark = match value("mode") {
            Some("light") => false,
            Some("dark") => true,
            _ => lightness(background) < lightness(foreground),
        };
        Some(Colors { dark, background, foreground, accent })
    }

    pub fn palette(self) -> Palette {
        let Colors { dark, background: bg, foreground: fg, accent } = self;
        let toward_text = |t| blend(bg, fg, t);
        let shade = |t| blend(bg, if dark { Color32::BLACK } else { fg }, t);
        let tab_idle = shade(if dark { 0.25 } else { 0.07 });
        // Soft themes leave little room between the text and the background.
        let text_weak = (72..100)
            .map(|percent| toward_text(percent as f32 / 100.0))
            .find(|&c| contrast(c, tab_idle) >= 4.5)
            .unwrap_or(fg);
        let mut palette = Palette {
            dark,
            window: shade(if dark { 0.25 } else { 0.07 }),
            panel: bg,
            inactive: shade(if dark { 0.15 } else { 0.035 }),
            card: if dark { toward_text(0.03) } else { bg },
            raised: if dark { toward_text(0.10) } else { blend(bg, Color32::WHITE, 0.6) },
            border: if dark { toward_text(0.12) } else { toward_text(0.16) },
            text: fg,
            text_weak,
            text_important: accent,
            row_odd: toward_text(0.025),
            hover: toward_text(if dark { 0.09 } else { 0.06 }),
            tab_idle,
            accent,
            glass: false,
            round: false,
        };
        // Selections are tinted with the accent. Some themes use the text colour as
        // accent, which would hide the text, so mute it until it reads.
        for step in 1..=10 {
            if contrast(fg, palette.selection()) >= 4.5 && contrast(fg, palette.cursor(true)) >= 4.0 {
                break;
            }
            palette.accent = blend(accent, bg, step as f32 / 10.0);
        }
        palette
    }
}

/// Hands over the colours now and after every change, until dropped.
pub struct Watch {
    colors: Receiver<Colors>,
    stop: Arc<AtomicBool>,
}

impl Watch {
    pub fn start(ctx: &egui::Context) -> Option<Watch> {
        Some(Watch::of(colors_file()?, POLL, ctx.clone()))
    }

    pub(crate) fn of(file: PathBuf, poll: Duration, ctx: egui::Context) -> Watch {
        let (tx, colors) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let read = |file: &Path| std::fs::read_to_string(file).ok();
        // Read before returning, so the first frame already has the colours.
        let mut seen = read(&file);
        if let Some(c) = seen.as_deref().and_then(Colors::parse) {
            let _ = tx.send(c);
        }
        let stopped = stop.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(poll);
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                // The file is missing for a moment during a switch; keep the old colours.
                let Some(text) = read(&file) else { continue };
                if seen.as_ref() == Some(&text) {
                    continue;
                }
                if let Some(c) = Colors::parse(&text)
                    && tx.send(c).is_ok()
                {
                    ctx.request_repaint();
                }
                seen = Some(text);
            }
        });
        Watch { colors, stop }
    }

    pub fn latest(&self) -> Option<Colors> {
        self.colors.try_iter().last()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_theme() {
        let c = Colors::parse(
            "mode = \"light\"\n\naccent = \"#205EA6\"\nbackground = \"#FFFCF0\"\nforeground = \"#100F0F\"\n",
        )
        .unwrap();
        assert!(!c.dark);
        assert_eq!(c.accent, hex("#205EA6"));
        assert_eq!(c.background, hex("#FFFCF0"));
        assert_eq!(c.foreground, hex("#100F0F"));
    }

    #[test]
    fn an_older_theme_without_mode_or_accent_still_works() {
        let c = Colors::parse("background = \"#1e1e2e\"\nforeground = \"#cdd6f4\"\ncolor4 = \"#89b4fa\"\n").unwrap();
        assert!(c.dark);
        assert_eq!(c.accent, hex("#89b4fa"));
        let c = Colors::parse("background = \"#ffffff\"\nforeground = \"#000000\"\n").unwrap();
        assert!(!c.dark);
        assert_eq!(c.accent, hex("#000000"));
    }

    #[test]
    fn a_broken_theme_gives_no_colours() {
        for bad in [
            "",
            "background = \"#1e1e2e\"",
            "background = \"red\"\nforeground = \"#ffffff\"",
            "background = \"#1e1e2\"\nforeground = \"#ffffff\"",
            "background = \"#żółć1\"\nforeground = \"#ffffff\"",
            "background = #1e1e2e\nforeground = \"#ffffff\"",
        ] {
            assert_eq!(Colors::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn follows_a_changing_file_and_survives_a_gap() {
        let dir = std::env::temp_dir().join(format!("fc-omarchy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("colors.toml");
        let write = |bg: &str| {
            std::fs::write(&file, format!("background = \"{bg}\"\nforeground = \"#ffffff\"\n")).unwrap()
        };
        let wait_for = |watch: &Watch| {
            let until = std::time::Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(c) = watch.latest() {
                    return c.background;
                }
                assert!(std::time::Instant::now() < until, "no change seen");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        write("#111111");
        let watch = Watch::of(file.clone(), Duration::from_millis(10), egui::Context::default());
        assert_eq!(watch.latest().map(|c| c.background), Some(hex("#111111")));

        std::fs::remove_file(&file).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(watch.latest(), None);
        write("#222222");
        assert_eq!(wait_for(&watch), hex("#222222"));

        std::fs::write(&file, "not a theme").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(watch.latest(), None);
        write("#333333");
        assert_eq!(wait_for(&watch), hex("#333333"));

        drop(watch);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
