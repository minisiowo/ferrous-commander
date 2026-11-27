//! Page-level PDF edits and quarter-turns for still images.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, Limits};
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

fn collect_pages(sources: &[PathBuf], pages: &str, keep: bool, cancel: &AtomicBool, report: &mut dyn FnMut(&Path)) -> Result<Document, String> {
    let mut output = Document::with_version("1.7");
    let root = output.new_object_id();
    let mut kids = Vec::new();
    let (mut outline, mut forms) = (Vec::new(), Vec::new());
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
        let kept: Vec<ObjectId> = ids.iter().filter(|(number, _)| chosen.contains(number)).map(|(_, &id)| id).collect();
        // Before omitted pages are cut off: a named destination would be lost with the
        // source's catalog, so bookmarks and links point at their pages directly.
        if keep { extras::make_destinations_explicit(&mut input, &kept); }
        if !omitted.is_empty() {
            // Internal links must not pull omitted pages and their contents into the output.
            input.traverse_objects(|object| {
                if let Object::Reference(id) = object && omitted.contains(id) {
                    *object = Object::Null;
                }
            });
        }
        ids.retain(|number, _| chosen.contains(number));
        if keep {
            outline.extend(extras::outline(&mut input));
            forms.extend(extras::form(&mut input, &kept));
        }
        prepare_pages(&mut input, &ids, root).map_err(|e| about(source)(e.to_string()))?;
        kids.extend(ids.into_values().map(Object::Reference));
        output.max_id = input.max_id;
        output.objects.append(&mut input.objects);
    }
    output.objects.insert(root, dictionary! { "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids }.into());
    let catalog = output.add_object(dictionary! { "Type" => "Catalog", "Pages" => root });
    output.trailer.set("Root", catalog);
    if keep { extras::attach(&mut output, catalog, &outline, forms); }
    output.prune_objects();
    output.renumber_objects();
    Ok(output)
}

/// Bookmarks and form fields, carried into a document built from chosen pages.
mod extras {
    use super::*;
    use lopdf::Dictionary;

    /// Named destinations: the name tree, and the older /Dests dictionary.
    fn named_destinations(doc: &Document) -> BTreeMap<Vec<u8>, Object> {
        let mut out = BTreeMap::new();
        let Ok(catalog) = doc.catalog() else { return out };
        if let Ok((_, Object::Dictionary(dests))) = catalog.get(b"Dests").and_then(|o| doc.dereference(o)) {
            for (key, value) in dests.iter() { out.insert(key.clone(), value.clone()); }
        }
        if let Ok((_, Object::Dictionary(names))) = catalog.get(b"Names").and_then(|o| doc.dereference(o))
            && let Ok(tree) = names.get(b"Dests") {
                name_tree(doc, tree, &mut out, &mut BTreeSet::new());
            }
        out
    }

    fn name_tree(doc: &Document, node: &Object, out: &mut BTreeMap<Vec<u8>, Object>, seen: &mut BTreeSet<ObjectId>) {
        let Ok((id, Object::Dictionary(node))) = doc.dereference(node) else { return };
        if id.is_some_and(|id| !seen.insert(id)) { return; }
        if let Ok(names) = node.get(b"Names").and_then(Object::as_array) {
            for [key, value] in names.as_chunks::<2>().0 {
                if let Ok(key) = key.as_str() { out.insert(key.to_vec(), value.clone()); }
            }
        }
        if let Ok(kids) = node.get(b"Kids").and_then(Object::as_array) {
            for kid in kids { name_tree(doc, kid, out, seen); }
        }
    }

    /// The explicit destination a name stands for.
    fn explicit(doc: &Document, names: &BTreeMap<Vec<u8>, Object>, dest: &Object) -> Option<Object> {
        let key = match dest { Object::Name(n) => n, Object::String(s, _) => s, _ => return None };
        let mut value = doc.dereference(names.get(key)?).ok()?.1;
        if let Object::Dictionary(d) = value { value = doc.dereference(d.get(b"D").ok()?).ok()?.1; }
        value.as_array().is_ok().then(|| value.clone())
    }

    /// The same action with a named destination replaced, when it has one.
    fn explicit_action(doc: &Document, names: &BTreeMap<Vec<u8>, Object>, action: &Dictionary) -> Option<Dictionary> {
        if action.get(b"S").and_then(Object::as_name).ok() != Some(b"GoTo".as_slice()) { return None; }
        let dest = explicit(doc, names, action.get(b"D").ok()?)?;
        let mut action = action.clone();
        action.set("D", dest);
        Some(action)
    }

    /// Bookmarks and the links on kept pages name their destinations directly.
    pub fn make_destinations_explicit(doc: &mut Document, kept: &[ObjectId]) {
        let names = named_destinations(doc);
        if names.is_empty() { return; }
        let mut holders: Vec<ObjectId> = Vec::new();
        let mut seen = BTreeSet::new();
        walk(doc, first_item(doc), &mut seen, &mut |id, _| holders.push(id));
        for page in kept {
            let annots = doc.get_dictionary(*page).ok().and_then(|p| p.get(b"Annots").ok())
                .and_then(|a| doc.dereference(a).ok()).and_then(|(_, a)| a.as_array().ok().cloned()).unwrap_or_default();
            holders.extend(annots.iter().filter_map(|a| a.as_reference().ok()));
        }
        for id in holders {
            let Ok(dict) = doc.get_dictionary(id) else { continue };
            let dest = dict.get(b"Dest").ok().and_then(|d| explicit(doc, &names, d));
            let action = match dict.get(b"A") {
                Ok(Object::Reference(action)) => doc.get_dictionary(*action).ok()
                    .and_then(|a| explicit_action(doc, &names, a)).map(|a| (Some(*action), a)),
                Ok(Object::Dictionary(action)) => explicit_action(doc, &names, action).map(|a| (None, a)),
                _ => None,
            };
            if let Some(dest) = dest && let Ok(dict) = doc.get_dictionary_mut(id) { dict.set("Dest", dest); }
            match action {
                Some((Some(action), fixed)) => { doc.objects.insert(action, fixed.into()); }
                Some((None, fixed)) => if let Ok(dict) = doc.get_dictionary_mut(id) { dict.set("A", fixed); },
                None => {}
            }
        }
    }

    /// A bookmark and the ones below it.
    pub struct Item { id: ObjectId, open: bool, children: Vec<Item> }

    fn first_item(doc: &Document) -> Option<ObjectId> {
        let catalog = doc.catalog().ok()?;
        let (_, root) = doc.dereference(catalog.get(b"Outlines").ok()?).ok()?;
        root.as_dict().ok()?.get(b"First").ok()?.as_reference().ok()
    }

    fn walk(doc: &Document, mut next: Option<ObjectId>, seen: &mut BTreeSet<ObjectId>, visit: &mut dyn FnMut(ObjectId, &Dictionary)) {
        while let Some(id) = next {
            if !seen.insert(id) { return; }
            let Ok(item) = doc.get_dictionary(id) else { return };
            visit(id, item);
            walk(doc, item.get(b"First").and_then(Object::as_reference).ok(), seen, visit);
            next = item.get(b"Next").and_then(Object::as_reference).ok();
        }
    }

    fn items(doc: &Document, mut next: Option<ObjectId>, seen: &mut BTreeSet<ObjectId>) -> Vec<Item> {
        let mut out = Vec::new();
        while let Some(id) = next {
            if !seen.insert(id) { break; }
            let Ok(item) = doc.get_dictionary(id) else { break };
            let open = item.get(b"Count").and_then(Object::as_i64).is_ok_and(|n| n > 0);
            let first = item.get(b"First").and_then(Object::as_reference).ok();
            next = item.get(b"Next").and_then(Object::as_reference).ok();
            out.push(Item { id, open, children: items(doc, first, seen) });
        }
        out
    }

    /// A bookmark whose page was left out. One that only opens a web page or does
    /// something else stays, and so does a heading without any destination.
    fn leads_nowhere(doc: &Document, id: ObjectId) -> bool {
        let Ok(item) = doc.get_dictionary(id) else { return true };
        let dest = match (item.get(b"Dest"), item.get(b"A")) {
            (Ok(dest), _) => Some(dest),
            (_, Ok(action)) => doc.dereference(action).ok().and_then(|(_, a)| a.as_dict().ok())
                .filter(|a| a.get(b"S").and_then(Object::as_name).ok() == Some(b"GoTo".as_slice()))
                .and_then(|a| a.get(b"D").ok()),
            _ => None,
        };
        dest.and_then(|d| doc.dereference(d).ok()).is_some_and(|(_, d)| match d.as_array() {
            Ok(array) => !matches!(array.first(), Some(Object::Reference(_))),
            // Still a name: its page is unknown, since the names stay behind.
            Err(_) => true,
        })
    }

    fn prune(doc: &mut Document, items: Vec<Item>) -> Vec<Item> {
        let mut out = Vec::new();
        for mut item in items {
            item.children = prune(doc, std::mem::take(&mut item.children));
            if leads_nowhere(doc, item.id) {
                if item.children.is_empty() { continue; }
                // A heading still groups the bookmarks under it.
                if let Ok(dict) = doc.get_dictionary_mut(item.id) { dict.remove(b"Dest"); dict.remove(b"A"); }
            }
            out.push(item);
        }
        out
    }

    /// The bookmarks still leading to kept pages, after omitted pages were cut off.
    pub fn outline(doc: &mut Document) -> Vec<Item> {
        let found = items(doc, first_item(doc), &mut BTreeSet::new());
        prune(doc, found)
    }

    /// Form fields with a widget on a kept page, and the form's settings.
    pub struct Form { settings: Dictionary, fields: Vec<ObjectId> }

    fn keep_field(doc: &mut Document, id: ObjectId, widgets: &BTreeSet<ObjectId>, seen: &mut BTreeSet<ObjectId>) -> bool {
        if !seen.insert(id) { return false; }
        if widgets.contains(&id) { return true; }
        let kids: Vec<ObjectId> = doc.get_dictionary(id).ok().and_then(|f| f.get(b"Kids").ok())
            .and_then(|k| k.as_array().ok()).map(|k| k.iter().filter_map(|k| k.as_reference().ok()).collect()).unwrap_or_default();
        if kids.is_empty() { return false; }
        let kept: Vec<Object> = kids.into_iter().filter(|&kid| keep_field(doc, kid, widgets, seen)).map(Object::Reference).collect();
        let any = !kept.is_empty();
        if let Ok(field) = doc.get_dictionary_mut(id) { field.set("Kids", kept); }
        any
    }

    pub fn form(doc: &mut Document, kept: &[ObjectId]) -> Option<Form> {
        let catalog = doc.catalog().ok()?;
        let (_, settings) = doc.dereference(catalog.get(b"AcroForm").ok()?).ok()?;
        let mut settings = settings.as_dict().ok()?.clone();
        let fields: Vec<ObjectId> = doc.dereference(settings.get(b"Fields").ok()?).ok()?.1.as_array().ok()?
            .iter().filter_map(|f| f.as_reference().ok()).collect();
        let mut widgets = BTreeSet::new();
        for page in kept {
            if let Some((_, Object::Array(annots))) = doc.get_dictionary(*page).ok().and_then(|p| p.get(b"Annots").ok()).and_then(|a| doc.dereference(a).ok()) {
                widgets.extend(annots.iter().filter_map(|a| a.as_reference().ok()));
            }
        }
        let mut seen = BTreeSet::new();
        let fields: Vec<ObjectId> = fields.into_iter().filter(|&f| keep_field(doc, f, &widgets, &mut seen)).collect();
        if fields.is_empty() { return None; }
        // XFA describes the whole original form, which no longer matches these pages.
        for key in [b"Fields".as_slice(), b"XFA"] { settings.remove(key); }
        // Shared resources travel by value: the source's own objects may be dropped.
        if let Ok(resources) = settings.get(b"DR") && let Ok((_, Object::Dictionary(resources))) = doc.dereference(resources) {
            let mut resources = resources.clone();
            if let Ok(fonts) = resources.get(b"Font") && let Ok((_, Object::Dictionary(fonts))) = doc.dereference(fonts) {
                let fonts = fonts.clone();
                resources.set("Font", fonts);
            }
            settings.set("DR", resources);
        }
        Some(Form { settings, fields })
    }

    fn link(doc: &mut Document, parent: ObjectId, items: &[Item]) -> i64 {
        let mut visible = 0;
        for (k, item) in items.iter().enumerate() {
            let below = link(doc, item.id, &item.children);
            let Ok(dict) = doc.get_dictionary_mut(item.id) else { continue };
            dict.set("Parent", parent);
            let neighbours = [("Prev", k.checked_sub(1).map(|p| items[p].id)), ("Next", items.get(k + 1).map(|n| n.id)),
                ("First", item.children.first().map(|c| c.id)), ("Last", item.children.last().map(|c| c.id))];
            for (key, id) in neighbours {
                match id { Some(id) => dict.set(key, id), None => { dict.remove(key.as_bytes()); } }
            }
            if item.children.is_empty() {
                dict.remove(b"Count");
            } else {
                dict.set("Count", if item.open { below } else { -(item.children.len() as i64) });
            }
            visible += 1 + if item.open { below } else { 0 };
        }
        visible
    }

    /// Hang the bookmarks and fields of every source on the new document, in order.
    /// Fields keep their values apart: a name already in use gets a number.
    pub fn attach(doc: &mut Document, catalog: ObjectId, outline: &[Item], forms: Vec<Form>) {
        if !outline.is_empty() {
            let root = doc.new_object_id();
            let visible = link(doc, root, outline);
            doc.objects.insert(root, dictionary! {
                "Type" => "Outlines", "First" => outline[0].id, "Last" => outline[outline.len() - 1].id, "Count" => visible,
            }.into());
            if let Ok(dict) = doc.get_dictionary_mut(catalog) { dict.set("Outlines", root); }
        }
        let mut forms = forms.into_iter();
        let Some(Form { mut settings, mut fields }) = forms.next() else { return };
        let name = |doc: &Document, id: ObjectId| doc.get_dictionary(id).ok().and_then(|f| f.get(b"T").ok()).and_then(|t| t.as_str().ok()).map(<[u8]>::to_vec);
        let mut used: BTreeSet<Vec<u8>> = fields.iter().filter_map(|&f| name(doc, f)).collect();
        for form in forms {
            for field in form.fields {
                if let Some(taken) = name(doc, field).filter(|n| used.contains(n)) {
                    let base = String::from_utf8_lossy(&taken).into_owned();
                    let fresh = (2..).map(|n| format!("{base} ({n})")).find(|t| !used.contains(t.as_bytes())).unwrap_or(base);
                    if let Ok(dict) = doc.get_dictionary_mut(field) { dict.set("T", Object::string_literal(fresh.clone())); }
                    used.insert(fresh.into_bytes());
                } else if let Some(n) = name(doc, field) {
                    used.insert(n);
                }
                fields.push(field);
            }
            // Fonts the later forms use, under names the first one lacks.
            if let (Ok(Object::Dictionary(theirs)), Ok(Object::Dictionary(ours))) = (form.settings.get(b"DR"), settings.get_mut(b"DR"))
                && let (Ok(Object::Dictionary(their_fonts)), Ok(Object::Dictionary(our_fonts))) = (theirs.get(b"Font"), ours.get_mut(b"Font")) {
                    for (key, font) in their_fonts.iter() {
                        if !our_fonts.has(key) { our_fonts.set(key.clone(), font.clone()); }
                    }
                }
        }
        settings.set("Fields", fields.into_iter().map(Object::Reference).collect::<Vec<_>>());
        let form = doc.add_object(settings);
        if let Ok(dict) = doc.get_dictionary_mut(catalog) { dict.set("AcroForm", form); }
    }
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
        // PNG stores pixels without loss, so turning the pixels loses nothing.
        let file = File::open(source).map_err(|e| crate::fsx::friendly_err(&e))?;
        let decoder = image::codecs::png::PngDecoder::with_limits(BufReader::new(file), Limits::default()).map_err(|e| e.to_string())?;
        if decoder.is_apng().map_err(|e| e.to_string())? {
            return Err("Animated PNGs cannot be rotated".into());
        }
        encode_image(image::codecs::png::PngEncoder::new(out), rotated_pixels(decoder, turn)?).map_err(|e| e.to_string())
    } else {
        let data = std::fs::read(source).map_err(|e| crate::fsx::friendly_err(&e))?;
        out.write_all(&jpeg::turned(&data, turn)?).map_err(|e| crate::fsx::friendly_err(&e))
    }
}

/// JPEG turns without loss: decoding and saving again would lower the quality every time.
/// Only the orientation in the photo's details (EXIF) changes, as phones and photo apps do,
/// and every other byte is kept.
mod jpeg {
    use super::Turn;

    const ORIENTATION: u16 = 0x0112;
    const SHORT: u16 = 3;
    const DAMAGED: &str = "This photo's details (EXIF) are damaged, so it cannot be rotated without loss";

    /// The EXIF orientations 1 to 8, as what they do to (x, y) with y pointing down.
    const MATRICES: [[i8; 4]; 8] = [
        [1, 0, 0, 1], [-1, 0, 0, 1], [-1, 0, 0, -1], [1, 0, 0, -1],
        [0, 1, 1, 0], [0, -1, 1, 0], [0, -1, -1, 0], [0, 1, -1, 0],
    ];

    /// The orientation that shows the picture as `orientation` does, then turned.
    pub fn compose(orientation: u16, turn: Turn) -> u16 {
        let [a, b, c, d] = MATRICES[usize::from(orientation.clamp(1, 8)) - 1];
        let [p, q, r, s] = match turn { Turn::Right => [0, -1, 1, 0], Turn::Left => [0, 1, -1, 0] };
        let product = [p * a + q * c, p * b + q * d, r * a + s * c, r * b + s * d];
        MATRICES.iter().position(|m| *m == product).map_or(1, |k| k as u16 + 1)
    }

    struct Order(bool);
    impl Order {
        fn u16(&self, b: &[u8], at: usize) -> Option<u16> {
            let x: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
            Some(if self.0 { u16::from_le_bytes(x) } else { u16::from_be_bytes(x) })
        }
        fn u32(&self, b: &[u8], at: usize) -> Option<u32> {
            let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
            Some(if self.0 { u32::from_le_bytes(x) } else { u32::from_be_bytes(x) })
        }
        fn put16(&self, v: u16) -> [u8; 2] { if self.0 { v.to_le_bytes() } else { v.to_be_bytes() } }
        fn put32(&self, v: u32) -> [u8; 4] { if self.0 { v.to_le_bytes() } else { v.to_be_bytes() } }
    }

    /// The TIFF block of an EXIF segment, with its orientation turned. A block without an
    /// orientation gets one: its first directory is copied to the end with the entry added,
    /// so nothing that other entries point to moves.
    fn turn_tiff(tiff: &[u8], turn: Turn) -> Option<Vec<u8>> {
        let order = match tiff.get(..2)? { b"II" => Order(true), b"MM" => Order(false), _ => return None };
        if order.u16(tiff, 2)? != 42 { return None; }
        let ifd = order.u32(tiff, 4)? as usize;
        let count = usize::from(order.u16(tiff, ifd)?);
        let entries = tiff.get(ifd + 2..ifd + 2 + 12 * count)?;
        let mut out = tiff.to_vec();
        for k in 0..count {
            let entry = &entries[12 * k..12 * k + 12];
            if order.u16(entry, 0)? == ORIENTATION {
                if order.u16(entry, 2)? != SHORT || order.u32(entry, 4)? != 1 { return None; }
                let turned = compose(order.u16(entry, 8)?, turn);
                let at = ifd + 2 + 12 * k + 8;
                out[at..at + 2].copy_from_slice(&order.put16(turned));
                return Some(out);
            }
        }
        let next = order.u32(tiff, ifd + 2 + 12 * count)?;
        if out.len() % 2 == 1 { out.push(0); }
        let moved = u32::try_from(out.len()).ok()?;
        let mut added = Vec::with_capacity(12);
        added.extend(order.put16(ORIENTATION));
        added.extend(order.put16(SHORT));
        added.extend(order.put32(1));
        added.extend(order.put16(compose(1, turn)));
        added.extend([0, 0]);
        let mut list: Vec<&[u8]> = entries.chunks(12).collect();
        let at = list.iter().position(|e| order.u16(e, 0).is_some_and(|tag| tag > ORIENTATION)).unwrap_or(list.len());
        list.insert(at, &added);
        out.extend(order.put16(u16::try_from(list.len()).ok()?));
        for entry in list { out.extend_from_slice(entry); }
        out.extend(order.put32(next));
        out[4..8].copy_from_slice(&order.put32(moved));
        Some(out)
    }

    fn segment(tiff: &[u8]) -> Result<Vec<u8>, String> {
        let length = u16::try_from(2 + 6 + tiff.len()).map_err(|_| "This photo's details (EXIF) are too large to change".to_string())?;
        let mut out = vec![0xFF, 0xE1];
        out.extend(length.to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend_from_slice(tiff);
        Ok(out)
    }

    /// The same JPEG, shown turned a quarter. The compressed picture is not touched.
    pub fn turned(data: &[u8], turn: Turn) -> Result<Vec<u8>, String> {
        if data.get(..2) != Some(&[0xFF, 0xD8]) { return Err("This file is damaged or is not a JPEG image".into()); }
        let mut at = 2;
        // Where a new EXIF segment goes: after a JFIF header, which must come first.
        let mut insert = 2;
        loop {
            if data.get(at) != Some(&0xFF) { return Err("This file is damaged or is not a JPEG image".into()); }
            let marker = *data.get(at + 1).ok_or("This file is damaged or is not a JPEG image")?;
            match marker {
                0xFF => { at += 1; continue; }
                // Start of the picture data, or its end: no EXIF before it.
                0xDA | 0xD9 => break,
                0x01 | 0xD0..=0xD7 => { at += 2; continue; }
                _ => {}
            }
            let length = usize::from(u16::from_be_bytes([*data.get(at + 2).ok_or(DAMAGED)?, *data.get(at + 3).ok_or(DAMAGED)?]));
            let end = at + 2 + length;
            let body = data.get(at + 4..end).ok_or("This file is damaged or is not a JPEG image")?;
            if marker == 0xE1 && body.starts_with(b"Exif\0\0") {
                let tiff = turn_tiff(&body[6..], turn).ok_or(DAMAGED)?;
                return Ok([&data[..at], &segment(&tiff)?[..], &data[end..]].concat());
            }
            if marker == 0xE0 && insert == at { insert = end; }
            at = end;
        }
        // No details yet: the smallest EXIF block that holds an orientation.
        let tiff = [b"MM\0\x2a\0\0\0\x08\0\x01".as_slice(), &ORIENTATION.to_be_bytes(), &SHORT.to_be_bytes(),
            &1u32.to_be_bytes(), &compose(1, turn).to_be_bytes(), &[0, 0], &0u32.to_be_bytes()].concat();
        Ok([&data[..insert], &segment(&tiff)?[..], &data[insert..]].concat())
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Cancelled".into()) } else { Ok(()) }
}

/// `keep`: bookmarks and form fields go with the pages taken from a PDF.
pub fn save(edit: Edit, sources: &[PathBuf], pages: &str, keep: bool, target: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(&Path)) -> Result<(), String> {
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
                let mut doc = collect_pages(sources, pages, keep, cancel, report)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageReader;
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
                save(edit, paths, pages, true, &target, &cancel, &mut |_| {})?;
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

    /// Three pages, bookmarks of every kind and a form with a field on pages 1 and 3.
    fn rich_pdf(path: &Path) {
        let mut doc = Document::with_version("1.7");
        let root = doc.new_object_id();
        let pages: Vec<ObjectId> = (0..3).map(|_| doc.new_object_id()).collect();
        let field = |doc: &mut Document, name: &str, page: ObjectId| doc.add_object(dictionary! {
            "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx", "T" => Object::string_literal(name),
            "Rect" => vec![0.into(), 0.into(), 50.into(), 20.into()], "P" => page,
        });
        let name = field(&mut doc, "name", pages[0]);
        let city = field(&mut doc, "city", pages[2]);
        for (k, &page) in pages.iter().enumerate() {
            let mut dict = dictionary! { "Type" => "Page", "Parent" => root };
            if k == 0 { dict.set("Annots", vec![Object::Reference(name)]); }
            if k == 2 { dict.set("Annots", vec![Object::Reference(city)]); }
            doc.objects.insert(page, dict.into());
        }
        doc.objects.insert(root, dictionary! {
            "Type" => "Pages", "Count" => 3, "Kids" => pages.iter().map(|&p| Object::Reference(p)).collect::<Vec<_>>(),
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        }.into());
        let fit = |page: ObjectId| vec![Object::Reference(page), Object::Name(b"Fit".to_vec())];
        let outlines = doc.new_object_id();
        let [one, two, part, three, web] = [(); 5].map(|_| doc.new_object_id());
        let title = |t: &str| Object::string_literal(t);
        doc.objects.insert(one, dictionary! { "Title" => title("One"), "Parent" => outlines, "Next" => two, "Dest" => fit(pages[0]) }.into());
        doc.objects.insert(two, dictionary! { "Title" => title("Two"), "Parent" => outlines, "Prev" => one, "Next" => part, "Dest" => Object::string_literal("chapter two") }.into());
        doc.objects.insert(part, dictionary! { "Title" => title("Part"), "Parent" => outlines, "Prev" => two, "Next" => web,
            "A" => dictionary! { "S" => "GoTo", "D" => fit(pages[0]) }, "First" => three, "Last" => three, "Count" => 1 }.into());
        doc.objects.insert(three, dictionary! { "Title" => title("Three"), "Parent" => part, "Dest" => fit(pages[2]) }.into());
        doc.objects.insert(web, dictionary! { "Title" => title("Web"), "Parent" => outlines, "Prev" => part,
            "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") } }.into());
        doc.objects.insert(outlines, dictionary! { "Type" => "Outlines", "First" => one, "Last" => web, "Count" => 5 }.into());
        let names = doc.add_object(dictionary! { "Names" => vec![Object::string_literal("chapter two"), fit(pages[1]).into()] });
        let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
        let form = doc.add_object(dictionary! { "Fields" => vec![Object::Reference(name), Object::Reference(city)],
            "DR" => dictionary! { "Font" => dictionary! { "Helv" => font } }, "XFA" => Object::string_literal("<xdp/>") });
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => root, "Outlines" => outlines,
            "Names" => dictionary! { "Dests" => names }, "AcroForm" => form });
        doc.trailer.set("Root", catalog);
        doc.save(path).unwrap();
    }

    /// Bookmarks as "title -> page number", indented by depth.
    fn bookmarks(doc: &Document) -> Vec<String> {
        let pages: BTreeMap<ObjectId, u32> = doc.get_pages().into_iter().map(|(n, id)| (id, n)).collect();
        let mut out = Vec::new();
        fn walk(doc: &Document, pages: &BTreeMap<ObjectId, u32>, next: Option<ObjectId>, depth: usize, out: &mut Vec<String>) {
            let mut next = next;
            while let Some(id) = next {
                let item = doc.get_dictionary(id).unwrap();
                let title = String::from_utf8_lossy(item.get(b"Title").unwrap().as_str().unwrap()).into_owned();
                let dest = item.get(b"Dest").ok().or_else(|| item.get(b"A").ok()
                    .and_then(|a| doc.dereference(a).ok()).and_then(|(_, a)| a.as_dict().ok()).and_then(|a| a.get(b"D").ok()));
                let page = dest.and_then(|d| doc.dereference(d).ok()).map(|(_, d)| d.as_array().ok().and_then(|a| a.first()?.as_reference().ok()));
                let target = match page {
                    Some(Some(page)) => format!(" -> {}", pages.get(&page).map_or("?".into(), |n| n.to_string())),
                    // A name, kept in the source's own list of names.
                    Some(None) => " -> named".into(),
                    None => String::new(),
                };
                out.push(format!("{}{title}{target}", "  ".repeat(depth)));
                walk(doc, pages, item.get(b"First").and_then(Object::as_reference).ok(), depth + 1, out);
                next = item.get(b"Next").and_then(Object::as_reference).ok();
            }
        }
        let first = doc.catalog().ok().and_then(|c| c.get(b"Outlines").ok()).and_then(|o| o.as_reference().ok())
            .and_then(|o| doc.get_dictionary(o).ok()?.get(b"First").ok()?.as_reference().ok());
        walk(doc, &pages, first, 0, &mut out);
        out
    }

    fn field_names(doc: &Document) -> Vec<String> {
        let Ok(form) = doc.catalog().and_then(|c| c.get(b"AcroForm")).and_then(|f| doc.dereference(f)).and_then(|(_, f)| f.as_dict()) else { return vec![] };
        assert!(!form.has(b"XFA"), "XFA describes the original form only");
        form.get(b"Fields").unwrap().as_array().unwrap().iter().map(|f| {
            String::from_utf8_lossy(doc.get_dictionary(f.as_reference().unwrap()).unwrap().get(b"T").unwrap().as_str().unwrap()).into_owned()
        }).collect()
    }

    #[test]
    fn bookmarks_and_form_fields_follow_the_pages_when_asked() {
        crate::ops::staged(&std::env::temp_dir(), |work| {
            let source = work.join("rich.pdf");
            rich_pdf(&source);
            let cancel = AtomicBool::new(false);
            let make = |edit, sources: &[PathBuf], pages: &str, keep: bool, name: &str| {
                let target = work.join(name);
                save(edit, sources, pages, keep, &target, &cancel, &mut |_| {}).unwrap();
                read_pdf(&target).unwrap()
            };
            let all = make(Edit::ExtractPdf, std::slice::from_ref(&source), "", true, "all.pdf");
            assert_eq!(bookmarks(&all), ["One -> 1", "Two -> 2", "Part -> 1", "  Three -> 3", "Web"]);
            assert_eq!(field_names(&all), ["name", "city"]);

            // Page 1 is left out: its bookmark goes, "Part" stays as a heading for "Three",
            // and the name given to page 2 still finds it.
            let later = make(Edit::ExtractPdf, std::slice::from_ref(&source), "2-3", true, "later.pdf");
            assert_eq!(bookmarks(&later), ["Two -> 1", "Part", "  Three -> 2", "Web"]);
            assert_eq!(field_names(&later), ["city"]);
            let part = later.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
            assert_eq!(later.get_dictionary(part).unwrap().get(b"Count").unwrap().as_i64().unwrap(), 4, "open headings count what they show");

            let merged = make(Edit::MergePdf, &[source.clone(), source.clone()], "", true, "merged.pdf");
            assert_eq!(bookmarks(&merged), ["One -> 1", "Two -> 2", "Part -> 1", "  Three -> 3", "Web",
                "One -> 4", "Two -> 5", "Part -> 4", "  Three -> 6", "Web"]);
            assert_eq!(field_names(&merged), ["name", "city", "name (2)", "city (2)"], "two copies of a form keep their own values");

            let plain = make(Edit::MergePdf, &[source.clone(), source.clone()], "", false, "plain.pdf");
            assert!(bookmarks(&plain).is_empty() && field_names(&plain).is_empty());
            assert_eq!(plain.get_pages().len(), 6);
            Ok(())
        }).unwrap();
    }

    /// Real documents: FERROUS_PDFS names them (separated like PATH), FERROUS_PDF_OUT the
    /// folder for the results, which `qpdf --check` can then read.
    #[test]
    #[ignore = "Reads the PDFs named in FERROUS_PDFS and writes to FERROUS_PDF_OUT"]
    fn real_pdfs_keep_their_bookmarks() {
        let sources: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("FERROUS_PDFS").expect("FERROUS_PDFS")).collect();
        let out = PathBuf::from(std::env::var_os("FERROUS_PDF_OUT").expect("FERROUS_PDF_OUT"));
        let cancel = AtomicBool::new(false);
        for (k, source) in sources.iter().enumerate() {
            let original = read_pdf(source).unwrap();
            let before = bookmarks(&original);
            let target = out.join(format!("{k} all.pdf"));
            save(Edit::ExtractPdf, std::slice::from_ref(source), "", true, &target, &cancel, &mut |_| {}).unwrap();
            let titles = |list: Vec<String>| list.into_iter().map(|b| b.split(" -> ").next().unwrap().to_owned()).collect::<Vec<_>>();
            let after = bookmarks(&read_pdf(&target).unwrap());
            assert_eq!(titles(after.clone()), titles(before.clone()), "{}", source.display());
            assert!(!after.iter().any(|b| b.ends_with(" -> named") || b.ends_with(" -> ?")), "every kept bookmark leads to a page: {after:?}");
            let count = original.get_pages().len();
            let target = out.join(format!("{k} second half.pdf"));
            save(Edit::ExtractPdf, std::slice::from_ref(source), &format!("{}-{count}", count / 2 + 1), true, &target, &cancel, &mut |_| {}).unwrap();
            println!("{}: {} bookmarks, {} in the second half", source.display(), before.len(), bookmarks(&read_pdf(&target).unwrap()).len());
        }
        let target = out.join("merged.pdf");
        save(Edit::MergePdf, &sources, "", true, &target, &cancel, &mut |_| {}).unwrap();
        let total: usize = sources.iter().map(|s| bookmarks(&read_pdf(s).unwrap()).len()).sum();
        assert_eq!(bookmarks(&read_pdf(&target).unwrap()).len(), total);
    }

    #[test]
    fn rotated_png_preserves_pixels_and_the_original() {
        crate::ops::staged(&std::env::temp_dir(), |work| {
            let source = work.join("pixels.png");
            let target = work.join("rotated.png");
            let pixels = image::RgbaImage::from_fn(3, 2, |x, y| image::Rgba([x as u8, y as u8, 0, 127]));
            pixels.save(&source).unwrap();
            save(Edit::RotateImage(Turn::Left), std::slice::from_ref(&source), "", true, &target, &AtomicBool::new(false), &mut |_| {})?;
            assert_eq!(image::open(target).unwrap().into_rgba8(), image::imageops::rotate270(&pixels));
            assert_eq!(image::open(source).unwrap().into_rgba8(), pixels);
            Ok(())
        }).unwrap();
    }

    /// The picture as a viewer shows it: decoded, then turned by its EXIF orientation.
    fn shown(path: &Path) -> image::RgbImage {
        let mut decoder = ImageReader::open(path).unwrap().with_guessed_format().unwrap().into_decoder().unwrap();
        let orientation = decoder.orientation().unwrap();
        let mut picture = DynamicImage::from_decoder(decoder).unwrap();
        picture.apply_orientation(orientation);
        picture.into_rgb8()
    }

    /// The compressed picture: everything from the start of the image data on.
    fn picture_data(path: &Path) -> Vec<u8> {
        let data = std::fs::read(path).unwrap();
        let start = data.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
        data[start..].to_vec()
    }

    fn jpeg_fixture(path: &Path, exif: Option<Vec<u8>>) {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new(File::create(path).unwrap());
        if let Some(exif) = exif { encoder.set_exif_metadata(exif).unwrap(); }
        // Different on each side, so a wrong turn shows.
        let pixels = image::RgbImage::from_fn(32, 16, |x, y| image::Rgb([(x * 8) as u8, (y * 16) as u8, if x < 16 { 0 } else { 255 }]));
        encoder.write_image(pixels.as_raw(), 32, 16, image::ExtendedColorType::Rgb8).unwrap();
    }

    /// Little-endian EXIF with Make = "Abc", and Orientation when given.
    fn exif(orientation: Option<u16>) -> Vec<u8> {
        let entries = 1 + u16::from(orientation.is_some());
        let mut tiff = [b"II*\0".as_slice(), &8u32.to_le_bytes(), &entries.to_le_bytes(),
            &0x010fu16.to_le_bytes(), &2u16.to_le_bytes(), &4u32.to_le_bytes(), b"Abc\0"].concat();
        if let Some(o) = orientation {
            tiff.extend([&0x0112u16.to_le_bytes()[..], &3u16.to_le_bytes(), &1u32.to_le_bytes(), &o.to_le_bytes(), &[0, 0]].concat());
        }
        tiff.extend(0u32.to_le_bytes());
        tiff
    }

    #[test]
    fn jpeg_turns_change_only_the_orientation_and_never_the_picture() {
        use image::metadata::Orientation;
        let exif_of = |path: &Path| {
            ImageReader::open(path).unwrap().with_guessed_format().unwrap().into_decoder().unwrap().exif_metadata().unwrap().unwrap()
        };
        crate::ops::staged(&std::env::temp_dir(), |work| {
            for (case, details) in [("no details", None), ("details without orientation", Some(exif(None))),
                ("turned right already", Some(exif(Some(6)))), ("mirrored", Some(exif(Some(2))))] {
                let source = work.join(format!("{case}.jpg"));
                jpeg_fixture(&source, details.clone());
                for (turn, name, expected) in [(Turn::Left, "left", image::imageops::rotate270(&shown(&source))),
                    (Turn::Right, "right", image::imageops::rotate90(&shown(&source)))] {
                    let target = work.join(format!("{case} ({name}).jpg"));
                    save(Edit::RotateImage(turn), std::slice::from_ref(&source), "", true, &target, &AtomicBool::new(false), &mut |_| {})?;
                    assert_eq!(picture_data(&target), picture_data(&source), "{case}: the compressed picture must stay as it was");
                    assert!(shown(&target) == expected, "{case}, {name}: shown turned");
                    if details.is_some() {
                        assert!(exif_of(&target).windows(4).any(|bytes| bytes == b"Abc\0"), "{case}: other details survive");
                    }
                }
            }
            // Four turns the same way show the photo as it was, with the same bytes.
            let source = work.join("photo.jpg");
            jpeg_fixture(&source, Some(exif(Some(1))));
            let mut current = source.clone();
            for k in 0..4 {
                let next = work.join(format!("turn {k}.jpg"));
                save(Edit::RotateImage(Turn::Right), std::slice::from_ref(&current), "", true, &next, &AtomicBool::new(false), &mut |_| {})?;
                current = next;
            }
            assert_eq!(std::fs::read(&current).unwrap(), std::fs::read(&source).unwrap());
            assert_eq!(Orientation::from_exif_chunk(&exif_of(&source)), Some(Orientation::NoTransforms));
            Ok(())
        }).unwrap();
    }

    #[test]
    fn orientations_compose_like_turns() {
        for o in 1..=8 {
            let right = |o| jpeg::compose(o, Turn::Right);
            assert_eq!(right(right(right(right(o)))), o);
            assert_eq!(jpeg::compose(right(o), Turn::Left), o);
        }
        assert_eq!(jpeg::compose(1, Turn::Right), 6);
        assert_eq!(jpeg::compose(1, Turn::Left), 8);
        assert_eq!(jpeg::compose(6, Turn::Right), 3);
        assert_eq!(jpeg::compose(2, Turn::Right), 7, "a mirrored photo stays mirrored");
    }

    #[test]
    fn a_damaged_jpeg_is_refused_and_not_rewritten() {
        assert!(jpeg::turned(b"not a jpeg", Turn::Left).is_err());
        assert!(jpeg::turned(&[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x40, b'E'], Turn::Left).is_err(), "a segment longer than the file");
        let broken = [&[0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x10][..], b"Exif\0\0II*\0\xff\xff\0\0", &[0xFF, 0xD9]].concat();
        assert!(jpeg::turned(&broken, Turn::Left).unwrap_err().contains("damaged"), "details pointing outside themselves");
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
