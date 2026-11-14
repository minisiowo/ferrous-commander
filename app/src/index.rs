//! A compact index of the names below the home folder, so that the quick palette finds an item
//! anywhere in it at once. It is built on other threads and kept on disk between runs; nothing
//! here draws, so all of it can be tested without a window.
//!
//! Every item is a `Node` of 12 bytes. Names live one after another in one string, in lower
//! case; the few that differ in lower case are kept again as written. A node's parent always
//! comes before it, so one pass from the start sees every folder before what is inside it.

use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::background::{Polled, Task, Wake};

const DIR: u8 = 1;
const HIDDEN: u8 = 2;
const ROOT: u8 = 4;
/// The name as written differs from its lower case, and is kept in `original`.
const ORIGINAL: u8 = 8;
const NO_PARENT: u32 = u32::MAX;

/// More items than this is a tree nobody searches by name; the index stops growing there.
pub const NODES_MAX: usize = 5_000_000;
/// Words searched for at once, as bits of a `u32`.
const WORDS_MAX: usize = 32;
/// Below this many matches, a single word is also looked for as letters apart.
const FEW: usize = 20;

const MAGIC: &[u8; 4] = b"FCIX";
const VERSION: u32 = 1;
const PLATFORM: u8 = if cfg!(windows) { 1 } else { 0 };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Node {
    parent: u32,
    /// Where the lower-case name starts in `lower`; for a root, its place in `roots`.
    at: u32,
    len: u16,
    flags: u8,
    /// How far below its root, at most 255.
    depth: u8,
}

/// What is left out of the index, beyond links, hidden folders and caches.
#[derive(Clone, Debug)]
pub struct Rules {
    /// Folder names, in lower case, whose contents are not indexed, besides hidden and dot ones.
    pub exclude: Vec<String>,
    /// Stay on the disk of each root: no other disks, network mounts or `/proc`.
    pub same_device: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules { exclude: vec!["node_modules".into(), "__pycache__".into()], same_device: true }
    }
}

impl Rules {
    fn excluded(&self, name: &str) -> bool {
        self.exclude.iter().any(|ex| ex.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub node: u32,
    pub score: i32,
}

pub struct Index {
    roots: Vec<PathBuf>,
    nodes: Vec<Node>,
    lower: String,
    /// The names whose lower case differs: node, place in `original`, length. In node order.
    originals: Vec<(u32, u32, u16)>,
    original: String,
    built: SystemTime,
    /// Folders that could not be read.
    pub skipped: usize,
    /// It stopped at `NODES_MAX`.
    pub limited: bool,
}

impl Index {
    fn empty() -> Index {
        Index {
            roots: Vec::new(),
            nodes: Vec::new(),
            lower: String::new(),
            originals: Vec::new(),
            original: String::new(),
            built: UNIX_EPOCH,
            skipped: 0,
            limited: false,
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[cfg(test)]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    fn push_root(&mut self, root: &Path) -> u32 {
        let node = self.nodes.len() as u32;
        self.nodes.push(Node { parent: NO_PARENT, at: self.roots.len() as u32, len: 0, flags: ROOT | DIR, depth: 0 });
        self.roots.push(root.to_path_buf());
        node
    }

    /// Adds an item, or says no when a limit is reached.
    fn push(&mut self, parent: u32, depth: u8, name: &str, flags: u8) -> Option<u32> {
        let lower = name.to_lowercase();
        let room = |arena: &String, len: usize| arena.len() + len <= u32::MAX as usize;
        if self.nodes.len() >= NODES_MAX || name.len() > u16::MAX as usize || lower.len() > u16::MAX as usize
            || !room(&self.lower, lower.len()) || !room(&self.original, name.len()) {
            self.limited = true;
            return None;
        }
        let node = self.nodes.len() as u32;
        let mut flags = flags & (DIR | HIDDEN);
        if lower != name {
            flags |= ORIGINAL;
            self.originals.push((node, self.original.len() as u32, name.len() as u16));
            self.original.push_str(name);
        }
        self.nodes.push(Node { parent, at: self.lower.len() as u32, len: lower.len() as u16, flags, depth });
        self.lower.push_str(&lower);
        Some(node)
    }

    fn lower_name(&self, node: &Node) -> &str {
        &self.lower[node.at as usize..node.at as usize + node.len as usize]
    }

    /// The name as written; for a root, its whole path.
    pub fn name(&self, node: u32) -> std::borrow::Cow<'_, str> {
        let n = &self.nodes[node as usize];
        if n.flags & ROOT != 0 {
            return self.roots[n.at as usize].to_string_lossy();
        }
        if n.flags & ORIGINAL != 0
            && let Ok(k) = self.originals.binary_search_by_key(&node, |o| o.0) {
            let (_, at, len) = self.originals[k];
            return self.original[at as usize..at as usize + len as usize].into();
        }
        self.lower_name(n).into()
    }

    pub fn is_dir(&self, node: u32) -> bool {
        self.nodes[node as usize].flags & DIR != 0
    }

    pub fn depth(&self, node: u32) -> u8 {
        self.nodes[node as usize].depth
    }

    /// The root `node` lies below, and the folders between them.
    fn chain(&self, node: u32) -> (u32, Vec<u32>) {
        let mut names = Vec::new();
        let mut at = node;
        while self.nodes[at as usize].flags & ROOT == 0 {
            names.push(at);
            at = self.nodes[at as usize].parent;
        }
        names.reverse();
        (at, names)
    }

    pub fn path(&self, node: u32) -> PathBuf {
        let (root, names) = self.chain(node);
        let mut path = self.roots[self.nodes[root as usize].at as usize].clone();
        for n in names { path.push(&*self.name(n)); }
        path
    }

    /// The best `limit` items whose name or folders hold every word, with one word at least in
    /// the name, best first. Hidden items only when `hidden`. A single word with few matches is
    /// also looked for as letters apart, ranked below every whole match.
    pub fn search(&self, text: &str, hidden: bool, limit: usize, cancel: &AtomicBool) -> Vec<Found> {
        let words: Vec<String> = text.to_lowercase().split_whitespace().take(WORDS_MAX).map(String::from).collect();
        let mut found = self.query(&words, hidden, limit, cancel);
        if let [word] = words.as_slice() && found.len() < FEW && word.chars().count() >= 3 {
            let known: HashSet<u32> = found.iter().map(|f| f.node).collect();
            let apart = self.scattered(word, hidden, limit.saturating_sub(found.len()), cancel);
            found.extend(apart.into_iter().filter(|f| !known.contains(&f.node)).map(|f| Found { score: f.score - 1000, ..f }));
        }
        found
    }

    fn query(&self, words: &[String], hidden: bool, limit: usize, cancel: &AtomicBool) -> Vec<Found> {
        if words.is_empty() || limit == 0 { return Vec::new(); }
        let all = if words.len() == 32 { u32::MAX } else { (1u32 << words.len()) - 1 };
        // What each folder's name and those above it hold. Files are never looked up.
        let mut masks = vec![0u32; self.nodes.len()];
        let mut best = Best::new(limit);
        for (i, node) in self.nodes.iter().enumerate() {
            if i % 65_536 == 0 && cancel.load(Ordering::Relaxed) { return Vec::new(); }
            if node.flags & ROOT != 0 { continue; }
            let name = self.lower_name(node);
            let mut own = 0u32;
            for (k, word) in words.iter().enumerate() {
                if name.contains(word.as_str()) { own |= 1 << k; }
            }
            let mask = masks[node.parent as usize] | own;
            if node.flags & DIR != 0 { masks[i] = mask; }
            if own == 0 || mask != all || (!hidden && node.flags & HIDDEN != 0) { continue; }
            best.offer(i as u32, score(name, words, own, node.depth));
        }
        best.into_sorted()
    }

    /// Items whose name holds the letters of `word` in order, but apart.
    fn scattered(&self, word: &str, hidden: bool, limit: usize, cancel: &AtomicBool) -> Vec<Found> {
        if limit == 0 { return Vec::new(); }
        let letters: Vec<char> = word.chars().collect();
        let mut best = Best::new(limit);
        for (i, node) in self.nodes.iter().enumerate() {
            if i % 65_536 == 0 && cancel.load(Ordering::Relaxed) { return Vec::new(); }
            if node.flags & ROOT != 0 || (!hidden && node.flags & HIDDEN != 0) { continue; }
            let name = self.lower_name(node);
            if let Some(span) = letters_apart(&letters, name) {
                let spread = (span - letters.len()).min(40) as i32;
                best.offer(i as u32, 100 - 2 * spread - 8 * node.depth as i32 - name.len().min(120) as i32 / 4);
            }
        }
        best.into_sorted()
    }
}

/// How many characters of `name` the letters span, from the first to the last, in order.
fn letters_apart(letters: &[char], name: &str) -> Option<usize> {
    let mut next = 0;
    let mut span = 0;
    for c in name.chars() {
        if next > 0 { span += 1; }
        if c == letters[next] {
            if next == 0 { span = 1; }
            next += 1;
            if next == letters.len() { return Some(span); }
        }
    }
    None
}

fn starts_word(name: &str, at: usize) -> bool {
    at == 0 || !name[..at].chars().next_back().is_some_and(char::is_alphanumeric)
}

/// A first guess at how well an item matches, so that the palette ranks only the likely ones.
fn score(name: &str, words: &[String], own: u32, depth: u8) -> i32 {
    let mut score = -8 * depth as i32 - name.len().min(120) as i32 / 4;
    for (k, word) in words.iter().enumerate() {
        if own & (1 << k) == 0 { continue; }
        let place = if name.starts_with(word.as_str()) {
            60
        } else if name.match_indices(word.as_str()).any(|(at, _)| starts_word(name, at)) {
            40
        } else {
            0
        };
        score += 100 + place + if name.len() == word.len() { 50 } else { 0 };
    }
    score
}

/// Keeps the best `limit` offers: the highest score, then the earliest node.
struct Best {
    limit: usize,
    heap: BinaryHeap<std::cmp::Reverse<(i32, std::cmp::Reverse<u32>)>>,
}

impl Best {
    fn new(limit: usize) -> Best {
        Best { limit, heap: BinaryHeap::new() }
    }

    fn offer(&mut self, node: u32, score: i32) {
        let entry = std::cmp::Reverse((score, std::cmp::Reverse(node)));
        if self.heap.len() < self.limit {
            self.heap.push(entry);
        } else if self.heap.peek().is_some_and(|worst| entry < *worst) {
            self.heap.pop();
            self.heap.push(entry);
        }
    }

    fn into_sorted(self) -> Vec<Found> {
        // Ascending `Reverse` is the best first.
        self.heap.into_sorted_vec().into_iter().map(|std::cmp::Reverse((score, std::cmp::Reverse(node)))| Found { node, score }).collect()
    }
}

// ---- Building -------------------------------------------------------------------------------

struct Child {
    name: String,
    flags: u8,
    /// The folder to list next, when its contents belong in the index.
    descend: Option<PathBuf>,
}

struct Listing {
    parent: u32,
    depth: u8,
    device: Option<u64>,
    children: Vec<Child>,
    failed: bool,
}

struct Job {
    node: u32,
    depth: u8,
    path: PathBuf,
    device: Option<u64>,
}

/// Folders waiting to be listed, shared by the workers.
#[derive(Default)]
struct Queue {
    work: Mutex<(VecDeque<Job>, bool)>,
    ready: Condvar,
}

impl Queue {
    fn take(&self) -> Option<Job> {
        let mut work = self.work.lock().unwrap();
        loop {
            if let Some(job) = work.0.pop_front() { return Some(job); }
            if work.1 { return None; }
            work = self.ready.wait(work).unwrap();
        }
    }

    fn add(&self, jobs: Vec<Job>) {
        if jobs.is_empty() { return; }
        self.work.lock().unwrap().0.extend(jobs);
        self.ready.notify_all();
    }

    fn close(&self) {
        self.work.lock().unwrap().1 = true;
        self.ready.notify_all();
    }
}

#[cfg(unix)]
fn device(md: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(md.dev())
}

/// Windows reaches other volumes only through mount points, which are links and never walked.
#[cfg(not(unix))]
fn device(_md: &std::fs::Metadata) -> Option<u64> {
    None
}

/// Indexing must not slow down what the user is doing.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain system calls on the calling thread's own id.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, 19);
    }
    #[cfg(windows)]
    crate::win::background_thread();
}

fn list(job: &Job, rules: &Rules) -> Listing {
    let mut listing = Listing { parent: job.node, depth: job.depth, device: job.device, children: Vec::new(), failed: false };
    let Ok(entries) = std::fs::read_dir(&job.path) else {
        listing.failed = true;
        return listing;
    };
    // Stop at the first error: on a drive that was pulled out it can repeat forever.
    for entry in entries.map_while(Result::ok) {
        let file_name = entry.file_name();
        // A name that is not Unicode cannot be typed in the palette, nor stored as text.
        let Some(name) = file_name.to_str() else { continue };
        if name == "CACHEDIR.TAG" {
            // A cache says so with this file; nobody looks for what is in it by name.
            listing.children.clear();
            return listing;
        }
        let Ok(kind) = entry.file_type() else { continue };
        let link = kind.is_symlink();
        let is_dir = if link { std::fs::metadata(entry.path()).is_ok_and(|md| md.is_dir()) } else { kind.is_dir() };
        let hidden = crate::fsx::hidden_by_name(name, is_dir);
        #[cfg(windows)]
        let hidden = hidden || entry.metadata().is_ok_and(|md| crate::fsx::hidden_by_attribute(&md));
        // Dot folders hold tools' own files (.git, .cargo, .rustup) even where Windows shows them.
        let mut descend = is_dir && !link && !hidden && !name.starts_with('.') && !rules.excluded(name);
        if descend && rules.same_device && job.device.is_some() {
            descend = entry.metadata().ok().as_ref().and_then(device) == job.device;
        }
        let flags = if is_dir { DIR } else { 0 } | if hidden { HIDDEN } else { 0 };
        listing.children.push(Child { name: name.to_string(), flags, descend: descend.then(|| entry.path()) });
    }
    listing
}

/// The roots an index is built for: absolute, stored as text, each once.
fn usable_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut usable: Vec<PathBuf> = Vec::new();
    for root in roots {
        if root.is_absolute() && root.to_str().is_some() && !usable.contains(root) { usable.push(root.clone()); }
    }
    usable
}

/// Indexes everything below `roots` by the rules, on several threads at low priority. `count`
/// follows the items indexed so far. `None` when cancelled.
pub fn build(roots: &[PathBuf], rules: &Rules, cancel: &AtomicBool, count: &AtomicUsize) -> Option<Index> {
    let mut index = Index::empty();
    let queue = Arc::new(Queue::default());
    let mut outstanding = 0usize;
    for root in usable_roots(roots) {
        let root = &root;
        let device = std::fs::metadata(root).ok().as_ref().and_then(device);
        let node = index.push_root(root);
        queue.add(vec![Job { node, depth: 0, path: root.clone(), device }]);
        outstanding += 1;
    }
    let (tx, rx) = mpsc::channel::<Listing>();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 6);
    let workers: Vec<_> = (0..threads).map(|_| {
        let (queue, tx, rules) = (queue.clone(), tx.clone(), rules.clone());
        std::thread::spawn(move || {
            lower_priority();
            while let Some(job) = queue.take() {
                if tx.send(list(&job, &rules)).is_err() { return; }
            }
        })
    }).collect();
    drop(tx);
    let mut cancelled = false;
    while outstanding > 0 {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let listing = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(listing) => listing,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        outstanding -= 1;
        if listing.failed { index.skipped += 1; }
        let depth = listing.depth.saturating_add(1);
        let mut jobs = Vec::new();
        for child in listing.children {
            let Some(node) = index.push(listing.parent, depth, &child.name, child.flags) else { break };
            if let Some(path) = child.descend {
                jobs.push(Job { node, depth, path, device: listing.device });
            }
        }
        outstanding += jobs.len();
        queue.add(jobs);
        count.store(index.nodes.len(), Ordering::Relaxed);
    }
    queue.close();
    drop(rx);
    for worker in workers { let _ = worker.join(); }
    if cancelled { return None; }
    index.built = SystemTime::now();
    Some(index)
}

// ---- On disk --------------------------------------------------------------------------------

fn put_u32(out: &mut Vec<u8>, n: u32) { out.extend_from_slice(&n.to_le_bytes()); }
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        if self.0.len() < n { return Err(io::Error::new(io::ErrorKind::InvalidData, "index file is cut short")); }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> io::Result<u8> { Ok(self.take(1)?[0]) }
    fn u16(&mut self) -> io::Result<u16> { Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap())) }
    fn u32(&mut self) -> io::Result<u32> { Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap())) }
    fn u64(&mut self) -> io::Result<u64> { Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap())) }
    fn text(&mut self) -> io::Result<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| bad("index file holds broken text"))
    }
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

impl Index {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.nodes.len() * 12 + self.lower.len() + self.original.len() + self.originals.len() * 10);
        out.extend_from_slice(MAGIC);
        put_u32(&mut out, VERSION);
        out.push(PLATFORM);
        out.extend_from_slice(&self.built.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_le_bytes());
        out.extend_from_slice(&(self.skipped as u64).to_le_bytes());
        out.push(self.limited as u8);
        put_u32(&mut out, self.roots.len() as u32);
        for root in &self.roots { put_bytes(&mut out, root.to_string_lossy().as_bytes()); }
        put_u32(&mut out, self.nodes.len() as u32);
        for n in &self.nodes {
            put_u32(&mut out, n.parent);
            put_u32(&mut out, n.at);
            out.extend_from_slice(&n.len.to_le_bytes());
            out.push(n.flags);
            out.push(n.depth);
        }
        put_bytes(&mut out, self.lower.as_bytes());
        put_bytes(&mut out, self.original.as_bytes());
        put_u32(&mut out, self.originals.len() as u32);
        for &(node, at, len) in &self.originals {
            put_u32(&mut out, node);
            put_u32(&mut out, at);
            out.extend_from_slice(&len.to_le_bytes());
        }
        out
    }

    /// Reads what `encode` wrote, and checks it all: a broken file must not break a search.
    fn decode(bytes: &[u8]) -> io::Result<Index> {
        let mut r = Reader(bytes);
        if r.take(4)? != MAGIC { return Err(bad("not an index file")); }
        if r.u32()? != VERSION || r.u8()? != PLATFORM { return Err(bad("index file of another version")); }
        let built = UNIX_EPOCH + Duration::from_secs(r.u64()?);
        let skipped = r.u64()? as usize;
        let limited = r.u8()? != 0;
        let roots = (0..r.u32()?).map(|_| r.text().map(PathBuf::from)).collect::<io::Result<Vec<_>>>()?;
        let count = r.u32()? as usize;
        if count > NODES_MAX || r.0.len() < count * 12 { return Err(bad("index file is cut short")); }
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            nodes.push(Node { parent: r.u32()?, at: r.u32()?, len: r.u16()?, flags: r.u8()?, depth: r.u8()? });
        }
        let lower = r.text()?;
        let original = r.text()?;
        let originals = (0..r.u32()?).map(|_| Ok((r.u32()?, r.u32()?, r.u16()?))).collect::<io::Result<Vec<_>>>()?;
        if !r.0.is_empty() { return Err(bad("index file has more than it should")); }
        for (i, n) in nodes.iter().enumerate() {
            let fine = if n.flags & ROOT != 0 {
                n.parent == NO_PARENT && (n.at as usize) < roots.len()
            } else {
                (n.parent as usize) < i && nodes[n.parent as usize].flags & DIR != 0
                    && lower.get(n.at as usize..n.at as usize + n.len as usize).is_some()
            };
            if !fine { return Err(bad("index file is broken")); }
        }
        let flagged = nodes.iter().filter(|n| n.flags & ORIGINAL != 0).count();
        let mut last = None;
        for &(node, at, len) in &originals {
            let fine = last.is_none_or(|last| node > last)
                && nodes.get(node as usize).is_some_and(|n| n.flags & ORIGINAL != 0)
                && original.get(at as usize..at as usize + len as usize).is_some();
            if !fine { return Err(bad("index file is broken")); }
            last = Some(node);
        }
        if flagged != originals.len() { return Err(bad("index file is broken")); }
        Ok(Index { roots, nodes, lower, originals, original, built, skipped, limited })
    }

    /// Writes the index whole or not at all.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
        let part = path.with_extension("part");
        let mut file = std::fs::File::create(&part)?;
        file.write_all(&self.encode())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, path)
    }

    pub fn load(path: &Path) -> io::Result<Index> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.read_to_end(&mut bytes)?;
        Index::decode(&bytes)
    }
}

// ---- Kept up to date ----------------------------------------------------------------------

/// A pause after the app starts before the first build, so that it does not slow the start.
const START_DELAY: Duration = Duration::from_secs(5);
/// An index older than this is built again when the palette opens.
const STALE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Off,
    /// Reading the index kept from the last run.
    Loading,
    /// Building, with the items found so far.
    Building(usize),
    Ready,
}

enum Work {
    Load(Task<Option<Index>>),
    Build { task: Task<Option<Index>>, cancel: Arc<AtomicBool>, count: Arc<AtomicUsize> },
}

/// Keeps an index of the home folder and the folders added in the settings: read from disk at
/// the start, built again soon after and whenever the palette finds it old, saved after each build.
pub struct Service {
    home: Option<PathBuf>,
    file: Option<PathBuf>,
    extra_roots: Vec<PathBuf>,
    extra_exclude: Vec<String>,
    enabled: bool,
    index: Option<Arc<Index>>,
    /// Changes with every new index, so that a search made on an older one is made again.
    pub generation: u64,
    work: Option<Work>,
    next_build: Option<std::time::Instant>,
}

impl Service {
    /// `home` is the folder always indexed and `file` where the index is kept; tests give neither.
    pub fn new(home: Option<PathBuf>, file: Option<PathBuf>) -> Service {
        Service { home, file, extra_roots: Vec::new(), extra_exclude: Vec::new(), enabled: false, index: None, generation: 0, work: None, next_build: None }
    }

    /// Where the index of this user's home is kept, and that home.
    pub fn for_home() -> Service {
        Service::new(dirs::home_dir(), dirs::cache_dir().map(|dir| dir.join("ferrous").join("index.bin")))
    }

    fn roots(&self) -> Vec<PathBuf> {
        usable_roots(&self.home.iter().chain(&self.extra_roots).cloned().collect::<Vec<_>>())
    }

    fn rules(&self) -> Rules {
        let mut rules = Rules::default();
        rules.exclude.extend(self.extra_exclude.iter().map(|name| name.to_lowercase()));
        rules
    }

    pub fn index(&self) -> Option<&Arc<Index>> {
        self.index.as_ref()
    }

    pub fn state(&self) -> State {
        match &self.work {
            _ if !self.enabled => State::Off,
            Some(Work::Load(_)) => State::Loading,
            Some(Work::Build { count, .. }) => State::Building(count.load(Ordering::Relaxed)),
            None => State::Ready,
        }
    }

    /// Follows the settings: on or off, and which folders beyond home, leaving out which names.
    pub fn sync(&mut self, on: bool, roots: &[PathBuf], exclude: &[String], wake: &Wake) {
        if on && (self.extra_roots != roots || self.extra_exclude != exclude) {
            self.extra_roots = roots.to_vec();
            self.extra_exclude = exclude.to_vec();
            if self.enabled {
                self.cancel();
                self.next_build = Some(std::time::Instant::now());
                wake.repaint();
            }
        }
        if on == self.enabled { return; }
        self.enabled = on;
        if on {
            if let Some(file) = self.file.clone().filter(|file| file.exists()) {
                self.work = Some(Work::Load(Task::spawn(wake, move || Index::load(&file).ok())));
            }
            let first = if self.roots().is_empty() { None } else if self.file.is_some() { Some(START_DELAY) } else { Some(Duration::ZERO) };
            self.next_build = first.map(|wait| std::time::Instant::now() + wait);
            if let Some(wait) = first { wake.repaint_after(wait); }
        } else {
            self.cancel();
            self.work = None;
            self.next_build = None;
            self.index = None;
            self.generation += 1;
            if let Some(file) = &self.file { let _ = std::fs::remove_file(file); }
        }
    }

    fn cancel(&mut self) {
        if let Some(Work::Build { cancel, .. }) = &self.work {
            cancel.store(true, Ordering::Relaxed);
            self.work = None;
        }
    }

    /// The palette opened: an old index, or none, is built again now.
    pub fn freshen(&mut self, wake: &Wake) {
        if !self.enabled || self.work.is_some() || self.roots().is_empty() { return; }
        let old = self.index.as_ref().is_none_or(|index| index.built.elapsed().is_ok_and(|age| age > STALE) || index.built > SystemTime::now());
        if old {
            self.next_build = Some(std::time::Instant::now());
            wake.repaint();
        }
    }

    /// What the index offered is gone: build it again now, whatever its age.
    pub fn invalidate(&mut self, wake: &Wake) {
        if self.index.is_some() && self.work.is_none() && self.enabled && !self.roots().is_empty() {
            self.next_build = Some(std::time::Instant::now());
            wake.repaint();
        }
    }

    /// Takes what the threads have done and starts a build when one is due. True when there is
    /// a new index.
    pub fn poll(&mut self, wake: &Wake) -> bool {
        let mut changed = false;
        let finished = match &self.work {
            Some(Work::Load(task) | Work::Build { task, .. }) => match task.poll() {
                Polled::Running => None,
                Polled::Done(index) => Some(index),
                Polled::Lost => Some(None),
            },
            None => None,
        };
        if let Some(index) = finished {
            let built = matches!(self.work, Some(Work::Build { .. }));
            self.work = None;
            // A kept index of other folders is of no use; one built meanwhile is newer.
            let roots = self.roots();
            if let Some(index) = index.filter(|index| index.roots == roots && (built || self.index.is_none())) {
                self.index = Some(Arc::new(index));
                self.generation += 1;
                changed = true;
            }
        }
        if self.enabled && self.work.is_none() && let Some(due) = self.next_build {
            let now = std::time::Instant::now();
            if due <= now {
                self.next_build = None;
                self.start_build(wake);
            } else {
                wake.repaint_after(due - now);
            }
        }
        changed
    }

    fn start_build(&mut self, wake: &Wake) {
        let (roots, rules, file) = (self.roots(), self.rules(), self.file.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let (stop, counted) = (cancel.clone(), count.clone());
        let task = Task::spawn(wake, move || {
            let index = build(&roots, &rules, &stop, &counted)?;
            // Saving can fail on a full or read-only disk; the index in memory still serves.
            // Turned off meanwhile, it must not leave the file behind.
            if let Some(file) = file && !stop.load(Ordering::Relaxed) { let _ = index.save(&file); }
            Some(index)
        });
        self.work = Some(Work::Build { task, cancel, count });
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree(PathBuf);
    impl Tree {
        fn new(tag: &str) -> Tree {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let root = std::env::temp_dir().join(format!("fc_index_{tag}_{}_{stamp}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            Tree(root)
        }
        fn file(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
            path
        }
        fn index(&self) -> Index {
            build(std::slice::from_ref(&self.0), &Rules::default(), &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap()
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn paths(index: &Index, text: &str, hidden: bool) -> Vec<PathBuf> {
        index.search(text, hidden, 100, &AtomicBool::new(false)).into_iter().map(|f| index.path(f.node)).collect()
    }

    fn all(index: &Index) -> HashSet<PathBuf> {
        (0..index.len() as u32).map(|n| index.path(n)).collect()
    }

    #[test]
    fn every_item_is_indexed_below_its_folder() {
        let tree = Tree::new("all");
        let files = [tree.file("a/b/c/Deep File.TXT"), tree.file("a/top.md"), tree.file("Zażółć/Gęślą.txt")];
        let index = tree.index();
        for (i, n) in index.nodes.iter().enumerate() {
            assert!(n.flags & ROOT != 0 || (n.parent as usize) < i, "a folder comes before its contents");
        }
        let all = all(&index);
        for file in &files { assert!(all.contains(file), "{file:?} is missing"); }
        assert!(all.contains(&tree.0.join("a/b")));
        assert_eq!(index.len(), 1 + 7, "the root and seven items");
        let deep = paths(&index, "deep", false);
        assert_eq!(deep, [files[0].clone()]);
        assert_eq!(index.name(index.search("deep", false, 1, &AtomicBool::new(false))[0].node), "Deep File.TXT", "the name as written");
        assert_eq!(paths(&index, "GĘŚL", false), [files[2].clone()], "case and accents");
    }

    #[test]
    fn words_may_name_the_folders_above_with_one_in_the_name() {
        let tree = Tree::new("words");
        let wanted = tree.file("Projekt/src/main.rs");
        tree.file("other/src/main.rs");
        tree.file("Projekt/notes.txt");
        let index = tree.index();
        assert_eq!(paths(&index, "projekt main", false), [wanted]);
        assert_eq!(paths(&index, "projekt", false), [tree.0.join("Projekt")], "a folder's name alone lists only it");
        let root_name = tree.0.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(paths(&index, &format!("{root_name} main"), false).is_empty(), "the root's own path does not count");
    }

    #[test]
    fn hidden_folders_caches_and_excluded_names_are_listed_but_not_entered() {
        let tree = Tree::new("skip");
        tree.file(".config/secret-setting.toml");
        tree.file("node_modules/pkg/skipped-module.js");
        tree.file("target/CACHEDIR.TAG");
        tree.file("target/debug/skipped-build.o");
        let hidden_file = tree.file(".hidden-file");
        // Windows hides by attribute, not by a leading dot.
        #[cfg(windows)]
        assert!(std::process::Command::new("attrib").arg("+h").arg(&hidden_file).status().unwrap().success());
        let kept = tree.file("src/kept.rs");
        let index = tree.index();
        let all = all(&index);
        for gone in ["secret", "skipped"] {
            assert!(paths(&index, gone, true).is_empty(), "{gone} is not indexed");
        }
        for listed in [".config", "node_modules", "target", ".hidden-file"] {
            assert!(all.contains(&tree.0.join(listed)), "{listed} itself is listed");
        }
        assert!(all.contains(&kept));
        assert!(paths(&index, "hidden", false).is_empty(), "hidden items need the setting");
        assert_eq!(paths(&index, "hidden", true), [hidden_file]);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_listed_but_never_walked() {
        let tree = Tree::new("links");
        let file = tree.file("sub/report.txt");
        std::os::unix::fs::symlink(&tree.0, tree.0.join("sub/loop")).unwrap();
        let index = tree.index();
        assert_eq!(paths(&index, "report", false), [file]);
        let link = index.search("loop", false, 5, &AtomicBool::new(false));
        assert!(index.is_dir(link[0].node), "a link to a folder goes to a folder");
    }

    #[test]
    fn a_name_at_the_start_and_near_the_root_ranks_first() {
        let tree = Tree::new("rank");
        let expected = [tree.file("report.txt"), tree.file("x/report-old.txt"), tree.file("x/y/z/old-report.txt"), tree.file("x/annual_report.txt")];
        let index = tree.index();
        let got = paths(&index, "report", false);
        assert_eq!(got[0], expected[0]);
        assert_eq!(got[1], expected[1]);
        assert_eq!(got.len(), 4);
        assert_eq!(index.search("report", false, 2, &AtomicBool::new(false)).len(), 2, "the limit holds");
    }

    #[test]
    fn letters_apart_are_looked_for_only_when_whole_words_find_little() {
        let tree = Tree::new("apart");
        let wanted = tree.file("docs/ferrous-commander.md");
        let index = tree.index();
        assert_eq!(paths(&index, "fcom", false), [wanted]);
        assert!(paths(&index, "fco x", false).is_empty(), "only a single word");
        assert!(paths(&index, "mocf", false).is_empty(), "the order counts");
        assert_eq!(letters_apart(&['a', 'b'], "xaxxb"), Some(4));
        assert_eq!(letters_apart(&['a', 'b'], "ba"), None);
    }

    #[test]
    fn a_saved_index_comes_back_the_same_and_a_broken_one_is_refused() {
        let tree = Tree::new("disk");
        tree.file("Mixed Case/Ünïcode.TXT");
        tree.file("plain/file.rs");
        let index = tree.index();
        let file = tree.0.join("cache").join("index.bin");
        index.save(&file).unwrap();
        let back = Index::load(&file).unwrap();
        assert_eq!(back.nodes, index.nodes);
        assert_eq!(all(&back), all(&index));
        assert_eq!(back.built.duration_since(UNIX_EPOCH).unwrap().as_secs(), index.built.duration_since(UNIX_EPOCH).unwrap().as_secs());
        let bytes = index.encode();
        assert!(Index::decode(&bytes[..bytes.len() - 1]).is_err(), "cut short");
        let mut other = bytes.clone();
        other[4] = 99;
        assert!(Index::decode(&other).is_err(), "another version");
        // A parent that comes after its child would loop when the path is made.
        let mut broken = index.encode();
        let first_node = 4 + 4 + 1 + 8 + 8 + 1 + 4 + 4 + tree.0.to_string_lossy().len() + 4 + 12;
        broken[first_node..first_node + 4].copy_from_slice(&5u32.to_le_bytes());
        assert!(Index::decode(&broken).is_err(), "a parent after its child");
        assert!(Index::decode(b"junk").is_err());
    }

    #[test]
    fn cancelling_stops_the_build_and_the_search() {
        let tree = Tree::new("cancel");
        tree.file("a/b.txt");
        let stop = AtomicBool::new(true);
        assert!(build(std::slice::from_ref(&tree.0), &Rules::default(), &stop, &AtomicUsize::new(0)).is_none());
        let index = tree.index();
        assert!(index.search("b", false, 10, &stop).is_empty());
        assert!(!index.search("b", false, 10, &AtomicBool::new(false)).is_empty());
    }

    /// `cargo test --release index::tests::measure_home -- --ignored --nocapture`
    #[test]
    #[ignore = "walks the real home folder, to measure"]
    fn measure_home() {
        let home = dirs::home_dir().unwrap();
        let started = std::time::Instant::now();
        let index = build(&[home], &Rules::default(), &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap();
        let took = started.elapsed();
        let bytes = index.lower.len() + index.original.len() + index.nodes.len() * 12 + index.originals.len() * 12;
        println!("items {}, skipped {}, built in {took:?}, memory {} KB, file {} KB",
            index.len(), index.skipped, bytes / 1024, index.encode().len() / 1024);
        for text in ["re", "main", "src main", "ferrous commander rs", "zzqx"] {
            let started = std::time::Instant::now();
            let found = index.search(text, false, 2000, &AtomicBool::new(false));
            println!("{text:?}: {} in {:?}", found.len(), started.elapsed());
        }
        let started = std::time::Instant::now();
        let file = std::env::temp_dir().join("fc_index_measure.bin");
        index.save(&file).unwrap();
        let saved = started.elapsed();
        let started = std::time::Instant::now();
        Index::load(&file).unwrap();
        println!("saved in {saved:?}, loaded in {:?}", started.elapsed());
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn the_service_takes_an_index_of_roots_named_twice_and_lets_it_go_when_off() {
        let tree = Tree::new("service");
        tree.file("kept.txt");
        let wake = Wake::default();
        // Home also among the added folders, as a user might write it, and a relative one.
        let mut service = Service::new(Some(tree.0.clone()), None);
        service.sync(true, &[tree.0.clone(), PathBuf::from("relative")], &[], &wake);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !service.poll(&wake) {
            assert!(std::time::Instant::now() < deadline, "the index never came");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(service.state(), State::Ready);
        let index = service.index().unwrap();
        assert_eq!(index.roots(), std::slice::from_ref(&tree.0));
        assert_eq!(paths(index, "kept", false), [tree.0.join("kept.txt")]);
        service.sync(false, &[], &[], &wake);
        assert!(service.index().is_none());
        assert_eq!(service.state(), State::Off);
    }

    #[test]
    fn missing_roots_are_noted_and_duplicates_dropped() {
        let tree = Tree::new("roots");
        tree.file("x.txt");
        let roots = [tree.0.clone(), tree.0.clone(), tree.0.join("missing")];
        let index = build(&roots, &Rules::default(), &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap();
        assert_eq!(index.roots().len(), 2);
        assert_eq!(index.skipped, 1);
        assert_eq!(paths(&index, "x.txt", false), [tree.0.join("x.txt")]);
    }
}
