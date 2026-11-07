//! Small vector glyphs drawn with the painter, so the app ships no icon files.

use egui::{Color32, CornerRadius, Painter, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::fsx::Kind;
use crate::theme::blend;

pub fn folder(p: &Painter, r: Rect, base: Color32) {
    let s = r.width().min(r.height());
    let r = Rect::from_center_size(r.center(), vec2(s, s * 0.82));
    let back = blend(base, Color32::BLACK, 0.18);
    let tab = Rect::from_min_size(r.min, vec2(s * 0.45, s * 0.25));
    let rad = (s * 0.12).clamp(1.0, 6.0) as u8;
    p.rect_filled(tab, CornerRadius::same(rad), back);
    let body_back = Rect::from_min_max(pos2(r.min.x, r.min.y + s * 0.12), r.max);
    p.rect_filled(body_back, CornerRadius::same(rad), back);
    let front = Rect::from_min_max(pos2(r.min.x, r.min.y + s * 0.26), r.max);
    p.rect_filled(front, CornerRadius::same(rad), base);
}

pub fn file(p: &Painter, r: Rect, kind: Kind, page: Color32, ext: &str, big: bool) {
    let h = r.height().min(r.width() * 1.25);
    let w = h * 0.78;
    let r = Rect::from_center_size(r.center(), vec2(w, h));
    let fold = w * 0.3;
    let rad = (h * 0.08).clamp(1.0, 5.0);
    let pts = vec![
        r.left_top(),
        pos2(r.right() - fold, r.top()),
        pos2(r.right(), r.top() + fold),
        r.right_bottom(),
        r.left_bottom(),
    ];
    let _ = rad;
    p.add(egui::Shape::convex_polygon(pts, page, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(
        vec![pos2(r.right() - fold, r.top()), pos2(r.right(), r.top() + fold), pos2(r.right() - fold, r.top() + fold)],
        blend(page, Color32::BLACK, 0.25),
        Stroke::NONE,
    ));
    let band = Rect::from_min_max(pos2(r.left(), r.bottom() - h * 0.36), r.right_bottom());
    p.rect_filled(band, CornerRadius::ZERO, kind.color());
    if big && !ext.is_empty() {
        let e: String = ext.chars().take(4).collect::<String>().to_uppercase();
        p.text(
            band.center(),
            egui::Align2::CENTER_CENTER,
            e,
            egui::FontId::proportional((h * 0.16).max(8.0)),
            Color32::from_rgb(0x1a, 0x1a, 0x1e),
        );
    }
}

pub fn entry(p: &Painter, r: Rect, kind: Kind, ext: &str, dark: bool, big: bool) {
    if kind == Kind::Folder {
        folder(p, r, Kind::Folder.color());
    } else {
        let page = if dark { Color32::from_rgb(0xE8, 0xE6, 0xEA) } else { Color32::from_rgb(0xFF, 0xFF, 0xFF) };
        if !dark {
            let h = r.height().min(r.width() * 1.25);
            let rr = Rect::from_center_size(r.center(), vec2(h * 0.78, h));
            p.rect_stroke(rr, CornerRadius::ZERO, Stroke::new(1.0, Color32::from_gray(0xC0)), StrokeKind::Outside);
        }
        file(p, r, kind, page, ext, big);
    }
}

/// The eject symbol: a triangle over a bar.
pub fn eject(p: &Painter, r: Rect, color: Color32) {
    let (c, w) = (r.center(), r.width().min(r.height()) * 0.42);
    let tip = pos2(c.x, c.y - w);
    let base = c.y + w * 0.25;
    p.add(egui::Shape::convex_polygon(vec![tip, pos2(c.x + w, base), pos2(c.x - w, base)], color, egui::Stroke::NONE));
    p.rect_filled(Rect::from_min_max(pos2(c.x - w, base + w * 0.3), pos2(c.x + w, base + w * 0.6)), 0.0, color);
}

pub fn drive(p: &Painter, r: Rect, color: Color32) {
    let s = r.width().min(r.height());
    let body = Rect::from_center_size(r.center(), vec2(s, s * 0.55));
    p.rect_filled(body, CornerRadius::same(2), color);
    p.circle_filled(pos2(body.right() - s * 0.18, body.center().y), s * 0.07, Color32::from_rgb(0x88, 0xC8, 0x66));
}

/// App icon (64×64 RGBA): a rounded gradient square holding a folder.
pub fn app_icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let inside = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, rad: f32| {
        let cx = x.clamp(x0 + rad, x1 - rad);
        let cy = y.clamp(y0 + rad, y1 - rad);
        (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad
    };
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let i = (y * n + x) * 4;
            if inside(fx, fy, 2.0, 2.0, 62.0, 62.0, 14.0) {
                let t = (fx + fy) / 128.0;
                let (r, g, b) = (
                    (0x00 as f32 * (1.0 - t) + 0x5E as f32 * t) as u8,
                    (0x78 as f32 * (1.0 - t) + 0x3A as f32 * t) as u8,
                    (0xD7 as f32 * (1.0 - t) + 0xC8 as f32 * t) as u8,
                );
                rgba[i..i + 4].copy_from_slice(&[r, g, b, 255]);
                let folder_back = inside(fx, fy, 14.0, 18.0, 34.0, 28.0, 3.0) || inside(fx, fy, 14.0, 22.0, 50.0, 46.0, 3.0);
                let folder_front = inside(fx, fy, 14.0, 28.0, 50.0, 46.0, 3.0);
                if folder_front {
                    rgba[i..i + 4].copy_from_slice(&[0xF2, 0xC2, 0x4F, 255]);
                } else if folder_back {
                    rgba[i..i + 4].copy_from_slice(&[0xC9, 0x9A, 0x30, 255]);
                }
            }
        }
    }
    egui::IconData { rgba, width: n as u32, height: n as u32 }
}

/// Toolbar glyphs the bundled fonts don't have.
pub fn sort_arrow(p: &Painter, center: egui::Pos2, ascending: bool, c: Color32) {
    let direction = if ascending { -1.0 } else { 1.0 };
    let tip = center + vec2(0.0, 3.0 * direction);
    let tail = center - vec2(0.0, 3.0 * direction);
    let stroke = Stroke::new(1.2, c);
    p.line_segment([tail, tip], stroke);
    for dx in [-2.5, 2.5] {
        p.line_segment([tip, tip + vec2(dx, -2.5 * direction)], stroke);
    }
}

pub fn glyph(p: &Painter, r: Rect, name: &str, c: Color32) {
    let s = Stroke::new(1.3, c);
    let b = Rect::from_center_size(r.center(), vec2(14.0, 12.0));
    match name {
        // Columns layout: three columns
        "layout" => {
            p.rect_stroke(b, CornerRadius::same(2), s, StrokeKind::Inside);
            for k in 1..3 {
                let x = b.min.x + b.width() * k as f32 / 3.0;
                p.line_segment([pos2(x, b.min.y), pos2(x, b.max.y)], s);
            }
        }
        "tree" => {
            let x = b.min.x + 2.0;
            p.line_segment([pos2(x, b.min.y), pos2(x, b.max.y - 1.0)], s);
            for (y, end) in [(b.min.y + 1.0, b.max.x - 3.0), (b.center().y, b.max.x), (b.max.y - 1.0, b.max.x)] {
                p.line_segment([pos2(x, y), pos2(end, y)], s);
                p.rect_filled(Rect::from_center_size(pos2(end, y), vec2(3.0, 3.0)), CornerRadius::ZERO, c);
            }
        }
        // Dual browsers: two panes
        "dual" => {
            let l = Rect::from_min_max(b.min, pos2(b.center().x - 1.5, b.max.y));
            let rr = Rect::from_min_max(pos2(b.center().x + 1.5, b.min.y), b.max);
            p.rect_stroke(l, CornerRadius::same(2), s, StrokeKind::Inside);
            p.rect_stroke(rr, CornerRadius::same(2), s, StrokeKind::Inside);
        }
        // Flip split direction: a pane cut top/bottom with a turn arrow
        "flip" => {
            p.rect_stroke(b, CornerRadius::same(2), s, StrokeKind::Inside);
            p.line_segment([pos2(b.min.x, b.center().y), pos2(b.max.x, b.center().y)], Stroke::new(1.0, c));
            p.line_segment([pos2(b.center().x, b.min.y), pos2(b.center().x, b.max.y)], Stroke::new(1.0, c.gamma_multiply(0.45)));
        }
        // Drag handle: two columns of dots
        "grip" => {
            for x in [-2.5, 2.5] {
                for y in [-4.0, 0.0, 4.0] {
                    p.circle_filled(r.center() + vec2(x, y), 1.2, c);
                }
            }
        }
        // Network drive: a screen with a line to the network
        "network" => {
            let scr = Rect::from_min_max(pos2(b.min.x + 1.0, b.min.y), pos2(b.max.x - 1.0, b.min.y + 8.0));
            p.rect_stroke(scr, CornerRadius::same(1), s, StrokeKind::Inside);
            p.line_segment([pos2(scr.center().x, scr.max.y), pos2(scr.center().x, b.max.y)], s);
            p.line_segment([pos2(b.min.x, b.max.y), pos2(b.max.x, b.max.y)], s);
        }
        _ => {}
    }
}
