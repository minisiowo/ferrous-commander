//! Window frame: keyboard, sidebar, browser panes and tabs.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, FontId, Frame, Key, Margin, Modifiers, Rect, RichText, Sense,
    Stroke, Ui, UiBuilder, pos2, vec2,
};

use crate::keymap::{self, Chord, Command, Input, Mode, Outcome, Target};
use crate::app::{Act, App, CrumbDrag, Dialog, DragPaths, DropTarget, FavDrag, ToastKind, is_computer};
use crate::fsx;
use crate::icons;
use crate::settings::SidebarSection;
use crate::theme::{Palette, blend, hex};

// ---- sizes ----
pub const SIDEBAR_W: f32 = crate::defaults::SIDEBAR_WIDTH;
/// The space around every block, square or round: the sidebar's cards and the file panels,
/// between one another and from the window's edges.
const SIDEBAR_GAP: i8 = 8;
/// The part of that space beside the file panels which belongs to them.
const PANELS_LEFT: i8 = 2;
/// The row of tools at the sidebar's foot: seven buttons 26 wide, 2 apart. It cannot narrow.
const SIDEBAR_TOOLS_W: f32 = 7.0 * 26.0 + 6.0 * 2.0;

/// The narrowest the sidebar goes: its tools and the room around them. Asking egui for
/// less would let the edge be dragged further than the sidebar can follow.
pub(crate) fn sidebar_min() -> f32 {
    SIDEBAR_TOOLS_W + f32::from(SIDEBAR_GAP + SIDEBAR_GAP - PANELS_LEFT)
}
pub const TOOLBAR_H: f32 = 36.0;
pub const DETAILS_H: f32 = crate::defaults::DETAILS_HEIGHT;
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

/// A menu entry with its shortcut at the right edge.
pub fn menu_entry(ui: &mut Ui, text: impl Into<egui::WidgetText>, keys: Option<String>) -> egui::Response {
    let button = egui::Button::new(text.into());
    ui.add(match keys {
        Some(keys) => button.shortcut_text(keys),
        None => button,
    })
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
    ui.painter().rect_filled(r, pal.corner(), bg);
    let col = if resp.hovered() || on {
        pal.text
    } else {
        pal.text_weak
    };
    icons::face(ui.painter(), r, text, 14.0, col);
    resp.on_hover_text(tip)
}

/// An egui button's room and frame around one symbol. See `icons::face`.
fn captionless(framed: bool) -> egui::Button<'static> {
    egui::Button::new("").frame(framed).min_size(vec2(22.0, 20.0))
}

fn paint_button_face(ui: &Ui, resp: &egui::Response, face: &str, pal: &Palette) {
    icons::face(ui.painter(), resp.rect, face, 14.0, if resp.hovered() { pal.text } else { pal.text_weak });
}

pub fn symbol_button(ui: &mut Ui, face: &str, framed: bool, pal: &Palette) -> egui::Response {
    let resp = ui.add(captionless(framed));
    paint_button_face(ui, &resp, face, pal);
    resp
}

fn symbol_menu(ui: &mut Ui, face: &str, pal: &Palette, add: impl FnOnce(&mut Ui)) -> egui::Response {
    let (resp, _) = egui::containers::menu::MenuButton::from_button(captionless(true)).ui(ui, add);
    paint_button_face(ui, &resp, face, pal);
    resp
}

pub fn chip(ui: &mut Ui, text: &str, on: bool, pal: &Palette) -> egui::Response {
    chip_with_room(ui, text, on, 0.0, pal)
}

pub fn sort_chip(ui: &mut Ui, text: &str, on: bool, ascending: Option<bool>, pal: &Palette) -> egui::Response {
    let resp = chip_with_room(ui, text, on, if ascending.is_some() { 12.0 } else { 0.0 }, pal);
    if let Some(ascending) = ascending {
        icons::sort_arrow(ui.painter(), resp.rect.right_center() - vec2(9.0, 0.0), ascending, pal.text);
    }
    resp
}

/// A lit chip that goes away when clicked, marked with a cross.
pub fn dismiss_chip(ui: &mut Ui, text: &str, pal: &Palette) -> egui::Response {
    let resp = chip_with_room(ui, text, true, 12.0, pal);
    icons::cross(ui.painter(), resp.rect.right_center() - vec2(10.0, 0.0), 3.0, Stroke::new(1.2, pal.text));
    resp
}

/// A chip whose text leaves `arrow_w` free at its right end for the caller to paint in.
fn chip_with_room(ui: &mut Ui, text: &str, on: bool, arrow_w: f32, pal: &Palette) -> egui::Response {
    let font = FontId::proportional(11.0);
    let gw = ui
        .painter()
        .layout_no_wrap(text.to_string(), font.clone(), pal.text)
        .size()
        .x;
    let (r, resp) = ui.allocate_exact_size(vec2(gw + 14.0 + arrow_w, 22.0), Sense::click());
    let bg = if on {
        blend(pal.panel, pal.accent, 0.35)
    } else if resp.hovered() {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, pal.corner(), bg);
    ui.painter().text(
        r.center() - vec2(arrow_w / 2.0, 0.0),
        Align2::CENTER_CENTER,
        text,
        font,
        if on { pal.text } else { pal.text_weak },
    );
    resp
}

pub fn card<R>(
    ui: &mut Ui,
    pal: &Palette,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    Frame::NONE
        .fill(pal.surface(pal.card))
        .corner_radius(pal.corner_large())
        .inner_margin(Margin::symmetric(8, 6))
        // Without outlines a card the colour of the window would still be lost in it.
        .stroke(if !pal.outlined && pal.card == pal.window { Stroke::new(1.0, pal.border) } else { pal.outline() })
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
    let corner = ui.visuals().widgets.inactive.corner_radius;
    painter.rect_filled(row, corner, color.gamma_multiply(0.25));
    painter.rect_stroke(row, corner, Stroke::new(1.5, color), egui::StrokeKind::Inside);
}

const NO_ROOM_TO_SPLIT: &str = "Not enough room to split";

/// A tab is no wider than this, however long its name.
const TAB_MAX: f32 = 190.0;
/// A short name still leaves a tab wide enough to hit and to tell from its close button.
const TAB_MIN: f32 = 84.0;
/// What a tab holds beside its name: the folder, the close button, the room around them
/// and the gap to the next tab.
const TAB_AROUND: f32 = 54.0;

/// How wide each tab is in a strip of this width: as wide as its name `wanted`, within
/// limits. In a strip too short for them all, the widest give way first.
pub(crate) fn tab_widths(strip: f32, wanted: &[f32]) -> Vec<f32> {
    let room = (strip - 60.0 - 80.0).max(0.0);
    let natural: Vec<f32> = wanted.iter().map(|w| (w + TAB_AROUND).ceil().clamp(TAB_MIN, TAB_MAX)).collect();
    let fits = |cap: f32| natural.iter().map(|w| w.min(cap)).sum::<f32>() <= room;
    if fits(TAB_MAX) { return natural; }
    // The widest cap under which they fit, but never so narrow that a tab is lost.
    let (mut low, mut high) = (12.0, TAB_MAX);
    for _ in 0..16 {
        let middle = (low + high) / 2.0;
        if fits(middle) { low = middle; } else { high = middle; }
    }
    natural.iter().map(|w| w.min(low.floor())).collect()
}

/// What a profile opens, one panel to a line: its folders, the active one first.
pub(crate) fn profile_summary(profile: &crate::settings::Profile) -> String {
    let panels = profile.panes.len().max(1);
    let mut lines = vec![if panels == 1 { "1 panel".to_string() } else { format!("{panels} panels") }];
    for pane in &profile.panes {
        let mut tabs: Vec<String> = pane.tabs.iter().map(|t| t.display().to_string()).collect();
        if pane.active < tabs.len() { let active = tabs.remove(pane.active); tabs.insert(0, active); }
        lines.push(tabs.join(", "));
    }
    lines.join("\n")
}

fn tab_name(path: &Path) -> String {
    if is_computer(path) { "This PC".to_string() } else { fsx::display_name(path) }
}

impl App {
    /// Every tab the strip of panel `i` lists, as panel and tab. With the panels stacked
    /// that is all of them, in the order of the layout.
    /// How wide the tabs in `slots` are in a strip of this width. See `tab_widths`.
    fn tab_widths(&self, ui: &Ui, slots: &[(usize, usize)], strip: f32) -> Vec<f32> {
        let font = FontId::proportional(self.st.font_size);
        let wanted: Vec<f32> = slots.iter().map(|&(pane, tab)| {
            let name = tab_name(&self.panes[pane].tabs[tab].path);
            ui.painter().layout_no_wrap(name, font.clone(), Color32::WHITE).size().x
        }).collect();
        tab_widths(strip, &wanted)
    }

    pub fn tab_slots(&self, i: usize) -> Vec<(usize, usize)> {
        let mut panels = Vec::new();
        if self.panels_stacked { self.tiles.leaves(&mut panels); }
        if !panels.contains(&i) { panels = vec![i]; }
        panels.into_iter().filter(|p| *p < self.panes.len())
            .flat_map(|p| (0..self.panes[p].tabs.len()).map(move |t| (p, t)))
            .collect()
    }

    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.begin_frame(&ctx);
        self.keyboard(&ctx);
        let pal = self.pal;

        // The same gap lies around the blocks as between the sidebar's cards.
        let around = SIDEBAR_GAP;
        let panels_left = if self.st.show_sidebar { PANELS_LEFT } else { SIDEBAR_GAP };
        if self.st.show_sidebar {
            egui::Panel::left("sidebar")
                .resizable(true)
                .default_size(SIDEBAR_W)
                .size_range(sidebar_min()..=420.0)
                .show_separator_line(false)
                .frame(Frame::NONE.fill(pal.backdrop()).inner_margin(Margin {
                    left: SIDEBAR_GAP,
                    right: SIDEBAR_GAP - panels_left,
                    // Level with the file panels.
                    top: around,
                    bottom: 0,
                }))
                .show(ui, |ui| self.sidebar(ui));
        }
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(pal.backdrop()).inner_margin(Margin {
                left: panels_left,
                right: around,
                top: around,
                bottom: around,
            }))
            .show(ui, |ui| self.browsers(ui));

        self.dialogs(&ctx);
        self.notices_ui(&ctx);
        self.guard_close(&ctx);
        self.preview_window_ui(&ctx);
        self.sound_follows();
        self.search_window(&ctx);
        self.settings_window(&ctx);
        self.shortcuts_window(&ctx);
        self.theme_editor_window(&ctx);
        self.palette_ui(&ctx);
        self.overlays(&ctx);
        self.which_key(&ctx);
        self.handle_drop(&ctx);
        self.end_frame(&ctx);
        self.screenshot(&ctx);
    }

    // ================================================================= keyboard

    fn keyboard(&mut self, ctx: &egui::Context) {
        let mut events = ctx.input(|inp| inp.events.clone());
        // Before anything returns, so that a release is never missed.
        keymap::drop_held_pastes(&mut events, &mut self.paste_held);
        let away = events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)));
        let busy = ctx.any_popup_open() || self.search.as_ref().is_some_and(|s| s.owns_keyboard(ctx)) || self.dialog.is_some() || !self.notices.is_empty() || ctx.text_edit_focused() || self.recording_shortcut() || self.palette.is_some();
        if away || busy {
            self.matcher.clear();
        }
        if busy {
            return;
        }
        #[cfg(target_os = "linux")]
        if self.properties.is_some() {
            self.matcher.clear();
            return;
        }
        let i = self.active;
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
            self.panes[i].vim_filtering = false;
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
                            self.matcher.clear();
                            swallow = true;
                            continue;
                        }
                        if chord.is_typing() {
                            // A shortcut begun before typing must not finish after it.
                            self.matcher.clear();
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
                                Target::Folder { path, new_tab: false } => self.acts.push(Act::Navigate(i, path)),
                                Target::Folder { path, new_tab: true } => self.acts.push(Act::OpenNewTab(i, path, true)),
                            }
                        }
                    }
                }
            }
        }
    }

    /// The shortcut to show for a command, if it has one that works right now.
    pub(crate) fn key_hint(&self, command: Command) -> Option<String> {
        self.keys.label(self.key_mode(self.active), command)
    }

    /// A tooltip naming the command's shortcut.
    pub(crate) fn tip(&self, text: &str, command: Command) -> String {
        match self.key_hint(command) {
            Some(keys) => format!("{text} ({keys})"),
            None => text.to_string(),
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
            Command::Left | Command::Right => {}
            Command::FolderOut if visual => {}
            Command::FolderOut if tree => self.tree_left(i),
            Command::FolderOut => self.acts.push(Act::Up(i)),
            Command::FolderIn if visual => {}
            Command::FolderIn => {
                let Some((dir, path)) = self.cursor_entry(i).map(|e| (e.is_dir, e.path.clone())) else { return };
                if dir && tree {
                    self.tree_right(i, page);
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
                    // A held key that has just emptied the filter must not go on up the folders.
                    if !repeat {
                        self.acts.push(Act::Up(i));
                    }
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
            Command::Filter => {
                // The selection's anchor is a row number, which filtering would change.
                if visual { self.end_visual(i); }
                self.panes[i].vim_filtering = true;
            }
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
                let slots = self.tab_slots(i);
                let len = slots.len();
                let at = slots.iter().position(|slot| *slot == (i, self.panes[i].active)).unwrap_or(0);
                let (pane, tab) = slots[if command == Command::PrevTab { (at + len - 1) % len } else { (at + 1) % len }];
                self.acts.push(Act::SwitchTab(pane, tab));
            }
            // panels, Hyprland style
            Command::NewPanel => self.acts.push(Act::OpenPanel(i, None)),
            Command::SaveProfile => self.dialog = Some(self.new_profile_dialog()),
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
            // Space previews (setting PreviewShortcut)
            Command::TogglePreview => {
                if self.preview_may_toggle() {
                    self.preview_window = !self.preview_window;
                }
            }
            Command::Palette => self.open_palette(crate::palette::Mode::Places),
            Command::PaletteCommands => self.open_palette(crate::palette::Mode::Commands),
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
                    let menu_id = ui.scope_id().with("theme_menu");
                    let open = egui::Popup::is_id_open(ui.ctx(), menu_id);
                    let button = icon_button(ui, "◑", "Theme", open, &pal);
                    egui::Popup::menu(&button)
                        .id(menu_id)
                        .align(egui::RectAlign::TOP_START)
                        .gap(4.0)
                        .show(|ui| self.theme_menu(ui));
                    if icon_button(ui, "#tree", "Tree files: click arrows to expand, double-click folders to open", self.st.tree_files, &pal).clicked() {
                        self.st.tree_files = !self.st.tree_files;
                        for pane in &mut self.panes { pane.view_key = None; pane.view.clear(); }
                        self.mark_dirty();
                    }
                    let tip = &self.tip("Open a new panel next to the active one", Command::NewPanel);
                    if icon_button(ui, "#dual", tip, false, &pal).clicked() {
                        self.acts.push(Act::OpenPanel(self.active, None));
                    }
                    if icon_button(
                        ui,
                        "👁",
                        &self.tip("Toggle hidden files", Command::ToggleHidden),
                        self.st.show_hidden,
                        &pal,
                    )
                    .clicked()
                    {
                        self.st.show_hidden = !self.st.show_hidden;
                        self.mark_dirty();
                    }
                    let tip = &self.tip("Search files starting from currently opened folder", Command::Search);
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
                    ui.spacing_mut().item_spacing.y = f32::from(SIDEBAR_GAP);
                    self.st.normalize_sidebar();
                    for section in self.st.sidebar_order.clone() {
                        // A hidden section must add no space of its own between the cards.
                        if !self.section_shown(section) { continue; }
                        ui.push_id(("section", format!("{section:?}")), |ui| match section {
                            SidebarSection::Drives => self.drives_card(ui),
                            SidebarSection::Cloud => self.cloud_card(ui),
                            SidebarSection::Favorite(g) => self.fav_group(ui, g),
                            SidebarSection::History => self.history_card(ui),
                            SidebarSection::Profiles => self.profiles_card(ui),
                        });
                    }
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), self.row_h() + 4.0), Sense::hover());
                    let half = (row.width() - 2.0) / 2.0;
                    let left = Rect::from_min_size(row.min, vec2(half, row.height()));
                    let right = Rect::from_min_size(pos2(row.max.x - half, row.min.y), vec2(half, row.height()));
                    if self.add_button(ui, left, "+  New group", "Creates new Favorite group and adds current folder").clicked() {
                        self.dialog = Some(Dialog::NewGroup { name: String::new(), focus: true });
                    }
                    let tip = self.tip("Saves the open panels, their tabs and layout under a name", Command::SaveProfile);
                    if self.add_button(ui, right, "+  New profile", &tip).clicked() {
                        self.dialog = Some(self.new_profile_dialog());
                    }
                    ui.add_space(8.0);
                });
        });
    }

    /// A quiet button at the sidebar's foot that lights up like a row under the pointer.
    fn add_button(&self, ui: &mut Ui, r: Rect, text: &str, tip: &str) -> egui::Response {
        let pal = self.pal;
        let resp = ui.interact(r, ui.scope_id().with(text), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() {
            ui.painter().rect_filled(r, pal.corner(), pal.hover);
        }
        let font = FontId::proportional(13.0);
        let label = fit_text(ui.painter(), text, &font, r.width() - 8.0);
        let color = if resp.hovered() { pal.text } else { pal.text_weak };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, font, color);
        resp.on_hover_text(tip)
    }

    /// Asks for a profile's name, suggesting the active panel's folder.
    pub(crate) fn new_profile_dialog(&self) -> Dialog {
        let name = fsx::display_name(self.panes[self.active].path());
        Dialog::NewProfile { name, focus: true }
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
                .rect_filled(r, pal.corner(), pal.selection());
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(r, pal.corner(), pal.hover);
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
                    symbol_menu(ui, "#plus", &pal, |ui| {
                        if ui.add_enabled(!is_computer(&cur), egui::Button::new("Add current folder")).clicked() {
                            self.acts.push(Act::AddFavorite(g, cur.clone()));
                            ui.close();
                        }
                        if ui.button("Add location…").clicked() {
                            self.dialog = Some(Dialog::AddLocation { group: g, name: String::new(), focus: true });
                            ui.close();
                        }
                    }).on_hover_text("Add a folder or network location to this group");
                    symbol_menu(ui, "#more", &pal, |ui| {
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
        // A scrolled-away part must not catch a drop aimed at what covers it.
        self.drop_zones.push((r.response.rect.intersect(ui.clip_rect()), target));
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
        if button.hovered() && !busy { ui.painter().rect_filled(rect, pal.corner(), pal.hover); }
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
        p.rect_filled(r, pal.corner(), bg);
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
            pal.corner_small(),
            blend(pal.panel, pal.text_weak, 0.2),
        );
        let fill = Rect::from_min_size(bar.min, vec2(bar.width() * pct, bar.height()));
        p.rect_filled(
            fill,
            pal.corner_small(),
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
                        "#close",
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

    fn profiles_card(&mut self, ui: &mut Ui) {
        let pal = self.pal;
        let profiles = self.st.profiles.clone();
        let r = card(ui, &pal, |ui| {
            ui.horizontal(|ui| {
                Self::sidebar_heading(ui, &pal, "Profiles", SidebarSection::Profiles);
            });
            ui.spacing_mut().item_spacing.y = 1.0;
            for (k, profile) in profiles.iter().enumerate() {
                let h = self.row_h() + 1.0;
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                if resp.hovered() {
                    ui.painter().rect_filled(r, pal.corner(), pal.hover);
                }
                let ir = Rect::from_min_size(pos2(r.min.x + 4.0, r.center().y - 8.0), vec2(16.0, 16.0));
                icons::face(ui.painter(), ir, "#layout", 14.0, pal.accent);
                let font = FontId::proportional(self.st.font_size);
                let t = fit_text(ui.painter(), &profile.name, &font, r.max.x - 4.0 - (ir.max.x + 6.0));
                ui.painter().text(pos2(ir.max.x + 6.0, r.center().y), Align2::LEFT_CENTER, t, font, pal.text);
                if resp.clicked() {
                    self.acts.push(Act::OpenProfile(k));
                }
                resp.on_hover_text(profile_summary(profile)).context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        self.acts.push(Act::OpenProfile(k));
                        ui.close();
                    }
                    if ui.button("Update to current layout").clicked() {
                        self.acts.push(Act::UpdateProfile(k));
                        ui.close();
                    }
                    if ui.button("Rename…").clicked() {
                        self.dialog = Some(Dialog::RenameProfile { profile: k, name: profile.name.clone(), focus: true });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Delete profile").clicked() {
                        self.acts.push(Act::DeleteProfile(k));
                        ui.close();
                    }
                });
            }
        });
        self.sidebar_group_drop(ui, &r.response, SidebarSection::Profiles);
    }

    // ================================================================= browsers

    /// Panels tile the area like Hyprland's dwindle layout; dividers drag to resize,
    /// a double-click on a divider makes both sides equal.
    fn browsers(&mut self, ui: &mut Ui) {
        let full = ui.max_rect();
        // The panels stand apart like the sidebar's cards, with no lines between them.
        let mut rects = Vec::new();
        let mut divs = Vec::new();
        self.panels_area = full.size();
        self.panels_stacked = self.panes.len() > 1 && !self.tiles.fits(full.size());
        if self.panels_stacked {
            // Only the active panel fits. The tab strip lists the others, and the layout
            // comes back as it was when the window grows.
            rects.push((self.active, full));
        } else {
            self.tiles
                .layout(full, &mut rects, &mut divs, &mut Vec::new());
            // Each panel after the first leaves the rest of a card's gap before it.
            let lead = f32::from(SIDEBAR_GAP) - crate::tiles::GAP;
            for (_, r) in &mut rects {
                if r.min.x > full.min.x + 0.5 { r.min.x += lead; }
                if r.min.y > full.min.y + 0.5 { r.min.y += lead; }
            }
        }
        for (i, r) in rects.clone() {
            if i < self.panes.len() {
                self.pane(ui, i, r);
            }
        }
        // The whole gap between two panels moves the split, and the split follows the gap's middle.
        let gap = f32::from(SIDEBAR_GAP);
        for d in &divs {
            let id = ui.scope_id().with(("divider", &d.path));
            let between = Rect::from_min_size(d.rect.min, if d.vertical { vec2(d.rect.width(), gap) } else { vec2(gap, d.rect.height()) });
            let resp = ui.interact(between, id, Sense::click_and_drag());
            let icon = if d.vertical {
                egui::CursorIcon::ResizeVertical
            } else {
                egui::CursorIcon::ResizeHorizontal
            };
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(icon);
                let line = if d.vertical { vec2(between.width(), 2.0) } else { vec2(2.0, between.height()) };
                ui.painter().rect_filled(
                    Rect::from_center_size(between.center(), line),
                    self.pal.corner_small(),
                    blend(self.pal.border, self.pal.accent, 0.6),
                );
            }
            if resp.dragged()
                && let Some(pos) = resp.interact_pointer_pos() {
                    // The split is the gap's first point, half a gap before the pointer.
                    let ratio = if d.vertical {
                        (pos.y - gap / 2.0 - d.area.min.y) / d.area.height().max(1.0)
                    } else {
                        (pos.x - gap / 2.0 - d.area.min.x) / d.area.width().max(1.0)
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
        if let Some(payload) = egui::DragAndDrop::payload::<TabDrag>(ui.ctx())
            && let Some(pos) = ui.input(|inp| inp.pointer.interact_pos())
                && let Some((target, r)) = rects.iter().find(|(_, r)| r.contains(pos)) {
                    let in_tabs = pos.y < r.min.y + 1.0 + TAB_H;
                    let local = (pos - r.min) / r.size();
                    let distances = [local.x, 1.0 - local.x, local.y, 1.0 - local.y];
                    let edge = (0..4).min_by(|a, b| distances[*a].total_cmp(&distances[*b])).unwrap();
                    let split = (!in_tabs && distances[edge] < 0.25).then_some((edge >= 2, edge == 0 || edge == 2));
                    let slots = self.tab_slots(*target);
                    // The tabs of this strip as they were just drawn, each with the gap after it.
                    let drawn: Vec<Rect> = self.tab_rects.iter().filter(|(strip, ..)| strip == target).map(|(.., rect)| *rect).collect();
                    // Past the middle of a tab means after it.
                    let slot = if in_tabs {
                        drawn.iter().take_while(|tab| pos.x > tab.center().x).count()
                    } else { slots.len() }.min(slots.len());
                    // Dropped on the list it joins the panel in view; on the strip, its neighbours.
                    let (place, at) = match slots.get(slot) {
                        Some(&here) if in_tabs => here,
                        _ if !in_tabs => (*target, self.panes[*target].tabs.len()),
                        _ => slots.last().map_or((*target, 0), |&(pane, tab)| (pane, tab + 1)),
                    };
                    let valid = *target != payload.pane
                        || (payload.tab.is_some() && (split.is_none() || self.panes[payload.pane].tabs.len() > 1));
                    if valid {
                        // Judged by the layout the drop would leave behind.
                        let refused = split.is_some_and(|(vertical, before)| {
                            let mut after = self.tiles.clone();
                            if payload.tab.is_none() || self.panes[payload.pane].tabs.len() == 1 {
                                after.relocate(payload.pane, *target, vertical, before);
                            } else {
                                after.insert_at(*target, usize::MAX, vertical, before);
                            }
                            !after.fits(full.size())
                        });
                        let mut preview = *r;
                        if split.is_some() {
                            match edge {
                                0 => preview.max.x = r.center().x,
                                1 => preview.min.x = r.center().x,
                                2 => preview.max.y = r.center().y,
                                _ => preview.min.y = r.center().y,
                            }
                        } else if in_tabs {
                            let x = match drawn.get(slot) {
                                Some(tab) => tab.min.x,
                                None => drawn.last().map_or(r.min.x + 5.0, |tab| tab.max.x + 2.0),
                            }.min(r.max.x - 2.0);
                            // From the top of a tab to its bottom: see `tab_bar`, inside the panel's 1-point edge.
                            preview = Rect::from_min_max(pos2(x, r.min.y + 1.0 + 4.0), pos2(x + 2.0, r.min.y + 1.0 + TAB_H));
                        }
                        let tint = if refused { hex("#ef7063").gamma_multiply(0.25) } else { self.pal.accent.gamma_multiply(if in_tabs { 0.9 } else { 0.25 }) };
                        ui.painter().rect_filled(preview, if in_tabs { self.pal.corner_small() } else { self.pal.corner_large() }, tint);
                        if refused {
                            ui.painter().text(preview.center(), Align2::CENTER_CENTER, NO_ROOM_TO_SPLIT, FontId::proportional(self.st.font_size), self.pal.text);
                        }
                        ui.ctx().set_cursor_icon(if refused { egui::CursorIcon::NotAllowed } else { egui::CursorIcon::Grabbing });
                        if ui.input(|inp| inp.pointer.any_released()) {
                            if refused {
                                let side = if split.is_some_and(|(vertical, _)| vertical) { "taller" } else { "wider" };
                                self.toast(ToastKind::Info, format!("{NO_ROOM_TO_SPLIT}. Make the window {side} first"));
                            } else {
                                self.acts.push(match payload.tab {
                                    Some(tab) => Act::DockTab { source: payload.pane, tab, target: place, at, split },
                                    None => Act::DockPanel { source: payload.pane, target: place, at, split },
                                });
                            }
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
            ui.painter().rect_filled(preview, self.pal.corner_large(), self.pal.accent.gamma_multiply(0.25));
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
        painter.rect_filled(rect, pal.corner_large(), pal.surface(fill));
        painter.rect_stroke(rect, pal.corner_large(), pal.outline(), egui::StrokeKind::Inside);
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

        let header = take_top(&mut r, HEADER_H);
        self.header(ui, i, header);
        if self.st.show_preview && r.height() > DETAILS_H + 120.0 {
            let det = take_bottom(&mut r, DETAILS_H);
            self.details_panel(ui, i, det);
        }
        if r.height() >= 24.0 {
            self.file_list(ui, i, r.shrink2(vec2(4.0, 0.0)));
        }
        ui.set_clip_rect(outer_clip);
        self.status_bar(ui, i, status);
    }

    fn tab_bar(&mut self, ui: &mut Ui, i: usize, r: Rect) {
        let pal = self.pal;
        let p = ui.painter_at(r);
        let slots = self.tab_slots(i);
        let widths = self.tab_widths(ui, &slots, r.width());
        // A tab lies this far inside the panel's edge, at the left and above: 4 here and the
        // 1 by which the panel's content is inset.
        const TAB_INSET: f32 = 4.0;
        let corner = pal.corner_within(TAB_INSET + 1.0);
        let mut x = r.min.x + TAB_INSET;
        let font = FontId::proportional(self.st.font_size);
        let shown = i;
        // From here to the end of the loop, `i` is the panel the tab really belongs to.
        for ((i, t), w) in slots.into_iter().zip(widths) {
            let tr = Rect::from_min_size(pos2(x, r.min.y + TAB_INSET), vec2(w - 2.0, r.height() - TAB_INSET));
            x += w;
            self.tab_rects.push((shown, i, t, tr));
            let id = ui.scope_id().with(("tab", i, t));
            let resp = ui.interact(tr, id, Sense::click_and_drag());
            if resp.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), TabDrag { pane: i, tab: Some(t) });
            }
            if resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            let selected = i == shown && self.panes[i].active == t;
            let look = crate::theme::tab_look(&pal, selected, self.active == shown, resp.hovered());
            let bg = look.fill;
            p.rect_filled(tr, corner, bg);
            let path = self.panes[i].tabs[t].path.clone();
            let ir =
                Rect::from_min_size(pos2(tr.min.x + 8.0, tr.center().y - 7.0), vec2(14.0, 14.0));
            if is_computer(&path) {
                icons::drive(&p, ir, pal.text_weak);
            } else {
                icons::folder(&p, ir, self.folder_color(&path));
            }
            let name = tab_name(&path);
            let show_x = true;
            let tw = tr.width() - 32.0 - if show_x { 18.0 } else { 0.0 };
            p.text(
                pos2(ir.max.x + 6.0, icons::text_level(&p, ir.center().y, font.clone())),
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
                    p.rect_filled(xr, pal.corner(), blend(bg, pal.text, 0.15));
                }
                icons::glyph(&p, xr, "close", pal.text_weak);
                if xresp.on_hover_text(self.tip("Close tab", Command::CloseTab)).clicked() {
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
                    for k in (0..self.panes[i].tabs.len()).rev().filter(|k| *k != t) {
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
        // Panels stacked into one strip have no layout to move or flip.
        let n_panels = if self.panels_stacked { 1 } else { self.panes.len() };
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
                p.rect_filled(br, pal.corner(), pal.hover);
            }
            icons::glyph(&p, br, "grip", if resp.hovered() { pal.text } else { pal.text_weak });
            resp.on_hover_text("Drag to move this panel with all its tabs: join another panel, or split at an edge");
        }
        let mut panel_btn = |ui: &mut Ui, glyph: &str, tip: &str, key: &str| -> bool {
            let br = Rect::from_min_max(pos2(bx - 24.0, r.min.y + 5.0), pos2(bx, r.max.y - 3.0));
            bx -= 26.0;
            let resp = ui.interact(br, ui.scope_id().with(("panelbtn", i, key)), Sense::click());
            if resp.hovered() {
                p.rect_filled(br, pal.corner(), pal.hover);
            }
            let col = if resp.hovered() {
                pal.text
            } else {
                pal.text_weak
            };
            icons::face(&p, br, glyph, 13.0, col);
            resp.on_hover_text(tip).clicked()
        };
        if panel_btn(
            ui,
            "#dual",
            &self.tip("Open a new panel next to this one", Command::NewPanel),
            "split",
        ) {
            self.acts.push(Act::OpenPanel(i, None));
        }
        if n_panels > 1
            && panel_btn(
                ui,
                "#flip",
                &self.tip("Flip this panel's split direction", Command::ToggleSplit),
                "flip",
            )
        {
            self.acts.push(Act::ToggleSplit(i));
        }
        let pr = Rect::from_min_size(pos2(x + 2.0, r.min.y + 6.0), vec2(26.0, r.height() - 8.0));
        let presp = ui.interact(pr, ui.scope_id().with(("newtab", i)), Sense::click());
        if presp.hovered() {
            p.rect_filled(pr, pal.corner(), pal.hover);
        }
        icons::glyph(&p, pr, "plus", pal.text_weak);
        if presp.on_hover_text(self.tip("Create new tab", Command::NewTab)).clicked() {
            self.acts.push(Act::NewTab(i));
        }
    }
}
