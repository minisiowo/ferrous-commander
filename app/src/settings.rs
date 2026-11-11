//! Settings and session, saved as a Lua data table in the user's config folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsx::Sorting;
use crate::theme::ThemeName;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Layout {
    /// Column strip on top, file list below, details at the bottom.
    #[default]
    Standard,
    /// Folder columns side by side, the current folder as the last column, preview at the right.
    Columns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidebarSection {
    Drives,
    Cloud,
    Favorite(usize),
    History,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FavGroup {
    pub name: String,
    pub paths: Vec<PathBuf>,
    /// Custom sidebar names by path. Shortcuts only: folders on disk keep their names.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
}

impl FavGroup {
    pub fn new(name: impl Into<String>, paths: Vec<PathBuf>) -> Self {
        FavGroup { name: name.into(), paths, labels: HashMap::new() }
    }

    fn label_key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// The name shown in the sidebar for a shortcut.
    pub fn label(&self, path: &Path) -> String {
        self.labels.get(&Self::label_key(path)).cloned().unwrap_or_else(|| crate::fsx::display_name(path))
    }

    /// Sets a shortcut's name; an empty name or the folder's own name clears the custom one.
    pub fn set_label(&mut self, path: &Path, name: &str) {
        let name = name.trim();
        if name.is_empty() || name == crate::fsx::display_name(path) {
            self.labels.remove(&Self::label_key(path));
        } else {
            self.labels.insert(Self::label_key(path), name.to_string());
        }
    }

    pub fn remove_at(&mut self, index: usize) -> Option<(PathBuf, Option<String>)> {
        if index >= self.paths.len() { return None; }
        let path = self.paths.remove(index);
        let label = self.labels.remove(&Self::label_key(&path));
        Some((path, label))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PaneSession {
    pub tabs: Vec<PathBuf>,
    pub active: usize,
}

/// The main window as it was last left, in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowPlace {
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl WindowPlace {
    pub const MIN: [f32; 2] = [550.0, 420.0];

    /// The size to open at. The file can be edited by hand, so nothing in it is trusted.
    pub fn size(self) -> [f32; 2] {
        let fit = |value: f32, min: f32, usual: f32| if value.is_finite() { value.clamp(min, 16_000.0) } else { usual };
        let usual = WindowPlace::default();
        [fit(self.width, Self::MIN[0], usual.width), fit(self.height, Self::MIN[1], usual.height)]
    }
}

impl Default for WindowPlace {
    fn default() -> Self {
        WindowPlace { width: 1200.0, height: 750.0, maximized: false }
    }
}

/// Settings that change the contents or order of a file listing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListingOptions {
    pub sorting: Sorting,
    pub show_hidden: bool,
    pub tree_files: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeName,
    pub accent: String,
    pub layout: Layout,
    pub dual: bool,
    pub show_hidden: bool,
    pub show_extensions: bool,
    pub age_colors: bool,
    pub mebibytes: bool,
    pub tree_files: bool,
    pub show_columns: bool,
    pub show_preview: bool,
    pub show_sidebar: bool,
    pub sorting: Sorting,
    /// Per-folder sorting.
    pub folder_sorting: HashMap<String, Sorting>,
    pub favorites: Vec<FavGroup>,
    pub history: Vec<PathBuf>,
    /// Folders visited, the latest first: the quick palette's history, longer than the sidebar's.
    pub recent_folders: Vec<PathBuf>,
    /// Commands and settings last used from the quick palette, by their ids.
    pub palette_recent: Vec<String>,
    pub show_history: bool,
    pub show_cloud: bool,
    pub sidebar_order: Vec<SidebarSection>,
    pub removed_groups: Vec<FavGroup>,
    pub panes: Vec<PaneSession>,
    /// Panel arrangement (tiling tree over `panes`).
    pub tiles: Option<crate::tiles::Tile>,
    pub tags: HashMap<String, u8>,
    pub font_size: f32,
    /// Keys work as in Neovim, on top of the standard shortcuts.
    pub vim_keys: bool,
    /// While a shortcut is unfinished, show which keys can come next.
    pub which_key: bool,
    pub keymap: crate::keymap::KeymapSettings,
    pub window: WindowPlace,
    /// Windows 11 only: the desktop's Mica material behind the whole window.
    pub window_effect: bool,
    /// Buttons, cards, menus and windows have rounded corners instead of square ones.
    pub rounded: bool,
    /// A thin line around every panel and every card of the sidebar.
    pub outlines: bool,
}

impl Settings {
    pub(crate) fn listing_options(&self) -> ListingOptions {
        ListingOptions { sorting: self.sorting, show_hidden: self.show_hidden, tree_files: self.tree_files }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeName::Dark,
            accent: crate::defaults::ACCENT_RGB.into(),
            layout: Layout::Standard,
            tree_files: false,
            dual: true,
            show_hidden: crate::defaults::SHOW_HIDDEN_FILES,
            show_extensions: crate::defaults::SHOW_FILE_EXTENSION,
            age_colors: crate::defaults::USE_FILE_AGE_COLOR,
            mebibytes: crate::defaults::USE_MEBIBYTES,
            // Column strip and preview are collapsed in the person's window; ` and the
            // settings bring them back.
            show_columns: false,
            show_preview: false,
            show_sidebar: true,
            sorting: Sorting::default(),
            folder_sorting: HashMap::new(),
            favorites: default_favorites(),
            history: Vec::new(),
            recent_folders: Vec::new(),
            palette_recent: Vec::new(),
            show_history: true,
            show_cloud: true,
            sidebar_order: Vec::new(),
            removed_groups: Vec::new(),
            panes: Vec::new(),
            tiles: None,
            tags: HashMap::new(),
            font_size: crate::defaults::ITEM_FONT_SIZE,
            vim_keys: false,
            which_key: true,
            keymap: Default::default(),
            window: WindowPlace::default(),
            window_effect: false,
            rounded: crate::defaults::ROUNDED_CORNERS,
            outlines: true,
        }
    }
}

fn default_favorites() -> Vec<FavGroup> {
    let mut paths = Vec::new();
    if let Some(h) = dirs::home_dir() {
        paths.push(h);
    }
    for d in [
        dirs::desktop_dir(),
        dirs::document_dir(),
        dirs::download_dir(),
        dirs::picture_dir(),
        dirs::audio_dir(),
        dirs::video_dir(),
    ]
    .into_iter()
    .flatten()
    {
        if !paths.contains(&d) {
            paths.push(d);
        }
    }
    let user = dirs::home_dir()
        .and_then(|h| h.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "Favorites".into());
    vec![FavGroup::new(user, paths)]
}

pub fn config_directory() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("ferrous"))
}

impl Settings {
    pub fn normalize_sidebar(&mut self) {
        let mut sections = vec![SidebarSection::Drives, SidebarSection::Cloud];
        sections.extend((0..self.favorites.len()).map(SidebarSection::Favorite));
        sections.push(SidebarSection::History);
        let mut seen = Vec::new();
        self.sidebar_order.retain(|s| {
            if sections.contains(s) && !seen.contains(s) { seen.push(*s); true } else { false }
        });
        for section in sections {
            if !self.sidebar_order.contains(&section) { self.sidebar_order.push(section); }
        }
    }

    pub fn reorder_sidebar(&mut self, source: SidebarSection, target: SidebarSection, after: bool) {
        self.normalize_sidebar();
        if source == target { return; }
        self.sidebar_order.retain(|s| *s != source);
        if let Some(i) = self.sidebar_order.iter().position(|s| *s == target) {
            self.sidebar_order.insert(i + usize::from(after), source);
        }
    }

    /// Moves a shortcut to `at`, an index into the target group as it looks before
    /// the move, keeping its custom name. Returns false when nothing changed.
    pub fn move_favorite(&mut self, from: usize, index: usize, to: usize, at: usize) -> bool {
        if to >= self.favorites.len() { return false; }
        let Some(path) = self.favorites.get(from).and_then(|g| g.paths.get(index)).cloned() else { return false };
        let at = at.min(self.favorites[to].paths.len());
        if from == to {
            if at == index || at == index + 1 { return false; }
        } else if self.favorites[to].paths.contains(&path) {
            return false;
        }
        let Some((path, label)) = self.favorites[from].remove_at(index) else { return false };
        let at = if from == to && index < at { at - 1 } else { at };
        let target = &mut self.favorites[to];
        if let Some(label) = label { target.labels.insert(FavGroup::label_key(&path), label); }
        target.paths.insert(at, path);
        true
    }

    /// Puts shortcuts at `at`, an index into the group as it looks before the
    /// call. Paths already in the group move there and keep their custom names.
    /// Returns false when nothing changed.
    pub fn place_favorites(&mut self, group: usize, at: usize, paths: Vec<PathBuf>) -> bool {
        let Some(target) = self.favorites.get_mut(group) else { return false };
        let before = target.paths.clone();
        let mut at = at.min(target.paths.len());
        for path in paths.into_iter().filter(|p| p.is_absolute()) {
            if let Some(old) = target.paths.iter().position(|p| *p == path) {
                target.paths.remove(old);
                if old < at { at -= 1; }
            }
            target.paths.insert(at, path);
            at += 1;
        }
        target.paths != before
    }

    /// The settings, and what is wrong when an existing file could not be used.
    pub fn load() -> (Settings, Option<String>) {
        let Some(base) = dirs::config_dir() else { return (Self::default(), None) };
        Self::load_from(&base)
    }

    fn load_from(base: &std::path::Path) -> (Settings, Option<String>) {
        let file = base.join("ferrous/settings.lua");
        let unusable = |e: &dyn std::fmt::Display| {
            eprintln!("Cannot load {}: {e}", file.display());
            (Self::default(), Some(format!("{}: {e}", file.display())))
        };
        match std::fs::read_to_string(&file) {
            Ok(text) => match Self::from_lua(&text) {
                Ok(settings) => (settings, None),
                Err(e) => unusable(&e),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Keep existing installations' preferences and sessions on migration.
                let settings = ["ferrous/settings.json", "ferrous-commander/settings.json"]
                    .into_iter().find_map(|path| std::fs::read_to_string(base.join(path))
                        .ok().and_then(|text| serde_json::from_str::<Self>(&text).ok()))
                    .unwrap_or_default();
                let _ = settings.save_to(&file);
                (settings, None)
            }
            Err(e) => unusable(&e),
        }
    }

    fn from_lua(text: &str) -> Result<Self, String> {
        serde_json::from_value(crate::lua_config::decode(text)?).map_err(|e| e.to_string())
    }

    pub fn save_result(&self) -> std::io::Result<PathBuf> {
        let dir = config_directory().ok_or_else(|| std::io::Error::other("User configuration directory is unavailable"))?;
        self.save_over(&dir.join("settings.lua"))?;
        Ok(dir)
    }

    fn save_over(&self, file: &std::path::Path) -> std::io::Result<()> {
        // Preserve hand-edited invalid configuration so a typo can be corrected.
        // The same goes for a file that cannot be read right now.
        match std::fs::read_to_string(file) {
            Ok(text) => { Self::from_lua(&text).map_err(std::io::Error::other)?; }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.save_to(file)
    }

    /// Text settings cannot hold a path that is not valid Unicode, and one such path among
    /// the history or tabs would otherwise stop every save, without a word. Linux drives
    /// can have such names. A tab keeps its place, on the nearest folder that can be stored.
    fn storable(&self) -> Self {
        let text = |p: &PathBuf| p.to_str().is_some();
        let mut s = self.clone();
        s.history.retain(text);
        s.recent_folders.retain(text);
        for group in s.favorites.iter_mut().chain(&mut s.removed_groups) { group.paths.retain(text); }
        for pane in &mut s.panes {
            for tab in &mut pane.tabs {
                if tab.to_str().is_none() {
                    *tab = tab.ancestors().find(|a| a.to_str().is_some()).unwrap_or(Path::new("")).to_path_buf();
                }
            }
        }
        s
    }

    fn save_to(&self, f: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let s = crate::lua_config::encode(&serde_json::to_value(self.storable())?);
        // Per-process name: two running windows must not write one temporary file.
        let tmp = f.with_extension(format!("lua.{}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        // Visited folders, tags and favorites are nobody else's business.
        #[cfg(unix)]
        { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        let written = options.open(&tmp).and_then(|mut file| {
            use std::io::Write;
            file.write_all(s.as_bytes())?;
            // On disk before the rename, or a power cut can leave an empty settings file.
            file.sync_all()
        });
        let result = written.and_then(|()| std::fs::rename(&tmp, f));
        if result.is_err() { let _ = std::fs::remove_file(&tmp); }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_a_raw_name_does_not_stop_settings_from_saving() {
        let dir = std::env::temp_dir().join(format!("fc_raw_settings_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        let raw = {
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(b"/mnt/usb/r\xff\xfe".to_vec()))
        };
        // NTFS and exFAT names are UTF-16 that nobody checks, so half a surrogate pair is legal.
        #[cfg(windows)]
        let raw = {
            use std::os::windows::ffi::OsStringExt;
            let name = std::ffi::OsString::from_wide(&[b'r' as u16, 0xD800]);
            PathBuf::from("/mnt/usb").join(name)
        };
        let mut s = Settings::default();
        s.font_size = 17.0;
        s.history.push(raw.clone());
        s.favorites[0].paths.push(raw.clone());
        s.panes.push(PaneSession { tabs: vec![raw.clone(), "/home".into()], active: 1 });
        let file = dir.join("settings.lua");
        s.save_to(&file).expect("settings must save");
        let back = Settings::from_lua(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(back.font_size, 17.0);
        assert!(back.history.is_empty());
        assert_eq!(back.panes[0].tabs, [PathBuf::from("/mnt/usb"), PathBuf::from("/home")], "tabs keep their places");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn favorites_move_with_their_custom_names() {
        let (a, b, c) = (Path::new("/a"), Path::new("/b"), Path::new("/c"));
        let mut s = Settings::default();
        s.favorites = vec![
            FavGroup::new("One", vec![a.into(), b.into(), c.into()]),
            FavGroup::new("Two", vec![c.into()]),
        ];
        s.favorites[0].set_label(a, "Alpha");
        assert!(!s.move_favorite(0, 0, 0, 0), "dropping on itself changes nothing");
        assert!(!s.move_favorite(0, 0, 0, 1), "dropping just below itself changes nothing");
        assert!(s.move_favorite(0, 0, 0, 2));
        assert_eq!(s.favorites[0].paths, [b, a, c]);
        assert!(s.move_favorite(0, 2, 0, 0));
        assert_eq!(s.favorites[0].paths, [c, b, a]);
        assert!(!s.move_favorite(0, 0, 1, 0), "a group never holds the same path twice");
        assert_eq!(s.favorites[0].paths, [c, b, a]);
        assert!(s.move_favorite(0, 2, 1, 0));
        assert_eq!(s.favorites[1].paths, [a, c]);
        assert_eq!(s.favorites[1].label(a), "Alpha");
        assert!(s.favorites[0].labels.is_empty());
        s.favorites[1].set_label(a, "  ");
        assert_eq!(s.favorites[1].label(a), "a");
        s.favorites[1].set_label(a, "a");
        assert!(s.favorites[1].labels.is_empty(), "the folder's own name is not stored");
        let old: FavGroup = serde_json::from_str(r#"{"name":"Old","paths":[]}"#).unwrap();
        assert!(old.labels.is_empty(), "settings saved before custom names still load");
    }

    #[test]
    fn lua_round_trip_preserves_session_and_special_paths() {
        let mut s = Settings::default();
        s.sidebar_order = vec![SidebarSection::Drives, SidebarSection::Favorite(0)];
        s.favorites = vec![FavGroup::new("Zażółć \"work\"\n", vec![PathBuf::from(r"C:\Work\Łódź")])];
        s.favorites[0].set_label(Path::new(r"C:\Work\Łódź"), "Praca \"główna\"");
        s.panes = vec![PaneSession { tabs: vec![PathBuf::from(r"\\server\share")], active: 0 }];
        s.tiles = Some(crate::tiles::Tile::Split { vertical: false, ratio: 0.5,
            a: Box::new(crate::tiles::Tile::Leaf(0)), b: Box::new(crate::tiles::Tile::Leaf(1)) });
        s.tags.insert("true".into(), 2);
        s.folder_sorting.insert(r"C:\Work".into(), Sorting::default());
        let expected = serde_json::to_value(&s).unwrap();
        let text = crate::lua_config::encode(&expected);
        let read = Settings::from_lua(&text).unwrap();
        assert_eq!(serde_json::to_value(read).unwrap(), expected);
        let defaults = Settings::default();
        assert_eq!(serde_json::to_value(Settings::from_lua(&crate::lua_config::encode(&serde_json::to_value(&defaults).unwrap())).unwrap()).unwrap(), serde_json::to_value(defaults).unwrap());
    }

    #[test]
    fn lua_partial_edits_comments_and_invalid_input() {
        let s = Settings::from_lua("-- preferences\nreturn { --[[ note ]] font_size = 14; show_hidden = true, theme = 'Nord', favorites = {}, }").unwrap();
        assert_eq!(s.font_size, 14.0);
        assert!(s.show_hidden && s.show_sidebar && s.favorites.is_empty());
        assert_eq!(s.theme, ThemeName::TokyoNight);
        for (old, new) in [("Midnight", ThemeName::TokyoNight), ("Light", ThemeName::CatppuccinLatte)] {
            let s = Settings::from_lua(&format!("return {{ theme = '{old}', favorites = {{}}, }}")).unwrap();
            assert_eq!(s.theme, new, "{old}");
        }
        for bad in ["return {", "return { dual = 'yes' }", "return {}; os.execute('x')", "while true do end", "return { font_size = 1 + 2 }"] {
            assert!(Settings::from_lua(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn saving_never_replaces_a_file_it_could_not_understand() {
        let dir = std::env::temp_dir().join(format!("ferrous_save_test_{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("settings.lua");
        for damaged in [&b"return { typo"[..], &b"\xff\xfe not text"[..]] {
            std::fs::write(&file, damaged).unwrap();
            assert!(Settings::default().save_over(&file).is_err());
            assert_eq!(std::fs::read(&file).unwrap(), damaged);
        }
        std::fs::remove_file(&file).unwrap();
        Settings::default().save_over(&file).unwrap();
        Settings::default().save_over(&file).unwrap();
        assert!(Settings::from_lua(&std::fs::read_to_string(&file).unwrap()).is_ok());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file left behind");
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn migrate_json_locations_and_prefer_existing_lua() {
        let base = std::env::temp_dir().join(format!("ferrous_lua_test_{}", std::process::id()));
        for source in ["ferrous/settings.json", "ferrous-commander/settings.json"] {
            let old = base.join(source);
            std::fs::create_dir_all(old.parent().unwrap()).unwrap();
            let s = Settings { font_size: 17.0, show_hidden: true, ..Settings::default() };
            let original = serde_json::to_string_pretty(&s).unwrap();
            std::fs::write(&old, &original).unwrap();
            assert_eq!(Settings::load_from(&base).0.font_size, 17.0);
            let lua = base.join("ferrous/settings.lua");
            assert!(lua.exists());
            assert_eq!(std::fs::read_to_string(&old).unwrap(), original);
            std::fs::write(&lua, "return { font_size = 14, show_hidden = false }").unwrap();
            assert_eq!(Settings::load_from(&base).0.font_size, 14.0);
            assert!(!Settings::load_from(&base).0.show_hidden);
            assert_eq!(Settings::load_from(&base).1, None);
            std::fs::write(&lua, "return { font_size = 'huge' }").unwrap();
            let (settings, problem) = Settings::load_from(&base);
            assert_eq!(settings.font_size, Settings::default().font_size);
            assert!(problem.is_some_and(|p| p.contains("settings.lua")), "a bad file is reported");
            std::fs::remove_file(lua).unwrap();
            std::fs::remove_file(old).unwrap();
        }
        std::fs::remove_dir(base.join("ferrous")).unwrap();
        std::fs::remove_dir(base.join("ferrous-commander")).unwrap();
        std::fs::remove_dir(base).unwrap();
    }

    #[test]
    fn the_window_opens_as_it_was_left_and_never_at_a_useless_size() {
        let old = Settings::from_lua("return { font_size = 14 }").unwrap();
        assert_eq!(old.window, WindowPlace::default(), "settings saved before the window was remembered");
        assert!(!old.window_effect);
        let mut s = Settings::default();
        s.window = WindowPlace { width: 1700.0, height: 900.0, maximized: true };
        s.window_effect = true;
        let back = Settings::from_lua(&crate::lua_config::encode(&serde_json::to_value(&s).unwrap())).unwrap();
        assert_eq!(back.window, s.window);
        assert!(back.window_effect);
        assert_eq!(back.window.size(), [1700.0, 900.0]);
        let edited = Settings::from_lua("return { window = { width = 3, height = 99999 } }").unwrap();
        assert_eq!(edited.window.size(), [WindowPlace::MIN[0], 16_000.0]);
        assert_eq!(WindowPlace { width: f32::NAN, height: f32::INFINITY, maximized: false }.size(), [1200.0, 750.0]);
    }

    #[test]
    fn the_palette_history_survives_the_lua_file_empty_or_filled() {
        let old = Settings::from_lua("return { font_size = 14 }").unwrap();
        assert!(old.recent_folders.is_empty() && old.palette_recent.is_empty(), "settings saved before the palette");
        let mut s = Settings::default();
        let round = |s: &Settings| Settings::from_lua(&crate::lua_config::encode(&serde_json::to_value(s).unwrap())).unwrap();
        // An empty Lua table says nothing about being a list.
        assert!(round(&s).recent_folders.is_empty() && round(&s).palette_recent.is_empty());
        s.recent_folders = vec![PathBuf::from("/home/me/Projects"), PathBuf::from(r"C:\Work")];
        s.palette_recent = vec!["toggle_hidden".into(), "theme:Gruvbox".into()];
        assert_eq!(round(&s).recent_folders, s.recent_folders);
        assert_eq!(round(&s).palette_recent, s.palette_recent);
    }

    #[test]
    fn settings_without_shortcuts_load_with_the_mode_off() {
        let s = Settings::from_lua("return { font_size = 14 }").unwrap();
        assert!(!s.vim_keys && s.keymap == Default::default());
        let mut changed = Settings::default();
        changed.vim_keys = true;
        changed.keymap.leader = "space".into();
        changed.keymap.standard.bindings.insert("new_tab".into(), String::new());
        changed.keymap.neovim.folders.push(crate::keymap::FolderKey { keys: "g p".into(), path: "/projects".into(), new_tab: true });
        let text = crate::lua_config::encode(&serde_json::to_value(&changed).unwrap());
        let back = Settings::from_lua(&text).unwrap();
        assert!(back.vim_keys);
        assert_eq!(back.keymap, changed.keymap, "an unbound command survives the Lua file");
    }
}
