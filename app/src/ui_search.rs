//! Search workspaces: independent scopes, retained result tabs and file actions.
//! Selection belongs to result paths, never to the file pane behind this window.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use chrono::{Local, NaiveDate, TimeZone};
use egui::{Align, Align2, FontId, Key, Layout, Modifiers, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use crate::app::{Act, App, Notice};
use crate::chrome::{self, Icon, Panel};
use crate::fsx::{self, Entry};
use crate::icons;
use crate::ops::OpKind;
use crate::preview::{Key as PreviewKey, Preview, PreviewContent};
use crate::search::{self, Criteria, FileType, NameMode, Outcome, Search};
use crate::theme::blend;
use crate::ui::{card, chip, fit_text, icon_button, take_right};

const TABS_MAX: usize = 8;
/// Results a new search stops at.
const LIMIT: usize = 10_000;
/// The narrowest and widest a result tab is drawn.
const TAB_W: (f32, f32) = (96.0, 220.0);
/// The width below which the preview no longer fits beside the results.
const PREVIEW_ROOM: f32 = 620.0;
const ERROR: egui::Color32 = egui::Color32::from_rgb(239, 112, 99);
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
    /// How many of the filters behind "More filters" are set.
    fn more_used(&self) -> usize {
        [
            &self.min_size,
            &self.max_size,
            &self.from,
            &self.through,
            &self.excluded,
        ]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .count()
            + usize::from(self.limit != LIMIT)
    }
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
            limit: LIMIT,
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
    /// The folder the shown results were searched in; locations are shown from it.
    scope: PathBuf,
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
    show_problems: bool,
}
impl SearchTab {
    fn new(id: u64, root: PathBuf, hidden: bool) -> Self {
        Self {
            id,
            inputs: Inputs::new(root, hidden),
            run: None,
            summary: String::new(),
            scope: PathBuf::new(),
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
            show_problems: false,
        }
    }
    fn title(&self) -> String {
        if self.summary.is_empty() {
            "New search".into()
        } else {
            self.summary.clone()
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
                    "All items"
                } else {
                    criteria.name.trim()
                },
                fsx::display_name(&root)
            );
            let scope = root.clone();
            search::start(root, criteria, ctx.clone()).map(|run| (run, summary, scope))
        }) {
            Ok((run, summary, scope)) => {
                self.run = Some(run);
                self.summary = summary;
                self.scope = scope;
                self.show_problems = false;
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

fn mode_label(mode: NameMode) -> (&'static str, &'static str) {
    match mode {
        NameMode::Words => ("Words", "The name contains all these words, in any order"),
        NameMode::Wildcard => (
            "Wildcard",
            "Masks such as *.mp3;*.flac, separated with semicolons",
        ),
        NameMode::Regex => ("Regex", "A regular expression matched against the name"),
    }
}

/// A label at the start of a row of the query card, so the rows line up.
fn caption(ui: &mut Ui, text: &str, pal: &crate::theme::Palette) {
    let (r, _) = ui.allocate_exact_size(vec2(56.0, 22.0), Sense::hover());
    ui.painter().text(
        r.left_center(),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.0),
        pal.text_weak,
    );
}

/// `12345` as `12,345`.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plural(n: usize) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{} items", grouped(n))
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

/// Where a result lies, from the searched folder's own name down, so the common
/// beginning of every path is not repeated on each row.
fn location(scope: &std::path::Path, path: &std::path::Path) -> String {
    let Some(parent) = path.parent() else {
        return String::new();
    };
    let base = scope.parent().unwrap_or(scope);
    match parent.strip_prefix(base) {
        Ok(relative) if !relative.as_os_str().is_empty() && !scope.as_os_str().is_empty() => {
            relative.display().to_string()
        }
        _ => parent.display().to_string(),
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
                        self.search_tabs(ui, &mut window);
                        let tab = &mut window.tabs[window.active];
                        ui.push_id(tab.id, |ui| {
                            self.search_inputs(ui, tab);
                            tab.collect();
                            self.search_status(ui, tab);
                            self.search_toolbar(ui, tab, &mut action);
                            self.search_transfer(ui, tab);
                            ui.add_space(2.0);
                            let height = ui.available_height().max(50.0);
                            let (mut area, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), height),
                                Sense::hover(),
                            );
                            let preview_width = (area.width() * 0.3).clamp(220.0, 320.0);
                            let preview = if tab.preview_on && area.width() >= PREVIEW_ROOM {
                                let r = take_right(&mut area, preview_width);
                                area.max.x -= 8.0;
                                Some(r)
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

    /// The result tabs, drawn like the file panels' tabs.
    fn search_tabs(&self, ui: &mut Ui, window: &mut SearchUi) {
        let pal = self.pal;
        let font = FontId::proportional(12.0);
        let closable = window.tabs.len() > 1;
        let mut next = window.active;
        let mut remove = None;
        let mut add = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let room = (ui.available_width() - 32.0).max(0.0);
            let cap = (room / window.tabs.len() as f32 - 2.0).clamp(60.0, TAB_W.1);
            for (i, tab) in window.tabs.iter().enumerate() {
                let title = tab.title();
                let wanted = ui
                    .painter()
                    .layout_no_wrap(title.clone(), font.clone(), pal.text)
                    .size()
                    .x;
                let width = (wanted + if closable { 50.0 } else { 34.0 })
                    .clamp(TAB_W.0, TAB_W.1)
                    .min(cap);
                let (r, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
                let selected = i == window.active;
                let look = crate::theme::tab_look(&pal, selected, true, response.hovered());
                let painter = ui.painter_at(r);
                painter.rect_filled(r, pal.corner(), look.fill);
                let mark =
                    Rect::from_center_size(pos2(r.left() + 13.0, r.center().y), vec2(14.0, 14.0));
                if tab.running() {
                    let t = ui.input(|i| i.time) as f32 * 5.0;
                    painter.circle_stroke(
                        mark.center(),
                        4.5,
                        egui::Stroke::new(1.5, pal.text_weak.gamma_multiply(0.4)),
                    );
                    painter.circle_filled(
                        mark.center() + 4.5 * vec2(t.cos(), t.sin()),
                        1.8,
                        pal.accent,
                    );
                } else {
                    icons::face(&painter, mark, "🔍", 10.0, look.text);
                }
                let close =
                    Rect::from_center_size(pos2(r.right() - 12.0, r.center().y), vec2(16.0, 16.0));
                let text_right = if closable {
                    close.left() - 2.0
                } else {
                    r.right() - 8.0
                };
                let left = r.left() + 25.0;
                painter.text(
                    pos2(left, icons::text_level(&painter, mark.center().y, font.clone())),
                    Align2::LEFT_CENTER,
                    fit_text(&painter, &title, &font, text_right - left),
                    font.clone(),
                    look.text,
                );
                let on_close = closable
                    && response
                        .hover_pos()
                        .or(response.interact_pointer_pos())
                        .is_some_and(|p| close.contains(p));
                if closable && (selected || response.hovered()) {
                    if on_close {
                        painter.rect_filled(close, pal.corner(), pal.hover);
                    }
                    icons::cross(
                        &painter,
                        close.center(),
                        3.5,
                        egui::Stroke::new(1.2, look.text),
                    );
                }
                let hover = if tab.scope.as_os_str().is_empty() {
                    title
                } else {
                    format!("{}\nin {}", tab.summary, tab.scope.display())
                };
                let response = response.on_hover_text(hover);
                if response.clicked() {
                    if on_close { remove = Some(i) } else { next = i }
                }
                if closable && response.middle_clicked() {
                    remove = Some(i);
                }
            }
            ui.add_enabled_ui(window.tabs.len() < TABS_MAX, |ui| {
                add = icon_button(ui, "#plus", "New search tab", false, &pal).clicked();
            });
        });
        if add {
            window.add_tab(active_root(self), self.st.show_hidden);
            next = window.active;
        }
        if let Some(i) = remove {
            window.tabs.remove(i);
            if next > i || next >= window.tabs.len() {
                next = next.saturating_sub(1);
            }
        }
        window.active = next.min(window.tabs.len() - 1);
        for (i, tab) in window.tabs.iter_mut().enumerate() {
            if i != window.active {
                tab.preview = None;
            }
        }
        ui.add_space(4.0);
    }

    /// What to look for and where: the name, the folder and the filters, on one card.
    fn search_inputs(&mut self, ui: &mut Ui, tab: &mut SearchTab) {
        let pal = self.pal;
        let mut start = std::mem::take(&mut tab.auto_start);
        card(ui, &pal, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 30.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    let (text, fill) = if tab.running() {
                        ("Stop", pal.raised)
                    } else {
                        ("Search", blend(pal.panel, pal.accent, 0.6))
                    };
                    let button = ui.add(
                        egui::Button::new(egui::RichText::new(text).color(pal.text))
                            .fill(fill)
                            .corner_radius(pal.corner())
                            .min_size(vec2(84.0, 28.0)),
                    );
                    if button.clicked() {
                        if let Some(run) = tab.run.as_ref().filter(|_| tab.running()) {
                            run.cancel.store(true, Ordering::Relaxed);
                        } else {
                            start = true;
                        }
                    }
                    ui.add_space(6.0);
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for mode in [NameMode::Regex, NameMode::Wildcard, NameMode::Words] {
                        let (label, tip) = mode_label(mode);
                        if chip(ui, label, tab.inputs.mode == mode, &pal)
                            .on_hover_text(tip)
                            .clicked()
                        {
                            tab.inputs.mode = mode;
                        }
                    }
                    ui.add_space(8.0);
                    let hint = match tab.inputs.mode {
                        NameMode::Words => {
                            "Name contains… (empty finds everything the filters allow)"
                        }
                        NameMode::Wildcard => "*.mp3;*.flac",
                        NameMode::Regex => "^report.*\\.txt$",
                    };
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut tab.inputs.name)
                            .hint_text(hint)
                            .font(FontId::proportional(self.st.font_size + 1.0))
                            .margin(vec2(8.0, 5.0))
                            .desired_width(ui.available_width()),
                    );
                    if std::mem::take(&mut tab.focus) {
                        response.request_focus();
                    }
                    start |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                },
            );
            // A fixed height, so the field and the button share one middle line.
            let row = vec2(ui.available_width(), 26.0);
            ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
                caption(ui, "Look in", &pal);
                let width = (ui.available_width() - 34.0).max(120.0);
                let id = ui.make_persistent_id("search_scope");
                let font = egui::TextStyle::Body.resolve(ui.style());
                let output = self.path_field(ui, id, &mut tab.inputs.root, font, |edit| {
                    edit.desired_width(width)
                });
                start |= output.0.response.response.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter));
                if icon_button(ui, "🗀", "Use the active panel's folder", false, &pal).clicked() {
                    tab.inputs.root = active_root(self).display().to_string();
                }
            });
            ui.horizontal_wrapped(|ui| {
                caption(ui, "Options", &pal);
                ui.spacing_mut().item_spacing.x = 2.0;
                for (label, on, tip) in [
                    (
                        "Subfolders",
                        &mut tab.inputs.recursive,
                        "Also look inside subfolders",
                    ),
                    (
                        "Hidden",
                        &mut tab.inputs.hidden,
                        "Include hidden files and folders",
                    ),
                    (
                        "Match case",
                        &mut tab.inputs.case_sensitive,
                        "Tell capital and small letters apart",
                    ),
                ] {
                    if chip(ui, label, *on, &pal).on_hover_text(tip).clicked() {
                        *on = !*on;
                    }
                }
                ui.add_space(6.0);
                egui::ComboBox::from_id_salt("file_type")
                    .selected_text(type_label(tab.inputs.file_type))
                    .width(120.0)
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
                ui.add_space(6.0);
                let used = tab.inputs.more_used();
                let label = if used > 0 {
                    format!("More filters · {used}")
                } else {
                    "More filters".into()
                };
                if chip(ui, &label, tab.inputs.advanced || used > 0, &pal)
                    .on_hover_text("Size, date, excluded folders and the result limit")
                    .clicked()
                {
                    tab.inputs.advanced = !tab.inputs.advanced;
                }
            });
            if tab.inputs.advanced {
                let field = |ui: &mut Ui, text: &mut String, hint: &str, width: f32| {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .hint_text(hint)
                            .desired_width(width),
                    )
                };
                ui.horizontal_wrapped(|ui| {
                    caption(ui, "Size", &pal);
                    field(ui, &mut tab.inputs.min_size, "from, e.g. 10 MB", 110.0);
                    ui.label("–");
                    field(ui, &mut tab.inputs.max_size, "to, e.g. 1 GiB", 110.0);
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("Modified").color(pal.text_weak));
                    field(ui, &mut tab.inputs.from, "YYYY-MM-DD", 96.0);
                    ui.label("–");
                    field(ui, &mut tab.inputs.through, "YYYY-MM-DD", 96.0);
                });
                ui.horizontal_wrapped(|ui| {
                    caption(ui, "Skip", &pal);
                    field(
                        ui,
                        &mut tab.inputs.excluded,
                        ".git;node_modules;target",
                        254.0,
                    )
                    .on_hover_text(
                        "Folder names or relative path masks, separated with semicolons",
                    );
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("At most").color(pal.text_weak));
                    ui.add(
                        egui::DragValue::new(&mut tab.inputs.limit)
                            .range(100..=search::RESULTS_MAX)
                            .suffix(" results"),
                    );
                });
            }
            if let Some(error) = &tab.error {
                ui.colored_label(ERROR, error.lines().next().unwrap_or(error))
                    .on_hover_text(error);
            }
        });
        if start && !tab.running() {
            tab.start(ui.ctx());
        }
    }

    /// One line on how the search went, and the skipped places when asked for.
    fn search_status(&self, ui: &mut Ui, tab: &mut SearchTab) {
        let pal = self.pal;
        let weak = |text: String| egui::RichText::new(text).color(pal.text_weak);
        ui.add_space(2.0);
        let Some(run) = &tab.run else {
            ui.label(weak(
                "Press Enter to search. An empty name lists everything the filters allow.".into(),
            ));
            return;
        };
        let p = run.progress.lock().unwrap().clone();
        let found = run.results.lock().unwrap().len();
        let running = tab.running();
        ui.horizontal(|ui| {
            ui.set_min_height(22.0);
            let (state, color) = match p.outcome {
                Outcome::Running if run.cancel.load(Ordering::Relaxed) => ("Stopping…", pal.text),
                Outcome::Running => ("Searching…", pal.text),
                Outcome::Complete => ("Done", pal.text),
                Outcome::Cancelled => ("Stopped", pal.text_important),
                // The reason that follows says which limit.
                Outcome::Limited => ("Partial results", pal.text_important),
            };
            if running {
                ui.add(egui::Spinner::new().size(12.0).color(pal.accent));
            }
            ui.label(egui::RichText::new(state).color(color).strong());
            ui.label(weak(format!(
                "{} found · {} scanned",
                grouped(found),
                grouped(run.scanned.load(Ordering::Relaxed))
            )));
            if p.skipped > 0
                && chip(
                    ui,
                    &format!("{} skipped", grouped(p.skipped)),
                    tab.show_problems,
                    &pal,
                )
                .on_hover_text("Places that could not be read")
                .clicked()
            {
                tab.show_problems = !tab.show_problems;
            }
            if let Some(reason) = p.limit_reason {
                ui.label(egui::RichText::new(format!("· {reason}")).color(pal.text_weak));
            }
            if running {
                let now = p.current.display().to_string();
                let font = FontId::proportional(11.0);
                let text = fit_text(ui.painter(), &now, &font, ui.available_width() - 8.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(egui::RichText::new(text).font(font).color(pal.text_weak))
                        .on_hover_text(now);
                });
            }
        });
        if tab.show_problems && !p.problems.is_empty() {
            card(ui, &pal, |ui| {
                ui.set_min_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .id_salt("search_problems")
                    .max_height(100.0)
                    .show(ui, |ui| {
                        for (path, error) in &p.problems {
                            ui.label(format!("{} — {error}", path.display()));
                        }
                        if p.skipped > p.problems.len() {
                            ui.label(weak("Only the first 100 problems are listed.".into()));
                        }
                    });
            });
        }
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    /// What can be done with the selected results.
    fn search_toolbar(&self, ui: &mut Ui, tab: &mut SearchTab, action: &mut Option<Action>) {
        let pal = self.pal;
        let ready = !tab.selected.is_empty();
        let can_preview = ui.available_width() >= PREVIEW_ROOM;
        ui.horizontal(|ui| {
            ui.set_min_height(24.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.add_enabled_ui(ready, |ui| {
                for (n, group) in [
                    &[
                        ("Open", Action::Open),
                        ("Show in folder", Action::ShowFolder),
                    ][..],
                    &[
                        ("Copy", Action::Copy),
                        ("Cut", Action::Cut),
                        ("Copy to…", Action::CopyTo),
                        ("Move to…", Action::MoveTo),
                    ],
                    &[("Trash…", Action::Trash)],
                ]
                .into_iter()
                .enumerate()
                {
                    if n > 0 {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);
                    }
                    for &(text, act) in group {
                        if chip(ui, text, false, &pal).clicked() {
                            *action = Some(act);
                            ui.memory_mut(|m| m.request_focus(tab.table_id()));
                        }
                    }
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(can_preview, |ui| {
                    if chip(ui, "Preview", tab.preview_on && can_preview, &pal)
                        .on_hover_text("Show the selected result beside the list")
                        .on_disabled_hover_text(
                            "Widen the search window to show a preview beside the results",
                        )
                        .clicked()
                    {
                        tab.preview_on = !tab.preview_on;
                    }
                });
                ui.add_space(8.0);
                let count = if ready {
                    format!("{} selected", grouped(tab.selected.len()))
                } else {
                    "Select results to act on them".into()
                };
                ui.label(egui::RichText::new(count).color(pal.text_weak));
            });
        });
    }

    /// The destination of a pending "Copy to…" or "Move to…".
    fn search_transfer(&mut self, ui: &mut Ui, tab: &mut SearchTab) {
        let pal = self.pal;
        let Some(transfer) = &mut tab.transfer else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        card(ui, &pal, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let verb = if transfer.moving { "Move" } else { "Copy" };
                ui.label(format!("{verb} {} into", plural(transfer.paths.len())));
                let width = (ui.available_width() - 130.0).max(80.0);
                let id = ui.make_persistent_id("search_destination");
                let font = egui::TextStyle::Body.resolve(ui.style());
                let output = self.path_field(ui, id, &mut transfer.destination, font, |edit| {
                    edit.desired_width(width)
                });
                // Enter is left to the path completion: a half-typed folder must not
                // receive the files.
                let _ = output;
                apply = ui
                    .add(egui::Button::new(verb).fill(blend(pal.panel, pal.accent, 0.6)))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if apply {
            let destination = crate::ui_list::typed_path(&transfer.destination);
            if destination.is_absolute() {
                let kind = if transfer.moving {
                    OpKind::Move
                } else {
                    OpKind::Copy
                };
                self.start_job(kind, transfer.paths.clone(), Some(destination), ui.ctx());
                tab.transfer = None;
                tab.error = None;
            } else {
                tab.error = Some("Choose an absolute destination folder".into());
            }
        } else if cancel {
            tab.transfer = None;
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
        table.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            egui::Stroke::new(1.0, self.pal.border),
        );
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
            let sorted = tab.sort == column;
            let color = if sorted || response.hovered() {
                self.pal.text
            } else {
                self.pal.text_weak
            };
            let right = column == Column::Size;
            let drawn = table.painter().text(
                if right {
                    r.right_center() - vec2(if sorted { 22.0 } else { 8.0 }, 0.0)
                } else {
                    r.left_center() + vec2(6.0, 0.0)
                },
                if right {
                    Align2::RIGHT_CENTER
                } else {
                    Align2::LEFT_CENTER
                },
                column.label(),
                FontId::proportional(12.0),
                color,
            );
            if sorted {
                icons::sort_arrow(
                    table.painter(),
                    pos2(drawn.max.x + 8.0, r.center().y),
                    tab.ascending,
                    color,
                );
            }
        }
        if tab.sort_dirty {
            tab.collect();
        }
        let Some(results) = tab.run.as_ref().map(|run| run.results.clone()) else {
            return;
        };
        let rh = self.row_h() + 4.0;
        let focused = table.memory(|m| m.has_focus(tab.table_id()));
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
                let at_cursor = tab.cursor.as_ref() == Some(&e.path);
                let (fill, corner) = if at_cursor && focused {
                    (self.pal.cursor(selected), self.pal.corner())
                } else if selected {
                    (self.pal.selection(), self.pal.corner())
                } else if response.hovered() {
                    (self.pal.hover, self.pal.corner())
                } else if row % 2 == 1 {
                    (self.pal.stripe(), egui::CornerRadius::ZERO)
                } else {
                    (egui::Color32::TRANSPARENT, egui::CornerRadius::ZERO)
                };
                ui.painter().rect_filled(r, corner, fill);
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
                        Column::Location => location(&tab.scope, &e.path),
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
                    let text = fit_text(ui.painter(), &value, &font, cell.max.x - left - 8.0);
                    let right = column == Column::Size;
                    ui.painter_at(cell.intersect(ui.clip_rect())).text(
                        if right {
                            pos2(cell.max.x - 8.0, r.center().y)
                        } else {
                            pos2(left, r.center().y)
                        },
                        if right {
                            Align2::RIGHT_CENTER
                        } else {
                            Align2::LEFT_CENTER
                        },
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
        if tab.view.is_empty() {
            let text = if tab.running() {
                "Searching…"
            } else if tab.run.is_some() {
                "No matches. Try another folder or fewer filters."
            } else {
                "Results will appear here."
            };
            table.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(13.0),
                self.pal.text_weak,
            );
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
