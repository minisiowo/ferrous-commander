//! The quick palette: one field for going to places, running commands and changing settings.
//! What it lists and how typed text ranks it is in `palette.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange, CharIndex, LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, CornerRadius, Event, FontId, Frame, Key, Margin, Rect, Sense, TextEdit, pos2, vec2};

use crate::app::{Act, App, is_computer};
use crate::fsx::{self, Entry, SortBy};
use crate::icons;
use crate::keymap::{COMMANDS, Chord, Command, Group, Layer, Matcher, Outcome, Target};
use crate::ops;
use crate::palette::{self, Item, Mode, Pick, Setting};
use crate::settings::Layout;
use crate::theme::{ThemeName, blend};
use crate::ui::fit_text;

/// How many results are listed. More than this is a reason to type another letter.
const ROWS_MAX: usize = 100;
/// How many rows show without scrolling.
const ROWS_SHOWN: usize = 10;
/// Folders visited, kept for the palette's history. The sidebar shows only the latest few.
pub const RECENT_FOLDERS_MAX: usize = 200;
/// Commands and settings used from the palette, offered first the next time.
const RECENT_PICKS_MAX: usize = 8;
/// A pause in typing before the folders below the current one are searched.
const DEEP_DELAY: Duration = Duration::from_millis(140);
const DEEP_RESULTS_MAX: usize = 300;
/// How often what the search has found joins the list while it runs, so rows do not shuffle on every find.
const DEEP_REFRESH: Duration = Duration::from_millis(250);
/// A folder larger than this is left to the search below it, which is limited.
const HERE_MAX: usize = 20_000;

/// A search of the folders below the active one, for the text it was started with.
struct Deep {
    needle: String,
    root: PathBuf,
    hidden: bool,
    /// `None` once it has ended and `found` holds all of its results.
    run: Option<ops::Search>,
    found: Vec<Entry>,
    /// What the search before this one found that matches this text too. It stays listed until
    /// this search ends, so that the list does not empty and fill again with every letter.
    before: Vec<Entry>,
    /// When `found` last took the search's results.
    taken: Instant,
}

/// The words a search looks for, as `ops::search_within` splits them.
fn search_words(needle: &str) -> Vec<String> {
    needle.to_lowercase().split_whitespace().map(String::from).collect()
}

impl Deep {
    /// Takes what the search has found since the last time, and notes its end.
    fn take(&mut self) {
        let Some(run) = &self.run else { return };
        // Read before the results: once done, they are all there.
        let done = run.done.load(std::sync::atomic::Ordering::Relaxed);
        {
            let results = run.results.lock().unwrap();
            if results.len() > self.found.len() {
                self.found.extend_from_slice(&results[self.found.len()..]);
            }
        }
        self.taken = Instant::now();
        if done {
            self.before.clear();
            self.run = None;
        }
    }

    /// Whether `found` is everything below the root that matches the needle.
    fn whole(&self) -> bool {
        self.run.is_none() && self.found.len() < DEEP_RESULTS_MAX
    }
}

/// An item as listed, with the characters of its title that matched what was typed.
struct Row {
    item: Item,
    title: Vec<usize>,
}

pub struct PaletteUi {
    /// The field's text, with the character that chooses the mode.
    pub query: String,
    pub selected: usize,
    items: Vec<Item>,
    rows: Vec<Row>,
    /// What the items were gathered for; `None` gathers them again.
    gathered: Option<(Mode, PathBuf, u64)>,
    /// The first items are those gathered; after them come the ones the search found.
    gathered_len: usize,
    /// The paths of the items gathered, which the search must not list again.
    known: HashSet<PathBuf>,
    /// How much of the search's results the items hold: found now, and kept from before.
    deep_shown: (usize, usize),
    /// The text the rows were ranked for.
    ranked: Option<String>,
    deep: Option<Deep>,
    /// When the text last changed.
    changed: Instant,
    /// The selected row must be scrolled into view.
    reveal: bool,
    /// The text was set from outside the field, which must put its cursor at the end.
    cursor_to_end: bool,
    /// Not drawn yet: the key press that opened it is still among this frame's events.
    fresh: bool,
}

impl PaletteUi {
    pub fn new(mode: Mode) -> PaletteUi {
        PaletteUi {
            query: mode.prefix().to_string(),
            selected: 0,
            items: Vec::new(),
            rows: Vec::new(),
            gathered: None,
            gathered_len: 0,
            known: HashSet::new(),
            deep_shown: (0, 0),
            ranked: None,
            deep: None,
            changed: Instant::now(),
            reveal: false,
            cursor_to_end: true,
            fresh: true,
        }
    }

    pub fn mode(&self) -> Mode {
        palette::split(&self.query).0
    }

    fn set_mode(&mut self, mode: Mode) {
        let needle = palette::split(&self.query).1;
        self.query = format!("{}{needle}", mode.prefix());
        self.cursor_to_end = true;
    }

    /// The titles listed, best first.
    #[cfg(test)]
    pub fn titles(&self) -> Vec<&str> {
        self.rows.iter().map(|row| row.item.title.as_str()).collect()
    }

    #[cfg(test)]
    pub fn searching(&self) -> bool {
        self.deep.as_ref().is_some_and(|deep| deep.run.is_some())
    }

    fn move_by(&mut self, by: isize, wrap: bool) {
        let n = self.rows.len() as isize;
        if n == 0 { return; }
        let to = self.selected as isize + by;
        self.selected = if wrap { to.rem_euclid(n) } else { to.clamp(0, n - 1) } as usize;
        self.reveal = true;
    }

    /// Starts, keeps or drops the search below `root`, and takes what it has found so far.
    fn search_below(&mut self, mode: Mode, needle: &str, root: &Path, show_hidden: bool, ctx: &egui::Context) {
        let wanted = mode == Mode::Places && needle.chars().count() >= 2 && !is_computer(root)
            && !crate::ui_list::typed_path(needle).is_absolute();
        if !wanted {
            self.deep = None;
            return;
        }
        if !self.deep.as_ref().is_some_and(|deep| deep.needle == needle && deep.root == root && deep.hidden == show_hidden) {
            // Until typing pauses, the last search keeps its results on screen.
            let waited = self.changed.elapsed();
            if waited < DEEP_DELAY {
                ctx.request_repaint_after(DEEP_DELAY - waited);
            } else {
                let words = search_words(needle);
                let mut old = self.deep.take().filter(|deep| deep.root == root && deep.hidden == show_hidden);
                if let Some(old) = &mut old { old.take(); }
                // Text that only narrows a search that ended needs no second walk of the disk:
                // what it would find is among what was found.
                let narrowed = old.as_ref().is_some_and(|old| {
                    old.whole() && search_words(&old.needle).iter().all(|was| words.iter().any(|word| word.contains(was.as_str())))
                });
                // The same rule as the search's own, so nothing kept is what it would not find.
                let mut seen = HashSet::new();
                let kept: Vec<Entry> = old.map(|old| old.found.into_iter().chain(old.before)
                        .filter(|e| words.iter().all(|word| e.lname().contains(word.as_str())) && seen.insert(e.path.clone()))
                        .collect())
                    .unwrap_or_default();
                let (run, found, before) = if narrowed {
                    (None, kept, Vec::new())
                } else {
                    let run = ops::search_within(root.to_path_buf(), needle.to_string(), show_hidden, Some(DEEP_RESULTS_MAX), ctx.clone());
                    (Some(run), Vec::new(), kept)
                };
                self.deep = Some(Deep { needle: needle.to_string(), root: root.to_path_buf(), hidden: show_hidden, run, found, before, taken: Instant::now() });
            }
        }
        if let Some(deep) = &mut self.deep && let Some(run) = &deep.run {
            let done = run.done.load(std::sync::atomic::Ordering::Relaxed);
            let first = deep.found.is_empty() && deep.before.is_empty();
            let waited = deep.taken.elapsed();
            if done || first || waited >= DEEP_REFRESH {
                deep.take();
            } else {
                ctx.request_repaint_after(DEEP_REFRESH - waited);
            }
        }
    }
}

/// What happened in the palette this frame, acted on once it has been drawn.
enum Verdict {
    Close,
    Run { row: usize, ctrl: bool, shift: bool },
}

fn group_name(group: Group) -> &'static str {
    crate::ui_keys::GROUPS.iter().find(|(g, _)| *g == group).map_or("", |(_, name)| name)
}

/// Commands that only mean something on the row under the cursor, or inside the list.
fn offered(command: Command) -> bool {
    let info = command.info();
    info.group != Group::Moving
        && !matches!(command, Command::Open | Command::OpenNewTab | Command::FolderIn | Command::FolderOut
            | Command::Left | Command::Right | Command::ParentOrErase | Command::Escape | Command::Filter
            | Command::Visual | Command::VisualCopy | Command::VisualCut | Command::EditPathSelected
            | Command::Palette | Command::PaletteCommands)
        && (command != Command::Properties || cfg!(target_os = "linux"))
}

fn entry_item(e: &Entry, detail: String, badge: &str) -> Item {
    let pick = if e.is_dir { Pick::Folder(e.path.clone()) } else { Pick::File(e.path.clone()) };
    Item::new(e.name(), detail, badge, pick)
}

/// Adds what the search found to `items`, but for the paths in `known`.
fn add_deep_items(deep: &Deep, known: &HashSet<PathBuf>, items: &mut Vec<Item>) {
    let mut seen: HashSet<&Path> = HashSet::new();
    for e in deep.found.iter().chain(&deep.before) {
        if known.contains(&e.path) || !seen.insert(&e.path) { continue; }
        // The deeper it lies, the less likely it is the one meant.
        let inside = e.path.parent().and_then(|p| p.strip_prefix(&deep.root).ok());
        let depth = inside.map_or(8, |p| p.components().count().min(8)) as i32;
        let detail = inside.map_or_else(|| parent_text(&e.path), |p| p.display().to_string());
        items.push(entry_item(e, detail, "").boost(-15 * depth));
    }
}

fn parent_text(path: &Path) -> String {
    path.parent().map(|p| p.display().to_string()).unwrap_or_default()
}

impl App {
    pub(crate) fn open_palette(&mut self, mode: Mode) {
        self.palette = Some(PaletteUi::new(mode));
    }

    /// Folders visited, the latest first: the long list, then what only the sidebar still has.
    fn visited(&self) -> impl Iterator<Item = &PathBuf> {
        self.st.recent_folders.iter().chain(&self.st.history)
    }

    /// The places known without searching, and their paths.
    fn place_items(&self) -> (Vec<Item>, HashSet<PathBuf>) {
        let i = self.active;
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut items = Vec::new();
        let mut add = |item: Item, path: &Path| {
            if seen.insert(path.to_path_buf()) { items.push(item); }
        };
        let here = self.panes[i].path();
        for path in self.visited().filter(|p| p.as_path() != here) {
            add(Item::new(fsx::display_name(path), parent_text(path), "recent", Pick::Folder(path.clone())).boost(80), path);
        }
        for group in &self.st.favorites {
            for path in &group.paths {
                // A shortcut can be a file, and only opening it finds that out.
                let pick = Pick::Typed(path.display().to_string());
                add(Item::new(group.label(path), parent_text(path), group.name.clone(), pick).boost(70), path);
            }
        }
        for drive in &self.drives {
            let name = if drive.label.is_empty() { drive.name.clone() } else { format!("{} {}", drive.name, drive.label) };
            add(Item::new(name, drive.path.display().to_string(), "drive", Pick::Folder(drive.path.clone())).boost(40), &drive.path);
        }
        for path in &self.cloud {
            add(Item::new(fsx::display_name(path), parent_text(path), "cloud", Pick::Folder(path.clone())).boost(40), path);
        }
        if let Some(entries) = self.pane_entries(i).filter(|entries| entries.len() <= HERE_MAX) {
            for e in entries.iter().filter(|e| self.st.show_hidden || !e.hidden) {
                add(entry_item(e, String::new(), "here").boost(60), &e.path);
            }
        }
        (items, seen)
    }

    fn command_items(&self) -> Vec<Item> {
        let st = &self.st;
        let mut items = Vec::new();
        for info in COMMANDS.iter().filter(|info| offered(info.command)) {
            let state = match info.command {
                Command::ToggleHidden => Some(st.show_hidden),
                Command::ToggleColumns => Some(st.show_columns),
                _ => None,
            };
            let keys = self.key_hint(info.command).unwrap_or_default();
            let mut item = Item::new(info.label, group_name(info.group), keys, Pick::Command(info.command));
            item.state = state;
            items.push(item);
        }
        let sorting = self.sorting_for(self.panes[self.active].path());
        for (by, name) in [(SortBy::Name, "name"), (SortBy::Date, "date"), (SortBy::Size, "size"), (SortBy::Type, "type")] {
            let badge = match (sorting.by == by, sorting.ascending) {
                (false, _) => "",
                (true, true) => "ascending",
                (true, false) => "descending",
            };
            items.push(Item::new(format!("Sort by {name}"), "This folder", badge, Pick::Sort(by)));
        }
        let settings = [
            (Setting::Sidebar, "Sidebar", Some(st.show_sidebar)),
            (Setting::PreviewPane, "Preview and details pane", Some(st.show_preview)),
            (Setting::Tree, "Tree of files", Some(st.tree_files)),
            (Setting::ColumnsLayout, "Columns layout", Some(st.layout == Layout::Columns)),
            (Setting::Extensions, "File extensions", Some(st.show_extensions)),
            (Setting::AgeColors, "Colours by file age", Some(st.age_colors)),
            (Setting::FoldersOnTop, "Folders above files", Some(st.sorting.folders_on_top)),
            (Setting::Mebibytes, "Sizes in MiB, as in Windows Explorer", Some(st.mebibytes)),
            (Setting::HistoryCard, "History in the sidebar", Some(st.show_history)),
            (Setting::Rounded, "Rounded corners", Some(st.rounded)),
            (Setting::Outlines, "Outlines around panels and cards", Some(st.outlines)),
            (Setting::NeovimKeys, "Neovim-style keys", Some(st.vim_keys)),
            (Setting::WhichKey, "Hints for the next key of a sequence", Some(st.which_key)),
            (Setting::FontLarger, "Larger text", None),
            (Setting::FontSmaller, "Smaller text", None),
            (Setting::OpenSettings, "Open settings", None),
            (Setting::OpenShortcuts, "Customize shortcuts", None),
            (Setting::ConfigFolder, "Open configuration folder", None),
        ];
        for (setting, title, state) in settings {
            let mut item = Item::new(title, "Settings", "", Pick::Setting(setting));
            item.state = state;
            items.push(item);
        }
        for theme in ThemeName::choices() {
            items.push(Item::new(format!("Theme: {}", theme.label()), "Settings", "", Pick::Theme(theme)).state(st.theme == theme));
        }
        // What was used last comes first, and wins among equal matches.
        let place = |item: &Item| item.pick.id().and_then(|id| st.palette_recent.iter().position(|r| *r == id));
        for item in &mut items {
            if let Some(at) = place(item) { item.boost = 2 * (RECENT_PICKS_MAX - at.min(RECENT_PICKS_MAX)) as i32; }
        }
        items.sort_by_key(|item| place(item).unwrap_or(usize::MAX));
        items
    }

    fn history_items(&self) -> Vec<Item> {
        let mut seen = HashSet::new();
        self.visited().filter(|path| seen.insert(path.as_path()))
            .map(|path| Item::new(fsx::display_name(path), parent_text(path), "", Pick::Folder(path.clone())))
            .collect()
    }

    /// Brings the rows up to date with the text, the folder and what the search has found.
    fn palette_rows(&mut self, palette: &mut PaletteUi, ctx: &egui::Context) {
        let query = palette.query.clone();
        let (mode, needle) = palette::split(&query);
        let here = self.panes[self.active].path().to_path_buf();
        palette.search_below(mode, needle, &here, self.st.show_hidden, ctx);
        let listing = self.cache.get(&here).map_or(0, |l| l.generation);
        let key = (mode, here, listing);
        let mut gathered = palette.gathered.as_ref() == Some(&key);
        if !gathered {
            (palette.items, palette.known) = match mode {
                Mode::Places => self.place_items(),
                Mode::Commands => (self.command_items(), HashSet::new()),
                Mode::History => (self.history_items(), HashSet::new()),
            };
            palette.gathered_len = palette.items.len();
            palette.deep_shown = (0, 0);
            palette.gathered = Some(key);
        }
        // Only what the search found is made again as it finds more: the folder can be large.
        let deep_now = palette.deep.as_ref().map_or((0, 0), |deep| (deep.found.len(), deep.before.len()));
        if palette.deep_shown != deep_now {
            palette.items.truncate(palette.gathered_len);
            if let Some(deep) = &palette.deep { add_deep_items(deep, &palette.known, &mut palette.items); }
            palette.deep_shown = deep_now;
            gathered = false;
        }
        let typed = palette.ranked.as_deref() != Some(query.as_str());
        if gathered && !typed { return; }
        // New results must not move the selection away from the row it was put on.
        let kept = (!typed && palette.selected > 0).then(|| palette.rows.get(palette.selected).map(|row| row.item.pick.clone())).flatten();
        let mut rows: Vec<Row> = palette::rank(&palette.items, needle, ROWS_MAX).into_iter()
            .map(|hit| Row { item: palette.items[hit.item].clone(), title: hit.title })
            .collect();
        let path = crate::ui_list::typed_path(needle);
        if mode == Mode::Places && path.is_absolute() {
            let item = Item::new(needle, "", "path", Pick::Typed(needle.to_string()));
            rows.insert(0, Row { item, title: Vec::new() });
        }
        let was = palette.selected;
        palette.selected = kept.and_then(|pick| rows.iter().position(|row| row.item.pick == pick)).unwrap_or(0);
        // Results that only arrive leave the list where the wheel put it.
        if typed || palette.selected != was { palette.reveal = true; }
        palette.rows = rows;
        palette.ranked = Some(query);
    }

    /// The palette's own keys, taken before the text field or anything else sees them.
    fn palette_keys(&mut self, palette: &mut PaletteUi, ctx: &egui::Context) -> Option<Verdict> {
        let mode = self.key_mode(self.active);
        let mut verdict = None;
        let mut switch = None;
        ctx.input_mut(|input| {
            let held = input.modifiers;
            input.events.retain(|event| {
                let (key, modifiers, repeat) = match event {
                    // Alt and a letter is a shortcut here, never a character to type. AltGr,
                    // which Windows reports as Ctrl+Alt, still types.
                    Event::Text(_) => return !(held.alt && !held.ctrl),
                    Event::Key { key, pressed: true, modifiers, repeat, .. } => (*key, *modifiers, *repeat),
                    _ => return true,
                };
                let chord = Chord::new(key, modifiers);
                let ctrl = chord.ctrl && !chord.alt && !chord.shift;
                // The shortcut that opened the palette closes it, or changes what it lists.
                if !repeat && let Outcome::Run { target: Target::Command(command @ (Command::Palette | Command::PaletteCommands)), .. }
                    = Matcher::default().feed(&self.keys, mode, chord, false)
                {
                    let wanted = if command == Command::Palette { Mode::Places } else { Mode::Commands };
                    if palette.mode() == wanted { verdict = Some(Verdict::Close); } else { switch = Some(wanted); }
                    return false;
                }
                match key {
                    Key::Escape => verdict = Some(Verdict::Close),
                    Key::Enter if !repeat => verdict = Some(Verdict::Run { row: palette.selected, ctrl: chord.ctrl, shift: chord.shift }),
                    Key::Enter => {}
                    Key::ArrowDown => palette.move_by(1, !repeat),
                    Key::ArrowUp => palette.move_by(-1, !repeat),
                    Key::N | Key::J if ctrl => palette.move_by(1, !repeat),
                    Key::P | Key::K if ctrl => palette.move_by(-1, !repeat),
                    Key::PageDown => palette.move_by(ROWS_SHOWN as isize - 1, false),
                    Key::PageUp => palette.move_by(1 - ROWS_SHOWN as isize, false),
                    Key::Home if ctrl => palette.move_by(-(ROWS_MAX as isize), false),
                    Key::End if ctrl => palette.move_by(ROWS_MAX as isize, false),
                    Key::Tab if !chord.ctrl && !chord.alt => switch = Some(palette.mode().next(chord.shift)),
                    _ => return true,
                }
                false
            });
        });
        if let Some(mode) = switch { palette.set_mode(mode); }
        verdict
    }

    pub fn palette_ui(&mut self, ctx: &egui::Context) {
        let Some(mut palette) = self.palette.take() else { return };
        // A question that came up meanwhile needs the keyboard more.
        if self.dialog.is_some() || !self.notices.is_empty() { return; }
        let mut verdict = if std::mem::take(&mut palette.fresh) { None } else { self.palette_keys(&mut palette, ctx) };
        let before = palette.query.clone();
        let pal = self.pal;
        let screen = ctx.content_rect();
        let width = (screen.width() - 40.0).clamp(260.0, 660.0);
        let row_h = self.row_h() + 6.0;
        let field = egui::Id::unique("palette_field");
        let mode = palette.mode();
        let area = egui::Area::new(egui::Id::unique("palette"))
            .order(egui::Order::Foreground)
            // It is asked for by a key and typed into at once: it must be there, not arrive.
            .fade_in(false)
            .fixed_pos(pos2(screen.center().x - width / 2.0, screen.top() + (screen.height() * 0.08).min(56.0)))
            .show(ctx, |ui| {
                ui.painter().with_clip_rect(screen).rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(if pal.dark { 90 } else { 50 }));
                Frame::NONE.fill(pal.content_fill()).corner_radius(pal.corner_large())
                    .stroke(pal.outline()).shadow(ctx.global_style().visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
                        Frame::NONE.inner_margin(Margin::symmetric(12, 9)).show(ui, |ui| ui.horizontal(|ui| {
                            // The symbol and the text stand in the same columns as those of the rows.
                            ui.spacing_mut().item_spacing.x = 10.0;
                            let slot = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            if std::mem::take(&mut palette.cursor_to_end) {
                                let end = CCursor::new(CharIndex(palette.query.chars().count()));
                                let mut state = TextEdit::load_state(ui.ctx(), field).unwrap_or_default();
                                state.cursor.set_char_range(Some(CCursorRange::one(end)));
                                TextEdit::store_state(ui.ctx(), field, state);
                            }
                            // egui's `font` leaves the hint out, so it must be told the same one.
                            let font = FontId::proportional(self.st.font_size);
                            let edit = TextEdit::singleline(&mut palette.query).id(field).frame(Frame::NONE)
                                .hint_text(egui::RichText::new(mode.hint()).font(font.clone())).desired_width(f32::INFINITY)
                                .font(font.clone()).align(Align2::LEFT_CENTER);
                            // Every frame: a click on a row or Tab must not take the keyboard away.
                            let typed = edit.show(ui);
                            typed.response.response.request_focus();
                            // Level with the capitals of the line of text, wherever the field put it.
                            let level = typed.galley_pos.y + typed.galley.rect.center().y + icons::capital_drop(ui.painter(), font);
                            let middle = pos2(slot.0.center().x, level);
                            icons::face(ui.painter(), Rect::from_center_size(middle, slot.0.size()), mode.face(), 15.0, pal.accent);
                            slot.1.on_hover_text(mode.label());
                        }));
                        if palette.query != before { palette.changed = Instant::now(); }
                        self.palette_rows(&mut palette, ui.ctx());
                        let line = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover()).0;
                        ui.painter().rect_filled(line, CornerRadius::ZERO, pal.border);
                        let gutter = ui.spacing().scroll.allocated_width().max(6.0);
                        let list_width = width - gutter;
                        egui::ScrollArea::vertical().max_height(row_h * ROWS_SHOWN as f32).show(ui, |ui| {
                            ui.set_width(list_width);
                            for n in 0..palette.rows.len() {
                                let (r, response) = ui.allocate_exact_size(vec2(list_width, row_h), Sense::click());
                                let selected = n == palette.selected;
                                // At once: an animation would trail behind a held arrow key and fast typing.
                                if selected && std::mem::take(&mut palette.reveal) {
                                    ui.scroll_to_rect_animation(r, None, egui::style::ScrollAnimation::none());
                                }
                                // The rows scrolled out of sight cost their text's layout for nothing.
                                if !ui.is_rect_visible(r) { continue; }
                                // Match the scrollbar gutter on the left, with or without rounded corners.
                                let lit = Rect::from_min_max(r.min + vec2(gutter, 0.0), r.max);
                                if selected {
                                    ui.painter().rect_filled(lit, pal.corner(), pal.selection());
                                } else if response.hovered() {
                                    ui.painter().rect_filled(lit, pal.corner(), pal.hover);
                                }
                                self.palette_row(ui, r, &palette.rows[n], selected);
                                if response.clicked() {
                                    let m = ui.input(|i| i.modifiers);
                                    verdict = Some(Verdict::Run { row: n, ctrl: m.command || m.ctrl, shift: m.shift });
                                }
                            }
                            if palette.rows.is_empty() {
                                let (r, _) = ui.allocate_exact_size(vec2(list_width, row_h), Sense::hover());
                                let text = if palette::split(&palette.query).1.is_empty() { "Nothing here yet" } else { "Nothing matches" };
                                ui.painter().text(pos2(r.min.x + 38.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(self.st.font_size), pal.text_weak);
                            }
                        });
                        self.palette_footer(ui, width, &palette);
                    });
            });
        let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
        match verdict {
            Some(Verdict::Run { row, ctrl, shift }) => {
                let Some(pick) = palette.rows.get(row).map(|r| r.item.pick.clone()) else {
                    self.palette = Some(palette);
                    return;
                };
                if self.palette_run(pick, ctrl, shift) {
                    // The items show what is on and off, which has just changed.
                    palette.gathered = None;
                    palette.selected = row;
                    self.palette = Some(palette);
                }
            }
            Some(Verdict::Close) => {}
            None if outside => {}
            None => self.palette = Some(palette),
        }
    }

    fn palette_row(&self, ui: &egui::Ui, r: Rect, row: &Row, selected: bool) {
        let pal = self.pal;
        let p = ui.painter_at(r);
        let item = &row.item;
        let icon = Rect::from_center_size(pos2(r.min.x + 20.0, r.center().y), vec2(16.0, 16.0));
        match &item.pick {
            Pick::Folder(path) => icons::folder(&p, icon, self.folder_color(path)),
            Pick::Typed(_) => icons::folder(&p, icon, fsx::Kind::Folder.color()),
            Pick::File(_) => {
                let ext = fsx::split_ext(&item.title, false);
                icons::entry(&p, icon, fsx::Kind::from_ext(ext), ext, pal.dark, false);
            }
            Pick::Theme(_) => icons::face(&p, icon, "◑", 13.0, pal.text_weak),
            Pick::Command(_) | Pick::Setting(_) | Pick::Sort(_) => icons::glyph(&p, icon, "next", pal.text_weak),
        }
        let small = FontId::proportional(11.0);
        let mut right = r.max.x - 12.0;
        if !item.badge.is_empty() {
            let drawn = p.text(pos2(right, r.center().y), Align2::RIGHT_CENTER, &item.badge, small.clone(), pal.text_weak);
            right = drawn.min.x - 10.0;
        }
        if let Some(on) = item.state {
            // A small switch, so that on and off read without a word.
            let track = Rect::from_min_size(pos2(right - 24.0, r.center().y - 6.0), vec2(24.0, 12.0));
            let knob = pos2(if on { track.max.x - 6.0 } else { track.min.x + 6.0 }, track.center().y);
            p.rect_filled(track, CornerRadius::same(if pal.round { 6 } else { 0 }), if on { pal.accent } else { blend(pal.panel, pal.text_weak, 0.3) });
            p.rect_filled(Rect::from_center_size(knob, vec2(8.0, 8.0)), pal.corner(), if on { Color32::WHITE } else { pal.text_weak });
            right = track.min.x - 10.0;
        }
        let left = icon.max.x + 10.0;
        let font = FontId::proportional(self.st.font_size);
        let marked = if selected { pal.text_important } else { blend(pal.accent, pal.text, 0.35) };
        let mut job = LayoutJob::default();
        job.wrap = TextWrapping { max_width: (right - left).max(0.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let mut run = String::new();
        let mut run_marked = false;
        let mut flush = |run: &mut String, is_marked: bool| {
            if run.is_empty() { return; }
            let format = TextFormat { font_id: font.clone(), color: if is_marked { marked } else { pal.text }, ..Default::default() };
            job.append(run, 0.0, format);
            run.clear();
        };
        for (at, c) in item.title.chars().enumerate() {
            let is_marked = row.title.contains(&at);
            if is_marked != run_marked { flush(&mut run, run_marked); }
            run_marked = is_marked;
            run.push(c);
        }
        flush(&mut run, run_marked);
        let galley = p.layout_job(job);
        let title_end = left + galley.size().x;
        p.galley(pos2(left, r.center().y - galley.size().y / 2.0), galley, pal.text);
        let room = right - title_end - 12.0;
        if !item.detail.is_empty() && room > 30.0 {
            let detail = fit_text(&p, &item.detail, &small, room);
            p.text(pos2(title_end + 12.0, r.center().y + 0.5), Align2::LEFT_CENTER, detail, small, pal.text_weak);
        }
    }

    fn palette_footer(&self, ui: &mut egui::Ui, width: f32, palette: &PaletteUi) {
        let pal = self.pal;
        let (r, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
        let p = ui.painter_at(r);
        p.rect_filled(Rect::from_min_size(r.min, vec2(width, 1.0)), CornerRadius::ZERO, pal.border);
        let keeps = |pick: &Pick| match pick {
            Pick::Theme(_) | Pick::Sort(_) => true,
            Pick::Setting(setting) => !matches!(setting, Setting::OpenSettings | Setting::OpenShortcuts | Setting::ConfigFolder),
            Pick::Command(command) => matches!(command, Command::ToggleHidden | Command::ToggleColumns),
            _ => false,
        };
        let hint = match palette.rows.get(palette.selected).map(|row| &row.item.pick) {
            Some(Pick::Folder(_) | Pick::Typed(_)) => "Enter go · Ctrl+Enter new tab · Shift+Enter other panel",
            Some(Pick::File(_)) => "Enter show in its folder · Ctrl+Enter open",
            Some(pick) if keeps(pick) => "Enter apply · Ctrl+Enter apply and stay",
            Some(_) => "Enter run",
            None => "",
        };
        let small = FontId::proportional(11.0);
        let searching = palette.deep.as_ref().is_some_and(|deep| deep.run.is_some());
        let next = format!("{}Tab {} · Esc close", if searching { "Searching… · " } else { "" }, palette.mode().next(false).label().to_lowercase());
        let drawn = p.text(pos2(r.max.x - 12.0, r.center().y), Align2::RIGHT_CENTER, next, small.clone(), pal.text_weak);
        let hint = fit_text(&p, hint, &small, drawn.min.x - r.min.x - 28.0);
        p.text(pos2(r.min.x + 12.0, r.center().y), Align2::LEFT_CENTER, hint, small, pal.text_weak);
    }

    /// Does what the item stands for. True when the palette stays open to show the change.
    fn palette_run(&mut self, pick: Pick, ctrl: bool, shift: bool) -> bool {
        let i = self.active;
        if let Some(id) = pick.id() {
            self.st.palette_recent.retain(|r| *r != id);
            self.st.palette_recent.insert(0, id);
            self.st.palette_recent.truncate(RECENT_PICKS_MAX);
            self.mark_dirty();
        }
        let mut stays = false;
        match pick {
            Pick::Folder(path) if ctrl => self.acts.push(Act::OpenNewTab(i, path, true)),
            Pick::Folder(path) if shift => self.acts.push(Act::OpenOtherPane(i, path)),
            Pick::Folder(path) => self.acts.push(Act::Navigate(i, path)),
            Pick::File(path) if ctrl => self.acts.push(Act::Open(i, path)),
            Pick::File(path) => {
                if let Some(parent) = path.parent() {
                    self.acts.push(Act::NavigateSelect(i, parent.to_path_buf(), path.clone()));
                }
            }
            Pick::Typed(text) => self.go_to_typed(i, &text, ctrl, shift),
            Pick::Command(command) => {
                let page = ((self.panes[i].list_h / self.row_h()).floor() as usize).max(1);
                self.run_command(i, command, false, None, false, page);
                stays = ctrl && matches!(command, Command::ToggleHidden | Command::ToggleColumns);
            }
            Pick::Sort(by) => {
                self.acts.push(Act::SetSort(i, by));
                stays = ctrl;
            }
            Pick::Theme(theme) => {
                self.st.theme = theme;
                stays = ctrl;
            }
            Pick::Setting(setting) => {
                let st = &mut self.st;
                stays = ctrl;
                match setting {
                    Setting::Extensions => st.show_extensions = !st.show_extensions,
                    Setting::AgeColors => st.age_colors = !st.age_colors,
                    Setting::Mebibytes => st.mebibytes = !st.mebibytes,
                    Setting::FoldersOnTop => st.sorting.folders_on_top = !st.sorting.folders_on_top,
                    Setting::Sidebar => st.show_sidebar = !st.show_sidebar,
                    Setting::PreviewPane => st.show_preview = !st.show_preview,
                    Setting::Tree => st.tree_files = !st.tree_files,
                    Setting::ColumnsLayout => st.layout = if st.layout == Layout::Columns { Layout::Standard } else { Layout::Columns },
                    Setting::NeovimKeys => st.vim_keys = !st.vim_keys,
                    Setting::WhichKey => st.which_key = !st.which_key,
                    Setting::HistoryCard => st.show_history = !st.show_history,
                    Setting::Rounded => st.rounded = !st.rounded,
                    Setting::Outlines => st.outlines = !st.outlines,
                    Setting::FontLarger => st.font_size = (st.font_size + 1.0).min(18.0),
                    Setting::FontSmaller => st.font_size = (st.font_size - 1.0).max(10.0),
                    Setting::OpenSettings => {
                        self.settings_open = true;
                        stays = false;
                    }
                    Setting::OpenShortcuts => {
                        let layer = if st.vim_keys { Layer::Neovim } else { Layer::Standard };
                        self.shortcuts = Some(crate::ui_keys::ShortcutsWindow::new(layer));
                        stays = false;
                    }
                    Setting::ConfigFolder => {
                        self.save();
                        let saved = self.st.save_result();
                        self.show_configuration_folder(saved);
                        stays = false;
                    }
                }
                // As after a change in the settings window: lists follow the new settings.
                self.mark_dirty();
                self.reload_all();
                for pane in &mut self.panes { pane.view_key = None; }
            }
        }
        stays
    }
}
