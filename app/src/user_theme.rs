//! The person's own themes, one file each in the `themes` folder next to the settings.
//! A file is Omarchy's `colors.toml` (mode, background, foreground, accent) with a
//! name and, optionally, colours that replace single shades of the palette:
//!
//! ```toml
//! name = "My night"
//! mode = "dark"
//! background = "#1a1b26"
//! foreground = "#a9b1d6"
//! accent = "#7aa2f7"
//! panel = "#202233"
//! ```
//!
//! Theme files are shared and downloaded, so anything unexpected in one is skipped,
//! never a crash.

use std::path::{Path, PathBuf};

use egui::Color32;

use crate::omarchy::{Colors, color, toml_value};
use crate::theme::Palette;

/// Larger than any theme; a bigger file is something else.
pub const MAX_FILE: u64 = 64 * 1024;
pub const EXTENSION: &str = "toml";

/// The shades a theme can set by hand, by their key in the file and what they colour.
/// The text and accent are the theme's own foreground and accent.
pub const FIELDS: [(&str, &str); 11] = [
    ("window", "Window background"),
    ("panel", "Panel in use"),
    ("inactive", "Other panels"),
    ("card", "Sidebar cards"),
    ("raised", "Raised surfaces"),
    ("border", "Borders"),
    ("text_weak", "Secondary text"),
    ("text_important", "Links"),
    ("row_odd", "Every second row"),
    ("hover", "Hover"),
    ("tab_idle", "Idle tabs"),
];

/// The colour in `pal` that field `index` of `FIELDS` names.
pub fn slot(pal: &mut Palette, index: usize) -> &mut Color32 {
    match index {
        0 => &mut pal.window,
        1 => &mut pal.panel,
        2 => &mut pal.inactive,
        3 => &mut pal.card,
        4 => &mut pal.raised,
        5 => &mut pal.border,
        6 => &mut pal.text_weak,
        7 => &mut pal.text_important,
        8 => &mut pal.row_odd,
        9 => &mut pal.hover,
        _ => &mut pal.tab_idle,
    }
}

pub type Overrides = [Option<Color32>; FIELDS.len()];

#[derive(Clone, PartialEq, Debug)]
pub struct UserTheme {
    /// The file's name without `.toml`; settings name the theme by it.
    pub slug: String,
    pub name: String,
    pub colors: Colors,
    pub overrides: Overrides,
}

fn hex_text(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

impl UserTheme {
    pub fn parse(slug: &str, text: &str) -> Option<UserTheme> {
        let colors = Colors::parse(text)?;
        let mut overrides: Overrides = [None; FIELDS.len()];
        for (i, (key, _)) in FIELDS.iter().enumerate() {
            overrides[i] = toml_value(text, key).and_then(color);
        }
        let name = toml_value(text, "name").map(clean_name).filter(|n| !n.is_empty()).unwrap_or_else(|| title_of(slug));
        Some(UserTheme { slug: slug.to_string(), name, colors, overrides })
    }

    pub fn to_toml(&self) -> String {
        let c = self.colors;
        let mut text = format!(
            "# Ferrous Commander theme\nname = \"{}\"\nmode = \"{}\"\nbackground = \"{}\"\nforeground = \"{}\"\naccent = \"{}\"\n",
            clean_name(&self.name),
            if c.dark { "dark" } else { "light" },
            hex_text(c.background),
            hex_text(c.foreground),
            hex_text(c.accent),
        );
        for ((key, _), value) in FIELDS.iter().zip(self.overrides) {
            if let Some(value) = value {
                text.push_str(&format!("{key} = \"{}\"\n", hex_text(value)));
            }
        }
        text
    }

    /// The palette from the three colours, without the shades set by hand.
    pub fn derived(&self) -> Palette {
        self.colors.palette()
    }

    pub fn palette(&self) -> Palette {
        let mut pal = self.derived();
        for (i, value) in self.overrides.iter().enumerate() {
            if let Some(value) = value {
                *slot(&mut pal, i) = *value;
            }
        }
        pal
    }

    /// A theme that looks like `pal`: its panel, text and accent as the three colours,
    /// and every shade they would not give by themselves set by hand.
    pub fn from_palette(slug: &str, name: &str, pal: &Palette) -> UserTheme {
        let colors = Colors { dark: pal.dark, background: pal.panel, foreground: pal.text, accent: pal.accent };
        let mut theme = UserTheme { slug: slug.to_string(), name: name.to_string(), colors, overrides: [None; FIELDS.len()] };
        let mut derived = theme.derived();
        let mut wanted = *pal;
        for i in 0..FIELDS.len() {
            let want = *slot(&mut wanted, i);
            if *slot(&mut derived, i) != want {
                theme.overrides[i] = Some(want);
            }
        }
        theme
    }
}

/// One line, without the quote that would end the value in the file.
fn clean_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control() && *c != '"').collect::<String>().trim().to_string()
}

/// `my-night` → `My night`.
fn title_of(slug: &str) -> String {
    let words = slug.replace(['-', '_'], " ");
    let mut chars = words.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Theme".to_string(),
    }
}

/// A file name for a theme called `name`: lower-case ASCII letters, digits and dashes.
pub fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'á' | 'à' | 'â' | 'ä' => 'a',
            'ö' | 'ô' => 'o',
            'ü' | 'û' | 'ú' => 'u',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "theme".to_string() } else { slug.chars().take(60).collect() }
}

/// `slug_of(name)`, or with `-2`, `-3`… when that is taken.
pub fn unique_slug(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = slug_of(name);
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|s| !taken(s)).unwrap_or(base)
}

pub fn themes_dir() -> Option<PathBuf> {
    Some(crate::settings::config_directory()?.join("themes"))
}

pub fn file_in(dir: &Path, slug: &str) -> PathBuf {
    dir.join(format!("{slug}.{EXTENSION}"))
}

/// The text of a theme file, if it is not too large to be one.
fn read_small(file: &Path) -> Result<String, String> {
    let size = std::fs::metadata(file).map_err(|e| crate::fsx::friendly_err(&e))?.len();
    if size > MAX_FILE {
        return Err("too large to be a theme".to_string());
    }
    std::fs::read_to_string(file).map_err(|e| crate::fsx::friendly_err(&e))
}

/// Every theme in `dir`, by name, and the files that are not themes.
pub fn load_dir(dir: &Path) -> (Vec<UserTheme>, Vec<String>) {
    let mut themes = Vec::new();
    let mut problems = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(EXTENSION) {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        match read_small(&path).and_then(|text| UserTheme::parse(slug, &text).ok_or_else(|| "not a theme".to_string())) {
            Ok(theme) => themes.push(theme),
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    themes.sort_by_cached_key(|t| (t.name.to_lowercase(), t.slug.clone()));
    (themes, problems)
}

/// What can be told about the theme files without reading them: names, sizes and
/// times of change. A missing folder holds no themes.
fn folder_state(dir: &Path) -> Vec<(std::ffi::OsString, u64, Option<std::time::SystemTime>)> {
    let mut state: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(EXTENSION))
        .map(|e| {
            let md = e.metadata().ok();
            (e.file_name(), md.as_ref().map_or(0, |m| m.len()), md.and_then(|m| m.modified().ok()))
        })
        .collect();
    state.sort();
    state
}

/// The themes in `dir` now and after every change made to them, by the app or outside it:
/// a file added, changed, renamed or deleted.
pub fn watch_dir(dir: PathBuf, poll: std::time::Duration, ctx: egui::Context) -> crate::omarchy::Watch<(Vec<UserTheme>, Vec<String>)> {
    let listed = dir.clone();
    crate::omarchy::Watch::polling(poll, ctx, move || Some(folder_state(&listed)), move |_| Some(load_dir(&dir)))
}

pub fn save_in(dir: &Path, theme: &UserTheme) -> std::io::Result<PathBuf> {
    let file = file_in(dir, &theme.slug);
    crate::fsx::write_atomic(&file, theme.to_toml().as_bytes(), false)?;
    Ok(file)
}

/// Reads a theme file from anywhere and keeps a copy in `dir` under a name of its own.
/// Omarchy keeps each theme's `colors.toml` in a folder named after the theme.
pub fn import_into(dir: &Path, file: &Path) -> Result<UserTheme, String> {
    let text = read_small(file)?;
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("theme");
    let named = if stem == "colors" {
        file.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or(stem)
    } else {
        stem
    };
    let mut theme = UserTheme::parse(&slug_of(named), &text)
        .ok_or_else(|| "it has no background and foreground colour".to_string())?;
    theme.slug = unique_slug(&theme.name, |s| file_in(dir, s).exists());
    save_in(dir, &theme).map_err(|e| crate::fsx::friendly_err(&e))?;
    Ok(theme)
}

/// Writes the theme into `dir` without replacing a file that is there.
pub fn export_into(dir: &Path, theme: &UserTheme) -> std::io::Result<PathBuf> {
    let name = format!("{}.{EXTENSION}", theme.slug);
    let file = crate::fsx::unique_os_name(dir, name.as_ref(), false);
    crate::fsx::write_atomic(&file, theme.to_toml().as_bytes(), false)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ThemeName, hex};

    fn sample() -> UserTheme {
        UserTheme::parse(
            "night",
            "name = \"Moja noc\"\nmode = \"dark\"\nbackground = \"#1a1b26\"\nforeground = \"#a9b1d6\"\naccent = \"#7aa2f7\"\n",
        )
        .unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fc-user-theme-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_theme_reads_back_as_it_was_written() {
        let mut theme = sample();
        assert_eq!(theme.name, "Moja noc");
        assert_eq!(UserTheme::parse("night", &theme.to_toml()), Some(theme.clone()));
        theme.overrides[1] = Some(hex("#202233"));
        theme.overrides[6] = Some(hex("#8890b0"));
        theme.name = "With \"quotes\"\nand a line".into();
        let back = UserTheme::parse("night", &theme.to_toml()).unwrap();
        assert_eq!(back.overrides, theme.overrides);
        assert_eq!(back.name, "With quotesand a line");
    }

    #[test]
    fn a_shade_set_by_hand_changes_only_itself() {
        let mut theme = sample();
        let plain = theme.palette();
        theme.overrides[1] = Some(hex("#123456"));
        let changed = theme.palette();
        assert_eq!(changed.panel, hex("#123456"));
        assert_eq!(Palette { panel: plain.panel, ..changed }, plain);
    }

    #[test]
    fn a_broken_shade_is_skipped_and_a_broken_theme_is_not_one() {
        let theme = UserTheme::parse("x", "background = \"#000000\"\nforeground = \"#ffffff\"\npanel = \"red\"\n").unwrap();
        assert_eq!(theme.overrides, [None; FIELDS.len()]);
        assert_eq!(theme.name, "X");
        assert_eq!(UserTheme::parse("x", "foreground = \"#ffffff\"\npanel = \"#000000\"\n"), None);

        let dir = temp_dir("load");
        std::fs::write(dir.join("good.toml"), sample().to_toml()).unwrap();
        std::fs::write(dir.join("bad.toml"), "nothing").unwrap();
        std::fs::write(dir.join("huge.toml"), "#".repeat(MAX_FILE as usize + 1)).unwrap();
        std::fs::write(dir.join("notes.txt"), "not looked at").unwrap();
        let (themes, problems) = load_dir(&dir);
        assert_eq!(themes.iter().map(|t| t.slug.as_str()).collect::<Vec<_>>(), ["good"]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_copy_of_a_built_in_theme_looks_exactly_like_it() {
        let accent = hex(crate::defaults::ACCENT_RGB);
        for name in ThemeName::ALL {
            let pal = Palette::get(name, accent);
            let copy = UserTheme::from_palette("copy", name.label(), &pal);
            assert_eq!(copy.palette(), pal, "{}", name.label());
            let back = UserTheme::parse("copy", &copy.to_toml()).unwrap();
            assert_eq!(back.palette(), pal, "{}: after saving", name.label());
        }
    }

    #[test]
    fn file_names_are_plain_and_never_taken() {
        assert_eq!(slug_of("Zażółć gęślą jaźń!"), "zazolc-gesla-jazn");
        assert_eq!(slug_of("  Tokyo   Night  "), "tokyo-night");
        assert_eq!(slug_of("../../etc"), "etc");
        assert_eq!(slug_of("★★★"), "theme");
        let taken = ["night", "night-2"];
        assert_eq!(unique_slug("Night", |s| taken.contains(&s)), "night-3");
        assert_eq!(unique_slug("Day", |s| taken.contains(&s)), "day");
    }

    #[test]
    fn an_omarchy_theme_imports_under_its_folder_name() {
        let dir = temp_dir("import");
        let themes = dir.join("themes");
        let source = dir.join("kanagawa");
        std::fs::create_dir_all(&source).unwrap();
        let file = source.join("colors.toml");
        std::fs::write(&file, "accent = \"#dcd7ba\"\nbackground = \"#1f1f28\"\nforeground = \"#dcd7ba\"\ncolor1 = \"#c34043\"\n").unwrap();
        let first = import_into(&themes, &file).unwrap();
        assert_eq!((first.slug.as_str(), first.name.as_str()), ("kanagawa", "Kanagawa"));
        let second = import_into(&themes, &file).unwrap();
        assert_eq!(second.slug, "kanagawa-2", "an import never replaces a theme");
        assert!(import_into(&themes, &dir.join("missing.toml")).is_err());
        std::fs::write(source.join("plain.toml"), "x = 1").unwrap();
        assert!(import_into(&themes, &source.join("plain.toml")).is_err());

        let out = export_into(&source, &first).unwrap();
        let again = export_into(&source, &first).unwrap();
        assert_ne!(out, again, "an export never replaces a file");
        assert_eq!(UserTheme::parse("kanagawa", &std::fs::read_to_string(&again).unwrap()).unwrap().palette(), first.palette());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
