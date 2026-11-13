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

pub(super) fn viewport() -> ViewportId {
    ViewportId::from_hash_of("ferrous_commander_preview")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cmd {
    Close,
    Step(i32),
    Page(i32),
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
    last_look: (f32, Vec2),
    sent: Option<[u16; 4]>,
    wheel: f32,
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
            let around = [page.checked_sub(1), (page + 1 < pages).then_some(page + 1)];
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
        let waiting = matches!(self.preview.content, PreviewContent::Loading);
        if waiting {
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
                changed = true;
            }
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
        let egui::Event::Key { key, pressed: true, repeat, modifiers, .. } = event else { continue };
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
            K::PageDown if paged => s.send(Cmd::Page(1), ctx),
            K::PageUp if paged => s.send(Cmd::Page(-1), ctx),
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
            K::M if !repeat => { s.view.source = !s.view.source; s.view.rows_for = (0, 0); }
            _ => {}
        }
    }
}

/// The shown page and the page count of content that has pages or sheets.
fn pages_of(content: &PreviewContent) -> Option<(u32, u32)> {
    match content {
        PreviewContent::Page { page, pages, .. } => Some((*page, *pages)),
        PreviewContent::Table(table) if table.sheets.len() > 1 => Some((table.page, table.sheets.len() as u32)),
        _ => None,
    }
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
            image_view(&mut child, s, tex, *orig, *alpha, content_body, sharpen, false);
        }
        PreviewContent::Page { texture, orig, .. } => image_view(&mut child, s, texture, *orig, false, content_body, true, true),
        PreviewContent::Animation(frames, orig) => {
            let tex = playing_frame(s, &ctx, frames);
            image_view(&mut child, s, tex, *orig, true, content_body, false, false);
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
    let pages = pages_of(&content);
    control_bar(ui, s, body, &ctx, zoomable, pages);
    if cfg!(windows) { ui.painter().rect_stroke(full, CornerRadius::ZERO, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside); }
}

fn title_bar(ui: &mut Ui, s: &mut Shared, r: Rect, embedded: bool) {
    let pal = s.pal;
    let response = ui.interact(r, ui.scope_id().with("preview_title"), Sense::click_and_drag());
    if response.drag_started() && !embedded { ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag); }
    let icon = if s.is_dir { Icon::Folder(s.folder) } else { Icon::Entry(s.kind, &s.ext) };
    let info = if s.size.is_empty() { format!("{} / {}", s.place.0, s.place.1) } else { format!("{}   ·   {} / {}", s.size, s.place.0, s.place.1) };
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

#[allow(clippy::too_many_arguments)]
fn image_view(ui: &mut Ui, s: &mut Shared, tex: &egui::TextureHandle, orig: [u32; 2], alpha: bool, body: Rect, sharpen: bool, paged: bool) {
    let response = ui.interact(body, ui.scope_id().with("preview_image"), Sense::click_and_drag());
    let (wheel, pinch, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
    let before = s.view.zoom;
    let over = response.contains_pointer();
    if over && paged && pinch == 1.0 && before <= 1.0 {
        // At fit size a document's wheel turns pages, as it would scroll through them.
        s.view.wheel += wheel;
        if s.view.wheel.abs() >= 40.0 {
            s.send(Cmd::Page(if s.view.wheel > 0.0 { -1 } else { 1 }), &ui.ctx().clone());
            s.view.wheel = 0.0;
        }
    } else if over && paged && pinch == 1.0 {
        s.view.pan.y += wheel;
    } else if over && (wheel != 0.0 || pinch != 1.0) {
        s.view.zoom = (s.view.zoom * pinch * (wheel * 0.002).exp()).clamp(1.0, MAX_ZOOM);
        if let Some(at) = hover {
            let from_center = at - body.center() - s.view.pan;
            s.view.pan -= from_center * (s.view.zoom / before - 1.0);
        }
    }
    if response.dragged() { s.view.pan += response.drag_delta(); }
    if response.double_clicked() { s.view.zoom = 1.0; s.view.pan = Vec2::ZERO; }
    let ppp = ui.ctx().pixels_per_point();
    let fitted = fitted_rect(body, tex, orig, s.view.zoom, paged, ppp);
    let spare = ((fitted.size() - body.size()) / 2.0).max(Vec2::ZERO);
    s.view.pan = s.view.pan.clamp(-spare, spare);
    let r = fitted.translate(s.view.pan);
    if alpha { checkerboard(ui, r, body, s.pal); }
    let p = ui.painter_at(body);
    p.image(tex.id(), r, unit_uv(), Color32::WHITE);
    if s.view.zoom > 1.0 && let Some((detail, [l, t, cr, cb])) = &s.detail {
        let at = |x: u16, y: u16| r.min + vec2(x as f32 / 65535.0 * r.width(), y as f32 / 65535.0 * r.height());
        p.image(detail.id(), Rect::from_min_max(at(*l, *t), at(*cr, *cb)), unit_uv(), Color32::WHITE);
    }
    let look = (s.view.zoom, s.view.pan);
    if s.view.last_look != look {
        s.view.last_look = look;
        s.view.moved = Some(Instant::now());
    }
    let coarse = r.width() * ppp > tex.size()[0] as f32 * 1.05;
    if sharpen && coarse && let Some(moved) = s.view.moved {
        let crop = visible_part(r, body);
        if moved.elapsed() < SHARPEN_AFTER {
            ui.ctx().request_repaint_after(SHARPEN_AFTER);
        } else if s.view.sent != Some(crop) {
            s.view.sent = Some(crop);
            s.send(Cmd::Detail(crop), &ui.ctx().clone());
        }
    }
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
    let output = area.show_rows(ui, row_h, total, |ui, range| {
        ui.set_min_width(content_w);
        let top = ui.min_rect().min;
        let n = range.len() as f32;
        let step = row_h + ui.spacing().item_spacing.y;
        let (r, _) = ui.allocate_exact_size(vec2(content_w, (n * step - ui.spacing().item_spacing.y).max(0.0)), Sense::hover());
        let p = ui.painter_at(r.expand(2.0));
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
            // One enormous line costs a whole frame to lay out; nobody scrolls that far right.
            let end = doc.text.floor_char_boundary((end as usize).min(start as usize + 20_000)) as u32;
            let galley = p.layout_job(coloured_row(doc, line, start, end, &font, pal));
            p.galley(pos2(top.x + text_x, y - galley.size().y / 2.0), galley, pal.text);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
    s.view.rows = rows;
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
    let output = area.show_viewport(ui, |ui, viewport| {
        let (r, _) = ui.allocate_exact_size(vec2(body.width(), height), Sense::hover());
        let p = ui.painter();
        if let Some((cover, size)) = &doc.cover {
            let h = cover_height(*size, width);
            let w = h * size[0] as f32 / size[1].max(1) as f32;
            p.image(cover.id(), Rect::from_min_size(r.min + vec2((body.width() - w) / 2.0, 24.0), vec2(w, h)), unit_uv(), Color32::WHITE);
        }
        for (galley, at, kind) in &laid {
            let size = galley.size();
            if at.y + size.y + 16.0 < viewport.min.y || at.y - 16.0 > viewport.max.y { continue; }
            let pos = r.min + vec2(left + at.x, at.y);
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
    let output = area.show_rows(ui, row_h, total, |ui, range| {
        let top = ui.min_rect().min;
        let step = row_h + ui.spacing().item_spacing.y;
        let (r, _) = ui.allocate_exact_size(vec2(width.max(body.width() - 16.0), range.len() as f32 * step), Sense::hover());
        let p = ui.painter_at(r.expand(2.0));
        for (at, row) in range.enumerate() {
            let band = Rect::from_min_size(top + vec2(0.0, at as f32 * step), vec2(r.width(), row_h));
            let Some(cells) = table.rows.get(row) else {
                p.text(pos2(top.x + numbers, band.center().y), Align2::LEFT_CENTER, "Only the beginning of this table is shown", FontId::proportional(12.0), pal.text_weak);
                continue;
            };
            if row == 0 { p.rect_filled(band, CornerRadius::ZERO, blend(pal.raised, pal.window, 0.3)); }
            else if row % 2 == 0 { p.rect_filled(band, CornerRadius::ZERO, blend(pal.window, pal.raised, 0.25)); }
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
        if symbol_button(&mut bar, "#prev", false, &pal).on_hover_text("Previous page").clicked() { s.send(Cmd::Page(-1), ctx); }
        bar.label(egui::RichText::new(format!("{} / {total}", page + 1)).size(12.0).color(pal.text_weak));
        if symbol_button(&mut bar, "#next", false, &pal).on_hover_text("Next page").clicked() { s.send(Cmd::Page(1), ctx); }
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
            previous: None, switched: Instant::now() - Duration::from_secs(1), loading: None, commands: Vec::new(),
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
        PreviewContent::Page { texture: ctx.load_texture("t", image, Default::default()), orig: [100, 200], page, pages }
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

    #[test]
    fn a_page_keeps_its_size_while_fading_to_the_next() {
        let ctx = egui::Context::default();
        let PreviewContent::Page { texture, .. } = page(&ctx, 0, 2) else { unreachable!() };
        let frame = Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0));
        let r = fitted_rect(frame, &texture, [595, 842], 1.0, true, 1.0);
        assert!(r.height() > 900.0, "a page fills the frame instead of using its size in points");
    }
}
