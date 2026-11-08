//! The floating preview: its own window on the screen, independent of the application's.
//!
//! It runs as a deferred viewport, so scrolling a preview does not redraw the file list.
//! What it shows is handed over through `Shared`; what the user asks for comes back as `Cmd`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, FontId, Key as K, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, pos2, vec2};

use super::{Key, PreviewContent, TextDoc};
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

fn viewport() -> ViewportId {
    ViewportId::from_hash_of("ferrous_commander_preview")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cmd {
    Close,
    Step(i32),
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
    content: PreviewContent,
    previous: Option<PreviewContent>,
    switched: Instant,
    loading: Option<Instant>,
    commands: Vec<Cmd>,
    view: View,
}

pub struct Quick {
    shared: Arc<Mutex<Shared>>,
    opened: Instant,
    size: Vec2,
    limit: Vec2,
    position: Option<Pos2>,
    shown: Option<Key>,
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
        let neighbours = self.panes[i].cursor.map_or_else(Vec::new, |row| {
            [row.wrapping_sub(1), row + 1].into_iter().filter_map(|r| self.entry_at(i, r)).filter(|n| !n.is_dir).map(Key::of).collect()
        });
        self.prefetch_preview(neighbours, ctx);
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
                    content: PreviewContent::Loading,
                    previous: None,
                    switched: Instant::now(),
                    loading: None,
                    commands: Vec::new(),
                    view: View { zoom: 1.0, ..Default::default() },
                })),
                opened: Instant::now(),
                size,
                limit,
                position,
                shown: None,
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
        let waiting = matches!(self.preview.content, PreviewContent::Loading);
        if waiting {
            if s.loading.is_none() {
                s.loading = Some(Instant::now());
                ctx.request_repaint_of(viewport());
            }
        } else if quick.shown.as_ref() != self.preview.key() {
            quick.shown = self.preview.key().cloned();
            let old = std::mem::replace(&mut s.content, self.preview.content.clone());
            s.previous = Some(old);
            s.switched = Instant::now();
            s.loading = None;
            s.view = View { zoom: 1.0, wrap: s.view.wrap, ..Default::default() };
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
        let egui::Event::Key { key, pressed: true, repeat, .. } = event else { continue };
        match key {
            K::Space | K::Escape if !repeat => s.send(Cmd::Close, ctx),
            K::Enter if !repeat => s.send(Cmd::Open, ctx),
            K::ArrowDown => s.send(Cmd::Step(1), ctx),
            K::ArrowUp => s.send(Cmd::Step(-1), ctx),
            K::PageDown => s.view.scroll_to = Some(s.view.offset + page),
            K::PageUp => s.view.scroll_to = Some((s.view.offset - page).max(0.0)),
            K::Home => s.view.scroll_to = Some(0.0),
            K::End => s.view.scroll_to = Some(f32::MAX / 2.0),
            K::Plus | K::Equals => zoom_by(&mut s.view, 1.25),
            K::Minus => zoom_by(&mut s.view, 0.8),
            K::Num0 => { s.view.zoom = 1.0; s.view.pan = Vec2::ZERO; }
            K::W if !repeat => { s.view.wrap = !s.view.wrap; s.view.rows_for = (0, 0); }
            _ => {}
        }
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
    if let Some(PreviewContent::Image(tex, orig, _)) = &s.previous {
        let r = image_rect(body, *orig, 1.0, Vec2::ZERO, ctx.pixels_per_point());
        ui.painter_at(body).image(tex.id(), r, unit_uv(), Color32::WHITE.gamma_multiply(1.0 - t));
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(body));
    child.set_clip_rect(body);
    child.multiply_opacity(t);
    let content = s.content.clone();
    match &content {
        PreviewContent::Text(doc) => text_view(&mut child, s, doc, body),
        PreviewContent::Image(tex, orig, alpha) => image_view(&mut child, s, tex, *orig, *alpha, body),
        PreviewContent::Folder { folders, files, size } => {
            let text = format!("{folders} folders, {files} files\n{}", fsx::fmt_size(*size, false));
            card(&child, s, body, &text);
        }
        PreviewContent::Binary => card(&child, s, body, "No preview for this kind of file"),
        PreviewContent::Error(message) => card(&child, s, body, message),
        PreviewContent::None | PreviewContent::Loading => {}
    }
    if let Some(since) = s.loading {
        if since.elapsed() >= SPINNER_AFTER {
            let r = Rect::from_center_size(body.center(), Vec2::splat(28.0));
            ui.put(r, egui::Spinner::new().size(28.0));
        } else {
            ctx.request_repaint_after(SPINNER_AFTER);
        }
    }
    control_bar(ui, s, body, &ctx, matches!(content, PreviewContent::Image(..)));
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

fn image_view(ui: &mut Ui, s: &mut Shared, tex: &egui::TextureHandle, orig: [u32; 2], alpha: bool, body: Rect) {
    let response = ui.interact(body, ui.scope_id().with("preview_image"), Sense::click_and_drag());
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
    let ppp = ui.ctx().pixels_per_point();
    let fitted = image_rect(body, orig, s.view.zoom, Vec2::ZERO, ppp);
    let spare = ((fitted.size() - body.size()) / 2.0).max(Vec2::ZERO);
    s.view.pan = s.view.pan.clamp(-spare, spare);
    let r = fitted.translate(s.view.pan);
    if alpha { checkerboard(ui, r, body, s.pal); }
    ui.painter_at(body).image(tex.id(), r, unit_uv(), Color32::WHITE);
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
    let key = (if wrap { columns } else { 1 }, doc.text.len() + 1);
    if s.view.rows_for != key {
        s.view.rows = doc.rows(wrap.then_some(columns));
        s.view.rows_for = key;
    }
    let rows = std::mem::take(&mut s.view.rows);
    let content_w = if wrap { view_w } else { doc.longest as f32 * char_w } + text_x + 16.0;
    let mut area = egui::ScrollArea::both().id_salt("preview_text").auto_shrink(false);
    if let Some(y) = s.view.scroll_to.take() { area = area.vertical_scroll_offset(y); }
    let footer = doc.trimmed as usize;
    let total = rows.len() + footer;
    let output = area.show_rows(ui, row_h, total, |ui, range| {
        ui.set_min_width(content_w);
        let top = ui.min_rect().min;
        let n = range.len() as f32;
        let (r, _) = ui.allocate_exact_size(vec2(content_w, n * (row_h + ui.spacing().item_spacing.y) - ui.spacing().item_spacing.y), Sense::hover());
        let p = ui.painter_at(r.expand(2.0));
        let step = row_h + ui.spacing().item_spacing.y;
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
            let piece = &doc.text[start as usize..end as usize];
            p.text(pos2(top.x + text_x, y), Align2::LEFT_CENTER, piece, font.clone(), pal.text);
        }
    });
    s.view.offset = output.state.clamped_offset().y;
    s.view.rows = rows;
}

fn control_bar(ui: &mut Ui, s: &mut Shared, body: Rect, ctx: &egui::Context, image: bool) {
    let moved = ui.input(|i| i.pointer.time_since_last_movement() < 2.0 && i.pointer.has_pointer());
    let opacity = ctx.animate_bool_with_time(ui.scope_id().with("preview_bar"), moved, 0.12);
    if moved { ctx.request_repaint_after(Duration::from_millis(2100)); }
    if opacity <= 0.0 { return; }
    let pal = s.pal;
    let width = if image { 300.0 } else { 170.0 };
    let r = Rect::from_center_size(pos2(body.center().x, body.max.y - 30.0), vec2(width, 36.0));
    let mut bar = ui.new_child(UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    bar.multiply_opacity(opacity);
    bar.painter().rect_filled(r, CornerRadius::same(8), blend(pal.raised, pal.window, 0.2));
    bar.add_space(8.0);
    let flat = |text: &str| egui::Button::new(egui::RichText::new(text).size(15.0)).frame(false);
    if bar.add(flat("‹")).on_hover_text("Previous file").clicked() { s.send(Cmd::Step(-1), ctx); }
    if bar.add(flat("›")).on_hover_text("Next file").clicked() { s.send(Cmd::Step(1), ctx); }
    if image {
        bar.add_space(10.0);
        if bar.add(flat("−")).clicked() { zoom_by(&mut s.view, 0.8); }
        bar.label(egui::RichText::new(format!("{:.0}%", s.view.zoom * 100.0)).size(12.0).color(pal.text_weak));
        if bar.add(flat("+")).clicked() { zoom_by(&mut s.view, 1.25); }
    }
    bar.add_space(10.0);
    if bar.add(flat("Open")).clicked() { s.send(Cmd::Open, ctx); }
}
