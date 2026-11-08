//! The quick preview: what is shown, what is remembered and what gets loaded next.
//!
//! Files are parsed in a helper process (see `helper.rs`), so a file that breaks a parser
//! costs one preview and not the application with its running copies.

mod archive_list;
pub mod helper;
pub mod load;
mod media;
mod model;
mod office;
mod pdf;
mod picture;
pub mod text;
pub mod window;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use crate::app::App;
use crate::fsx::Entry;
use load::{ArchiveEntry, Block, Colours, Loaded, ModelView, Options, Picture, Reply};

/// Longest side of a decoded picture; `MAX_TEXTURE_SIDE` in `main.rs` limits it from above.
pub const MAX_SIDE: u32 = 2048;
/// Decoded previews kept for going back and forth. Textures count: on a machine with
/// integrated graphics they are ordinary memory.
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_ITEMS: usize = 64;
/// A preview left alone this long gives its memory back.
const CACHE_IDLE: Duration = Duration::from_secs(60);
/// A running job is allowed to finish, for the cache, unless it is this slow.
const ABANDON_AFTER: Duration = Duration::from_millis(200);
/// One file may not occupy a lane for longer than this.
const JOB_LIMIT: Duration = Duration::from_secs(15);
/// A lane without work gives its helper's memory back.
const IDLE_LIMIT: Duration = Duration::from_secs(30);
/// Sooner for the neighbours, whose helper has nothing to come back to.
const NEIGHBOURS_IDLE_LIMIT: Duration = Duration::from_secs(10);
/// Files read ahead of being asked for stay below this, and 3D models are not read at all:
/// a neighbour may never be looked at.
const PREFETCH_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Neighbours wait until the selection stops moving.
pub const PREFETCH_DELAY: Duration = Duration::from_millis(150);
/// Arrowing past videos should not start ffmpeg several times for each of them.
const FILM_DELAY: Duration = Duration::from_millis(400);

/// A file as the listing knows it. Asking the disk here could stall the interface on a
/// sleeping share; a changed date or size makes a new key, so changed files load again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Key {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
    pub size: u64,
    pub page: u32,
    pub crop: Option<[u16; 4]>,
    pub view: Option<ModelView>,
}

impl Key {
    /// Worth reading before it is known to be wanted.
    fn light(&self) -> bool {
        let model = self.path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("stl") || ext.eq_ignore_ascii_case("3mf"));
        self.size <= PREFETCH_MAX_BYTES && !model
    }

    fn options(&self) -> Options {
        Options { max_side: MAX_SIDE, page: self.page, crop: self.crop, view: self.view }
    }

    pub fn of(e: &Entry) -> Key {
        Key { path: e.path.clone(), modified: e.modified, size: e.size, page: 0, crop: None, view: None }
    }
}

/// Pictures and how many milliseconds each one stays.
pub type Frames = Arc<Vec<(egui::TextureHandle, u32)>>;

/// Text with its line positions, so drawing can go straight to the visible lines.
pub struct TextDoc {
    pub text: String,
    /// Byte range of each line.
    pub lines: Vec<(u32, u32)>,
    /// Characters in the longest line.
    pub longest: usize,
    pub trimmed: bool,
    /// Colours per line; lines beyond the end of this have none.
    pub spans: Colours,
}

impl TextDoc {
    fn bytes(&self) -> usize {
        self.text.len() + self.lines.len() * 8 + self.spans.spans.len() * 8 + self.spans.ends.len() * 4
    }

    pub fn new(text: String, trimmed: bool, spans: Colours) -> TextDoc {
        let mut lines = Vec::new();
        let mut longest = 0;
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            let shown = line.trim_end_matches(['\n', '\r']);
            longest = longest.max(shown.chars().count());
            lines.push((at as u32, (at + shown.len()) as u32));
            at += line.len();
        }
        TextDoc { text, lines, longest, trimmed, spans }
    }

    /// One row per line, or several rows per line when wrapped to `columns`.
    pub fn rows(&self, columns: Option<usize>) -> Vec<(u32, u32, u32)> {
        let mut rows = Vec::with_capacity(self.lines.len());
        for (n, &(start, end)) in self.lines.iter().enumerate() {
            let Some(columns) = columns else {
                rows.push((n as u32, start, end));
                continue;
            };
            let line = &self.text[start as usize..end as usize];
            let (mut from, mut count) = (0, 0);
            for (at, _) in line.char_indices() {
                if count == columns {
                    rows.push((n as u32, start + from as u32, start + at as u32));
                    (from, count) = (at, 0);
                }
                count += 1;
            }
            rows.push((n as u32, start + from as u32, end));
        }
        rows
    }
}

#[derive(Clone)]
pub enum PreviewContent {
    None,
    Loading,
    Text(Arc<TextDoc>),
    /// Frames with their durations, and the original size.
    Animation(Frames, [u32; 2]),
    Page { texture: egui::TextureHandle, orig: [u32; 2], page: u32, pages: u32 },
    /// A sharper copy of the part of the shown picture named by the job.
    Detail(egui::TextureHandle),
    Doc(Arc<DocData>),
    Hex(Arc<Vec<u8>>, u64),
    Table(Arc<TableDoc>),
    Archive(Arc<Vec<ArchiveEntry>>, u64, bool),
    /// Facts about a file, and its cover or the frames of a video.
    Info(Arc<Vec<(String, String)>>, Option<(Frames, [u32; 2])>, bool),
    /// The texture, the original size, and whether anything in it is see-through.
    Image(egui::TextureHandle, [u32; 2], bool),
    /// The first picture of a 3D model, and its size in points.
    Model(egui::TextureHandle, [u32; 2]),
    Folder { folders: usize, files: usize, size: u64 },
    Error(String),
}

pub struct DocData {
    pub blocks: Vec<Block>,
    pub source: Option<Arc<TextDoc>>,
    pub trimmed: bool,
    pub cover: Option<(egui::TextureHandle, [u32; 2])>,
}

pub struct TableDoc {
    pub rows: Vec<Vec<String>>,
    /// Width in characters, kept within reason.
    pub widths: Vec<usize>,
    pub sheets: Vec<String>,
    pub page: u32,
    pub trimmed: bool,
}

impl PreviewContent {
    fn bytes(&self) -> usize {
        match self {
            PreviewContent::Text(t) => t.bytes(),
            PreviewContent::Image(tex, ..) | PreviewContent::Model(tex, _) | PreviewContent::Page { texture: tex, .. } | PreviewContent::Detail(tex) => tex.byte_size(),
            PreviewContent::Animation(frames, _) => frames_bytes(frames),
            PreviewContent::Doc(doc) => {
                let blocks: usize = doc.blocks.iter().map(|b| b.text.len() + 32).sum();
                blocks + doc.source.as_ref().map_or(0, |s| s.bytes()) + doc.cover.as_ref().map_or(0, |(tex, _)| tex.byte_size())
            }
            PreviewContent::Hex(bytes, _) => bytes.len(),
            PreviewContent::Table(table) => table.rows.iter().flatten().map(|c| c.len() + 24).sum(),
            PreviewContent::Archive(entries, ..) => entries.iter().map(|e| e.name.len() + 48).sum(),
            PreviewContent::Info(rows, cover, _) => {
                let text: usize = rows.iter().map(|(k, v)| k.len() + v.len() + 48).sum();
                text + cover.as_ref().map_or(0, |(frames, _)| frames_bytes(frames))
            }
            _ => 64,
        }
    }

    /// A failure may be temporary, so it is tried again next time.
    fn remembered(&self) -> bool {
        !matches!(self, PreviewContent::None | PreviewContent::Loading | PreviewContent::Error(_))
    }
}

fn frames_bytes(frames: &Frames) -> usize {
    frames.iter().map(|(tex, _)| tex.byte_size()).sum()
}

struct Item {
    key: Key,
    content: PreviewContent,
    bytes: usize,
    used: Instant,
}

/// Least recently used first.
#[derive(Default)]
struct Cache {
    items: Vec<Item>,
}

impl Cache {
    fn get(&mut self, key: &Key) -> Option<PreviewContent> {
        let at = self.items.iter().position(|item| item.key == *key)?;
        let mut item = self.items.remove(at);
        item.used = Instant::now();
        let content = item.content.clone();
        self.items.push(item);
        Some(content)
    }

    fn contains(&self, key: &Key) -> bool {
        self.items.iter().any(|item| item.key == *key)
    }

    fn insert(&mut self, key: Key, content: PreviewContent) {
        self.items.retain(|item| item.key != key);
        let bytes = content.bytes();
        // Remembering it would push out everything else; the shown copy stays on screen.
        if bytes > CACHE_BYTES / 2 { return; }
        self.items.push(Item { key, content, bytes, used: Instant::now() });
        let mut total: usize = self.items.iter().map(|item| item.bytes).sum();
        while self.items.len() > CACHE_ITEMS || total > CACHE_BYTES {
            total -= self.items.remove(0).bytes;
        }
    }

    fn expire(&mut self) {
        self.items.retain(|item| item.used.elapsed() < CACHE_IDLE);
    }
}

#[derive(Clone)]
struct Job {
    key: Key,
    ctx: egui::Context,
    prefetch: bool,
}

/// What a lane should do next. Each request replaces the waiting ones, so rapid selection
/// changes never queue decodes or create more workers.
#[derive(Default)]
struct Slot {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
}

impl Slot {
    fn replace(&self, jobs: Vec<Job>) {
        *self.jobs.lock().unwrap() = jobs.into();
        self.wake.notify_one();
    }

    /// True when something else is waiting and this job is no longer asked for.
    fn superseded(&self, key: &Key) -> bool {
        let jobs = self.jobs.lock().unwrap();
        !jobs.is_empty() && !jobs.iter().any(|j| j.key == *key)
    }
}

type Start = fn() -> std::io::Result<helper::Helper>;
/// Tests have no helper binary to start: their executable is the test runner.
#[cfg(not(test))]
const START: Option<Start> = Some(helper::Helper::start);
#[cfg(test)]
const START: Option<Start> = None;

fn run_lane(slot: Arc<Slot>, results: mpsc::Sender<(Key, PreviewContent)>, start: Option<Start>, idle: Duration) {
    let mut running: Option<helper::Helper> = None;
    loop {
        let job = {
            let mut jobs = slot.jobs.lock().unwrap();
            loop {
                if let Some(job) = jobs.pop_front() { break job; }
                let (guard, wait) = slot.wake.wait_timeout(jobs, idle).unwrap();
                jobs = guard;
                if wait.timed_out() && jobs.is_empty() {
                    running = None;
                    // Only this thread still holds it: the application is gone.
                    if Arc::strong_count(&slot) == 1 { return; }
                }
            }
        };
        if job.prefetch && crate::drives::foreign_place(&job.key.path).is_some() { continue; }
        let started = Instant::now();
        let reply = match start {
            Some(start) => {
                // The next look at a model needs this helper's copy of it, so it is not ended.
                let stale = || job.key.view.is_none() && started.elapsed() > ABANDON_AFTER && slot.superseded(&job.key);
                match helper::ask(&mut running, start, &job.key.path, job.key.options(), JOB_LIMIT, &stale) {
                    Some(reply) => reply,
                    None => continue,
                }
            }
            None => load::load(&job.key.path, job.key.options()),
        };
        if trace() {
            eprintln!("[preview] {} loaded in {} ms", job.key.path.display(), started.elapsed().as_millis());
        }
        // Texture creation is thread safe, and this conversion is too slow for the interface.
        let content = content_of(reply, &job.ctx);
        if results.send((job.key, content)).is_err() { return; }
        // Not the current viewport: the list is what takes the result and passes it on.
        job.ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

fn texture(rgba: &[u8], size: [u32; 2], ctx: &egui::Context) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([size[0] as usize, size[1] as usize], rgba);
    ctx.load_texture("preview", image, egui::TextureOptions::LINEAR)
}

fn picture_content(p: Picture, orig: [u32; 2], ctx: &egui::Context) -> PreviewContent {
    if p.delays.is_empty() {
        return PreviewContent::Image(texture(&p.rgba, p.size, ctx), orig, p.rgba.iter().skip(3).step_by(4).any(|&alpha| alpha < 255));
    }
    PreviewContent::Animation(frames_of(&p, ctx), orig)
}

/// A still picture is one frame that never ends.
fn frames_of(p: &Picture, ctx: &egui::Context) -> Frames {
    if p.delays.is_empty() { return Arc::new(vec![(texture(&p.rgba, p.size, ctx), 0)]); }
    let frame_bytes = p.size[0] as usize * p.size[1] as usize * 4;
    Arc::new(p.rgba.chunks_exact(frame_bytes).zip(&p.delays).map(|(rgba, &ms)| (texture(rgba, p.size, ctx), ms)).collect())
}

fn content_of((loaded, picture): Reply, ctx: &egui::Context) -> PreviewContent {
    match (loaded, picture) {
        (Loaded::Text { text, trimmed, spans }, _) => PreviewContent::Text(Arc::new(TextDoc::new(text, trimmed, spans))),
        (Loaded::Image(orig), Some(p)) => picture_content(p, orig, ctx),
        (Loaded::Model(orig), Some(p)) => PreviewContent::Model(texture(&p.rgba, p.size, ctx), orig),
        (Loaded::Page { orig, page, pages }, Some(p)) => PreviewContent::Page { texture: texture(&p.rgba, p.size, ctx), orig, page, pages },
        (Loaded::Detail, Some(p)) => PreviewContent::Detail(texture(&p.rgba, p.size, ctx)),
        (Loaded::Image(_) | Loaded::Model(_) | Loaded::Page { .. } | Loaded::Detail, None) => PreviewContent::Error("The picture is missing".into()),
        (Loaded::Doc { blocks, source, trimmed }, picture) => {
            let source = source.map(|s| Arc::new(TextDoc::new(s, trimmed, Colours::default())));
            let cover = picture.map(|p| (texture(&p.rgba, p.size, ctx), p.size));
            PreviewContent::Doc(Arc::new(DocData { blocks, source, trimmed, cover }))
        }
        (Loaded::Table { rows, sheets, page, trimmed }, _) => {
            let mut widths: Vec<usize> = Vec::new();
            for row in &rows {
                if row.len() > widths.len() { widths.resize(row.len(), 3); }
                for (width, cell) in widths.iter_mut().zip(row) { *width = (*width).max(cell.chars().count().min(40)); }
            }
            PreviewContent::Table(Arc::new(TableDoc { rows, widths, sheets, page, trimmed }))
        }
        (Loaded::Archive { entries, total, trimmed }, _) => PreviewContent::Archive(Arc::new(entries), total, trimmed),
        (Loaded::Info { rows, more }, picture) => PreviewContent::Info(Arc::new(rows), picture.map(|p| (frames_of(&p, ctx), p.size)), more),
        (Loaded::Hex { bytes, total }, _) => PreviewContent::Hex(Arc::new(bytes), total),
        (Loaded::Folder { folders, files, size }, _) => PreviewContent::Folder { folders, files, size },
        (Loaded::Error(e), _) => PreviewContent::Error(e),
    }
}

/// `FERROUS_PREVIEW_TRACE=1` prints how long previews take.
fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FERROUS_PREVIEW_TRACE").is_some())
}

pub struct Preview {
    pub path: Option<PathBuf>,
    pub content: PreviewContent,
    pub page: u32,
    /// Grows when the shown preview is replaced by a better one of the same file.
    pub revision: u32,
    film_asked: Option<Key>,
    /// A sharper copy of part of the shown picture, and which part.
    pub detail: Option<(egui::TextureHandle, [u16; 4])>,
    key: Option<Key>,
    cache: Cache,
    tx: mpsc::Sender<(Key, PreviewContent)>,
    rx: mpsc::Receiver<(Key, PreviewContent)>,
    /// The shown file's lane, and the one for its neighbours.
    current: Arc<Slot>,
    neighbours: Arc<Slot>,
    started: [bool; 2],
    asked: Option<Instant>,
    prefetched: Option<Key>,
}

impl Preview {
    pub fn new() -> Preview {
        let (tx, rx) = mpsc::channel();
        Preview {
            path: None,
            content: PreviewContent::None,
            page: 0,
            revision: 0,
            film_asked: None,
            detail: None,
            key: None,
            cache: Cache::default(),
            tx,
            rx,
            current: Arc::default(),
            neighbours: Arc::default(),
            started: [false; 2],
            asked: None,
            prefetched: None,
        }
    }

    /// Lets go of what has not been looked at for a while.
    pub fn expire(&mut self) {
        self.cache.expire();
    }

    #[cfg(test)]
    pub(crate) fn remember(&mut self, key: Key, content: PreviewContent) {
        self.cache.insert(key, content);
    }

    pub fn page_key(&self, page: u32) -> Option<Key> {
        self.key.clone().map(|key| Key { page, ..key })
    }

    pub fn key(&self) -> Option<&Key> {
        self.key.as_ref()
    }

    fn send(&mut self, lane: usize, jobs: Vec<Job>) {
        let slot = if lane == 0 { &self.current } else { &self.neighbours };
        if !std::mem::replace(&mut self.started[lane], true) {
            let (slot, tx) = (slot.clone(), self.tx.clone());
            let idle = if lane == 0 { IDLE_LIMIT } else { NEIGHBOURS_IDLE_LIMIT };
            std::thread::spawn(move || run_lane(slot, tx, START, idle));
        }
        slot.replace(jobs);
    }
}

impl App {
    /// Starts loading in this very frame, instead of waiting for the next one to happen.
    pub fn want_preview(&mut self, mut key: Option<Key>, ctx: &egui::Context) {
        let preview = &mut self.preview;
        // The listing knows files, not pages: the page belongs to the file on show.
        match &mut key {
            Some(k) if preview.path.as_ref() == Some(&k.path) => k.page = preview.page,
            _ => preview.page = 0,
        }
        if preview.key == key { return; }
        preview.detail = None;
        // A film given up on when the selection moved on is worth asking for again.
        preview.film_asked = None;
        preview.path = key.as_ref().map(|k| k.path.clone());
        preview.key = key.clone();
        preview.asked = Some(Instant::now());
        let Some(key) = key else {
            preview.content = PreviewContent::None;
            return;
        };
        if let Some(content) = preview.cache.get(&key) {
            preview.content = content;
            return;
        }
        preview.content = PreviewContent::Loading;
        preview.send(0, vec![Job { key, ctx: ctx.clone(), prefetch: false }]);
    }

    /// Once the selection rests on a video, starts on its moving preview.
    /// Only for the preview window: the side pane has no use for it.
    pub fn want_film(&mut self, ctx: &egui::Context) {
        let preview = &mut self.preview;
        if !matches!(preview.content, PreviewContent::Info(_, _, true)) { return; }
        let (Some(key), Some(asked)) = (preview.key.clone(), preview.asked) else { return };
        if preview.film_asked.as_ref() == Some(&key) { return; }
        if asked.elapsed() < FILM_DELAY {
            ctx.request_repaint_after(FILM_DELAY);
            return;
        }
        preview.film_asked = Some(key.clone());
        let key = Key { page: media::FILM_PAGE, ..key };
        preview.send(0, vec![Job { key, ctx: ctx.clone(), prefetch: false }]);
    }

    /// Asks for a sharper copy of part of the shown picture.
    pub fn want_detail(&mut self, crop: [u16; 4], ctx: &egui::Context) {
        let Some(mut key) = self.preview.key.clone() else { return };
        key.crop = Some(crop);
        self.preview.send(0, vec![Job { key, ctx: ctx.clone(), prefetch: false }]);
    }

    /// Asks for another look at the shown 3D model.
    pub fn want_model_view(&mut self, view: ModelView, ctx: &egui::Context) {
        let Some(mut key) = self.preview.key.clone() else { return };
        key.view = Some(view);
        self.preview.send(0, vec![Job { key, ctx: ctx.clone(), prefetch: false }]);
    }

    /// Loads the files around the shown one, once the selection has settled on it.
    pub fn prefetch_preview(&mut self, neighbours: Vec<Key>, ctx: &egui::Context) {
        let preview = &mut self.preview;
        let Some(asked) = preview.asked else { return };
        if preview.prefetched == preview.key { return; }
        if asked.elapsed() < PREFETCH_DELAY {
            ctx.request_repaint_after(PREFETCH_DELAY);
            return;
        }
        preview.prefetched = preview.key.clone();
        let jobs = neighbours.into_iter().filter(|k| k.light() && !preview.cache.contains(k))
            .map(|key| Job { key, ctx: ctx.clone(), prefetch: true }).collect();
        preview.send(1, jobs);
    }

    pub(crate) fn receive_preview(&mut self) {
        let preview = &mut self.preview;
        while let Ok((key, content)) = preview.rx.try_recv() {
            if key.view.is_some() {
                // A failed look leaves the last good one on screen.
                let current = preview.key.as_ref().is_some_and(|k| k.path == key.path);
                if let (true, PreviewContent::Detail(texture)) = (current, &content) { preview.detail = Some((texture.clone(), [0, 0, 65535, 65535])); }
                continue;
            }
            if let (Some(crop), PreviewContent::Detail(texture)) = (key.crop, &content) {
                let current = preview.key.as_ref().is_some_and(|k| k.path == key.path && k.page == key.page);
                if current { preview.detail = Some((texture.clone(), crop)); }
                continue;
            }
            if key.page == media::FILM_PAGE && matches!(content, PreviewContent::Info(..)) {
                // Stands in for the single frame, here and for later visits.
                let still = Key { page: 0, ..key };
                preview.cache.insert(still.clone(), content.clone());
                if preview.key.as_ref() == Some(&still) {
                    preview.content = content;
                    preview.revision += 1;
                }
                continue;
            }
            if content.remembered() { preview.cache.insert(key.clone(), content.clone()); }
            if preview.key.as_ref() == Some(&key) {
                if trace() && let Some(asked) = preview.asked {
                    eprintln!("[preview] {} ready after {} ms", key.path.display(), asked.elapsed().as_millis());
                }
                preview.content = content;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key { path: PathBuf::from(name), modified: None, size: 0, page: 0, crop: None, view: None }
    }

    fn job(name: &str, ctx: &egui::Context) -> Job {
        Job { key: key(name), ctx: ctx.clone(), prefetch: false }
    }

    #[test]
    fn waiting_requests_collapse_into_the_latest() {
        let ctx = egui::Context::default();
        let slot = Slot::default();
        for i in 0..1000 { slot.replace(vec![job(&format!("f{i}"), &ctx)]); }
        assert_eq!(slot.jobs.lock().unwrap().len(), 1);
        assert!(slot.superseded(&key("f0")));
        assert!(!slot.superseded(&key("f999")));
        slot.jobs.lock().unwrap().clear();
        assert!(!slot.superseded(&key("f0")), "nothing else is waiting for the lane");
    }

    #[test]
    fn cache_keeps_recent_items_within_its_limits() {
        let mut cache = Cache::default();
        for i in 0..CACHE_ITEMS + 5 { cache.insert(key(&format!("f{i}")), PreviewContent::Hex(Arc::default(), 0)); }
        assert_eq!(cache.items.len(), CACHE_ITEMS);
        assert!(!cache.contains(&key("f0")));
        assert!(cache.get(&key("f5")).is_some());
        cache.insert(key("new"), PreviewContent::Hex(Arc::default(), 0));
        assert!(cache.contains(&key("f5")), "using an item keeps it");
        let huge = Arc::new(TextDoc::new("x".repeat(CACHE_BYTES), false, Colours::default()));
        cache.insert(key("huge"), PreviewContent::Text(huge));
        assert!(!cache.contains(&key("huge")), "an oversized preview is not remembered");
        assert!(cache.contains(&key("f5")), "and does not push out the others");
    }

    #[test]
    fn previews_left_alone_are_forgotten() {
        let mut cache = Cache::default();
        cache.insert(key("old"), PreviewContent::Hex(Arc::default(), 0));
        cache.insert(key("new"), PreviewContent::Hex(Arc::default(), 0));
        cache.items[0].used = Instant::now() - CACHE_IDLE;
        cache.expire();
        assert!(!cache.contains(&key("old")));
        assert!(cache.contains(&key("new")));
    }

    #[test]
    fn the_budget_counts_colours() {
        let plain = PreviewContent::Text(Arc::new(TextDoc::new("a\n".into(), false, Colours::default())));
        let coloured = PreviewContent::Text(Arc::new(TextDoc::new("a\n".into(), false, Colours { spans: vec![(1, 1); 1000], ends: vec![1000] })));
        assert!(coloured.bytes() >= plain.bytes() + 8000);
    }

    #[test]
    fn text_rows_follow_lines_and_wrap_on_characters() {
        let doc = TextDoc::new("żółw\r\n\nabcdef".into(), false, Colours::default());
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.longest, 6);
        let piece = |r: &(u32, u32, u32)| doc.text[r.1 as usize..r.2 as usize].to_string();
        assert_eq!(doc.rows(None).iter().map(piece).collect::<Vec<_>>(), ["żółw", "", "abcdef"]);
        let wrapped = doc.rows(Some(4));
        assert_eq!(wrapped.iter().map(piece).collect::<Vec<_>>(), ["żółw", "", "abcd", "ef"]);
        assert_eq!(wrapped.iter().map(|r| r.0).collect::<Vec<_>>(), [0, 1, 2, 2]);
    }

    #[test]
    fn heavy_neighbours_are_not_read_ahead() {
        assert!(key("photo.jpg").light());
        assert!(!key("part.STL").light());
        assert!(!key("part.3mf").light());
        assert!(!Key { size: PREFETCH_MAX_BYTES + 1, ..key("film.jpg") }.light());
    }

    #[test]
    fn a_changed_file_is_a_different_preview() {
        let mut changed = key("f");
        changed.size = 1;
        assert_ne!(key("f"), changed);
    }

    #[test]
    fn failures_are_not_remembered_and_closing_ignores_late_results() {
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        let ctx = egui::Context::default();
        app.preview.tx.send((key("broken"), PreviewContent::Error("x".into()))).unwrap();
        app.receive_preview();
        assert!(!app.preview.cache.contains(&key("broken")));
        app.want_preview(None, &ctx);
        app.preview.tx.send((key("old"), PreviewContent::Text(Arc::new(TextDoc::new("old".into(), false, Colours::default()))))).unwrap();
        app.receive_preview();
        assert!(matches!(app.preview.content, PreviewContent::None));
        assert!(app.preview.cache.contains(&key("old")), "a finished read still serves the next visit");
    }

    #[test]
    fn a_film_replaces_the_single_frame_of_its_video() {
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        let ctx = egui::Context::default();
        let info = |more| PreviewContent::Info(Arc::default(), None, more);
        app.preview.cache.insert(key("v"), info(true));
        app.want_preview(Some(key("v")), &ctx);
        let film = Key { page: media::FILM_PAGE, ..key("v") };
        app.preview.tx.send((film, info(false))).unwrap();
        app.receive_preview();
        assert_eq!(app.preview.revision, 1);
        assert!(matches!(app.preview.content, PreviewContent::Info(_, _, false)));
        assert!(matches!(app.preview.cache.get(&key("v")), Some(PreviewContent::Info(_, _, false))), "the next visit starts with the film");
        app.preview.film_asked = Some(key("v"));
        app.want_preview(Some(key("w")), &ctx);
        assert!(app.preview.film_asked.is_none(), "coming back asks again");
    }

    #[test]
    fn a_remembered_file_shows_without_loading() {
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        let ctx = egui::Context::default();
        app.preview.cache.insert(key("seen"), PreviewContent::Hex(Arc::default(), 0));
        app.want_preview(Some(key("seen")), &ctx);
        assert!(matches!(app.preview.content, PreviewContent::Hex(..)));
    }
}
