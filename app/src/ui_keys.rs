//! The window where keyboard shortcuts are changed.

use std::time::Duration;

use egui::{Align2, Button, CornerRadius, Event, Frame, Margin, RichText, Stroke, TextEdit, vec2};

use crate::app::{App, is_computer};
use crate::chrome::{self, Icon, Panel};
use crate::keymap::{self, COMMANDS, Chord, Command, Group, Info, Input, Layer, MAX_KEYS, Mode, Target};
use crate::theme::hex;

/// How long an unfinished shortcut waits before its next keys are shown, so that
/// typing a familiar sequence quickly never flashes a panel.
pub const WHICH_KEY_DELAY: Duration = Duration::from_millis(300);

/// A shortcut being recorded: everything pressed goes here until Save or Cancel.
pub struct Capture {
    pub target: Target,
    pub keys: Vec<Chord>,
}

pub struct ShortcutsWindow {
    pub layer: Layer,
    pub search: String,
    pub capture: Option<Capture>,
    confirm_reset: bool,
}

impl ShortcutsWindow {
    pub fn new(layer: Layer) -> ShortcutsWindow {
        ShortcutsWindow { layer, search: String::new(), capture: None, confirm_reset: false }
    }
}

/// A change picked in the list, applied once the list has been drawn.
enum Change {
    Remove(&'static Info, usize),
    Record(Target),
    ResetRow(&'static Info),
    RemoveFolder(std::path::PathBuf),
    ResetLayer,
}

const GROUPS: [(Group, &str); 6] = [
    (Group::Moving, "Moving"),
    (Group::Folders, "Folders"),
    (Group::Files, "Files"),
    (Group::Tabs, "Tabs"),
    (Group::Panels, "Panels"),
    (Group::View, "View"),
];

/// Commands that mean nothing in this layer or on this system stay out of the list.
fn listed(info: &Info, layer: Layer) -> bool {
    let neovim_only = matches!(info.command, Command::Filter | Command::Visual | Command::VisualCopy | Command::VisualCut);
    !(neovim_only && layer == Layer::Standard) && (info.command != Command::Properties || cfg!(target_os = "linux"))
}

impl App {
    pub(crate) fn recording_shortcut(&self) -> bool {
        self.shortcuts.as_ref().is_some_and(|w| w.capture.is_some())
    }

    /// Takes every key press for the recording, so nothing else reacts to it.
    fn read_capture(&mut self, ctx: &egui::Context, window: &mut ShortcutsWindow) {
        let Some(capture) = &mut window.capture else { return };
        for input in ctx.input(|i| keymap::presses(&i.events)) {
            if let Input::Press { chord, repeat: false, .. } = input
                && capture.keys.len() < MAX_KEYS
            {
                capture.keys.push(chord);
            }
        }
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e,
            Event::Key { .. } | Event::Text(_) | Event::Copy | Event::Cut | Event::Paste(_))));
    }

    fn keymap_changed(&mut self) {
        self.keys = keymap::Keymap::new(&self.st.keymap);
        self.matcher.clear();
        self.mark_dirty();
    }

    /// Which-key: the keys that can come next, while a shortcut is unfinished.
    pub fn which_key(&mut self, ctx: &egui::Context) {
        if !self.st.which_key || self.recording_shortcut() { return; }
        let Some(waited) = self.matcher.waited() else { return };
        if waited < self.which_key_delay {
            ctx.request_repaint_after(self.which_key_delay - waited);
            return;
        }
        let hints = self.keys.hints(self.key_mode(self.active), self.matcher.keys());
        if hints.is_empty() { return; }
        let pal = self.pal;
        const COLUMN: f32 = 230.0;
        let screen = ctx.input(|i| i.content_rect());
        let columns = (((screen.width() - 60.0) / COLUMN).floor() as usize).clamp(1, 4).min(hints.len());
        let rows = hints.len().div_ceil(columns);
        let pending = self.matcher.pending();
        egui::Area::new(egui::Id::unique("which_key"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -34.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                Frame::NONE.fill(pal.raised).corner_radius(CornerRadius::same(1))
                    .stroke(Stroke::new(1.0, pal.accent)).inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(pending).monospace().strong().color(pal.accent));
                            ui.label(RichText::new("Esc cancels").size(11.0).color(pal.text_weak));
                        });
                        ui.add_space(4.0);
                        egui::Grid::new("which_key_grid").num_columns(columns * 2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                            for row in 0..rows {
                                for column in 0..columns {
                                    match hints.get(row + column * rows) {
                                        Some(hint) => {
                                            ui.label(RichText::new(hint.key.label()).monospace().strong().color(pal.accent));
                                            ui.add(egui::Label::new(RichText::new(&hint.label).color(pal.text)).wrap_mode(egui::TextWrapMode::Extend));
                                        }
                                        None => { ui.label(""); ui.label(""); }
                                    }
                                }
                                ui.end_row();
                            }
                        });
                    });
            });
    }

    pub fn shortcuts_window(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.shortcuts.take() else { return };
        self.read_capture(ctx, &mut window);
        let closed = Panel::new("Keyboard shortcuts", self.pal, Icon::Glyph("⚙", None))
            .window(|w| w.id(egui::Id::unique("keyboard_shortcuts")))
            .fit_screen(ctx, vec2(620.0, 640.0), vec2(420.0, 300.0))
            .show(ctx, |ui| {
                chrome::scrolled_body(ui, false, |ui| self.shortcuts_body(ui, &mut window));
            });
        if !closed {
            self.shortcuts = Some(window);
        }
    }

    fn capture_strip(&mut self, ui: &mut egui::Ui, window: &mut ShortcutsWindow) {
        let layer = window.layer;
        let Some(capture) = &window.capture else { return };
        let weak = self.pal.text_weak;
        ui.label(RichText::new(format!("New shortcut for: {}", capture.target.label())).strong());
        if capture.keys.is_empty() {
            ui.label(RichText::new("Press the keys in order, then click Save.").color(weak));
        } else {
            ui.label(RichText::new(keymap::keys_label(&capture.keys)).monospace().strong());
        }
        let clash = keymap::conflict(&self.st.keymap, layer, &capture.keys, &capture.target).filter(|_| !capture.keys.is_empty());
        if let Some(clash) = &clash {
            ui.label(RichText::new(format!("Already used by: {}", clash.label())).color(hex("#ef7063")));
        }
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let text = if clash.is_some() { "Replace" } else { "Save" };
            save = ui.add_enabled(!capture.keys.is_empty(), Button::new(text)).clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if save && let Some(capture) = window.capture.take() {
            self.st.keymap.clear_conflicts(layer, &capture.keys, &capture.target);
            self.st.keymap.assign(layer, &capture.target, capture.keys);
            self.keymap_changed();
        }
        if cancel {
            window.capture = None;
        }
        ui.separator();
    }

    fn shortcuts_body(&mut self, ui: &mut egui::Ui, window: &mut ShortcutsWindow) {
        ui.spacing_mut().item_spacing.y = 6.0;
        self.capture_strip(ui, window);
        let mut change = None;
        let idle = window.capture.is_none();
        ui.add_enabled_ui(idle, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut window.layer, Layer::Standard, "Standard");
                ui.selectable_value(&mut window.layer, Layer::Neovim, "Neovim");
                ui.add(TextEdit::singleline(&mut window.search).hint_text("Search").desired_width(180.0));
            });
            if window.layer == Layer::Neovim {
                ui.label(RichText::new("These take priority over the standard shortcuts while Neovim-style keys are on.").color(self.pal.text_weak).size(11.0));
            }
            change = self.shortcut_rows(ui, window);
        });
        let layer = window.layer;
        match change {
            Some(Change::Remove(info, k)) => {
                let mut keys = self.st.keymap.keys_of(layer, info);
                keys.remove(k);
                self.st.keymap.set_keys(layer, info, &keys);
                self.keymap_changed();
            }
            Some(Change::Record(target)) => window.capture = Some(Capture { target, keys: Vec::new() }),
            Some(Change::ResetRow(info)) => {
                self.st.keymap.layer_mut(layer).bindings.remove(info.id);
                self.keymap_changed();
            }
            Some(Change::RemoveFolder(path)) => {
                self.st.keymap.layer_mut(layer).folders.retain(|f| f.path != path);
                self.keymap_changed();
            }
            Some(Change::ResetLayer) => {
                *self.st.keymap.layer_mut(layer) = Default::default();
                self.keymap_changed();
            }
            None => {}
        }
    }

    fn shortcut_rows(&self, ui: &mut egui::Ui, window: &mut ShortcutsWindow) -> Option<Change> {
        let layer = window.layer;
        let accent = self.pal.accent;
        let query = window.search.to_lowercase();
        let mut change = None;
        for (group, title) in GROUPS {
            let rows: Vec<(&'static Info, Vec<String>)> = COMMANDS.iter()
                .filter(|info| info.group == group && listed(info, layer))
                .map(|info| (info, self.st.keymap.keys_of(layer, info).iter().map(|k| keymap::keys_label(k)).collect()))
                .filter(|(info, labels): &(&Info, Vec<String>)| query.is_empty()
                    || info.label.to_lowercase().contains(&query)
                    || labels.iter().any(|l| l.to_lowercase().contains(&query)))
                .collect();
            if rows.is_empty() { continue; }
            ui.add_space(6.0);
            ui.label(RichText::new(title).strong().color(accent));
            egui::Grid::new(("shortcut_rows", title)).num_columns(2).min_col_width(230.0).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                for (info, labels) in rows {
                    ui.label(info.label);
                    ui.horizontal_wrapped(|ui| {
                        // A row of the upper layer can be empty and still have a working key.
                        if labels.is_empty() && layer == Layer::Neovim
                            && let Some(inherited) = self.keys.label(Mode::Normal, info.command)
                        {
                            ui.label(RichText::new(format!("Standard: {inherited}")).color(self.pal.text_weak).size(11.0));
                        }
                        for (k, label) in labels.iter().enumerate() {
                            if ui.add(Button::new(format!("{label}  ×")).small()).on_hover_text("Remove this shortcut").clicked() {
                                change = Some(Change::Remove(info, k));
                            }
                        }
                        if ui.add(Button::new("+").small()).on_hover_text("Add a shortcut").clicked() {
                            change = Some(Change::Record(Target::Command(info.command)));
                        }
                        if self.st.keymap.layer(layer).bindings.contains_key(info.id)
                            && ui.add(Button::new("Reset").small()).on_hover_text("Back to the default shortcuts").clicked() {
                            change = Some(Change::ResetRow(info));
                        }
                    });
                    ui.end_row();
                }
            });
        }
        if let Some(c) = self.folder_rows(ui, window) { change = Some(c); }
        change
    }

    fn folder_rows(&self, ui: &mut egui::Ui, window: &mut ShortcutsWindow) -> Option<Change> {
        let layer = window.layer;
        let mut change = None;
        ui.add_space(6.0);
        ui.label(RichText::new("Your folders").strong().color(self.pal.accent));
        let folders = &self.st.keymap.layer(layer).folders;
        if !folders.is_empty() {
            egui::Grid::new("shortcut_folders").num_columns(2).min_col_width(230.0).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                for folder in folders {
                    ui.label(folder.path.display().to_string());
                    ui.horizontal(|ui| {
                        let label = keymap::parse_keys(&folder.keys).map_or(folder.keys.clone(), |k| keymap::keys_label(&k));
                        if ui.add(Button::new(label).small()).on_hover_text("Record other keys").clicked() {
                            change = Some(Change::Record(Target::Folder(folder.path.clone())));
                        }
                        if ui.add(Button::new("×").small()).on_hover_text("Remove this folder").clicked() {
                            change = Some(Change::RemoveFolder(folder.path.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let current = self.panes[self.active].path().to_path_buf();
            let add = Button::new("+ Folder").small();
            if ui.add_enabled(!is_computer(&current), add).on_hover_text("Give the active panel's folder a shortcut").clicked() {
                change = Some(Change::Record(Target::Folder(current)));
            }
            let text = if window.confirm_reset { "Really reset this layer?" } else { "Reset all" };
            if ui.add(Button::new(text).small()).clicked() {
                if window.confirm_reset { change = Some(Change::ResetLayer); }
                window.confirm_reset = !window.confirm_reset;
            }
        });
        change
    }
}
