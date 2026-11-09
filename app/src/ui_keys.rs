//! The window where keyboard shortcuts are changed.

use std::time::Duration;

use egui::{Align2, Button, CornerRadius, Event, Frame, Margin, RichText, Stroke, TextEdit, vec2};

use crate::app::{App, is_computer};
use crate::chrome::{self, Icon, Panel};
use crate::keymap::{self, COMMANDS, Chord, Command, FolderKey, Group, Info, Input, Layer, MAX_KEYS, Mode, Target};
use crate::theme::hex;

/// How long an unfinished shortcut waits before its next keys are shown, so that
/// typing a familiar sequence quickly never flashes a panel.
pub const WHICH_KEY_DELAY: Duration = Duration::from_millis(300);

/// A command of the user's own: a folder and where it opens.
pub struct FolderForm {
    pub path: String,
    pub new_tab: bool,
    /// The entry being changed, when this is not a new one.
    original: Option<Target>,
}

pub enum Subject {
    Command(Command),
    Folder(FolderForm),
}

/// A shortcut being recorded: every key pressed outside a text field goes here until
/// Save or Cancel.
pub struct Capture {
    pub subject: Subject,
    pub keys: Vec<Chord>,
    /// The keys shown are the old ones, and the first press starts over.
    replace_keys: bool,
}

impl Capture {
    pub fn command(command: Command) -> Capture {
        Capture { subject: Subject::Command(command), keys: Vec::new(), replace_keys: false }
    }

    pub fn folder(path: String) -> Capture {
        let form = FolderForm { path, new_tab: false, original: None };
        Capture { subject: Subject::Folder(form), keys: Vec::new(), replace_keys: false }
    }

    fn edit(folder: &FolderKey) -> Capture {
        let form = FolderForm { path: folder.path.display().to_string(), new_tab: folder.new_tab, original: Some(folder.target()) };
        let keys = keymap::parse_keys(&folder.keys).unwrap_or_default();
        Capture { subject: Subject::Folder(form), keys, replace_keys: true }
    }

    /// What Save would bind the keys to; `None` until a full folder path is entered.
    fn target(&self) -> Option<Target> {
        match &self.subject {
            Subject::Command(command) => Some(Target::Command(*command)),
            Subject::Folder(form) => {
                let path = crate::ui_list::typed_path(&form.path);
                path.is_absolute().then_some(Target::Folder { path, new_tab: form.new_tab })
            }
        }
    }
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
    Record(Command),
    ResetRow(&'static Info),
    NewFolder,
    EditFolder(FolderKey),
    RemoveFolder(Target),
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
        if ctx.text_edit_focused() { return; }
        for input in ctx.input(|i| keymap::presses(&i.events)) {
            if let Input::Press { chord, repeat: false, .. } = input {
                if std::mem::take(&mut capture.replace_keys) {
                    capture.keys.clear();
                }
                if capture.keys.len() < MAX_KEYS {
                    capture.keys.push(chord);
                }
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
        let columns = (((screen.width() - 60.0) / COLUMN).floor() as usize).clamp(1, 2).min(hints.len());
        let rows = hints.len().div_ceil(columns);
        let pending = self.matcher.pending();
        egui::Area::new(egui::Id::unique("which_key"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-10.0, -34.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                Frame::NONE.fill(pal.panel).corner_radius(CornerRadius::same(1))
                    .stroke(Stroke::new(1.0, pal.border)).inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(pending).monospace().color(pal.text_weak));
                            ui.label(RichText::new("Esc cancels").size(11.0).color(pal.text_weak));
                        });
                        ui.add_space(4.0);
                        egui::Grid::new("which_key_grid").num_columns(columns * 2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                            for row in 0..rows {
                                for column in 0..columns {
                                    match hints.get(row + column * rows) {
                                        Some(hint) => {
                                            ui.label(RichText::new(hint.key.label()).monospace().color(pal.text));
                                            ui.add(egui::Label::new(RichText::new(&hint.label).color(pal.text_weak)).wrap_mode(egui::TextWrapMode::Extend));
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
        let active = self.panes[self.active].path().to_path_buf();
        let weak = self.pal.text_weak;
        let red = hex("#ef7063");
        let Some(capture) = &mut window.capture else { return };
        match &mut capture.subject {
            Subject::Command(command) => {
                ui.label(RichText::new(format!("New shortcut for: {}", command.info().label)).strong());
            }
            Subject::Folder(form) => {
                ui.label(RichText::new("Your command").strong());
                ui.horizontal(|ui| {
                    ui.label("Folder");
                    ui.add(TextEdit::singleline(&mut form.path).hint_text("/path/to/folder").desired_width(340.0));
                    if ui.add_enabled(!is_computer(&active), Button::new("Use active panel")).clicked() {
                        form.path = active.display().to_string();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Open in");
                    ui.selectable_value(&mut form.new_tab, false, "This tab");
                    ui.selectable_value(&mut form.new_tab, true, "New tab");
                });
            }
        }
        if capture.keys.is_empty() {
            let prompt = if ui.ctx().text_edit_focused() {
                "Click outside the field, then press the keys in order."
            } else {
                "Press the keys in order, then click Save."
            };
            ui.label(RichText::new(prompt).color(weak));
        } else {
            ui.label(RichText::new(keymap::keys_label(&capture.keys)).monospace().strong());
        }
        let target = capture.target();
        // An entry being changed must not clash with its own old keys.
        let mut others = self.st.keymap.clone();
        if let Subject::Folder(FolderForm { original: Some(original), .. }) = &capture.subject {
            others.remove_folder(layer, original);
        }
        let clash = target.as_ref().filter(|_| !capture.keys.is_empty())
            .and_then(|t| keymap::conflict(&others, layer, &capture.keys, t));
        if let Some(clash) = &clash {
            ui.label(RichText::new(format!("Already used by: {}", clash.label())).color(red));
        }
        if target.is_none() {
            ui.label(RichText::new("Enter a full folder path, or start it with ~.").color(weak));
        }
        let (mut save, mut clear, mut cancel) = (false, false, false);
        ui.horizontal(|ui| {
            let text = if clash.is_some() { "Replace" } else { "Save" };
            save = ui.add_enabled(target.is_some() && !capture.keys.is_empty(), Button::new(text)).clicked();
            clear = ui.add_enabled(!capture.keys.is_empty(), Button::new("Clear keys")).clicked();
            cancel = ui.button("Cancel").clicked();
        });
        if clear {
            capture.keys.clear();
            capture.replace_keys = false;
        }
        if save && let Some(target) = target {
            others.clear_conflicts(layer, &capture.keys, &target);
            others.assign(layer, &target, std::mem::take(&mut capture.keys));
            self.st.keymap = others;
            window.capture = None;
            self.keymap_changed();
        } else if cancel {
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
            Some(Change::Record(command)) => window.capture = Some(Capture::command(command)),
            Some(Change::NewFolder) => {
                let current = self.panes[self.active].path();
                let path = if is_computer(current) { String::new() } else { current.display().to_string() };
                window.capture = Some(Capture::folder(path));
            }
            Some(Change::EditFolder(folder)) => window.capture = Some(Capture::edit(&folder)),
            Some(Change::ResetRow(info)) => {
                self.st.keymap.layer_mut(layer).bindings.remove(info.id);
                self.keymap_changed();
            }
            Some(Change::RemoveFolder(target)) => {
                self.st.keymap.remove_folder(layer, &target);
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
                            change = Some(Change::Record(info.command));
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
        ui.label(RichText::new("Your commands").strong().color(self.pal.accent));
        let folders = &self.st.keymap.layer(layer).folders;
        if !folders.is_empty() {
            egui::Grid::new("shortcut_folders").num_columns(2).min_col_width(230.0).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                for folder in folders {
                    ui.label(folder.path.display().to_string());
                    ui.horizontal(|ui| {
                        let label = keymap::parse_keys(&folder.keys).map_or(folder.keys.clone(), |k| keymap::keys_label(&k));
                        if ui.add(Button::new(label).small()).on_hover_text("Change this command").clicked() {
                            change = Some(Change::EditFolder(folder.clone()));
                        }
                        ui.label(RichText::new(if folder.new_tab { "new tab" } else { "this tab" }).color(self.pal.text_weak).size(11.0));
                        if ui.add(Button::new("×").small()).on_hover_text("Remove this command").clicked() {
                            change = Some(Change::RemoveFolder(folder.target()));
                        }
                    });
                    ui.end_row();
                }
            });
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(Button::new("+ Command").small()).on_hover_text("Open a folder of your choice with your own keys").clicked() {
                change = Some(Change::NewFolder);
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
