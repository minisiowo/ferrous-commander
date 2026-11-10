//! The look every Ferrous window shares: a title bar, the content, and a strip of buttons.
//!
//! Inside `Panel::show`, call `body` or `scrolled_body`, then `footer` if the window has
//! buttons. Each takes its own closure, one after the other, so they can all use `self`.

use std::time::{Duration, Instant};

use egui::{Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2};

use crate::fsx::Kind;
use crate::icons;
use crate::theme::Palette;
use crate::ui::fit_text;

pub const BAR_H: f32 = 38.0;
const FOOTER_H: f32 = 44.0;
const MARGIN: i8 = 14;
/// How long a new window keeps one fixed size: that is what makes a tiling compositor float it.
const SIZE_LOCK: Duration = Duration::from_millis(520);

/// A window of its own on the desktop, remembered between frames.
#[derive(Clone, Copy)]
struct Native {
    size: Vec2,
    opened: Instant,
    position: Option<Pos2>,
    last_frame: u64,
}

#[derive(Clone, Copy)]
pub enum Icon<'a> {
    /// A symbol from the font, in its own colour or the weak text colour.
    Glyph(&'a str, Option<Color32>),
    Folder(Color32),
    /// A file icon: its kind and extension.
    Entry(Kind, &'a str),
}

/// Paints the title bar into `r`; true when its close button was clicked. An empty `info`
/// leaves the right side free. Anything else on `r`, such as dragging, is up to the caller.
pub fn title_bar(ui: &mut Ui, pal: Palette, r: Rect, icon: Icon, title: &str, info: &str) -> bool {
    let p = ui.painter_at(r);
    let slot = Rect::from_center_size(pos2(r.min.x + 22.0, r.center().y), Vec2::splat(20.0));
    match icon {
        Icon::Glyph(glyph, color) => {
            icons::symbol(&p, slot.center(), glyph, FontId::proportional(14.0), color.unwrap_or(pal.text_weak));
        }
        Icon::Folder(color) => icons::folder(&p, slot, color),
        Icon::Entry(kind, ext) => icons::entry(&p, slot, kind, ext, pal.dark, false),
    }
    let close = Rect::from_center_size(pos2(r.max.x - 22.0, r.center().y), Vec2::splat(26.0));
    let hit = ui.interact(close, ui.scope_id().with("chrome_close"), Sense::click());
    if hit.hovered() { p.rect_filled(close, CornerRadius::ZERO, pal.hover); }
    icons::cross(&p, close.center(), 5.0, Stroke::new(1.5, if hit.hovered() { pal.text } else { pal.text_weak }));
    let small = FontId::proportional(12.0);
    let room = close.min.x - 10.0 - (r.min.x + 40.0);
    let shown = fit_text(&p, info, &small, room * 0.6);
    let right = pos2(close.min.x - 10.0, r.center().y);
    let info_rect = if info.is_empty() { Rect::from_min_max(right, right) } else { p.text(right, Align2::RIGHT_CENTER, &shown, small, pal.text_weak) };
    if shown != info { ui.interact(info_rect, ui.scope_id().with("chrome_info"), Sense::hover()).on_hover_text(info); }
    let font = FontId::proportional(14.0);
    let name = fit_text(&p, title, &font, (info_rect.min.x - r.min.x - 60.0).max(40.0));
    p.text(pos2(r.min.x + 40.0, r.center().y), Align2::LEFT_CENTER, name, font, pal.text);
    hit.clicked()
}

/// A window inside the application with the shared look.
pub struct Panel<'a> {
    window: egui::Window<'a>,
    pal: Palette,
    title: &'a str,
    icon: Icon<'a>,
    info: &'a str,
    width: Option<f32>,
    /// The size to open at and the smallest allowed, when resizable.
    size: Option<(Vec2, Vec2)>,
}

impl<'a> Panel<'a> {
    pub fn new(title: &'a str, pal: Palette, icon: Icon<'a>) -> Self {
        Panel { window: egui::Window::new(title), pal, title, icon, info: "", width: None, size: None }
    }

    pub fn info(mut self, info: &'a str) -> Self {
        self.info = info;
        self
    }

    /// One width, with the height following the content.
    pub fn fixed_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self.window = self.window.resizable(false).default_width(width);
        self
    }

    /// Resizable, centred, and never larger than the application's window.
    pub fn fit_screen(mut self, ctx: &egui::Context, default: Vec2, min: Vec2) -> Self {
        let available = (ctx.content_rect().size() - vec2(48.0, 64.0)).max(vec2(300.0, 240.0));
        let size = default.min(available);
        self.window = self.window.resizable(true).default_size(size).min_size(min.min(available)).max_size(available)
            .default_pos(ctx.content_rect().center() - size * 0.5);
        self.size = Some((default, min));
        self
    }

    /// The remaining `egui::Window` settings: id, anchor, order and so on.
    pub fn window(mut self, set: impl FnOnce(egui::Window<'a>) -> egui::Window<'a>) -> Self {
        self.window = set(self.window);
        self
    }

    /// Without egui's own title bar the window moves by any empty spot, so the bar
    /// needs no drag handling of its own. True when the close button was clicked.
    pub fn show(self, ctx: &egui::Context, add: impl FnMut(&mut Ui)) -> bool {
        // A window of one width is a small, passing question: it stays with the application.
        if ctx.embed_viewports() || self.width.is_some() { self.show_inside(ctx, add) } else { self.show_native(ctx, add) }
    }

    /// As a real window, free to go anywhere on the monitor.
    fn show_native(self, ctx: &egui::Context, mut add: impl FnMut(&mut Ui)) -> bool {
        let Panel { pal, title, icon, info, size, .. } = self;
        let id = ViewportId::from_hash_of(("ferrous_panel", title));
        let key = egui::Id::unique("ferrous_panel").with(title);
        let frame = ctx.cumulative_frame_nr();
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let (default, min) = size.unwrap_or((vec2(440.0, 180.0), Vec2::ZERO));
        let mut state = ctx.data(|d| d.get_temp::<Native>(key))
            // A gap means the window was closed in between, so it opens afresh.
            .filter(|s| s.last_frame + 2 >= frame)
            .unwrap_or_else(|| {
                let size = monitor.map_or(default, |m| default.min(m - vec2(48.0, 64.0)));
                let position = ctx.input(|i| i.viewport().outer_rect).map(|r| r.center() - size * 0.5);
                Native { size, opened: Instant::now(), position, last_frame: frame }
            });
        state.last_frame = frame;
        let locked = state.opened.elapsed() < SIZE_LOCK;
        if locked { ctx.request_repaint_after(SIZE_LOCK); }
        let mut builder = ViewportBuilder::default()
            .with_title(title)
            .with_app_id("ferrous-commander-dialog")
            .with_decorations(false)
            .with_inner_size(state.size)
            .with_min_inner_size(if locked { state.size } else { min })
            .with_max_inner_size(if locked { state.size } else { monitor.unwrap_or(Vec2::splat(16_000.0)) });
        if let Some(position) = state.position { builder = builder.with_position(position); }
        let mut closed = false;
        ctx.show_viewport_immediate(id, builder, |ui, _class| {
            let full = ui.max_rect();
            ui.painter().rect_filled(full, CornerRadius::ZERO, pal.content_fill());
            if cfg!(windows) { ui.painter().rect_stroke(full, CornerRadius::ZERO, Stroke::new(1.0, pal.border), egui::StrokeKind::Inside); }
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.visuals_mut().text_edit_bg_color = Some(pal.raised);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAR_H), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::ZERO, pal.panel);
            let bar = ui.interact(r, ui.scope_id().with("panel_bar"), Sense::click_and_drag());
            if bar.drag_started() { ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag); }
            // egui can run this twice in a frame, and only the last run is seen.
            closed |= title_bar(ui, pal, r, icon, title, info);
            add(ui);
            closed |= ui.input(|i| i.viewport().close_requested());
            resize_grip(ui, full, pal);
        });
        ctx.data_mut(|d| d.insert_temp(key, state));
        closed
    }

    fn show_inside(self, ctx: &egui::Context, mut add: impl FnMut(&mut Ui)) -> bool {
        let Panel { window, pal, title, icon, info, width, .. } = self;
        let frame = Frame::window(&ctx.global_style()).inner_margin(0.0).fill(pal.content_fill());
        let mut closed = false;
        window.collapsible(false).title_bar(false).frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            // The content's shade would swallow the usual, nearly equal field colour.
            ui.visuals_mut().text_edit_bg_color = Some(pal.raised);
            if let Some(width) = width { ui.set_width(width); }
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAR_H), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::ZERO, pal.panel);
            closed = title_bar(ui, pal, r, icon, title, info);
            add(ui);
        });
        closed
    }
}

/// A window without a frame has no edges to grab, so its corner resizes it.
fn resize_grip(ui: &mut Ui, full: Rect, pal: Palette) {
    let r = Rect::from_min_max(full.max - vec2(16.0, 16.0), full.max);
    let response = ui.interact(r, ui.scope_id().with("panel_grip"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeNwSe);
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
    }
    for inset in [4.0, 8.0] {
        ui.painter().line_segment([pos2(r.max.x - inset, r.max.y - 3.0), pos2(r.max.x - 3.0, r.max.y - inset)], Stroke::new(1.0, pal.text_weak));
    }
}

/// The window's normal spacing, which the bars around the content do without.
fn spaced(ui: &mut Ui) {
    ui.spacing_mut().item_spacing = ui.ctx().global_style().spacing.item_spacing;
}

/// Content with the usual margin.
pub fn body<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::NONE.inner_margin(Margin::same(MARGIN)).show(ui, |ui| {
        spaced(ui);
        ui.set_min_width(ui.available_width());
        add(ui)
    }).inner
}

/// Content that fills the window and scrolls while the bars stay in place.
/// `footer` leaves room for a `footer` call after it.
pub fn scrolled_body<R>(ui: &mut Ui, footer: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let height = (ui.available_height() - if footer { FOOTER_H } else { 0.0 }).max(0.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(height).content_margin(Margin::same(MARGIN)).show(ui, |ui| {
        spaced(ui);
        add(ui)
    }).inner
}

/// The strip of buttons at the bottom of a window.
pub fn footer<R>(ui: &mut Ui, pal: Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), FOOTER_H), Sense::hover());
    ui.painter().rect_filled(r, CornerRadius::ZERO, pal.panel);
    let inner = r.shrink2(vec2(MARGIN as f32, 0.0));
    let mut strip = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
    spaced(&mut strip);
    add(&mut strip)
}
