//! Reads folders on a few worker threads, and checks the visible ones for changes on
//! its own timer, so that an idle window does not redraw to notice them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::time::{Duration, Instant, SystemTime};

use crate::app::{Listing, computer};
use crate::background::Wake;
use crate::fsx::{self, Sorting};

/// One unresponsive network share occupies one worker; the others keep loading.
const WORKERS: usize = 4;
/// Requests waiting for a worker. A full queue is asked again on a later frame.
const QUEUE: usize = 32;

#[derive(Clone)]
pub struct FolderRequest {
    pub path: PathBuf,
    pub sorting: Sorting,
    pub old_mtime: Option<SystemTime>,
    pub force: bool,
    /// Sent by the watcher: an unchanged folder produces no result.
    pub poll: bool,
}

pub struct FolderResult {
    pub path: PathBuf,
    pub listing: Option<Listing>,
    /// The folder is gone, and this is the nearest folder still there.
    pub fallback: Option<PathBuf>,
    pub polled: bool,
    /// Asked for again while this reading was under way.
    pub again: bool,
}

pub struct FolderLoader {
    tx: mpsc::SyncSender<FolderRequest>,
    rx: mpsc::Receiver<FolderResult>,
    /// Folders the watcher checks for changes.
    watch: Arc<Mutex<Vec<FolderRequest>>>,
    /// Folders on their way, and whether they are to be read once more after that.
    pending: HashMap<PathBuf, bool>,
    wake: Wake,
}

impl FolderLoader {
    /// A bounded queue and a few workers: slow network paths never block rendering
    /// or accumulate a new thread on every refresh.
    pub fn start(wake: Wake) -> FolderLoader {
        let (tx, requests) = mpsc::sync_channel::<FolderRequest>(QUEUE);
        let requests = Arc::new(Mutex::new(requests));
        let (results, rx) = mpsc::channel();
        let watch = Arc::new(Mutex::new(Vec::new()));
        let watching = Arc::new(Mutex::new(HashSet::new()));
        spawn_watcher(Arc::downgrade(&watch), watching.clone(), tx.clone());
        let pace = Arc::new(Mutex::new(RefreshPace::default()));
        for _ in 0..WORKERS {
            let (requests, results, wake) = (requests.clone(), results.clone(), wake.clone());
            let (watching, pace) = (watching.clone(), pace.clone());
            std::thread::spawn(move || {
                // The closure consumes the lock guard, so it is released before the work.
                while let Ok(req) = requests.lock().map_err(drop).and_then(|queue| queue.recv().map_err(drop)) {
                    let poll = req.poll;
                    let path = req.path.clone();
                    let result = read(req, &pace);
                    if poll {
                        if let Ok(mut watching) = watching.lock() {
                            watching.remove(&path);
                        }
                        // Nothing changed: no reason to wake the window.
                        if result.listing.is_none() && result.fallback.is_none() {
                            continue;
                        }
                    }
                    if results.send(result).is_err() {
                        break;
                    }
                    wake.repaint();
                }
            });
        }
        FolderLoader { tx, rx, watch, pending: HashMap::new(), wake }
    }

    pub fn is_pending(&self, path: &Path) -> bool {
        self.pending.contains_key(path)
    }

    /// A folder already on its way is not asked for again. A forced request then has it
    /// read once more afterwards. True when `path` was on its way.
    pub fn coalesce(&mut self, path: &Path, force: bool) -> bool {
        let Some(again) = self.pending.get_mut(path) else { return false };
        *again |= force;
        true
    }

    pub fn send(&mut self, request: FolderRequest) {
        let path = request.path.clone();
        if self.tx.try_send(request).is_ok() {
            self.pending.insert(path, false);
        } else {
            // The queue is full, and the next frame asks again. It may be far off otherwise.
            self.wake.repaint_after(Duration::from_millis(50));
        }
    }

    /// The next folder read, if one is ready.
    pub fn receive(&mut self) -> Option<FolderResult> {
        let mut result = self.rx.try_recv().ok()?;
        result.again = self.pending.remove(&result.path).unwrap_or(false);
        Some(result)
    }

    /// Waits up to `wait` for the next folder read.
    #[cfg(test)]
    pub fn receive_within(&mut self, wait: Duration) -> Option<FolderResult> {
        let mut result = self.rx.recv_timeout(wait).ok()?;
        result.again = self.pending.remove(&result.path).unwrap_or(false);
        Some(result)
    }

    /// The folders on their way, and whether each is to be read once more.
    #[cfg(test)]
    pub fn pending(&self) -> &HashMap<PathBuf, bool> {
        &self.pending
    }

    /// What the watcher checks from now on.
    pub fn watch(&self, list: Vec<FolderRequest>) {
        if let Ok(mut watch) = self.watch.lock() {
            *watch = list;
        }
    }
}

/// Reads one folder, unless the watcher asked and nothing changed.
fn read(req: FolderRequest, pace: &Mutex<RefreshPace>) -> FolderResult {
    let metadata = std::fs::metadata(&req.path);
    let fallback = metadata
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        .then(|| req.path.ancestors().skip(1).find(|p| p.is_dir()).map(Path::to_path_buf).unwrap_or_else(computer));
    let mtime = metadata.and_then(|m| m.modified()).ok();
    // Asked for by the user, it is read at once. The watcher comes back anyway.
    let due = !req.poll || req.force || pace.lock().map_or(true, |pace| pace.due(&req.path, Instant::now()));
    let listing = (due && (req.force || mtime != req.old_mtime)).then(|| {
        let started = Instant::now();
        let (mut entries, error) = match fsx::read_dir(&req.path) {
            Ok(entries) => (entries, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        fsx::sort_entries(&mut entries, req.sorting);
        if let Ok(mut pace) = pace.lock() {
            pace.note(&req.path, started.elapsed(), Instant::now());
        }
        Listing { mtime, error, ..Listing::empty(entries, req.sorting) }
    });
    FolderResult { path: req.path, listing, fallback, polled: req.poll, again: false }
}

/// Checks the watched folders every `REFRESH_MS`. It stops when the loader that owns
/// `watch` is gone.
fn spawn_watcher(watch: Weak<Mutex<Vec<FolderRequest>>>, watching: Arc<Mutex<HashSet<PathBuf>>>, folders: mpsc::SyncSender<FolderRequest>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(crate::app::REFRESH_MS));
        let Some(watch) = watch.upgrade() else { return };
        let Ok(list) = watch.lock().map(|list| list.clone()) else { return };
        drop(watch);
        for req in list {
            let Ok(mut watching) = watching.lock() else { return };
            // A silent share stays in flight here instead of queueing up every tick.
            if !watching.insert(req.path.clone()) {
                continue;
            }
            match folders.try_send(req) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(req)) => {
                    watching.remove(&req.path);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return,
            }
        }
    });
}

/// Reading and sorting a huge folder takes seconds, and a copy into it changes it all the
/// time. It is read again only after a multiple of what the last reading took.
#[derive(Default)]
struct RefreshPace {
    wait_until: HashMap<PathBuf, Instant>,
}

impl RefreshPace {
    const SLOW_READ: Duration = Duration::from_millis(100);
    const WAIT_FACTOR: u32 = 5;
    const LONGEST_WAIT: Duration = Duration::from_secs(10);

    fn due(&self, path: &Path, now: Instant) -> bool {
        self.wait_until.get(path).is_none_or(|until| now >= *until)
    }

    fn note(&mut self, path: &Path, took: Duration, now: Instant) {
        if took < Self::SLOW_READ {
            self.wait_until.remove(path);
        } else {
            self.wait_until.insert(path.to_path_buf(), now + (took * Self::WAIT_FACTOR).min(Self::LONGEST_WAIT));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_huge_changing_folder_is_read_again_less_often() {
        let (mut pace, now, huge) = (RefreshPace::default(), Instant::now(), Path::new("huge"));
        assert!(pace.due(huge, now), "a folder never read is read");
        pace.note(huge, Duration::from_secs(1), now);
        assert!(!pace.due(huge, now + Duration::from_secs(4)));
        assert!(pace.due(huge, now + Duration::from_secs(5)));
        pace.note(huge, Duration::from_secs(60), now);
        assert!(pace.due(huge, now + Duration::from_secs(10)), "not for longer than this");
        pace.note(huge, Duration::from_millis(10), now);
        assert!(pace.due(huge, now), "quick to read, it follows every change");
    }

    #[test]
    fn an_unchanged_folder_is_read_only_when_forced_and_a_gone_one_falls_back() {
        let dir = std::env::temp_dir().join(format!("fc-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let pace = Mutex::new(RefreshPace::default());
        let ask = |path: &Path, old_mtime, force, poll| FolderRequest { path: path.to_path_buf(), sorting: Sorting::default(), old_mtime, force, poll };
        let first = read(ask(&dir, None, false, true), &pace);
        let listing = first.listing.expect("a new folder is read");
        assert_eq!(listing.entries.len(), 1);
        assert!(first.fallback.is_none());
        assert!(read(ask(&dir, listing.mtime, false, true), &pace).listing.is_none(), "unchanged");
        assert!(read(ask(&dir, listing.mtime, true, false), &pace).listing.is_some(), "forced");
        let gone = dir.join("gone");
        assert_eq!(read(ask(&gone, None, true, false), &pace).fallback.as_deref(), Some(dir.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
