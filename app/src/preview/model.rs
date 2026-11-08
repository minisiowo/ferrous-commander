//! 3D models for printing (STL, 3MF), drawn as one shaded picture. Runs inside the helper process.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::load::{Loaded, ModelView, Options, Picture, Reply, failed};
use super::office::attribute;
use super::picture::to_picture;
use crate::fsx;

const MAX_FILE: u64 = 500_000_000;
const MAX_TRIANGLES: usize = 8_000_000;
/// Unpacked size of one part of a 3MF, which is a ZIP and could unpack to anything.
const MAX_MODEL_BYTES: u64 = 2_000_000_000;
const MAX_THUMBNAIL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PARTS: usize = 4096;
const MAX_NESTING: u32 = 16;
/// Objects may repeat each other, so a small file can describe an endless tree.
const MAX_VISITS: usize = 1_000_000;
const MAX_RENDER_SIDE: u32 = 1600;
/// Samples per pixel along each side, for smooth edges.
const SAMPLES: usize = 2;
const MODEL_COLOR: [f32; 3] = [176.0, 184.0, 196.0];
/// Degrees of the first view: from the front, a little to the right and above.
const TURN: f32 = 35.0;
const TILT: f32 = 28.0;
/// Stops short of looking straight down, where "up" stops meaning anything.
pub(super) const TILT_OFFSETS: (f32, f32) = (-85.0 - TILT, 85.0 - TILT);
pub(super) const MIN_MODEL_ZOOM: f32 = 0.25;
/// Above this many triangles, a model is drawn at half size while it moves.
const QUICK_LIMIT: usize = 300_000;
const MARGIN: f32 = 8.0;

const TOO_LARGE: &str = "This model is too large to preview";
const NO_SHAPE: &str = "This file holds no model to show";

type Point = [f32; 3];
type Triangle = [Point; 3];

fn dot(a: Point, b: Point) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Point, b: Point) -> Point {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn minus(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn unit(v: Point) -> Point {
    let length = dot(v, v).sqrt();
    if length > 0.0 { [v[0] / length, v[1] / length, v[2] / length] } else { v }
}

fn mix(parts: [(Point, f32); 3]) -> Point {
    std::array::from_fn(|i| parts.iter().map(|(v, by)| v[i] * by).sum())
}

fn usable(t: &Triangle) -> bool {
    t.iter().flatten().all(|v| v.is_finite())
}

fn too_large(file: &Path) -> bool {
    std::fs::metadata(file).is_ok_and(|m| m.len() >= MAX_FILE)
}

/// The directions of a camera, with Z up as printers have it.
struct Camera {
    right: Point,
    up: Point,
    to_camera: Point,
}

fn camera(turn: f32, tilt: f32) -> Camera {
    let (turn, tilt) = (turn.to_radians(), tilt.to_radians());
    let to_camera = [tilt.cos() * turn.sin(), -tilt.cos() * turn.cos(), tilt.sin()];
    let right = unit(cross([0.0, 0.0, 1.0], to_camera));
    Camera { right, up: cross(to_camera, right), to_camera }
}

/// What every view of a model shares: the point it turns around, and how far it
/// reaches from there in the first view, which sets its size on the screen.
struct Framing {
    pivot: Point,
    reach: [f32; 2],
}

fn framing(triangles: &[Triangle]) -> Option<Framing> {
    let (mut low, mut high) = ([f32::MAX; 3], [f32::MIN; 3]);
    for point in triangles.iter().flatten() {
        for i in 0..3 {
            low[i] = low[i].min(point[i]);
            high[i] = high[i].max(point[i]);
        }
    }
    let pivot: Point = std::array::from_fn(|i| low[i] / 2.0 + high[i] / 2.0);
    let first = camera(TURN, TILT);
    let mut reach = [0f32; 2];
    for point in triangles.iter().flatten() {
        let from_pivot = minus(*point, pivot);
        reach[0] = reach[0].max(dot(from_pivot, first.right).abs());
        reach[1] = reach[1].max(dot(from_pivot, first.up).abs());
    }
    (reach[0].max(reach[1]) > 0.0).then_some(Framing { pivot, reach })
}

fn shown(triangles: &[Triangle], options: Options) -> Reply {
    let Some(framing) = framing(triangles) else { return failed(NO_SHAPE) };
    let side = options.max_side.min(MAX_RENDER_SIDE).max(32) as f32;
    let first_scale = (side / 2.0 - MARGIN) / framing.reach[0].max(framing.reach[1]);
    let first_size = framing.reach.map(|reach| (((reach * first_scale + MARGIN) * 2.0).ceil() as usize).max(32));
    let Some(view) = options.view else {
        let centre = first_size.map(|v| v as f32 / 2.0);
        let picture = paint(triangles, &framing, &camera(TURN, TILT), first_scale, centre, first_size, SAMPLES);
        // Half the drawn size: like vector art, it has no size of its own and fills the window.
        let orig = [picture.size[0].div_ceil(2), picture.size[1].div_ceil(2)];
        return (Loaded::Model(orig), Some(picture));
    };
    (Loaded::Detail, Some(look(triangles, &framing, view, side, first_scale, first_size[0])))
}

fn look(triangles: &[Triangle], framing: &Framing, view: ModelView, side: f32, first_scale: f32, first_width: usize) -> Picture {
    let [width, height] = view.size.map(|v| v.clamp(16, 4096) as f32);
    let quick = if !view.fine && triangles.len() > QUICK_LIMIT { 0.5 } else { 1.0 };
    let shrink = (side / width.max(height)).min(1.0) * quick;
    let zoom = view.zoom.clamp(10, 6400) as f32 / 100.0;
    let scale = first_scale * view.fit.max(1) as f32 / first_width as f32 * zoom * shrink;
    let size = [(width * shrink).ceil() as usize, (height * shrink).ceil() as usize];
    let centre = [size[0] as f32 / 2.0 + view.pan[0] as f32 * shrink, size[1] as f32 / 2.0 + view.pan[1] as f32 * shrink];
    let camera = camera(TURN - view.turn as f32 / 10.0, TILT + view.tilt as f32 / 10.0);
    paint(triangles, framing, &camera, scale, centre, size, if view.fine { SAMPLES } else { 1 })
}

fn paint(triangles: &[Triangle], framing: &Framing, camera: &Camera, scale: f32, centre: [f32; 2], [width, height]: [usize; 2], samples: usize) -> Picture {
    let Camera { right, up, to_camera } = *camera;
    let key = unit(mix([(right, -0.4), (up, 0.7), (to_camera, 0.6)]));
    let fill = unit(mix([(right, 0.7), (up, -0.1), (to_camera, 0.5)]));
    let (columns, rows) = (width * samples, height * samples);
    let fine = scale * samples as f32;
    let shift = centre.map(|v| v * samples as f32);

    let mut depth = vec![f32::NEG_INFINITY; columns * rows];
    let mut light = vec![0u8; columns * rows];
    for triangle in triangles {
        let [a, b, c] = triangle.map(|p| {
            let from_pivot = minus(p, framing.pivot);
            [dot(from_pivot, right) * fine + shift[0], shift[1] - dot(from_pivot, up) * fine, dot(from_pivot, to_camera)]
        });
        let edge = |from: Point, to: Point, x: f32, y: f32| (to[0] - from[0]) * (y - from[1]) - (to[1] - from[1]) * (x - from[0]);
        let area = edge(a, b, c[0], c[1]);
        if area == 0.0 || !area.is_finite() { continue; }
        let mut normal = unit(cross(minus(triangle[1], triangle[0]), minus(triangle[2], triangle[0])));
        // Models often have some faces inside out; these are lit like the rest.
        if dot(normal, to_camera) < 0.0 { normal = normal.map(|v| -v); }
        let brightness = 0.30 + 0.60 * dot(normal, key).max(0.0) + 0.20 * dot(normal, fill).max(0.0);
        let shade = (brightness.min(1.0) * 255.0) as u8;
        let first = |values: [f32; 3]| values.into_iter().fold(f32::MAX, f32::min).floor().max(0.0) as usize;
        let last = |values: [f32; 3], limit: usize| (values.into_iter().fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(limit - 1);
        for y in first([a[1], b[1], c[1]])..=last([a[1], b[1], c[1]], rows) {
            for x in first([a[0], b[0], c[0]])..=last([a[0], b[0], c[0]], columns) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (u, v, w) = (edge(b, c, px, py) / area, edge(c, a, px, py) / area, edge(a, b, px, py) / area);
                if u < 0.0 || v < 0.0 || w < 0.0 { continue; }
                let z = u * a[2] + v * b[2] + w * c[2];
                let at = y * columns + x;
                if z > depth[at] {
                    depth[at] = z;
                    light[at] = shade.max(1);
                }
            }
        }
    }

    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let (mut sum, mut covered) = (0u32, 0u32);
            for sy in 0..samples {
                for sx in 0..samples {
                    let sample = light[(y * samples + sy) * columns + x * samples + sx];
                    if sample > 0 { sum += sample as u32; covered += 1; }
                }
            }
            let shade = sum as f32 / covered.max(1) as f32 / 255.0;
            rgba.extend(MODEL_COLOR.map(|c| (c * shade) as u8));
            rgba.push((covered * 255 / (samples * samples) as u32) as u8);
        }
    }
    Picture { size: [width as u32, height as u32], delays: Vec::new(), rgba }
}

/// The last model read, so that each new look at it does not read the file again.
struct Held {
    file: PathBuf,
    stamp: (u64, Option<SystemTime>),
    triangles: Arc<Vec<Triangle>>,
}

static HELD: Mutex<Option<Held>> = Mutex::new(None);

fn stamp(file: &Path) -> Option<(u64, Option<SystemTime>)> {
    std::fs::metadata(file).ok().map(|m| (m.len(), m.modified().ok()))
}

fn held(file: &Path) -> Option<Arc<Vec<Triangle>>> {
    let now = stamp(file);
    HELD.lock().unwrap().as_ref().filter(|h| h.file == file && Some(h.stamp) == now).map(|h| h.triangles.clone())
}

fn hold(file: &Path, triangles: Vec<Triangle>) -> Arc<Vec<Triangle>> {
    let triangles = Arc::new(triangles);
    if let Some(stamp) = stamp(file) {
        *HELD.lock().unwrap() = Some(Held { file: file.to_path_buf(), stamp, triangles: triangles.clone() });
    }
    triangles
}

pub fn stl(file: &Path, options: Options) -> Reply {
    if let Some(triangles) = held(file) { return shown(&triangles, options); }
    if too_large(file) { return failed(TOO_LARGE); }
    match read_stl(file) {
        Ok(triangles) => shown(&hold(file, triangles), options),
        Err(message) => failed(message),
    }
}

fn read_stl(file: &Path) -> Result<Vec<Triangle>, String> {
    const UNREADABLE: &str = "This STL file could not be read";
    let open = || std::fs::File::open(file).map(|f| BufReader::with_capacity(1 << 16, f)).map_err(|e| fsx::friendly_err(&e));
    let length = std::fs::metadata(file).map_err(|e| fsx::friendly_err(&e))?.len();
    let mut reader = open()?;
    let mut head = Vec::with_capacity(84);
    (&mut reader).take(84).read_to_end(&mut head).map_err(|e| fsx::friendly_err(&e))?;
    let count = if head.len() == 84 { u32::from_le_bytes([head[80], head[81], head[82], head[83]]) as u64 } else { 0 };
    let expected = 84 + 50 * count;
    // Some binary files start with "solid" too; only their exact length gives them away.
    let text = head.trim_ascii_start().starts_with(b"solid");
    if head.len() == 84 && (expected == length || (!text && count > 0 && expected <= length)) {
        if count as usize > MAX_TRIANGLES { return Err(TOO_LARGE.into()); }
        let mut triangles = Vec::with_capacity(count as usize);
        let mut record = [0u8; 50];
        for _ in 0..count {
            reader.read_exact(&mut record).map_err(|_| UNREADABLE.to_string())?;
            let number = |at: usize| f32::from_le_bytes([record[at], record[at + 1], record[at + 2], record[at + 3]]);
            let triangle = [12, 24, 36].map(|at| [number(at), number(at + 4), number(at + 8)]);
            if usable(&triangle) { triangles.push(triangle); }
        }
        return Ok(triangles);
    }
    if !text { return Err(UNREADABLE.into()); }
    let mut reader = open()?;
    let (mut line, mut corners, mut triangles) = (Vec::new(), Vec::with_capacity(3), Vec::new());
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(|e| fsx::friendly_err(&e))? == 0 { break; }
        let Some(rest) = line.trim_ascii().strip_prefix(b"vertex") else { continue };
        let mut numbers = std::str::from_utf8(rest).map_err(|_| UNREADABLE.to_string())?.split_ascii_whitespace().map(str::parse::<f32>);
        let mut next = || numbers.next().and_then(Result::ok).ok_or_else(|| UNREADABLE.to_string());
        corners.push([next()?, next()?, next()?]);
        if corners.len() == 3 {
            let triangle = [corners[0], corners[1], corners[2]];
            corners.clear();
            if usable(&triangle) { triangles.push(triangle); }
            if triangles.len() > MAX_TRIANGLES { return Err(TOO_LARGE.into()); }
        }
    }
    Ok(triangles)
}

/// A 3MF transform: the rows of a 4×3 matrix, applied to a point on the left.
type Transform = [f32; 12];
const IDENTITY: Transform = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn turned(m: &Transform, p: Point) -> Point {
    std::array::from_fn(|i| p[0] * m[i] + p[1] * m[3 + i] + p[2] * m[6 + i])
}

fn moved(m: &Transform, p: Point) -> Point {
    let r = turned(m, p);
    [r[0] + m[9], r[1] + m[10], r[2] + m[11]]
}

/// `inner` first, then `outer`.
fn combined(inner: &Transform, outer: &Transform) -> Transform {
    let rows = [turned(outer, [inner[0], inner[1], inner[2]]), turned(outer, [inner[3], inner[4], inner[5]]), turned(outer, [inner[6], inner[7], inner[8]]), moved(outer, [inner[9], inner[10], inner[11]])];
    std::array::from_fn(|i| rows[i / 3][i % 3])
}

/// A part of the package and an object in it.
type ObjectKey = (String, u32);

struct Reference {
    target: ObjectKey,
    transform: Transform,
}

#[derive(Default)]
struct Object {
    vertices: Vec<Point>,
    faces: Vec<[u32; 3]>,
    parts: Vec<Reference>,
}

type Zip = zip::ZipArchive<BufReader<std::fs::File>>;

fn part_name(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

/// What an element refers to: an object, possibly in another part of the package.
fn reference(e: &BytesStart, id_name: &str, part: &str) -> Option<Reference> {
    let id = attribute(e, id_name)?.parse().ok()?;
    let target = attribute(e, "path").map_or_else(|| part.to_string(), |path| part_name(&path));
    let transform = attribute(e, "transform").and_then(|text| {
        let numbers: Vec<f32> = text.split_ascii_whitespace().map_while(|n| n.parse().ok()).collect();
        <[f32; 12]>::try_from(numbers).ok()
    }).unwrap_or(IDENTITY);
    Some(Reference { target: (target, id), transform })
}

fn read_part(zip: &mut Zip, part: &str, objects: &mut HashMap<ObjectKey, Object>, items: &mut Vec<Reference>) -> Result<(), String> {
    const UNREADABLE: &str = "This 3MF file could not be read";
    let Ok(entry) = zip.by_name(part) else { return Ok(()) };
    let mut reader = Reader::from_reader(BufReader::with_capacity(1 << 16, entry.take(MAX_MODEL_BYTES)));
    let (mut buffer, mut current, mut vertices_seen) = (Vec::new(), None::<(u32, Object)>, 0usize);
    loop {
        buffer.clear();
        let event = reader.read_event_into(&mut buffer).map_err(|_| UNREADABLE.to_string())?;
        match event {
            Event::Eof => return Ok(()),
            Event::End(e) if e.local_name().as_ref() == "object" => {
                if let Some((id, object)) = current.take() { objects.insert((part.to_string(), id), object); }
            }
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "object" => current = attribute(&e, "id").and_then(|id| id.parse().ok()).map(|id| (id, Object::default())),
                "vertex" => if let Some((_, object)) = &mut current {
                    let mut point = [f32::NAN; 3];
                    for a in e.attributes().with_checks(false).flatten() {
                        let slot = match a.key.as_ref() { "x" => 0, "y" => 1, "z" => 2, _ => continue };
                        point[slot] = a.value.parse().unwrap_or(f32::NAN);
                    }
                    vertices_seen += 1;
                    if vertices_seen > MAX_TRIANGLES * 3 { return Err(TOO_LARGE.into()); }
                    object.vertices.push(point);
                },
                "triangle" => if let Some((_, object)) = &mut current {
                    let mut face = [u32::MAX; 3];
                    for a in e.attributes().with_checks(false).flatten() {
                        let slot = match a.key.as_ref() { "v1" => 0, "v2" => 1, "v3" => 2, _ => continue };
                        face[slot] = a.value.parse().unwrap_or(u32::MAX);
                    }
                    if object.faces.len() >= MAX_TRIANGLES { return Err(TOO_LARGE.into()); }
                    object.faces.push(face);
                },
                "component" => if let Some((_, object)) = &mut current {
                    object.parts.extend(reference(&e, "objectid", part));
                },
                "item" => items.extend(reference(&e, "objectid", part)),
                _ => {}
            },
            _ => {}
        }
    }
}

fn root_part(zip: &mut Zip) -> String {
    let mut text = String::new();
    if let Ok(entry) = zip.by_name("_rels/.rels") { let _ = entry.take(1 << 20).read_to_string(&mut text); }
    let mut reader = Reader::from_str(&text);
    while let Ok(event) = reader.read_event() {
        match event {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" && attribute(&e, "Type").is_some_and(|t| t.ends_with("3dmodel")) => {
                if let Some(target) = attribute(&e, "Target") { return part_name(&target); }
            }
            _ => {}
        }
    }
    "3D/3dmodel.model".into()
}

fn collect(objects: &HashMap<ObjectKey, Object>, reference: &Reference, outer: &Transform, nesting: u32, visits: &mut usize, out: &mut Vec<Triangle>) -> Result<(), String> {
    *visits += 1;
    if *visits > MAX_VISITS || nesting > MAX_NESTING { return Err(TOO_LARGE.into()); }
    let Some(object) = objects.get(&reference.target) else { return Ok(()) };
    let transform = combined(&reference.transform, outer);
    if out.len() + object.faces.len() > MAX_TRIANGLES { return Err(TOO_LARGE.into()); }
    for face in &object.faces {
        let corner = |n: usize| object.vertices.get(face[n] as usize).map(|p| moved(&transform, *p));
        if let (Some(a), Some(b), Some(c)) = (corner(0), corner(1), corner(2)) && usable(&[a, b, c]) { out.push([a, b, c]); }
    }
    for part in &object.parts { collect(objects, part, &transform, nesting + 1, visits, out)?; }
    Ok(())
}

fn read_3mf(zip: &mut Zip) -> Result<Vec<Triangle>, String> {
    let root = root_part(zip);
    let (mut objects, mut items) = (HashMap::new(), Vec::new());
    read_part(zip, &root, &mut objects, &mut items)?;
    let mut read = vec![root];
    loop {
        // Only what the parts read so far refer to; a package holds other files too.
        let mut wanted: Vec<String> = objects.values().flat_map(|o: &Object| o.parts.iter()).chain(items.iter()).map(|r| r.target.0.clone()).filter(|part| !read.contains(part)).collect();
        wanted.sort();
        wanted.dedup();
        if wanted.is_empty() { break; }
        for part in wanted {
            if read.len() >= MAX_PARTS { return Err(TOO_LARGE.into()); }
            read_part(zip, &part, &mut objects, &mut items)?;
            read.push(part);
        }
    }
    let (mut visits, mut triangles) = (0, Vec::new());
    for item in &items { collect(&objects, item, &IDENTITY, 0, &mut visits, &mut triangles)?; }
    Ok(triangles)
}

/// The picture a slicer stored, for packages that keep only the print and not the shape.
fn thumbnail(zip: &mut Zip, options: Options) -> Option<Reply> {
    for name in ["Metadata/plate_1.png", "Metadata/thumbnail.png", "Thumbnails/thumbnail.png", "thumbnail.png"] {
        let Ok(entry) = zip.by_name(name) else { continue };
        let mut bytes = Vec::new();
        if entry.take(MAX_THUMBNAIL_BYTES).read_to_end(&mut bytes).is_err() { continue; }
        let Ok(image) = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png) else { continue };
        let orig = [image.width(), image.height()];
        return Some((Loaded::Image(orig), Some(to_picture(image, options.max_side))));
    }
    None
}

pub fn three_mf(file: &Path, options: Options) -> Reply {
    if let Some(triangles) = held(file) { return shown(&triangles, options); }
    if too_large(file) { return failed(TOO_LARGE); }
    let opened = std::fs::File::open(file).map_err(|e| fsx::friendly_err(&e))
        .and_then(|f| zip::ZipArchive::new(BufReader::new(f)).map_err(|_| "This 3MF file could not be read".to_string()));
    let mut zip = match opened {
        Ok(zip) => zip,
        Err(message) => return failed(message),
    };
    match read_3mf(&mut zip) {
        Ok(triangles) if !triangles.is_empty() => shown(&hold(file, triangles), options),
        Ok(_) => thumbnail(&mut zip, options).unwrap_or_else(|| failed(NO_SHAPE)),
        Err(message) => failed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::super::load::{load, scratch, whole};
    use super::*;
    use std::io::Write;

    const TETRA: [Triangle; 4] = [
        [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
        [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 0.0, 10.0]],
        [[0.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
        [[10.0, 0.0, 0.0], [0.0, 10.0, 0.0], [0.0, 0.0, 10.0]],
    ];

    fn binary_stl(header: &[u8; 80]) -> Vec<u8> {
        let mut bytes = header.to_vec();
        bytes.extend((TETRA.len() as u32).to_le_bytes());
        for triangle in TETRA {
            bytes.extend([0u8; 12]);
            for v in triangle.iter().flatten() { bytes.extend(v.to_le_bytes()); }
            bytes.extend([0u8; 2]);
        }
        bytes
    }

    fn package(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const CUBE_CORNER: &str = r#"<mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="1" y="0" z="0"/><vertex x="0" y="1" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/><triangle v1="0" v2="1" v3="99"/></triangles></mesh>"#;

    fn triangles_of(bytes: Vec<u8>) -> Result<Vec<Triangle>, String> {
        read_3mf(&mut zip::ZipArchive::new(BufReader::new(temp_file(bytes))).unwrap())
    }

    fn temp_file(bytes: Vec<u8>) -> std::fs::File {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!("fc_model_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&path, bytes).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        file
    }

    #[test]
    fn stl_in_both_forms_becomes_a_picture_of_the_shape() {
        let root = scratch("stl");
        let mut ascii = String::from("solid x\n");
        for t in TETRA {
            ascii.push_str("facet normal 0 0 0\nouter loop\n");
            for v in t { ascii.push_str(&format!("  vertex {} {} {}\n", v[0], v[1], v[2])); }
            ascii.push_str("endloop\nendfacet\n");
        }
        ascii.push_str("endsolid x\n");
        let mut misleading = [0u8; 80];
        misleading[..5].copy_from_slice(b"solid");
        for (name, bytes) in [("text.stl", ascii.into_bytes()), ("binary.stl", binary_stl(&[0; 80])), ("solid.stl", binary_stl(&misleading))] {
            let file = root.join(name);
            std::fs::write(&file, bytes).unwrap();
            let (loaded, picture) = load(&file, whole(256));
            let picture = picture.unwrap_or_else(|| panic!("{name}: {loaded:?}"));
            assert!(matches!(loaded, Loaded::Model(orig) if orig[0] < picture.size[0]), "{name}: fills the window");
            assert_eq!(picture.rgba.len(), (picture.size[0] * picture.size[1] * 4) as usize);
            assert_eq!(picture.rgba[3], 0, "{name}: nothing behind the model");
            assert!(picture.rgba.chunks_exact(4).any(|p| p[3] == 255 && p[0] > 0), "{name}: the model is drawn");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_model_can_be_looked_at_from_elsewhere() {
        let root = scratch("turn");
        let file = root.join("shape.stl");
        std::fs::write(&file, binary_stl(&[0; 80])).unwrap();
        let view = ModelView { turn: 0, tilt: 0, zoom: 100, pan: [0; 2], size: [200, 150], fit: 100, fine: true };
        let at = |view| { let options = Options { view: Some(view), ..whole(256) }; let (loaded, picture) = load(&file, options); assert!(matches!(loaded, Loaded::Detail), "{loaded:?}"); picture.unwrap() };
        let plain = at(view);
        assert_eq!(plain.size, [200, 150], "the size asked for");
        assert_ne!(plain.rgba, at(ModelView { turn: 900, ..view }).rgba, "turning shows another side");
        assert_ne!(plain.rgba, at(ModelView { zoom: 200, ..view }).rgba);
        assert_eq!(at(ModelView { fine: false, ..view }).size, [200, 150], "a small model stays full size while moving");
        let huge = at(ModelView { size: [u16::MAX, u16::MAX], zoom: u16::MAX, pan: [i16::MIN, i16::MAX], ..view });
        assert!(huge.size[0] <= 256, "the helper keeps to its own limits");
        std::fs::remove_file(&file).unwrap();
        let again = load(&file, Options { view: Some(view), ..whole(256) }).0;
        assert!(matches!(again, Loaded::Error(_)), "a file that is gone is not shown from memory");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_stl_files_are_refused() {
        let root = scratch("stl_bad");
        for (name, bytes) in [("short.stl", vec![1u8; 20]), ("cut.stl", binary_stl(&[0; 80])[..120].to_vec()), ("text.stl", b"solid x\nvertex 1 two 3\n".to_vec()), ("flat.stl", b"solid x\nendsolid x\n".to_vec())] {
            let file = root.join(name);
            std::fs::write(&file, bytes).unwrap();
            assert!(matches!(load(&file, whole(64)).0, Loaded::Error(_)), "{name}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_3mf_follows_components_across_parts_and_applies_transforms() {
        let root_model = br#"<model xmlns:p="production"><resources><object id="1"><components><component p:path="/3D/Objects/a.model" objectid="7" transform="1 0 0 0 1 0 0 0 1 5 0 0"/></components></object></resources><build><item objectid="1" transform="2 0 0 0 2 0 0 0 2 0 0 1"/></build></model>"#;
        let part = format!(r#"<model><resources><object id="7">{CUBE_CORNER}</object></resources></model>"#);
        let rels = br#"<Relationships><Relationship Target="/3D/3dmodel.model" Id="r" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;
        let triangles = triangles_of(package(&[("_rels/.rels", rels), ("3D/3dmodel.model", root_model), ("3D/Objects/a.model", part.as_bytes())])).unwrap();
        assert_eq!(triangles.len(), 1, "a face naming a missing vertex is left out");
        assert_eq!(triangles[0], [[10.0, 0.0, 1.0], [12.0, 0.0, 1.0], [10.0, 2.0, 1.0]], "the part's move, then the item's scale and lift");
    }

    #[test]
    fn a_3mf_that_refers_to_itself_ends() {
        let model = br#"<model><resources><object id="1"><components><component objectid="1"/><component objectid="1"/></components></object></resources><build><item objectid="1"/></build></model>"#;
        assert!(triangles_of(package(&[("3D/3dmodel.model", model)])).is_err());
    }

    #[test]
    fn a_3mf_without_a_shape_shows_its_thumbnail_or_says_so() {
        let root = scratch("3mf");
        let model = br#"<model><resources/><build/></model>"#;
        let mut png = Vec::new();
        image::RgbaImage::new(6, 4).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let (with, without) = (root.join("with.3mf"), root.join("without.3mf"));
        std::fs::write(&with, package(&[("3D/3dmodel.model", model), ("Metadata/plate_1.png", &png)])).unwrap();
        std::fs::write(&without, package(&[("3D/3dmodel.model", model)])).unwrap();
        assert!(matches!(load(&with, whole(64)).0, Loaded::Image([6, 4])));
        assert!(matches!(load(&without, whole(64)).0, Loaded::Error(_)));
        std::fs::write(&without, b"PK not a package").unwrap();
        assert!(matches!(load(&without, whole(64)).0, Loaded::Error(_)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
