//! Colour themes. Dark is the default;
//! the others use the colours of Omarchy's themes of the same name;
//! Omarchy follows the desktop theme (see omarchy.rs). The person's own themes
//! are files (see user_theme.rs).

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

/// The theme in use: one built in, or one of the person's own by the name of its file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ThemeId {
    Builtin(ThemeName),
    Custom(String),
}

impl Default for ThemeId {
    fn default() -> Self {
        ThemeId::Builtin(ThemeName::default())
    }
}

impl From<ThemeName> for ThemeId {
    fn from(name: ThemeName) -> Self {
        ThemeId::Builtin(name)
    }
}

const CUSTOM_PREFIX: &str = "custom:";

impl ThemeId {
    /// Built-in themes keep the names saved before own themes existed.
    pub fn key(&self) -> String {
        match self {
            ThemeId::Builtin(name) => format!("{name:?}"),
            ThemeId::Custom(slug) => format!("{CUSTOM_PREFIX}{slug}"),
        }
    }

    pub fn builtin(&self) -> Option<ThemeName> {
        match self {
            ThemeId::Builtin(name) => Some(*name),
            ThemeId::Custom(_) => None,
        }
    }

    pub fn custom(&self) -> Option<&str> {
        match self {
            ThemeId::Builtin(_) => None,
            ThemeId::Custom(slug) => Some(slug),
        }
    }

    /// Only Dark uses the chosen accent colour; the others bring their own.
    pub fn has_own_accent(&self) -> bool {
        self.builtin().is_none_or(ThemeName::has_own_accent)
    }
}

impl Serialize for ThemeId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.key())
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::IntoDeserializer;
        let text = String::deserialize(d)?;
        if let Some(slug) = text.strip_prefix(CUSTOM_PREFIX) {
            return Ok(ThemeId::Custom(slug.to_string()));
        }
        let name: Result<ThemeName, serde::de::value::Error> = ThemeName::deserialize(text.as_str().into_deserializer());
        // A theme that is gone must not make the whole settings file unreadable.
        Ok(ThemeId::Builtin(name.unwrap_or_default()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// The desktop's own material is behind the main window, and its surfaces let it through.
    pub glass: bool,
    /// Corners are rounded instead of square.
    pub round: bool,
    /// Blocks have a thin line around them.
    pub outlined: bool,
}

/// `radius` on the two upper corners only: a tab, or the bar at the top of a window.
pub fn top(radius: CornerRadius) -> CornerRadius {
    CornerRadius { sw: 0, se: 0, ..radius }
}

/// `radius` on the two lower corners only: the strip at the bottom of a window.
pub fn bottom(radius: CornerRadius) -> CornerRadius {
    CornerRadius { nw: 0, ne: 0, ..radius }
}

/// The corners of what lies just inside a line one point wide around `radius`.
pub fn inside(radius: CornerRadius) -> CornerRadius {
    let less = |r: u8| r.saturating_sub(1);
    CornerRadius { nw: less(radius.nw), ne: less(radius.ne), sw: less(radius.sw), se: less(radius.se) }
}

/// How much of a surface's own colour stays over the desktop's material.
const GLASS_SURFACE: f32 = 0.45;

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
                // Darkest behind, so that every block stands out from it: the panel in use
                // most, the others less, as in the themes made from two colours.
                window: hex("#161616"),
                panel: hex("#202020"),
                inactive: hex("#1B1B1B"),
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
                glass: false,
                round: false,
                outlined: true,
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

    /// Behind everything in the main window.
    pub fn backdrop(&self) -> Color32 {
        if self.glass { Color32::TRANSPARENT } else { self.window }
    }

    /// A large surface of the main window, such as a panel or a sidebar card.
    pub fn surface(&self, color: Color32) -> Color32 {
        if self.glass { color.gamma_multiply(GLASS_SURFACE) } else { color }
    }

    /// Every second row of a list. On glass a second coat of the row colour would
    /// show as a band, so the row gets a trace of the text colour.
    pub fn stripe(&self) -> Color32 {
        if self.glass { self.text.gamma_multiply(0.025) } else { self.row_odd }
    }

    /// Behind the content of a window, a shade away from its title bar.
    pub fn content_fill(&self) -> Color32 {
        blend(self.window, self.inactive, 0.5)
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

    /// The thin line around every block: a panel, a card of the sidebar. A theme whose
    /// background is nearly black leaves no room for shades to tell its blocks apart.
    pub fn outline(&self) -> Stroke {
        if self.outlined { Stroke::new(1.0, blend(self.panel, self.text, 0.12)) } else { Stroke::NONE }
    }

    /// The corners of small things: buttons, fields, highlights, the rows of a card.
    pub fn corner(&self) -> CornerRadius {
        CornerRadius::same(if self.round { 4 } else { 0 })
    }

    /// The corners of thin bars, which a larger radius would turn into dots.
    pub fn corner_small(&self) -> CornerRadius {
        CornerRadius::same(if self.round { 2 } else { 0 })
    }

    /// The corners of something that lies `gap` inside the edge of a surface and follows
    /// its corner. With any other radius the two curves come closer at the corner than
    /// along the sides.
    pub fn corner_within(&self, gap: f32) -> CornerRadius {
        let outer = f32::from(self.corner_large().nw);
        CornerRadius::same((outer - gap).max(0.0).round() as u8)
    }

    /// The corners of surfaces: windows, menus, cards.
    pub fn corner_large(&self) -> CornerRadius {
        CornerRadius::same(if self.round { 8 } else { 0 })
    }

    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
        let mut v = if self.dark { Visuals::dark() } else { Visuals::light() };
        v.panel_fill = self.window;
        v.window_fill = self.panel;
        v.window_stroke = Stroke::new(1.0, blend(self.panel, self.text_weak, 0.28));
        v.window_corner_radius = self.corner_large();
        v.menu_corner_radius = self.corner_large();
        v.window_shadow = egui::epaint::Shadow::NONE;
        v.popup_shadow = egui::epaint::Shadow::NONE;
        v.extreme_bg_color = if self.dark { blend(self.window, self.panel, 0.5) } else { hex("#FFFFFF") };
        v.faint_bg_color = self.row_odd;
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.text_important;
        v.selection.bg_fill = self.selection();
        v.selection.stroke = Stroke::new(1.0, self.text);
        let r = self.corner();
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

/// What makes a palette hard to use, in words for the person making a theme.
/// Empty when everything reads. The tests hold every built-in theme to this.
pub fn readability_issues(pal: &Palette) -> Vec<&'static str> {
    let mut issues = Vec::new();
    let mut check = |ok: bool, issue| {
        if !ok {
            issues.push(issue);
        }
    };
    check(contrast(pal.text, pal.panel) >= 4.5, "Text is hard to read on the panels");
    check(contrast(pal.text, pal.row_odd) >= 4.5, "Text is hard to read on every second row");
    let in_use = tab_look(pal, true, true, false);
    let other = tab_look(pal, true, false, false);
    check(contrast(in_use.fill, other.fill) > 1.2, "The panel in use does not stand out from the others");
    let tabs = [in_use, other, tab_look(pal, false, true, false), tab_look(pal, false, false, true)];
    check(tabs.iter().all(|look| contrast(look.text, look.fill) >= 4.5), "Tab names are hard to read");
    check(
        [pal.selection(), pal.cursor(false)].iter().all(|&fill| contrast(pal.text, fill) >= 4.5),
        "Selected rows are hard to read",
    );
    // One row at a time, on the strongest tint, which soft themes cannot keep at 4.5.
    check(contrast(pal.text, pal.cursor(true)) >= 4.0, "The keyboard cursor on a selected row is hard to read");
    check(contrast(pal.cursor(false), pal.panel) > 1.15, "The keyboard cursor is hard to see");
    // Small print in a window; soft themes cannot keep 4.5 here either.
    check(contrast(pal.text_weak, pal.content_fill()) >= 4.0, "Small print in windows is hard to read");
    issues
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
            // On glass the panel under the tab is thinned already.
            fill: if hovered { pal.hover } else if pal.glass { Color32::TRANSPARENT } else { pal.tab_idle },
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

    fn assert_readable(name: &str, pal: &Palette) {
        assert_eq!(tab_look(pal, true, true, false).fill, pal.selection(), "{name}");
        assert_ne!(pal.cursor(false), pal.hover, "{name}");
        assert_ne!(pal.cursor(true), pal.selection(), "{name}");
        assert_eq!(readability_issues(pal), Vec::<&str>::new(), "{name}");
    }

    #[test]
    fn an_unreadable_palette_is_named_as_such() {
        let good = Palette::get(ThemeName::Gruvbox, Color32::WHITE);
        let bad = Palette { text: good.panel, ..good };
        assert!(readability_issues(&bad).contains(&"Text is hard to read on the panels"));
        let flat = Palette { inactive: good.panel, raised: good.panel, accent: good.panel, ..good };
        assert!(!readability_issues(&flat).is_empty());
    }

    #[test]
    fn theme_ids_keep_the_saved_names_and_survive_a_missing_theme() {
        let read = |text: &str| serde_json::from_value::<ThemeId>(serde_json::Value::String(text.into())).unwrap();
        assert_eq!(read("Gruvbox"), ThemeId::Builtin(ThemeName::Gruvbox));
        assert_eq!(read("Nord"), ThemeId::Builtin(ThemeName::TokyoNight));
        assert_eq!(read("Light"), ThemeId::Builtin(ThemeName::CatppuccinLatte));
        assert_eq!(read("Removed"), ThemeId::default());
        assert_eq!(read("custom:my-night"), ThemeId::Custom("my-night".into()));
        for id in [ThemeId::Builtin(ThemeName::RosePine), ThemeId::Custom("zz".into())] {
            assert_eq!(read(serde_json::to_value(&id).unwrap().as_str().unwrap()), id);
        }
        assert!(ThemeId::Custom("x".into()).has_own_accent());
        assert!(!ThemeId::default().has_own_accent());
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
            assert_readable(name.label(), &pal);
        }
    }

    #[test]
    fn corners_are_square_unless_asked_to_be_round_and_then_only_a_little() {
        let square = Palette::get(ThemeName::Dark, hex(crate::defaults::ACCENT_RGB));
        assert!(!square.round, "a theme does not choose the corners");
        assert_eq!((square.corner(), square.corner_large()), (CornerRadius::ZERO, CornerRadius::ZERO));
        let round = Palette { round: true, ..square };
        assert!(round.corner().nw > 0 && round.corner().nw < round.corner_large().nw && round.corner_large().nw <= 8);
        let ctx = egui::Context::default();
        for (pal, small, large) in [(square, square.corner(), square.corner_large()), (round, round.corner(), round.corner_large())] {
            pal.apply(&ctx);
            let v = ctx.global_style().visuals.clone();
            assert_eq!((v.widgets.inactive.corner_radius, v.widgets.hovered.corner_radius), (small, small));
            assert_eq!((v.window_corner_radius, v.menu_corner_radius), (large, large));
        }
        assert!(square.outlined && square.outline().width == 1.0, "blocks are outlined unless turned off");
        assert_eq!(Palette { outlined: false, ..square }.outline(), Stroke::NONE);
        assert_eq!(top(round.corner_large()), CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 });
        assert_eq!(bottom(inside(round.corner_large())), CornerRadius { nw: 0, ne: 0, sw: 7, se: 7 });
    }

    #[test]
    fn glass_thins_the_main_window_and_nothing_else() {
        for name in ThemeName::ALL {
            let solid = Palette::get(name, hex(crate::defaults::ACCENT_RGB));
            assert_eq!((solid.backdrop(), solid.surface(solid.panel), solid.stripe()), (solid.window, solid.panel, solid.row_odd));
            let glass = Palette { glass: true, ..solid };
            assert_eq!(glass.backdrop(), Color32::TRANSPARENT);
            for surface in [glass.surface(glass.panel), glass.surface(glass.card), glass.stripe(), tab_look(&glass, false, true, false).fill] {
                assert!(!surface.is_opaque(), "{}: the desktop cannot show through {surface:?}", name.label());
            }
            // Dialogs are windows of their own with nothing behind them.
            assert!(glass.content_fill().is_opaque() && glass.selection().is_opaque());
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
            assert_readable(&name, &pal);
        }
    }
}
