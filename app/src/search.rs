//! Shared, cancellable name search for the quick palette and the search window.
//! A directory is streamed instead of collected; results, pending folders and error
//! details have limits, so a broad search cannot grow without bound.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::fsx::{self, Entry, Kind};
use regex_automata::{meta::Regex, util::syntax};

pub const RESULTS_MAX: usize = 50_000;
const FOLDERS_MAX: usize = 100_000;
const PROBLEMS_MAX: usize = 100;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum NameMode {
    #[default]
    Words,
    Wildcard,
    Regex,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum FileType {
    #[default]
    All,
    Files,
    Folders,
    Audio,
    Video,
    Images,
    Documents,
    Text,
    Archives,
}

#[derive(Clone, Debug)]
pub struct Criteria {
    pub name: String,
    pub mode: NameMode,
    pub case_sensitive: bool,
    pub recursive: bool,
    pub hidden: bool,
    pub file_type: FileType,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_from: Option<SystemTime>,
    /// Exclusive upper bound, allowing the UI to include an entire calendar day.
    pub modified_before: Option<SystemTime>,
    pub excluded_dirs: Vec<String>,
    pub limit: usize,
    /// Stay on the file system of the root: no other disks, network mounts or `/proc`.
    pub same_device: bool,
}

impl Default for Criteria {
    fn default() -> Self {
        Self {
            name: String::new(),
            mode: NameMode::Words,
            case_sensitive: false,
            recursive: true,
            hidden: false,
            file_type: FileType::All,
            min_size: None,
            max_size: None,
            modified_from: None,
            modified_before: None,
            excluded_dirs: Vec::new(),
            limit: RESULTS_MAX,
            same_device: false,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum Outcome {
    #[default]
    Running,
    Complete,
    Cancelled,
    Limited,
}

#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub current: PathBuf,
    pub skipped: usize,
    pub problems: Vec<(PathBuf, String)>,
    pub outcome: Outcome,
    pub limit_reason: Option<&'static str>,
}

pub struct Search {
    pub results: Arc<Mutex<Vec<Entry>>>,
    pub done: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub scanned: Arc<AtomicUsize>,
    pub progress: Arc<Mutex<Progress>>,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Filter {
    criteria: Criteria,
    words: Vec<String>,
    pattern: Option<Regex>,
    excluded: Option<Regex>,
}

fn regex(pattern: &str, case_sensitive: bool) -> Result<Regex, String> {
    Regex::builder()
        .configure(
            Regex::config()
                .nfa_size_limit(Some(2 << 20))
                .dfa_size_limit(Some(2 << 20)),
        )
        .syntax(syntax::Config::new().case_insensitive(!case_sensitive))
        .build(pattern)
        .map_err(|e| {
            format!(
                "Invalid pattern: {}",
                e.syntax_error()
                    .map_or_else(|| e.to_string(), ToString::to_string)
            )
        })
}

fn wildcard(pattern: &str) -> String {
    let mut out = String::from("\\A(?s:");
    for (n, part) in pattern
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        if n > 0 {
            out.push('|');
        }
        for c in part.chars() {
            match c {
                '*' => out.push_str(".*"),
                '?' => out.push('.'),
                '.' | '+' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
    }
    out.push_str(")\\z");
    out
}

impl Filter {
    fn new(criteria: Criteria) -> Result<Self, String> {
        if (criteria.mode != NameMode::Words && criteria.name.len() > 2048)
            || criteria
                .excluded_dirs
                .iter()
                .map(String::len)
                .sum::<usize>()
                > 2048
        {
            return Err("Search patterns are too long (maximum 2048 bytes)".into());
        }
        if criteria
            .min_size
            .zip(criteria.max_size)
            .is_some_and(|(a, b)| a > b)
        {
            return Err("Minimum size is greater than maximum size".into());
        }
        if criteria
            .modified_from
            .zip(criteria.modified_before)
            .is_some_and(|(a, b)| a >= b)
        {
            return Err("The modification date range is reversed".into());
        }
        if criteria.limit == 0 || criteria.limit > RESULTS_MAX {
            return Err("Invalid result limit".into());
        }
        let name = criteria.name.trim();
        let pattern = match criteria.mode {
            NameMode::Words => None,
            NameMode::Wildcard if !name.is_empty() => {
                Some(regex(&wildcard(name), criteria.case_sensitive)?)
            }
            NameMode::Regex if !name.is_empty() => Some(regex(name, criteria.case_sensitive)?),
            _ => None,
        };
        let words = if criteria.case_sensitive {
            name.to_string()
        } else {
            name.to_lowercase()
        }
        .split_whitespace()
        .map(String::from)
        .collect();
        let excluded = if criteria.excluded_dirs.is_empty() {
            None
        } else {
            Some(regex(
                &wildcard(&criteria.excluded_dirs.join(";")),
                criteria.case_sensitive,
            )?)
        };
        Ok(Self {
            criteria,
            words,
            pattern,
            excluded,
        })
    }

    fn excluded(&self, entry: &Entry, root: &Path) -> bool {
        entry.is_dir
            && self.excluded.as_ref().is_some_and(|re| {
                re.is_match(entry.name())
                    || re.is_match(
                        &entry
                            .path
                            .strip_prefix(root)
                            .unwrap_or(&entry.path)
                            .to_string_lossy()
                            .replace('\\', "/"),
                    )
            })
    }

    /// `parent` is the folder of `e` below the root, as the words are written.
    fn matches(&self, e: &Entry, parent: &str) -> bool {
        let c = &self.criteria;
        let kind_matches = match c.file_type {
            FileType::All => true,
            FileType::Files => !e.is_dir,
            FileType::Folders => e.is_dir,
            FileType::Audio => e.kind == Kind::Audio,
            FileType::Video => e.kind == Kind::Video,
            FileType::Images => e.kind == Kind::Image,
            FileType::Documents => e.kind == Kind::Document,
            FileType::Text => e.kind == Kind::Text,
            FileType::Archives => e.kind == Kind::Archive,
        };
        if !kind_matches {
            return false;
        }
        if (c.min_size.is_some() || c.max_size.is_some()) && e.is_dir {
            return false;
        }
        if c.min_size.is_some_and(|n| e.size < n) || c.max_size.is_some_and(|n| e.size > n) {
            return false;
        }
        if (c.modified_from.is_some() || c.modified_before.is_some()) && e.modified.is_none() {
            return false;
        }
        if c.modified_from
            .is_some_and(|from| e.modified.is_some_and(|t| t < from))
            || c.modified_before
                .is_some_and(|before| e.modified.is_some_and(|t| t >= before))
        {
            return false;
        }
        if let Some(pattern) = &self.pattern {
            return pattern.is_match(e.name());
        }
        let name = if c.case_sensitive {
            e.name()
        } else {
            e.lname()
        };
        words_match(&self.words, name, parent)
    }

    /// The folder below the root, in the case the words are written in.
    fn parent_text(&self, folder: &Path, root: &Path) -> String {
        let text = folder.strip_prefix(root).unwrap_or(folder).to_string_lossy();
        if self.criteria.case_sensitive { text.into_owned() } else { text.to_lowercase() }
    }
}

/// Whether every word is in the name or in the folders above it, and one of them at least in
/// the name, so that a word matching a folder does not list everything inside it.
pub fn words_match<S: AsRef<str>>(words: &[S], name: &str, parent: &str) -> bool {
    let mut in_name = words.is_empty();
    for word in words {
        let word = word.as_ref();
        if name.contains(word) {
            in_name = true;
        } else if !parent.contains(word) {
            return false;
        }
    }
    in_name
}

/// The device a folder lies on, where the system tells it cheaply.
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

fn problem(progress: &Mutex<Progress>, path: PathBuf, error: std::io::Error) {
    let mut p = progress.lock().unwrap();
    p.skipped += 1;
    if p.problems.len() < PROBLEMS_MAX {
        p.problems.push((path, fsx::friendly_err(&error)));
    }
}

pub fn start(root: PathBuf, criteria: Criteria, ctx: egui::Context) -> Result<Search, String> {
    let filter = Filter::new(criteria)?;
    let search = Search {
        results: Arc::new(Mutex::new(Vec::new())),
        done: Arc::new(AtomicBool::new(false)),
        cancel: Arc::new(AtomicBool::new(false)),
        scanned: Arc::new(AtomicUsize::new(0)),
        progress: Arc::new(Mutex::new(Progress::default())),
    };
    let (results, done, cancel, scanned, progress) = (
        search.results.clone(),
        search.done.clone(),
        search.cancel.clone(),
        search.scanned.clone(),
        search.progress.clone(),
    );
    std::thread::spawn(move || {
        let mut folders = VecDeque::from([root.clone()]);
        let mut last = Instant::now();
        let mut outcome = Outcome::Complete;
        let mut limit_reason = None;
        let root_device = if filter.criteria.same_device {
            std::fs::metadata(&root).ok().as_ref().and_then(device)
        } else {
            None
        };
        'walk: while let Some(folder) = folders.pop_front() {
            if cancel.load(Ordering::Relaxed) {
                outcome = Outcome::Cancelled;
                break;
            }
            progress.lock().unwrap().current = folder.clone();
            let entries = match std::fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(e) => {
                    problem(&progress, folder, e);
                    continue;
                }
            };
            let parent = filter.parent_text(&folder, &root);
            for entry in entries {
                if cancel.load(Ordering::Relaxed) {
                    outcome = Outcome::Cancelled;
                    break 'walk;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        problem(&progress, folder.clone(), e);
                        continue;
                    }
                };
                scanned.fetch_add(1, Ordering::Relaxed);
                let path = entry.path();
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        problem(&progress, path, e);
                        continue;
                    }
                };
                let link = fsx::is_link_like(&metadata);
                let metadata = if link {
                    match std::fs::metadata(&path) {
                        Ok(m) => m,
                        Err(e) => {
                            problem(&progress, path, e);
                            continue;
                        }
                    }
                } else {
                    metadata
                };
                let elsewhere = root_device.is_some() && device(&metadata) != root_device;
                let e = fsx::make_entry(path, &metadata);
                if (!filter.criteria.hidden && e.hidden) || filter.excluded(&e, &root) {
                    continue;
                }
                if e.is_dir && !link && !elsewhere && filter.criteria.recursive {
                    if folders.len() >= FOLDERS_MAX {
                        outcome = Outcome::Limited;
                        limit_reason = Some("Too many pending folders; narrow the search scope");
                        break 'walk;
                    }
                    folders.push_back(e.path.clone());
                }
                if filter.matches(&e, &parent) {
                    let mut found = results.lock().unwrap();
                    found.push(e);
                    if found.len() >= filter.criteria.limit {
                        outcome = Outcome::Limited;
                        limit_reason =
                            Some("Result limit reached; narrow the search or add filters");
                        break 'walk;
                    }
                }
                if last.elapsed() >= Duration::from_millis(100) {
                    last = Instant::now();
                    ctx.request_repaint();
                }
            }
        }
        if cancel.load(Ordering::Relaxed) {
            outcome = Outcome::Cancelled;
        }
        {
            let mut p = progress.lock().unwrap();
            p.outcome = outcome;
            p.limit_reason = limit_reason;
        }
        done.store(true, Ordering::Release);
        ctx.request_repaint();
    });
    Ok(search)
}

#[cfg(test)]
pub fn search(root: PathBuf, query: String, hidden: bool, ctx: egui::Context) -> Search {
    search_within(root, query, hidden, None, ctx)
}

pub fn search_within(
    root: PathBuf,
    query: String,
    hidden: bool,
    limit: Option<usize>,
    ctx: egui::Context,
) -> Search {
    // These callers supply plain text and a fixed, valid limit. A quick search stays on the
    // disk it starts on: other disks are listed but not walked.
    start(
        root,
        Criteria {
            name: query,
            hidden,
            limit: limit.unwrap_or(RESULTS_MAX),
            same_device: true,
            ..Default::default()
        },
        ctx,
    )
    .expect("plain name search")
}

pub fn parse_size(text: &str) -> Result<Option<u64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let at = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let number = text[..at]
        .parse::<f64>()
        .map_err(|_| "Invalid size; use e.g. 10 MB or 1.5 GiB".to_string())?;
    let multiplier = match text[at..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1048576.0,
        "gib" => 1073741824.0,
        "tib" => 1099511627776.0,
        _ => return Err("Unknown size unit; use B, KB, MB, GB, TB or KiB, MiB, GiB, TiB".into()),
    };
    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes < 0.0 || bytes >= u64::MAX as f64 {
        return Err("Size is out of range".into());
    }
    Ok(Some(bytes.round() as u64))
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
