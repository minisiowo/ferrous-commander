//! Search workspaces: independent scopes, retained result tabs and file actions.
//! Selection belongs to result paths, never to the file pane behind this window.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use chrono::{Local, NaiveDate, TimeZone};
use egui::{Align2, FontId, Key, Modifiers, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use crate::app::{Act, App, Notice};
use crate::chrome::{self, Icon, Panel};
use crate::fsx::{self, Entry};
use crate::icons;
use crate::ops::OpKind;
use crate::preview::{Key as PreviewKey, Preview, PreviewContent};
use crate::search::{self, Criteria, FileType, NameMode, Outcome, Search};
use crate::ui::{fit_text, take_right};

const TABS_MAX: usize = 8;
const REFRESH: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Column {
    Name,
    Location,
    Type,
    Size,
    Modified,
}
impl Column {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Location => "Location",
            Self::Type => "Type",
            Self::Size => "Size",
            Self::Modified => "Modified",
        }
    }
}

struct Inputs {
    root: String,
    name: String,
    mode: NameMode,
    case_sensitive: bool,
    recursive: bool,
    hidden: bool,
    file_type: FileType,
    min_size: String,
    max_size: String,
    from: String,
    through: String,
    excluded: String,
    limit: usize,
    advanced: bool,
}
impl Inputs {
    fn new(root: PathBuf, hidden: bool) -> Self {
        Self {
            root: root.display().to_string(),
            name: String::new(),
            mode: NameMode::Words,
            case_sensitive: false,
            recursive: true,
            hidden,
            file_type: FileType::All,
            min_size: String::new(),
            max_size: String::new(),
            from: String::new(),
            through: String::new(),
            excluded: String::new(),
            limit: 10_000,
            advanced: false,
        }
    }
    fn criteria(&self) -> Result<(PathBuf, Criteria), String> {
        let root = crate::ui_list::typed_path(&self.root);
        if !root.is_absolute() {
            return Err("Choose an absolute folder path".into());
        }
        Ok((
            root,
            Criteria {
                name: self.name.clone(),
                mode: self.mode,
                case_sensitive: self.case_sensitive,
                recursive: self.recursive,
                hidden: self.hidden,
                file_type: self.file_type,
                min_size: search::parse_size(&self.min_size)?,
                max_size: search::parse_size(&self.max_size)?,
                modified_from: date(&self.from, false)?,
                modified_before: date(&self.through, true)?,
                excluded_dirs: self
                    .excluded
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect(),
                limit: self.limit,
            },
        ))
    }
}

fn date(text: &str, through: bool) -> Result<Option<SystemTime>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut day = NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|_| "Use dates in YYYY-MM-DD format".to_string())?;
    if through {
        day = day.succ_opt().ok_or("Date is out of range")?;
    }
    let time = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .ok_or("This local date is unavailable")?;
    let seconds = time.timestamp();
    let duration = Duration::from_secs(seconds.unsigned_abs());
    let time = if seconds >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(duration)
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(duration)
    };
    time.map(Some)
        .ok_or_else(|| "Date is out of range on this system".into())
}

struct Transfer {
    moving: bool,
    destination: String,
    paths: Vec<PathBuf>,
}

struct SearchTab {
    id: u64,
    inputs: Inputs,
    run: Option<Search>,
    summary: String,
    error: Option<String>,
    view: Vec<usize>,
    sort: Column,
    ascending: bool,
    sort_dirty: bool,
    refreshed: Instant,
    run_id: u64,
    selected: HashSet<PathBuf>,
    cursor: Option<PathBuf>,
    cursor_index: Option<usize>,
    scroll: f32,
    page_rows: usize,
    anchor: Option<PathBuf>,
    reveal: bool,
    focus: bool,
    auto_start: bool,
    preview_on: bool,
    preview: Option<Preview>,
    transfer: Option<Transfer>,
}
impl SearchTab {
    fn new(id: u64, root: PathBuf, hidden: bool) -> Self {
        Self {
            id,
            inputs: Inputs::new(root, hidden),
            run: None,
            summary: String::new(),
            error: None,
            view: Vec::new(),
            sort: Column::Name,
            ascending: true,
            sort_dirty: false,
            refreshed: Instant::now(),
            run_id: 0,
            selected: HashSet::new(),
            cursor: None,
            cursor_index: None,
            scroll: 0.0,
            page_rows: 1,
            anchor: None,
            reveal: false,
            focus: true,
            auto_start: false,
            preview_on: false,
            preview: None,
            transfer: None,
        }
    }
    fn table_id(&self) -> egui::Id {
        egui::Id::unique("search_results").with(self.id)
    }
    fn running(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|r| !r.done.load(Ordering::Acquire))
    }
    fn start(&mut self, ctx: &egui::Context) {
        match self.inputs.criteria().and_then(|(root, criteria)| {
            let summary = format!(
                "{} · {}",
                if criteria.name.trim().is_empty() {
                    "All names"
                } else {
                    criteria.name.trim()
                },
                root.display()
            );
            search::start(root, criteria, ctx.clone()).map(|run| (run, summary))
        }) {
            Ok((run, summary)) => {
                self.run = Some(run);
                self.summary = summary;
                self.error = None;
                self.view.clear();
                self.selected.clear();
                self.cursor = None;
                self.cursor_index = None;
                self.scroll = 0.0;
                self.anchor = None;
                self.preview = None;
                self.transfer = None;
                self.sort_dirty = true;
                self.run_id += 1;
            }
            Err(e) => self.error = Some(e),
        }
    }
    fn collect(&mut self) {
        let Some(run) = &self.run else { return };
        let results = run.results.lock().unwrap();
        let changed = results.len() != self.view.len();
        if !self.sort_dirty
            && (!changed
                || (self.running() && !self.view.is_empty() && self.refreshed.elapsed() < REFRESH))
        {
            return;
        }
        if changed {
            self.view.extend(self.view.len()..results.len());
        }
        let (sort, ascending) = (self.sort, self.ascending);
        self.view.sort_by(|&a, &b| {
            let (a, b) = (&results[a], &results[b]);
            let order = match sort {
                Column::Name => a.lname().cmp(b.lname()),
                Column::Location => a.path.parent().cmp(&b.path.parent()),
                Column::Type => a.ext().cmp(b.ext()),
                Column::Size => a.size.cmp(&b.size),
                Column::Modified => a.modified.cmp(&b.modified),
            }
            .then_with(|| a.path.cmp(&b.path));
            if ascending { order } else { order.reverse() }
        });
        self.sort_dirty = false;
        self.refreshed = Instant::now();
    }
    fn paths(&self) -> Vec<PathBuf> {
        let Some(run) = &self.run else {
            return Vec::new();
        };
        let entries = run.results.lock().unwrap();
        self.view
            .iter()
            .filter_map(|&i| entries.get(i))
            .filter(|e| self.selected.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }
    fn cursor_entry(&self) -> Option<Entry> {
        let entry = self
            .run
            .as_ref()?
            .results
            .lock()
            .unwrap()
            .get(self.cursor_index?)
            .cloned()?;
        (Some(&entry.path) == self.cursor.as_ref()).then_some(entry)
    }
    fn select(&mut self, row: usize, modifiers: Modifiers) {
        let Some(run) = &self.run else { return };
        let entries = run.results.lock().unwrap();
        let Some(entry) = self.view.get(row).and_then(|&i| entries.get(i)) else {
            return;
        };
        let path = entry.path.clone();
        if modifiers.shift {
            let anchor = self
                .anchor
                .as_ref()
                .and_then(|p| self.view.iter().position(|&i| &entries[i].path == p))
                .unwrap_or(row);
            if !modifiers.command && !modifiers.ctrl {
                self.selected.clear();
            }
            for &i in &self.view[anchor.min(row)..=anchor.max(row)] {
                self.selected.insert(entries[i].path.clone());
            }
        } else if modifiers.command || modifiers.ctrl {
            if !self.selected.remove(&path) {
                self.selected.insert(path.clone());
            }
            self.anchor = Some(path.clone());
        } else {
            self.selected.clear();
            self.selected.insert(path.clone());
            self.anchor = Some(path.clone());
        }
        self.cursor = Some(path);
        self.cursor_index = Some(self.view[row]);
    }
}

pub struct SearchUi {
    tabs: Vec<SearchTab>,
    active: usize,
    next_id: u64,
    layer: Option<egui::LayerId>,
    keyboard_active: bool,
}
impl SearchUi {
    pub fn new(root: PathBuf, hidden: bool) -> Self {
        Self {
            tabs: vec![SearchTab::new(0, root, hidden)],
            active: 0,
            next_id: 1,
            layer: None,
            keyboard_active: false,
        }
    }
    pub fn add_tab(&mut self, root: PathBuf, hidden: bool) {
        if self.tabs.len() >= TABS_MAX {
            self.tabs[self.active].error =
                Some("Close a search tab before opening another (maximum 8)".into());
            return;
        }
        self.tabs.push(SearchTab::new(self.next_id, root, hidden));
        self.next_id += 1;
        self.active = self.tabs.len() - 1;
    }
    pub(crate) fn screenshot(root: PathBuf, hidden: bool) -> Self {
        let mut window = Self::new(root, hidden);
        let tab = &mut window.tabs[0];
        tab.inputs.mode = NameMode::Wildcard;
        tab.inputs.name = "*".into();
        tab.inputs.advanced = true;
        tab.auto_start = true;
        tab.focus = false;
        window
    }
    pub(crate) fn owns_keyboard(&self, ctx: &egui::Context) -> bool {
        let focused = ctx
            .memory(|m| m.focused())
            .and_then(|id| ctx.read_response(id));
        focused.map_or(self.keyboard_active, |response| {
            Some(response.layer_id) == self.layer
        }) || self
            .tabs
            .get(self.active)
            .is_some_and(|s| ctx.memory(|m| m.has_focus(s.table_id())))
    }
}

#[derive(Clone, Copy)]
enum Action {
    Open,
    ShowFolder,
    Copy,
    Cut,
    Trash,
    CopyTo,
    MoveTo,
    CopyPaths,
}

fn active_root(app: &App) -> PathBuf {
    let root = app.panes[app.active].path();
    if crate::app::is_computer(root) {
        dirs::home_dir().unwrap_or_else(std::env::temp_dir)
    } else {
        root.to_path_buf()
    }
}

fn mode_label(mode: NameMode) -> &'static str {
    match mode {
        NameMode::Words => "Contains words",
        NameMode::Wildcard => "Wildcard masks",
        NameMode::Regex => "Regular expression",
    }
}
fn type_label(kind: FileType) -> &'static str {
    match kind {
        FileType::All => "All items",
        FileType::Files => "Files only",
        FileType::Folders => "Folders only",
        FileType::Audio => "Audio",
        FileType::Video => "Video",
        FileType::Images => "Images",
        FileType::Documents => "Documents",
        FileType::Text => "Text & code",
        FileType::Archives => "Archives",
    }
}

fn columns(width: f32) -> Vec<(Column, f32)> {
    let mut columns = vec![
        (Column::Name, (width * 0.33).clamp(120.0, 260.0)),
        (Column::Location, 0.0),
        (Column::Size, 80.0),
    ];
    if width >= 580.0 {
        columns.insert(2, (Column::Type, 65.0));
    }
    if width >= 700.0 {
        columns.push((Column::Modified, 130.0));
    }
    let used: f32 = columns.iter().map(|(_, w)| w).sum();
    columns[1].1 = (width - used).max(40.0);
    columns
}

impl App {
    pub fn search_window(&mut self, ctx: &egui::Context) {
        let Some(mut window) = self.search.take() else {
            return;
        };
        let pal = self.pal;
        let mut action = None;
        let blocked = self.dialog.is_some() || !self.notices.is_empty();
        let closed = Panel::new("Search", pal, Icon::Glyph("🔍", None))
            .info("Files & folders")
            .fit_screen(ctx, vec2(1080.0, 740.0), vec2(640.0, 480.0))
            .show(ctx, |ui| {
                chrome::body(ui, |ui| {
                    window.layer = Some(ui.layer_id());
                    if let Some(at) = ui.input(|i| {
                        i.pointer
                            .any_pressed()
                            .then(|| i.pointer.interact_pos())
                            .flatten()
                    }) {
                        window.keyboard_active = ui.ctx().layer_id_at(at) == window.layer;
                    }
                    if ui.ctx().viewport_id() != egui::ViewportId::ROOT {
                        for event in ui.input(|i| i.events.clone()) {
                            if let egui::Event::WindowFocused(focused) = event {
                                window.keyboard_active = focused;
                            }
                        }
                    }
                    ui.add_enabled_ui(!blocked, |ui| {
                        let mut next = window.active;
                        let mut remove = None;
                        ui.horizontal_wrapped(|ui| {
                            for (i, tab) in window.tabs.iter().enumerate() {
                                let title = if tab.summary.is_empty() {
                                    "New search".to_string()
                                } else {
                                    tab.summary.chars().take(26).collect()
                                };
                                if ui
                                    .selectable_label(i == window.active, title)
                                    .on_hover_text(&tab.summary)
                                    .clicked()
                                {
                                    next = i;
                                }
                                if window.tabs.len() > 1
                                    && ui
                                        .small_button("×")
                                        .on_hover_text("Close this search")
                                        .clicked()
                                {
                                    remove = Some(i);
                                }
                            }
                            if ui
                                .add_enabled(
                                    window.tabs.len() < TABS_MAX,
                                    egui::Button::new("+ New search"),
                                )
                                .clicked()
                            {
                                window.add_tab(active_root(self), self.st.show_hidden);
                                next = window.active;
                            }
                        });
                        if let Some(i) = remove {
                            window.tabs.remove(i);
                            if next > i {
                                next -= 1;
                            }
                            next = next.min(window.tabs.len() - 1);
                        }
                        window.active = next;
                        for (i, tab) in window.tabs.iter_mut().enumerate() {
                            if i != window.active {
                                tab.preview = None;
                            }
                        }
                        let tab = &mut window.tabs[window.active];
                        ui.push_id(tab.id, |ui| {
                            self.search_inputs(ui, tab);
                            tab.collect();
                            self.search_status(ui, tab);
                            ui.separator();
                            let ready = !tab.selected.is_empty();
                            let can_preview = ui.available_width() >= 620.0;
                            ui.horizontal_wrapped(|ui| {
                                for (text, act) in [
                                    ("Show in folder", Action::ShowFolder),
                                    ("Open", Action::Open),
                                    ("Copy", Action::Copy),
                                    ("Cut", Action::Cut),
                                    ("Copy to…", Action::CopyTo),
                                    ("Move to…", Action::MoveTo),
                                    ("Trash…", Action::Trash),
                                ] {
                                    if ui.add_enabled(ready, egui::Button::new(text)).clicked() {
                                        action = Some(act);
                                        ui.memory_mut(|m| m.request_focus(tab.table_id()));
                                    }
                                }
                                ui.add_enabled(
                                    can_preview,
                                    egui::Checkbox::new(&mut tab.preview_on, "Preview"),
                                )
                                .on_disabled_hover_text(
                                    "Widen the search window to show a preview beside the results",
                                );
                                ui.label(format!("{} selected", tab.selected.len()));
                            });
                            if let Some(transfer) = &mut tab.transfer {
                                let mut apply = false;
                                let mut cancel = false;
                                ui.horizontal(|ui| {
                                    ui.label(format!(
                                        "{} {} items into",
                                        if transfer.moving { "Move" } else { "Copy" },
                                        transfer.paths.len()
                                    ));
                                    let destination_width =
                                        (ui.available_width() - 120.0).max(80.0);
                                    let id = ui.make_persistent_id("search_destination");
                                    let font = egui::TextStyle::Body.resolve(ui.style());
                                    self.path_field(
                                        ui,
                                        id,
                                        &mut transfer.destination,
                                        font,
                                        |edit| edit.desired_width(destination_width),
                                    );
                                    apply = ui
                                        .button(if transfer.moving { "Move" } else { "Copy" })
                                        .clicked();
                                    cancel = ui.button("Cancel").clicked();
                                });
                                if apply {
                                    let destination =
                                        crate::ui_list::typed_path(&transfer.destination);
                                    if destination.is_absolute() {
                                        self.start_job(
                                            if transfer.moving {
                                                OpKind::Move
                                            } else {
                                                OpKind::Copy
                                            },
                                            transfer.paths.clone(),
                                            Some(destination),
                                            ui.ctx(),
                                        );
                                        tab.transfer = None;
                                        tab.error = None;
                                    } else {
                                        tab.error =
                                            Some("Choose an absolute destination folder".into());
                                    }
                                } else if cancel {
                                    tab.transfer = None;
                                }
                            }
                            let height = ui.available_height().max(50.0);
                            let (mut area, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), height),
                                Sense::hover(),
                            );
                            let preview_width = (area.width() * 0.3).clamp(220.0, 320.0);
                            let preview = if tab.preview_on && area.width() >= 620.0 {
                                Some(take_right(&mut area, preview_width))
                            } else {
                                None
                            };
                            self.search_table(ui, tab, area, &mut action);
                            if let Some(r) = preview {
                                self.search_preview(ui, tab, r);
                            } else {
                                tab.preview = None;
                            }
                            if action.is_none()
                                && ui.is_enabled()
                                && ui.memory(|m| m.has_focus(tab.table_id()))
                                && !ui.ctx().text_edit_focused()
                            {
                                self.search_keys(ui, tab, &mut action);
                            }
                        });
                    });
                })
            });
        if let Some(action) = action {
            let tab = &mut window.tabs[window.active];
            let mut paths = tab.paths();
            let focused = tab
                .cursor
                .as_ref()
                .filter(|p| tab.selected.contains(*p))
                .cloned()
                .or_else(|| paths.first().cloned());
            if !matches!(
                action,
                Action::Open | Action::ShowFolder | Action::CopyPaths
            ) {
                paths = fsx::outermost_paths(paths);
            }
            match action {
                Action::Copy | Action::Cut => {
                    self.copy_paths_to_clipboard(paths, matches!(action, Action::Cut), ctx)
                }
                Action::CopyPaths => ctx.copy_text(
                    paths
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Action::Trash if !paths.is_empty() => {
                    self.notices.push(Notice::ConfirmTrash { items: paths });
                    ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
                }
                Action::CopyTo | Action::MoveTo if !paths.is_empty() => {
                    tab.transfer = Some(Transfer {
                        moving: matches!(action, Action::MoveTo),
                        destination: active_root(self).display().to_string(),
                        paths,
                    });
                }
                Action::ShowFolder => {
                    if let Some(path) = focused.as_ref()
                        && let Some(parent) = path.parent()
                    {
                        self.acts.push(Act::NavigateSelect(
                            self.active,
                            parent.to_path_buf(),
                            path.clone(),
                        ));
                        ctx.send_viewport_cmd_to(
                            egui::ViewportId::ROOT,
                            egui::ViewportCommand::Focus,
                        );
                    }
                }
                Action::Open => {
                    if let Some(path) = focused.as_ref() {
                        self.acts.push(Act::Open(self.active, path.clone()));
                        ctx.send_viewport_cmd_to(
                            egui::ViewportId::ROOT,
                            egui::ViewportCommand::Focus,
                        );
                    }
                }
                _ => {}
            }
        }
        if !closed {
            self.search = Some(window);
        }
    }

    fn search_inputs(&mut self, ui: &mut Ui, tab: &mut SearchTab) {
        let mut start = std::mem::take(&mut tab.auto_start);
        ui.horizontal(|ui| {
            ui.label("Search in");
            let width = (ui.available_width() - 140.0).max(120.0);
            let id = ui.make_persistent_id("search_scope");
            let font = egui::TextStyle::Body.resolve(ui.style());
            self.path_field(ui, id, &mut tab.inputs.root, font, |edit| {
                edit.desired_width(width)
            });
            if ui.button("Use active folder").clicked() {
                tab.inputs.root = active_root(self).display().to_string();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Name");
            let hint = match tab.inputs.mode {
                NameMode::Words => "All words in the name · leave empty to use filters",
                NameMode::Wildcard => "*.mp3;*.flac",
                NameMode::Regex => "^report.*\\.txt$",
            };
            let response = ui.add(
                egui::TextEdit::singleline(&mut tab.inputs.name)
                    .hint_text(hint)
                    .desired_width((ui.available_width() - 245.0).max(100.0)),
            );
            if std::mem::take(&mut tab.focus) {
                response.request_focus();
            }
            start |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            egui::ComboBox::from_id_salt("name_mode")
                .selected_text(mode_label(tab.inputs.mode))
                .show_ui(ui, |ui| {
                    for mode in [NameMode::Words, NameMode::Wildcard, NameMode::Regex] {
                        ui.selectable_value(&mut tab.inputs.mode, mode, mode_label(mode));
                    }
                });
            if tab.running() {
                if ui.button("Stop").clicked()
                    && let Some(run) = &tab.run
                {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            } else {
                start |= ui.button("Search").clicked();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut tab.inputs.recursive, "Subfolders");
            ui.checkbox(&mut tab.inputs.hidden, "Hidden items");
            ui.checkbox(&mut tab.inputs.case_sensitive, "Match case");
            egui::ComboBox::from_id_salt("file_type")
                .selected_text(type_label(tab.inputs.file_type))
                .show_ui(ui, |ui| {
                    for kind in [
                        FileType::All,
                        FileType::Files,
                        FileType::Folders,
                        FileType::Audio,
                        FileType::Video,
                        FileType::Images,
                        FileType::Documents,
                        FileType::Text,
                        FileType::Archives,
                    ] {
                        ui.selectable_value(&mut tab.inputs.file_type, kind, type_label(kind));
                    }
                });
            ui.toggle_value(&mut tab.inputs.advanced, "More filters");
        });
        if tab.inputs.advanced {
            ui.horizontal_wrapped(|ui| {
                ui.label("Size from");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.inputs.min_size)
                        .hint_text("10 MB")
                        .desired_width(85.0),
                );
                ui.label("to");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.inputs.max_size)
                        .hint_text("1 GiB")
                        .desired_width(85.0),
                );
                ui.label("Modified from");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.inputs.from)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(100.0),
                );
                ui.label("through");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.inputs.through)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(100.0),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Exclude folders");
                ui.add(
                    egui::TextEdit::singleline(&mut tab.inputs.excluded)
                        .hint_text(".git;node_modules;target")
                        .desired_width(260.0),
                )
                .on_hover_text("Folder names or relative path masks, separated with semicolons");
                ui.label("Result limit");
                ui.add(
                    egui::DragValue::new(&mut tab.inputs.limit).range(100..=search::RESULTS_MAX),
                );
            });
        }
        if let Some(error) = &tab.error {
            ui.colored_label(
                egui::Color32::from_rgb(239, 112, 99),
                error.lines().next().unwrap_or(error),
            )
            .on_hover_text(error);
        }
        if start && !tab.running() {
            tab.start(ui.ctx());
        }
    }

    fn search_status(&self, ui: &mut Ui, tab: &SearchTab) {
        let Some(run) = &tab.run else {
            ui.label("Choose a folder and criteria, then press Search. Empty name searches all matching items.");
            return;
        };
        let p = run.progress.lock().unwrap().clone();
        let state = match p.outcome {
            Outcome::Running if run.cancel.load(Ordering::Relaxed) => "Stopping",
            Outcome::Running => "Searching",
            Outcome::Complete if p.skipped > 0 => "Completed with skipped items",
            Outcome::Complete => "Completed",
            Outcome::Cancelled => "Stopped · partial results",
            Outcome::Limited => "Limit reached · partial results",
        };
        let count = run.results.lock().unwrap().len();
        ui.label(format!(
            "{state} · {count} found · {} scanned · {} skipped",
            run.scanned.load(Ordering::Relaxed),
            p.skipped
        ));
        let description = if tab.running() {
            format!("{} · now: {}", tab.summary, p.current.display())
        } else {
            tab.summary.clone()
        };
        ui.label(
            egui::RichText::new(fit_text(
                ui.painter(),
                &description,
                &FontId::proportional(11.0),
                ui.available_width(),
            ))
            .small()
            .color(self.pal.text_weak),
        )
        .on_hover_text(description);
        if let Some(reason) = p.limit_reason {
            ui.colored_label(self.pal.text_important, reason);
        }
        if !p.problems.is_empty() {
            ui.collapsing(format!("Skipped locations ({})", p.skipped), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("search_problems")
                    .max_height(100.0)
                    .show(ui, |ui| {
                        for (path, error) in &p.problems {
                            ui.label(format!("{} — {error}", path.display()));
                        }
                        if p.skipped > p.problems.len() {
                            ui.label("Only the first 100 problems are listed.");
                        }
                    });
            });
        }
        if tab.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn search_table(
        &mut self,
        ui: &mut Ui,
        tab: &mut SearchTab,
        area: Rect,
        action: &mut Option<Action>,
    ) {
        let mut table = ui.new_child(UiBuilder::new().max_rect(area));
        table.set_clip_rect(area.intersect(ui.clip_rect()));
        table.spacing_mut().item_spacing.y = 0.0;
        if table
            .interact(area, tab.table_id(), Sense::click())
            .clicked()
        {
            table.memory_mut(|m| m.request_focus(tab.table_id()));
        }
        let gutter = table.spacing().scroll.allocated_width().max(6.0);
        let row_width = area.width() - gutter;
        let columns = columns(row_width);
        let (header, _) = table.allocate_exact_size(vec2(area.width(), 26.0), Sense::hover());
        table
            .painter()
            .rect_filled(header, self.pal.corner_small(), self.pal.raised);
        let mut x = header.min.x;
        for &(column, width) in &columns {
            let r = Rect::from_min_size(pos2(x, header.min.y), vec2(width, header.height()));
            x += width;
            let response =
                table.interact(r, table.make_persistent_id(column.label()), Sense::click());
            if response.clicked() {
                if tab.sort == column {
                    tab.ascending = !tab.ascending;
                } else {
                    tab.sort = column;
                    tab.ascending = true;
                }
                tab.sort_dirty = true;
                tab.reveal = tab.cursor.is_some();
            }
            let drawn = table.painter().text(
                r.left_center() + vec2(6.0, 0.0),
                Align2::LEFT_CENTER,
                column.label(),
                FontId::proportional(12.0),
                self.pal.text,
            );
            if tab.sort == column {
                icons::sort_arrow(
                    table.painter(),
                    pos2(drawn.max.x + 8.0, r.center().y),
                    tab.ascending,
                    self.pal.text,
                );
            }
        }
        if tab.sort_dirty {
            tab.collect();
        }
        let Some(results) = tab.run.as_ref().map(|run| run.results.clone()) else {
            return;
        };
        let rh = self.row_h() + 6.0;
        let viewport_h = table.available_height().max(rh);
        tab.page_rows = (viewport_h / rh).floor().max(1.0) as usize;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(("search_rows", tab.id, tab.run_id))
            .auto_shrink([false, false]);
        if tab.reveal {
            if let Some(row) = tab.view.iter().position(|&i| Some(i) == tab.cursor_index) {
                let top = row as f32 * rh;
                let offset = if top < tab.scroll {
                    top
                } else if top + rh > tab.scroll + viewport_h {
                    top + rh - viewport_h
                } else {
                    tab.scroll
                };
                scroll = scroll.vertical_scroll_offset(offset.max(0.0));
            }
            tab.reveal = false;
        }
        let output = scroll.show_rows(&mut table, rh, tab.view.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let visible: Vec<(usize, Entry)> = {
                let entries = results.lock().unwrap();
                range
                    .filter_map(|row| {
                        tab.view
                            .get(row)
                            .and_then(|&i| entries.get(i))
                            .map(|e| (row, e.clone()))
                    })
                    .collect()
            };
            for (row, e) in visible {
                let (r, response) = ui.allocate_exact_size(vec2(row_width, rh), Sense::click());
                let selected = tab.selected.contains(&e.path);
                if selected || response.hovered() {
                    ui.painter().rect_filled(
                        Rect::from_min_max(r.min + vec2(gutter, 0.0), r.max),
                        self.pal.corner(),
                        if selected {
                            self.pal.selection()
                        } else {
                            self.pal.hover
                        },
                    );
                }
                let mut x = r.min.x;
                for &(column, width) in &columns {
                    let cell = Rect::from_min_size(pos2(x, r.min.y), vec2(width, r.height()));
                    x += width;
                    let mut left = cell.min.x + 6.0;
                    if column == Column::Name {
                        let icon = Rect::from_center_size(
                            pos2(left + 8.0, r.center().y),
                            vec2(16.0, 16.0),
                        );
                        if e.is_dir {
                            icons::folder(ui.painter(), icon, self.folder_color(&e.path));
                        } else {
                            icons::entry(ui.painter(), icon, e.kind, e.ext(), self.pal.dark, false);
                        }
                        left += 24.0;
                    }
                    let value = match column {
                        Column::Name => e.name().to_string(),
                        Column::Location => e
                            .path
                            .parent()
                            .map_or(String::new(), |p| p.display().to_string()),
                        Column::Type => {
                            if e.is_dir {
                                "Folder".into()
                            } else {
                                e.ext().to_string()
                            }
                        }
                        Column::Size => {
                            if e.is_dir {
                                "—".into()
                            } else {
                                fsx::fmt_size(e.size, self.st.mebibytes)
                            }
                        }
                        Column::Modified => fsx::fmt_datetime(e.modified),
                    };
                    let font = FontId::proportional(if column == Column::Name {
                        self.st.font_size
                    } else {
                        11.0
                    });
                    let text = fit_text(ui.painter(), &value, &font, cell.max.x - left - 6.0);
                    ui.painter_at(cell.intersect(ui.clip_rect())).text(
                        pos2(left, r.center().y),
                        Align2::LEFT_CENTER,
                        text,
                        font,
                        if column == Column::Name {
                            self.pal.text
                        } else {
                            self.pal.text_weak
                        },
                    );
                }
                if response.clicked() {
                    tab.select(row, ui.input(|i| i.modifiers));
                    ui.memory_mut(|m| m.request_focus(tab.table_id()));
                }
                if response.double_clicked() {
                    *action = Some(Action::Open);
                }
                if response.secondary_clicked() {
                    if !tab.selected.contains(&e.path) {
                        tab.select(row, Modifiers::NONE);
                    }
                    tab.cursor = Some(e.path.clone());
                    tab.cursor_index = Some(tab.view[row]);
                }
                response
                    .on_hover_text(e.path.display().to_string())
                    .context_menu(|ui| {
                        for (text, a) in [
                            ("Show in folder", Action::ShowFolder),
                            ("Open", Action::Open),
                            ("Copy", Action::Copy),
                            ("Cut", Action::Cut),
                            ("Copy paths", Action::CopyPaths),
                            ("Copy to…", Action::CopyTo),
                            ("Move to…", Action::MoveTo),
                            ("Move to Trash…", Action::Trash),
                        ] {
                            if ui.button(text).clicked() {
                                *action = Some(a);
                                ui.close();
                            }
                        }
                    });
            }
        });
        tab.scroll = output.state.clamped_offset().y;
        if tab.view.is_empty() && !tab.running() {
            table.label(if tab.run.is_some() {
                "No matches. Change the scope or relax the filters."
            } else {
                "Results will appear here."
            });
        }
    }

    fn search_keys(&mut self, ui: &Ui, tab: &mut SearchTab, action: &mut Option<Action>) {
        ui.input_mut(|input| {
            input.events.retain(|event| match event {
                egui::Event::Copy => {
                    if !tab.selected.is_empty() {
                        *action = Some(Action::Copy);
                    }
                    false
                }
                egui::Event::Cut => {
                    if !tab.selected.is_empty() {
                        *action = Some(Action::Cut);
                    }
                    false
                }
                _ => true,
            });
            if input.consume_key(Modifiers::COMMAND, Key::A)
                && let Some(run) = &tab.run
            {
                tab.selected = run
                    .results
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
            }
            for (key, a) in [(Key::C, Action::Copy), (Key::X, Action::Cut)] {
                if input.consume_key(Modifiers::COMMAND, key) && !tab.selected.is_empty() {
                    *action = Some(a);
                }
            }
            if input.consume_key(Modifiers::NONE, Key::Delete) && !tab.selected.is_empty() {
                *action = Some(Action::Trash);
            }
            if input.consume_key(Modifiers::NONE, Key::Enter) && tab.cursor.is_some() {
                *action = Some(Action::Open);
            }
            for (key, by) in [
                (Key::ArrowDown, 1isize),
                (Key::ArrowUp, -1),
                (Key::PageDown, tab.page_rows as isize),
                (Key::PageUp, -(tab.page_rows as isize)),
                (Key::Home, isize::MIN),
                (Key::End, isize::MAX),
            ] {
                if input.key_pressed(key) && !tab.view.is_empty() {
                    let row = tab.view.iter().position(|&i| Some(i) == tab.cursor_index);
                    let next = if key == Key::End {
                        tab.view.len() - 1
                    } else if key == Key::Home {
                        0
                    } else {
                        row.map_or(0, |row| {
                            (row as isize)
                                .saturating_add(by)
                                .clamp(0, tab.view.len() as isize - 1)
                                as usize
                        })
                    };
                    tab.select(next, input.modifiers);
                    tab.reveal = true;
                    input.consume_key(input.modifiers, key);
                }
            }
        });
        if tab.reveal {
            ui.ctx().request_repaint();
        }
    }

    fn search_preview(&mut self, ui: &mut Ui, tab: &mut SearchTab, area: Rect) {
        ui.painter().rect_filled(
            area,
            self.pal.corner_large(),
            self.pal.surface(self.pal.card),
        );
        ui.painter().rect_stroke(
            area,
            self.pal.corner_large(),
            self.pal.outline(),
            egui::StrokeKind::Inside,
        );
        let mut body = ui.new_child(UiBuilder::new().max_rect(area.shrink(10.0)));
        body.set_clip_rect(area.intersect(ui.clip_rect()));
        let Some(entry) = tab.cursor_entry() else {
            body.label("Select a result to preview it.");
            return;
        };
        body.label(egui::RichText::new(entry.name()).strong());
        body.label(egui::RichText::new(entry.path.display().to_string()).small());
        let preview = tab.preview.get_or_insert_with(Preview::new);
        preview.want(Some(PreviewKey::of(&entry)), ui.ctx());
        preview.receive();
        preview.expire();
        let r = Rect::from_min_size(
            body.cursor().min + vec2(0.0, 8.0),
            vec2(body.available_width(), body.available_height().max(40.0)),
        );
        self.preview_content_box(&mut body, r, Some(&entry), preview);
        match &preview.content {
            PreviewContent::Loading => {
                body.label("Loading preview…");
            }
            PreviewContent::Error(error) => {
                body.label(error);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "ui_search_tests.rs"]
mod tests;
