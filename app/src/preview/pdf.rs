//! PDF pages, drawn one at a time.

use std::path::Path;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_syntax::Pdf;
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{PixmapSettings, RenderCache, RenderSettings};

use super::load::{Loaded, Options, Picture, Reply, failed};
use crate::fsx;

const MAX_FILE: u64 = 512 * 1024 * 1024;

fn to_picture(pixmap: Pixmap) -> Picture {
    let size = [pixmap.width() as u32, pixmap.height() as u32];
    // Drawn over a white page, so nothing is see-through and no alpha needs undoing.
    Picture { size, delays: Vec::new(), rgba: pixmap.data_as_u8_slice().to_vec() }
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
        return (Loaded::Page { orig: [w.round() as u32, h.round() as u32], page: number, pages: total }, Some(to_picture(pixmap)));
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
        assert!(matches!(loaded, Loaded::Page { orig: [200, 100], page: 0, pages: 2 }));
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
