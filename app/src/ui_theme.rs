//! The person's own themes in the app: choosing, following their files, and the
//! editor that makes, changes, exports and deletes them.

use std::path::Path;

use egui::{Align2, Color32, FontId, RichText, Sense, vec2};

use crate::app::{App, ToastKind, is_computer};
use crate::chrome::{self, Icon, Panel};
use crate::omarchy::Colors;
use crate::theme::{Palette, ThemeId, ThemeName, hex, readability_issues, tab_look};
use crate::user_theme::{self as files, FIELDS, UserTheme};

/// A theme being made or changed. The window shows it while it is open; nothing is
/// saved or chosen until Save.
pub struct ThemeEditor {
    pub draft: UserTheme,
    /// The file being changed; none for a theme not saved yet.
    pub original: Option<String>,
    confirm_delete: bool,
}

impl ThemeEditor {
    pub fn new(draft: UserTheme, original: Option<String>) -> ThemeEditor {
        ThemeEditor { draft, original, confirm_delete: false }
    }
}

impl App {
    /// Every theme to offer, with its name: the built-in ones, then the person's own.
    pub fn theme_choices(&self) -> Vec<(ThemeId, String)> {
        let builtin = ThemeName::choices().into_iter().map(|name| (ThemeId::Builtin(name), name.label().to_string()));
        let own = self.user_themes.iter().map(|t| (ThemeId::Custom(t.slug.clone()), t.name.clone()));
        builtin.chain(own).collect()
    }

    pub fn theme_label(&self, id: &ThemeId) -> String {
        match id {
            ThemeId::Builtin(name) => name.label().to_string(),
            ThemeId::Custom(slug) => self.user_themes.iter().find(|t| t.slug == *slug).map_or_else(|| slug.clone(), |t| t.name.clone()),
        }
    }

    pub fn reload_user_themes(&mut self) {
        let Some(dir) = &self.themes_dir else { return };
        let loaded = files::load_dir(dir);
        self.take_user_themes(loaded);
    }

    fn take_user_themes(&mut self, (themes, problems): (Vec<UserTheme>, Vec<String>)) {
        self.user_themes = themes;
        // Each broken file is told once, not again at every save.
        for problem in problems {
            if !self.theme_problems_told.contains(&problem) {
                self.toast(ToastKind::Error, format!("Not a theme: {problem}"));
                self.theme_problems_told.push(problem);
            }
        }
    }

    /// The own theme chosen in the settings, if its file is there.
    pub(crate) fn chosen_user_theme(&mut self) -> Option<UserTheme> {
        let slug = self.st.theme.custom()?.to_string();
        let found = self.user_themes.iter().find(|t| t.slug == slug).cloned();
        if found.is_none() && self.theme_warned.as_deref() != Some(slug.as_str()) {
            self.toast(ToastKind::Error, format!("The theme \"{slug}\" is missing, so the Dark theme is in use"));
            self.theme_warned = Some(slug);
        }
        found
    }

    /// Theme files added, changed or deleted outside the app show within a second.
    pub(crate) fn follow_user_themes(&mut self, ctx: &egui::Context) {
        if self.themes_watch.is_none()
            && let Some(dir) = self.themes_dir.clone()
        {
            self.themes_watch = Some(files::watch_dir(dir, crate::omarchy::POLL, ctx.clone()));
        }
        if let Some(loaded) = self.themes_watch.as_ref().and_then(|watch| watch.latest()) {
            self.take_user_themes(loaded);
        }
    }

    /// Opens the editor on a new theme with the colours of the one in use.
    /// Opens the editor on a new theme that starts as an exact copy of the one in use,
    /// built in or not: a theme is usually made from one that is liked already.
    pub fn new_theme(&mut self) {
        let name = format!("{} copy", self.theme_label(&self.st.theme));
        let draft = match self.chosen_user_theme() {
            Some(theme) => UserTheme { name, slug: String::new(), ..theme },
            None => UserTheme::from_palette("", &name, &self.pal),
        };
        self.theme_editor = Some(ThemeEditor::new(draft, None));
    }

    /// Opens the editor on the own theme in use; a built-in one is copied first.
    pub fn edit_theme(&mut self) {
        match self.chosen_user_theme() {
            Some(theme) => {
                let slug = theme.slug.clone();
                self.theme_editor = Some(ThemeEditor::new(theme, Some(slug)));
            }
            None => self.new_theme(),
        }
    }

    /// The list behind the toolbar's theme button: the built-in themes, the person's
    /// own below a line, and ways to make or change one.
    pub fn theme_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(190.0);
        let choices = self.theme_choices();
        let first_own = choices.iter().position(|(id, _)| id.custom().is_some());
        for (k, (id, label)) in choices.into_iter().enumerate() {
            if Some(k) == first_own {
                ui.separator();
            }
            let chosen = id == self.st.theme;
            if ui.selectable_label(chosen, label).clicked() {
                if !chosen {
                    self.st.theme = id;
                    self.mark_dirty();
                }
                ui.close();
            }
        }
        ui.separator();
        if ui.button("+ New theme").on_hover_text("Start a theme of your own from a copy of this one").clicked() {
            self.new_theme();
            ui.close();
        }
        if self.st.theme.custom().is_some() && ui.button("Edit current theme").clicked() {
            self.edit_theme();
            ui.close();
        }
    }

    /// The one `.toml` file selected in the active panel, which could be a theme.
    pub fn selected_theme_file(&self) -> Option<std::path::PathBuf> {
        let [file] = <[_; 1]>::try_from(self.selection(self.active)).ok()?;
        is_theme_file(&file).then_some(file)
    }

    pub fn import_theme(&mut self, file: &Path) {
        let Some(dir) = self.themes_dir.clone() else {
            return self.toast(ToastKind::Error, "Themes cannot be kept: no configuration folder");
        };
        match files::import_into(&dir, file) {
            Ok(theme) => {
                self.toast(ToastKind::Success, format!("Theme \"{}\" imported and in use", theme.name));
                self.st.theme = ThemeId::Custom(theme.slug);
                self.reload_user_themes();
                self.mark_dirty();
            }
            Err(e) => self.toast(ToastKind::Error, format!("{} cannot be imported as a theme: {e}", file.display())),
        }
    }

    pub fn show_themes_folder(&mut self) {
        let Some(dir) = self.themes_dir.clone() else { return };
        let made = std::fs::create_dir_all(&dir).map(|()| dir);
        self.show_configuration_folder(made);
    }

    fn save_theme(&mut self, editor: &mut ThemeEditor) -> bool {
        let Some(dir) = self.themes_dir.clone() else {
            self.toast(ToastKind::Error, "Themes cannot be kept: no configuration folder");
            return false;
        };
        let mut theme = editor.draft.clone();
        if theme.name.trim().is_empty() {
            theme.name = "Theme".into();
        }
        // A new name does not move the file: the settings name the theme by it.
        theme.slug = match &editor.original {
            Some(slug) => slug.clone(),
            None => files::unique_slug(&theme.name, |s| files::file_in(&dir, s).exists()),
        };
        match files::save_in(&dir, &theme) {
            Ok(_) => {
                self.st.theme = ThemeId::Custom(theme.slug.clone());
                self.reload_user_themes();
                self.mark_dirty();
                self.toast(ToastKind::Success, format!("Theme \"{}\" saved", theme.name));
                true
            }
            Err(e) => {
                self.toast(ToastKind::Error, format!("The theme could not be saved: {}", crate::fsx::friendly_err(&e)));
                false
            }
        }
    }

    fn delete_theme(&mut self, slug: &str) -> bool {
        let Some(dir) = self.themes_dir.clone() else { return false };
        let file = files::file_in(&dir, slug);
        if let Err(e) = trash::delete(&file) {
            self.toast(ToastKind::Error, format!("The theme could not be moved to the trash: {e}"));
            return false;
        }
        if self.st.theme.custom() == Some(slug) {
            self.st.theme = ThemeId::default();
            self.mark_dirty();
        }
        self.reload_user_themes();
        self.toast(ToastKind::Success, "Theme moved to the trash");
        true
    }

    fn export_theme(&mut self, theme: &UserTheme) {
        let folder = self.panes[self.active].path().to_path_buf();
        if is_computer(&folder) {
            return self.toast(ToastKind::Error, "Open a folder in the active panel to export the theme into");
        }
        let mut theme = theme.clone();
        if theme.slug.is_empty() {
            theme.slug = files::slug_of(&theme.name);
        }
        match files::export_into(&folder, &theme) {
            Ok(file) => self.toast(ToastKind::Success, format!("Theme exported to {}", file.display())),
            Err(e) => self.toast(ToastKind::Error, format!("The theme could not be exported: {}", crate::fsx::friendly_err(&e))),
        }
    }

    pub fn theme_editor_window(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.theme_editor.take() else { return };
        let pal = self.pal;
        let mut done = false;
        let mut save = false;
        let mut export = false;
        let mut delete = false;
        let closed = Panel::new("Theme editor", pal, Icon::Glyph("◑", None))
            .window(|w| w.id(egui::Id::unique("theme_editor")))
            .fit_screen(ctx, vec2(560.0, 660.0), vec2(420.0, 320.0))
            .show(ctx, |ui| {
                chrome::scrolled_body(ui, true, |ui| editor_body(ui, &mut editor.draft, &pal));
                chrome::footer(ui, pal, |ui| {
                    if editor.confirm_delete {
                        ui.label("Move this theme to the trash?");
                        if ui.button("Delete").clicked() {
                            delete = true;
                        }
                        if ui.button("Keep").clicked() {
                            editor.confirm_delete = false;
                        }
                        return;
                    }
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        done = true;
                    }
                    if ui.button("Export…").on_hover_text("Write the theme file into the folder of the active panel").clicked() {
                        export = true;
                    }
                    if editor.original.is_some() && ui.button("Delete…").clicked() {
                        editor.confirm_delete = true;
                    }
                });
            });
        if export {
            self.export_theme(&editor.draft);
        }
        if save && self.save_theme(&mut editor) {
            done = true;
        }
        if delete {
            let slug = editor.original.clone().unwrap_or_default();
            if self.delete_theme(&slug) {
                done = true;
            } else {
                editor.confirm_delete = false;
            }
        }
        if !closed && !done {
            self.theme_editor = Some(editor);
        }
    }
}

pub fn is_theme_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(files::EXTENSION))
}

fn color_button(ui: &mut egui::Ui, color: &mut Color32) -> bool {
    ui.color_edit_button_srgba(color).changed()
}

fn editor_body(ui: &mut egui::Ui, draft: &mut UserTheme, app_pal: &Palette) {
    let weak = app_pal.text_weak;
    egui::Grid::new("theme_editor_grid").num_columns(2).spacing(vec2(16.0, 8.0)).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(240.0));
        ui.end_row();
        ui.label("Mode");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut draft.colors.dark, true, "Dark");
            ui.selectable_value(&mut draft.colors.dark, false, "Light");
        });
        ui.end_row();
        let Colors { background, foreground, accent, .. } = &mut draft.colors;
        for (label, color) in [("Background", background), ("Text", foreground), ("Accent", accent)] {
            ui.label(label);
            color_button(ui, color);
            ui.end_row();
        }
    });
    ui.label(RichText::new("The other shades follow from these three colours.").color(weak).size(11.0));

    ui.add_space(8.0);
    let pal = Palette { glass: false, ..draft.palette() };
    preview(ui, &Palette { round: app_pal.round, outlined: app_pal.outlined, ..pal });
    let issues = readability_issues(&pal);
    if issues.is_empty() {
        ui.label(RichText::new("✔ Everything reads well").color(weak));
    }
    // Orange that reads on the theme in use, light or dark.
    let warning = if app_pal.dark { hex("#eda95d") } else { hex("#9a5a12") };
    for issue in issues {
        ui.label(RichText::new(format!("⚠ {issue}")).color(warning));
    }

    ui.add_space(8.0);
    egui::CollapsingHeader::new("Advanced: set single shades by hand").id_salt("theme_editor_advanced").show(ui, |ui| {
        let mut derived = draft.derived();
        egui::Grid::new("theme_editor_shades").num_columns(3).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
            for (i, (_, label)) in FIELDS.iter().enumerate() {
                let mut own = draft.overrides[i].is_some();
                if ui.checkbox(&mut own, *label).changed() {
                    draft.overrides[i] = own.then(|| *files::slot(&mut derived, i));
                }
                match &mut draft.overrides[i] {
                    Some(color) => {
                        color_button(ui, color);
                    }
                    None => {
                        let mut shown = *files::slot(&mut derived, i);
                        ui.add_enabled_ui(false, |ui| color_button(ui, &mut shown));
                    }
                }
                ui.end_row();
            }
        });
        if draft.overrides.iter().any(Option::is_some) && ui.button("Clear all").clicked() {
            draft.overrides = [None; FIELDS.len()];
        }
    });
}

/// A small panel in the theme's colours: tabs, rows and small print.
fn preview(ui: &mut egui::Ui, pal: &Palette) {
    let width = ui.available_width().min(460.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 172.0), Sense::hover());
    let p = ui.painter_at(rect);
    let font = FontId::proportional(13.0);
    p.rect_filled(rect, pal.corner_large(), pal.window);
    let inner = rect.shrink(8.0);
    let body = egui::Rect::from_min_max(inner.min + vec2(0.0, 24.0), inner.max);
    p.rect(body, pal.corner_large(), pal.panel, pal.outline(), egui::StrokeKind::Inside);
    let tabs = [("Documents", true, true, false), ("Pictures", true, false, false), ("Music", false, true, false)];
    let mut x = inner.min.x;
    for (name, selected, in_use, hovered) in tabs {
        let look = tab_look(pal, selected, in_use, hovered);
        let tab = egui::Rect::from_min_size(egui::pos2(x, inner.min.y), vec2(96.0, 22.0));
        p.rect_filled(tab, crate::theme::top(pal.corner()), look.fill);
        p.text(tab.center(), Align2::CENTER_CENTER, name, font.clone(), look.text);
        x += 100.0;
    }
    let rows = [
        ("report.pdf", None),
        ("notes.txt", Some(pal.stripe())),
        ("holiday.jpg", Some(pal.selection())),
        ("song.flac", Some(pal.cursor(false))),
    ];
    let mut y = body.min.y + 6.0;
    for (name, fill) in rows {
        let row = egui::Rect::from_min_size(egui::pos2(body.min.x + 6.0, y), vec2(body.width() - 12.0, 20.0));
        if let Some(fill) = fill {
            p.rect_filled(row, pal.corner(), fill);
        }
        p.text(row.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, name, font.clone(), pal.text);
        p.text(row.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, "2 days ago", font.clone(), pal.text_weak);
        y += 22.0;
    }
    // Under the rows, as the status line of a panel.
    let line = egui::pos2(body.min.x + 14.0, y + 12.0);
    let shown = p.text(line, Align2::LEFT_CENTER, "4 items", font.clone(), pal.text_weak);
    p.text(shown.right_center() + vec2(16.0, 0.0), Align2::LEFT_CENTER, "Link", font, pal.text_important);
}
