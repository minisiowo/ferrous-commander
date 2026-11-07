//! Colour themes. Dark is the default;
//! the others use the colours of Omarchy's themes of the same name;
//! Omarchy follows the desktop theme (see omarchy.rs).

use crate::omarchy::Colors;
use egui::{Color32, CornerRadius, Stroke, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum ThemeName {
    #[default]
    Dark,
    // Themes that were removed still have to load from saved settings: an unknown
    // name would make the whole file unreadable.
    #[serde(alias = "Midnight", alias = "Nord")]
    TokyoNight,
    Gruvbox,
    #[serde(alias = "Light")]
    CatppuccinLatte,
    RosePine,
    Omarchy,
}

impl ThemeName {
    pub const ALL: [ThemeName; 5] =
        [ThemeName::Dark, ThemeName::TokyoNight, ThemeName::Gruvbox, ThemeName::CatppuccinLatte, ThemeName::RosePine];

    /// The themes to offer: `ALL`, plus Omarchy on a computer running it.
    pub fn choices() -> Vec<ThemeName> {
        let mut themes = ThemeName::ALL.to_vec();
        if crate::omarchy::available() {
            themes.push(ThemeName::Omarchy);
        }
        themes
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Dark => "Dark",
            ThemeName::TokyoNight => "Tokyo Night",
            ThemeName::Gruvbox => "Gruvbox",
            ThemeName::CatppuccinLatte => "Catppuccin Latte",
            ThemeName::RosePine => "Rosé Pine",
            ThemeName::Omarchy => "Omarchy",
        }
    }

    /// Only Dark uses the chosen accent colour; the others bring their own.
    pub fn has_own_accent(self) -> bool {
        self != ThemeName::Dark
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Window background behind the panels.
    pub window: Color32,
    /// Browser background (the active one in dual view).
    pub panel: Color32,
    /// The other browser in dual view.
    pub inactive: Color32,
    /// Sidebar group cards.
    pub card: Color32,
    /// Slightly raised surfaces (selected tab, headers).
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub text_important: Color32,
    pub row_odd: Color32,
    pub hover: Color32,
    pub tab_idle: Color32,
    pub accent: Color32,
}

pub fn hex(s: &str) -> Color32 {
    let s = s.trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).unwrap_or(0);
    if s.len() == 8 {
        Color32::from_rgba_unmultiplied((v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8)
    } else {
        Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
}

fn preset(dark: bool, background: &str, foreground: &str, accent: &str) -> Palette {
    Colors { dark, background: hex(background), foreground: hex(foreground), accent: hex(accent) }.palette()
}

impl Palette {
    pub fn get(name: ThemeName, accent: Color32) -> Palette {
        match name {
            // Omarchy shows until its colours are read, or when it is not there any more.
            ThemeName::Dark | ThemeName::Omarchy => Palette {
                dark: true,
                window: hex("#202020"),
                panel: hex("#202020"),
                inactive: hex("#181818"),
                card: hex("#242424"),
                raised: hex("#2E2E2E"),
                border: hex("#151515"),
                text: hex("#FFFFFF"),
                text_weak: hex("#9C9C9C"),
                text_important: hex("#0F92FE"),
                row_odd: hex("#222222"),
                hover: hex("#2F2F2F"),
                tab_idle: hex("#202020"),
                accent,
            },
            ThemeName::TokyoNight => preset(true, "#1a1b26", "#a9b1d6", "#7aa2f7"),
            ThemeName::Gruvbox => preset(true, "#282828", "#d4be98", "#7daea3"),
            ThemeName::CatppuccinLatte => preset(false, "#eff1f5", "#4c4f69", "#1e66f5"),
            ThemeName::RosePine => preset(false, "#faf4ed", "#575279", "#56949f"),
        }
    }

    pub fn selection(&self) -> Color32 {
        let a = self.accent;
        if self.dark {
            blend(self.panel, a, 0.45)
        } else {
            blend(self.panel, a, 0.22)
        }
    }

    /// The row the keyboard is on: a lighter tint than a selected row, or a stronger
    /// one on a selected row, so it can be told apart among several selected rows.
    pub fn cursor(&self, selected: bool) -> Color32 {
        if selected {
            blend(self.selection(), self.accent, 0.2)
        } else {
            blend(self.panel, self.accent, if self.dark { 0.22 } else { 0.16 })
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
        let mut v = if self.dark { Visuals::dark() } else { Visuals::light() };
        v.panel_fill = self.window;
        v.window_fill = self.panel;
        v.window_stroke = Stroke::new(1.0, blend(self.panel, self.text_weak, 0.28));
        v.window_corner_radius = CornerRadius::same(2);
        v.menu_corner_radius = CornerRadius::same(6);
        v.window_shadow = egui::epaint::Shadow::NONE;
        v.popup_shadow = egui::epaint::Shadow::NONE;
        v.extreme_bg_color = if self.dark { blend(self.window, self.panel, 0.5) } else { hex("#FFFFFF") };
        v.faint_bg_color = self.row_odd;
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.text_important;
        v.selection.bg_fill = self.selection();
        v.selection.stroke = Stroke::new(1.0, self.text);
        let r = CornerRadius::same(1);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = self.panel;
        w.noninteractive.weak_bg_fill = self.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.text);
        w.noninteractive.corner_radius = r;
        w.inactive.bg_fill = self.raised;
        w.inactive.weak_bg_fill = self.raised;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.inactive.corner_radius = r;
        w.hovered.bg_fill = self.hover;
        w.hovered.weak_bg_fill = self.hover;
        w.hovered.bg_stroke = Stroke::new(1.0, blend(self.border, self.accent, 0.4));
        w.hovered.fg_stroke = Stroke::new(1.5, self.text);
        w.hovered.corner_radius = r;
        w.active.bg_fill = self.selection();
        w.active.weak_bg_fill = self.selection();
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.fg_stroke = Stroke::new(2.0, self.text);
        w.active.corner_radius = r;
        w.open.bg_fill = self.raised;
        w.open.weak_bg_fill = self.raised;
        w.open.corner_radius = r;
        ctx.set_visuals(v);
        ctx.global_style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(6.0, 4.0);
            s.spacing.button_padding = egui::vec2(6.0, 3.0);
            s.spacing.window_margin = egui::Margin::same(14);
            s.spacing.scroll = egui::style::ScrollStyle::thin();
            s.interaction.selectable_labels = false;
            s.animation_time = ANIMATION_MS / 1000.0;
        });
    }
}

pub const ANIMATION_MS: f32 = crate::defaults::ANIMATION_MS;

pub fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// WCAG contrast ratio; 4.5 is the usual minimum for reading text.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let luminance = |c: Color32| {
        let channel = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// How a tab is painted. Only the selected tab of the panel in use carries the
/// accent, like a selected row, so with several panels the place where keys will
/// act stands out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabLook {
    pub fill: Color32,
    pub text: Color32,
}

pub fn tab_look(pal: &Palette, selected: bool, panel_in_use: bool, hovered: bool) -> TabLook {
    match (selected, panel_in_use) {
        (true, true) => TabLook {
            fill: pal.selection(),
            text: pal.text,
        },
        (true, false) => TabLook {
            fill: pal.raised,
            text: blend(pal.text_weak, pal.text, 0.5),
        },
        (false, _) => TabLook {
            fill: if hovered { pal.hover } else { pal.tab_idle },
            text: if hovered { pal.text } else { pal.text_weak },
        },
    }
}

/// Colour tags.
pub const TAGS: [(&str, &str); 7] = [
    ("Red", "#ef7063"),
    ("Orange", "#eda95d"),
    ("Yellow", "#efcf68"),
    ("Green", "#88c866"),
    ("Blue", "#6fb8ef"),
    ("Purple", "#c78edd"),
    ("Gray", "#7c7c7f"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_tabs_work(name: &str, pal: &Palette) {
        let in_use = tab_look(pal, true, true, false);
        let other = tab_look(pal, true, false, false);
        assert_eq!(in_use.fill, pal.selection(), "{name}");
        assert!(contrast(in_use.fill, other.fill) > 1.2, "{name}: panels must differ at a glance");
        for look in [in_use, other, tab_look(pal, false, true, false), tab_look(pal, false, false, true)] {
            assert!(contrast(look.text, look.fill) >= 4.5, "{name}: unreadable tab {look:?}");
        }
    }

    fn assert_rows_work(name: &str, pal: &Palette) {
        for fill in [pal.selection(), pal.cursor(false)] {
            assert!(contrast(pal.text, fill) >= 4.5, "{name}: unreadable row on {fill:?}");
        }
        // One row at a time, on the strongest tint, which soft themes cannot keep at 4.5.
        assert!(contrast(pal.text, pal.cursor(true)) >= 4.0, "{name}: unreadable cursor on a selected row");
        assert!(contrast(pal.cursor(false), pal.panel) > 1.15, "{name}: keyboard cursor is hard to see");
        assert_ne!(pal.cursor(false), pal.hover, "{name}");
        assert_ne!(pal.cursor(true), pal.selection(), "{name}");
    }

    #[test]
    fn two_dark_and_two_light_themes_besides_the_default() {
        let dark = |name| Palette::get(name, hex(crate::defaults::ACCENT_RGB)).dark;
        assert_eq!(ThemeName::default(), ThemeName::Dark);
        assert_eq!(ThemeName::ALL.iter().filter(|&&name| dark(name)).count(), 3);
        assert_eq!(ThemeName::ALL.iter().filter(|&&name| !dark(name)).count(), 2);
    }

    #[test]
    fn only_the_panel_in_use_shows_an_accented_tab() {
        for name in ThemeName::ALL {
            let pal = Palette::get(name, hex(crate::defaults::ACCENT_RGB));
            assert_tabs_work(name.label(), &pal);
            assert_rows_work(name.label(), &pal);
        }
    }

    /// Omarchy's darkest, lightest and greyest themes. A computer with Omarchy
    /// also checks every theme it has installed.
    #[test]
    fn omarchy_themes_stay_readable() {
        let sample = |mode, background, foreground, accent| {
            format!("mode = \"{mode}\"\naccent = \"{accent}\"\nbackground = \"{background}\"\nforeground = \"{foreground}\"\n")
        };
        let mut themes = vec![
            ("vantablack".to_string(), sample("dark", "#000000", "#ffffff", "#8d8d8d")),
            ("last-horizon".to_string(), sample("dark", "#0c0b0c", "#FAFCFB", "#b59790")),
            ("kanagawa".to_string(), sample("dark", "#1f1f28", "#dcd7ba", "#dcd7ba")),
            ("white".to_string(), sample("light", "#ffffff", "#000000", "#6e6e6e")),
            ("flexoki-light".to_string(), sample("light", "#FFFCF0", "#100F0F", "#205EA6")),
            ("rose-pine".to_string(), sample("light", "#faf4ed", "#575279", "#56949f")),
        ];
        let home = dirs::home_dir().unwrap_or_default();
        for root in [std::path::PathBuf::from("/usr/share/omarchy/themes"), home.join(".config/omarchy/themes")] {
            for theme in std::fs::read_dir(root).into_iter().flatten().flatten() {
                if let Ok(text) = std::fs::read_to_string(theme.path().join("colors.toml")) {
                    themes.push((theme.file_name().to_string_lossy().into_owned(), text));
                }
            }
        }
        for (name, text) in themes {
            let colors = crate::omarchy::Colors::parse(&text).unwrap_or_else(|| panic!("{name}: unreadable"));
            let pal = colors.palette();
            assert_tabs_work(&name, &pal);
            assert_rows_work(&name, &pal);
        }
    }
}
