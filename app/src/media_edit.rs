//! Page-level PDF edits and quarter-turns for still images.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits};
use lopdf::{Document, Object, ObjectId, dictionary};

#[derive(Clone, Copy)]
pub enum Turn {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub enum Edit {
    RotatePdf(Turn),
    ExtractPdf,
    MergePdf,
    RotateImage(Turn),
}

impl Edit {
    pub fn title(self) -> &'static str {
        match self {
            Self::RotatePdf(Turn::Left) => "Rotate PDF pages left",
            Self::RotatePdf(Turn::Right) => "Rotate PDF pages right",
            Self::ExtractPdf => "Extract PDF pages",
            Self::MergePdf => "Merge PDFs",
            Self::RotateImage(Turn::Left) => "Rotate image left",
            Self::RotateImage(Turn::Right) => "Rotate image right",
        }
    }

    pub fn takes_pages(self) -> bool {
        matches!(self, Self::RotatePdf(_) | Self::ExtractPdf)
    }

    pub fn output_suffix(self, source: &Path) -> &'static str {
        match self {
            Self::RotateImage(_) if image_format(source) == Some(ImageFormat::Png) => ".png",
            Self::RotateImage(_) if source.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("jpeg")) => ".jpeg",
            Self::RotateImage(_) => ".jpg",
            _ => ".pdf",
        }
    }

    pub fn suggested_name(self, source: &Path) -> String {
        if matches!(self, Self::MergePdf) {
            return "Merged".into();
        }
        let stem = source.file_stem().unwrap_or_default().to_string_lossy();
        let suffix = match self {
            Self::ExtractPdf => "pages",
            Self::RotatePdf(Turn::Left) | Self::RotateImage(Turn::Left) => "left",
            Self::RotatePdf(Turn::Right) | Self::RotateImage(Turn::Right) => "right",
            Self::MergePdf => unreachable!(),
        };
        format!("{stem} ({suffix})")
    }
}

pub fn output_name(name: &str, suffix: &str) -> String {
    let name = name.trim();
    if name.len() > suffix.len() && name.get(name.len() - suffix.len()..).is_some_and(|end| end.eq_ignore_ascii_case(suffix)) {
        name.into()
    } else {
        format!("{name}{suffix}")
    }
}

pub fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
}

pub fn image_format(path: &Path) -> Option<ImageFormat> {
    match ImageFormat::from_path(path).ok()? {
        format @ (ImageFormat::Png | ImageFormat::Jpeg) => Some(format),
        _ => None,
    }
}

/// Page ranges as typed, checked for form only, so the dialog can flag mistakes before the
/// document is opened. Empty parts are skipped, and en or em dashes work like hyphens.
pub fn page_ranges(input: &str) -> Result<Vec<(u32, u32)>, String> {
    let number = |text: &str| {
        text.trim().parse::<u32>().ok().filter(|&n| n > 0)
            .ok_or_else(|| "Enter page numbers such as 1, 3-5".to_string())
    };
    let mut ranges = Vec::new();
    for part in input.split(',').map(str::trim).filter(|part| !part.is_empty()) {
        let (first, last) = match part.split_once(['-', '\u{2013}', '\u{2014}']) {
            Some((first, last)) => (number(first)?, number(last)?),
            None => {
                let page = number(part)?;
                (page, page)
            }
        };
        if first > last {
            return Err(format!("Page range {first}-{last} must be in ascending order"));
        }
        ranges.push((first, last));
    }
    Ok(ranges)
}

/// Empty input selects every page. Validate the entire range before modifying a document.
fn page_numbers(input: &str, count: u32) -> Result<Vec<u32>, String> {
    if count == 0 {
        return Err("This PDF has no pages".into());
    }
    let ranges = page_ranges(input)?;
    if ranges.is_empty() {
        return Ok((1..=count).collect());
    }
    if ranges.iter().any(|&(_, last)| last > count) {
        return Err(if count == 1 {
            "This PDF has only 1 page".into()
        } else {
            format!("This PDF has {count} pages. Enter page numbers from 1 to {count}")
        });
    }
    let pages: BTreeSet<_> = ranges.into_iter().flat_map(|(first, last)| first..=last).collect();
    Ok(pages.into_iter().collect())
}

/// Prefixes an error with the file it came from.
fn about(path: &Path) -> impl Fn(String) -> String {
    let name = crate::fsx::display_name(path);
    move |e| format!("{name}: {e}")
}

fn read_pdf(path: &Path) -> Result<Document, String> {
    let doc = Document::load(path).map_err(|e| match e {
        lopdf::Error::IO(e) => crate::fsx::friendly_err(&e),
        _ => "This file is damaged or is not a PDF".into(),
    })?;
    if doc.was_encrypted() || doc.is_encrypted() {
        return Err("Editing password-protected PDFs is unavailable".into());
    }
    Ok(doc)
}

fn inherited(doc: &Document, mut id: ObjectId, key: &[u8]) -> lopdf::Result<Option<Object>> {
    let mut parents = BTreeSet::new();
    while parents.insert(id) {
        let node = doc.get_dictionary(id)?;
        if node.has(key) {
            return node.get(key).cloned().map(Some);
        }
        if !node.has(b"Parent") {
            return Ok(None);
        }
        id = node.get(b"Parent")?.as_reference()?;
    }
    Err(lopdf::Error::ReferenceCycle(id))
}

fn rotate_pdf(doc: &mut Document, pages: &str, turn: Turn) -> Result<(), String> {
    let ids = doc.get_pages();
    let angle = match turn { Turn::Left => -90, Turn::Right => 90 };
    for number in page_numbers(pages, ids.len() as u32)? {
        let id = ids[&number];
        let mut old = inherited(doc, id, b"Rotate").map_err(|e| e.to_string())?.unwrap_or(Object::Integer(0));
        if let Object::Reference(reference) = old {
            old = doc.get_object(reference).map_err(|e| e.to_string())?.clone();
        }
        let old = old.as_i64().map_err(|e| e.to_string())?;
        let angle = (old.rem_euclid(360) + angle).rem_euclid(360);
        doc.get_dictionary_mut(id).map_err(|e| e.to_string())?.set("Rotate", angle);
    }
    Ok(())
}

/// Page attributes inherited from old page trees must travel with the pages.
fn prepare_pages(doc: &mut Document, pages: &BTreeMap<u32, ObjectId>, root: ObjectId) -> lopdf::Result<()> {
    for id in pages.values().copied() {
        for key in [b"Resources".as_slice(), b"MediaBox", b"CropBox", b"Rotate"] {
            if let Some(value) = inherited(doc, id, key)? {
                doc.get_dictionary_mut(id)?.set(key, value);
            }
        }
        doc.get_dictionary_mut(id)?.set("Parent", root);
    }
    Ok(())
}

fn collect_pages(sources: &[PathBuf], pages: &str, cancel: &AtomicBool, report: &mut dyn FnMut(&Path)) -> Result<Document, String> {
    let mut output = Document::with_version("1.7");
    let root = output.new_object_id();
    let mut kids = Vec::new();
    for source in sources {
        check_cancel(cancel)?;
        report(source);
        let mut input = read_pdf(source).map_err(about(source))?;
        if input.version.starts_with("2.") {
            output.version.clone_from(&input.version);
        }
        input.renumber_objects_with(output.max_id + 1);
        let mut ids = input.get_pages();
        let chosen: BTreeSet<_> = page_numbers(pages, ids.len() as u32).map_err(about(source))?.into_iter().collect();
        let omitted: BTreeSet<_> = ids.iter()
            .filter_map(|(number, &id)| (!chosen.contains(number)).then_some(id))
            .collect();
        if !omitted.is_empty() {
            // Internal links must not pull omitted pages and their contents into the output.
            input.traverse_objects(|object| {
                if let Object::Reference(id) = object && omitted.contains(id) {
                    *object = Object::Null;
                }
            });
        }
        ids.retain(|number, _| chosen.contains(number));
        prepare_pages(&mut input, &ids, root).map_err(|e| about(source)(e.to_string()))?;
        kids.extend(ids.into_values().map(Object::Reference));
        output.max_id = input.max_id;
        output.objects.append(&mut input.objects);
    }
    output.objects.insert(root, dictionary! { "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids }.into());
    let catalog = output.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
    output.trailer.set("Root", catalog);
    output.prune_objects();
    output.renumber_objects();
    Ok(output)
}

/// Decoded pixels, already turned upright and then by `turn`, with the metadata to write back.
struct Rotated {
    pixels: DynamicImage,
    profile: Option<Vec<u8>>,
    /// Camera, date and location details, with the orientation reset because it is applied to the pixels.
    exif: Option<Vec<u8>>,
}

fn rotated_pixels(mut decoder: impl ImageDecoder, turn: Turn) -> Result<Rotated, String> {
    use image::metadata::Orientation;
    if decoder.total_bytes() > Limits::default().max_alloc.unwrap_or(u64::MAX) {
        return Err("This image is too large to rotate".into());
    }
    let mut exif = decoder.exif_metadata().map_err(|e| e.to_string())?;
    let orientation = exif.as_mut().and_then(|chunk| Orientation::remove_from_exif_chunk(chunk)).unwrap_or(Orientation::NoTransforms);
    let profile = decoder.icc_profile().map_err(|e| e.to_string())?;
    let mut pixels = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    pixels.apply_orientation(orientation);
    pixels.apply_orientation(match turn {
        Turn::Left => Orientation::Rotate270,
        Turn::Right => Orientation::Rotate90,
    });
    Ok(Rotated { pixels, profile, exif })
}

fn encode_image(mut encoder: impl ImageEncoder, image: Rotated) -> image::ImageResult<()> {
    let Rotated { pixels, profile, exif } = image;
    if let Some(profile) = profile {
        encoder.set_icc_profile(profile).map_err(image::ImageError::Unsupported)?;
    }
    if let Some(exif) = exif {
        encoder.set_exif_metadata(exif).map_err(image::ImageError::Unsupported)?;
    }
    encoder.write_image(pixels.as_bytes(), pixels.width(), pixels.height(), pixels.color().into())
}

fn rotate_image(source: &Path, turn: Turn, out: &mut impl Write) -> Result<(), String> {
    let format = image_format(source).ok_or("Choose a PNG or JPEG image")?;
    if format == ImageFormat::Png {
        let file = File::open(source).map_err(|e| crate::fsx::friendly_err(&e))?;
        let decoder = image::codecs::png::PngDecoder::with_limits(BufReader::new(file), Limits::default()).map_err(|e| e.to_string())?;
        if decoder.is_apng().map_err(|e| e.to_string())? {
            return Err("Animated PNGs cannot be rotated".into());
        }
        encode_image(image::codecs::png::PngEncoder::new(out), rotated_pixels(decoder, turn)?)
    } else {
        let mut reader = ImageReader::open(source).map_err(|e| crate::fsx::friendly_err(&e))?;
        reader.set_format(format);
        let rotated = rotated_pixels(reader.into_decoder().map_err(|e| e.to_string())?, turn)?;
        encode_image(image::codecs::jpeg::JpegEncoder::new_with_quality(out, 95), rotated)
    }.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Dictionary;

    fn pdf_fixture(path: &Path, width: i64) {
        let mut doc = Document::with_version("1.7");
        let root = doc.new_object_id();
        let resources = doc.add_object(Dictionary::new());
        let content = doc.add_object(lopdf::Stream::new(Dictionary::new(), b"0 0 m 10 10 l S".to_vec()));
        let last = doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "Rotate" => 180, "Contents" => content });
        let first = doc.add_object(dictionary! { "Type" => "Page", "Parent" => root, "Contents" => content });
        let link = doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "Dest" => vec![Object::Reference(first), Object::Name(b"Fit".to_vec())],
        });
        doc.get_dictionary_mut(last).unwrap().set("Annots", vec![Object::Reference(link)]);
        doc.objects.insert(root, dictionary! {
            "Type" => "Pages", "Count" => 2, "Kids" => vec![Object::Reference(first), Object::Reference(last)],
            "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), width.into(), 120.into()], "Rotate" => 90,
        }.into());
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }

    #[test]
    fn edits_keep_page_order_and_inherited_properties_and_never_replace() {
        let folder = std::env::temp_dir();
        crate::ops::staged(&folder, |work| {
            let a = work.join("a.pdf");
            let b = work.join("b.pdf");
            pdf_fixture(&a, 100);
            pdf_fixture(&b, 200);
            let cancel = AtomicBool::new(false);
            let save_copy = |edit, paths: &[PathBuf], pages, name| {
                let target = work.join(name);
                save(edit, paths, pages, &target, &cancel, &mut |_| {})?;
                read_pdf(&target)
            };
            let rotated = save_copy(Edit::RotatePdf(Turn::Right), std::slice::from_ref(&a), "1", "rotated.pdf")?;
            let rotations = |doc: &Document| doc.page_iter().map(|id| inherited(doc, id, b"Rotate").unwrap().unwrap().as_i64().unwrap()).collect::<Vec<_>>();
            assert_eq!(rotations(&rotated), [180, 180]);
            assert_eq!(rotations(&read_pdf(&a)?), [90, 180]);

            let extracted = save_copy(Edit::ExtractPdf, std::slice::from_ref(&a), "2", "page.pdf")?;
            assert_eq!(rotations(&extracted), [180]);
            let stored_pages = extracted.objects.values().filter(|object| object.as_dict().is_ok_and(|dict| {
                dict.get(b"Type").and_then(Object::as_name).is_ok_and(|name| name == b"Page")
            })).count();
            assert_eq!(stored_pages, 1, "an internal link must not retain an omitted page");
            assert!(save_copy(Edit::ExtractPdf, std::slice::from_ref(&a), "3", "invalid.pdf").is_err());
            assert!(!work.join("invalid.pdf").exists());

            let merged = save_copy(Edit::MergePdf, &[b, a.clone()], "", "merged.pdf")?;
            assert_eq!(rotations(&merged), [90, 180, 90, 180]);
            let sizes: Vec<_> = merged.page_iter().map(|id| {
                let page = merged.get_dictionary(id).unwrap();
                let resources = page.get(b"Resources").unwrap().as_reference().unwrap();
                assert!(merged.get_dictionary(resources).is_ok());
                assert_eq!(merged.get_page_content(id).trim_ascii(), b"0 0 m 10 10 l S");
                page.get(b"MediaBox").unwrap().as_array().unwrap()[2].as_i64().unwrap()
            }).collect();
            assert_eq!(sizes, [200, 200, 100, 100]);
            assert!(save_copy(Edit::RotatePdf(Turn::Left), std::slice::from_ref(&a), "", "a.pdf").is_err());
            assert_eq!(rotations(&read_pdf(&a)?), [90, 180]);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn rotated_png_preserves_pixels_and_the_original() {
        crate::ops::staged(&std::env::temp_dir(), |work| {
            let source = work.join("pixels.png");
            let target = work.join("rotated.png");
            let pixels = image::RgbaImage::from_fn(3, 2, |x, y| image::Rgba([x as u8, y as u8, 0, 127]));
            pixels.save(&source).unwrap();
            save(Edit::RotateImage(Turn::Left), std::slice::from_ref(&source), "", &target, &AtomicBool::new(false), &mut |_| {})?;
            assert_eq!(image::open(target).unwrap().into_rgba8(), image::imageops::rotate270(&pixels));
            assert_eq!(image::open(source).unwrap().into_rgba8(), pixels);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn rotated_jpeg_keeps_exif_and_applies_its_orientation_once() {
        use image::metadata::Orientation;
        // A little-endian TIFF block holding Make = "Abc" and Orientation = 6 (turned right).
        let exif = [
            b"II*\0".as_slice(), &8u32.to_le_bytes(), &2u16.to_le_bytes(),
            &0x010fu16.to_le_bytes(), &2u16.to_le_bytes(), &4u32.to_le_bytes(), b"Abc\0",
            &0x0112u16.to_le_bytes(), &3u16.to_le_bytes(), &1u32.to_le_bytes(), &6u16.to_le_bytes(), &[0, 0],
            &0u32.to_le_bytes(),
        ].concat();
        let exif_of = |path: &Path| {
            ImageReader::open(path).unwrap().with_guessed_format().unwrap().into_decoder().unwrap().exif_metadata().unwrap().unwrap()
        };
        crate::ops::staged(&std::env::temp_dir(), |work| {
            let source = work.join("photo.jpg");
            let target = work.join("turned.jpg");
            let mut encoder = image::codecs::jpeg::JpegEncoder::new(File::create(&source).unwrap());
            encoder.set_exif_metadata(exif.clone()).unwrap();
            encoder.write_image(&[200; 6 * 4 * 3], 6, 4, image::ExtendedColorType::Rgb8).unwrap();
            // Upright the photo is 4 x 6; a left turn lays it back to 6 x 4.
            save(Edit::RotateImage(Turn::Left), std::slice::from_ref(&source), "", &target, &AtomicBool::new(false), &mut |_| {})?;
            assert_eq!(image::image_dimensions(&target).unwrap(), (6, 4));
            let kept = exif_of(&target);
            assert!(kept.windows(4).any(|bytes| bytes == b"Abc\0"), "other EXIF details must survive");
            assert_eq!(Orientation::from_exif_chunk(&kept), Some(Orientation::NoTransforms));
            assert_eq!(Orientation::from_exif_chunk(&exif_of(&source)), Some(Orientation::Rotate90));
            Ok(())
        }).unwrap();
    }

    #[test]
    fn page_ranges_forgive_spacing_dashes_and_stray_commas() {
        assert_eq!(page_numbers(" 1, 3\u{2013}4 ,, 2-2, ", 5).unwrap(), [1, 2, 3, 4]);
        assert_eq!(page_numbers(" , ", 3).unwrap(), [1, 2, 3]);
        assert!(page_ranges("0").is_err());
        assert!(page_ranges("2-").is_err());
        assert!(page_ranges("a").is_err());
        assert!(page_ranges("4-2").unwrap_err().contains("ascending"));
        assert!(page_ranges("9").is_ok());
        assert_eq!(page_numbers("9", 1).unwrap_err(), "This PDF has only 1 page");
        assert_eq!(page_numbers("2, 6", 5).unwrap_err(), "This PDF has 5 pages. Enter page numbers from 1 to 5");
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Cancelled".into()) } else { Ok(()) }
}

pub fn save(edit: Edit, sources: &[PathBuf], pages: &str, target: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(&Path)) -> Result<(), String> {
    let source = sources.first().ok_or("Select a file first")?;
    let folder = target.parent().ok_or("Choose an output folder")?;
    check_cancel(cancel)?;
    let output_err = about(target);
    let write_err = |e: std::io::Error| output_err(crate::fsx::friendly_err(&e));
    crate::ops::staged(folder, |stage| {
        let draft = stage.join("output");
        let file = File::create_new(&draft).map_err(write_err)?;
        let mut out = BufWriter::new(file);
        // Object and cross-reference streams keep rewritten PDFs close to their original size.
        let write_pdf = |doc: &mut Document, out: &mut BufWriter<File>| doc.save_modern(out).map_err(write_err);
        match edit {
            Edit::RotateImage(turn) => {
                report(source);
                rotate_image(source, turn, &mut out).map_err(about(source))?;
            }
            Edit::RotatePdf(turn) => {
                report(source);
                let mut doc = read_pdf(source).map_err(about(source))?;
                rotate_pdf(&mut doc, pages, turn).map_err(about(source))?;
                check_cancel(cancel)?;
                write_pdf(&mut doc, &mut out)?;
            }
            Edit::ExtractPdf | Edit::MergePdf => {
                let mut doc = collect_pages(sources, pages, cancel, report)?;
                check_cancel(cancel)?;
                write_pdf(&mut doc, &mut out)?;
            }
        }
        out.flush().and_then(|()| out.get_ref().sync_all()).map_err(write_err)?;
        drop(out);
        check_cancel(cancel)?;
        crate::ops::publish(&draft, target, cancel).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                output_err("A file with that name already exists. Choose a different name.".into())
            } else {
                write_err(e)
            }
        })
    })
}
