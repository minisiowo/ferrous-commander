//! PDF pages, drawn one at a time.

use std::path::Path;

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask, interpret_page};
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{Affine, BezPath, Point, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use super::load::{Loaded, Options, Picture, Reply, failed};
use super::select::PageText;
use crate::fsx;

const MAX_FILE: u64 = 512 * 1024 * 1024;
/// A page with more text than this is a table of numbers or worse; it is shown without it.
const MAX_GLYPHS: usize = 100_000;
const MAX_TEXT: usize = 1024 * 1024;

fn to_picture(pixmap: Pixmap) -> Picture {
    let size = [pixmap.width() as u32, pixmap.height() as u32];
    // Drawn over a white page, so nothing is see-through and no alpha needs undoing.
    Picture { size, delays: Vec::new(), rgba: pixmap.take_rgba8(ImageAlphaType::AlphaPremultiplied) }
}

pub fn load(p: &Path, options: Options) -> Reply {
    if std::fs::metadata(p).is_ok_and(|m| m.len() > MAX_FILE) { return failed("This PDF is too large to preview"); }
    let data = match std::fs::read(p) {
        Ok(data) => data,
        Err(e) => return failed(fsx::friendly_err(&e)),
    };
    let pdf = match Pdf::new(data) {
        Ok(pdf) => pdf,
        Err(e) if format!("{e:?}").to_lowercase().contains("encrypt") => return failed("This PDF is protected with a password"),
        Err(_) => return failed("This PDF could not be read"),
    };
    let pages = pdf.pages();
    let total = pages.len() as u32;
    if total == 0 { return failed("This PDF has no pages"); }
    let number = options.page.min(total - 1);
    let page = &pages[number as usize];
    let (w, h) = page.render_dimensions();
    if !(w > 0.0 && h > 0.0) { return failed("This PDF page is empty"); }
    let cache = RenderCache::new();
    let side = options.max_side as f32;
    let (interpret, render) = (InterpreterSettings::default(), RenderSettings::default());
    let Some([l, t, r, b]) = options.crop else {
        let scale = (side / w).min(side / h);
        let settings = PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
        let pixmap = hayro::render(page, &cache, &interpret, &render, &settings);
        let text = page_text(page, w, h);
        return (Loaded::Page { orig: [w.round() as u32, h.round() as u32], page: number, pages: total, text }, Some(to_picture(pixmap)));
    };
    let along = |v: u16, of: f32| v as f32 / 65535.0 * of;
    let (x, y, cw, ch) = (along(l, w), along(t, h), along(r, w) - along(l, w), along(b, h) - along(t, h));
    if !(cw > 0.0 && ch > 0.0) { return failed("Nothing to draw"); }
    let scale = (side / cw).min(side / ch);
    let mut ctx = RenderContext::new(((cw * scale).round() as u16).max(1), ((ch * scale).round() as u16).max(1));
    let transform = Affine::scale(scale as f64) * Affine::translate((-x as f64, -y as f64)) * page.initial_transform(true).to_kurbo();
    hayro::render_into(page, &cache, &interpret, &render, &mut ctx, transform);
    ctx.flush();
    let mut pixmap = Pixmap::new(ctx.width(), ctx.height());
    ctx.render_with(&mut pixmap, &mut Resources::default(), RasterizerSettings { target_init: TargetInit::Clear(WHITE), ..Default::default() });
    (Loaded::Detail, Some(to_picture(pixmap)))
}

/// The text of a page in the order it is drawn, with where each character is.
fn page_text(page: &Page<'_>, w: f32, h: f32) -> Option<PageText> {
    let cache = InterpreterCache::new();
    let bbox = Rect::new(0.0, 0.0, w as f64, h as f64);
    let mut context = Context::new(page.initial_transform(true).to_kurbo(), bbox, &cache, page.xref(), InterpreterSettings::default());
    let mut collector = TextCollector { size: (w as f64, h as f64), ..Default::default() };
    interpret_page(page, &mut context, &mut collector);
    (!collector.full && !collector.out.chars.is_empty()).then_some(collector.out)
}

/// Collects glyphs instead of drawing them. Invisible text counts too: that is the text
/// a scanner's recognition lays over the picture of a page.
#[derive(Default)]
struct TextCollector {
    size: (f64, f64),
    out: PageText,
    /// Where the last glyph's baseline started and ended, and its size.
    last: Option<(Point, Point, f64)>,
    full: bool,
}

impl TextCollector {
    fn add(&mut self, text: &str, transform: Affine, advance: f64) {
        if self.full || text.is_empty() { return; }
        let origin = transform * Point::ZERO;
        let end = transform * Point::new(advance, 0.0);
        let em = (transform * Point::new(0.0, 1000.0) - origin).hypot();
        if !(em.is_finite() && em > 0.0) { return; }
        let blank = text.chars().all(char::is_whitespace);
        if let Some((last_origin, last_end, last_em)) = self.last {
            let size = em.max(last_em);
            let ends_blank = self.out.text.ends_with(char::is_whitespace);
            if (origin.y - last_origin.y).abs() > size * 0.5 || origin.x < last_origin.x - size {
                self.out.text.truncate(self.out.text.trim_end_matches([' ', '\t']).len());
                if !self.out.text.ends_with('\n') { self.out.text.push('\n'); }
            } else if origin.x - last_end.x > size * 0.25 && !ends_blank && !blank {
                self.out.text.push(' ');
            }
        }
        // A glyph box from below the baseline to above the letters.
        let corners = [Point::new(0.0, -220.0), Point::new(advance, -220.0), Point::new(0.0, 880.0), Point::new(advance, 880.0)].map(|p| transform * p);
        let (mut l, mut t, mut r, mut b) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in corners {
            (l, t, r, b) = (l.min(p.x), t.min(p.y), r.max(p.x), b.max(p.y));
        }
        let (w, h) = self.size;
        let part = [l / w, t / h, r / w, b / h].map(|v| v.clamp(0.0, 1.0) as f32);
        self.out.chars.push((self.out.text.len() as u32, part));
        self.out.text.push_str(text);
        self.last = Some((origin, end, em));
        if self.out.chars.len() > MAX_GLYPHS || self.out.text.len() > MAX_TEXT { self.full = true; }
    }
}

impl<'a> Device<'a> for TextCollector {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for glyph in run.glyphs() {
            let text = match glyph.as_unicode() {
                Some(BfString::Char(c)) if !c.is_control() => c.to_string(),
                Some(BfString::String(s)) => s.chars().filter(|c| !c.is_control()).collect(),
                _ => continue,
            };
            let advance = match &**glyph {
                Glyph::Outline(outline) => outline.advance_width(),
                Glyph::Type3(_) => None,
            };
            let advance = advance.filter(|a| a.is_finite() && *a > 0.0).unwrap_or(500.0) as f64;
            self.add(&text, props.transform * glyph.transform(), advance);
        }
    }
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::super::load::{scratch, whole};
    use super::*;

    /// Two 200×100 pages; the second is filled red. Written by hand so the offsets are exact.
    fn two_pages() -> Vec<u8> {
        let contents = "1 0 0 rg 0 0 200 100 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 5 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{contents}\nendstream", contents.len()),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets { out.extend(format!("{offset:010} 00000 n \n").bytes()); }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
        out
    }

    #[test]
    fn pages_render_on_demand() {
        let root = scratch("pdf");
        let file = root.join("doc.pdf");
        std::fs::write(&file, two_pages()).unwrap();
        let (loaded, first) = load(&file, whole(400));
        assert!(matches!(loaded, Loaded::Page { orig: [200, 100], page: 0, pages: 2, text: None }), "no text on these pages");
        let first = first.unwrap();
        assert_eq!(first.size, [400, 200]);
        assert_eq!(&first.rgba[..4], &[255, 255, 255, 255], "an empty page is white");
        let second = Options { max_side: 400, page: 1, crop: None, view: None };
        let (loaded, pixels) = load(&file, second);
        assert!(matches!(loaded, Loaded::Page { page: 1, .. }));
        assert_eq!(&pixels.unwrap().rgba[..4], &[255, 0, 0, 255]);
        let past_the_end = Options { max_side: 400, page: 99, crop: None, view: None };
        assert!(matches!(load(&file, past_the_end).0, Loaded::Page { page: 1, .. }));
        let part = Options { max_side: 64, page: 1, crop: Some([0, 0, 32768, 65535]), view: None };
        let (loaded, pixels) = load(&file, part);
        assert!(matches!(loaded, Loaded::Detail));
        assert_eq!(pixels.unwrap().size[0], 64);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// One page with two lines of Helvetica, written by hand like `two_pages`.
    fn words() -> Vec<u8> {
        let contents = "BT /F1 12 Tf 10 70 Td (Hello world) Tj 0 -20 Td (Zweite) Tj ET 3 Tr BT /F1 10 Tf 10 20 Td (hidden) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>
stream
{contents}
endstream", contents.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4
".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj
{object}
endobj
", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref
0 {}
0000000000 65535 f 
", objects.len() + 1).bytes());
        for offset in offsets { out.extend(format!("{offset:010} 00000 n 
").bytes()); }
        out.extend(format!("trailer
<< /Size {} /Root 1 0 R >>
startxref
{xref}
%%EOF
", objects.len() + 1).bytes());
        out
    }

    #[test]
    fn the_text_of_a_page_comes_with_where_it_is() {
        let root = scratch("pdf_text");
        let file = root.join("words.pdf");
        std::fs::write(&file, words()).unwrap();
        let (Loaded::Page { text: Some(text), .. }, _) = load(&file, whole(400)) else { panic!("no text") };
        assert_eq!(text.text, "Hello world
Zweite
hidden", "invisible text counts, as scanners lay it");
        assert_eq!(text.chars.len(), "Helloworld".len() + 1 + "Zweite".len() + "hidden".len(), "the space is drawn, the line ends are not");
        let first = text.chars[0].1;
        let last_of_line = text.chars[10].1;
        assert!(first.iter().all(|v| (0.0..=1.0).contains(v)), "{first:?}");
        assert!(first[0] > 0.04 && first[0] < 0.06, "H starts 10 points in: {first:?}");
        assert!(last_of_line[0] > first[2], "letters go right");
        assert!(first[3] > 0.25 && first[1] < 0.35, "the first line is 30 points from the top: {first:?}");
        assert!(text.chars[11].1[1] > first[3] - 0.05, "the second line is lower");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_pdfs_are_reported() {
        let root = scratch("pdf_broken");
        let file = root.join("broken.pdf");
        std::fs::write(&file, b"%PDF-1.4\ngarbage").unwrap();
        assert!(matches!(load(&file, whole(100)).0, Loaded::Error(_)));
        let whole_file = two_pages();
        std::fs::write(&file, &whole_file[..whole_file.len() / 2]).unwrap();
        let _ = load(&file, whole(100));
        std::fs::remove_dir_all(root).unwrap();
    }
}
