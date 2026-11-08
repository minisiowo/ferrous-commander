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
use super::{DocData, Key, PreviewContent, TableDoc, TextDoc};
use crate::app::{Act, App};
use crate::fsx::{self, Entry, Kind};
use crate::icons;
use crate::theme::{Palette, blend};
use crate::ui::fit_text;

/// How long the file list waits for content before showing a window with a spinner.
const OPEN_WAIT: Duration = Duration::from_millis(80);
const SPINNER_AFTER: Duration = Duration::from_millis(150);
const SWITCH_SECONDS: f32 = 0.09;
/// Held Space must not close the preview and open it again from the file list.
const REOPEN_PAUSE: Duration = Duration::from_millis(300);
/// The compositor has decided by then whether the window floats.
const SIZE_LOCK: Duration = Duration::from_millis(400);
const BAR_H: f32 = 38.0;
const MAX_ZOOM: f32 = 32.0;
/// Pause after zooming or moving before a sharper copy is worth making.
const SHARPEN_AFTER: Duration = Duration::from_millis(150);
const READING_WIDTH: f32 = 760.0;
/// Past the end of any preview, yet small enough to compute with.
const SCROLL_TO_END: f32 = 1e9;

fn viewport() -> ViewportId {
    ViewportId::from_hash_of("ferrous_commander_preview")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cmd {
    Close,
    Step(i32),
    Page(i32),
    Detail([u16; 4]),
    Open,
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
}

pub struct Quick {
    shared: Arc<Mutex<Shared>>,
    opened: Instant,
    size: Vec2,
    limit: Vec2,
    position: Option<Pos2>,
    shown: Option<Key>,
    revision: u32,
    path: Option<std::path::PathBuf>,
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
                Cmd::Step(by) => self.key(i, if by > 0 { K::ArrowDown } else { K::ArrowUp }, egui::Modifiers::NONE, 1),
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
        let quick = self.quick.as_ref().unwrap();
        let waiting = matches!(self.preview.content, PreviewContent::Loading);
        if waiting && quick.opened.elapsed() < OPEN_WAIT {
            ctx.request_repaint_after(OPEN_WAIT);
            return;
        }
        let locked = quick.opened.elapsed() < SIZE_LOCK;
        if locked { ctx.request_repaint_after(SIZE_LOCK); }
        let mut builder = ViewportBuilder::default()
            .with_title(format!("{} — Preview", e.name))
            .with_app_id("ferrous-commander-preview")
            .with_decorations(false)
            .with_inner_size(quick.size)
            // A window of one fixed size is what makes a tiling compositor float it.
            .with_min_inner_size(if locked { quick.size } else { vec2(320.0, 240.0) })
            .with_max_inner_size(if locked { quick.size } else { quick.limit });
        if let Some(position) = quick.position { builder = builder.with_position(position); }
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
                })),
                opened: Instant::now(),
                size,
                limit,
                position,
                shown: None,
                revision: 0,
                path: None,
            }
        });
        let mut s = quick.shared.lock().unwrap();
        let mut changed = false;
        if quick.path.as_ref() != Some(&e.path) {
            quick.path = Some(e.path.clone());
            s.name = e.name.clone();
            s.kind = e.kind;
            s.ext = e.ext.clone();
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
        if changed { ctx.request_repaint_of(viewport()); }
    }
}

impl Shared {
    fn send(&mut self, command: Cmd, ctx: &egui::Context) {
        self.commands.push(command);
        ctx.request_repaint_of(ViewportId::ROOT);
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
        match key {
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
            K::Num0 => { s.view.zoom = 1.0; s.view.pan = Vec2::ZERO; }
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
    view.zoom = (view.zoom * factor).clamp(1.0, MAX_ZOOM);
    if view.zoom <= 1.0 { view.pan = Vec2::ZERO; }
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
    ui.painter().rect_filled(body, CornerRadius::ZERO, blend(pal.window, pal.inactive, 0.5));
    let t = (s.switched.elapsed().as_secs_f32() / SWITCH_SECONDS).min(1.0);
    if t < 1.0 { ctx.request_repaint(); } else { s.previous = None; }
    if let Some(PreviewContent::Image(tex, orig, _) | PreviewContent::Page { texture: tex, orig, .. }) = &s.previous {
        let paged = matches!(s.previous, Some(PreviewContent::Page { .. }));
        let r = fitted_rect(body, tex, *orig, 1.0, paged, ctx.pixels_per_point());
        ui.painter_at(body).image(tex.id(), r, unit_uv(), Color32::WHITE.gamma_multiply(1.0 - t));
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(body));
    child.set_clip_rect(body);
    child.multiply_opacity(t);
    let content = s.content.clone();
    match &content {
        PreviewContent::Text(doc) => text_view(&mut child, s, doc, body),
        PreviewContent::Image(tex, orig, alpha) => {
            let sharpen = tex.size()[0] < orig[0] as usize;
            image_view(&mut child, s, tex, *orig, *alpha, body, sharpen, false);
        }
        PreviewContent::Page { texture, orig, .. } => image_view(&mut child, s, texture, *orig, false, body, true, true),
        PreviewContent::Animation(frames, orig) => {
            let tex = playing_frame(s, &ctx, frames);
            image_view(&mut child, s, tex, *orig, true, body, false, false);
        }
        PreviewContent::Doc(doc) => match (&doc.source, s.view.source) {
            (Some(source), true) => text_view(&mut child, s, source, body),
            _ => doc_view(&mut child, s, doc, body),
        },
        PreviewContent::Hex(bytes, total) => hex_view(&mut child, s, bytes, *total, body),
        PreviewContent::Table(table) => table_view(&mut child, s, table, body),
        PreviewContent::Archive(entries, total, trimmed) => archive_view(&mut child, s, entries, *total, *trimmed, body),
        PreviewContent::Info(rows, cover, _) => {
            let picture = cover.as_ref().map(|(frames, size)| (playing_frame(s, &ctx, frames), *size));
            info_view(&child, s, rows, picture, body);
        }
        PreviewContent::Folder { folders, files, size } => {
            let text = format!("{folders} folders, {files} files\n{}", fsx::fmt_size(*size, s.mebibytes));
            card(&child, s, body, &text);
        }
        PreviewContent::Error(message) => card(&child, s, body, message),
        PreviewContent::Detail(_) | PreviewContent::None | PreviewContent::Loading => {}
    }
    if let Some(since) = s.loading {
        if since.elapsed() >= SPINNER_AFTER {
            let r = Rect::from_center_size(body.center(), Vec2::splat(28.0));
            ui.put(r, egui::Spinner::new().size(28.0));
        } else {
            ctx.request_repaint_after(SPINNER_AFTER);
        }
    }
    let zoomable = matches!(content, PreviewContent::Image(..) | PreviewContent::Page { .. } | PreviewContent::Animation(..));
    let pages = pages_of(&content);
    control_bar(ui, s, body, &ctx, zoomable, pages);
    if cfg!(windows) { ui.painter().rect_stroke(full, CornerRadius::ZERO, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside); }
}

fn title_bar(ui: &mut Ui, s: &mut Shared, r: Rect, embedded: bool) {
    let pal = s.pal;
    let response = ui.interact(r, ui.scope_id().with("preview_title"), Sense::click_and_drag());
    if response.drag_started() && !embedded { ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag); }
    let p = ui.painter_at(r);
    let icon = Rect::from_center_size(pos2(r.min.x + 22.0, r.center().y), Vec2::splat(20.0));
    if s.is_dir { icons::folder(&p, icon, s.folder); } else { icons::entry(&p, icon, s.kind, &s.ext, pal.dark, false); }
    let close = Rect::from_center_size(pos2(r.max.x - 22.0, r.center().y), Vec2::splat(26.0));
    let hit = ui.interact(close, ui.scope_id().with("preview_close"), Sense::click());
    if hit.hovered() { p.rect_filled(close, CornerRadius::same(4), pal.hover); }
    let x = close.shrink(8.0);
    let stroke = Stroke::new(1.5, if hit.hovered() { pal.text } else { pal.text_weak });
    p.line_segment([x.left_top(), x.right_bottom()], stroke);
    p.line_segment([x.right_top(), x.left_bottom()], stroke);
    if hit.clicked() { s.send(Cmd::Close, ui.ctx()); }
    let small = FontId::proportional(12.0);
    let info = if s.size.is_empty() { format!("{} / {}", s.place.0, s.place.1) } else { format!("{}   ·   {} / {}", s.size, s.place.0, s.place.1) };
    let info_rect = p.text(pos2(close.min.x - 10.0, r.center().y), Align2::RIGHT_CENTER, info, small, pal.text_weak);
    let font = FontId::proportional(14.0);
    let name = fit_text(&p, &s.name, &font, (info_rect.min.x - r.min.x - 60.0).max(40.0));
    p.text(pos2(r.min.x + 40.0, r.center().y), Align2::LEFT_CENTER, name, font, pal.text);
}

fn unit_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

/// Fits inside the frame; a small picture stays at its own size instead of being blown up.
fn image_rect(frame: Rect, orig: [u32; 2], zoom: f32, pan: Vec2, ppp: f32) -> Rect {
    let size = vec2(orig[0].max(1) as f32, orig[1].max(1) as f32);
    let area = frame.shrink(12.0);
    let fit = (area.width() / size.x).min(area.height() / size.y).min(1.0 / ppp.max(0.1));
    Rect::from_center_size(frame.center() + pan, size * fit * zoom)
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
    if paged || tex.size()[0] > orig[0] as usize { page_rect(frame, orig, zoom) } else { image_rect(frame, orig, zoom, Vec2::ZERO, ppp) }
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
    let Some(spans) = doc.spans.get(line as usize) else {
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
                BlockKind::Quote => { p.rect_filled(Rect::from_min_size(pos - vec2(14.0, 0.0), vec2(3.0, size.y)), CornerRadius::same(1), pal.raised); }
                BlockKind::Code => { p.rect_filled(Rect::from_min_size(pos - vec2(8.0, 6.0), vec2(width - at.x, size.y + 12.0)), CornerRadius::same(4), blend(pal.raised, pal.window, 0.45)); }
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
                icons::entry(&p, icon, Kind::from_ext(&ext), &ext, pal.dark, false);
            }
            let size = if entry.folder { String::new() } else { fsx::fmt_size(entry.size, s.mebibytes) };
            let size_rect = p.text(pos2(top.x + width - 16.0, y), Align2::RIGHT_CENTER, size, FontId::proportional(12.0), pal.text_weak);
            let name = fit_text(&p, &entry.name, &font, (size_rect.min.x - top.x - 60.0).max(40.0));
            p.text(pos2(top.x + 44.0, y), Align2::LEFT_CENTER, name, font.clone(), pal.text);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
}

fn info_view(ui: &Ui, s: &Shared, rows: &[(String, String)], cover: Option<(&egui::TextureHandle, [u32; 2])>, body: Rect) {
    let pal = s.pal;
    let p = ui.painter_at(body);
    let (row_h, table_h) = (24.0, rows.len() as f32 * 24.0);
    let area = body.shrink(28.0);
    let picture_h = (area.height() - table_h - 24.0).max(64.0);
    let top = if let Some((tex, size)) = cover {
        let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
        let h = picture_h.min(area.width() / aspect);
        let total = h + 24.0 + table_h;
        let at = body.center().y - total / 2.0;
        p.image(tex.id(), Rect::from_center_size(pos2(body.center().x, at + h / 2.0), vec2(h * aspect, h)), unit_uv(), Color32::WHITE);
        at + h + 24.0
    } else {
        let side = 96.0;
        let total = side + 24.0 + table_h;
        let at = body.center().y - total / 2.0;
        icons::entry(&p, Rect::from_center_size(pos2(body.center().x, at + side / 2.0), Vec2::splat(side)), s.kind, &s.ext, pal.dark, true);
        at + side + 24.0
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
            p.text(pos2(top.x + 520.0, y), Align2::LEFT_CENTER, ascii, font.clone(), pal.text_weak);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
}

fn control_bar(ui: &mut Ui, s: &mut Shared, body: Rect, ctx: &egui::Context, image: bool, pages: Option<(u32, u32)>) {
    let moved = ui.input(|i| i.pointer.time_since_last_movement() < 2.0 && i.pointer.has_pointer());
    let opacity = ctx.animate_bool_with_time(ui.scope_id().with("preview_bar"), moved, 0.12);
    if moved { ctx.request_repaint_after(Duration::from_millis(2100)); }
    if opacity <= 0.0 { return; }
    let pal = s.pal;
    let width = 170.0 + if image { 130.0 } else { 0.0 } + if pages.is_some() { 110.0 } else { 0.0 };
    let r = Rect::from_center_size(pos2(body.center().x, body.max.y - 30.0), vec2(width, 36.0));
    let mut bar = ui.new_child(UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    bar.multiply_opacity(opacity);
    bar.painter().rect_filled(r, CornerRadius::same(8), blend(pal.raised, pal.window, 0.2));
    bar.add_space(8.0);
    let flat = |text: &str| egui::Button::new(egui::RichText::new(text).size(15.0)).frame(false);
    if bar.add(flat("‹")).on_hover_text("Previous file").clicked() { s.send(Cmd::Step(-1), ctx); }
    if bar.add(flat("›")).on_hover_text("Next file").clicked() { s.send(Cmd::Step(1), ctx); }
    if let Some((page, total)) = pages {
        bar.add_space(10.0);
        if bar.add(flat("‹")).on_hover_text("Previous page").clicked() { s.send(Cmd::Page(-1), ctx); }
        bar.label(egui::RichText::new(format!("{} / {total}", page + 1)).size(12.0).color(pal.text_weak));
        if bar.add(flat("›")).on_hover_text("Next page").clicked() { s.send(Cmd::Page(1), ctx); }
    }
    if image {
        bar.add_space(10.0);
        if bar.add(flat("−")).clicked() { zoom_by(&mut s.view, 0.8); }
        bar.label(egui::RichText::new(format!("{:.0}%", s.view.zoom * 100.0)).size(12.0).color(pal.text_weak));
        if bar.add(flat("+")).clicked() { zoom_by(&mut s.view, 1.25); }
    }
    bar.add_space(10.0);
    if bar.add(flat("Open")).clicked() { s.send(Cmd::Open, ctx); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(), lname: name.into(), ext: "bin".into(), path: PathBuf::from(name), is_dir: false, size: 1,
            modified: None, created: None, hidden: false, readonly: false, kind: Kind::Other,
        }
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

    #[test]
    fn a_page_keeps_its_size_while_fading_to_the_next() {
        let ctx = egui::Context::default();
        let PreviewContent::Page { texture, .. } = page(&ctx, 0, 2) else { unreachable!() };
        let frame = Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0));
        let r = fitted_rect(frame, &texture, [595, 842], 1.0, true, 1.0);
        assert!(r.height() > 900.0, "a page fills the frame instead of using its size in points");
    }
}
