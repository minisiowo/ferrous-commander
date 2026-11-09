//! The application state and all actions. Drawing lives in `ui.rs`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use egui::Color32;

use crate::fsx::{self, Entry, Sorting};
use crate::ops::{self, Job, OpKind};
use crate::preview::Preview;
use crate::settings::{FavGroup, Layout, PaneSession, Settings};
use crate::theme::{Palette, ThemeName, hex};
use crate::tiles::Tile;

/// Forgetting old listings and renewing the watch list need no doing on every frame.
const EVICT_EVERY: Duration = Duration::from_secs(1);

/// Folder refresh poll.
pub const REFRESH_MS: u64 = crate::defaults::REFRESH_MS;
/// One unresponsive network share occupies one worker; the others keep loading.
const FOLDER_WORKERS: usize = 4;
/// Slowest redraw while nothing happens, for the drive list, sidebar and clipboard.
/// Everything that needs the window sooner wakes it.
const IDLE_HEARTBEAT: Duration = Duration::from_secs(5);
/// Longest the interface waits to learn what a path is. A sleeping share can take
/// half a minute; past this limit the background folder loader takes over.
pub const PATH_PROBE_LIMIT: Duration = Duration::from_millis(150);
/// Toast lifetime.
pub const TOAST_MS: u64 = crate::defaults::TOAST_MS;
/// History pane length.
pub const HISTORY_MAX: usize = crate::defaults::HISTORY_MAX;

static START_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// A folder given on the command line opens in a new tab of the first pane.
pub fn set_start_dir(p: PathBuf) {
    let _ = START_DIR.set(p);
}

/// The "This PC" level above the drives and mounted devices.
pub fn computer() -> PathBuf {
    PathBuf::new()
}
pub fn is_computer(p: &Path) -> bool {
    p.as_os_str().is_empty()
}

#[derive(Clone, Debug)]
pub struct DriveInfo {
    pub path: PathBuf,
    pub name: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
    /// Network share (mapped drive, NFS/SMB mount); `remote` holds its server path.
    pub network: bool,
    pub remote: String,
    /// What `drives::eject` needs, on drives that can be taken offline.
    pub eject: Option<String>,
}

#[derive(Clone)]
struct FolderRequest {
    path: PathBuf,
    sorting: Sorting,
    old_mtime: Option<SystemTime>,
    force: bool,
    /// Sent by the watcher: an unchanged folder produces no result.
    poll: bool,
}
struct FolderResult {
    path: PathBuf,
    listing: Option<Listing>,
    fallback: Option<PathBuf>,
    polled: bool,
}

pub struct Listing {
    pub entries: Vec<Entry>,
    pub error: Option<String>,
    pub mtime: Option<SystemTime>,
    pub generation: u64,
    pub sorting: Sorting,
    pub used: Instant,
}

#[derive(Clone, Debug)]
pub struct Tab {
    pub path: PathBuf,
    pub back: Vec<PathBuf>,
    pub fwd: Vec<PathBuf>,
}

impl Tab {
    pub fn new(path: PathBuf) -> Tab {
        Tab {
            path,
            back: Vec::new(),
            fwd: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub closed: Vec<PathBuf>,
    pub selected: HashSet<PathBuf>,
    pub cursor: Option<usize>,
    pub cursor_path: Option<PathBuf>,
    pub anchor: Option<usize>,
    pub filter: String,
    pub path_edit: Option<String>,
    /// The path field selects its whole text the next time it is drawn.
    pub path_select_all: bool,
    /// Neovim mode: moving the cursor selects rows.
    pub vim_visual: bool,
    /// Neovim mode: letters are going into the filter.
    pub vim_filtering: bool,
    pub rename: Option<(PathBuf, String)>,
    pub rename_focus: bool,
    pub expanded: HashSet<PathBuf>,
    pub tree_entries: Vec<Entry>,
    // Expanded paths and the listing revision used to flatten this tree.
    tree_key: Option<(PathBuf, u64, bool, HashSet<PathBuf>)>,
    pub view: Vec<usize>,
    pub view_key: Option<(PathBuf, u64, String, bool)>,
    pub scroll_to: Option<usize>,
    pub list_offset: f32,
    pub list_h: f32,
    pub list_w: f32,
    pub pending_select: Option<PathBuf>,
    /// New items selected before the folder's fresh listing shows them.
    arriving: HashSet<PathBuf>,
}

impl Pane {
    pub fn path(&self) -> &Path {
        &self.tabs[self.active].path
    }
    pub fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
    /// Leave no row in use, so nothing is outlined or acted on until the next pick.
    pub fn clear_cursor(&mut self) {
        self.cursor = None;
        self.cursor_path = None;
        self.anchor = None;
    }
    fn clear_view_state(&mut self) {
        self.arriving.clear();
        self.expanded.clear();
        self.tree_entries.clear();
        self.tree_key = None;
        self.selected.clear();
        self.cursor = None;
        self.cursor_path = None;
        self.anchor = None;
        self.filter.clear();
        self.rename = None;
        self.path_edit = None;
        self.path_select_all = false;
        self.vim_visual = false;
        self.vim_filtering = false;
        self.view_key = None;
        self.list_offset = 0.0;
        self.scroll_to = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub at: Instant,
}

pub enum Dialog {
    AddLocation { group: usize, name: String, focus: bool },
    RenameFavorite { group: usize, path: PathBuf, name: String, focus: bool },
    NewFolder {
        pane: usize,
        name: String,
        focus: bool,
    },
    NewFile {
        pane: usize,
        name: String,
        focus: bool,
    },
    RenameGroup {
        group: usize,
        name: String,
        focus: bool,
    },
    NewGroup {
        name: String,
        focus: bool,
    },
    Compress {
        pane: usize,
        sources: Vec<PathBuf>,
        name: String,
        kind: crate::compress::Kind,
        focus: bool,
    },
    Password {
        archive: PathBuf,
        dest: Option<PathBuf>,
        wrong: bool,
        name: String,
        focus: bool,
    },
}

/// Something the user must see or decide. Unlike a toast, it stays until answered.
pub enum Notice {
    /// Each item with why it cannot go to the trash; empty when the user asked to skip it.
    ConfirmDestroy { items: Vec<(PathBuf, String)> },
    ConfirmClose,
    /// A program from a place the user does not control. See `App::open`.
    ConfirmRun { path: PathBuf, place: &'static str, link: Option<String> },
    Problems { title: String, lines: Vec<String> },
}

pub struct SearchUi {
    pub query: String,
    pub root: PathBuf,
    pub run: Option<ops::Search>,
    pub focus: bool,
}

/// Paths dragged between panes, tabs, folders and favourites.
#[derive(Clone)]
pub struct DragPaths(pub Vec<PathBuf>);

/// A piece of the path bar being dragged: stands for the folder, and never moves it.
#[derive(Clone)]
pub struct CrumbDrag {
    pub pane: usize,
    pub path: PathBuf,
}

/// A sidebar shortcut being rearranged: moves the shortcut, never the folder.
#[derive(Clone)]
pub struct FavDrag {
    pub group: usize,
    pub index: usize,
}

#[derive(Clone)]
pub enum DropTarget {
    Folder(PathBuf),
    /// A place between a group's shortcuts; `at` indexes the group's paths.
    Favorites { group: usize, at: usize },
}

pub enum Act {
    Navigate(usize, PathBuf),
    NavigateSelect(usize, PathBuf, PathBuf),
    Open(usize, PathBuf),
    OpenNewTab(usize, PathBuf, bool),
    OpenOtherPane(usize, PathBuf),
    NewTab(usize),
    CloseTab(usize, usize),
    SwitchTab(usize, usize),
    DockTab { source: usize, tab: usize, target: usize, at: usize, split: Option<(bool, bool)> },
    DockPanel { source: usize, target: usize, at: usize, split: Option<(bool, bool)> },
    ReopenTab(usize),
    Back(usize),
    Forward(usize),
    Up(usize),
    Copy(usize),
    Cut(usize),
    Paste(usize, Option<PathBuf>),
    ToOther(usize, bool),
    #[cfg(target_os = "linux")]
    LinkToOther(usize, bool),
    Delete(usize),
    DeleteForever(usize),
    Rename(usize),
    CommitRename(usize),
    NewFolder(usize),
    NewFile(usize),
    Duplicate(usize),
    CopyPath(usize),
    #[cfg(target_os = "linux")]
    Properties(PathBuf),
    Drop {
        target: PathBuf,
        paths: Vec<PathBuf>,
        copy: bool,
    },
    TagPaths(Vec<PathBuf>, Option<u8>),
    AddFavorite(usize, PathBuf),
    RemoveFavorite(usize, usize),
    MoveFavorite { from: usize, index: usize, to: usize, at: usize },
    PlaceFavorites { group: usize, at: usize, paths: Vec<PathBuf> },
    DeleteGroup(usize),
    MoveGroup(usize, i32),
    Refresh,
    Terminal(PathBuf),
    SetSort(usize, fsx::SortBy),
    ToggleFoldersOnTop(usize),
    SelectAll(usize),
    Search(usize),
    /// Open a new panel split from this one (optionally at a path).
    OpenPanel(usize, Option<PathBuf>),
    ClosePanel(usize),
    ToggleSplit(usize),
    OpenWith(PathBuf, Option<crate::openwith::AppEntry>),
    /// Unpack these archives into this folder, or next to each archive.
    Extract(Vec<PathBuf>, Option<PathBuf>),
    /// Ask for a name and format, then pack these items in the pane's folder.
    Compress(usize, Vec<PathBuf>),
}

pub struct ShotState {
    pub file: PathBuf,
    pub requested: bool,
    pub done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct App {
    pub st: Settings,
    pub keys: crate::keymap::Keymap,
    pub matcher: crate::keymap::Matcher,
    pub pal: Palette,
    pub panes: Vec<Pane>,
    pub tiles: Tile,
    pub active: usize,
    /// The panel used before the active one: target of "copy to other panel".
    pub prev_active: usize,
    /// Panel rectangles from the last frame (to split along the longer side).
    pub pane_rects: Vec<(usize, egui::Rect)>,
    pub cloud: Vec<PathBuf>,
    drives_rx: Option<mpsc::Receiver<(Vec<DriveInfo>, Vec<PathBuf>)>>,
    pub cache: HashMap<PathBuf, Listing>,
    folder_tx: mpsc::SyncSender<FolderRequest>,
    /// Lets background threads wake the window as soon as a result is ready.
    wake: std::sync::Arc<std::sync::OnceLock<egui::Context>>,
    /// Folders the background watcher checks for changes, without drawing a frame.
    watch: std::sync::Arc<std::sync::Mutex<Vec<FolderRequest>>>,
    folder_rx: mpsc::Receiver<FolderResult>,
    folder_pending: HashMap<PathBuf, bool>,
    pub drives: Vec<DriveInfo>,
    drives_at: Option<Instant>,
    pub clip: Option<(std::sync::Arc<[PathBuf]>, bool)>,
    #[cfg(windows)]
    clip_sequence: u32,
    pub jobs: Vec<Job>,
    pub toasts: Vec<Toast>,
    open_with_pending: Vec<(PathBuf, mpsc::Receiver<Result<(), String>>)>,
    /// Drives being ejected: mount point, name and the eventual outcome.
    pub ejecting: Vec<(PathBuf, String, mpsc::Receiver<Result<String, String>>)>,
    pub dialog: Option<Dialog>,
    /// Archives waiting for a password, one question at a time.
    locked: VecDeque<(PathBuf, Option<PathBuf>, bool)>,
    pub notices: Vec<Notice>,
    /// The window closes as soon as the cancelled operations have stopped.
    pub closing: bool,
    #[cfg(target_os = "linux")]
    pub properties: Option<crate::linux_properties::Properties>,
    #[cfg(not(windows))]
    pub app_chooser: Option<crate::openwith::Chooser>,
    pub preview: Preview,
    pub preview_window: bool,
    pub quick: Option<crate::preview::window::Quick>,
    /// When the preview window closed itself; its key press must not open it again.
    pub preview_closed: Option<Instant>,
    pub search: Option<SearchUi>,
    pub settings_open: bool,
    pub shot: Option<ShotState>,
    pub frame: u64,
    dirty: bool,
    last_save: Instant,
    last_evict: Instant,
    watched_generation: u64,
    applied: Option<(ThemeName, String, u32, Option<crate::omarchy::Colors>)>,
    omarchy_watch: Option<crate::omarchy::Watch>,
    omarchy_colors: Option<crate::omarchy::Colors>,
    persist: bool,
    generation: u64,
    pub drop_zones: Vec<(egui::Rect, DropTarget)>,
    pub acts: Vec<Act>,
    /// Last highlighted child per (pane, column) so a column scrolls to it once.
    pub col_hl: HashMap<(usize, PathBuf), PathBuf>,
}

/// Checks visible folders on its own timer, so that an idle window does not redraw
/// to notice changes. It stops when the app that owns `watch` is gone.
fn spawn_folder_watcher(
    watch: std::sync::Weak<std::sync::Mutex<Vec<FolderRequest>>>,
    watching: std::sync::Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    folders: mpsc::SyncSender<FolderRequest>,
) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(REFRESH_MS));
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

impl App {
    pub fn new(shot: Option<PathBuf>) -> App {
        let persist = shot.is_none();
        let mut st = if persist {
            Settings::load()
        } else {
            Settings::default()
        };
        if !persist {
            // A clean, deterministic first-start picture: start in the home folder.
            st.panes = Vec::new();
            st.tiles = None;
            // For checking other looks: FC_SHOT=columns,light,dual,gruvbox
            let look = std::env::var("FC_SHOT").unwrap_or_default();
            if look.contains("columns") {
                st.layout = Layout::Columns;
            }
            st.tree_files = look.contains("tree");
            if look.contains("light") {
                st.theme = ThemeName::CatppuccinLatte;
            }
            if look.contains("gruvbox") {
                st.theme = ThemeName::Gruvbox;
            }
            if look.contains("rose") {
                st.theme = ThemeName::RosePine;
            }
            if look.contains("tokyo") {
                st.theme = ThemeName::TokyoNight;
            }
            if look.contains("omarchy") {
                st.theme = ThemeName::Omarchy;
            }
            if look.contains("single") {
                st.dual = false;
            }
            if look.contains("full") {
                st.show_columns = true;
                st.show_preview = true;
            }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let n = if st.panes.is_empty() {
            if st.dual { 2 } else { 1 }
        } else {
            st.panes.len()
        };
        let mut panes: Vec<Pane> = (0..n).map(|_| Pane::default()).collect();
        for (i, p) in panes.iter_mut().enumerate() {
            let sess = st.panes.get(i).cloned().unwrap_or_default();
            // No disk access here: the folder loader moves a tab whose folder is gone
            // to its nearest parent, and a sleeping share cannot delay the window.
            // A relative path saved by an older version means nothing in a later run.
            let mut tabs: Vec<Tab> = sess.tabs.iter().filter(|p| p.is_absolute() || is_computer(p)).cloned().map(Tab::new).collect();
            if tabs.is_empty() {
                let start = if i == 0 {
                    home.clone()
                } else {
                    dirs::download_dir().unwrap_or_else(|| home.clone())
                };
                tabs.push(Tab::new(start));
            }
            p.active = sess.active.min(tabs.len() - 1);
            if i == 0
                && let Some(d) = START_DIR.get() {
                    tabs.push(Tab::new(d.clone()));
                    p.active = tabs.len() - 1;
                }
            p.tabs = tabs;
        }
        let accent = hex(&st.accent);
        let tiles = st.tiles.clone().unwrap_or(Tile::Leaf(0)).sanitize(n);
        // A bounded queue and a few workers: slow network paths never block rendering
        // or accumulate a new thread on every refresh.
        let (folder_tx, requests) = mpsc::sync_channel::<FolderRequest>(32);
        let requests = std::sync::Arc::new(std::sync::Mutex::new(requests));
        let (results, folder_rx) = mpsc::channel();
        let wake = std::sync::Arc::new(std::sync::OnceLock::<egui::Context>::new());
        let watch = std::sync::Arc::new(std::sync::Mutex::new(Vec::<FolderRequest>::new()));
        let watching = std::sync::Arc::new(std::sync::Mutex::new(HashSet::<PathBuf>::new()));
        spawn_folder_watcher(std::sync::Arc::downgrade(&watch), watching.clone(), folder_tx.clone());
        for _ in 0..FOLDER_WORKERS {
            let (requests, results, wake) = (requests.clone(), results.clone(), wake.clone());
            let watching = watching.clone();
            std::thread::spawn(move || {
                // The closure consumes the lock guard, so it is released before the work.
                while let Ok(req) = requests.lock().map_err(drop).and_then(|queue| queue.recv().map_err(drop)) {
                let metadata = std::fs::metadata(&req.path);
                let fallback = if metadata
                    .as_ref()
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    Some(
                        req.path
                            .ancestors()
                            .skip(1)
                            .find(|p| p.is_dir())
                            .map(Path::to_path_buf)
                            .unwrap_or_else(computer),
                    )
                } else {
                    None
                };
                let mtime = metadata.and_then(|m| m.modified()).ok();
                let listing = if req.force || mtime != req.old_mtime {
                    let (mut entries, error) = match fsx::read_dir(&req.path) {
                        Ok(entries) => (entries, None),
                        Err(error) => (Vec::new(), Some(error)),
                    };
                    fsx::sort_entries(&mut entries, req.sorting);
                    Some(Listing {
                        entries,
                        error,
                        mtime,
                        generation: 0,
                        sorting: req.sorting,
                        used: Instant::now(),
                    })
                } else {
                    None
                };
                if req.poll {
                    if let Ok(mut watching) = watching.lock() {
                        watching.remove(&req.path);
                    }
                    // Nothing changed: no reason to wake the window.
                    if listing.is_none() && fallback.is_none() {
                        continue;
                    }
                }
                if results
                    .send(FolderResult {
                        path: req.path,
                        polled: req.poll,
                        listing,
                        fallback,
                    })
                    .is_err()
                {
                    break;
                }
                if let Some(ctx) = wake.get() {
                    ctx.request_repaint();
                }
                }
            });
        }
        App {
            folder_tx,
            wake,
            watch,
            folder_rx,
            folder_pending: HashMap::new(),
            pal: Palette::get(st.theme, accent),
            keys: crate::keymap::Keymap::new(&st.keymap),
            matcher: Default::default(),
            st,
            panes,
            tiles,
            active: 0,
            prev_active: 0,
            pane_rects: Vec::new(),
            cloud: Vec::new(),
            drives_rx: None,
            cache: HashMap::new(),
            drives: Vec::new(),
            drives_at: None,
            clip: None,
            #[cfg(windows)]
            clip_sequence: 0,
            jobs: Vec::new(),
            toasts: Vec::new(),
            open_with_pending: Vec::new(),
            ejecting: Vec::new(),
            dialog: None,
            locked: VecDeque::new(),
            notices: Vec::new(),
            closing: false,
            #[cfg(target_os = "linux")]
            properties: None,
            #[cfg(not(windows))]
            app_chooser: None,
            preview: Preview::new(),
            preview_window: false,
            quick: None,
            preview_closed: None,
            search: None,
            settings_open: false,
            shot: shot.map(|file| ShotState {
                file,
                requested: false,
                done: Default::default(),
            }),
            frame: 0,
            dirty: false,
            last_save: Instant::now(),
            last_evict: Instant::now(),
            watched_generation: 0,
            applied: None,
            omarchy_watch: None,
            omarchy_colors: None,
            persist,
            generation: 1,
            drop_zones: Vec::new(),
            acts: Vec::new(),
            col_hl: HashMap::new(),
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            at: Instant::now(),
        });
        if self.toasts.len() > 5 {
            self.toasts.remove(0);
        }
    }

    /// The panel to copy/move to: the one used before this one, else the next.
    pub fn other(&self, pane: usize) -> usize {
        let n = self.panes.len();
        if n < 2 {
            pane
        } else if self.prev_active != pane && self.prev_active < n {
            self.prev_active
        } else {
            (pane + 1) % n
        }
    }

    pub fn set_active(&mut self, i: usize) {
        if i != self.active && i < self.panes.len() {
            self.prev_active = self.active;
            self.active = i;
        }
    }

    /// Open a new panel next to `from` (Hyprland dwindle: split along the longer side).
    pub fn open_panel(&mut self, from: usize, path: Option<PathBuf>) -> usize {
        let p = path.unwrap_or_else(|| self.panes[from].path().to_path_buf());
        let mut leaves = Vec::new();
        self.tiles.leaves(&mut leaves);
        let target = *leaves.last().unwrap_or(&from);
        let vertical = self
            .pane_rects
            .iter()
            .find(|(k, _)| *k == target)
            .is_some_and(|(_, r)| r.height() > r.width());
        let mut pane = Pane::default();
        pane.tabs.push(Tab::new(p));
        self.panes.push(pane);
        let new = self.panes.len() - 1;
        if !self.tiles.split(target, new, vertical) {
            self.tiles = self.tiles.clone().sanitize(self.panes.len());
        }
        self.set_active(new);
        self.mark_dirty();
        new
    }

    pub fn close_panel(&mut self, i: usize) {
        if self.panes.len() < 2 || !self.tiles.remove(i) {
            return;
        }
        self.panes.remove(i);
        let fix = |k: usize| {
            if k > i {
                k - 1
            } else if k == i {
                0
            } else {
                k
            }
        };
        self.active = fix(self.prev_active).min(self.panes.len() - 1);
        self.prev_active = self.active;
        self.col_hl.clear();
        self.mark_dirty();
    }

    // ---------------------------------------------------------------- per frame

    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        self.wake.get_or_init(|| ctx.clone());
        if self.st.theme == ThemeName::Omarchy {
            if self.omarchy_watch.is_none() {
                self.omarchy_watch = crate::omarchy::Watch::start(ctx);
            }
            if let Some(colors) = self.omarchy_watch.as_ref().and_then(|w| w.latest()) {
                self.omarchy_colors = Some(colors);
            }
        } else {
            self.omarchy_watch = None;
            self.omarchy_colors = None;
        }
        let key = (
            self.st.theme,
            self.st.accent.clone(),
            self.st.font_size.to_bits(),
            self.omarchy_colors,
        );
        if self.applied.as_ref() != Some(&key) {
            self.pal = match self.omarchy_colors {
                Some(colors) => colors.palette(),
                None => Palette::get(self.st.theme, hex(&self.st.accent)),
            };
            self.pal.apply(ctx);
            let fs = self.st.font_size;
            ctx.global_style_mut(|s| {
                use egui::{FontFamily, FontId, TextStyle};
                s.text_styles
                    .insert(TextStyle::Body, FontId::new(fs, FontFamily::Proportional));
                s.text_styles
                    .insert(TextStyle::Button, FontId::new(fs, FontFamily::Proportional));
                s.text_styles.insert(
                    TextStyle::Small,
                    FontId::new(fs - 2.0, FontFamily::Proportional),
                );
                s.text_styles.insert(
                    TextStyle::Heading,
                    FontId::new(fs + 1.0, FontFamily::Proportional),
                );
            });
            self.applied = Some(key);
        }
        self.drop_zones.clear();
        #[cfg(windows)]
        {
            let sequence = crate::file_transfer::sequence();
            if self.clip_sequence != sequence
                && let Ok(files) = crate::file_transfer::read() {
                    self.clip = files.map(|(paths, cut)| (paths.into(), cut));
                    self.clip_sequence = sequence;
                }
        }
        self.toasts
            .retain(|t| t.at.elapsed() < Duration::from_millis(TOAST_MS));

        if self
            .drives_at
            .is_none_or(|t| t.elapsed() > Duration::from_secs(10))
        {
            self.refresh_drives();
        }
        self.receive_drives(ctx);
        self.finish_jobs();
        self.ask_for_password();
        self.receive_preview();
        for i in (0..self.ejecting.len()).rev() {
            let result = match self.ejecting[i].2.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => continue,
                Err(mpsc::TryRecvError::Disconnected) => Err("Ejecting stopped unexpectedly".into()),
            };
            let (mount, name, _) = self.ejecting.swap_remove(i);
            self.finish_eject(&mount, &name, result);
        }
        for i in (0..self.open_with_pending.len()).rev() {
            let result = match self.open_with_pending[i].1.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Application handler stopped unexpectedly".into())
                }
            };
            let (path, _) = self.open_with_pending.swap_remove(i);
            match result {
                Ok(()) => self.toast(
                    ToastKind::Info,
                    format!("Opened {}", fsx::display_name(&path)),
                ),
                Err(e) => self.toast(
                    ToastKind::Error,
                    format!("Couldn't open {}: {e}", fsx::display_name(&path)),
                ),
            }
        }
        while let Ok(result) = self.folder_rx.try_recv() {
            let again = self.folder_pending.remove(&result.path).unwrap_or(false);
            // A watcher result may predate a change of the folder's sorting.
            let stale = result.polled && result.listing.as_ref().is_some_and(|l| l.sorting != self.sorting_for(&result.path));
            if let Some(mut listing) = result.listing.filter(|_| !stale) {
                self.generation += 1;
                listing.generation = self.generation;
                self.cache.insert(result.path.clone(), listing);
            }
            if let Some(parent) = result.fallback {
                for pane in &mut self.panes {
                    if pane.path() == result.path {
                        pane.tab().path = parent.clone();
                        pane.clear_view_state();
                    }
                }
            }
            if again {
                self.request_folder(&result.path, true);
            }
        }

        ctx.request_repaint_after(IDLE_HEARTBEAT);
        let toast_life = Duration::from_millis(TOAST_MS);
        if let Some(left) = self.toasts.iter().map(|t| toast_life.saturating_sub(t.at.elapsed())).min() {
            ctx.request_repaint_after(left);
        }

        for i in 0..self.panes.len() {
            let p = self.panes[i].path().to_path_buf();
            if self.st.show_columns || self.st.layout == Layout::Columns {
                for a in self.column_chain(&p) {
                    self.ensure(&a);
                }
            }
            self.ensure(&p);
            if self.st.tree_files {
                self.build_tree(i, &p);
            } else if self.panes[i].tree_key.take().is_some() {
                self.panes[i].tree_entries.clear();
                self.panes[i].view_key = None;
            }
            self.update_view(i);
        }
        let due = self.last_evict.elapsed() >= EVICT_EVERY;
        if due {
            self.last_evict = Instant::now();
            self.evict();
            self.preview.expire();
        }
        // A new listing changes what the watcher should compare with.
        if due || self.generation != self.watched_generation {
            self.watched_generation = self.generation;
            self.publish_watch_list();
        }
    }

    /// Closing kills worker threads, so a running operation would stop at a random
    /// point with its remaining items silently unprocessed.
    pub fn guard_close(&mut self, ctx: &egui::Context) {
        if self.closing && self.jobs.is_empty() {
            self.closing = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.jobs.is_empty() && ctx.input(|inp| inp.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing && !self.notices.iter().any(|n| matches!(n, Notice::ConfirmClose)) {
                self.notices.push(Notice::ConfirmClose);
            }
        }
    }

    pub fn end_frame(&mut self, ctx: &egui::Context) {
        let acts = std::mem::take(&mut self.acts);
        for a in acts {
            self.run(a, ctx);
        }
        if self.persist && self.dirty && self.last_save.elapsed() > Duration::from_secs(2) {
            self.save();
        }
    }

    pub fn can_paste_files(&self) -> bool {
        #[cfg(windows)]
        { crate::file_transfer::has_files() }
        #[cfg(not(windows))]
        { self.clip.is_some() }
    }

    pub fn save(&mut self) {
        if !self.persist {
            return;
        }
        self.st.panes = self
            .panes
            .iter()
            .map(|p| PaneSession {
                tabs: p.tabs.iter().map(|t| t.path.clone()).collect(),
                active: p.active,
            })
            .collect();
        self.st.tiles = Some(self.tiles.clone());
        self.st.save();
        self.dirty = false;
        self.last_save = Instant::now();
    }

    /// Folder columns shown left of / above the current folder: the computer level,
    /// then every ancestor down to the current folder.
    pub fn column_chain(&self, p: &Path) -> Vec<PathBuf> {
        let mut v = Vec::new();
        v.push(computer());
        if !is_computer(p) {
            v.extend(fsx::ancestors(p));
        }
        v
    }

    pub fn eject(&mut self, drive: &DriveInfo) {
        let Some(id) = drive.eject.clone() else { return };
        if self.ejecting.iter().any(|(mount, ..)| *mount == drive.path) { return; }
        // Everything is refused, not only jobs touching this drive: a job does not list
        // its sources, and pulling a drive under a running one is what this must prevent.
        if !self.jobs.is_empty() {
            self.notices.push(Notice::Problems {
                title: format!("{} was not ejected", drive.name),
                lines: vec!["File operations are still running. Wait for them to finish, then try again.".into()],
            });
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (mount, wake) = (drive.path.clone(), self.wake.clone());
        self.ejecting.push((mount.clone(), drive.name.clone(), rx));
        self.toast(ToastKind::Info, format!("Ejecting {}. Keep it connected until this finishes", drive.name));
        std::thread::spawn(move || {
            let _ = tx.send(crate::drives::eject(&mount, &id));
            if let Some(ctx) = wake.get() { ctx.request_repaint(); }
        });
    }

    fn finish_eject(&mut self, mount: &Path, name: &str, result: Result<String, String>) {
        match result {
            Ok(message) => {
                self.toast(ToastKind::Success, format!("{name}: {message}"));
                // Tabs must not keep pointing at a folder that is gone.
                for i in 0..self.panes.len() {
                    let active = self.panes[i].active;
                    for t in 0..self.panes[i].tabs.len() {
                        let tab = &mut self.panes[i].tabs[t];
                        if !tab.path.starts_with(mount) { continue; }
                        tab.back.retain(|p| !p.starts_with(mount));
                        tab.fwd.retain(|p| !p.starts_with(mount));
                        tab.path = computer();
                        if t == active { self.panes[i].clear_view_state(); }
                    }
                }
                self.mark_dirty();
                self.refresh_drives();
            }
            Err(error) => self.notices.push(Notice::Problems { title: format!("{name} was not ejected"), lines: vec![error] }),
        }
    }

    /// Ask a background thread for the drive list (network drives can take seconds to answer).
    pub fn refresh_drives(&mut self) {
        self.drives_at = Some(Instant::now());
        if self.drives_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let _ = tx.send((crate::drives::list(), crate::drives::cloud_folders()));
            if let Some(ctx) = wake.get() {
                ctx.request_repaint();
            }
        });
        self.drives_rx = Some(rx);
    }

    fn receive_drives(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.drives_rx else { return };
        match rx.try_recv() {
            Ok((drives, cloud)) => {
                self.drives = drives;
                self.cloud = cloud;
                self.drives_rx = None;
                if let Some(l) = self.cache.get_mut(&computer()) {
                    l.error = Some("reload".into());
                }
                ctx.request_repaint();
            }
            Err(mpsc::TryRecvError::Disconnected) => self.drives_rx = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    fn sorting_for(&self, p: &Path) -> Sorting {
        self.st
            .folder_sorting
            .get(&p.to_string_lossy().to_string())
            .copied()
            .unwrap_or(self.st.sorting)
    }

    fn load(&mut self, p: &Path) -> Listing {
        self.generation += 1;
        let sorting = self.sorting_for(p);
        if is_computer(p) {
            let entries = self
                .drives
                .iter()
                .map(|d| {
                    // no file-system call here: a network drive may be asleep
                    let name = if d.label.is_empty() || !cfg!(windows) {
                        d.name.clone()
                    } else {
                        format!("{} {}", d.name, d.label)
                    };
                    let mut e = Entry::named(d.path.clone(), name, true, fsx::Kind::Folder);
                    e.size = d.total.saturating_sub(d.free);
                    e
                })
                .collect();
            return Listing {
                entries,
                error: None,
                mtime: None,
                generation: self.generation,
                sorting,
                used: Instant::now(),
            };
        }
        // Initial placeholder; the worker supplies the actual listing.
        Listing {
            entries: Vec::new(),
            error: None,
            mtime: None,
            generation: 0,
            sorting,
            used: Instant::now(),
        }
    }

    pub(crate) fn request_folder(&mut self, p: &Path, force: bool) {
        if is_computer(p) {
            return;
        }
        if let Some(again) = self.folder_pending.get_mut(p) {
            *again |= force;
            return;
        }
        let req = FolderRequest {
            path: p.to_path_buf(),
            sorting: self.sorting_for(p),
            old_mtime: self.cache.get(p).and_then(|l| l.mtime),
            force,
            poll: false,
        };
        if self.folder_tx.try_send(req).is_ok() {
            self.folder_pending.insert(p.to_path_buf(), false);
        }
    }

    pub fn ensure(&mut self, p: &Path) {
        let need = match self.cache.get_mut(p) {
            Some(l) => {
                l.used = Instant::now();
                is_computer(p) && l.error.is_some()
            }
            None => true,
        };
        if need {
            let l = self.load(p);
            self.cache.insert(p.to_path_buf(), l);
        }
        if self.cache.get(p).is_some_and(|l| l.generation == 0)
            && !self.folder_pending.contains_key(p)
        {
            self.request_folder(p, true);
        }
    }

    pub fn reload(&mut self, p: &Path) {
        if is_computer(p) {
            let l = self.load(p);
            self.cache.insert(p.to_path_buf(), l);
        } else {
            self.request_folder(p, true);
        }
    }

    pub fn reload_all(&mut self) {
        let visible = self.visible_folders();
        self.cache.retain(|p, _| visible.contains(p));
        let keys: Vec<PathBuf> = self.cache.keys().cloned().collect();
        for k in keys {
            self.reload(&k);
        }
    }

    fn publish_watch_list(&self) {
        let list = self.visible_folders().into_iter().filter(|p| !is_computer(p)).map(|path| FolderRequest {
            sorting: self.sorting_for(&path),
            old_mtime: self.cache.get(&path).and_then(|l| l.mtime),
            force: false,
            poll: true,
            path,
        }).collect();
        if let Ok(mut watch) = self.watch.lock() {
            *watch = list;
        }
    }

    fn visible_folders(&self) -> HashSet<PathBuf> {
        let mut visible = HashSet::new();
        for pane in &self.panes {
            visible.insert(pane.path().to_path_buf());
            if self.st.tree_files {
                visible.extend(pane.tree_entries.iter()
                    .filter(|e| e.is_dir && pane.expanded.contains(&e.path))
                    .map(|e| e.path.clone()));
            }
            if self.st.show_columns || self.st.layout == Layout::Columns {
                visible.extend(self.column_chain(pane.path()));
            }
        }
        visible
    }

    /// Bound inactive listings by age, folder count and entry count. Visible folders
    /// stay pinned, including when a single folder exceeds the cache budget.
    fn evict(&mut self) {
        let visible = self.visible_folders();
        self.cache
            .retain(|p, l| visible.contains(p) || l.used.elapsed() < Duration::from_secs(30));
        let mut entries: usize = self.cache.values().map(|l| l.entries.len()).sum();
        if self.cache.len() > 24 || entries > 50_000 {
            let mut inactive: Vec<_> = self
                .cache
                .iter()
                .filter(|(p, _)| !visible.contains(*p))
                .map(|(p, l)| (p.clone(), l.used))
                .collect();
            inactive.sort_by_key(|(_, used)| *used);
            for (p, _) in inactive {
                if self.cache.len() <= 24 && entries <= 50_000 {
                    break;
                }
                if let Some(l) = self.cache.remove(&p) {
                    entries -= l.entries.len();
                }
            }
        }
        self.col_hl
            .retain(|(pane, p), _| *pane < self.panes.len() && visible.contains(p));
    }

    /// Flatten only expanded branches. Directory reads use the existing bounded worker.
    fn build_tree(&mut self, i: usize, root: &Path) {
        let pane = &self.panes[i];
        if pane.tree_key.as_ref().is_some_and(|(path, generation, hidden, expanded)| {
            path == root && *generation == self.generation
                && *hidden == self.st.show_hidden && expanded == &pane.expanded
        }) {
            return;
        }
        let mut entries = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        self.append_tree(i, root, 0, &mut entries, &mut folders);
        for folder in folders {
            self.ensure(&folder);
        }
        let pane = &mut self.panes[i];
        pane.tree_entries = entries;
        pane.tree_key = Some((root.to_path_buf(), self.generation,
            self.st.show_hidden, pane.expanded.clone()));
        pane.view_key = None;
    }

    fn append_tree(&self, i: usize, folder: &Path, depth: usize,
        out: &mut Vec<Entry>, folders: &mut Vec<PathBuf>) {
        let Some(listing) = self.cache.get(folder) else { return };
        // Borrow cached listings; clone each displayed entry only once.
        for e in &listing.entries {
            if !self.st.show_hidden && e.hidden { continue; }
            out.push(e.clone());
            if e.is_dir && self.panes[i].expanded.contains(&e.path) && depth < 64 {
                folders.push(e.path.clone());
                self.append_tree(i, &e.path, depth + 1, out, folders);
            }
        }
    }

    pub fn toggle_tree_folder(&mut self, i: usize, path: PathBuf) {
        if !self.panes[i].expanded.remove(&path) {
            self.panes[i].expanded.insert(path.clone());
            self.ensure(&path);
        }
        self.panes[i].view_key = None;
    }

    pub fn update_view(&mut self, i: usize) {
        let Some(l) = self.cache.get(self.panes[i].path()) else { return };
        if l.generation == 0 {
            // Keep a requested selection until the real listing arrives.
            self.panes[i].view.clear();
            return;
        }
        let unchanged = self.panes[i].view_key.as_ref().is_some_and(|(path, generation, filter, hidden)| {
            path == self.panes[i].path() && *generation == l.generation
                && *filter == self.panes[i].filter && *hidden == self.st.show_hidden
        });
        if unchanged {
            return;
        }
        let key = (
            self.panes[i].path().to_path_buf(),
            l.generation,
            self.panes[i].filter.clone(),
            self.st.show_hidden,
        );
        let pane = &mut self.panes[i];
        let entries = if self.st.tree_files { &pane.tree_entries } else { &l.entries };
        let f = pane.filter.to_lowercase();
        let show_hidden = self.st.show_hidden;
        // A matching folder is a search result whose expanded contents must remain
        // browsable, even when the children's names do not match the filter.
        let matching_folders: HashSet<&Path> = if self.st.tree_files && !f.is_empty() {
            entries.iter()
                .filter(|e| e.is_dir && e.lname().contains(&f) && pane.expanded.contains(&e.path))
                .map(|e| e.path.as_path())
                .collect()
        } else {
            HashSet::new()
        };
        let view: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| (show_hidden || !e.hidden) && (f.is_empty() || e.lname().contains(&f)
                || e.path.ancestors().skip(1).any(|p| matching_folders.contains(p))))
            .map(|(k, _)| k)
            .collect();
        // Keep the cursor on the same item; after a navigation, put it on the item asked for.
        let pending = pane.pending_select.take();
        let selecting = pending.is_some();
        let target = pending.or_else(|| pane.cursor_path.clone());
        let found = target
            .as_ref()
            .and_then(|w| view.iter().position(|&k| &entries[k].path == w));
        match found {
            Some(row) => {
                pane.cursor = Some(row);
                pane.cursor_path = target.clone();
                if selecting {
                    pane.anchor = Some(row);
                    pane.scroll_to = Some(row);
                    if let Some(t) = target {
                        pane.selected.insert(t);
                    }
                }
            }
            None => {
                // Asked for, but the fresh listing has yet to show it.
                if selecting && pane.arriving.contains(target.as_ref().unwrap()) { pane.pending_select = target; }
                // The item went away (deleted, filtered): stay at the same row.
                pane.cursor = pane
                    .cursor
                    .filter(|_| !view.is_empty())
                    .map(|c| c.min(view.len() - 1));
                pane.cursor_path = pane.cursor.map(|c| entries[view[c]].path.clone());
            }
        }
        let alive: HashSet<&PathBuf> = view.iter().map(|&k| &entries[k].path).collect();
        pane.arriving.retain(|s| !alive.contains(s));
        pane.selected.retain(|s| alive.contains(s) || pane.arriving.contains(s));
        pane.view = view;
        pane.view_key = Some(key);
    }

    pub fn pane_entries(&self, i: usize) -> Option<&[Entry]> {
        let pane = &self.panes[i];
        let l = self.cache.get(pane.path())?;
        Some(if self.st.tree_files { &pane.tree_entries } else { &l.entries })
    }

    pub fn entry_at(&self, i: usize, row: usize) -> Option<&Entry> {
        let entries = self.pane_entries(i)?;
        self.panes[i].view.get(row).and_then(|&k| entries.get(k))
    }

    pub fn cursor_entry(&self, i: usize) -> Option<&Entry> {
        self.panes[i].cursor.and_then(|r| self.entry_at(i, r))
    }

    /// Selected paths, or the cursor item when nothing is selected.
    pub fn selection(&self, i: usize) -> Vec<PathBuf> {
        let pane = &self.panes[i];
        let Some(entries) = self.pane_entries(i) else {
            return Vec::new();
        };
        let mut v: Vec<PathBuf> = pane
            .view
            .iter()
            .map(|&k| &entries[k].path)
            .filter(|p| pane.selected.contains(*p))
            .cloned()
            .collect();
        if v.is_empty()
            && let Some(e) = self.cursor_entry(i) {
                v.push(e.path.clone());
            }
        v
    }

    pub fn set_cursor(&mut self, i: usize, row: usize, mods: egui::Modifiers) {
        let path = match self.entry_at(i, row) {
            Some(e) => e.path.clone(),
            None => return,
        };
        let rows: Vec<PathBuf> = if mods.shift {
            let a = self.panes[i].anchor.unwrap_or(row);
            let (lo, hi) = if a <= row { (a, row) } else { (row, a) };
            (lo..=hi)
                .filter_map(|r| self.entry_at(i, r).map(|e| e.path.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let toggle = mods.command || mods.ctrl;
        let pane = &mut self.panes[i];
        if mods.shift {
            if !toggle {
                pane.selected.clear();
            }
            pane.selected.extend(rows);
        } else if toggle {
            if !pane.selected.remove(&path) {
                pane.selected.insert(path.clone());
            }
            pane.anchor = Some(row);
        } else {
            pane.selected.clear();
            pane.selected.insert(path.clone());
            pane.anchor = Some(row);
        }
        pane.cursor = Some(row);
        pane.cursor_path = Some(path);
        pane.scroll_to = Some(row);
    }

    // ---------------------------------------------------------------- navigation

    pub fn navigate(&mut self, i: usize, to: PathBuf, select: Option<PathBuf>) {
        let from = self.panes[i].path().to_path_buf();
        if from == to {
            if select.is_some() {
                self.panes[i].pending_select = select;
                self.panes[i].view_key = None;
            }
            return;
        }
        let unavailable = !is_computer(&to)
            && matches!(fsx::probe_paths(std::slice::from_ref(&to), PATH_PROBE_LIMIT)[0],
                Some(fsx::PathKind::File | fsx::PathKind::Missing));
        if unavailable {
            self.toast(
                ToastKind::Error,
                format!("Folder {} not found", to.display()),
            );
            return;
        }
        let pane = &mut self.panes[i];
        let tab = pane.tab();
        tab.back.push(from.clone());
        tab.fwd.clear();
        tab.path = to.clone();
        pane.clear_view_state();
        // Going up selects the folder we came from.
        pane.pending_select = select.or_else(|| {
            if from.parent() == Some(&to) || (is_computer(&to) && from.parent().is_none()) {
                Some(from)
            } else {
                None
            }
        });
        self.push_history(&to);
        self.mark_dirty();
    }

    fn push_history(&mut self, p: &Path) {
        if is_computer(p) {
            return;
        }
        self.st.history.retain(|h| h != p);
        self.st.history.insert(0, p.to_path_buf());
        self.st.history.truncate(HISTORY_MAX);
    }

    pub fn go_back(&mut self, i: usize) {
        let pane = &mut self.panes[i];
        let tab = pane.tab();
        if let Some(prev) = tab.back.pop() {
            let cur = std::mem::replace(&mut tab.path, prev);
            tab.fwd.push(cur.clone());
            pane.clear_view_state();
            pane.pending_select = Some(cur);
        }
    }

    pub fn go_forward(&mut self, i: usize) {
        let pane = &mut self.panes[i];
        let tab = pane.tab();
        if let Some(next) = tab.fwd.pop() {
            let cur = std::mem::replace(&mut tab.path, next);
            tab.back.push(cur);
            pane.clear_view_state();
        }
    }

    pub fn go_up(&mut self, i: usize) {
        let p = self.panes[i].path().to_path_buf();
        if is_computer(&p) {
            return;
        }
        match p.parent() {
            Some(par) => self.navigate(i, par.to_path_buf(), Some(p.clone())),
            None => self.navigate(i, computer(), Some(p.clone())),
        }
    }

    /// What the listing already knows about a path, so opening needs no disk access.
    fn listed_is_dir(&self, p: &Path) -> Option<bool> {
        let listing = self.cache.get(p.parent()?)?;
        listing.entries.iter().find(|e| e.path == p).map(|e| e.is_dir)
    }

    pub fn open(&mut self, i: usize, p: PathBuf) {
        let is_dir = is_computer(&p) || self.listed_is_dir(&p).unwrap_or_else(|| {
            let kind = fsx::probe_paths(std::slice::from_ref(&p), PATH_PROBE_LIMIT)[0];
            !matches!(kind, Some(fsx::PathKind::File | fsx::PathKind::Missing))
        });
        if is_dir {
            self.navigate(i, p, None);
        } else if crate::archive::is_archive(&p) {
            // As in Nautilus: opening an archive unpacks it beside itself. "Open with" still
            // reaches an archive manager.
            self.acts.push(Act::Extract(vec![p], None));
            if let Some(ctx) = self.wake.get() { ctx.request_repaint(); }
        } else if let Some(place) = run_warning(&p) {
            // A click behind the open window must not stack a second question.
            if !self.notices.iter().any(|n| matches!(n, Notice::ConfirmRun { path, .. } if *path == p)) {
                let link = link_target(&p);
                self.notices.push(Notice::ConfirmRun { path: p, place, link });
            }
        } else {
            self.launch(&p);
        }
    }

    /// Hand a file to the system, which runs it if it is a program.
    pub fn launch(&mut self, p: &Path) {
        match fsx::open_with_system(p) {
            Ok(_) => self.toast(ToastKind::Info, format!("Opened {}", fsx::display_name(p))),
            Err(e) => self.toast(
                ToastKind::Error,
                format!("Couldn't open {}: {e}", fsx::display_name(p)),
            ),
        }
    }

    pub fn new_tab(&mut self, i: usize, path: Option<PathBuf>, switch: bool) {
        // config NewTabClonesCurrent = True, CreateTabAfterCurrent = True
        let pane = &mut self.panes[i];
        let p = path.unwrap_or_else(|| pane.path().to_path_buf());
        let at = pane.active + 1;
        pane.tabs.insert(at, Tab::new(p));
        if switch {
            pane.active = at;
            pane.clear_view_state();
        }
        self.mark_dirty();
    }

    pub fn close_tab(&mut self, i: usize, t: usize) {
        if i >= self.panes.len() || t >= self.panes[i].tabs.len() {
            return;
        }
        if self.panes[i].tabs.len() == 1 {
            self.close_panel(i);
            return;
        }
        let pane = &mut self.panes[i];
        let tab = pane.tabs.remove(t);
        pane.closed.push(tab.path);
        if pane.active > t || pane.active >= pane.tabs.len() {
            pane.active = pane.active.saturating_sub(1);
        }
        pane.clear_view_state();
        self.mark_dirty();
    }

    /// Transfer the actual tab, including its navigation history, between tiles.
    pub fn dock_tab(&mut self, source: usize, tab: usize, mut target: usize, at: usize, split: Option<(bool, bool)>) {
        if source >= self.panes.len() || target >= self.panes.len() || tab >= self.panes[source].tabs.len() {
            return;
        }
        if let Some((vertical, before)) = split {
            if self.panes[source].tabs.len() == 1 {
                if self.tiles.relocate(source, target, vertical, before) {
                    self.set_active(source);
                    self.mark_dirty();
                }
                return;
            }
        } else if source == target {
            let pane = &mut self.panes[source];
            let moved = pane.tabs.remove(tab);
            let at = at.saturating_sub(usize::from(at > tab)).min(pane.tabs.len());
            pane.tabs.insert(at, moved);
            pane.active = at;
            pane.clear_view_state();
            self.set_active(source);
            self.mark_dirty();
            return;
        }
        let moved = self.panes[source].tabs.remove(tab);
        if let Some((vertical, before)) = split {
            let new = self.panes.len();
            self.tiles.insert_at(target, new, vertical, before);
            self.panes.push(Pane { tabs: vec![moved], ..Default::default() });
            target = new;
        } else {
            let pane = &mut self.panes[target];
            let at = at.min(pane.tabs.len());
            pane.tabs.insert(at, moved);
            pane.active = at;
            pane.clear_view_state();
        }
        if self.panes[source].tabs.is_empty() {
            self.close_panel(source);
            if target > source { target -= 1; }
        } else {
            let pane = &mut self.panes[source];
            if pane.active > tab || pane.active >= pane.tabs.len() {
                pane.active = pane.active.saturating_sub(1);
            }
            pane.clear_view_state();
        }
        self.set_active(target);
        self.mark_dirty();
    }

    /// Move a whole panel beside another, or merge all its tabs and their histories into it.
    pub fn dock_panel(&mut self, source: usize, target: usize, at: usize, split: Option<(bool, bool)>) {
        if source == target || source >= self.panes.len() || target >= self.panes.len() {
            return;
        }
        if let Some((vertical, before)) = split {
            if self.tiles.relocate(source, target, vertical, before) {
                self.set_active(source);
                self.mark_dirty();
            }
            return;
        }
        let tabs = std::mem::take(&mut self.panes[source].tabs);
        let closed = std::mem::take(&mut self.panes[source].closed);
        let active = self.panes[source].active;
        let pane = &mut self.panes[target];
        let at = at.min(pane.tabs.len());
        pane.tabs.splice(at..at, tabs);
        pane.closed.extend(closed);
        pane.active = at + active;
        pane.clear_view_state();
        self.close_panel(source);
        self.set_active(target - usize::from(target > source));
        self.mark_dirty();
    }

    // ---------------------------------------------------------------- jobs

    pub fn start_job(
        &mut self,
        kind: OpKind,
        sources: Vec<PathBuf>,
        dest: Option<PathBuf>,
        ctx: &egui::Context,
    ) {
        if sources.is_empty() {
            return;
        }
        let sources: Vec<PathBuf> = sources
            .into_iter()
            .filter(|s| !is_computer(s) && s.parent().is_some())
            .collect();
        if sources.is_empty() {
            return;
        }
        if let Some(d) = &dest
            && kind == OpKind::Move && sources.iter().all(|s| s.parent() == Some(d.as_path())) {
                self.toast(
                    ToastKind::Info,
                    "Drop operation cancelled - destination same as source",
                );
                return;
            }
        let n = sources.len();
        let what = match &dest {
            Some(d) => format!("{} {n} item(s) to {}", kind.verb(), fsx::display_name(d)),
            None => format!("{} {n} item(s)", kind.verb()),
        };
        self.toast(ToastKind::Info, what);
        self.jobs.push(ops::start(kind, sources, dest, ctx.clone()));
    }

    fn finish_jobs(&mut self) {
        let mut finished = Vec::new();
        self.jobs.retain(|j| {
            let p = j.progress.lock().unwrap();
            if p.finished {
                finished.push((
                    j.kind,
                    j.dest.clone(),
                    p.done_items,
                    p.errors.clone(),
                    p.created.clone(),
                    p.unrecoverable.clone(),
                    p.locked.clone(),
                ));
                false
            } else {
                true
            }
        });
        for (kind, dest, n, errors, created, unrecoverable, locked) in finished {
            self.reload_all();
            for i in 0..self.panes.len() {
                self.panes[i].view_key = None;
            }
            let processed = n.saturating_sub(unrecoverable.len() + locked.len());
            self.locked.extend(locked.into_iter().map(|(archive, wrong)| (archive, dest.clone(), wrong)));
            if errors.iter().all(|e| e.ends_with("Cancelled")) && !errors.is_empty() {
                self.toast(ToastKind::Info, "Cancelled");
            } else if !errors.is_empty() {
                let title = format!("{} finished with problems", kind.verb());
                self.notices.push(Notice::Problems { title, lines: errors });
            } else if processed > 0 {
                let word = match kind {
                    OpKind::Trash if cfg!(windows) => "Moved to Recycle Bin",
                    OpKind::Trash => "Moved to Trash",
                    OpKind::Destroy => "Permanently deleted",
                    OpKind::Extract => "Extracted",
                    OpKind::Compress => "Compressed",
                    _ => "Finished processing",
                };
                self.toast(ToastKind::Success, format!("{word} {processed} item(s)"));
            }
            if !unrecoverable.is_empty() {
                self.notices.push(Notice::ConfirmDestroy { items: unrecoverable });
            }
            // Select what arrived in the destination pane.
            if let Some(d) = dest {
                for i in 0..self.panes.len() {
                    if self.panes[i].path() == d {
                        self.panes[i].arriving = created.iter().cloned().collect();
                        self.panes[i].selected = created.iter().cloned().collect();
                        if let Some(first) = created.first() {
                            self.panes[i].pending_select = Some(first.clone());
                        }
                    }
                }
            }
        }
    }

    pub fn compress(&mut self, pane: usize, sources: Vec<PathBuf>, name: String, kind: crate::compress::Kind, ctx: &egui::Context) {
        let dest = self.panes[pane].path().to_path_buf();
        self.toast(ToastKind::Info, format!("Compressing {} item(s) to {name}", sources.len()));
        self.jobs.push(ops::start_compress(sources, dest, name, kind, ctx.clone()));
    }

    fn ask_for_password(&mut self) {
        if self.dialog.is_none()
            && let Some((archive, dest, wrong)) = self.locked.pop_front() {
            self.dialog = Some(Dialog::Password { archive, dest, wrong, name: String::new(), focus: true });
        }
    }

    /// The password lives only in this job: it is never saved or shown.
    pub fn extract_with_password(&mut self, archive: PathBuf, dest: Option<PathBuf>, password: String, ctx: &egui::Context) {
        self.jobs.push(ops::start_with_password(OpKind::Extract, vec![archive], dest, Some(password), ctx.clone()));
    }

    // ---------------------------------------------------------------- actions

    fn run(&mut self, a: Act, ctx: &egui::Context) {
        match a {
            Act::Navigate(i, p) => self.navigate(i, p, None),
            Act::NavigateSelect(i, p, s) => self.navigate(i, p, Some(s)),
            Act::Open(i, p) => self.open(i, p),
            #[cfg(target_os = "linux")]
            Act::Properties(p) => self.properties = Some(crate::linux_properties::Properties::new(p, ctx.clone())),
            Act::OpenNewTab(i, p, switch) => self.new_tab(i, Some(p), switch),
            Act::OpenOtherPane(i, p) => {
                if self.panes.len() < 2 {
                    self.open_panel(i, Some(p));
                } else {
                    let o = self.other(i);
                    self.new_tab(o, Some(p), true);
                    self.set_active(o);
                }
            }
            Act::OpenPanel(i, p) => {
                self.open_panel(i, p);
            }
            Act::ClosePanel(i) => self.close_panel(i),
            Act::ToggleSplit(i) => {
                self.tiles.toggle_split(i);
                self.mark_dirty();
            }
            Act::OpenWith(p, app) => {
                #[cfg(not(windows))]
                if app.is_none() {
                    if !p.is_file() {
                        self.toast(ToastKind::Error, "The selected file no longer exists");
                    } else {
                        self.app_chooser = Some(crate::openwith::Chooser::new(p, ctx.clone()));
                    }
                    return;
                }
                let (tx, rx) = mpsc::channel();
                self.open_with_pending.push((p.clone(), rx));
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(crate::openwith::launch(&p, app.as_ref()));
                    ctx.request_repaint();
                });
            }
            Act::Compress(i, paths) => {
                let folder = self.panes[i].path().to_path_buf();
                let sources = crate::compress::outermost(
                    paths.into_iter().filter(|p| !is_computer(p) && p.parent().is_some()).collect(),
                );
                if !is_computer(&folder) && !sources.is_empty() {
                    let name = crate::compress::suggested_name(&sources, &folder);
                    self.dialog = Some(Dialog::Compress { pane: i, sources, name, kind: crate::compress::Kind::Zip, focus: true });
                }
            }
            Act::Extract(archives, dest) => {
                // Search results can sit in different folders, and each job has one target.
                let mut groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
                for archive in archives {
                    let Some(dir) = dest.clone().or_else(|| archive.parent().map(Path::to_path_buf)) else { continue };
                    match groups.iter_mut().find(|(d, _)| *d == dir) {
                        Some((_, group)) => group.push(archive),
                        None => groups.push((dir, vec![archive])),
                    }
                }
                for (dir, group) in groups {
                    self.start_job(OpKind::Extract, group, Some(dir), ctx);
                }
            }
            Act::NewTab(i) => self.new_tab(i, None, true),
            Act::CloseTab(i, t) => {
                if self.panes.len() == 1 && i == 0 && t == 0 && self.panes[0].tabs.len() == 1 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.close_tab(i, t);
                }
            },
            Act::DockTab { source, tab, target, at, split } => self.dock_tab(source, tab, target, at, split),
            Act::DockPanel { source, target, at, split } => self.dock_panel(source, target, at, split),
            Act::SwitchTab(i, t) => {
                if t < self.panes[i].tabs.len() && self.panes[i].active != t {
                    self.panes[i].active = t;
                    self.panes[i].clear_view_state();
                    self.mark_dirty();
                }
            }
            Act::ReopenTab(i) => {
                if let Some(p) = self.panes[i].closed.pop() {
                    self.new_tab(i, Some(p), true);
                }
            }
            Act::Back(i) => self.go_back(i),
            Act::Forward(i) => self.go_forward(i),
            Act::Up(i) => self.go_up(i),
            Act::Copy(i) | Act::Cut(i) => {
                let cut = matches!(a, Act::Cut(_));
                let sel = self.selection(i);
                if sel.is_empty() {
                    return;
                }
                #[cfg(windows)]
                if let Err(e) = crate::file_transfer::write(&sel, cut) {
                    self.toast(ToastKind::Error, e);
                    return;
                }
                #[cfg(not(windows))]
                ctx.copy_text(
                    sel.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
                self.toast(
                    ToastKind::Info,
                    if cut {
                        "Selection cut to Clipboard"
                    } else {
                        "Selection copied to Clipboard"
                    },
                );
                self.clip = Some((sel.into(), cut));
            }
            Act::Paste(i, into) => {
                let dest = into.unwrap_or_else(|| self.panes[i].path().to_path_buf());
                if is_computer(&dest) {
                    return;
                }
                #[cfg(windows)]
                let clipboard = match crate::file_transfer::read() {
                    Ok(value) => value.map(|(paths, cut)| (std::sync::Arc::<[PathBuf]>::from(paths), cut)),
                    Err(e) => { self.toast(ToastKind::Error, e); return; }
                };
                #[cfg(not(windows))]
                let clipboard = self.clip.take();
                match clipboard {
                    Some((paths, cut)) => {
                        let kind = if cut { OpKind::Move } else { OpKind::Copy };
                        if !cut {
                            self.clip = Some((paths.clone(), false));
                        }
                        self.start_job(kind, paths.to_vec(), Some(dest), ctx);
                    }
                    None => self.toast(ToastKind::Info, "Clipboard doesn't contain files"),
                }
            }
            #[cfg(target_os = "linux")]
            Act::LinkToOther(i, hard) => {
                let o = self.other(i);
                if o == i { self.toast(ToastKind::Info, "Open a second panel first"); return; }
                let dest = self.panes[o].path().to_path_buf();
                self.start_job(if hard { OpKind::Hardlink } else { OpKind::Symlink }, self.selection(i), Some(dest), ctx);
            }
            Act::ToOther(i, mv) => {
                let o = self.other(i);
                if o == i {
                    self.toast(
                        ToastKind::Info,
                        "Open a second panel first (Ctrl+Shift+Enter)",
                    );
                    return;
                }
                let dest = self.panes[o].path().to_path_buf();
                let sel = self.selection(i);
                self.start_job(
                    if mv { OpKind::Move } else { OpKind::Copy },
                    sel,
                    Some(dest),
                    ctx,
                );
            }
            Act::Delete(i) => {
                let sel = self.selection(i);
                // Move the cursor to the neighbour so the list stays put.
                let pane = &mut self.panes[i];
                if let Some(c) = pane.cursor {
                    pane.cursor_path = None;
                    pane.cursor = Some(c);
                }
                self.start_job(OpKind::Trash, sel, None, ctx);
            }
            Act::DeleteForever(i) => {
                let items: Vec<(PathBuf, String)> = self
                    .selection(i)
                    .into_iter()
                    .filter(|p| !is_computer(p) && p.parent().is_some())
                    .map(|p| (p, String::new()))
                    .collect();
                if !items.is_empty() {
                    self.notices.push(Notice::ConfirmDestroy { items });
                }
            }
            Act::Rename(i) => {
                // the cursor item when it is selected, else the first selected item
                let sel = self.selection(i);
                let pick = self
                    .cursor_entry(i)
                    .filter(|e| sel.is_empty() || sel.contains(&e.path))
                    .cloned()
                    .or_else(|| {
                        self.cache
                            .get(self.panes[i].path())?
                            .entries
                            .iter()
                            .find(|e| sel.first() == Some(&e.path))
                            .cloned()
                    });
                if let Some(e) = pick.as_ref() {
                    let (p, n) = (e.path.clone(), e.name().to_string());
                    if is_computer(self.panes[i].path()) {
                        return;
                    }
                    self.panes[i].rename = Some((p, n));
                    self.panes[i].rename_focus = true;
                }
            }
            Act::CommitRename(i) => {
                let Some((old, new)) = self.panes[i].rename.take() else {
                    return;
                };
                let old_name = fsx::display_name(&old);
                let new = new.to_string();
                if new == old_name {
                    self.toast(ToastKind::Info, "Same name. Nothing changed");
                    return;
                }
                if !fsx::valid_name(&new) {
                    self.toast(ToastKind::Error, format!("Invalid name {new}"));
                    return;
                }
                let target = old.with_file_name(&new);
                #[cfg(windows)]
                let same = new.to_lowercase() == old_name.to_lowercase();
                #[cfg(unix)]
                let same = ops::same_entry(&old, &target);
                if std::fs::symlink_metadata(&target).is_ok() && !same {
                    self.toast(ToastKind::Error, "File with this name already exists");
                    return;
                }
                match ops::rename_no_replace(&old, &target) {
                    Ok(_) => {
                        let (oe, ne) = (
                            fsx::split_ext(&old_name, old.is_dir()),
                            fsx::split_ext(&new, target.is_dir()),
                        );
                        if !target.is_dir()
                            && oe.to_lowercase() != ne.to_lowercase()
                            && !oe.is_empty()
                        {
                            self.toast(
                                ToastKind::Info,
                                format!("Item renamed. The file extension changed from .{oe}"),
                            );
                        } else {
                            self.toast(ToastKind::Success, "Item renamed");
                        }
                        let dir = self.panes[i].path().to_path_buf();
                        self.panes[i].pending_select = Some(target.clone());
                        self.panes[i].selected = [target].into_iter().collect();
                        self.reload(&dir);
                    }
                    Err(e) => self.toast(
                        ToastKind::Error,
                        format!("Rename failed. {}", fsx::friendly_err(&e)),
                    ),
                }
            }
            Act::NewFolder(i) => {
                if !is_computer(self.panes[i].path()) {
                    self.dialog = Some(Dialog::NewFolder {
                        pane: i,
                        name: "New folder".into(),
                        focus: true,
                    });
                }
            }
            Act::NewFile(i) => {
                if !is_computer(self.panes[i].path()) {
                    self.dialog = Some(Dialog::NewFile {
                        pane: i,
                        name: "New text document.txt".into(),
                        focus: true,
                    });
                }
            }
            Act::Duplicate(i) => {
                let dir = self.panes[i].path().to_path_buf();
                let sel = self.selection(i);
                self.start_job(OpKind::Copy, sel, Some(dir), ctx);
            }
            Act::CopyPath(i) => {
                let sel = self.selection(i);
                let text = if sel.is_empty() {
                    self.panes[i].path().display().to_string()
                } else {
                    sel.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                ctx.copy_text(text);
                self.toast(ToastKind::Info, "Path(s) copied to Clipboard");
            }
            Act::Drop {
                target,
                paths,
                copy,
            } => {
                // config DragAlwaysMoves = True: drag moves, Ctrl+drag copies.
                if paths.iter().any(|p| p == &target) {
                    return;
                }
                self.start_job(
                    if copy { OpKind::Copy } else { OpKind::Move },
                    paths,
                    Some(target),
                    ctx,
                );
            }
            Act::TagPaths(paths, tag) => {
                for p in paths {
                    let k = p.to_string_lossy().to_string();
                    match tag {
                        Some(t) => {
                            self.st.tags.insert(k, t);
                        }
                        None => {
                            self.st.tags.remove(&k);
                        }
                    }
                }
                self.mark_dirty();
            }
            Act::AddFavorite(g, p) => {
                if let Some(grp) = self.st.favorites.get_mut(g)
                    && !grp.paths.contains(&p) && p.is_absolute() {
                        grp.paths.push(p);
                    }
                self.mark_dirty();
            }
            Act::RemoveFavorite(g, k) => {
                if let Some(grp) = self.st.favorites.get_mut(g) {
                    grp.remove_at(k);
                }
                self.mark_dirty();
            }
            Act::MoveFavorite { from, index, to, at } => {
                if self.st.move_favorite(from, index, to, at) {
                    self.mark_dirty();
                } else if from != to {
                    self.toast(ToastKind::Info, "This group already has that shortcut");
                }
            }
            Act::PlaceFavorites { group, at, paths } => {
                if self.st.place_favorites(group, at, paths) {
                    self.mark_dirty();
                }
            }
            Act::DeleteGroup(g) => {
                if g < self.st.favorites.len() {
                    self.st.sidebar_order.retain(|s| *s != crate::settings::SidebarSection::Favorite(g));
                    for section in &mut self.st.sidebar_order {
                        if let crate::settings::SidebarSection::Favorite(i) = section
                            && *i > g { *i -= 1; }
                    }
                    let removed = self.st.favorites.remove(g);
                    self.st.removed_groups.push(removed);
                }
                self.mark_dirty();
            }
            Act::MoveGroup(g, d) => {
                self.st.normalize_sidebar();
                let section = crate::settings::SidebarSection::Favorite(g);
                if let Some(i) = self.st.sidebar_order.iter().position(|s| *s == section) {
                    let t = i as i32 + d;
                    if t >= 0 && (t as usize) < self.st.sidebar_order.len() {
                        self.st.sidebar_order.swap(i, t as usize);
                    }
                }
                self.mark_dirty();
            }
            Act::Refresh => {
                self.reload_all();
                self.refresh_drives();
                self.toast(ToastKind::Info, "Refreshed");
            }
            Act::Terminal(dir) => {
                if is_computer(&dir) {
                    return;
                }
                match open_terminal(&dir) {
                    Ok(_) => self.toast(ToastKind::Info, "Program launched"),
                    Err(e) => self.toast(
                        ToastKind::Error,
                        format!("Could not open command shell here: {e}"),
                    ),
                }
            }
            Act::SetSort(i, by) => {
                let p = self.panes[i].path().to_path_buf();
                let mut s = self.sorting_for(&p);
                if s.by == by {
                    s.ascending = !s.ascending;
                } else {
                    s.by = by;
                    // dates and sizes read best newest/largest first
                    s.ascending = matches!(by, fsx::SortBy::Name | fsx::SortBy::Type);
                }
                self.st
                    .folder_sorting
                    .insert(p.to_string_lossy().to_string(), s);
                self.reload(&p);
                self.mark_dirty();
            }
            Act::ToggleFoldersOnTop(i) => {
                let p = self.panes[i].path().to_path_buf();
                let mut s = self.sorting_for(&p);
                s.folders_on_top = !s.folders_on_top;
                self.st
                    .folder_sorting
                    .insert(p.to_string_lossy().to_string(), s);
                self.reload(&p);
                self.mark_dirty();
            }
            Act::SelectAll(i) => {
                self.panes[i].selected = (0..self.panes[i].view.len())
                    .filter_map(|row| self.entry_at(i, row).map(|e| e.path.clone()))
                    .collect();
            }
            Act::Search(i) => {
                let root = self.panes[i].path().to_path_buf();
                let root = if is_computer(&root) {
                    dirs::home_dir().unwrap_or_default()
                } else {
                    root
                };
                self.search = Some(SearchUi {
                    query: String::new(),
                    root,
                    run: None,
                    focus: true,
                });
            }
        }
    }

    pub fn create_item(&mut self, pane: usize, name: &str, folder: bool, open_after: bool) {
        let dir = self.panes[pane].path().to_path_buf();
        if !fsx::valid_name(name) {
            self.toast(ToastKind::Error, format!("Invalid name {name}"));
            return;
        }
        let target = dir.join(name);
        if std::fs::symlink_metadata(&target).is_ok() {
            self.toast(
                ToastKind::Error,
                if folder {
                    "Folder with this name already exists"
                } else {
                    "File with this name already exists"
                },
            );
            return;
        }
        let r = if folder {
            std::fs::create_dir(&target)
        } else {
            std::fs::File::create_new(&target).map(|_| ())
        };
        match r {
            Ok(_) => {
                self.toast(
                    ToastKind::Success,
                    if folder {
                        "New folder created"
                    } else {
                        "New file created"
                    },
                );
                self.reload(&dir);
                if open_after {
                    self.open(pane, target);
                } else {
                    self.panes[pane].pending_select = Some(target.clone());
                    self.panes[pane].selected = [target].into_iter().collect();
                }
            }
            Err(e) => self.toast(
                ToastKind::Error,
                format!("Error creating item. {}", fsx::friendly_err(&e)),
            ),
        }
    }

    pub fn group_add_new(&mut self, name: String) {
        let p = self.panes[self.active].path().to_path_buf();
        let paths = if is_computer(&p) { Vec::new() } else { vec![p] };
        self.st.favorites.push(FavGroup::new(name, paths));
        self.mark_dirty();
    }

    pub fn folder_color(&self, path: &Path) -> Color32 {
        self.tag_color(path).unwrap_or_else(|| fsx::Kind::Folder.color())
    }

    pub fn tag_color(&self, p: &Path) -> Option<Color32> {
        if self.st.tags.is_empty() {
            return None;
        }
        self.st
            .tags
            .get(&p.to_string_lossy().to_string())
            .and_then(|&t| crate::theme::TAGS.get(t as usize))
            .map(|(_, c)| hex(c))
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if self.persist {
            self.save();
        }
    }
}

#[cfg(test)]
mod performance_tests {
    #[test]
    fn the_window_changes_with_the_desktop_theme_and_can_leave_it() {
        let dir = std::env::temp_dir().join(format!("fc-omarchy-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("colors.toml");
        let write = |background: &str, accent: &str| {
            let text = format!("background = \"{background}\"\nforeground = \"#ffffff\"\naccent = \"{accent}\"\n");
            std::fs::write(&file, text).unwrap();
        };
        write("#101010", "#ff0000");
        let ctx = egui::Context::default();
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        app.st.theme = ThemeName::Omarchy;
        app.omarchy_watch = Some(crate::omarchy::Watch::of(file.clone(), Duration::from_millis(10), ctx.clone()));
        app.begin_frame(&ctx);
        assert_eq!(app.pal.panel, hex("#101010"));
        assert_eq!(app.pal.accent, hex("#ff0000"));
        assert_eq!(ctx.global_style().visuals.window_fill, hex("#101010"));

        write("#202030", "#007700");
        let until = Instant::now() + Duration::from_secs(10);
        while app.pal.panel != hex("#202030") {
            assert!(Instant::now() < until, "the window stayed on the old theme");
            std::thread::sleep(Duration::from_millis(5));
            app.begin_frame(&ctx);
        }
        assert_eq!(app.pal.accent, hex("#007700"));

        app.st.theme = ThemeName::Dark;
        app.begin_frame(&ctx);
        assert_eq!(app.pal.panel, Palette::get(ThemeName::Dark, app.pal.accent).panel);
        assert_eq!(app.pal.accent, hex(&app.st.accent), "the saved accent comes back");
        assert!(app.omarchy_watch.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    use super::*;

    fn listing(used: Instant) -> Listing {
        Listing {
            entries: Vec::new(),
            error: None,
            mtime: None,
            generation: 0,
            sorting: Sorting::default(),
            used,
        }
    }

    #[test]
    fn tree_cache_reuses_rows_and_tracks_child_changes() {
        let root = std::env::temp_dir().join(format!("fc_tree_cache_{}", std::process::id()));
        let branch = root.join("branch");
        std::fs::create_dir_all(&branch).unwrap();
        std::fs::write(branch.join("first.txt"), "one").unwrap();
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        app.st.tree_files = true;
        app.panes[0].tab().path = root.clone();
        for path in [&root, &branch] {
            let mut l = listing(Instant::now());
            l.entries = fsx::read_dir(path).unwrap();
            l.generation = 1;
            app.cache.insert(path.clone(), l);
        }
        app.build_tree(0, &root);
        app.update_view(0);
        assert_eq!(app.panes[0].view.len(), 1);
        let rows = app.panes[0].tree_entries.as_ptr();
        let view = app.panes[0].view.as_ptr();
        for _ in 0..100 {
            app.build_tree(0, &root);
            app.update_view(0);
            assert_eq!(app.panes[0].tree_entries.as_ptr(), rows);
            assert_eq!(app.panes[0].view.as_ptr(), view);
        }
        app.toggle_tree_folder(0, branch.clone());
        app.build_tree(0, &root);
        app.update_view(0);
        assert_eq!(app.panes[0].view.len(), 2);
        // A child refresh must invalidate the view even if the root is unchanged.
        std::fs::write(branch.join("second.txt"), "two").unwrap();
        app.cache.get_mut(&branch).unwrap().entries = fsx::read_dir(&branch).unwrap();
        app.generation += 1;
        app.build_tree(0, &root);
        app.update_view(0);
        assert_eq!(app.panes[0].view.len(), 3);
        app.toggle_tree_folder(0, branch);
        app.build_tree(0, &root);
        app.update_view(0);
        assert_eq!(app.panes[0].view.len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_reads_are_queued_and_refreshes_coalesce() {
        let root = std::env::temp_dir().join(format!("fc_async_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("loaded.txt"), "data").unwrap();
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        app.panes[0].tab().path = root.clone();
        app.panes[0].pending_select = Some(root.join("loaded.txt"));
        app.ensure(&root);
        app.update_view(0);
        assert!(
            app.panes[0].pending_select.is_some(),
            "selection waits for the background result"
        );
        assert!(
            app.cache[&root].entries.is_empty(),
            "ensure must not read on the UI thread"
        );
        for _ in 0..100 {
            app.reload(&root);
        }
        assert_eq!(app.folder_pending.len(), 1);
        assert!(
            app.folder_pending[&root],
            "one follow-up preserves changes during a pending read"
        );
        let result = app.folder_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result.path, root);
        let mut listing = result.listing.unwrap();
        assert_eq!(listing.entries[0].name(), "loaded.txt");
        listing.generation = 1;
        app.cache.insert(root.clone(), listing);
        app.update_view(0);
        assert!(app.panes[0].selected.contains(&root.join("loaded.txt")));
        std::fs::remove_file(root.join("loaded.txt")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn watcher_reports_only_changes_and_needs_no_frame() {
        let root = std::env::temp_dir().join(format!("fc_watch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        for pane in &mut app.panes {
            pane.tabs.truncate(1);
            pane.active = 0;
            pane.tab().path = root.clone();
        }
        let mut known = listing(Instant::now());
        known.mtime = std::fs::metadata(&root).unwrap().modified().ok();
        app.cache.insert(root.clone(), known);
        app.publish_watch_list();
        let quiet = Duration::from_millis(REFRESH_MS * 2 + 100);
        assert!(app.folder_rx.recv_timeout(quiet).is_err(), "an unchanged folder stays silent");
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(root.join("new.txt"), "new").unwrap();
        let result = app.folder_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(result.polled);
        assert_eq!(result.listing.unwrap().entries.len(), 1);
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inactive_cache_is_bounded_and_expires_even_below_limit() {
        let mut app = App::new(Some(PathBuf::from("unused.png")));
        let active = app.panes[0].path().to_path_buf();
        let old = Instant::now() - Duration::from_secs(31);
        app.cache.insert(active.clone(), listing(old));
        let abandoned = PathBuf::from("abandoned");
        app.cache.insert(abandoned.clone(), listing(old));
        app.col_hl
            .insert((0, abandoned.clone()), PathBuf::from("child"));
        app.evict();
        assert!(app.cache.contains_key(&active));
        assert!(!app.cache.contains_key(&abandoned));
        assert!(app.col_hl.is_empty());
        for i in 0..100 {
            app.cache.insert(
                PathBuf::from(format!("inactive-{i}")),
                listing(Instant::now()),
            );
        }
        app.evict();
        assert_eq!(app.cache.len(), 24);
        assert!(app.cache.contains_key(&active));
    }
}

/// Asking only here keeps the question meaningful: one shown for every local program
/// would soon be dismissed without being read. "Open with" needs no question, because
/// choosing an application shows the file is meant to be read, not run.
fn run_warning(p: &Path) -> Option<&'static str> {
    let place = crate::drives::foreign_place(p)?;
    might_run(p).then_some(place)
}

/// The system goes by a link's target and by contents, so the name shown is not enough:
/// `notes.txt` can be a link to a program. A type that cannot be told counts as a program.
fn might_run(p: &Path) -> bool {
    let target = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if fsx::is_runnable(&fsx::display_name(p)) || fsx::is_runnable(&fsx::display_name(&target)) { return true; }
    #[cfg(not(windows))]
    return crate::openwith::file_type(p).is_none_or(|mime| fsx::is_runnable_type(&mime));
    #[cfg(windows)]
    false
}

/// The name a link leads to, for showing what would really start.
fn link_target(p: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(p).ok()?.file_type().is_symlink() { return None; }
    Some(fsx::display_name(&std::fs::canonicalize(p).ok()?))
}

fn open_terminal(dir: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // config CommandShell = CMD
        std::process::Command::new(crate::win::system_program("cmd.exe"))
            .arg("/K")
            .current_dir(dir)
            .creation_flags(0x0000_0010)
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut last = std::io::Error::other("no terminal found");
        for (terminal, flag) in terminal_choices(default_terminal_is_ghostty()) {
            match terminal_command(terminal, flag, dir).spawn() {
                Ok(_) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

#[cfg(not(windows))]
const GHOSTTY: (&str, &str) = ("ghostty", "--working-directory");

#[cfg(not(windows))]
fn default_terminal_is_ghostty() -> bool {
    std::process::Command::new("xdg-terminal-exec").arg("--print-id").stderr(std::process::Stdio::null()).output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim().starts_with("com.mitchellh.ghostty"))
}

/// Terminals to try, in order. `xdg-terminal-exec` starts Ghostty through its desktop entry,
/// which forces single-instance mode, so a Ghostty that is the default is started directly.
#[cfg(not(windows))]
fn terminal_choices(ghostty_first: bool) -> impl Iterator<Item = (&'static str, &'static str)> {
    ghostty_first.then_some(GHOSTTY).into_iter().chain([
        ("xdg-terminal-exec", "--dir"),
        ("x-terminal-emulator", ""),
        GHOSTTY,
        ("foot", "--working-directory"),
        ("gnome-terminal", "--working-directory"),
        ("konsole", "--workdir"),
        ("xfce4-terminal", "--working-directory"),
        ("kitty", "--directory"),
        ("alacritty", "--working-directory"),
        ("xterm", ""),
    ])
}

#[cfg(not(windows))]
fn terminal_command(terminal: &str, flag: &str, dir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(terminal);
    command.current_dir(dir);
    if terminal == GHOSTTY.0 {
        // A running single instance opens the window in the folder it was started from.
        command.arg("--gtk-single-instance=false");
    }
    if !flag.is_empty() {
        let mut argument = std::ffi::OsString::from(flag);
        argument.push("=");
        argument.push(dir.as_os_str());
        command.arg(argument);
    }
    command
}

#[cfg(all(test, target_os = "linux"))]
mod terminal_tests {
    use super::*;

    #[test]
    fn ghostty_as_the_default_is_started_directly_and_on_its_own() {
        assert_eq!(terminal_choices(true).next(), Some(GHOSTTY));
        assert_eq!(terminal_choices(false).next().map(|t| t.0), Some("xdg-terminal-exec"));
        let dir = Path::new("/some/folder with space");
        let command = terminal_command(GHOSTTY.0, GHOSTTY.1, dir);
        assert_eq!(command.get_current_dir(), Some(dir));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--gtk-single-instance=false", "--working-directory=/some/folder with space"]);
    }

    #[test]
    fn other_terminals_only_get_the_folder() {
        let args: Vec<_> = terminal_command("foot", "--working-directory", Path::new("/x")).get_args().map(|a| a.to_owned()).collect();
        assert_eq!(args, ["--working-directory=/x"]);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod preview_tests {
    use super::*;

    #[test]
    fn a_harmless_name_does_not_hide_a_program() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("fc_might_run_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (jar, text) = (root.join("payload.jar"), root.join("plain.txt"));
        std::fs::write(&jar, "stub").unwrap();
        std::fs::write(&text, "words").unwrap();
        symlink(&jar, root.join("notes.txt")).unwrap();
        symlink(&text, root.join("letter.txt")).unwrap();
        std::fs::copy("/usr/bin/true", root.join("photo.jpg")).unwrap();
        let told = crate::openwith::file_type(&text).is_some();
        assert!(might_run(&jar));
        assert!(might_run(&root.join("notes.txt")), "a link is judged by where it leads");
        assert_eq!(link_target(&root.join("notes.txt")).as_deref(), Some("payload.jar"));
        assert_eq!(link_target(&text), None);
        if told {
            assert!(might_run(&root.join("photo.jpg")), "contents decide, not the extension");
            assert!(!might_run(&text));
            assert!(!might_run(&root.join("letter.txt")));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
