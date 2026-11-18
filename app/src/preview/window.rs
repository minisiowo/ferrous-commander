//! The floating preview: its own window on the screen, independent of the application's.
//!
//! It runs as a deferred viewport, so scrolling a preview does not redraw the file list.
//! What it shows is handed over through `Shared`; what the user asks for comes back as `Cmd`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, FontId, Key as K, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, pos2, vec2};

use super::load::BlockKind;
use super::text::{CLASS_COMMENT, CLASS_FUNCTION, CLASS_KEYWORD, CLASS_NUMBER, CLASS_STRING, CLASS_TAG, CLASS_TYPE};
use super::load::ArchiveEntry;
use super::load::ModelView;
use super::model::{MIN_MODEL_ZOOM, TILT_OFFSETS};
use super::select::{self, PageText, Selection};
use super::{DocData, Job, Key, PreviewContent, Slot, TableDoc, TextDoc, sound};
use crate::app::{Act, App};
use crate::chrome::{self, BAR_H, Icon};
use crate::fsx::{self, Entry, Kind};
use crate::icons;
use crate::theme::{Palette, blend};
use crate::ui::{fit_text, symbol_button};

/// How long the file list waits for content before showing a window with a spinner.
/// Content that arrives in time also sets the size the window first appears at.
const OPEN_WAIT: Duration = Duration::from_millis(200);
const SPINNER_AFTER: Duration = Duration::from_millis(150);
const SPINNER_FRAME: Duration = Duration::from_millis(33);
const SWITCH_SECONDS: f32 = 0.09;
/// Held Space must not close the preview and open it again from the file list.
const REOPEN_PAUSE: Duration = Duration::from_millis(300);
/// The compositor has decided by then whether the window floats.
const SIZE_LOCK: Duration = Duration::from_millis(520);
/// The smallest window a preview opens at: room for the control bar and a card.
const MIN_OPEN: Vec2 = vec2(480.0, 320.0);
const CONTROL_H: f32 = 36.0;
const CONTROL_MARGIN: f32 = 12.0;
const MUSIC_GAP: f32 = 12.0;
const CONTENT_GAP: f32 = 20.0;
const INFO_ROW_H: f32 = 24.0;
const INFO_PADDING: f32 = 28.0;
const INFO_PICTURE_GAP: f32 = 24.0;

/// The same footer budget sizes the window and keeps its metadata clear of controls.
fn info_footer_height(music: bool) -> f32 {
    CONTROL_MARGIN + CONTROL_H + CONTENT_GAP + if music { MUSIC_GAP + sound::TRANSPORT_H } else { 0.0 }
}

fn navigation_rect(body: Rect, width: f32) -> Rect {
    Rect::from_center_size(pos2(body.center().x, body.max.y - CONTROL_MARGIN - CONTROL_H / 2.0), vec2(width, CONTROL_H))
}

const MAX_ZOOM: f32 = 32.0;
/// Degrees a 3D model turns for each point the pointer moves.
const TURN_PER_POINT: f32 = 0.4;
/// Pause after zooming or moving before a sharper copy is worth making.
const SHARPEN_AFTER: Duration = Duration::from_millis(150);
const READING_WIDTH: f32 = 760.0;
/// Where the text column of the hex view starts.
const HEX_ASCII_X: f32 = 520.0;
/// Past the end of any preview, yet small enough to compute with.
const SCROLL_TO_END: f32 = 1e9;
/// How long the title bar says that text was copied.
const COPIED_FOR: Duration = Duration::from_millis(1500);
/// Room between the pages of a document, and above the first.
const PAGE_GAP: f32 = 12.0;
/// Pages kept at hand on each side of the one in view, to scroll into.
const STRIP_AROUND: u32 = 2;

pub(super) fn viewport() -> ViewportId {
    ViewportId::from_hash_of("ferrous_commander_preview")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cmd {
    Close,
    Step(i32),
    Page(i32),
    /// The page of a document now in view, which loads it and its neighbours.
    ShowPage(u32),
    Detail([u16; 4]),
    Turn(ModelView),
    Open,
    /// The play button of a music file, and moving to a place in it, in milliseconds.
    Play,
    Seek(u64),
}

#[derive(Default)]
struct View {
    zoom: f32,
    pan: Vec2,
    wrap: bool,
    scroll_to: Option<f32>,
    offset: f32,
    /// Visual rows of wrapped text: (line, start, end), and the width they were made for.
    rows: Vec<(u32, u32, u32)>,
    rows_for: (usize, usize),
    /// Show the source of a document instead of its layout.
    source: bool,
    laid_out: Vec<(Arc<egui::Galley>, Pos2, BlockKind)>,
    laid_out_for: (usize, usize),
    laid_out_height: f32,
    moved: Option<Instant>,
    last_look: (f32, Vec2, f32),
    sent: Option<[u16; 4]>,
    /// How far down the pages of a document are scrolled, in pages: 2.5 is halfway
    /// down the third. The page in the middle of the view, and the one last asked for.
    at: f32,
    on_page: u32,
    asked_page: Option<u32>,
    started: Option<Instant>,
    /// Showing a 3D model, which may also get smaller than its first picture.
    model: bool,
    /// Degrees a 3D model is turned and tilted from its first view.
    turn: f32,
    tilt: f32,
    sent_view: Option<ModelView>,
    /// The newest picture of a 3D model seen from elsewhere than at first.
    look: Option<egui::TextureHandle>,
    /// For tracing: since when a look has been waiting.
    look_waiting: Option<Instant>,
    /// Text chosen with the mouse, and when some was last copied.
    select: Selection,
    copied: Option<Instant>,
    /// A drag on a page that began on its text chooses text instead of moving the page.
    choosing: bool,
}

struct Shared {
    pal: Palette,
    name: String,
    kind: Kind,
    ext: String,
    is_dir: bool,
    folder: Color32,
    size: String,
    place: (usize, usize),
    mebibytes: bool,
    content: PreviewContent,
    /// The pages of the shown document around `content` that have been loaded.
    strip: Vec<PreviewContent>,
    previous: Option<PreviewContent>,
    switched: Instant,
    loading: Option<Instant>,
    commands: Vec<Cmd>,
    view: View,
    detail: Option<(egui::TextureHandle, [u16; 4])>,
    /// Where looks at a 3D model are asked for and collected, and the file they belong to.
    lane: Option<Arc<Slot>>,
    model: Option<Key>,
    /// What the music is doing, straight from the player.
    sound: Arc<Mutex<sound::Status>>,
}

pub struct Quick {
    shared: Arc<Mutex<Shared>>,
    opened: Instant,
    /// When the size last changed. A compositor only follows a change while the limits hold it.
    sized: Instant,
    size: Vec2,
    /// Whether `size` already suits the first content. It is fitted once, so that
    /// moving to the next file does not make the window jump.
    fitted: bool,
    limit: Vec2,
    position: Option<Pos2>,
    shown: Option<Key>,
    revision: u32,
    path: Option<std::path::PathBuf>,
    /// What the strip of pages was last gathered for: the page asked for, the state of
    /// the cache and whether the file has pages.
    strip_for: Option<(Option<Key>, u64, bool)>,
    /// Whether the window has been asked for. Windows shows a new window before it moves it
    /// into place, so there it is made hidden and shown one frame later.
    made: bool,
}

/// About 70% × 80% of the screen the application is on.
fn placement(ctx: &egui::Context) -> (Vec2, Vec2, Option<Pos2>) {
    #[cfg(windows)]
    {
        let ppp = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
        let middle = ctx.input(|i| i.viewport().outer_rect).map(|r| r.center());
        if let Some(area) = middle.and_then(|m| crate::win::work_area_at(m.x * ppp, m.y * ppp)) {
            let screen = Rect::from_min_max(pos2(area[0] / ppp, area[1] / ppp), pos2(area[2] / ppp, area[3] / ppp));
            let size = vec2(screen.width() * 0.7, screen.height() * 0.8);
            return (size, screen.size(), Some(screen.center() - size / 2.0));
        }
    }
    let screen = ctx.input(|i| i.viewport().monitor_size).filter(|s| s.x > 0.0 && s.y > 0.0).unwrap_or_else(|| ctx.content_rect().size());
    (vec2(screen.x * 0.7, screen.y * 0.8), screen, None)
}

/// A picture at its own size, one screen pixel per pixel, as `image_rect` draws it.
fn picture_body(orig: [u32; 2], ppp: f32) -> Vec2 {
    vec2(orig[0] as f32, orig[1] as f32) / ppp.max(0.1) + Vec2::splat(24.0)
}

/// A page has no size of its own on the screen: as large as fits, in its proportions.
fn page_body(orig: [u32; 2], largest: Vec2) -> Vec2 {
    let size = vec2(orig[0].max(1) as f32, orig[1].max(1) as f32);
    let room = largest - vec2(24.0, BAR_H + 24.0);
    size * (room.x / size.x).min(room.y / size.y) + Vec2::splat(24.0)
}

/// The window that suits `content`, no larger than `largest`.
fn fitting_size(ctx: &egui::Context, content: &PreviewContent, largest: Vec2, music: bool) -> Vec2 {
    const SCROLLBAR: f32 = 10.0;
    let mono = FontId::monospace(13.0);
    let (char_w, line_h) = ctx.fonts_mut(|f| (f.glyph_width(&mono, '0'), f.row_height(&mono)));
    let digit = ctx.fonts_mut(|f| f.glyph_width(&FontId::proportional(13.0), '0'));
    let gap = ctx.global_style().spacing.item_spacing.y;
    let (text_step, list_step) = ((line_h + 3.0).round() + gap, 24.0 + gap);
    let body = match content {
        // Drawn larger than its nominal size means vector art, which has no size of its own.
        PreviewContent::Image(tex, orig, _) if tex.size()[0] > orig[0] as usize => page_body(*orig, largest),
        PreviewContent::Model(_, orig) => page_body(*orig, largest),
        PreviewContent::Image(_, orig, _) | PreviewContent::Animation(_, orig) => picture_body(*orig, ctx.pixels_per_point()),
        PreviewContent::Page { orig, .. } => page_body(*orig, largest),
        PreviewContent::Text(doc) if !doc.text.is_empty() => {
            let digits = doc.lines.len().max(1).ilog10() as f32 + 1.0;
            let width = (digits + doc.longest.min(100_000) as f32) * char_w + 42.0 + SCROLLBAR;
            vec2(width, (doc.lines.len() + doc.trimmed as usize) as f32 * text_step + SCROLLBAR)
        }
        PreviewContent::Hex(bytes, total) => {
            let rows = bytes.len().div_ceil(16) + (*total as usize > bytes.len()) as usize;
            vec2(HEX_ASCII_X + 16.0 * char_w + 16.0 + SCROLLBAR, rows as f32 * text_step + SCROLLBAR)
        }
        PreviewContent::Table(table) => {
            let columns: f32 = table.widths.iter().map(|w| *w as f32 * digit * 1.15 + 20.0).sum();
            let numbers = table.rows.len().max(1).ilog10() as f32 * digit + digit + 24.0;
            vec2(numbers + columns + 16.0 + SCROLLBAR, (table.rows.len() + table.trimmed as usize) as f32 * list_step + SCROLLBAR)
        }
        PreviewContent::Archive(entries, ..) => vec2(READING_WIDTH, (entries.len() + 1) as f32 * list_step + SCROLLBAR),
        PreviewContent::Info(rows, cover, _) => {
            let picture = cover.as_ref().map_or(Vec2::splat(96.0), |(_, size)| picture_body(*size, ctx.pixels_per_point()) - Vec2::splat(24.0));
            vec2(picture.x.max(504.0) + 56.0, picture.y + INFO_PICTURE_GAP + rows.len() as f32 * INFO_ROW_H + INFO_PADDING * 2.0 + info_footer_height(music))
        }
        // Laid out only once shown, so its length is not known yet: a page to read.
        PreviewContent::Doc(_) => vec2(READING_WIDTH + 64.0, largest.y),
        _ => Vec2::ZERO,
    };
    (body + vec2(0.0, BAR_H)).max(MIN_OPEN).min(largest)
}

impl App {
    /// False for a moment after the preview window closed itself.
    pub(crate) fn preview_may_toggle(&self) -> bool {
        self.preview_closed.is_none_or(|at| at.elapsed() > REOPEN_PAUSE)
    }

    fn preview_commands(&mut self, ctx: &egui::Context) {
        let Some(quick) = &self.quick else { return };
        let commands = std::mem::take(&mut quick.shared.lock().unwrap().commands);
        let i = self.active;
        for command in commands {
            match command {
                Cmd::Close => {
                    self.preview_window = false;
                    self.preview_closed = Some(Instant::now());
                    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
                }
                Cmd::Page(by) => {
                    // While the next page loads, the one still on screen knows the count.
                    let known = pages_of(&self.preview.content).or_else(|| pages_of(&self.quick.as_ref()?.shared.lock().unwrap().content));
                    if let Some((_, pages)) = known {
                        self.preview.page = self.preview.page.saturating_add_signed(by).min(pages.saturating_sub(1));
                    }
                }
                Cmd::ShowPage(page) => {
                    let known = pages_of(&self.preview.content).or_else(|| pages_of(&self.quick.as_ref()?.shared.lock().unwrap().content));
                    if let Some((_, pages)) = known { self.preview.page = page.min(pages.saturating_sub(1)); }
                }
                Cmd::Detail(crop) => self.want_detail(crop, ctx),
                Cmd::Turn(view) => self.want_model_view(view, ctx),
                Cmd::Step(by) => {
                    let step = if by > 0 { crate::keymap::Command::CursorDown } else { crate::keymap::Command::CursorUp };
                    self.run_command(i, step, false, None, false, 1);
                }
                Cmd::Play | Cmd::Seek(_) => {
                    let Some(path) = self.preview.path.clone() else { continue };
                    let ask = if let Cmd::Seek(ms) = command { sound::Ask::Seek(Duration::from_millis(ms)) } else { sound::Ask::Toggle };
                    self.sound_ask(&path, ask, ctx);
                }
                Cmd::Open => {
                    if let Some(e) = self.cursor_entry(i) {
                        self.acts.push(Act::Open(i, e.path.clone()));
                    }
                    self.preview_window = false;
                    self.preview_closed = Some(Instant::now());
                }
            }
        }
    }

    pub fn preview_window_ui(&mut self, ctx: &egui::Context) {
        self.preview_commands(ctx);
        let i = self.active;
        let focused = self.preview_window.then(|| self.focused_item(i)).flatten();
        let Some(e) = focused else {
            self.preview_window = false;
            self.quick = None;
            return;
        };
        self.want_preview(Some(Key::of(&e)), ctx);
        self.sound_preview(&e, ctx);
        let mut neighbours: Vec<Key> = self.panes[i].cursor.map_or_else(Vec::new, |row| {
            [row.wrapping_sub(1), row + 1].into_iter().filter_map(|r| self.entry_at(i, r)).filter(|n| !n.is_dir).map(Key::of).collect()
        });
        if let PreviewContent::Page { page, pages, .. } = self.preview.content {
            // Most often read onwards, so the next pages come first.
            let around = [(page + 1 < pages).then_some(page + 1), page.checked_sub(1), (page + 2 < pages).then_some(page + 2)];
            neighbours.splice(0..0, around.into_iter().flatten().filter_map(|n| self.preview.page_key(n)));
        }
        self.prefetch_preview(neighbours, ctx);
        self.want_film(ctx);
        self.hand_over(ctx, &e, i);
        let quick = self.quick.as_mut().unwrap();
        let waiting = matches!(self.preview.content, PreviewContent::Loading);
        if waiting && quick.opened.elapsed() < OPEN_WAIT {
            ctx.request_repaint_after(OPEN_WAIT);
            return;
        }
        if !quick.fitted && !matches!(self.preview.content, PreviewContent::Loading | PreviewContent::None) {
            quick.fitted = true;
            quick.sized = Instant::now();
            let size = fitting_size(ctx, &self.preview.content, quick.size, sound::playable(e.kind, e.ext()));
            if let Some(position) = &mut quick.position { *position += (quick.size - size) / 2.0; }
            quick.size = size;
        }
        let locked = quick.sized.elapsed() < SIZE_LOCK;
        if locked { ctx.request_repaint_after(SIZE_LOCK); }
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{} — Preview", e.name()))
            .with_app_id("ferrous-commander-preview")
            .with_decorations(false)
            .with_inner_size(quick.size)
            // A window of one fixed size is what makes a tiling compositor float it.
            .with_min_inner_size(if locked { quick.size } else { vec2(320.0, 240.0) })
            .with_max_inner_size(if locked { quick.size } else { quick.limit });
        if let Some(position) = quick.position { builder = builder.with_position(position); }
        if cfg!(windows) {
            builder = builder.with_visible(quick.made);
            if !quick.made { ctx.request_repaint(); }
        }
        quick.made = true;
        let shared = quick.shared.clone();
        ctx.show_viewport_deferred(viewport(), builder, move |ui, class| {
            draw(ui, &mut shared.lock().unwrap(), class);
        });
    }

    fn hand_over(&mut self, ctx: &egui::Context, e: &Entry, i: usize) {
        let folder = self.folder_color(&e.path);
        let pal = self.pal;
        let size = if e.is_dir { String::new() } else { fsx::fmt_size(e.size, self.st.mebibytes) };
        let place = (self.panes[i].cursor.map_or(0, |c| c + 1), self.panes[i].view.len());
        let sound = self.sound.shared();
        let quick = self.quick.get_or_insert_with(|| {
            let (size, limit, position) = placement(ctx);
            Quick {
                shared: Arc::new(Mutex::new(Shared {
                    pal,
                    name: String::new(),
                    kind: e.kind,
                    ext: String::new(),
                    is_dir: e.is_dir,
                    folder,
                    size: String::new(),
                    place,
                    mebibytes: false,
                    content: PreviewContent::Loading,
                    strip: Vec::new(),
                    previous: None,
                    switched: Instant::now(),
                    loading: None,
                    commands: Vec::new(),
                    view: View { zoom: 1.0, ..Default::default() },
                    detail: None,
                    lane: None,
                    model: None,
                    sound,
                })),
                opened: Instant::now(),
                sized: Instant::now(),
                size,
                fitted: false,
                limit,
                position,
                shown: None,
                revision: 0,
                path: None,
                strip_for: None,
                made: false,
            }
        });
        let mut s = quick.shared.lock().unwrap();
        let mut changed = false;
        if quick.path.as_ref() != Some(&e.path) {
            quick.path = Some(e.path.clone());
            s.name = e.name().to_string();
            s.kind = e.kind;
            s.ext = e.ext().to_string();
            s.is_dir = e.is_dir;
            s.folder = folder;
            s.size = size;
            changed = true;
        }
        if s.place != place { s.place = place; changed = true; }
        s.pal = pal;
        s.mebibytes = self.st.mebibytes;
        let detail = self.preview.detail.clone();
        if s.detail.as_ref().map(|d| d.0.id()) != detail.as_ref().map(|d| d.0.id()) {
            s.detail = detail;
            changed = true;
        }
        // The pages around the one asked for, as far as they have loaded.
        // Looked for again only once another page is asked for or more have loaded.
        let paged = matches!(s.content, PreviewContent::Page { .. }) || matches!(self.preview.content, PreviewContent::Page { .. });
        let generation = self.preview.cache_generation();
        if quick.strip_for.as_ref().map(|(key, g, p)| (key.as_ref(), *g, *p)) != Some((self.preview.key(), generation, paged)) {
            quick.strip_for = Some((self.preview.key().cloned(), generation, paged));
            let wanted = self.preview.page;
            let strip: Vec<PreviewContent> = if paged {
                (wanted.saturating_sub(STRIP_AROUND)..=wanted.saturating_add(STRIP_AROUND)).filter_map(|n| self.preview.cached_page(n)).collect()
            } else {
                Vec::new()
            };
            let ids = |pages: &[PreviewContent]| pages.iter().filter_map(|c| if let PreviewContent::Page { page, texture, .. } = c { Some((*page, texture.id())) } else { None }).collect::<Vec<_>>();
            if ids(&strip) != ids(&s.strip) {
                s.strip = strip;
                changed = true;
            }
        }
        let same_file = quick.shown.as_ref().map(|k| &k.path) == self.preview.key().map(|k| &k.path);
        // Another page of the document on show: the strip has a place for it while it loads.
        let turning = same_file && matches!(s.content, PreviewContent::Page { .. });
        let waiting = matches!(self.preview.content, PreviewContent::Loading);
        if waiting && turning {
            s.loading = None;
        } else if waiting {
            if s.loading.is_none() {
                s.loading = Some(Instant::now());
                ctx.request_repaint_of(viewport());
            }
        } else if quick.shown.as_ref() == self.preview.key() {
            // Back on the file still on screen, from one that never finished loading.
            if s.loading.take().is_some() { changed = true; }
            if quick.revision != self.preview.revision {
                quick.revision = self.preview.revision;
                s.content = self.preview.content.clone();
                s.view.started = None;
                s.view.select = Selection::None;
                changed = true;
            }
        } else if turning && matches!(self.preview.content, PreviewContent::Page { .. }) {
            // Scrolled or turned to another page: the view stays where it is.
            quick.revision = self.preview.revision;
            quick.shown = self.preview.key().cloned();
            s.content = self.preview.content.clone();
            s.loading = None;
            s.view.sent = None;
            s.view.moved = Some(Instant::now());
            changed = true;
        } else {
            quick.revision = self.preview.revision;
            quick.shown = self.preview.key().cloned();
            let old = std::mem::replace(&mut s.content, self.preview.content.clone());
            s.previous = Some(old);
            s.switched = Instant::now();
            s.loading = None;
            s.view = View { zoom: 1.0, wrap: s.view.wrap, source: s.view.source, ..Default::default() };
            changed = true;
        }
        s.lane = self.preview.look_lane();
        s.model = quick.shown.clone();
        if changed { ctx.request_repaint_of(viewport()); }
    }
}

impl Shared {
    fn send(&mut self, command: Cmd, ctx: &egui::Context) {
        self.commands.push(command);
        ctx.request_repaint_of(ViewportId::ROOT);
    }

    /// Straight to the lane: going through the file list would redraw it for every look.
    fn ask_look(&mut self, view: ModelView, ctx: &egui::Context) {
        let (Some(lane), Some(model)) = (&self.lane, &self.model) else {
            self.send(Cmd::Turn(view), ctx);
            return;
        };
        let key = Key { view: Some(view), ..model.clone() };
        lane.ask_look(Job { key, ctx: ctx.clone(), prefetch: false });
    }

    /// The music of the shown file, while it can be paused or moved in.
    fn music(&self) -> Option<sound::Status> {
        let status = self.sound.lock().unwrap().clone();
        let mine = status.state != sound::State::Idle && status.track.as_ref() == self.model.as_ref().map(|key| &key.path);
        mine.then_some(status)
    }

    /// Takes the newest look at the shown model, if one has arrived.
    fn collect_look(&mut self) {
        if self.view.sent_view.is_none() { return; }
        let (Some(lane), Some(model)) = (&self.lane, &self.model) else { return };
        let Some((key, texture)) = lane.look.lock().unwrap().take() else { return };
        if (Key { view: None, ..key }) != *model { return; }
        self.view.look = Some(texture);
        if let Some(waiting) = self.view.look_waiting.take() && super::trace() {
            eprintln!("[look] shown after {} us", waiting.elapsed().as_micros());
        }
    }
}

fn keys(ctx: &egui::Context, s: &mut Shared, page: f32) {
    if ctx.input(|i| i.viewport().close_requested()) {
        // The list decides, so that it does not recreate a window the system just closed.
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        s.send(Cmd::Close, ctx);
    }
    let events = ctx.input(|i| i.events.clone());
    for event in events {
        if matches!(event, egui::Event::Copy) {
            copy_selection(s, ctx);
            continue;
        }
        let egui::Event::Key { key, pressed: true, repeat, modifiers, .. } = event else { continue };
        if key == K::A && modifiers.command && !modifiers.alt {
            s.view.select = select_all(s);
            continue;
        }
        if modifiers.ctrl || modifiers.alt || modifiers.command { continue; }
        let paged = matches!(s.content, PreviewContent::Page { .. });
        let music = s.music();
        match key {
            K::P if !repeat && sound::playable(s.kind, &s.ext) => s.send(Cmd::Play, ctx),
            K::ArrowRight | K::ArrowLeft if music.is_some() => {
                let Some(status) = music else { continue };
                let to = if key == K::ArrowRight { status.position() + sound::STEP } else { status.position().saturating_sub(sound::STEP) };
                s.send(Cmd::Seek(to.as_millis() as u64), ctx);
            }
            K::PageDown | K::ArrowRight if paged => turn_page(&mut s.view, 1),
            K::PageUp | K::ArrowLeft if paged => turn_page(&mut s.view, -1),
            K::Home if paged => s.view.at = 0.0,
            K::End if paged => s.view.at = f32::MAX,
            K::Escape if !repeat && !s.view.select.is_empty() => s.view.select = Selection::None,
            K::Space | K::Escape if !repeat => s.send(Cmd::Close, ctx),
            K::Enter if !repeat => s.send(Cmd::Open, ctx),
            K::ArrowDown => s.send(Cmd::Step(1), ctx),
            K::ArrowUp => s.send(Cmd::Step(-1), ctx),
            K::ArrowRight => s.send(Cmd::Page(1), ctx),
            K::ArrowLeft => s.send(Cmd::Page(-1), ctx),
            K::PageDown => s.view.scroll_to = Some(s.view.offset + page),
            K::PageUp => s.view.scroll_to = Some((s.view.offset - page).max(0.0)),
            K::Home => s.view.scroll_to = Some(0.0),
            K::End => s.view.scroll_to = Some(SCROLL_TO_END),
            K::Plus | K::Equals => zoom_by(&mut s.view, 1.25),
            K::Minus => zoom_by(&mut s.view, 0.8),
            K::Num0 => reset_look(&mut s.view),
            K::W if !repeat => { s.view.wrap = !s.view.wrap; s.view.rows_for = (0, 0); }
            K::M if !repeat => { s.view.source = !s.view.source; s.view.rows_for = (0, 0); s.view.select = Selection::None; }
            _ => {}
        }
    }
}

/// What is chosen in the shown content, as it goes to the clipboard.
fn selected_text(s: &Shared) -> Option<String> {
    match (&s.content, s.view.select) {
        (PreviewContent::Text(doc), Selection::Text { anchor, head }) => select::text_between(&doc.text, anchor, head),
        (PreviewContent::Doc(doc), Selection::Text { anchor, head }) => select::text_between(&doc.source.as_ref()?.text, anchor, head),
        (PreviewContent::Doc(doc), Selection::Doc { anchor, head }) => select::blocks_between(&doc.blocks, anchor, head),
        (PreviewContent::Table(table), Selection::Cells { anchor, head }) => select::cells_between(&table.rows, anchor, head),
        (PreviewContent::Page { .. }, Selection::Page { page, anchor, head }) => select::page_between(loaded_page(s, page)?.2?, anchor, head),
        _ => None,
    }
}

fn select_all(s: &Shared) -> Selection {
    match &s.content {
        PreviewContent::Text(doc) => Selection::Text { anchor: 0, head: doc.text.len() as u32 },
        PreviewContent::Doc(doc) => match (&doc.source, s.view.source) {
            (Some(source), true) => Selection::Text { anchor: 0, head: source.text.len() as u32 },
            _ => match doc.blocks.last() {
                Some(last) => Selection::Doc { anchor: (0, 0), head: (doc.blocks.len() as u32 - 1, last.text.len() as u32) },
                None => Selection::None,
            },
        },
        PreviewContent::Table(table) if !table.rows.is_empty() => {
            let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
            Selection::Cells { anchor: (0, 0), head: (table.rows.len() as u32 - 1, columns as u32 - 1) }
        }
        PreviewContent::Page { .. } => match loaded_page(s, s.view.on_page).and_then(|p| p.2) {
            Some(text) => Selection::Page { page: s.view.on_page, anchor: 0, head: text.chars.len() as u32 },
            None => Selection::None,
        },
        _ => Selection::None,
    }
}

fn copy_selection(s: &mut Shared, ctx: &egui::Context) {
    let Some(text) = selected_text(s) else { return };
    ctx.copy_text(text);
    s.view.copied = Some(Instant::now());
    ctx.request_repaint_after(COPIED_FOR);
}

/// Copy and Select all on the right button, for the views with text.
fn copy_menu(response: &egui::Response, s: &mut Shared) {
    response.context_menu(|ui| {
        if ui.add_enabled(!s.view.select.is_empty(), egui::Button::new("Copy")).clicked() {
            copy_selection(s, &ui.ctx().clone());
            ui.close();
        }
        if ui.button("Select all").clicked() {
            s.view.select = select_all(s);
            ui.close();
        }
    });
}

/// The colour behind chosen text.
fn chosen(pal: Palette) -> Color32 {
    let [r, g, b, _] = pal.accent.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, 90)
}

/// While text is chosen past an edge of a view, the view follows the pointer.
fn follow_pointer(ui: &Ui, s: &mut Shared, dragging: bool, area: Rect, offset: f32) {
    if !dragging { return; }
    let Some(y) = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y) else { return };
    let by = if y < area.top() { (y - area.top()).max(-60.0) } else if y > area.bottom() { (y - area.bottom()).min(60.0) } else { return };
    s.view.scroll_to = Some((offset + by / 2.0).max(0.0));
    ui.ctx().request_repaint();
}

/// The shown page and the page count of content that has pages or sheets.
fn pages_of(content: &PreviewContent) -> Option<(u32, u32)> {
    match content {
        PreviewContent::Page { page, pages, .. } => Some((*page, *pages)),
        PreviewContent::Table(table) if table.sheets.len() > 1 => Some((table.page, table.sheets.len() as u32)),
        _ => None,
    }
}

/// To the top of the next page of a document, or back to the top of this one and then
/// to the one before.
fn turn_page(view: &mut View, by: i32) {
    let here = view.at.floor();
    let by = if by < 0 && view.at - here > 0.01 { 0.0 } else { by as f32 };
    view.at = (here + by).max(0.0);
}

/// A loaded page's picture, size and text.
type LoadedPage<'a> = (&'a egui::TextureHandle, [u32; 2], Option<&'a Arc<PageText>>);

/// A loaded page of the shown document, the one on show or one around it.
fn loaded_page(s: &Shared, n: u32) -> Option<LoadedPage<'_>> {
    std::iter::once(&s.content).chain(&s.strip).find_map(|content| match content {
        PreviewContent::Page { texture, orig, page, text, .. } if *page == n => Some((texture, *orig, text.as_ref())),
        _ => None,
    })
}

fn zoom_by(view: &mut View, factor: f32) {
    view.zoom = (view.zoom * factor).clamp(if view.model { MIN_MODEL_ZOOM } else { 1.0 }, MAX_ZOOM);
    if view.zoom <= 1.0 && !view.model { view.pan = Vec2::ZERO; }
}

fn reset_look(view: &mut View) {
    view.zoom = 1.0;
    view.pan = Vec2::ZERO;
    view.turn = 0.0;
    view.tilt = 0.0;
}

/// `egui::Spinner` asks for the next frame at once. On Windows a window that keeps doing
/// that can leave the file list without frames, and the finished preview and the key that
/// closes this window both go through the list. So this one leaves a pause.
fn paced_spinner(ui: &Ui, r: Rect) {
    use std::f64::consts::TAU;
    ui.ctx().request_repaint_after(SPINNER_FRAME);
    let radius = r.height() * 0.5 - 2.0;
    let time = ui.input(|i| i.time);
    let (start, sweep) = (time * TAU, 240f64.to_radians() * time.sin());
    let points = (0..radius.round() as u32).map(|i| {
        let (sin, cos) = (start + sweep * i as f64 / radius.round() as f64).sin_cos();
        r.center() + radius * vec2(cos as f32, sin as f32)
    }).collect();
    ui.painter().add(egui::Shape::line(points, Stroke::new(3.0, ui.visuals().strong_text_color())));
}

fn draw(ui: &mut Ui, s: &mut Shared, class: ViewportClass) {
    let ctx = ui.ctx().clone();
    let embedded = class == ViewportClass::EmbeddedWindow;
    if embedded { ui.set_min_size(vec2(640.0, 420.0)); }
    let full = if embedded { ui.available_rect_before_wrap() } else { ctx.content_rect() };
    if !embedded { keys(&ctx, s, full.height() - BAR_H * 2.0); }
    let pal = s.pal;
    ui.painter().rect_filled(full, CornerRadius::ZERO, pal.panel);
    let (top, body) = (Rect::from_min_size(full.min, vec2(full.width(), BAR_H)), Rect::from_min_max(pos2(full.min.x, full.min.y + BAR_H), full.max));
    title_bar(ui, s, top, embedded);
    ui.painter().rect_filled(body, CornerRadius::ZERO, pal.content_fill());
    let t = (s.switched.elapsed().as_secs_f32() / SWITCH_SECONDS).min(1.0);
    if t < 1.0 { ctx.request_repaint(); } else { s.previous = None; }
    if let Some(PreviewContent::Image(tex, orig, _) | PreviewContent::Model(tex, orig) | PreviewContent::Page { texture: tex, orig, .. }) = &s.previous {
        let paged = matches!(s.previous, Some(PreviewContent::Page { .. }));
        let r = fitted_rect(body, tex, *orig, 1.0, paged, ctx.pixels_per_point());
        ui.painter_at(body).image(tex.id(), r, unit_uv(), Color32::WHITE.gamma_multiply(1.0 - t));
    }
    // Keep music controls above the navigation overlay even in a short window.
    let music = sound::playable(s.kind, &s.ext).then(|| music_strip(body));
    let footer = if music.is_some() || matches!(s.content, PreviewContent::Info(..)) { info_footer_height(music.is_some()) } else { 0.0 };
    let content_body = Rect::from_min_max(body.min, pos2(body.max.x, body.max.y - footer));
    let mut child = ui.new_child(UiBuilder::new().max_rect(content_body));
    child.set_clip_rect(content_body);
    child.multiply_opacity(t);
    let content = s.content.clone();
    match &content {
        PreviewContent::Text(doc) if doc.text.is_empty() => card(&child, s, content_body, "This file is empty"),
        PreviewContent::Text(doc) => text_view(&mut child, s, doc, content_body),
        PreviewContent::Model(tex, orig) => model_view(&mut child, s, tex, *orig, content_body),
        PreviewContent::Image(tex, orig, alpha) => {
            let sharpen = tex.size()[0] < orig[0] as usize;
            image_view(&mut child, s, tex, *orig, *alpha, content_body, sharpen);
        }
        PreviewContent::Page { orig, pages, .. } => pages_view(&mut child, s, content_body, *pages, *orig),
        PreviewContent::Animation(frames, orig) => {
            let tex = playing_frame(s, &ctx, frames);
            image_view(&mut child, s, tex, *orig, true, content_body, false);
        }
        PreviewContent::Doc(doc) => match (&doc.source, s.view.source) {
            (Some(source), true) => text_view(&mut child, s, source, content_body),
            _ => doc_view(&mut child, s, doc, content_body),
        },
        PreviewContent::Hex(bytes, total) => hex_view(&mut child, s, bytes, *total, content_body),
        PreviewContent::Table(table) => table_view(&mut child, s, table, content_body),
        PreviewContent::Archive(entries, total, trimmed) => archive_view(&mut child, s, entries, *total, *trimmed, content_body),
        PreviewContent::Info(rows, cover, _) => {
            let picture = cover.as_ref().map(|(frames, size)| (playing_frame(s, &ctx, frames), *size));
            info_view(&mut child, s, rows, picture, content_body);
        }
        PreviewContent::Folder { folders, files, size } => {
            let text = format!("{folders} folders, {files} files\n{}", fsx::fmt_size(*size, s.mebibytes));
            card(&child, s, content_body, &text);
        }
        PreviewContent::Error(message) => card(&child, s, content_body, message),
        PreviewContent::Detail(_) | PreviewContent::None | PreviewContent::Loading => {}
    }
    if let Some(since) = s.loading {
        if since.elapsed() >= SPINNER_AFTER {
            paced_spinner(ui, Rect::from_center_size(body.center(), Vec2::splat(28.0)));
        } else {
            ctx.request_repaint_after(SPINNER_AFTER);
        }
    }
    if let Some(strip) = music {
        match sound::transport(ui, strip, pal, s.music().as_ref(), egui::Id::unique("preview_sound")) {
            Some(sound::Ask::Toggle) => s.send(Cmd::Play, &ctx),
            Some(sound::Ask::Seek(to)) => s.send(Cmd::Seek(to.as_millis() as u64), &ctx),
            None => {}
        }
    }
    let zoomable = matches!(content, PreviewContent::Image(..) | PreviewContent::Model(..) | PreviewContent::Page { .. } | PreviewContent::Animation(..));
    // A document shows the page in view, which may still be loading.
    let pages = match &content {
        PreviewContent::Page { pages, .. } => Some((s.view.on_page, *pages)),
        _ => pages_of(&content),
    };
    control_bar(ui, s, body, &ctx, zoomable, pages);
    if cfg!(windows) { ui.painter().rect_stroke(full, CornerRadius::ZERO, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside); }
}

fn title_bar(ui: &mut Ui, s: &mut Shared, r: Rect, embedded: bool) {
    let pal = s.pal;
    let response = ui.interact(r, ui.scope_id().with("preview_title"), Sense::click_and_drag());
    if response.drag_started() && !embedded { ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag); }
    let icon = if s.is_dir { Icon::Folder(s.folder) } else { Icon::Entry(s.kind, &s.ext) };
    let mut info = if s.size.is_empty() { format!("{} / {}", s.place.0, s.place.1) } else { format!("{}   ·   {} / {}", s.size, s.place.0, s.place.1) };
    if s.view.copied.is_some_and(|at| at.elapsed() < COPIED_FOR) { info = format!("Copied   ·   {info}"); }
    if chrome::title_bar(ui, pal, r, icon, &s.name, &info) { s.send(Cmd::Close, ui.ctx()); }
}

fn unit_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

/// Fits inside the frame; a small picture stays at its own size instead of being blown up.
fn image_rect(frame: Rect, orig: [u32; 2], zoom: f32, ppp: f32) -> Rect {
    let size = vec2(orig[0].max(1) as f32, orig[1].max(1) as f32);
    let area = frame.shrink(12.0);
    let fit = (area.width() / size.x).min(area.height() / size.y).min(1.0 / ppp.max(0.1));
    Rect::from_center_size(frame.center(), size * fit * zoom)
}

fn checkerboard(ui: &Ui, r: Rect, clip: Rect, pal: Palette) {
    const SQUARE: f32 = 12.0;
    let (a, b) = (blend(pal.panel, pal.text_weak, 0.18), blend(pal.panel, pal.text_weak, 0.32));
    let visible = r.intersect(clip);
    let p = ui.painter_at(visible);
    p.rect_filled(visible, CornerRadius::ZERO, a);
    let first = ((visible.min - r.min) / SQUARE).floor();
    let (mut x, cols, rows) = (first.x, (visible.width() / SQUARE).ceil() + 2.0, (visible.height() / SQUARE).ceil() + 2.0);
    while x < first.x + cols {
        let mut y = first.y;
        while y < first.y + rows {
            if (x + y) as i64 % 2 == 0 {
                p.rect_filled(Rect::from_min_size(r.min + vec2(x, y) * SQUARE, Vec2::splat(SQUARE)), CornerRadius::ZERO, b);
            }
            y += 1.0;
        }
        x += 1.0;
    }
}

/// The frame to show now, with a repaint booked for when it ends.
fn playing_frame<'a>(s: &mut Shared, ctx: &egui::Context, frames: &'a [(egui::TextureHandle, u32)]) -> &'a egui::TextureHandle {
    if frames.len() < 2 { return &frames[0].0; }
    let total = frames.iter().map(|f| f.1).sum::<u32>().max(1);
    let started = *s.view.started.get_or_insert_with(Instant::now);
    let mut at = (started.elapsed().as_millis() % total as u128) as u32;
    for (texture, ms) in frames {
        if at < *ms {
            ctx.request_repaint_after(Duration::from_millis((*ms - at) as u64));
            return texture;
        }
        at -= ms;
    }
    ctx.request_repaint_after(Duration::from_millis(20));
    &frames[0].0
}

/// Normalised part of `r` that lies inside the frame.
fn visible_part(r: Rect, frame: Rect) -> [u16; 4] {
    let seen = r.intersect(frame);
    let along = |v: f32| (v.clamp(0.0, 1.0) * 65535.0) as u16;
    [along((seen.min.x - r.min.x) / r.width()), along((seen.min.y - r.min.y) / r.height()), along((seen.max.x - r.min.x) / r.width()), along((seen.max.y - r.min.y) / r.height())]
}

/// A 3D model: the pointer changes the look, and the helper draws each new one.
fn model_view(ui: &mut Ui, s: &mut Shared, tex: &egui::TextureHandle, orig: [u32; 2], body: Rect) {
    s.view.model = true;
    let ctx = ui.ctx().clone();
    let response = ui.interact(body, ui.scope_id().with("preview_model"), Sense::click_and_drag());
    let (wheel, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
    if response.contains_pointer() && (wheel != 0.0 || pinch != 1.0) {
        s.view.zoom = (s.view.zoom * pinch * (wheel * 0.002).exp()).clamp(MIN_MODEL_ZOOM, MAX_ZOOM);
    }
    let delta = response.drag_delta();
    if response.dragged_by(egui::PointerButton::Primary) {
        s.view.turn = (s.view.turn + delta.x * TURN_PER_POINT).rem_euclid(360.0);
        s.view.tilt = (s.view.tilt + delta.y * TURN_PER_POINT).clamp(TILT_OFFSETS.0, TILT_OFFSETS.1);
    } else if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
        s.view.pan += delta;
    }
    if response.double_clicked() { reset_look(&mut s.view); }
    let cursor = if response.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab };
    if response.hovered() || response.dragged() { ctx.set_cursor_icon(cursor); }

    let ppp = ctx.pixels_per_point();
    let first = page_rect(body, orig, 1.0);
    let resting = s.view.turn == 0.0 && s.view.tilt == 0.0 && s.view.zoom == 1.0 && s.view.pan == Vec2::ZERO;
    let look = ModelView {
        turn: (s.view.turn * 10.0) as i16,
        tilt: (s.view.tilt * 10.0) as i16,
        zoom: (s.view.zoom * 100.0) as u16,
        pan: [(s.view.pan.x * ppp).clamp(-32000.0, 32000.0) as i16, (s.view.pan.y * ppp).clamp(-32000.0, 32000.0) as i16],
        size: [(body.width() * ppp) as u16, (body.height() * ppp) as u16],
        fit: (first.width() * ppp) as u16,
        fine: false,
    };
    let sent = s.view.sent_view;
    if sent.map(|view| ModelView { fine: false, ..view }) != Some(look) {
        if sent.is_some() || !resting {
            s.ask_look(look, &ctx);
            s.view.look_waiting.get_or_insert_with(Instant::now);
            s.view.sent_view = Some(look);
            s.view.moved = Some(Instant::now());
            ctx.request_repaint_after(SHARPEN_AFTER);
        }
    } else if sent.is_some_and(|view| !view.fine) && let Some(moved) = s.view.moved {
        if moved.elapsed() < SHARPEN_AFTER {
            ctx.request_repaint_after(SHARPEN_AFTER);
        } else {
            let fine = ModelView { fine: true, ..look };
            s.ask_look(fine, &ctx);
            s.view.sent_view = Some(fine);
        }
    }

    s.collect_look();
    let p = ui.painter_at(body);
    if let Some(drawn) = &s.view.look {
        let size = vec2(drawn.size()[0] as f32, drawn.size()[1] as f32);
        let fit = (body.width() / size.x).min(body.height() / size.y);
        p.image(drawn.id(), Rect::from_center_size(body.center(), size * fit), unit_uv(), Color32::WHITE);
    } else {
        p.image(tex.id(), first, unit_uv(), Color32::WHITE);
    }
}

fn image_view(ui: &mut Ui, s: &mut Shared, tex: &egui::TextureHandle, orig: [u32; 2], alpha: bool, body: Rect, sharpen: bool) {
    let response = ui.interact(body, ui.scope_id().with("preview_image"), Sense::click_and_drag());
    let ppp = ui.ctx().pixels_per_point();
    let (wheel, pinch, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
    let before = s.view.zoom;
    if response.contains_pointer() && (wheel != 0.0 || pinch != 1.0) {
        s.view.zoom = (s.view.zoom * pinch * (wheel * 0.002).exp()).clamp(1.0, MAX_ZOOM);
        if let Some(at) = hover {
            let from_center = at - body.center() - s.view.pan;
            s.view.pan -= from_center * (s.view.zoom / before - 1.0);
        }
    }
    if response.dragged() { s.view.pan += response.drag_delta(); }
    if response.double_clicked() { s.view.zoom = 1.0; s.view.pan = Vec2::ZERO; }
    let fitted = fitted_rect(body, tex, orig, s.view.zoom, false, ppp);
    let spare = ((fitted.size() - body.size()) / 2.0).max(Vec2::ZERO);
    s.view.pan = s.view.pan.clamp(-spare, spare);
    let r = fitted.translate(s.view.pan);
    if alpha { checkerboard(ui, r, body, s.pal); }
    let p = ui.painter_at(body);
    p.image(tex.id(), r, unit_uv(), Color32::WHITE);
    paint_detail(&p, s, r);
    if sharpen { sharpen_when_still(ui, s, r, body, tex, 0.0); }
}

/// The sharper copy of part of the shown picture, over it while zoomed in.
fn paint_detail(p: &egui::Painter, s: &Shared, r: Rect) {
    if s.view.zoom > 1.0 && let Some((detail, [l, t, cr, cb])) = &s.detail {
        let at = |x: u16, y: u16| r.min + vec2(x as f32 / 65535.0 * r.width(), y as f32 / 65535.0 * r.height());
        p.image(detail.id(), Rect::from_min_max(at(*l, *t), at(*cr, *cb)), unit_uv(), Color32::WHITE);
    }
}

/// Once the picture at `r` has rested for a moment larger than its texture, asks for a
/// sharper copy of the part in view. A change of `scrolled` counts as moving it too.
fn sharpen_when_still(ui: &Ui, s: &mut Shared, r: Rect, body: Rect, tex: &egui::TextureHandle, scrolled: f32) {
    let look = (s.view.zoom, s.view.pan, scrolled);
    if s.view.last_look != look {
        s.view.last_look = look;
        s.view.moved = Some(Instant::now());
    }
    let coarse = r.width() * ui.ctx().pixels_per_point() > tex.size()[0] as f32 * 1.05;
    if coarse && r.intersects(body) && let Some(moved) = s.view.moved {
        let crop = visible_part(r, body);
        if moved.elapsed() < SHARPEN_AFTER {
            ui.ctx().request_repaint_after(SHARPEN_AFTER);
        } else if s.view.sent != Some(crop) {
            s.view.sent = Some(crop);
            s.send(Cmd::Detail(crop), &ui.ctx().clone());
        }
    }
}

/// A page of a document where it is drawn, with its picture and text once loaded.
struct Sheet {
    page: u32,
    rect: Rect,
    texture: Option<egui::TextureHandle>,
    text: Option<Arc<PageText>>,
}

/// The pages in view, one under another, scrolled to `s.view.at`.
fn sheets(s: &Shared, body: Rect, pages: u32, fallback: [u32; 2]) -> Vec<Sheet> {
    let first = (s.view.at.max(0.0) as u32).min(pages.saturating_sub(1));
    let mut y = body.top() + PAGE_GAP - (s.view.at - first as f32) * page_height(s, body, first, fallback);
    let mut out = Vec::new();
    for page in first..pages {
        if y > body.bottom() { break; }
        let loaded = loaded_page(s, page);
        let size = page_rect(body, loaded.as_ref().map_or(fallback, |l| l.1), s.view.zoom).size();
        let rect = Rect::from_min_size(pos2(body.center().x - size.x / 2.0 + s.view.pan.x, y), size);
        let (texture, text) = loaded.map_or((None, None), |(texture, _, text)| (Some(texture.clone()), text.cloned()));
        out.push(Sheet { page, rect, texture, text });
        y += size.y + PAGE_GAP;
    }
    out
}

/// The room page `n` takes in the strip, gap included.
fn page_height(s: &Shared, body: Rect, n: u32, fallback: [u32; 2]) -> f32 {
    page_rect(body, loaded_page(s, n).map_or(fallback, |l| l.1), s.view.zoom).height() + PAGE_GAP
}

/// The pages of a document one under another, scrolled through as one long strip.
/// The keys and the buttons still turn to the next page, by scrolling to it.
fn pages_view(ui: &mut Ui, s: &mut Shared, body: Rect, pages: u32, fallback: [u32; 2]) {
    let response = ui.interact(body, ui.scope_id().with("preview_pages"), Sense::click_and_drag());
    let PreviewContent::Page { page: current, .. } = s.content else { return };
    // Where the pages were drawn last, which is where the pointer found their text.
    let before = sheets(s, body, pages, fallback);
    let choosing = page_choosing(ui, s, &before, &response);
    let (wheel, pinch, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
    let height = |s: &Shared| page_height(s, body, s.view.at.max(0.0) as u32, fallback);
    let over = response.contains_pointer();
    if over && pinch != 1.0 {
        let old = height(s);
        let zoom = s.view.zoom;
        s.view.zoom = (zoom * pinch).clamp(1.0, MAX_ZOOM);
        if let Some(at) = hover {
            // The part of the page under the pointer stays under it.
            s.view.pan.x -= (at.x - body.center().x - s.view.pan.x) * (s.view.zoom / zoom - 1.0);
            let down = at.y - body.top();
            s.view.at += down / old - down / height(s);
        }
    } else if over && wheel != 0.0 {
        s.view.at -= wheel / height(s);
    }
    if response.dragged() && !choosing {
        let delta = response.drag_delta();
        s.view.pan.x += delta.x;
        s.view.at -= delta.y / height(s);
    }
    if response.double_clicked() && !choosing { s.view.zoom = 1.0; s.view.pan = Vec2::ZERO; }
    // Down to where the bottom of the last page shows.
    let last = pages.saturating_sub(1);
    let tail = page_height(s, body, last, fallback);
    let end = last as f32 + ((tail + PAGE_GAP - body.height()) / tail).max(0.0);
    s.view.at = s.view.at.clamp(0.0, end);
    let width = page_rect(body, fallback, s.view.zoom).width();
    let spare = ((width - body.width()) / 2.0).max(0.0);
    s.view.pan = vec2(s.view.pan.x.clamp(-spare, spare), 0.0);

    let now = sheets(s, body, pages, fallback);
    let p = ui.painter_at(body);
    let blank = blend(s.pal.panel, s.pal.text_weak, 0.12);
    for sheet in &now {
        match &sheet.texture {
            Some(texture) => { p.image(texture.id(), sheet.rect, unit_uv(), Color32::WHITE); }
            None => {
                p.rect_filled(sheet.rect, CornerRadius::ZERO, blank);
                paced_spinner(ui, Rect::from_center_size(sheet.rect.intersect(body).center(), Vec2::splat(28.0)));
            }
        }
        if let (Some(text), Selection::Page { page, anchor, head }) = (&sheet.text, s.view.select) && page == sheet.page {
            paint_page_choice(&p, text, sheet.rect, select::ordered(anchor, head), chosen(s.pal));
        }
    }
    // The page across the middle of the view is the one the window is on.
    let middle = body.center().y;
    let on = now.iter().find(|sheet| sheet.rect.bottom() + PAGE_GAP >= middle).or(now.last()).map_or(0, |sheet| sheet.page);
    s.view.on_page = on;
    if s.view.asked_page.unwrap_or(current) != on {
        s.view.asked_page = Some(on);
        s.send(Cmd::ShowPage(on), &ui.ctx().clone());
    }
    if let Some(sheet) = now.iter().find(|sheet| sheet.page == current) && let Some(texture) = &sheet.texture {
        paint_detail(&p, s, sheet.rect);
        sharpen_when_still(ui, s, sheet.rect, body, texture, s.view.at);
    }
    if now.iter().any(|sheet| sheet.text.is_some()) { copy_menu(&response, s); }
}

/// Where on the screen a character of a page is, for a page drawn at `r`.
fn char_rect(r: Rect, part: [f32; 4]) -> Rect {
    Rect::from_min_max(r.min + vec2(part[0] * r.width(), part[1] * r.height()), r.min + vec2(part[2] * r.width(), part[3] * r.height()))
}

/// The place between characters at `pos`: before the character under it or after it,
/// by which half is pointed at. Away from all text, only when `nearest`.
fn page_place(text: &PageText, r: Rect, pos: Pos2, nearest: bool) -> Option<u32> {
    // Its characters lie on it: away from a page, none of them is under the pointer.
    if !nearest && !r.expand(2.0).contains(pos) { return None; }
    let mut best: Option<(f32, usize)> = None;
    for (i, &(_, part)) in text.chars.iter().enumerate() {
        let distance = char_rect(r, part).expand(1.0).distance_sq_to_pos(pos);
        if distance == 0.0 { best = Some((0.0, i)); break; }
        if nearest && best.is_none_or(|(d, _)| distance < d) { best = Some((distance, i)); }
    }
    let (_, i) = best?;
    let after = pos.x > char_rect(r, text.chars[i].1).center().x;
    Some(i as u32 + after as u32)
}

/// Text on a page is chosen by dragging from a character; a drag that starts away from
/// the text still moves the pages. True while choosing, so that they do not move.
fn page_choosing(ui: &Ui, s: &mut Shared, sheets: &[Sheet], response: &egui::Response) -> bool {
    let (pressed, down, shift, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift, i.pointer.interact_pos()));
    let Some(pos) = pos else { return s.view.choosing };
    let texts: Vec<(u32, Rect, &PageText)> = sheets.iter().filter_map(|sheet| Some((sheet.page, sheet.rect, sheet.text.as_deref()?))).collect();
    let over = response.hovered().then(|| texts.iter().find_map(|&(page, r, text)| Some((page, page_place(text, r, pos, false)?, text)))).flatten();
    if over.is_some() && !response.dragged() { ui.ctx().set_cursor_icon(egui::CursorIcon::Text); }
    let held = match s.view.select { Selection::Page { page, anchor, .. } => Some((page, anchor)), _ => None };
    if pressed && response.hovered() {
        s.view.choosing = over.is_some();
        s.view.select = match (over, shift.then_some(held).flatten()) {
            (Some((page, at, _)), Some((from_page, from))) if from_page == page => Selection::Page { page, anchor: from, head: at },
            (Some((page, at, _)), _) => Selection::Page { page, anchor: at, head: at },
            (None, _) => Selection::None,
        };
    } else if s.view.choosing && down && let Some((page, anchor)) = held
        && let Some(&(_, r, text)) = texts.iter().find(|t| t.0 == page)
        && let Some(at) = page_place(text, r, pos, true)
    {
        s.view.select = Selection::Page { page, anchor, head: at };
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    if response.double_clicked() && let Some((page, at, text)) = over {
        let i = (at as usize).min(text.chars.len() - 1);
        let (from, to) = select::word_at(&text.text, text.chars[i].0 as usize);
        s.view.select = Selection::Page { page, anchor: select::page_char(text, from), head: select::page_char(text, to) };
        return true;
    }
    // Held until the button is let go: egui calls it a drag only once the pointer has moved
    // a little, and the frames before that must not end the choosing.
    let choosing = s.view.choosing;
    if !down && !pressed { s.view.choosing = false; }
    choosing
}

/// Chosen characters, joined along their lines so that no seams show.
fn paint_page_choice(p: &egui::Painter, text: &PageText, r: Rect, (from, to): (u32, u32), color: Color32) {
    let mut run: Option<Rect> = None;
    for &(_, part) in text.chars.iter().take(to as usize).skip(from as usize) {
        let c = char_rect(r, part);
        run = match run {
            Some(line) if (c.center().y - line.center().y).abs() < line.height() * 0.3 && c.left() <= line.right() + line.height() => Some(line.union(c)),
            Some(line) => { p.rect_filled(line, CornerRadius::ZERO, color); Some(c) }
            None => Some(c),
        };
    }
    if let Some(line) = run { p.rect_filled(line, CornerRadius::ZERO, color); }
}

/// Where a picture goes before panning.
fn fitted_rect(frame: Rect, tex: &egui::TextureHandle, orig: [u32; 2], zoom: f32, paged: bool, ppp: f32) -> Rect {
    // Drawn larger than its nominal size means vector art, which has no size of its own.
    if paged || tex.size()[0] > orig[0] as usize { page_rect(frame, orig, zoom) } else { image_rect(frame, orig, zoom, ppp) }
}

/// A document page fills the frame, because its size in points says nothing about the screen.
fn page_rect(frame: Rect, orig: [u32; 2], zoom: f32) -> Rect {
    let size = vec2(orig[0].max(1) as f32, orig[1].max(1) as f32);
    let area = frame.shrink(12.0);
    let fit = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(frame.center(), size * fit * zoom)
}

fn card(ui: &Ui, s: &Shared, body: Rect, message: &str) {
    let p = ui.painter_at(body);
    let side = body.width().min(body.height()).clamp(64.0, 128.0);
    let icon = Rect::from_center_size(body.center() - vec2(0.0, side * 0.4), Vec2::splat(side));
    if s.is_dir { icons::folder(&p, icon, s.folder); } else { icons::entry(&p, icon, s.kind, &s.ext, s.pal.dark, true); }
    let g = p.layout(message.to_owned(), FontId::proportional(14.0), s.pal.text_weak, body.width() - 80.0);
    let at = pos2(body.center().x - g.size().x / 2.0, icon.max.y + 18.0);
    p.galley(at, g, s.pal.text_weak);
}

fn class_color(class: u8, pal: Palette) -> Color32 {
    match class {
        CLASS_COMMENT => pal.text_weak,
        CLASS_STRING => pal.text_important,
        CLASS_NUMBER => blend(pal.accent, pal.text, 0.45),
        CLASS_KEYWORD => pal.accent,
        CLASS_FUNCTION => blend(pal.text, pal.accent, 0.35),
        CLASS_TYPE => blend(pal.text, pal.text_important, 0.55),
        CLASS_TAG => blend(pal.accent, pal.text_important, 0.5),
        _ => pal.text,
    }
}

/// The coloured pieces of `start..end`, a stretch of one line.
fn coloured_row(doc: &TextDoc, line: u32, start: u32, end: u32, font: &FontId, pal: Palette) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat::simple(font.clone(), color);
    let Some(spans) = doc.colours.line(line as usize) else {
        job.append(&doc.text[start as usize..end as usize], 0.0, format(pal.text));
        return job;
    };
    let mut at = doc.lines[line as usize].0;
    for &(len, class) in spans {
        let (from, to) = (at.max(start), (at + len).min(end));
        if from < to { job.append(&doc.text[from as usize..to as usize], 0.0, format(class_color(class, pal))); }
        at += len;
    }
    job
}

/// Only visible rows are laid out, so a large file costs the same as a small one.
fn text_view(ui: &mut Ui, s: &mut Shared, doc: &Arc<TextDoc>, body: Rect) {
    let pal = s.pal;
    let font = FontId::monospace(13.0);
    let (char_w, line_h) = ui.ctx().fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = (line_h + 3.0).round();
    let digits = doc.lines.len().max(1).ilog10() as f32 + 1.0;
    let gutter = digits * char_w + 18.0;
    let text_x = gutter + 8.0;
    let view_w = body.width() - text_x - 16.0;
    let columns = ((view_w / char_w) as usize).max(10);
    let wrap = s.view.wrap;
    let key = (if wrap { columns } else { 1 }, Arc::as_ptr(doc) as usize);
    if s.view.rows_for != key {
        s.view.rows = doc.rows(wrap.then_some(columns));
        s.view.rows_for = key;
    }
    let rows = std::mem::take(&mut s.view.rows);
    let content_w = if wrap { view_w } else { doc.longest.min(100_000) as f32 * char_w } + text_x + 16.0;
    let mut area = egui::ScrollArea::both().id_salt("preview_text").auto_shrink(false);
    if let Some(y) = s.view.scroll_to.take() { area = area.vertical_scroll_offset(y); }
    let total = rows.len() + doc.trimmed as usize;
    let mut dragging = false;
    let output = area.show_rows(ui, row_h, total, |ui, range| {
        ui.set_min_width(content_w);
        let top = ui.min_rect().min;
        let n = range.len() as f32;
        let step = row_h + ui.spacing().item_spacing.y;
        let (r, response) = ui.allocate_exact_size(vec2(content_w, (n * step - ui.spacing().item_spacing.y).max(0.0)), Sense::click_and_drag());
        let p = ui.painter_at(r.expand(2.0));
        // One enormous line costs a whole frame to lay out; nobody scrolls that far right.
        let cut = |start: u32, end: u32| doc.text.floor_char_boundary((end as usize).min(start as usize + 20_000)) as u32;
        let shown = range.start..range.end.min(rows.len());
        // The place under the pointer, held to the rows on screen while choosing past them.
        let place = |pos: Pos2| -> Option<u32> {
            if shown.is_empty() { return None; }
            let row = (shown.start as isize + ((pos.y - top.y) / step).floor() as isize).clamp(shown.start as isize, shown.end as isize - 1) as usize;
            let (line, start, end) = rows[row];
            if pos.x < top.x + text_x { return Some(start); }
            let end = cut(start, end);
            let galley = p.layout_job(coloured_row(doc, line, start, end, &font, pal));
            let x = pos.x - top.x - text_x;
            Some(start + select::byte_at(&galley, &doc.text[start as usize..end as usize], vec2(x, galley.size().y / 2.0)) as u32)
        };
        text_choosing(ui, s, &response, &place, |at| {
            let (start, end) = select::word_at(&doc.text, at as usize);
            (start as u32, end as u32)
        }, |at| {
            let line = doc.lines.partition_point(|l| l.1 < at).min(doc.lines.len().saturating_sub(1));
            doc.lines.get(line).map_or((at, at), |&(start, end)| (start, (end as usize + 1).min(doc.text.len()) as u32))
        });
        dragging = response.dragged();
        if response.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::Text); }
        copy_menu(&response, s);
        let chosen_range = match s.view.select {
            Selection::Text { anchor, head } if anchor != head => Some(select::ordered(anchor, head)),
            _ => None,
        };
        let mut last = None;
        for (at, row) in range.enumerate() {
            let y = top.y + at as f32 * step + row_h / 2.0;
            let Some(&(line, start, end)) = rows.get(row) else {
                p.text(pos2(top.x + text_x, y), Align2::LEFT_CENTER, "Only the beginning of this file is shown", FontId::proportional(12.0), pal.text_weak);
                continue;
            };
            if last != Some(line) {
                p.text(pos2(top.x + gutter - 6.0, y), Align2::RIGHT_CENTER, (line + 1).to_string(), font.clone(), blend(pal.text_weak, pal.panel, 0.35));
            }
            last = Some(line);
            let end = cut(start, end);
            let galley = p.layout_job(coloured_row(doc, line, start, end, &font, pal));
            let at = pos2(top.x + text_x, y - galley.size().y / 2.0);
            if let Some((from, to)) = chosen_range && from <= end && to > start {
                let row_text = &doc.text[start as usize..end as usize];
                let a = select::chars_before(row_text, from.saturating_sub(start) as usize);
                let b = select::chars_before(row_text, (to.min(end) - start) as usize);
                // The line end is chosen too: show it as a little room past the text.
                let past = to > end && end == doc.lines[line as usize].1;
                let row0 = &galley.rows[0];
                let (x0, x1) = (row0.x_offset(egui::text::CharIndex(a)), if past { row0.size.x + char_w / 2.0 } else { row0.x_offset(egui::text::CharIndex(b)) });
                if x1 > x0 {
                    p.rect_filled(Rect::from_x_y_ranges(at.x + x0..=at.x + x1, y - row_h / 2.0..=y + row_h / 2.0), CornerRadius::ZERO, chosen(pal));
                }
            }
            p.galley(at, galley, pal.text);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
    s.view.rows = rows;
    follow_pointer(ui, s, dragging, output.inner_rect, s.view.offset);
}

/// Choosing text with the mouse: a press starts, a drag extends, Shift extends from where
/// it started, a double click takes the word and a triple click the line. `place` turns a
/// pointer position into a byte; `word` and `line` turn a byte into a stretch.
fn text_choosing(ui: &Ui, s: &mut Shared, response: &egui::Response, place: &dyn Fn(Pos2) -> Option<u32>, word: impl Fn(u32) -> (u32, u32), line: impl Fn(u32) -> (u32, u32)) {
    let (pressed, shift, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift, i.pointer.interact_pos()));
    let Some(pos) = pos else { return };
    let anchor = match s.view.select { Selection::Text { anchor, .. } => Some(anchor), _ => None };
    if pressed && response.hovered() {
        let Some(at) = place(pos) else { return };
        s.view.select = Selection::Text { anchor: if shift { anchor.unwrap_or(at) } else { at }, head: at };
    } else if response.dragged_by(egui::PointerButton::Primary) && let (Some(anchor), Some(at)) = (anchor, place(pos)) {
        s.view.select = Selection::Text { anchor, head: at };
    }
    if response.triple_clicked() && let Some(at) = place(pos) {
        let (anchor, head) = line(at);
        s.view.select = Selection::Text { anchor, head };
    } else if response.double_clicked() && let Some(at) = place(pos) {
        let (anchor, head) = word(at);
        s.view.select = Selection::Text { anchor, head };
    }
}

/// Sized once per width; only the blocks on screen are painted.
fn doc_view(ui: &mut Ui, s: &mut Shared, doc: &Arc<DocData>, body: Rect) {
    let pal = s.pal;
    let width = (body.width() - 64.0).clamp(120.0, READING_WIDTH.max(120.0));
    let left = (body.width() - width) / 2.0;
    let key = (width as usize, Arc::as_ptr(doc) as usize);
    if s.view.laid_out_for != key {
        s.view.laid_out_for = key;
        let p = ui.painter();
        let cover_h = doc.cover.as_ref().map_or(0.0, |(_, size)| cover_height(*size, width) + 24.0);
        let (mut y, mut laid) = (24.0 + cover_h, Vec::with_capacity(doc.blocks.len()));
        for block in &doc.blocks {
            let (font, color, indent, before, after) = match block.kind {
                BlockKind::H1 => (FontId::proportional(26.0), pal.text, 0.0, 14.0, 10.0),
                BlockKind::H2 => (FontId::proportional(21.0), pal.text, 0.0, 12.0, 8.0),
                BlockKind::H3 => (FontId::proportional(17.0), pal.text, 0.0, 10.0, 6.0),
                BlockKind::Paragraph => (FontId::proportional(15.0), pal.text, 0.0, 0.0, 10.0),
                BlockKind::Quote => (FontId::proportional(15.0), pal.text_weak, 16.0, 0.0, 10.0),
                BlockKind::Code => (FontId::monospace(13.0), pal.text, 12.0, 2.0, 10.0),
                BlockKind::Item => (FontId::proportional(15.0), pal.text, 18.0, 0.0, 4.0),
                BlockKind::Rule => (FontId::proportional(6.0), pal.text_weak, 0.0, 6.0, 12.0),
            };
            y += before;
            let galley = p.layout(block.text.clone(), font, color, width - indent * 2.0);
            let height = galley.size().y;
            laid.push((galley, pos2(indent, y), block.kind));
            y += height + after;
        }
        if doc.trimmed {
            let note = p.layout("Only the beginning of this document is shown".to_owned(), FontId::proportional(12.0), pal.text_weak, width);
            laid.push((note, pos2(0.0, y + 8.0), BlockKind::Paragraph));
            y += 32.0;
        }
        s.view.laid_out = laid;
        s.view.laid_out_height = y + 48.0;
    }
    let laid = std::mem::take(&mut s.view.laid_out);
    let height = s.view.laid_out_height;
    let mut area = egui::ScrollArea::vertical().id_salt("preview_doc").auto_shrink(false);
    if let Some(y) = s.view.scroll_to.take() { area = area.vertical_scroll_offset(y); }
    let mut dragging = false;
    let output = area.show_viewport(ui, |ui, viewport| {
        let (r, response) = ui.allocate_exact_size(vec2(body.width(), height), Sense::click_and_drag());
        doc_choosing(ui, s, doc, &laid, &response, r.min + vec2(left, 0.0));
        dragging = response.dragged();
        if response.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::Text); }
        copy_menu(&response, s);
        let chosen_range = match s.view.select {
            Selection::Doc { anchor, head } if anchor != head => Some(select::ordered(anchor, head)),
            _ => None,
        };
        let p = ui.painter();
        if let Some((cover, size)) = &doc.cover {
            let h = cover_height(*size, width);
            let w = h * size[0] as f32 / size[1].max(1) as f32;
            p.image(cover.id(), Rect::from_min_size(r.min + vec2((body.width() - w) / 2.0, 24.0), vec2(w, h)), unit_uv(), Color32::WHITE);
        }
        for (i, (galley, at, kind)) in laid.iter().enumerate() {
            let size = galley.size();
            if at.y + size.y + 16.0 < viewport.min.y || at.y - 16.0 > viewport.max.y { continue; }
            let pos = r.min + vec2(left + at.x, at.y);
            if let Some((from, to)) = chosen_range && let Some(block) = doc.blocks.get(i) && (from.0..=to.0).contains(&(i as u32)) {
                let a = if i as u32 == from.0 { select::chars_before(&block.text, from.1 as usize) } else { 0 };
                let b = if i as u32 == to.0 { select::chars_before(&block.text, to.1 as usize) } else { usize::MAX };
                for span in select::galley_spans(galley, a, b) {
                    p.rect_filled(span.translate(pos.to_vec2()), CornerRadius::ZERO, chosen(pal));
                }
            }
            match kind {
                BlockKind::Quote => { p.rect_filled(Rect::from_min_size(pos - vec2(14.0, 0.0), vec2(3.0, size.y)), pal.corner_small(), pal.raised); }
                BlockKind::Code => { p.rect_filled(Rect::from_min_size(pos - vec2(8.0, 6.0), vec2(width - at.x, size.y + 12.0)), pal.corner(), blend(pal.raised, pal.window, 0.45)); }
                BlockKind::Item => { p.circle_filled(pos + vec2(-11.0, 10.0), 2.5, pal.text_weak); }
                BlockKind::Rule => { p.line_segment([pos, pos + vec2(width, 0.0)], Stroke::new(1.0, pal.raised)); }
                _ => {}
            }
            p.galley(pos, galley.clone(), pal.text);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
    s.view.laid_out = laid;
    follow_pointer(ui, s, dragging, output.inner_rect, s.view.offset);
}

/// As `text_choosing`, over the blocks of a document laid out from `origin`.
fn doc_choosing(ui: &Ui, s: &mut Shared, doc: &DocData, laid: &[(Arc<egui::Galley>, Pos2, BlockKind)], response: &egui::Response, origin: Pos2) {
    let (pressed, shift, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift, i.pointer.interact_pos()));
    let Some(pos) = pos else { return };
    if !(pressed || response.dragged() || response.double_clicked() || response.triple_clicked()) { return; }
    // The note about a cut document comes after the blocks and is not part of them.
    let count = doc.blocks.len().min(laid.len());
    if count == 0 { return; }
    let local = pos - origin;
    let i = laid[..count].partition_point(|(galley, at, _)| at.y + galley.size().y < local.y).min(count - 1);
    let (galley, at, _) = &laid[i];
    let text = &doc.blocks[i].text;
    let byte = if local.y < at.y { 0 } else { select::byte_at(galley, text, local - at.to_vec2()) } as u32;
    let here = (i as u32, byte);
    let anchor = match s.view.select { Selection::Doc { anchor, .. } => Some(anchor), _ => None };
    if pressed && response.hovered() {
        s.view.select = Selection::Doc { anchor: if shift { anchor.unwrap_or(here) } else { here }, head: here };
    } else if response.dragged_by(egui::PointerButton::Primary) && let Some(anchor) = anchor {
        s.view.select = Selection::Doc { anchor, head: here };
    }
    if response.triple_clicked() {
        s.view.select = Selection::Doc { anchor: (here.0, 0), head: (here.0, text.len() as u32) };
    } else if response.double_clicked() {
        let (from, to) = select::word_at(text, byte as usize);
        s.view.select = Selection::Doc { anchor: (here.0, from as u32), head: (here.0, to as u32) };
    }
}

fn cover_height(size: [u32; 2], width: f32) -> f32 {
    let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
    (width / aspect).min(420.0)
}

fn table_view(ui: &mut Ui, s: &mut Shared, table: &Arc<TableDoc>, body: Rect) {
    let pal = s.pal;
    let font = FontId::proportional(13.0);
    let digit = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0'));
    let row_h = 24.0;
    let columns: Vec<f32> = table.widths.iter().map(|w| *w as f32 * digit * 1.15 + 20.0).collect();
    let numbers = table.rows.len().max(1).ilog10() as f32 * digit + digit + 24.0;
    let width = numbers + columns.iter().sum::<f32>() + 16.0;
    let mut area = egui::ScrollArea::both().id_salt("preview_table").auto_shrink(false);
    if let Some(y) = s.view.scroll_to.take() { area = area.vertical_scroll_offset(y); }
    let total = table.rows.len() + table.trimmed as usize;
    let mut dragging = false;
    let output = area.show_rows(ui, row_h, total, |ui, range| {
        let top = ui.min_rect().min;
        let step = row_h + ui.spacing().item_spacing.y;
        let (r, response) = ui.allocate_exact_size(vec2(width.max(body.width() - 16.0), range.len() as f32 * step), Sense::click_and_drag());
        cell_choosing(ui, s, &response, &columns, top + vec2(numbers, 0.0), step, range.start..range.end.min(table.rows.len()));
        dragging = response.dragged();
        copy_menu(&response, s);
        let chosen_cells = match s.view.select {
            Selection::Cells { anchor, head } => Some((select::ordered(anchor.0, head.0), select::ordered(anchor.1, head.1))),
            _ => None,
        };
        let p = ui.painter_at(r.expand(2.0));
        for (at, row) in range.enumerate() {
            let band = Rect::from_min_size(top + vec2(0.0, at as f32 * step), vec2(r.width(), row_h));
            let Some(cells) = table.rows.get(row) else {
                p.text(pos2(top.x + numbers, band.center().y), Align2::LEFT_CENTER, "Only the beginning of this table is shown", FontId::proportional(12.0), pal.text_weak);
                continue;
            };
            if row == 0 { p.rect_filled(band, CornerRadius::ZERO, blend(pal.raised, pal.window, 0.3)); }
            else if row % 2 == 0 { p.rect_filled(band, CornerRadius::ZERO, blend(pal.window, pal.raised, 0.25)); }
            if let Some(((first, last), (left_col, right_col))) = chosen_cells && (first..=last).contains(&(row as u32)) {
                let x_of = |c: u32| top.x + numbers + columns.iter().take(c as usize).sum::<f32>();
                let right = x_of((right_col + 1).min(columns.len() as u32));
                let shade = Rect::from_x_y_ranges(x_of(left_col) - 4.0..=right - 4.0, band.y_range());
                p.rect_filled(shade, CornerRadius::ZERO, chosen(pal));
            }
            p.text(pos2(top.x + numbers - 12.0, band.center().y), Align2::RIGHT_CENTER, (row + 1).to_string(), FontId::monospace(11.0), pal.text_weak);
            let mut x = top.x + numbers;
            for (cell, column) in cells.iter().zip(&columns) {
                if !cell.is_empty() {
                    let clip = Rect::from_min_size(pos2(x, band.min.y), vec2(*column - 10.0, row_h));
                    let shown = fit_text(&p, cell.lines().next().unwrap_or(""), &font, clip.width());
                    p.with_clip_rect(clip.intersect(p.clip_rect())).text(pos2(x, band.center().y), Align2::LEFT_CENTER, shown, font.clone(), pal.text);
                }
                x += column;
            }
        }
    });
    s.view.offset = output.state.clamped_offset().y;
    follow_pointer(ui, s, dragging, output.inner_rect, s.view.offset);
}

/// Cells are chosen as a rectangle: a press on one, a drag to another. A press on a row
/// number takes the whole row.
#[allow(clippy::too_many_arguments)]
fn cell_choosing(ui: &Ui, s: &mut Shared, response: &egui::Response, columns: &[f32], origin: Pos2, step: f32, shown: std::ops::Range<usize>) {
    let (pressed, shift, pos) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift, i.pointer.interact_pos()));
    let Some(pos) = pos else { return };
    if shown.is_empty() || columns.is_empty() || !(pressed || response.dragged()) { return; }
    let row = (shown.start as isize + ((pos.y - origin.y) / step).floor() as isize).clamp(shown.start as isize, shown.end as isize - 1) as u32;
    let last = columns.len() as u32 - 1;
    let mut x = pos.x - origin.x;
    let numbers = x < 0.0;
    let column = columns.iter().position(|w| { x -= w; x < 0.0 }).map_or(last, |c| c as u32);
    let anchor = match s.view.select { Selection::Cells { anchor, .. } => Some(anchor), _ => None };
    if pressed && response.hovered() {
        s.view.select = match (numbers, shift.then_some(anchor).flatten()) {
            (true, from) => Selection::Cells { anchor: (from.map_or(row, |a| a.0), 0), head: (row, last) },
            (false, Some(from)) => Selection::Cells { anchor: from, head: (row, column) },
            (false, None) => Selection::Cells { anchor: (row, column), head: (row, column) },
        };
    } else if response.dragged_by(egui::PointerButton::Primary) && let Some(anchor) = anchor {
        let head = if anchor.1 == 0 && numbers { (row, last) } else { (row, column) };
        s.view.select = Selection::Cells { anchor, head };
    }
}

fn archive_view(ui: &mut Ui, s: &mut Shared, entries: &[ArchiveEntry], total: u64, trimmed: bool, body: Rect) {
    let pal = s.pal;
    let font = FontId::proportional(13.0);
    let row_h = 24.0;
    let mut area = egui::ScrollArea::vertical().id_salt("preview_archive").auto_shrink(false);
    if let Some(y) = s.view.scroll_to.take() { area = area.vertical_scroll_offset(y); }
    let output = area.show_rows(ui, row_h, entries.len() + 1, |ui, range| {
        let top = ui.min_rect().min;
        let step = row_h + ui.spacing().item_spacing.y;
        let width = body.width() - 16.0;
        let (r, _) = ui.allocate_exact_size(vec2(width, range.len() as f32 * step), Sense::hover());
        let p = ui.painter_at(r.expand(2.0));
        for (at, row) in range.enumerate() {
            let y = top.y + at as f32 * step + row_h / 2.0;
            let Some(entry) = entries.get(row) else {
                let note = if trimmed { format!("The first {} items are shown", entries.len()) } else { format!("{total} items") };
                p.text(pos2(top.x + 16.0, y), Align2::LEFT_CENTER, note, FontId::proportional(12.0), pal.text_weak);
                continue;
            };
            let icon = Rect::from_center_size(pos2(top.x + 26.0, y), Vec2::splat(16.0));
            if entry.folder {
                icons::folder(&p, icon, Kind::Folder.color());
            } else {
                let ext = fsx::split_ext(&entry.name, false);
                icons::entry(&p, icon, Kind::from_ext(ext), ext, pal.dark, false);
            }
            let size = if entry.folder { String::new() } else { fsx::fmt_size(entry.size, s.mebibytes) };
            let size_rect = p.text(pos2(top.x + width - 16.0, y), Align2::RIGHT_CENTER, size, FontId::proportional(12.0), pal.text_weak);
            let name = fit_text(&p, &entry.name, &font, (size_rect.min.x - top.x - 60.0).max(40.0));
            p.text(pos2(top.x + 44.0, y), Align2::LEFT_CENTER, name, font.clone(), pal.text);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
}

fn info_view(ui: &mut Ui, s: &mut Shared, rows: &[(String, String)], cover: Option<(&egui::TextureHandle, [u32; 2])>, body: Rect) {
    let picture_min = if cover.is_some() { 64.0 } else { 96.0 };
    let minimum = picture_min + INFO_PICTURE_GAP + rows.len() as f32 * INFO_ROW_H + INFO_PADDING * 2.0;
    if minimum <= body.height() {
        info_contents(ui, s, rows, cover, body);
        return;
    }
    let output = egui::ScrollArea::vertical().id_salt("preview_info")
        .auto_shrink([false, false]).vertical_scroll_offset(s.view.offset)
        .show(ui, |ui| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), minimum), Sense::hover());
            info_contents(ui, s, rows, cover, r);
        });
    s.view.offset = output.state.clamped_offset().y;
}

fn info_contents(ui: &Ui, s: &Shared, rows: &[(String, String)], cover: Option<(&egui::TextureHandle, [u32; 2])>, body: Rect) {
    let pal = s.pal;
    let p = ui.painter_at(body);
    let (row_h, table_h) = (INFO_ROW_H, rows.len() as f32 * INFO_ROW_H);
    let area = body.shrink(INFO_PADDING);
    let picture_h = (area.height() - table_h - INFO_PICTURE_GAP).max(64.0);
    let top = if let Some((tex, size)) = cover {
        let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
        let h = picture_h.min(area.width() / aspect);
        let total = h + INFO_PICTURE_GAP + table_h;
        let at = body.center().y - total / 2.0;
        p.image(tex.id(), Rect::from_center_size(pos2(body.center().x, at + h / 2.0), vec2(h * aspect, h)), unit_uv(), Color32::WHITE);
        at + h + INFO_PICTURE_GAP
    } else {
        let side = 96.0;
        let total = side + INFO_PICTURE_GAP + table_h;
        let at = body.center().y - total / 2.0;
        icons::entry(&p, Rect::from_center_size(pos2(body.center().x, at + side / 2.0), Vec2::splat(side)), s.kind, &s.ext, pal.dark, true);
        at + side + INFO_PICTURE_GAP
    };
    let font = FontId::proportional(13.0);
    let middle = body.center().x;
    for (i, (key, value)) in rows.iter().enumerate() {
        let y = top + i as f32 * row_h + row_h / 2.0;
        p.text(pos2(middle - 10.0, y), Align2::RIGHT_CENTER, key, font.clone(), pal.text_weak);
        let shown = fit_text(&p, value, &font, body.max.x - middle - 40.0);
        p.text(pos2(middle + 10.0, y), Align2::LEFT_CENTER, shown, font.clone(), pal.text);
    }
}

fn hex_view(ui: &mut Ui, s: &mut Shared, bytes: &[u8], total: u64, body: Rect) {
    let pal = s.pal;
    let font = FontId::monospace(13.0);
    let row_h = (ui.ctx().fonts_mut(|f| f.row_height(&font)) + 3.0).round();
    let rows = bytes.len().div_ceil(16);
    let cut = (total as usize > bytes.len()) as usize;
    let mut area = egui::ScrollArea::vertical().id_salt("preview_hex").auto_shrink(false);
    if let Some(y) = s.view.scroll_to.take() { area = area.vertical_scroll_offset(y); }
    let output = area.show_rows(ui, row_h, rows + cut, |ui, range| {
        let top = ui.min_rect().min;
        let step = row_h + ui.spacing().item_spacing.y;
        let (r, _) = ui.allocate_exact_size(vec2(body.width() - 16.0, range.len() as f32 * step), Sense::hover());
        let p = ui.painter_at(r.expand(2.0));
        for (at, row) in range.enumerate() {
            let y = top.y + at as f32 * step + row_h / 2.0;
            let Some(chunk) = bytes.get(row * 16..).filter(|rest| !rest.is_empty()).map(|rest| &rest[..rest.len().min(16)]) else {
                p.text(pos2(top.x + 16.0, y), Align2::LEFT_CENTER, format!("Only the first {} of this file is shown", fsx::fmt_size(bytes.len() as u64, s.mebibytes)), FontId::proportional(12.0), pal.text_weak);
                continue;
            };
            p.text(pos2(top.x + 16.0, y), Align2::LEFT_CENTER, format!("{:08x}", row * 16), font.clone(), pal.text_weak);
            let hex: Vec<String> = chunk.iter().enumerate().map(|(i, b)| if i == 8 { format!(" {b:02x}") } else { format!("{b:02x}") }).collect();
            p.text(pos2(top.x + 110.0, y), Align2::LEFT_CENTER, hex.join(" "), font.clone(), pal.text);
            let ascii: String = chunk.iter().map(|&b| if (32..127).contains(&b) { b as char } else { '·' }).collect();
            p.text(pos2(top.x + HEX_ASCII_X, y), Align2::LEFT_CENTER, ascii, font.clone(), pal.text_weak);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
}

fn music_strip(body: Rect) -> Rect {
    Rect::from_center_size(
        pos2(body.center().x, navigation_rect(body, 0.0).min.y - MUSIC_GAP - sound::TRANSPORT_H / 2.0),
        vec2((body.width() - 40.0).clamp(sound::TRANSPORT_H, 420.0), sound::TRANSPORT_H),
    )
}

fn control_bar(ui: &mut Ui, s: &mut Shared, body: Rect, ctx: &egui::Context, image: bool, pages: Option<(u32, u32)>) {
    let moved = ui.input(|i| i.pointer.time_since_last_movement() < 2.0 && i.pointer.has_pointer());
    let opacity = ctx.animate_bool_with_time(ui.scope_id().with("preview_bar"), moved, 0.12);
    if moved { ctx.request_repaint_after(Duration::from_millis(2100)); }
    if opacity <= 0.0 { return; }
    let pal = s.pal;
    let width = 170.0 + if image { 130.0 } else { 0.0 } + if pages.is_some() { 110.0 } else { 0.0 };
    let r = navigation_rect(body, width);
    let mut bar = ui.new_child(UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    bar.multiply_opacity(opacity);
    bar.painter().rect_filled(r, pal.corner_large(), blend(pal.raised, pal.window, 0.2));
    bar.add_space(8.0);
    let flat = |text: &str| egui::Button::new(egui::RichText::new(text).size(15.0)).frame(false);
    if symbol_button(&mut bar, "#prev", false, &pal).on_hover_text("Previous file").clicked() { s.send(Cmd::Step(-1), ctx); }
    if symbol_button(&mut bar, "#next", false, &pal).on_hover_text("Next file").clicked() { s.send(Cmd::Step(1), ctx); }
    if let Some((page, total)) = pages {
        bar.add_space(10.0);
        let paged = matches!(s.content, PreviewContent::Page { .. });
        let turn = |s: &mut Shared, by: i32| if paged { turn_page(&mut s.view, by) } else { s.send(Cmd::Page(by), ctx) };
        if symbol_button(&mut bar, "#prev", false, &pal).on_hover_text("Previous page").clicked() { turn(s, -1); }
        bar.label(egui::RichText::new(format!("{} / {total}", page + 1)).size(12.0).color(pal.text_weak));
        if symbol_button(&mut bar, "#next", false, &pal).on_hover_text("Next page").clicked() { turn(s, 1); }
    }
    if image {
        bar.add_space(10.0);
        if symbol_button(&mut bar, "#minus", false, &pal).clicked() { zoom_by(&mut s.view, 0.8); }
        bar.label(egui::RichText::new(format!("{:.0}%", s.view.zoom * 100.0)).size(12.0).color(pal.text_weak));
        if symbol_button(&mut bar, "#plus", false, &pal).clicked() { zoom_by(&mut s.view, 1.25); }
    }
    bar.add_space(10.0);
    if bar.add(flat("Open")).clicked() { s.send(Cmd::Open, ctx); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_on_screen(ctx: &egui::Context) -> Shared {
        let texture = ctx.load_texture("model", egui::ColorImage::filled([64, 64], Color32::WHITE), Default::default());
        Shared {
            pal: Palette::get(crate::theme::ThemeName::Dark, Color32::WHITE),
            name: "shape.stl".into(), kind: Kind::Other, ext: "stl".into(), is_dir: false, folder: Color32::WHITE,
            size: String::new(), place: (1, 1), mebibytes: false,
            content: PreviewContent::Model(texture, [32, 32]),
            strip: Vec::new(), previous: None, switched: Instant::now() - Duration::from_secs(1), loading: None, commands: Vec::new(),
            view: View { zoom: 1.0, ..Default::default() }, detail: None, lane: None, model: None,
            sound: sound::Sound::new().shared(),
        }
    }

    fn turns(s: &mut Shared) -> Vec<ModelView> {
        s.commands.drain(..).filter_map(|c| if let Cmd::Turn(view) = c { Some(view) } else { None }).collect()
    }

    #[test]
    fn the_spinner_leaves_time_for_the_file_list_to_be_drawn() {
        let ctx = egui::Context::default();
        let mut s = model_on_screen(&ctx);
        s.loading = Some(Instant::now() - SPINNER_AFTER * 2);
        let mut delay = Duration::ZERO;
        // The first frames of a new context ask for more on their own.
        for _ in 0..4 {
            let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| draw(ui, &mut s, ViewportClass::Deferred));
            out.textures_delta.clear();
            delay = out.viewport_output[&ViewportId::ROOT].repaint_delay;
        }
        assert!(delay >= SPINNER_FRAME / 2, "{delay:?}");
    }

    #[test]
    fn dragging_a_model_asks_for_new_looks_and_a_sharp_one_at_rest() {
        use egui::{Event, Modifiers, PointerButton, RawInput};
        let ctx = egui::Context::default();
        let mut s = model_on_screen(&ctx);
        let run = |s: &mut Shared, events: Vec<Event>| {
            let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| draw(ui, s, ViewportClass::Deferred));
            out.textures_delta.clear();
        };
        let button = |pos, pressed, button| Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE };
        let (from, to) = (pos2(300.0, 300.0), pos2(350.0, 300.0));
        run(&mut s, vec![Event::PointerMoved(from)]);
        run(&mut s, vec![]);
        assert!(turns(&mut s).is_empty(), "the first picture needs nothing more");

        run(&mut s, vec![button(from, true, PointerButton::Primary)]);
        run(&mut s, vec![Event::PointerMoved(to)]);
        run(&mut s, vec![Event::PointerMoved(pos2(375.0, 300.0))]);
        let moving = *turns(&mut s).last().expect("a drag asks for a new look");
        assert!(moving.turn > 0 && moving.tilt == 0 && !moving.fine, "{moving:?}");
        run(&mut s, vec![button(pos2(375.0, 300.0), false, PointerButton::Primary)]);
        turns(&mut s);

        std::thread::sleep(SHARPEN_AFTER + Duration::from_millis(20));
        run(&mut s, vec![]);
        let at_rest = turns(&mut s);
        assert_eq!(at_rest.len(), 1);
        assert!(at_rest[0].fine && at_rest[0].turn == moving.turn, "{at_rest:?}");
        run(&mut s, vec![]);
        assert!(turns(&mut s).is_empty(), "asked once");

        let (pan_from, pan_to) = (pos2(200.0, 200.0), pos2(260.0, 200.0));
        run(&mut s, vec![Event::PointerMoved(pan_from)]);
        run(&mut s, vec![button(pan_from, true, PointerButton::Secondary)]);
        run(&mut s, vec![Event::PointerMoved(pan_to)]);
        run(&mut s, vec![Event::PointerMoved(pos2(280.0, 200.0))]);
        run(&mut s, vec![button(pos2(280.0, 200.0), false, PointerButton::Secondary)]);
        let moved = *turns(&mut s).last().expect("the other button moves the model");
        assert!(moved.pan[0] > 0 && moved.turn == moving.turn, "{moved:?}");

        zoom_by(&mut s.view, 0.1);
        assert_eq!(s.view.zoom, MIN_MODEL_ZOOM, "a model can get smaller than its first picture");
        reset_look(&mut s.view);
        run(&mut s, vec![]);
        let back = *turns(&mut s).last().expect("the way back is drawn too");
        assert!(back.turn == 0 && back.tilt == 0 && back.zoom == 100 && back.pan == [0, 0], "{back:?}");
    }

    #[test]
    fn the_window_opens_at_the_size_of_its_content() {
        let largest = vec2(1400.0, 900.0);
        let fit = |content: &PreviewContent| {
            let ctx = egui::Context::default();
            let mut size = Vec2::ZERO;
            let mut out = ctx.run_ui(Default::default(), |ui| size = fitting_size(ui.ctx(), content, largest, false));
            out.textures_delta.clear();
            size
        };
        let text = |lines: usize, width: usize| PreviewContent::Text(Arc::new(TextDoc::new(vec!["x".repeat(width); lines].join("\n"), false, Default::default())));
        assert_eq!(fit(&text(3, 10)), MIN_OPEN, "a few short lines need no more than the smallest window");
        let middling = fit(&text(30, 90));
        assert!(middling.x > MIN_OPEN.x && middling.x < largest.x && middling.y > MIN_OPEN.y && middling.y < largest.y, "{middling:?}");
        assert_eq!(fit(&text(5000, 2000)), largest, "the screen limit holds");
        assert_eq!(fit(&PreviewContent::Error("broken".into())), MIN_OPEN);

        assert_eq!(picture_body([800, 600], 1.0), vec2(824.0, 624.0));
        assert_eq!(picture_body([800, 600], 2.0), vec2(424.0, 324.0), "one screen pixel per pixel on a scaled display");
        let page = page_body([595, 842], largest) - Vec2::splat(24.0);
        assert!((page.x / page.y - 595.0 / 842.0).abs() < 0.01 && page.y <= largest.y - BAR_H - 24.0 + 0.5, "{page:?}");
    }
    use std::path::PathBuf;

    fn entry(name: &str) -> Entry {
        let mut entry = Entry::named(PathBuf::from(name), name.into(), false, Kind::Other);
        entry.size = 1;
        entry
    }

    fn app() -> (App, egui::Context) {
        (App::new(Some(PathBuf::from("unused.png"))), egui::Context::default())
    }

    fn page(ctx: &egui::Context, page: u32, pages: u32) -> PreviewContent {
        let image = egui::ColorImage::filled([2, 2], Color32::WHITE);
        PreviewContent::Page { texture: ctx.load_texture("t", image, Default::default()), orig: [100, 200], page, pages, text: None }
    }

    #[test]
    fn returning_to_the_shown_file_ends_the_loading_state() {
        let (mut app, ctx) = app();
        let (seen, unfinished) = (entry("seen.bin"), entry("/does/not/exist.bin"));
        app.preview.remember(Key::of(&seen), PreviewContent::Hex(Arc::default(), 0));
        app.want_preview(Some(Key::of(&seen)), &ctx);
        app.hand_over(&ctx, &seen, 0);
        app.want_preview(Some(Key::of(&unfinished)), &ctx);
        app.hand_over(&ctx, &unfinished, 0);
        assert!(app.quick.as_ref().unwrap().shared.lock().unwrap().loading.is_some());
        app.want_preview(Some(Key::of(&seen)), &ctx);
        app.hand_over(&ctx, &seen, 0);
        assert!(app.quick.as_ref().unwrap().shared.lock().unwrap().loading.is_none(), "no spinner over a finished preview");
    }

    #[test]
    fn turning_pages_during_loading_keeps_the_place() {
        let (mut app, ctx) = app();
        let file = entry("a.pdf");
        app.hand_over(&ctx, &file, 0);
        app.quick.as_ref().unwrap().shared.lock().unwrap().content = page(&ctx, 2, 5);
        app.preview.content = PreviewContent::Loading;
        app.preview.page = 2;
        app.quick.as_ref().unwrap().shared.lock().unwrap().commands.push(Cmd::Page(1));
        app.preview_commands(&ctx);
        assert_eq!(app.preview.page, 3, "not back to the first page");
        app.preview.content = page(&ctx, 3, 5);
        app.quick.as_ref().unwrap().shared.lock().unwrap().commands.extend([Cmd::Page(1), Cmd::Page(1), Cmd::Page(1)]);
        app.preview_commands(&ctx);
        assert_eq!(app.preview.page, 4, "stops at the last page");
    }

    fn music_on_screen(ctx: &egui::Context) -> Shared {
        let rows = Arc::new(vec![("Length".to_string(), "1:40".to_string())]);
        let key = Key { path: PathBuf::from("/music/song.mp3"), modified: None, size: 1, page: 0, crop: None, view: None };
        Shared { name: "song.mp3".into(), kind: Kind::Audio, ext: "mp3".into(), content: PreviewContent::Info(rows, None, false), model: Some(key), ..model_on_screen(ctx) }
    }

    #[test]
    fn info_windows_only_reserve_music_controls_for_audio() {
        let ctx = egui::Context::default();
        let content = PreviewContent::Info(Arc::new(vec![("Tag".into(), "Value".into()); 6]), None, false);
        let mut sizes = [Vec2::ZERO; 2];
        let mut out = ctx.run_ui(Default::default(), |ui| {
            for (i, music) in [false, true].into_iter().enumerate() {
                sizes[i] = fitting_size(ui.ctx(), &content, vec2(1400.0, 900.0), music);
            }
        });
        out.textures_delta.clear();
        assert_eq!(sizes[1] - sizes[0], vec2(0.0, sound::TRANSPORT_H + MUSIC_GAP));
    }

    #[test]
    fn audio_metadata_scrolls_in_a_small_window_without_moving_controls() {
        use egui::{Event, MouseWheelUnit, Modifiers, RawInput};
        let ctx = egui::Context::default();
        let mut s = music_on_screen(&ctx);
        s.content = PreviewContent::Info(Arc::new((0..12).map(|i| (format!("Tag{i}"), "Value".into())).collect()), None, false);
        let full = Rect::from_min_size(Pos2::ZERO, vec2(320.0, 240.0));
        let mut last_visible = false;
        for i in 0..12 {
            let events = if i == 2 {
                vec![Event::PointerMoved(pos2(160.0, 80.0)), Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, -1000.0), phase: egui::TouchPhase::Move, modifiers: Modifiers::NONE, source: egui::MouseWheelSource::Wheel }]
            } else { vec![] };
            let mut out = ctx.run_ui(RawInput { screen_rect: Some(full), events, ..Default::default() }, |ui| draw(ui, &mut s, ViewportClass::Deferred));
            out.textures_delta.clear();
            last_visible |= out.shapes.iter().any(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.job.text == "Tag11" => shape.clip_rect.intersects(t.galley.rect.translate(t.pos.to_vec2())),
                _ => false,
            });
        }
        assert!(s.view.offset > 0.0 && last_visible, "the last metadata row must be reachable");
        assert!(s.commands.is_empty(), "scrolling metadata does not seek or navigate");
    }

    #[test]
    fn music_controls_stay_visible_and_clickable_above_navigation() {
        use egui::{Event, Modifiers, PointerButton, RawInput};
        for size in [vec2(320.0, 240.0), vec2(560.0, 420.0), vec2(800.0, 600.0)] {
            let ctx = egui::Context::default();
            let mut s = music_on_screen(&ctx);
            let full = Rect::from_min_size(Pos2::ZERO, size);
            let body = Rect::from_min_max(pos2(0.0, BAR_H), full.max);
            let strip = music_strip(body);
            let navigation = navigation_rect(body, 170.0);
            assert!(body.contains_rect(strip) && !strip.intersects(navigation));
            let at = strip.left_center() + vec2(sound::TRANSPORT_H / 2.0, 0.0);
            let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            // Dense metadata and loading must never cover or hide the player.
            for content in [PreviewContent::Info(Arc::new(vec![("Tag".into(), "Value".into()); 12]), None, false), PreviewContent::Loading] {
                s.content = content;
                for events in [vec![Event::PointerMoved(at)], vec![button(true)], vec![button(false)], vec![]] {
                    let mut out = ctx.run_ui(RawInput { screen_rect: Some(full), events, ..Default::default() }, |ui| draw(ui, &mut s, ViewportClass::Deferred));
                    out.textures_delta.clear();
                }
                assert_eq!(std::mem::take(&mut s.commands), [Cmd::Play], "{size:?}");
            }
        }
    }

    #[test]
    fn music_preview_starts_each_file_once_and_preserves_pause_and_failures() {
        let (mut app, ctx) = app();
        let song = Entry::named(PathBuf::from("song.mp3"), "song.mp3".into(), false, Kind::Audio);
        let next = Entry::named(PathBuf::from("next.mp3"), "next.mp3".into(), false, Kind::Audio);
        app.sound_preview(&song, &ctx);
        assert_eq!(app.sound.status().state, sound::State::Playing);
        for state in [sound::State::Paused, sound::State::Ended, sound::State::NoDevice, sound::State::Failed] {
            app.sound.shared().lock().unwrap().state = state;
            app.sound_preview(&song, &ctx);
            assert_eq!(app.sound.status().state, state);
        }
        app.sound_preview(&next, &ctx);
        assert_eq!(app.sound.status().track, Some(next.path));
        assert_eq!(app.sound.status().state, sound::State::Playing);
        app.sound_follows();
        app.sound_follows();
        assert_eq!(app.sound.status().state, sound::State::Idle);
        app.sound_preview(&song, &ctx);
        assert_eq!(app.sound.status().state, sound::State::Playing, "reopening starts again");
    }

    #[test]
    fn p_plays_the_shown_music_and_the_arrows_move_in_it() {
        use egui::{Event, Modifiers, RawInput};
        let ctx = egui::Context::default();
        let mut s = music_on_screen(&ctx);
        let press = |s: &mut Shared, key| {
            let events = vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }];
            let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| draw(ui, s, ViewportClass::Deferred));
            out.textures_delta.clear();
            std::mem::take(&mut s.commands)
        };
        assert_eq!(press(&mut s, K::ArrowRight), [Cmd::Page(1)], "nothing plays yet");
        assert_eq!(press(&mut s, K::P), [Cmd::Play]);
        let mut player = sound::Sound::new();
        s.sound = player.shared();
        player.play(std::path::Path::new("/music/song.mp3"), &ctx);
        s.sound.lock().unwrap().length = Duration::from_secs(100);
        player.seek(Duration::from_secs(40));
        let [Cmd::Seek(ahead)] = press(&mut s, K::ArrowRight)[..] else { panic!("no move ahead") };
        let [Cmd::Seek(back)] = press(&mut s, K::ArrowLeft)[..] else { panic!("no move back") };
        assert!((45_000..46_000).contains(&ahead) && (35_000..36_000).contains(&back), "{ahead} {back}");
        player.play(std::path::Path::new("/music/other.mp3"), &ctx);
        assert_eq!(press(&mut s, K::ArrowRight), [Cmd::Page(1)], "the music of another file is not this window's");
        s.kind = Kind::Other;
        assert!(press(&mut s, K::P).is_empty(), "only music has a play key");
    }

    #[test]
    fn the_play_command_starts_the_shown_file_and_leaving_it_ends_the_music() {
        let (mut app, ctx) = app();
        let song = entry("song.mp3");
        app.want_preview(Some(Key::of(&song)), &ctx);
        app.hand_over(&ctx, &song, 0);
        let ask = |app: &mut App, command| {
            app.quick.as_ref().unwrap().shared.lock().unwrap().commands.push(command);
            app.preview_commands(&ctx);
            app.sound.status()
        };
        let status = ask(&mut app, Cmd::Play);
        assert_eq!((status.track.as_ref(), status.state), (Some(&song.path), sound::State::Playing));
        assert_eq!(ask(&mut app, Cmd::Play).state, sound::State::Paused);
        assert_eq!(ask(&mut app, Cmd::Seek(7000)).position(), Duration::from_secs(7));
        assert_eq!(ask(&mut app, Cmd::Play).state, sound::State::Playing);
        app.sound_shown = Some(song.path.clone());
        app.sound_follows();
        assert_eq!(app.sound.status().state, sound::State::Playing, "its preview is still on show");
        app.sound_shown = Some(PathBuf::from("next.mp3"));
        app.sound_follows();
        assert_eq!(app.sound.status().state, sound::State::Idle, "another file is on show");
        ask(&mut app, Cmd::Play);
        app.sound_follows();
        assert_eq!(app.sound.status().state, sound::State::Idle, "no preview is on show at all");
    }

    /// Runs one frame and gives what was copied in it.
    fn frame(ctx: &egui::Context, s: &mut Shared, events: Vec<egui::Event>) -> Option<String> {
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| draw(ui, s, ViewportClass::Deferred));
        out.textures_delta.clear();
        out.platform_output.commands.iter().find_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None })
    }

    fn drag(ctx: &egui::Context, s: &mut Shared, from: Pos2, to: Pos2) {
        use egui::{Event, Modifiers, PointerButton};
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(ctx, s, vec![Event::PointerMoved(from)]);
        frame(ctx, s, vec![button(from, true)]);
        // Held a moment and moved a little, as a hand does, before egui calls it a drag.
        frame(ctx, s, vec![]);
        frame(ctx, s, vec![Event::PointerMoved(from + vec2(1.0, 0.0))]);
        frame(ctx, s, vec![Event::PointerMoved(from.lerp(to, 0.5))]);
        frame(ctx, s, vec![Event::PointerMoved(to)]);
        frame(ctx, s, vec![button(to, false)]);
    }

    /// A key pressed and let go, so that the next press is not a repeat.
    fn key(key: K, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        [true, false].map(|pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }).into()
    }

    #[test]
    fn text_is_chosen_with_the_mouse_and_copied() {
        let ctx = egui::Context::default();
        let doc = TextDoc::new("first line
second
third".into(), false, Default::default());
        let mut s = Shared { content: PreviewContent::Text(Arc::new(doc)), ..model_on_screen(&ctx) };
        frame(&ctx, &mut s, vec![]);
        let row = BAR_H + 8.0;
        drag(&ctx, &mut s, pos2(2.0, row), pos2(790.0, row));
        assert_eq!(frame(&ctx, &mut s, vec![egui::Event::Copy]).as_deref(), Some("first line"));
        assert!(s.commands.is_empty(), "choosing text neither closes nor moves");
        frame(&ctx, &mut s, key(K::A, egui::Modifiers::COMMAND));
        assert_eq!(frame(&ctx, &mut s, vec![egui::Event::Copy]).as_deref(), Some("first line
second
third"));
        frame(&ctx, &mut s, key(K::Escape, egui::Modifiers::NONE));
        assert!(s.commands.is_empty() && s.view.select.is_empty(), "the first Escape lets go of the text");
        assert_eq!(frame(&ctx, &mut s, vec![egui::Event::Copy]), None, "nothing chosen, nothing copied");
        frame(&ctx, &mut s, key(K::Escape, egui::Modifiers::NONE));
        assert_eq!(s.commands, [Cmd::Close]);
    }

    #[test]
    fn a_document_copies_its_paragraphs_and_a_word_is_a_double_click() {
        use super::super::load::Block;
        use egui::{Event, Modifiers, PointerButton};
        let ctx = egui::Context::default();
        let block = |kind, text: &str| Block { kind, text: text.into() };
        let blocks = vec![block(BlockKind::H1, "Title"), block(BlockKind::Paragraph, "Some words here"), block(BlockKind::Item, "one"), block(BlockKind::Item, "two")];
        let doc = DocData { blocks, source: None, trimmed: true, cover: None };
        let mut s = Shared { content: PreviewContent::Doc(Arc::new(doc)), ..model_on_screen(&ctx) };
        frame(&ctx, &mut s, key(K::A, Modifiers::COMMAND));
        assert_eq!(frame(&ctx, &mut s, vec![Event::Copy]).as_deref(), Some("Title

Some words here

one
two"), "the note about a cut document is not copied");
        let (galley, at, _) = s.view.laid_out[1].clone();
        let left = (800.0 - (800.0f32 - 64.0).clamp(120.0, READING_WIDTH.max(120.0))) / 2.0;
        let on = pos2(left, BAR_H) + at.to_vec2() + vec2(galley.size().x * 0.45, galley.size().y / 2.0);
        let click = |pressed| Event::PointerButton { pos: on, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(&ctx, &mut s, vec![Event::PointerMoved(on)]);
        frame(&ctx, &mut s, vec![click(true), click(false), click(true), click(false)]);
        assert_eq!(frame(&ctx, &mut s, vec![Event::Copy]).as_deref(), Some("words"));
    }

    #[test]
    fn a_whole_table_copies_as_tab_separated_rows() {
        let ctx = egui::Context::default();
        let rows = vec![vec!["Name".to_string(), "Size".into()], vec!["a.txt".into(), "12".into()]];
        let table = TableDoc { rows, widths: vec![6, 4], sheets: Vec::new(), page: 0, trimmed: false };
        let mut s = Shared { content: PreviewContent::Table(Arc::new(table)), ..model_on_screen(&ctx) };
        frame(&ctx, &mut s, key(K::A, egui::Modifiers::COMMAND));
        assert_eq!(frame(&ctx, &mut s, vec![egui::Event::Copy]).as_deref(), Some("Name	Size
a.txt	12"));
    }

    #[test]
    fn a_drag_on_the_text_of_a_page_chooses_it_and_elsewhere_moves_the_page() {
        let ctx = egui::Context::default();
        let image = egui::ColorImage::filled([2, 2], Color32::WHITE);
        let chars: Vec<(u32, [f32; 4])> = (0..5).map(|i| (i, [0.1 + i as f32 * 0.1, 0.1, 0.2 + i as f32 * 0.1, 0.2])).collect();
        let text = Arc::new(PageText { text: "Hello".into(), chars });
        let content = PreviewContent::Page { texture: ctx.load_texture("t", image, Default::default()), orig: [100, 100], page: 0, pages: 1, text: Some(text) };
        let mut s = Shared { content, ..model_on_screen(&ctx) };
        frame(&ctx, &mut s, vec![]);
        let body = Rect::from_min_max(pos2(0.0, BAR_H), pos2(800.0, 600.0));
        let page = page_rect(body, [100, 100], 1.0);
        let at = |x: f32, y: f32| page.min + vec2(x * page.width(), y * page.height());
        drag(&ctx, &mut s, at(0.11, 0.15), at(0.38, 0.15));
        assert_eq!(frame(&ctx, &mut s, vec![egui::Event::Copy]).as_deref(), Some("Hel"));
        assert_eq!(s.view.pan, Vec2::ZERO, "choosing does not move the page");
        frame(&ctx, &mut s, key(K::Plus, egui::Modifiers::NONE));
        frame(&ctx, &mut s, vec![]);
        drag(&ctx, &mut s, at(0.5, 0.7), at(0.45, 0.6));
        assert!(s.view.at > 0.0, "away from the text a drag still moves the page");
        assert!(s.view.select.is_empty(), "and lets go of the text");
    }

    fn pdf_on_screen(ctx: &egui::Context, shown: u32, loaded: &[u32]) -> Shared {
        let strip = loaded.iter().map(|&n| page(ctx, n, 5)).collect();
        Shared { content: page(ctx, shown, 5), strip, ..model_on_screen(ctx) }
    }

    fn shown_pages(s: &mut Shared) -> Vec<u32> {
        s.commands.drain(..).filter_map(|c| if let Cmd::ShowPage(n) = c { Some(n) } else { None }).collect()
    }

    #[test]
    fn the_wheel_scrolls_through_the_pages_and_asks_for_the_one_in_view() {
        use egui::{Event, Modifiers, MouseWheelUnit};
        let ctx = egui::Context::default();
        let mut s = pdf_on_screen(&ctx, 0, &[1, 2]);
        frame(&ctx, &mut s, vec![]);
        let wheel = Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, -120.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
        frame(&ctx, &mut s, vec![Event::PointerMoved(pos2(400.0, 300.0))]);
        for _ in 0..40 { frame(&ctx, &mut s, vec![wheel.clone()]); }
        for _ in 0..20 { frame(&ctx, &mut s, vec![]); }
        assert!(s.view.at > 0.5, "the pages moved up: {}", s.view.at);
        assert!(shown_pages(&mut s).contains(&1), "the second page is asked for once it fills the middle");
        assert_eq!(s.view.on_page, s.view.at.round() as u32);
    }

    #[test]
    fn the_page_keys_jump_to_the_top_of_a_page_and_stop_at_the_ends() {
        let ctx = egui::Context::default();
        let mut s = pdf_on_screen(&ctx, 0, &[1, 2]);
        frame(&ctx, &mut s, vec![]);
        frame(&ctx, &mut s, key(K::ArrowRight, egui::Modifiers::NONE));
        frame(&ctx, &mut s, key(K::PageDown, egui::Modifiers::NONE));
        assert_eq!(s.view.at, 2.0);
        assert_eq!(shown_pages(&mut s), [1, 2], "each page in view is asked for");
        s.view.at = 2.4;
        frame(&ctx, &mut s, key(K::PageUp, egui::Modifiers::NONE));
        assert_eq!(s.view.at, 2.0, "back to the top of this page first");
        frame(&ctx, &mut s, key(K::End, egui::Modifiers::NONE));
        assert_eq!(s.view.at, 4.0, "the last page, which fits the window");
        frame(&ctx, &mut s, key(K::Home, egui::Modifiers::NONE));
        assert_eq!(s.view.at, 0.0);
        assert!(!s.commands.iter().any(|c| matches!(c, Cmd::Page(_))), "the list turns no pages itself");
    }

    #[test]
    fn another_page_of_the_same_document_keeps_the_view_and_its_neighbours_come_along() {
        let (mut app, ctx) = app();
        let file = entry("a.pdf");
        for n in 0..3 { app.preview.remember(Key { page: n, ..Key::of(&file) }, page(&ctx, n, 5)); }
        app.want_preview(Some(Key::of(&file)), &ctx);
        app.hand_over(&ctx, &file, 0);
        {
            let mut s = app.quick.as_ref().unwrap().shared.lock().unwrap();
            assert_eq!(s.strip.iter().filter_map(|c| pages_of(c).map(|p| p.0)).collect::<Vec<_>>(), [0, 1, 2]);
            s.view.zoom = 2.0;
            s.view.at = 0.8;
            s.commands.push(Cmd::ShowPage(1));
        }
        app.preview_commands(&ctx);
        app.want_preview(Some(Key::of(&file)), &ctx);
        app.hand_over(&ctx, &file, 0);
        let s = app.quick.as_ref().unwrap().shared.lock().unwrap();
        assert!(matches!(s.content, PreviewContent::Page { page: 1, .. }));
        assert_eq!((s.view.zoom, s.view.at), (2.0, 0.8), "scrolling on does not start the view again");
        assert!(s.loading.is_none());
        drop(s);
        app.quick.as_ref().unwrap().shared.lock().unwrap().commands.push(Cmd::ShowPage(99));
        app.preview_commands(&ctx);
        assert_eq!(app.preview.page, 4, "no page past the last");
    }

    #[test]
    fn a_page_keeps_its_size_while_fading_to_the_next() {
        let ctx = egui::Context::default();
        let PreviewContent::Page { texture, .. } = page(&ctx, 0, 2) else { unreachable!() };
        let frame = Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0));
        let r = fitted_rect(frame, &texture, [595, 842], 1.0, true, 1.0);
        assert!(r.height() > 900.0, "a page fills the frame instead of using its size in points");
    }
}
