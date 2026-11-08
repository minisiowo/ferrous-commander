//! Raster pictures, animations and SVG.

use std::io::BufReader;
use std::path::Path;

use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use super::load::{Loaded, Options, Picture, Reply, failed};
use crate::fsx;

const MAX_FILE: u64 = 200_000_000;
const MAX_FRAMES: usize = 240;
const MAX_ANIMATION_BYTES: usize = 96 * 1024 * 1024;
const MAX_FRAME_SIDE: u32 = 720;

/// Strong signatures count on their own; shorter ones ("BM" starts plenty of text) need the name.
pub fn is_picture(head: &[u8], ext: &str) -> bool {
    let strong = matches!(image::guess_format(head), Ok(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP));
    strong || fsx::IMAGE_EXTS.contains(&ext)
}

pub(super) fn to_picture(img: DynamicImage, max_side: u32) -> Picture {
    let small = if img.width() > max_side || img.height() > max_side { img.thumbnail(max_side, max_side) } else { img };
    let rgba = small.into_rgba8();
    Picture { size: [rgba.width(), rgba.height()], delays: Vec::new(), rgba: rgba.into_raw() }
}

fn decode(p: &Path) -> Result<DynamicImage, String> {
    let mut decoder = ImageReader::open(p).map_err(|e| e.to_string())?
        .with_guessed_format().map_err(|e| e.to_string())?
        .into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(orientation) = orientation { img.apply_orientation(orientation); }
    Ok(img)
}

/// `None` when the file is not an animation, or too long to keep in memory as one.
fn animation(p: &Path, max_side: u32) -> Option<(Picture, [u32; 2])> {
    let reader = || std::fs::File::open(p).ok().map(BufReader::new);
    let format = ImageReader::open(p).ok()?.with_guessed_format().ok()?.format()?;
    let frames = match format {
        ImageFormat::Gif => image::codecs::gif::GifDecoder::new(reader()?).ok()?.into_frames(),
        ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(reader()?).ok()?;
            if !decoder.has_animation() { return None; }
            decoder.into_frames()
        }
        _ => return None,
    };
    let side = max_side.min(MAX_FRAME_SIDE);
    let (mut rgba, mut delays, mut size, mut orig) = (Vec::new(), Vec::new(), [0, 0], [0, 0]);
    for frame in frames.take(MAX_FRAMES + 1) {
        let frame = frame.ok()?;
        if delays.len() == MAX_FRAMES { return None; }
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        delays.push((numerator / denominator.max(1)).max(20));
        let buffer = frame.into_buffer();
        orig = [buffer.width(), buffer.height()];
        let img = to_picture(DynamicImage::ImageRgba8(buffer), side);
        if delays.len() > 1 && img.size != size { return None; }
        size = img.size;
        rgba.extend(img.rgba);
        if rgba.len() > MAX_ANIMATION_BYTES { return None; }
    }
    (delays.len() > 1).then_some((Picture { size, delays, rgba }, orig))
}

pub fn load(p: &Path, options: Options) -> Reply {
    if std::fs::metadata(p).is_ok_and(|m| m.len() >= MAX_FILE) { return failed("This picture is too large to preview"); }
    if options.crop.is_none() && let Some((picture, orig)) = animation(p, options.max_side) {
        return (Loaded::Image(orig), Some(picture));
    }
    let img = match decode(p) {
        Ok(img) => img,
        Err(e) => return failed(e),
    };
    let orig = [img.width(), img.height()];
    match options.crop {
        None => (Loaded::Image(orig), Some(to_picture(img, options.max_side))),
        Some([l, t, r, b]) => {
            let at = |v: u16, of: u32| (v as u64 * of as u64 / 65535) as u32;
            let (x, y) = (at(l, orig[0]), at(t, orig[1]));
            let part = img.crop_imm(x, y, at(r, orig[0]).saturating_sub(x).max(1), at(b, orig[1]).saturating_sub(y).max(1));
            (Loaded::Detail, Some(to_picture(part, options.max_side)))
        }
    }
}

pub fn svg(p: &Path, options: Options) -> Reply {
    use resvg::{tiny_skia, usvg};
    let data = match std::fs::read(p) {
        Ok(data) => data,
        Err(e) => return failed(fsx::friendly_err(&e)),
    };
    let mut settings = usvg::Options {
        // A picture may embed images, but it has no business reading other files.
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    // Compressed SVG cannot be searched, so it is assumed to have text.
    if data.starts_with(&[0x1F, 0x8B]) || data.windows(5).any(|w| w == b"<text") {
        let fonts = settings.fontdb_mut();
        fonts.load_system_fonts();
        // Left to chance, unnamed text can land on a symbol font and come out as Greek.
        let plain = ["Noto Sans", "DejaVu Sans", "Liberation Sans", "Segoe UI", "Arial"].into_iter()
            .find(|name| fonts.faces().any(|face| face.families.iter().any(|(family, _)| family == name)));
        if let Some(family) = plain {
            fonts.set_sans_serif_family(family);
            fonts.set_serif_family(family);
            settings.font_family = family.to_string();
        }
    }
    let tree = match usvg::Tree::from_data(&data, &settings) {
        Ok(tree) => tree,
        Err(e) => return failed(format!("This SVG could not be read: {e}")),
    };
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    let scale = (options.max_side as f32 / w).min(options.max_side as f32 / h);
    let (tw, th) = (((w * scale).ceil() as u32).max(1), ((h * scale).ceil() as u32).max(1));
    let Some(mut pixmap) = tiny_skia::Pixmap::new(tw, th) else { return failed("This SVG has an impossible size") };
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let rgba = pixmap.take_demultiplied();
    (Loaded::Image([w.round() as u32, h.round() as u32]), Some(Picture { size: [tw, th], delays: Vec::new(), rgba }))
}

#[cfg(test)]
mod tests {
    use super::super::load::{load as load_file, scratch, whole};
    use super::*;

    #[test]
    fn large_pictures_shrink_and_keep_their_original_size() {
        let root = scratch("picture");
        let file = root.join("image.png");
        image::RgbaImage::new(2048, 100).save(&file).unwrap();
        let (loaded, pixels) = load(&file, whole(1024));
        assert!(matches!(loaded, Loaded::Image([2048, 100])));
        let pixels = pixels.unwrap();
        assert!(pixels.size[0] == 1024 && pixels.size[1] <= 1024);
        assert_eq!(pixels.rgba.len(), (pixels.size[0] * pixels.size[1] * 4) as usize);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_crop_returns_that_part_of_the_picture() {
        let root = scratch("crop");
        let file = root.join("half.png");
        let mut img = image::RgbaImage::new(100, 100);
        for x in 50..100 { for y in 0..100 { img.put_pixel(x, y, image::Rgba([255, 0, 0, 255])); } }
        img.save(&file).unwrap();
        let (loaded, pixels) = load(&file, Options { max_side: 64, page: 0, crop: Some([32768, 0, 65535, 65535]), view: None });
        assert!(matches!(loaded, Loaded::Detail));
        let pixels = pixels.unwrap();
        assert_eq!(pixels.size[1], 64);
        assert_eq!(&pixels.rgba[0..4], &[255, 0, 0, 255], "the right half only");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gif_animations_keep_every_frame() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame, RgbaImage};
        let root = scratch("gif");
        let file = root.join("anim.gif");
        let mut encoder = GifEncoder::new(std::fs::File::create(&file).unwrap());
        for shade in [0u8, 255] {
            let frame = Frame::from_parts(RgbaImage::from_pixel(8, 8, image::Rgba([shade, 0, 0, 255])), 0, 0, Delay::from_numer_denom_ms(100, 1));
            encoder.encode_frame(frame).unwrap();
        }
        drop(encoder);
        let (loaded, pixels) = load_file(&file, whole(64));
        assert!(matches!(loaded, Loaded::Image([8, 8])));
        let pixels = pixels.unwrap();
        assert_eq!(pixels.delays, vec![100, 100]);
        assert_eq!(pixels.rgba.len(), 8 * 8 * 4 * 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn svg_renders_and_cannot_reach_other_files() {
        let root = scratch("svg");
        let secret = root.join("secret.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255])).save(&secret).unwrap();
        let file = root.join("drawing.svg");
        std::fs::write(&file, format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="5"><rect width="5" height="5" fill="red"/><image x="5" width="5" height="5" xlink:href="{}"/></svg>"#, secret.display())).unwrap();
        let (loaded, pixels) = load_file(&file, whole(100));
        assert!(matches!(loaded, Loaded::Image([10, 5])));
        let pixels = pixels.unwrap();
        assert_eq!(pixels.size, [100, 50]);
        assert_eq!(&pixels.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(pixels.rgba[4 * 75 + 3], 0, "the referenced file stays out");
        std::fs::write(&file, "<svg").unwrap();
        assert!(matches!(load_file(&file, whole(100)).0, Loaded::Error(_)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
