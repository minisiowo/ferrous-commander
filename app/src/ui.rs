//! Window frame: keyboard, sidebar, browser panes, tabs and folder columns.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Key, Margin, Modifiers, Rect, RichText, Sense,
    Stroke, Ui, UiBuilder, pos2, vec2,
};

use crate::keymap::{self, Chord, Command, Input, Mode, Outcome, Target};
use crate::app::{Act, App, CrumbDrag, Dialog, DragPaths, DropTarget, FavDrag, ToastKind, is_computer};
use crate::fsx;
use crate::icons;
use crate::settings::{Layout, SidebarSection};
use crate::theme::{Palette, ThemeName, blend, hex};

// ---- sizes ----
pub const SIDEBAR_W: f32 = crate::defaults::SIDEBAR_WIDTH;
pub const TOOLBAR_H: f32 = 36.0;
pub const DETAILS_H: f32 = crate::defaults::DETAILS_HEIGHT;
pub const PREVIEW_W: f32 = crate::defaults::PREVIEW_WIDTH;
pub const COLLAPSE_W: f32 = crate::defaults::COLLAPSE_WIDTH;
pub const COL_W: f32 = crate::defaults::COLUMN_WIDTH;
pub const COL_SMALL_W: f32 = crate::defaults::COLUMN_SMALL_WIDTH;
pub const COLS_H: f32 = crate::defaults::COLUMNS_HEIGHT;
pub const CURRENT_COL_W: f32 = crate::defaults::CURRENT_COLUMN_WIDTH;
pub const ITEM_PAD: f32 = crate::defaults::LIST_ITEM_PADDING;
pub const TAB_H: f32 = 30.0;
#[derive(Clone)]
/// `tab` is `None` when the whole panel is dragged by its handle.
struct TabDrag { pane: usize, tab: Option<usize> }

pub const HEADER_H: f32 = 52.0;
pub const STATUS_H: f32 = 22.0;

pub fn take_top(r: &mut Rect, h: f32) -> Rect {
    let h = h.min(r.height());
    let t = Rect::from_min_size(r.min, vec2(r.width(), h));
    r.min.y += h;
    t
}
pub fn take_bottom(r: &mut Rect, h: f32) -> Rect {
    let h = h.min(r.height());
    let b = Rect::from_min_max(pos2(r.min.x, r.max.y - h), r.max);
    r.max.y -= h;
    b
}
pub fn take_left(r: &mut Rect, w: f32) -> Rect {
    let w = w.min(r.width());
    let l = Rect::from_min_size(r.min, vec2(w, r.height()));
    r.min.x += w;
    l
}
pub fn take_right(r: &mut Rect, w: f32) -> Rect {
    let w = w.min(r.width());
    let l = Rect::from_min_max(pos2(r.max.x - w, r.min.y), r.max);
    r.max.x -= w;
    l
}

/// Trim text in the middle to fit.
pub fn fit_text(painter: &egui::Painter, text: &str, font: &FontId, max_w: f32) -> String {
    let w = |s: &str| {
        painter
            .layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if max_w <= 8.0 {
        return String::new();
    }
    if w(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |n: usize| -> String {
        let keep_end = (n * 2 / 5).min(n);
        chars[..n - keep_end]
            .iter()
            .chain(['…'].iter())
            .chain(chars[chars.len() - keep_end..].iter())
            .collect()
    };
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if w(&build(mid)) <= max_w {
            lo = mid
        } else {
            hi = mid - 1
        }
    }
    build(lo)
}

pub fn icon_button(ui: &mut Ui, text: &str, tip: &str, on: bool, pal: &Palette) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    let bg = if on {
        blend(pal.panel, pal.accent, 0.45)
    } else if resp.hovered() {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, CornerRadius::same(1), bg);
    let col = if resp.hovered() || on {
        pal.text
    } else {
        pal.text_weak
    };
    match text.strip_prefix('#') {
        Some(name) => icons::glyph(ui.painter(), r, name, col),
        None => {
            ui.painter().text(
                r.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(14.0),
                col,
            );
        }
    }
    resp.on_hover_text(tip)
}

pub fn chip(ui: &mut Ui, text: &str, on: bool, pal: &Palette) -> egui::Response {
    sort_chip(ui, text, on, None, pal)
}

pub fn sort_chip(ui: &mut Ui, text: &str, on: bool, ascending: Option<bool>, pal: &Palette) -> egui::Response {
    let font = FontId::proportional(11.0);
    let gw = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), pal.text)
        .size()
        .x;
    let arrow_w = if ascending.is_some() { 12.0 } else { 0.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(gw + 14.0 + arrow_w, 22.0), Sense::click());
    let bg = if on {
        blend(pal.panel, pal.accent, 0.35)
    } else if resp.hovered() {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, CornerRadius::same(1), bg);
    ui.painter().text(
        r.center() - vec2(arrow_w / 2.0, 0.0),
        Align2::CENTER_CENTER,
        text,
        font,
        if on { pal.text } else { pal.text_weak },
    );
    if let Some(ascending) = ascending {
        icons::sort_arrow(ui.painter(), r.right_center() - vec2(9.0, 0.0), ascending, pal.text);
    }
    resp
}

pub fn card<R>(
    ui: &mut Ui,
    pal: &Palette,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let stroke = if pal.card == pal.window {
        Stroke::new(1.0, pal.border)
    } else {
        Stroke::NONE
    };
    Frame::NONE
        .fill(pal.card)
        .corner_radius(CornerRadius::same(1))
        .inner_margin(Margin::symmetric(8, 6))
        .stroke(stroke)
        .show(ui, add)
}

// Metadata for favorites/history may involve sleeping network shares. Never read it
// on the render thread. One bounded worker refreshes a small cache periodically.
fn side_metadata(path: &Path) -> (bool, bool) {
    struct Cache {
        tx: std::sync::mpsc::SyncSender<PathBuf>,
        rx: std::sync::mpsc::Receiver<(PathBuf, bool, bool)>,
        entries: std::collections::HashMap<PathBuf, (bool, bool, std::time::Instant)>,
        pending: std::collections::HashSet<PathBuf>,
    }
    thread_local! {
        static CACHE: std::cell::RefCell<Cache> = {
            let (tx, requests) = std::sync::mpsc::sync_channel::<PathBuf>(128);
            let (results, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                while let Ok(path) = requests.recv() {
                    let md = std::fs::metadata(&path);
                    let exists = md.is_ok();
                    let file = md.is_ok_and(|m| m.is_file());
                    if results.send((path, exists, file)).is_err() { break; }
                }
            });
            std::cell::RefCell::new(Cache { tx, rx, entries: Default::default(), pending: Default::default() })
        };
    }
    CACHE.with(|cache| {
        let mut c = cache.borrow_mut();
        while let Ok((p, exists, file)) = c.rx.try_recv() {
            c.pending.remove(&p);
            if c.entries.len() >= 256 {
                c.entries
                    .retain(|_, (_, _, at)| at.elapsed().as_secs() < 10);
            }
            c.entries
                .insert(p, (exists, file, std::time::Instant::now()));
        }
        let info = c.entries.get(path).copied();
        if info.is_none_or(|(_, _, at)| at.elapsed().as_secs() >= 10) && !c.pending.contains(path)
            && c.tx.try_send(path.to_path_buf()).is_ok() {
                c.pending.insert(path.to_path_buf());
            }
        info.map(|(exists, file, _)| (exists, file))
            .unwrap_or((true, false))
    })
}

/// Whether files can be dropped into a sidebar row's folder.
fn accepts_files(path: &Path) -> bool {
    let (exists, is_file) = side_metadata(path);
    exists && !is_file && !is_computer(path)
}

/// Where a drag at height `y` lands among a group's rows.
enum RowDrop {
    /// Inside the folder of this row.
    Into(usize),
    /// Between rows, before this index.
    At(usize),
}

/// A row that can take files keeps its top and bottom edges for placing a
/// shortcut beside it; other rows are simply split in half.
fn row_drop(rows: &[Rect], y: f32, accepts: impl Fn(usize) -> bool) -> RowDrop {
    let Some(k) = rows.iter().position(|r| y < r.bottom()) else { return RowDrop::At(rows.len()) };
    let r = rows[k];
    let edge = if accepts(k) { r.height() * 0.3 } else { r.height() / 2.0 };
    if y < r.top() + edge {
        RowDrop::At(k)
    } else if y >= r.bottom() - edge {
        RowDrop::At(k + 1)
    } else {
        RowDrop::Into(k)
    }
}

/// Draws the line showing where a shortcut will land in a group.
fn insertion_line(ui: &Ui, card: Rect, rows: &[Rect], at: usize, color: Color32) {
    let y = match (rows.get(at), rows.last()) {
        (Some(row), _) => row.top(),
        (None, Some(last)) => last.bottom(),
        (None, None) => card.bottom() - 4.0,
    };
    ui.painter().hline(card.shrink2(vec2(6.0, 0.0)).x_range(), y, Stroke::new(2.0, color));
}

/// Marks a sidebar row as the folder that dragged files will go into.
fn highlight_drop_row(ui: &Ui, row: Rect, color: Color32) {
    let painter = ui.painter();
    painter.rect_filled(row, CornerRadius::same(1), color.gamma_multiply(0.25));
    painter.rect_stroke(row, CornerRadius::same(1), Stroke::new(1.5, color), egui::StrokeKind::Inside);
}

impl App {
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.begin_frame(&ctx);
        self.keyboard(&ctx);
        let pal = self.pal;

        if self.st.show_sidebar {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(SIDEBAR_W)
                .size_range(180.0..=420.0)
                .show_separator_line(false)
                .frame(Frame::NONE.fill(pal.window).inner_margin(Margin {
                    left: 6,
                    right: 6,
                    top: 6,
                    bottom: 0,
                }))
                .show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(pal.window).inner_margin(Margin {
                left: 2,
                right: 0,
                top: 0,
                bottom: 0,
            }))
            .show(ui, |ui| self.browsers(ui));

        self.dialogs(&ctx);
        self.notices_ui(&ctx);
        self.guard_close(&ctx);
        self.preview_window_ui(&ctx);
        self.search_window(&ctx);
        self.settings_window(&ctx);
        self.overlays(&ctx);
        self.handle_drop(&ctx);
        self.end_frame(&ctx);
        self.screenshot(&ctx);
    }

    // ================================================================= keyboard

    fn keyboard(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() || !self.notices.is_empty() || ctx.text_edit_focused() {
            self.matcher.clear();
            return;
        }
        #[cfg(target_os = "linux")]
        if self.properties.is_some() { return; }
        let i = self.active;
        let events = ctx.input(|inp| inp.events.clone());
        let (back, fwd) = ctx.input(|inp| {
            (
                inp.pointer.button_pressed(egui::PointerButton::Extra1),
                inp.pointer.button_pressed(egui::PointerButton::Extra2),
            )
        });
        if back {
            self.acts.push(Act::Back(i));
        }
        if fwd {
            self.acts.push(Act::Forward(i));
        }
        let page = ((self.panes[i].list_h / self.row_h()).floor() as usize).max(1);
        if !self.st.vim_keys {
            self.panes[i].vim_visual = false;
        }
        // A key that did something must not also type its character.
        let mut swallow = false;
        for input in keymap::presses(&events) {
            match input {
                Input::Text(text) => {
                    if !std::mem::take(&mut swallow) {
                        self.typed(i, &text);
                    }
                }
                Input::Press { chord, repeat, text } => {
                    swallow = false;
                    if self.st.vim_keys && self.panes[i].vim_filtering {
                        if self.filter_key(i, chord) {
                            swallow = true;
                            continue;
                        }
                        if chord.is_typing() {
                            if let Some(text) = text {
                                self.typed(i, &text);
                            }
                            continue;
                        }
                    }
                    let mode = self.key_mode(i);
                    match self.matcher.feed(&self.keys, mode, chord, repeat) {
                        Outcome::Unbound => {
                            if let Some(text) = text {
                                self.typed(i, &text);
                            }
                        }
                        Outcome::Waiting | Outcome::Cancelled => swallow = true,
                        Outcome::Run { target, count, extend } => {
                            swallow = true;
                            match target {
                                Target::Command(command) => self.run_command(i, command, extend, count, repeat, page),
                                Target::Folder(path) => self.acts.push(Act::Navigate(i, path)),
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn key_mode(&self, i: usize) -> Mode {
        match (self.st.vim_keys, self.panes[i].vim_visual) {
            (false, _) => Mode::Standard,
            (true, false) => Mode::Normal,
            (true, true) => Mode::Visual,
        }
    }

    /// Characters no shortcut took go to the filter, unless Neovim mode owns the letters.
    fn typed(&mut self, i: usize, text: &str) {
        if self.st.vim_keys && !self.panes[i].vim_filtering {
            return;
        }
        self.panes[i].filter.push_str(text);
        self.panes[i].pending_select = None;
        self.panes[i].cursor = Some(0);
        self.panes[i].cursor_path = None;
    }

    /// The keys that end typing a filter in Neovim mode.
    fn filter_key(&mut self, i: usize, chord: Chord) -> bool {
        if chord.ctrl || chord.alt || chord.shift {
            return false;
        }
        let pane = &mut self.panes[i];
        match chord.key {
            Key::Enter => pane.vim_filtering = false,
            Key::Escape => {
                pane.vim_filtering = false;
                pane.filter.clear();
            }
            Key::Backspace => {
                pane.filter.pop();
                pane.vim_filtering = !pane.filter.is_empty();
            }
            _ => return false,
        }
        true
    }

    fn end_visual(&mut self, i: usize) {
        self.panes[i].vim_visual = false;
        if let Some(row) = self.panes[i].cursor {
            self.set_cursor(i, row, Modifiers::NONE);
        }
    }

    pub(crate) fn run_command(&mut self, i: usize, command: Command, extend: bool, count: Option<usize>, repeat: bool, page: usize) {
        let n = self.panes[i].view.len();
        let cur = self.panes[i].cursor;
        let visual = self.panes[i].vim_visual;
        let mv = |s: &mut Self, to: usize| {
            if n > 0 {
                let mods = Modifiers {
                    shift: extend || visual,
                    ..Default::default()
                };
                s.set_cursor(i, to.min(n - 1), mods);
            }
        };
        let by = count.unwrap_or(1);
        let half = (page / 2).max(1).saturating_mul(by);
        let row = |count: usize| count.saturating_sub(1);
        let columns = self.st.layout == Layout::Columns;
        let tree = self.st.tree_files;
        match command {
            Command::CursorDown => mv(self, cur.map_or(0, |c| c.saturating_add(by))),
            Command::CursorUp => mv(self, cur.map_or(0, |c| c.saturating_sub(by))),
            Command::PageDown => mv(self, cur.map_or(0, |c| c.saturating_add(page.saturating_mul(by)))),
            Command::PageUp => mv(self, cur.map_or(0, |c| c.saturating_sub(page.saturating_mul(by)))),
            Command::HalfPageDown => mv(self, cur.map_or(0, |c| c.saturating_add(half))),
            Command::HalfPageUp => mv(self, cur.map_or(0, |c| c.saturating_sub(half))),
            Command::First => mv(self, count.map_or(0, row)),
            Command::Last => mv(self, count.map_or(n.saturating_sub(1), row)),
            Command::Parent => self.acts.push(Act::Up(i)),
            Command::Back => self.acts.push(Act::Back(i)),
            Command::Forward => self.acts.push(Act::Forward(i)),
            Command::Right if tree => self.tree_right(i, page),
            Command::Left if tree => self.tree_left(i),
            Command::Left if columns => self.acts.push(Act::Up(i)),
            Command::Right if columns => self.enter_column(i),
            Command::Left | Command::Right => {}
            Command::FolderOut if visual => {}
            Command::FolderOut if tree => self.tree_left(i),
            Command::FolderOut => self.acts.push(Act::Up(i)),
            Command::FolderIn if visual => {}
            Command::FolderIn => {
                let Some((dir, path)) = self.cursor_entry(i).map(|e| (e.is_dir, e.path.clone())) else { return };
                if dir && tree {
                    self.tree_right(i, page);
                } else if dir && columns {
                    self.acts.push(Act::Navigate(i, path));
                } else if dir || !repeat {
                    // Holding the key walks into folders but never opens file after file.
                    self.acts.push(Act::Open(i, path));
                }
            }
            Command::GoHome => {
                if let Some(home) = dirs::home_dir() {
                    self.acts.push(Act::Navigate(i, home));
                }
            }
            Command::Properties => {
                #[cfg(target_os = "linux")]
                if let Some(e) = self.cursor_entry(i) { self.acts.push(Act::Properties(e.path.clone())); }
            }
            Command::Open => {
                if let Some(e) = self.cursor_entry(i) {
                    let p = e.path.clone();
                    self.acts.push(Act::Open(i, p));
                }
            }
            Command::OpenNewTab => {
                if let Some(e) = self.cursor_entry(i) {
                    let p = e.path.clone();
                    self.acts.push(if e.is_dir { Act::OpenNewTab(i, p, true) } else { Act::Open(i, p) });
                }
            }
            Command::ParentOrErase => {
                if self.panes[i].filter.is_empty() {
                    self.acts.push(Act::Up(i));
                } else {
                    self.panes[i].filter.pop();
                }
            }
            Command::Escape => {
                if self.preview_window {
                    self.preview_window = false;
                } else if visual {
                    self.end_visual(i);
                } else if !self.panes[i].filter.is_empty() {
                    self.panes[i].filter.clear();
                } else {
                    self.panes[i].selected.clear();
                    self.clip = None;
                }
            }
            Command::Filter => self.panes[i].vim_filtering = true,
            Command::Visual if visual => self.end_visual(i),
            Command::Visual => {
                if n > 0 {
                    self.set_cursor(i, cur.unwrap_or(0).min(n - 1), Modifiers::NONE);
                    self.panes[i].vim_visual = true;
                }
            }
            Command::VisualCopy => {
                self.panes[i].vim_visual = false;
                self.acts.push(Act::Copy(i));
            }
            Command::VisualCut => {
                self.panes[i].vim_visual = false;
                self.acts.push(Act::Cut(i));
            }
            Command::Copy => self.acts.push(Act::Copy(i)),
            Command::Cut => self.acts.push(Act::Cut(i)),
            Command::Paste => self.acts.push(Act::Paste(i, None)),
            Command::SelectAll => self.acts.push(Act::SelectAll(i)),
            Command::Delete => self.acts.push(Act::Delete(i)),
            Command::DeleteForever => self.acts.push(Act::DeleteForever(i)),
            Command::Rename => self.acts.push(Act::Rename(i)),
            Command::Refresh => self.acts.push(Act::Refresh),
            Command::Terminal => self.acts.push(Act::Terminal(self.panes[i].path().to_path_buf())),
            Command::NewFolder => self.acts.push(Act::NewFolder(i)),
            Command::NewFile => self.acts.push(Act::NewFile(i)),
            Command::Duplicate => self.acts.push(Act::Duplicate(i)),
            Command::CopyPath => self.acts.push(Act::CopyPath(i)),
            Command::ReopenTab => self.acts.push(Act::ReopenTab(i)),
            Command::NewTab => self.acts.push(Act::NewTab(i)),
            Command::CloseTab => {
                let t = self.panes[i].active;
                self.acts.push(Act::CloseTab(i, t));
            }
            Command::NextTab | Command::PrevTab => {
                let len = self.panes[i].tabs.len();
                let t = self.panes[i].active;
                let next = if command == Command::PrevTab { (t + len - 1) % len } else { (t + 1) % len };
                self.acts.push(Act::SwitchTab(i, next));
            }
            // panels, Hyprland style
            Command::NewPanel => self.acts.push(Act::OpenPanel(i, None)),
            Command::ClosePanel => self.acts.push(Act::ClosePanel(i)),
            Command::ToggleSplit => self.acts.push(Act::ToggleSplit(i)),
            Command::FocusLeft => self.focus_dir(-1.0, 0.0),
            Command::FocusRight => self.focus_dir(1.0, 0.0),
            Command::FocusUp => self.focus_dir(0.0, -1.0),
            Command::FocusDown => self.focus_dir(0.0, 1.0),
            Command::NextPanel | Command::PrevPanel => {
                let n = self.panes.len();
                if n > 1 {
                    self.set_active(if command == Command::PrevPanel { (i + n - 1) % n } else { (i + 1) % n });
                }
            }
            Command::ToggleHidden => {
                self.st.show_hidden = !self.st.show_hidden;
                let msg = if self.st.show_hidden {
                    "Showing hidden files"
                } else {
                    "Hiding hidden files"
                };
                self.toast(ToastKind::Info, msg);
                self.mark_dirty();
            }
            Command::Search => self.acts.push(Act::Search(i)),
            Command::EditPath => {
                self.panes[i].path_edit = Some(self.panes[i].path().display().to_string())
            }
            Command::EditPathSelected => {
                self.panes[i].path_edit = Some(self.panes[i].path().display().to_string());
                self.panes[i].path_select_all = true;
            }
            // ` toggles columns navigation (resource ColumnsToggle_S)
            Command::ToggleColumns => {
                self.st.show_columns = !self.st.show_columns;
                self.mark_dirty();
            }
            // Space previews (setting PreviewShortcut)
            Command::TogglePreview => {
                if self.preview_may_toggle() {
                    self.preview_window = !self.preview_window;
                }
            }
        }
    }

    fn tree_right(&mut self, i: usize, page: usize) {
        if let Some(e) = self.cursor_entry(i) {
            if e.is_dir && !self.panes[i].expanded.contains(&e.path) {
                self.toggle_tree_folder(i, e.path.clone());
            } else {
                self.run_command(i, Command::CursorDown, false, None, false, page);
            }
        }
    }

    fn tree_left(&mut self, i: usize) {
        let n = self.panes[i].view.len();
        if let Some(e) = self.cursor_entry(i) {
            let path = e.path.clone();
            if self.panes[i].expanded.contains(&path) {
                self.toggle_tree_folder(i, path);
            } else if let Some(parent) = path.parent()
                && let Some(row) = (0..n).find(|&row| self.entry_at(i, row).is_some_and(|e| e.path == parent)) {
                    self.set_cursor(i, row, Modifiers::NONE);
                }
        }
    }

    fn enter_column(&mut self, i: usize) {
        if let Some(e) = self.cursor_entry(i)
            && e.is_dir {
                self.acts.push(Act::Navigate(i, e.path.clone()));
            }
    }

    pub fn row_h(&self) -> f32 {
        (self.st.font_size + 2.0 * ITEM_PAD + 7.0).round()
    }

    // ================================================================= sidebar

    fn sidebar(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let mut full = ui.max_rect();
        let bar = take_bottom(&mut full, TOOLBAR_H);
        ui.scope_builder(
            UiBuilder::new().max_rect(bar.shrink2(vec2(0.0, 6.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon_button(ui, "⚙", "Program settings", self.settings_open, &pal).clicked()
                    {
                        self.settings_open = !self.settings_open;
                    }
                    if icon_button(ui, "◑", "Switch theme", false, &pal).clicked() {
                        let themes = ThemeName::choices();
                        let k = themes
                            .iter()
                            .position(|t| *t == self.st.theme)
                            .unwrap_or(0);
                        self.st.theme = themes[(k + 1) % themes.len()];
                        self.mark_dirty();
                    }
                    let cols = self.st.layout == Layout::Columns;
                    if icon_button(
                        ui,
                        "#layout",
                        "Switch between Standard and Columns layouts",
                        cols,
                        &pal,
                    )
                    .clicked()
                    {
                        self.st.layout = if cols {
                            Layout::Standard
                        } else {
                            Layout::Columns
                        };
                        self.mark_dirty();
                    }
                    if icon_button(ui, "#tree", "Tree files: click arrows to expand, double-click folders to open", self.st.tree_files, &pal).clicked() {
                        self.st.tree_files = !self.st.tree_files;
                        for pane in &mut self.panes { pane.view_key = None; pane.view.clear(); }
                        self.mark_dirty();
                    }
                    let tip = "Open a new panel next to the active one (Ctrl+Shift+Enter)";
                    if icon_button(ui, "#dual", tip, false, &pal).clicked() {
                        self.acts.push(Act::OpenPanel(self.active, None));
                    }
                    if icon_button(
                        ui,
                        "👁",
                        "Toggle hidden files (Ctrl+H)",
                        self.st.show_hidden,
                        &pal,
                    )
                    .clicked()
                    {
                        self.st.show_hidden = !self.st.show_hidden;
                        self.mark_dirty();
                    }
                    let tip = "Search files starting from currently opened folder (Ctrl+F)";
                    if icon_button(ui, "🔍", tip, self.search.is_some(), &pal).clicked() {
                        self.acts.push(Act::Search(self.active));
                    }
                });
            },
        );

        ui.scope_builder(UiBuilder::new().max_rect(full), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("sidebar_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    self.st.normalize_sidebar();
                    for section in self.st.sidebar_order.clone() {
                        ui.push_id(("section", format!("{section:?}")), |ui| match section {
                            SidebarSection::Drives => self.drives_card(ui),
                            SidebarSection::Cloud if self.st.show_cloud && !self.cloud.is_empty() => self.cloud_card(ui),
                            SidebarSection::Favorite(g) => self.fav_group(ui, g),
                            SidebarSection::History if self.st.show_history => self.history_card(ui),
                            _ => {}
                        });
                    }
                    ui.vertical_centered(|ui| {
                        let b =
                            egui::Button::new(RichText::new("+  New group").color(pal.text_weak))
                                .frame(false);
                        if ui
                            .add(b)
                            .on_hover_text("Creates new Favorite group and adds current folder")
                            .clicked()
                        {
                            self.dialog = Some(Dialog::NewGroup {
                                name: String::new(),
                                focus: true,
                            });
                        }
                    });
                    ui.add_space(8.0);
                });
        });
    }

    fn side_item(&mut self, ui: &mut Ui, path: &Path, label: &str) -> egui::Response {
        let resp = self.side_item_with(ui, path, label, Sense::click());
        self.side_row_drop(ui, resp.rect, path);
        resp
    }

    /// Lets a sidebar row outside the favorite groups take dragged files.
    fn side_row_drop(&mut self, ui: &Ui, row: Rect, path: &Path) {
        // A scrolled-away row must not catch a drop aimed at what covers it.
        let row = row.intersect(ui.clip_rect());
        if !accepts_files(path) { return; }
        self.drop_zones.push((row, DropTarget::Folder(path.to_path_buf())));
        let dragged = egui::DragAndDrop::payload::<DragPaths>(ui.ctx());
        if dragged.is_some_and(|d| !d.0.iter().any(|p| p == path))
            && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| row.contains(p)) {
            highlight_drop_row(ui, row, self.pal.accent);
        }
    }

    fn side_item_with(&mut self, ui: &mut Ui, path: &Path, label: &str, sense: Sense) -> egui::Response {
        let pal = self.pal;
        let h = self.row_h() + 1.0;
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), sense);
        let current = self.panes[self.active].path() == path;
        if current {
            ui.painter()
                .rect_filled(r, CornerRadius::same(1), pal.selection());
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, CornerRadius::same(1), pal.hover);
        }
        let ir = Rect::from_min_size(pos2(r.min.x + 4.0, r.center().y - 8.0), vec2(16.0, 16.0));
        let (exists, is_file) = side_metadata(path);
        if is_file {
            // The label may be a custom name; the icon follows the real file.
            let name = fsx::display_name(path);
            let ext = fsx::split_ext(&name, false);
            icons::entry(
                ui.painter(),
                ir,
                fsx::Kind::from_ext(&ext),
                &ext,
                pal.dark,
                false,
            );
        } else {
            icons::folder(
                ui.painter(),
                ir,
                if self.tag_color(path).is_some() || exists {
                    self.folder_color(path)
                } else {
                    pal.text_weak
                },
            );
        }
        let font = FontId::proportional(self.st.font_size);
        let t = fit_text(ui.painter(), label, &font, r.max.x - 4.0 - (ir.max.x + 6.0));
        let col = if exists { pal.text } else { pal.text_weak };
        ui.painter().text(
            pos2(ir.max.x + 6.0, r.center().y),
            Align2::LEFT_CENTER,
            t,
            font,
            col,
        );
        let p = path.to_path_buf();
        let i = self.active;
        if resp.clicked() {
            match (is_file, path.parent()) {
                (true, Some(par)) => {
                    self.acts
                        .push(Act::NavigateSelect(i, par.to_path_buf(), p.clone()))
                }
                _ => self.acts.push(Act::Navigate(i, p.clone())),
            }
        }
        if resp.middle_clicked() && !is_file {
            self.acts.push(Act::OpenNewTab(i, p, true));
        }
        resp.on_hover_text(path.display().to_string())
    }

    fn sidebar_heading(ui: &mut Ui, pal: &Palette, title: &str, section: SidebarSection) {
        let response = ui.add(egui::Label::new(RichText::new(title).size(13.0).strong().color(pal.text))
            .sense(Sense::drag())).on_hover_cursor(egui::CursorIcon::Grab)
            .on_hover_text("Drag this heading to reorder sidebar groups");
        response.dnd_set_drag_payload(section);
    }

    fn sidebar_group_drop(&mut self, ui: &mut Ui, response: &egui::Response, target: SidebarSection) {
        let after = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.y > response.rect.center().y);
        if response.dnd_hover_payload::<SidebarSection>().is_some_and(|s| *s != target) {
            let y = if after { response.rect.bottom() } else { response.rect.top() };
            ui.painter().hline(response.rect.x_range(), y, Stroke::new(2.0, self.pal.accent));
        }
        // Taking a payload removes it whatever its type, so leave other drags alone.
        if !egui::DragAndDrop::has_payload_of_type::<SidebarSection>(ui.ctx()) { return; }
        if let Some(source) = response.dnd_release_payload::<SidebarSection>() {
            self.st.reorder_sidebar(*source, target, after);
            self.mark_dirty();
        }
    }

    /// Shows where a dragged shortcut will land in a group, and moves it on release.
    fn fav_item_drop(&mut self, ui: &mut Ui, card: &egui::Response, g: usize, rows: &[Rect]) {
        if card.dnd_hover_payload::<FavDrag>().is_none() { return; }
        let Some(y) = ui.input(|i| i.pointer.hover_pos()).map(|p| p.y) else { return };
        let (RowDrop::At(at) | RowDrop::Into(at)) = row_drop(rows, y, |_| false);
        insertion_line(ui, card.rect, rows, at, self.pal.accent);
        if let Some(drag) = card.dnd_release_payload::<FavDrag>() {
            self.acts.push(Act::MoveFavorite { from: drag.group, index: drag.index, to: g, at });
        }
    }

    /// The folder a dragged tab or path-bar piece stands for.
    fn dragged_folder(&self, ctx: &egui::Context) -> Option<PathBuf> {
        if let Some(crumb) = egui::DragAndDrop::payload::<CrumbDrag>(ctx) {
            return Some(crumb.path.clone());
        }
        let drag = egui::DragAndDrop::payload::<TabDrag>(ctx)?;
        let path = self.panes.get(drag.pane)?.tabs.get(drag.tab?)?.path.clone();
        (!is_computer(&path)).then_some(path)
    }

    /// Adds a dragged tab or path-bar piece to a group as a shortcut, where the line shows.
    fn fav_folder_drop(&mut self, ui: &Ui, card: Rect, g: usize, rows: &[Rect]) {
        let Some(path) = self.dragged_folder(ui.ctx()) else { return };
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| card.contains(*p) && ui.clip_rect().contains(*p));
        let Some(pos) = pointer else { return };
        let (RowDrop::At(at) | RowDrop::Into(at)) = row_drop(rows, pos.y, |_| false);
        insertion_line(ui, card, rows, at, self.pal.accent);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if ui.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(ui.ctx());
            self.acts.push(Act::PlaceFavorites { group: g, at, paths: vec![path] });
        }
    }

    /// What releasing dragged files on a group does, shown on the group as it is
    /// decided: a highlighted folder, or a line between two shortcuts.
    fn fav_paths_target(&self, ui: &Ui, card: Rect, g: usize, paths: &[PathBuf], rows: &[Rect]) -> DropTarget {
        let end = DropTarget::Favorites { group: g, at: paths.len() };
        let Some(dragged) = egui::DragAndDrop::payload::<DragPaths>(ui.ctx()) else { return end };
        let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| card.contains(*p) && ui.clip_rect().contains(*p));
        let Some(pos) = pointer else { return end };
        let accent = self.pal.accent;
        match row_drop(rows, pos.y, |k| accepts_files(&paths[k]) && !dragged.0.contains(&paths[k])) {
            RowDrop::Into(k) => {
                highlight_drop_row(ui, rows[k], accent);
                DropTarget::Folder(paths[k].clone())
            }
            RowDrop::At(at) => {
                insertion_line(ui, card, rows, at, accent);
                DropTarget::Favorites { group: g, at }
            }
        }
    }

    fn fav_group(&mut self, ui: &mut Ui, g: usize) {
        let pal = self.pal;
        let name = self.st.favorites[g].name.clone();
        let group = self.st.favorites[g].clone();
        let paths = &group.paths;
        let mut rows = Vec::with_capacity(paths.len());
        let cur = self.panes[self.active].path().to_path_buf();
        let r = card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                Self::sidebar_heading(ui, &pal, &name, SidebarSection::Favorite(g));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button(RichText::new("+").color(pal.text_weak), |ui| {
                        if ui.add_enabled(!is_computer(&cur), egui::Button::new("Add current folder")).clicked() {
                            self.acts.push(Act::AddFavorite(g, cur.clone()));
                            ui.close();
                        }
                        if ui.button("Add location…").clicked() {
                            self.dialog = Some(Dialog::AddLocation { group: g, name: String::new(), focus: true });
                            ui.close();
                        }
                    }).response.on_hover_text("Add a folder or network location to this group");
                    ui.menu_button(RichText::new("…").color(pal.text_weak), |ui| {
                        if ui.button("Rename group").clicked() {
                            self.dialog = Some(Dialog::RenameGroup {
                                group: g,
                                name: name.clone(),
                                focus: true,
                            });
                            ui.close();
                        }
                        if ui.button("Move up").clicked() {
                            self.acts.push(Act::MoveGroup(g, -1));
                            ui.close();
                        }
                        if ui.button("Move down").clicked() {
                            self.acts.push(Act::MoveGroup(g, 1));
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Delete group").clicked() {
                            self.acts.push(Act::DeleteGroup(g));
                            ui.close();
                        }
                    });
                });
            });
            ui.spacing_mut().item_spacing.y = 1.0;
            for (k, p) in paths.iter().enumerate() {
                let label = group.label(p);
                let resp = self.side_item_with(ui, p, &label, Sense::click_and_drag());
                resp.dnd_set_drag_payload(FavDrag { group: g, index: k });
                rows.push(resp.rect);
                resp.context_menu(|ui| {
                    self.path_color_menu(ui, "Folder color", vec![p.clone()]);
                    if ui.button("Open in new tab").clicked() {
                        self.acts
                            .push(Act::OpenNewTab(self.active, p.clone(), true));
                        ui.close();
                    }
                    if ui.button("Rename…").clicked() {
                        self.dialog = Some(Dialog::RenameFavorite {
                            group: g,
                            path: p.clone(),
                            name: label.clone(),
                            focus: true,
                        });
                        ui.close();
                    }
                    if ui.button("Remove from group").clicked() {
                        self.acts.push(Act::RemoveFavorite(g, k));
                        ui.close();
                    }
                });
            }
            if paths.is_empty() {
                ui.label(
                    RichText::new("Drag folders here")
                        .color(pal.text_weak)
                        .size(11.0),
                );
            }
        });
        self.fav_item_drop(ui, &r.response, g, &rows);
        self.fav_folder_drop(ui, r.response.rect, g, &rows);
        self.sidebar_group_drop(ui, &r.response, SidebarSection::Favorite(g));
        let target = self.fav_paths_target(ui, r.response.rect, g, paths, &rows);
        self.drop_zones.push((r.response.rect, target));
    }

    fn drives_card(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let drives = self.drives.clone();
        let r = card(ui, &pal, |ui| {
            Self::sidebar_heading(ui, &pal, if cfg!(windows) { "Drives" } else { "Devices" }, SidebarSection::Drives);
            ui.spacing_mut().item_spacing.y = 2.0;
            for d in &drives {
                let resp = self.drive_row(ui, d, true);
                self.side_row_drop(ui, resp.rect, &d.path);
                self.eject_controls(ui, &resp, d);
            }
        });
        self.sidebar_group_drop(ui, &r.response, SidebarSection::Drives);
    }

    /// The button on a drive row, and the same choice on right click.
    fn eject_controls(&mut self, ui: &mut Ui, row: &egui::Response, d: &crate::app::DriveInfo) {
        if d.eject.is_none() { return; }
        let busy = self.ejecting.iter().any(|(mount, ..)| *mount == d.path);
        let rect = Rect::from_center_size(pos2(row.rect.max.x - 46.0, row.rect.min.y + 11.0), vec2(20.0, 20.0));
        let button = ui.interact(rect, row.id.with("eject"), if busy { Sense::hover() } else { Sense::click() });
        let pal = self.pal;
        if button.hovered() && !busy { ui.painter().rect_filled(rect, CornerRadius::same(3), pal.hover); }
        icons::eject(ui.painter(), rect.shrink(3.0), if busy { blend(pal.panel, pal.text_weak, 0.4) } else { pal.text_weak });
        let clicked = button.on_hover_text(if busy { "Ejecting…" } else { "Eject safely" }).clicked();
        let mut chosen = clicked;
        row.context_menu(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Eject safely")).clicked() {
                chosen = true;
                ui.close();
            }
        });
        if chosen { self.eject(d); }
    }

    pub fn drive_row(
        &mut self,
        ui: &mut Ui,
        d: &crate::app::DriveInfo,
        sidebar: bool,
    ) -> egui::Response {
        let pal = self.pal;
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
        let current = sidebar && self.panes[self.active].path().starts_with(&d.path);
        let bg = if current {
            blend(pal.panel, pal.accent, 0.18)
        } else if resp.hovered() {
            pal.hover
        } else {
            Color32::TRANSPARENT
        };
        let p = ui.painter();
        p.rect_filled(r, CornerRadius::same(1), bg);
        let ir = Rect::from_min_size(pos2(r.min.x + 4.0, r.min.y + 4.0), vec2(16.0, 14.0));
        if d.network {
            icons::glyph(p, ir, "network", pal.text_weak);
        } else {
            icons::drive(p, ir, pal.text_weak);
        }
        let used = d.total.saturating_sub(d.free);
        let pct = if d.total > 0 {
            used as f32 / d.total as f32
        } else {
            0.0
        };
        let font = FontId::proportional(12.5);
        let title = if d.label.is_empty() || !cfg!(windows) {
            d.name.clone()
        } else {
            format!("{}  {}", d.name, d.label)
        };
        // Room left for the eject button, which covers the end of the title line.
        let t = fit_text(p, &title, &font, r.width() - 34.0 - 40.0 - if d.eject.is_some() { 22.0 } else { 0.0 });
        p.text(
            pos2(ir.max.x + 6.0, ir.center().y),
            Align2::LEFT_CENTER,
            t,
            font,
            pal.text,
        );
        let small = FontId::proportional(11.0);
        p.text(
            pos2(r.max.x - 4.0, ir.center().y),
            Align2::RIGHT_CENTER,
            format!("{:.0}%", pct * 100.0),
            small,
            pal.text_weak,
        );
        let bar = Rect::from_min_max(
            pos2(ir.max.x + 6.0, r.max.y - 13.0),
            pos2(r.max.x - 76.0, r.max.y - 9.0),
        );
        p.rect_filled(
            bar,
            CornerRadius::same(2),
            blend(pal.panel, pal.text_weak, 0.2),
        );
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * pct, bar.height()));
        p.rect_filled(
            fill,
            CornerRadius::same(2),
            if pct > 0.9 {
                hex("#ef7063")
            } else {
                pal.accent
            },
        );
        let gb = |b: u64| b as f64 / 1e9;
        let sizes = format!("{:.0} / {:.0} GB", gb(used), gb(d.total));
        p.text(
            pos2(r.max.x - 4.0, bar.center().y),
            Align2::RIGHT_CENTER,
            sizes,
            FontId::proportional(10.5),
            pal.text_weak,
        );
        if resp.clicked() {
            self.acts.push(Act::Navigate(self.active, d.path.clone()));
        }
        if resp.middle_clicked() {
            self.acts
                .push(Act::OpenNewTab(self.active, d.path.clone(), true));
        }
        if cfg!(target_os = "linux") {
            resp.on_hover_text(format!("{}\n{}\n{}", d.name, d.path.display(), d.label))
        } else if d.network && !d.remote.is_empty() {
            resp.on_hover_text(format!("Network drive {}", d.remote))
        } else {
            resp
        }
    }

    /// OneDrive, Dropbox, Google Drive…
    fn cloud_card(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let cloud = self.cloud.clone();
        let r = card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                Self::sidebar_heading(ui, &pal, "Cloud", SidebarSection::Cloud);
                ui.label(RichText::new("i").italics().color(pal.text_weak)).on_hover_text("Automatically detected local sync folders (OneDrive, Dropbox and others). No cloud account is connected by this app. Manage visibility in Settings.");
            });
            ui.spacing_mut().item_spacing.y = 1.0;
            for p in &cloud {
                let resp = self.side_item(ui, p, &fsx::display_name(p));
                resp.context_menu(|ui| {
                    self.path_color_menu(ui, "Folder color", vec![p.clone()]);
                    if ui.button("Open in new tab").clicked() {
                        self.acts
                            .push(Act::OpenNewTab(self.active, p.clone(), true));
                        ui.close();
                    }
                    if ui.button("Open in new panel").clicked() {
                        self.acts.push(Act::OpenPanel(self.active, Some(p.clone())));
                        ui.close();
                    }
                });
            }
        });
        self.sidebar_group_drop(ui, &r.response, SidebarSection::Cloud);
    }

    fn history_card(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let hist = self.st.history.clone();
        let r = card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                Self::sidebar_heading(ui, &pal, "History", SidebarSection::History);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(
                        ui,
                        "✖",
                        "Hide History (restore in Settings → Restore groups)",
                        false,
                        &pal,
                    )
                    .clicked()
                    {
                        self.st.show_history = false;
                        self.mark_dirty();
                    }
                });
            });
            ui.spacing_mut().item_spacing.y = 1.0;
            for p in &hist {
                let resp = self.side_item(ui, p, &fsx::display_name(p));
                resp.context_menu(|ui| {
                    self.path_color_menu(ui, "Folder color", vec![p.clone()]);
                });
            }
        });
        self.sidebar_group_drop(ui, &r.response, SidebarSection::History);
    }

    // ================================================================= browsers

    /// Panels tile the area like Hyprland's dwindle layout; dividers drag to resize,
    /// a double-click on a divider makes both sides equal.
    fn browsers(&mut self, ui: &mut Ui) {
        let full = ui.max_rect();
        ui.painter()
            .rect_filled(full, CornerRadius::ZERO, self.pal.border);
        let mut rects = Vec::new();
        let mut divs = Vec::new();
        if self.panes.len() > 1 && full.width() < 2.0 * COLLAPSE_W {
            // too narrow for two browsers, so only the
            // active panel is shown (resource MSG_I_CannotFitBothBrowsers)
            rects.push((self.active, full));
        } else {
            self.tiles
                .layout(full, &mut rects, &mut divs, &mut Vec::new());
        }
        for d in &divs {
            let id = ui.scope_id().with(("divider", &d.path));
            let resp = ui.interact(d.rect.expand(2.0), id, Sense::click_and_drag());
            let icon = if d.vertical {
                egui::CursorIcon::ResizeVertical
            } else {
                egui::CursorIcon::ResizeHorizontal
            };
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(icon);
                ui.painter().rect_filled(
                    d.rect,
                    CornerRadius::ZERO,
                    blend(self.pal.border, self.pal.accent, 0.6),
                );
            }
            if resp.dragged()
                && let Some(pos) = resp.interact_pointer_pos() {
                    let ratio = if d.vertical {
                        (pos.y - d.area.min.y) / d.area.height().max(1.0)
                    } else {
                        (pos.x - d.area.min.x) / d.area.width().max(1.0)
                    };
                    self.tiles.set_ratio(&d.path, ratio);
                }
            if resp.drag_stopped() {
                self.mark_dirty();
            }
            if resp.double_clicked() {
                self.tiles.set_ratio(&d.path, 0.5);
                self.mark_dirty();
            }
        }
        for (i, r) in rects.clone() {
            if i < self.panes.len() {
                self.pane(ui, i, r);
            }
        }
        if let Some(payload) = egui::DragAndDrop::payload::<TabDrag>(ui.ctx())
            && let Some(pos) = ui.input(|inp| inp.pointer.interact_pos())
                && let Some((target, r)) = rects.iter().find(|(_, r)| r.contains(pos)) {
                    let in_tabs = pos.y < r.min.y + 1.0 + TAB_H;
                    let local = (pos - r.min) / r.size();
                    let distances = [local.x, 1.0 - local.x, local.y, 1.0 - local.y];
                    let edge = (0..4).min_by(|a, b| distances[*a].total_cmp(&distances[*b])).unwrap();
                    let split = (!in_tabs && distances[edge] < 0.25).then_some((edge >= 2, edge == 0 || edge == 2));
                    let n = self.panes[*target].tabs.len();
                    let w = ((r.width() - 2.0 - 60.0 - 80.0) / n as f32).clamp(12.0, 190.0);
                    let at = if in_tabs {
                        ((pos.x - r.min.x - 5.0 + w / 2.0) / w).floor().max(0.0) as usize
                    } else { n }.min(n);
                    let valid = *target != payload.pane
                        || (payload.tab.is_some() && (split.is_none() || self.panes[payload.pane].tabs.len() > 1));
                    if valid {
                        let mut preview = *r;
                        if split.is_some() {
                            match edge {
                                0 => preview.max.x = r.center().x,
                                1 => preview.min.x = r.center().x,
                                2 => preview.max.y = r.center().y,
                                _ => preview.min.y = r.center().y,
                            }
                        } else if in_tabs {
                            let x = (r.min.x + 5.0 + at as f32 * w).min(r.max.x - 2.0);
                            preview = Rect::from_min_max(pos2(x, r.min.y + 4.0), pos2(x + 2.0, r.min.y + TAB_H));
                        }
                        ui.painter().rect_filled(preview, CornerRadius::same(1), self.pal.accent.gamma_multiply(if in_tabs { 0.9 } else { 0.25 }));
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        if ui.input(|inp| inp.pointer.any_released()) {
                            self.acts.push(match payload.tab {
                                Some(tab) => Act::DockTab { source: payload.pane, tab, target: *target, at, split },
                                None => Act::DockPanel { source: payload.pane, target: *target, at, split },
                            });
                            egui::DragAndDrop::clear_payload(ui.ctx());
                        }
                    }
                }
        if let Some(payload) = egui::DragAndDrop::payload::<CrumbDrag>(ui.ctx())
            && let Some(pos) = ui.input(|inp| inp.pointer.interact_pos())
            && let Some((target, r)) = rects.iter().find(|(_, r)| r.contains(pos))
            // Over its own panel only the tab strip counts, so a shaky click opens nothing.
            && (*target != payload.pane || pos.y < r.min.y + 1.0 + TAB_H)
        {
            let mut preview = *r;
            if *target == payload.pane {
                preview.max.y = r.min.y + 1.0 + TAB_H;
            }
            ui.painter().rect_filled(preview, CornerRadius::same(1), self.pal.accent.gamma_multiply(0.25));
            if ui.input(|inp| inp.pointer.any_released()) {
                self.acts.push(Act::OpenNewTab(*target, payload.path.clone(), true));
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
        }
        self.pane_rects = rects;
    }

    /// Move focus to the nearest panel in a direction (Ctrl+Alt+arrows).
    fn focus_dir(&mut self, dx: f32, dy: f32) {
        let Some((_, cur)) = self
            .pane_rects
            .iter()
            .find(|(k, _)| *k == self.active)
            .copied()
        else {
            return;
        };
        let c = cur.center();
        let best = self
            .pane_rects
            .iter()
            .filter(|(k, r)| {
                *k != self.active && {
                    let v = r.center() - c;
                    v.x * dx + v.y * dy > 1.0
                }
            })
            .min_by(|(_, a), (_, b)| {
                (a.center() - c)
                    .length()
                    .total_cmp(&(b.center() - c).length())
            })
            .map(|(k, _)| *k);
        if let Some(k) = best {
            self.set_active(k);
        }
    }

    fn pane(&mut self, ui: &mut Ui, i: usize, rect: Rect) {
        let pal = self.pal;
        let (pressed, pos) =
            ui.input(|inp| (inp.pointer.any_pressed(), inp.pointer.interact_pos()));
        if pressed && pos.is_some_and(|p| rect.contains(p)) && self.active != i {
            self.set_active(i);
        }
        let painter = ui.painter_at(rect);
        // the other browser is dimmed
        let fill = if self.active != i && self.panes.len() > 1 {
            pal.inactive
        } else {
            pal.panel
        };
        painter.rect_filled(rect, CornerRadius::same(2), fill);
        let mut r = rect.shrink(1.0);
        // Reserve the summary before any controls, even in a very short tile.
        let status = take_bottom(&mut r, STATUS_H);
        let tabs = take_top(&mut r, TAB_H);
        let outer_clip = ui.clip_rect();
        ui.set_clip_rect(outer_clip.intersect(tabs));
        self.tab_bar(ui, i, tabs);
        // Header widgets and the list may have minimum heights of their own.
        // Keep all their painting and hit targets inside the content area.
        ui.set_clip_rect(outer_clip.intersect(r));
        let path = self.panes[i].path().to_path_buf();
        self.drop_zones.push((r, DropTarget::Folder(path)));

        match self.st.layout {
            Layout::Standard => {
                if self.st.show_columns && r.height() > 260.0 {
                    let h = COLS_H.min(r.height() * 0.38);
                    let strip = take_top(&mut r, h);
                    self.columns_strip(ui, i, strip, false);
                }
                let header = take_top(&mut r, HEADER_H);
                self.header(ui, i, header);
                if self.st.show_preview && r.height() > DETAILS_H + 120.0 {
                    let det = take_bottom(&mut r, DETAILS_H);
                    self.details_panel(ui, i, det);
                }
                if r.height() >= 24.0 {
                    self.file_list(ui, i, r.shrink2(vec2(4.0, 0.0)));
                }
            }
            Layout::Columns => {
                let header = take_top(&mut r, HEADER_H);
                self.header(ui, i, header);
                if self.st.show_preview && r.width() > PREVIEW_W + 360.0 {
                    let pv = take_right(&mut r, PREVIEW_W);
                    self.preview_pane(ui, i, pv);
                }
                let main_w = CURRENT_COL_W.min(r.width());
                let cols_w = r.width() - main_w;
                if self.st.show_columns && cols_w > COL_SMALL_W {
                    let strip = take_left(&mut r, cols_w);
                    self.columns_strip(ui, i, strip, true);
                }
                if r.height() >= 24.0 {
                    self.file_list(ui, i, r.shrink2(vec2(4.0, 0.0)));
                }
            }
        }
        ui.set_clip_rect(outer_clip);
        self.status_bar(ui, i, status);
    }

    fn tab_bar(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        let p = ui.painter_at(r);
        let n = self.panes[i].tabs.len();
        let w = ((r.width() - 60.0 - 80.0) / n as f32).clamp(12.0, 190.0);
        let mut x = r.min.x + 4.0;
        let font = FontId::proportional(self.st.font_size);
        for t in 0..n {
            let tr = Rect::from_min_size(pos2(x, r.min.y + 4.0), vec2(w - 2.0, r.height() - 4.0));
            x += w;
            let id = ui.scope_id().with(("tab", i, t));
            let resp = ui.interact(tr, id, Sense::click_and_drag());
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), TabDrag { pane: i, tab: Some(t) });
            }
            if resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            let selected = self.panes[i].active == t;
            let look = crate::theme::tab_look(&pal, selected, self.active == i, resp.hovered());
            let bg = look.fill;
            p.rect_filled(tr, CornerRadius::same(1), bg);
            let path = self.panes[i].tabs[t].path.clone();
            let ir =
                Rect::from_min_size(pos2(tr.min.x + 8.0, tr.center().y - 7.0), vec2(14.0, 14.0));
            if is_computer(&path) {
                icons::drive(&p, ir, pal.text_weak);
            } else {
                icons::folder(&p, ir, self.folder_color(&path));
            }
            let name = if is_computer(&path) {
                "This PC".to_string()
            } else {
                fsx::display_name(&path)
            };
            let show_x = true;
            let tw = tr.width() - 32.0 - if show_x { 18.0 } else { 0.0 };
            p.text(
                pos2(ir.max.x + 6.0, tr.center().y),
                Align2::LEFT_CENTER,
                fit_text(&p, &name, &font, tw),
                font.clone(),
                look.text,
            );
            if show_x {
                let xr =
                    Rect::from_center_size(pos2(tr.max.x - 12.0, tr.center().y), vec2(16.0, 16.0));
                let xresp = ui.interact(xr, id.with("x"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if xresp.hovered() {
                    p.rect_filled(xr, CornerRadius::same(1), blend(bg, pal.text, 0.15));
                }
                p.text(
                    xr.center(),
                    Align2::CENTER_CENTER,
                    "×",
                    FontId::proportional(14.0),
                    pal.text_weak,
                );
                if xresp.on_hover_text("Close tab (Ctrl+W)").clicked() {
                    self.acts.push(Act::CloseTab(i, t));
                    continue;
                }
            }
            if resp.clicked() {
                self.acts.push(Act::SwitchTab(i, t));
            }
            if resp.middle_clicked() {
                self.acts.push(Act::CloseTab(i, t));
            }
            let tip = if is_computer(&path) {
                "This PC".into()
            } else {
                path.display().to_string()
            };
            resp.on_hover_text(format!("{tip}\nDrag to reorder, join another panel, split at an edge, or add to a favorites group")).context_menu(|ui| {
                if ui.button("Close tab").clicked() {
                    self.acts.push(Act::CloseTab(i, t));
                    ui.close();
                }
                if ui.button("Close other tabs").clicked() {
                    for k in (0..n).rev().filter(|k| *k != t) {
                        self.acts.push(Act::CloseTab(i, k));
                    }
                    ui.close();
                }
                if ui.button("Reopen closed tab").clicked() {
                    self.acts.push(Act::ReopenTab(i));
                    ui.close();
                }
            });
            if !is_computer(&path) {
                self.drop_zones.push((tr, DropTarget::Folder(path)));
            }
        }
        // Panel tools; closing belongs to each tab.
        let mut bx = r.max.x - 4.0;
        let n_panels = self.panes.len();
        // Carries the panel with all its tabs; it drops exactly where a tab would.
        if n_panels > 1 {
            let br = Rect::from_min_max(pos2(bx - 24.0, r.min.y + 5.0), pos2(bx, r.max.y - 3.0));
            bx -= 26.0;
            let resp = ui.interact(br, ui.scope_id().with(("panelbtn", i, "move")), Sense::click_and_drag());
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), TabDrag { pane: i, tab: None });
            }
            if resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if resp.hovered() {
                p.rect_filled(br, CornerRadius::same(1), pal.hover);
            }
            icons::glyph(&p, br, "grip", if resp.hovered() { pal.text } else { pal.text_weak });
            resp.on_hover_text("Drag to move this panel with all its tabs: join another panel, or split at an edge");
        }
        let mut panel_btn = |ui: &mut Ui, glyph: &str, tip: &str, key: &str| -> bool {
            let br = Rect::from_min_max(pos2(bx - 24.0, r.min.y + 5.0), pos2(bx, r.max.y - 3.0));
            bx -= 26.0;
            let resp = ui.interact(br, ui.scope_id().with(("panelbtn", i, key)), Sense::click());
            if resp.hovered() {
                p.rect_filled(br, CornerRadius::same(1), pal.hover);
            }
            let col = if resp.hovered() {
                pal.text
            } else {
                pal.text_weak
            };
            match glyph.strip_prefix('#') {
                Some(g) => icons::glyph(&p, br, g, col),
                None => {
                    p.text(
                        br.center(),
                        Align2::CENTER_CENTER,
                        glyph,
                        FontId::proportional(13.0),
                        col,
                    );
                }
            }
            resp.on_hover_text(tip).clicked()
        };
        if panel_btn(
            ui,
            "#dual",
            "Open a new panel next to this one (Ctrl+Shift+Enter)",
            "split",
        ) {
            self.acts.push(Act::OpenPanel(i, None));
        }
        if n_panels > 1
            && panel_btn(
                ui,
                "#flip",
                "Flip this panel's split direction (Ctrl+Shift+J)",
                "flip",
            )
        {
            self.acts.push(Act::ToggleSplit(i));
        }
        let pr = Rect::from_min_size(pos2(x + 2.0, r.min.y + 6.0), vec2(26.0, r.height() - 8.0));
        let presp = ui.interact(pr, ui.scope_id().with(("newtab", i)), Sense::click());
        if presp.hovered() {
            p.rect_filled(pr, CornerRadius::same(1), pal.hover);
        }
        p.text(
            pr.center(),
            Align2::CENTER_CENTER,
            "+",
            FontId::proportional(17.0),
            pal.text_weak,
        );
        if presp.on_hover_text("Create new tab (Ctrl+T)").clicked() {
            self.acts.push(Act::NewTab(i));
        }
    }

    /// Miller columns: one column per ancestor folder listing its sub-folders.
    fn columns_strip(&mut self, ui: &mut Ui, i: usize, r: Rect, with_files: bool) {
        let pal = self.pal;
        let path = self.panes[i].path().to_path_buf();
        let mut chain = self.column_chain(&path);
        if with_files {
            // in Columns layout the current folder is the main list, not a column
            chain.pop();
        }
        if chain.is_empty() {
            return;
        }
        // 200 each; shrink the left-most to 128; drop the left-most when still too wide
        let n = chain.len();
        let mut widths = vec![COL_W; n];
        let mut k = 0;
        while widths.iter().sum::<f32>() > r.width() && k < n.saturating_sub(1) {
            widths[k] = COL_SMALL_W;
            k += 1;
        }
        let mut first = 0;
        while widths[first..].iter().sum::<f32>() > r.width() && first < n - 1 {
            first += 1;
        }
        let mut x = r.min.x;
        for c in first..n {
            let w = widths[c].min(r.max.x - x);
            if w < 40.0 {
                break;
            }
            let cr = Rect::from_min_size(pos2(x, r.min.y), vec2(w, r.height()));
            x += w;
            let next = chain
                .get(c + 1)
                .cloned()
                .or_else(|| if with_files { Some(path.clone()) } else { None });
            let col = chain[c].clone();
            self.column(ui, i, cr, &col, next.as_deref(), with_files);
        }
        let s = Stroke::new(1.0, pal.raised);
        ui.painter().line_segment(
            [pos2(r.min.x, r.max.y - 0.5), pos2(r.max.x, r.max.y - 0.5)],
            s,
        );
    }

    fn column(
        &mut self,
        ui: &mut Ui,
        i: usize,
        r: Rect,
        dir: &Path,
        highlight: Option<&Path>,
        with_files: bool,
    ) {
        let pal = self.pal;
        let p = ui.painter_at(r);
        let inner = r.shrink2(vec2(3.0, 0.0));
        p.line_segment(
            [
                pos2(r.max.x - 0.5, r.min.y + 6.0),
                pos2(r.max.x - 0.5, r.max.y - 6.0),
            ],
            Stroke::new(1.0, pal.raised),
        );
        let title = if is_computer(dir) {
            "This PC".to_string()
        } else {
            fsx::display_name(dir)
        };
        let tr = Rect::from_min_size(inner.min + vec2(4.0, 4.0), vec2(inner.width() - 8.0, 22.0));
        // Hash the literal path: Path's component hash can collide for the
        // empty "This PC" sentinel and a filesystem root such as `/`.
        let tresp = ui.interact(
            tr,
            ui.scope_id().with(("coltitle", i, dir.as_os_str())),
            Sense::click(),
        );
        let tf = FontId::proportional(14.0);
        let tc = if tresp.hovered() {
            pal.text_important
        } else {
            pal.text
        };
        p.text(
            tr.left_center(),
            Align2::LEFT_CENTER,
            fit_text(&p, &title, &tf, tr.width()),
            tf,
            tc,
        );
        if tresp.clicked() {
            self.acts.push(Act::Navigate(i, dir.to_path_buf()));
        }
        let list_r = Rect::from_min_max(pos2(inner.min.x, tr.max.y + 4.0), inner.max);
        if is_computer(dir) {
            let drives = self.drives.clone();
            ui.scope_builder(UiBuilder::new().max_rect(list_r), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt(("colpc", i))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for d in &drives {
                            let on = highlight.is_some_and(|h| h == d.path);
                            let resp = self.drive_row(ui, d, false);
                            self.eject_controls(ui, &resp, d);
                            if on {
                                ui.painter().rect_filled(
                                    resp.rect,
                                    CornerRadius::same(1),
                                    pal.accent.gamma_multiply(0.2),
                                );
                            }
                        }
                    });
            });
            return;
        }
        let Some(l) = self.cache.get(dir) else { return };
        let show_hidden = self.st.show_hidden;
        let items: Vec<(PathBuf, String, bool, fsx::Kind, String)> = l
            .entries
            .iter()
            .filter(|e| (e.is_dir || with_files) && (show_hidden || !e.hidden))
            .map(|e| {
                (
                    e.path.clone(),
                    e.name().to_string(),
                    e.is_dir,
                    e.kind,
                    e.ext().to_string(),
                )
            })
            .collect();
        let err = l.error.clone();
        let rh = self.row_h();
        let hl_idx = highlight.and_then(|h| items.iter().position(|it| it.0 == h));
        let key = (i, dir.to_path_buf());
        let need_scroll =
            highlight.is_some_and(|h| self.col_hl.get(&key).map(|p| p.as_path()) != Some(h));
        if let Some(h) = highlight {
            self.col_hl.insert(key, h.to_path_buf());
        }
        let font = FontId::proportional(self.st.font_size);
        ui.scope_builder(UiBuilder::new().max_rect(list_r), |ui| {
            let mut sa = egui::ScrollArea::vertical()
                .id_salt(("col", i, dir.as_os_str()))
                .auto_shrink([false, false]);
            if let (true, Some(k)) = (need_scroll, hl_idx) {
                sa = sa
                    .vertical_scroll_offset((k as f32 * rh - list_r.height() / 2.0 + rh).max(0.0));
            }
            sa.show_rows(ui, rh, items.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for k in range {
                    let (path, name, is_dir, kind, ext) = &items[k];
                    let (rr, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), rh),
                        Sense::click_and_drag(),
                    );
                    let p = ui.painter();
                    if hl_idx == Some(k) {
                        p.rect_filled(rr, CornerRadius::same(1), pal.cursor(false));
                    } else if resp.hovered() {
                        p.rect_filled(rr, CornerRadius::same(1), pal.hover);
                    }
                    let ir = Rect::from_min_size(
                        pos2(rr.min.x + 4.0, rr.center().y - 7.0),
                        vec2(14.0, 14.0),
                    );
                    if *is_dir {
                        icons::folder(p, ir, self.folder_color(path));
                    } else {
                        icons::entry(p, ir, *kind, ext, pal.dark, false);
                    }
                    let col = if *is_dir { pal.text } else { pal.text_weak };
                    let t = fit_text(p, name, &font, rr.max.x - ir.max.x - 10.0);
                    p.text(
                        pos2(ir.max.x + 6.0, rr.center().y),
                        Align2::LEFT_CENTER,
                        t,
                        font.clone(),
                        col,
                    );
                    if resp.clicked() {
                        if *is_dir {
                            self.acts.push(Act::Navigate(i, path.clone()));
                        } else {
                            self.acts
                                .push(Act::NavigateSelect(i, dir.to_path_buf(), path.clone()));
                        }
                    }
                    if resp.double_clicked() && !*is_dir {
                        self.acts.push(Act::Open(i, path.clone()));
                    }
                    if resp.middle_clicked() && *is_dir {
                        self.acts.push(Act::OpenNewTab(i, path.clone(), false));
                    }
                    if resp.drag_started() {
                        egui::DragAndDrop::set_payload(ui.ctx(), DragPaths(vec![path.clone()]));
                    }
                    if *is_dir {
                        self.drop_zones.push((rr, DropTarget::Folder(path.clone())));
                    }
                    resp.context_menu(|ui| {
                        if *is_dir && ui.button("Open in new tab").clicked() {
                            self.acts.push(Act::OpenNewTab(i, path.clone(), true));
                            ui.close();
                        }
                        if ui.button("Copy as path").clicked() {
                            ui.ctx().copy_text(path.display().to_string());
                            ui.close();
                        }
                    });
                }
            });
            if items.is_empty() {
                let msg = err.unwrap_or_else(|| "Empty".into());
                let pos = list_r.center_top() + vec2(0.0, 20.0);
                ui.painter().text(
                    pos,
                    Align2::CENTER_CENTER,
                    msg,
                    FontId::proportional(12.0),
                    pal.text_weak,
                );
            }
        });
    }
}
