//! Keyboard shortcuts as data: which keys run which command, in which layer.
//! Nothing here draws or acts, so all of it can be tested without a window.

use std::collections::BTreeMap;
use std::path::PathBuf;

use egui::{Event, Key, Modifiers};
use serde::{Deserialize, Serialize};

/// The longest key sequence a shortcut can have.
pub const MAX_KEYS: usize = 4;

/// Stands for the leader key in the settings and in the Neovim defaults, so that
/// every shortcut built on it follows when the leader changes.
const LEADER: &str = "leader";
const DEFAULT_LEADER: &str = "ctrl+b";

/// The names shown for the groups of shortcuts under the leader.
const LEADER_GROUPS: [(Key, &str); 4] = [
    (Key::B, "+Tabs"),
    (Key::F, "+Files"),
    (Key::U, "+View"),
    (Key::W, "+Panels"),
];

/// One press: a key and the modifiers held with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

fn digit(key: Key) -> Option<usize> {
    Some(match key {
        Key::Num0 => 0, Key::Num1 => 1, Key::Num2 => 2, Key::Num3 => 3, Key::Num4 => 4,
        Key::Num5 => 5, Key::Num6 => 6, Key::Num7 => 7, Key::Num8 => 8, Key::Num9 => 9,
        _ => return None,
    })
}

fn is_letter(key: Key) -> bool {
    let name = key.name();
    name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic()
}

/// egui reports these as keys of their own, but they only ever accompany a shortcut.
fn is_modifier(key: Key) -> bool {
    matches!(key, Key::ShiftLeft | Key::ShiftRight | Key::ControlLeft | Key::ControlRight
        | Key::AltLeft | Key::AltRight | Key::SuperLeft | Key::SuperRight)
}

/// Keys that produce a character whose key combination depends on the keyboard layout.
fn is_punctuation(key: Key) -> bool {
    matches!(key, Key::Space | Key::Colon | Key::Comma | Key::Minus | Key::Period | Key::Plus
        | Key::Equals | Key::Semicolon | Key::Backslash | Key::Slash | Key::Pipe | Key::Questionmark
        | Key::Exclamationmark | Key::OpenBracket | Key::CloseBracket | Key::OpenCurlyBracket
        | Key::CloseCurlyBracket | Key::Backtick | Key::Quote)
}

impl Chord {
    pub fn new(key: Key, m: Modifiers) -> Chord {
        Chord { key, ctrl: m.command || m.ctrl, shift: m.shift, alt: m.alt }
    }

    fn plain(key: Key) -> Chord {
        Chord { key, ctrl: false, shift: false, alt: false }
    }

    /// Whether this press would type a character.
    pub fn is_typing(self) -> bool {
        !self.ctrl && !self.alt && (is_letter(self.key) || is_punctuation(self.key) || digit(self.key).is_some())
    }

    fn parse(text: &str) -> Option<Chord> {
        let mut parts: Vec<&str> = text.split('+').collect();
        let name = parts.pop()?;
        let key = Key::ALL.iter().copied().find(|k| k.name().eq_ignore_ascii_case(name)).or_else(|| Key::from_name(name))?;
        let mut chord = Chord::plain(key);
        for part in parts {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" => chord.alt = true,
                _ => return None,
            }
        }
        Some(chord)
    }

    /// The form stored in the settings file.
    fn stored(self) -> String {
        let mut out = String::new();
        for (on, name) in [(self.ctrl, "ctrl+"), (self.shift, "shift+"), (self.alt, "alt+")] {
            if on { out.push_str(name); }
        }
        out + &self.key.name().to_ascii_lowercase()
    }

    /// The form shown to people.
    pub fn label(self) -> String {
        let mut out = String::new();
        for (on, name) in [(self.ctrl, "Ctrl+"), (self.shift, "Shift+"), (self.alt, "Alt+")] {
            if on { out.push_str(name); }
        }
        let modified = !out.is_empty();
        let name = self.key.name();
        if self.key == Key::Space {
            out.push_str("Space");
        } else if is_punctuation(self.key) {
            out.push_str(self.key.symbol_or_name());
        } else if is_letter(self.key) && !modified {
            out.push_str(&name.to_ascii_lowercase());
        } else {
            out.push_str(name);
        }
        out
    }
}

fn parse_with(text: &str, leader: Option<Chord>) -> Option<Vec<Chord>> {
    let mut keys: Vec<Chord> = text.split_whitespace()
        .map(|part| if part.eq_ignore_ascii_case(LEADER) { leader } else { Chord::parse(part) })
        .collect::<Option<_>>()?;
    // Earlier versions recorded a modifier as a step of its own.
    keys.retain(|c| !is_modifier(c.key));
    (!keys.is_empty() && keys.len() <= MAX_KEYS).then_some(keys)
}

/// `g h` or `ctrl+shift+n`; `None` when any part is not a known key.
pub fn parse_keys(text: &str) -> Option<Vec<Chord>> {
    parse_with(text, None)
}

pub fn stored_keys(keys: &[Chord]) -> String {
    keys.iter().map(|c| c.stored()).collect::<Vec<_>>().join(" ")
}

pub fn keys_label(keys: &[Chord]) -> String {
    keys.iter().map(|c| c.label()).collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    CursorDown, CursorUp, PageDown, PageUp, HalfPageDown, HalfPageUp, First, Last,
    Open, OpenNewTab, FolderIn, FolderOut, Left, Right, ParentOrErase, Parent, Back, Forward,
    GoHome, EditPath, EditPathSelected, Search, Filter,
    SelectAll, Visual, VisualCopy, VisualCut, Copy, Cut, Paste, CopyPath, Rename, Duplicate,
    Delete, DeleteForever, NewFile, NewFolder, Refresh, Terminal, Properties,
    NewTab, CloseTab, ReopenTab, NextTab, PrevTab,
    NewPanel, ClosePanel, ToggleSplit, FocusLeft, FocusRight, FocusUp, FocusDown, NextPanel, PrevPanel,
    Escape, ToggleHidden, ToggleColumns, TogglePreview,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group { Moving, Folders, Files, Tabs, Panels, View }

pub struct Info {
    pub command: Command,
    /// Names the command in the settings file, so it must never change.
    pub id: &'static str,
    pub label: &'static str,
    pub group: Group,
    pub standard: &'static [&'static str],
    pub neovim: &'static [&'static str],
    /// Holding the key down runs the command again.
    pub repeat: bool,
}

const fn c(
    command: Command, id: &'static str, label: &'static str, group: Group,
    standard: &'static [&'static str], neovim: &'static [&'static str], repeat: bool,
) -> Info {
    Info { command, id, label, group, standard, neovim, repeat }
}

const PROPERTIES: &[&str] = if cfg!(target_os = "linux") { &["alt+enter"] } else { &[] };

use Command as K;
use Group::*;
pub const COMMANDS: &[Info] = &[
    c(K::CursorDown, "cursor_down", "Move down", Moving, &["down"], &["j"], true),
    c(K::CursorUp, "cursor_up", "Move up", Moving, &["up"], &["k"], true),
    c(K::PageDown, "page_down", "Page down", Moving, &["pagedown"], &[], true),
    c(K::PageUp, "page_up", "Page up", Moving, &["pageup"], &[], true),
    c(K::HalfPageDown, "half_page_down", "Half a page down", Moving, &[], &["ctrl+d"], true),
    c(K::HalfPageUp, "half_page_up", "Half a page up", Moving, &[], &["ctrl+u"], true),
    c(K::First, "first", "First item", Moving, &["home"], &["g g"], true),
    c(K::Last, "last", "Last item", Moving, &["end"], &["shift+g"], true),

    c(K::Open, "open", "Open", Folders, &["enter"], &[], false),
    c(K::OpenNewTab, "open_new_tab", "Open folder in a new tab", Folders, &["ctrl+enter"], &[], false),
    c(K::FolderIn, "folder_in", "Enter folder or open file", Folders, &[], &["l"], true),
    c(K::FolderOut, "folder_out", "Leave folder", Folders, &[], &["h"], true),
    c(K::Left, "left", "Collapse, or go to the parent column", Folders, &["left"], &[], true),
    c(K::Right, "right", "Expand, or enter the column", Folders, &["right"], &[], true),
    c(K::ParentOrErase, "parent_or_erase", "Parent folder, or erase the filter", Folders, &["backspace"], &[], true),
    c(K::Parent, "parent", "Parent folder", Folders, &["alt+up"], &[], true),
    c(K::Back, "back", "Back", Folders, &["alt+left"], &["ctrl+o"], true),
    c(K::Forward, "forward", "Forward", Folders, &["alt+right"], &["ctrl+i"], true),
    c(K::GoHome, "go_home", "Home folder", Folders, &[], &["g h"], false),
    c(K::EditPath, "edit_path", "Edit path", Folders, &["\\", "alt+d"], &[], false),
    c(K::EditPathSelected, "edit_path_selected", "Edit path, all of it selected", Folders, &["ctrl+l"], &["leader f p"], false),
    c(K::Search, "search", "Search", Folders, &["ctrl+f"], &["leader f f"], false),
    c(K::Filter, "filter", "Filter this folder", Folders, &[], &["/"], false),

    c(K::SelectAll, "select_all", "Select all", Files, &["ctrl+a"], &[], false),
    c(K::Visual, "visual", "Select rows", Files, &[], &["v", "shift+v"], false),
    c(K::Copy, "copy", "Copy", Files, &["ctrl+c"], &["y y"], false),
    c(K::Cut, "cut", "Cut", Files, &["ctrl+x"], &["d d"], false),
    c(K::Paste, "paste", "Paste", Files, &["ctrl+v"], &["p"], false),
    c(K::VisualCopy, "visual_copy", "Copy selected rows", Files, &[], &["y"], false),
    c(K::VisualCut, "visual_cut", "Cut selected rows", Files, &[], &["d"], false),
    c(K::CopyPath, "copy_path", "Copy path", Files, &["ctrl+shift+c"], &["leader f y"], false),
    c(K::Rename, "rename", "Rename", Files, &["f2"], &["leader f r"], false),
    c(K::Duplicate, "duplicate", "Duplicate", Files, &["ctrl+d"], &["leader f c"], false),
    c(K::Delete, "delete", "Delete", Files, &["delete"], &["leader f x"], false),
    c(K::DeleteForever, "delete_forever", "Delete for good", Files, &["shift+delete"], &[], false),
    c(K::NewFile, "new_file", "New file", Files, &["ctrl+n"], &["leader f n"], false),
    c(K::NewFolder, "new_folder", "New folder", Files, &["ctrl+shift+n"], &["leader f d"], false),
    c(K::Refresh, "refresh", "Refresh", Files, &["f5"], &[], false),
    c(K::Terminal, "terminal", "Open in command shell", Files, &["f10"], &["leader f t"], false),
    c(K::Properties, "properties", "Properties", Files, PROPERTIES, &[], false),

    c(K::NewTab, "new_tab", "New tab", Tabs, &["ctrl+t"], &["leader b n"], false),
    c(K::CloseTab, "close_tab", "Close tab", Tabs, &["ctrl+w"], &["leader b d", "ctrl+w q", "ctrl+w c"], false),
    c(K::ReopenTab, "reopen_tab", "Reopen closed tab", Tabs, &["ctrl+shift+t"], &["leader b u"], false),
    c(K::NextTab, "next_tab", "Next tab", Tabs, &["ctrl+tab"], &["shift+l"], true),
    c(K::PrevTab, "prev_tab", "Previous tab", Tabs, &["ctrl+shift+tab"], &["shift+h"], true),

    c(K::NewPanel, "new_panel", "New panel", Panels, &["ctrl+shift+enter"], &["leader w n"], false),
    c(K::ClosePanel, "close_panel", "Close panel", Panels, &["ctrl+shift+q"], &["leader w d"], false),
    c(K::ToggleSplit, "toggle_split", "Flip split", Panels, &["ctrl+shift+j"], &["leader w s"], false),
    c(K::FocusLeft, "focus_left", "Panel to the left", Panels, &["ctrl+alt+left"], &["leader w h", "ctrl+w h"], true),
    c(K::FocusRight, "focus_right", "Panel to the right", Panels, &["ctrl+alt+right"], &["leader w l", "ctrl+w l"], true),
    c(K::FocusUp, "focus_up", "Panel above", Panels, &["ctrl+alt+up"], &["leader w k", "ctrl+w k"], true),
    c(K::FocusDown, "focus_down", "Panel below", Panels, &["ctrl+alt+down"], &["leader w j", "ctrl+w j"], true),
    c(K::NextPanel, "next_panel", "Next panel", Panels, &["tab"], &["leader w w", "ctrl+w w"], true),
    c(K::PrevPanel, "prev_panel", "Previous panel", Panels, &["shift+tab"], &[], true),

    c(K::Escape, "escape", "Clear filter, then selection", View, &["escape"], &[], false),
    c(K::ToggleHidden, "toggle_hidden", "Hidden files", View, &["ctrl+h"], &["leader u h"], false),
    c(K::ToggleColumns, "toggle_columns", "Folder columns", View, &["`"], &["leader u c"], false),
    c(K::TogglePreview, "toggle_preview", "Preview", View, &["space"], &["leader u p"], false),
];

impl Command {
    pub fn info(self) -> &'static Info {
        COMMANDS.iter().find(|i| i.command == self).expect("every command is in the table")
    }

    /// Only meaningful while rows are being selected.
    pub fn visual_only(self) -> bool {
        matches!(self, Command::VisualCopy | Command::VisualCut)
    }

    /// Shift with a non-letter key that moves the cursor extends the selection.
    fn extends(self) -> bool {
        matches!(self, Command::CursorDown | Command::CursorUp | Command::PageDown | Command::PageUp | Command::First | Command::Last)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Command(Command),
    Folder { path: PathBuf, new_tab: bool },
}

impl Target {
    /// The name shown in the list of shortcuts.
    pub fn label(&self) -> String {
        match self {
            Target::Command(command) => command.info().label.to_string(),
            Target::Folder { path, new_tab: false } => format!("Go to {}", path.display()),
            Target::Folder { path, new_tab: true } => format!("Open {} in a new tab", path.display()),
        }
    }

    /// A name that fits a small hint.
    pub fn short_label(&self) -> String {
        match self {
            Target::Folder { path, new_tab } => {
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                if *new_tab { format!("{name} in a new tab") } else { format!("Go to {name}") }
            }
            Target::Command(_) => self.label(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub keys: Vec<Chord>,
    pub target: Target,
}

/// A folder opened by a shortcut of the user's own.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderKey {
    pub keys: String,
    pub path: PathBuf,
    /// Opens in a new tab instead of the current one.
    pub new_tab: bool,
}

impl FolderKey {
    pub fn target(&self) -> Target {
        Target::Folder { path: self.path.clone(), new_tab: self.new_tab }
    }
}

/// What the user changed in one layer. A command listed here replaces its defaults: its
/// shortcuts are one comma-separated text, and an empty text means no shortcut.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerSettings {
    pub bindings: BTreeMap<String, String>,
    pub folders: Vec<FolderKey>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeymapSettings {
    /// The key that `leader` stands for in the Neovim layer. Empty means none.
    pub leader: String,
    pub standard: LayerSettings,
    pub neovim: LayerSettings,
}

impl Default for KeymapSettings {
    fn default() -> KeymapSettings {
        KeymapSettings { leader: DEFAULT_LEADER.to_string(), standard: Default::default(), neovim: Default::default() }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer { Standard, Neovim }

impl Info {
    fn defaults(&self, layer: Layer) -> &'static [&'static str] {
        match layer { Layer::Standard => self.standard, Layer::Neovim => self.neovim }
    }
}

impl KeymapSettings {
    pub fn layer(&self, layer: Layer) -> &LayerSettings {
        match layer { Layer::Standard => &self.standard, Layer::Neovim => &self.neovim }
    }

    pub fn layer_mut(&mut self, layer: Layer) -> &mut LayerSettings {
        match layer { Layer::Standard => &mut self.standard, Layer::Neovim => &mut self.neovim }
    }

    pub fn leader(&self) -> Option<Chord> {
        Chord::parse(self.leader.trim())
    }

    fn leader_in(&self, layer: Layer) -> Option<Chord> {
        self.leader().filter(|_| layer == Layer::Neovim)
    }

    /// Reads keys from the settings, where `leader` can stand for the leader key.
    pub fn parse(&self, layer: Layer, text: &str) -> Option<Vec<Chord>> {
        parse_with(text, self.leader_in(layer))
    }

    /// Keys in the settings' form: a sequence that starts with the leader says so.
    fn stored(&self, layer: Layer, keys: &[Chord]) -> String {
        match keys.split_first() {
            Some((first, rest)) if !rest.is_empty() && Some(*first) == self.leader_in(layer) => {
                format!("{LEADER} {}", stored_keys(rest))
            }
            _ => stored_keys(keys),
        }
    }

    /// The shortcut in the Neovim layer that a new leader would take the key from.
    pub fn leader_clash(&self, chord: Chord) -> Option<Target> {
        let without_leader = KeymapSettings { leader: String::new(), ..self.clone() };
        layer_bindings(&without_leader, Layer::Neovim).into_iter().find(|b| b.keys[0] == chord).map(|b| b.target)
    }

    /// Moves everything built on the leader to `chord`, and removes the shortcuts in its way.
    pub fn set_leader(&mut self, chord: Chord) {
        let in_the_way = |text: &str| parse_keys(text).is_some_and(|k| k[0] == chord);
        for info in COMMANDS {
            let stored = self.neovim.bindings.get(info.id).cloned();
            let texts: Vec<String> = match &stored {
                Some(text) => text.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
                None => info.neovim.iter().map(|t| t.to_string()).collect(),
            };
            let kept: Vec<String> = texts.iter().filter(|t| !in_the_way(t)).cloned().collect();
            if kept.len() != texts.len() {
                self.neovim.bindings.insert(info.id.to_string(), kept.join(", "));
            }
        }
        self.neovim.folders.retain(|f| !in_the_way(&f.keys));
        self.leader = chord.stored();
    }

    /// Everything in the layer back to its defaults.
    pub fn reset_layer(&mut self, layer: Layer) {
        *self.layer_mut(layer) = Default::default();
        if layer == Layer::Neovim {
            self.leader = DEFAULT_LEADER.to_string();
        }
    }

    /// The shortcuts a command has in a layer, valid ones only.
    pub fn keys_of(&self, layer: Layer, info: &Info) -> Vec<Vec<Chord>> {
        match self.layer(layer).bindings.get(info.id) {
            Some(text) => text.split(',').filter_map(|t| self.parse(layer, t)).collect(),
            None => info.defaults(layer).iter().filter_map(|t| self.parse(layer, t)).collect(),
        }
    }

    /// Stores a command's shortcuts, or drops the entry when they equal the defaults.
    pub fn set_keys(&mut self, layer: Layer, info: &Info, keys: &[Vec<Chord>]) {
        let text = keys.iter().map(|k| self.stored(layer, k)).collect::<Vec<_>>().join(", ");
        let defaults: Vec<String> = info.defaults(layer).iter().filter_map(|t| self.parse(layer, t)).map(|k| self.stored(layer, &k)).collect();
        let bindings = &mut self.layer_mut(layer).bindings;
        if text == defaults.join(", ") {
            bindings.remove(info.id);
        } else {
            bindings.insert(info.id.to_string(), text);
        }
    }
}

impl KeymapSettings {
    /// Removes what a new shortcut would clash with, leaving the target's own shortcuts.
    pub fn clear_conflicts(&mut self, layer: Layer, keys: &[Chord], own: &Target) {
        let visual = matches!(own, Target::Command(c) if c.visual_only());
        for info in COMMANDS.iter().filter(|i| i.command.visual_only() == visual) {
            let mine = *own == Target::Command(info.command);
            let before = self.keys_of(layer, info);
            // Its own shortcut is in the way only when one would cut the other short.
            let kept: Vec<Vec<Chord>> = before.iter().filter(|k| !clashes(k, keys) || (mine && k.as_slice() == keys)).cloned().collect();
            if kept.len() != before.len() {
                self.set_keys(layer, info, &kept);
            }
        }
        if !visual {
            let leader = self.leader_in(layer);
            self.layer_mut(layer).folders.retain(|f| {
                f.target() == *own || parse_with(&f.keys, leader).is_none_or(|k| !clashes(&k, keys))
            });
        }
    }

    pub fn remove_folder(&mut self, layer: Layer, target: &Target) {
        self.layer_mut(layer).folders.retain(|f| f.target() != *target);
    }

    /// Gives a command one more shortcut, or a folder its only one.
    pub fn assign(&mut self, layer: Layer, target: &Target, keys: Vec<Chord>) {
        match target {
            Target::Command(command) => {
                let info = command.info();
                let mut all = self.keys_of(layer, info);
                if !all.contains(&keys) {
                    all.push(keys);
                }
                self.set_keys(layer, info, &all);
            }
            Target::Folder { path, new_tab } => {
                let stored = self.stored(layer, &keys);
                let folders = &mut self.layer_mut(layer).folders;
                match folders.iter_mut().find(|f| f.target() == *target) {
                    Some(folder) => folder.keys = stored,
                    None => folders.push(FolderKey { keys: stored, path: path.clone(), new_tab: *new_tab }),
                }
            }
        }
    }
}

/// How the keyboard is being read right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode { Standard, Normal, Visual }

/// The active shortcuts, layer by layer.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    standard: Vec<Binding>,
    neovim: Vec<Binding>,
    visual: Vec<Binding>,
    leader: Option<Chord>,
}

/// Two shortcuts in the same place clash when one is the start of the other.
fn clashes(a: &[Chord], b: &[Chord]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// What a layer's shortcuts run, in table order and then the user's folders.
fn layer_bindings(settings: &KeymapSettings, layer: Layer) -> Vec<Binding> {
    let mut out = Vec::new();
    for info in COMMANDS {
        for keys in settings.keys_of(layer, info) {
            out.push(Binding { keys, target: Target::Command(info.command) });
        }
    }
    for folder in &settings.layer(layer).folders {
        if let Some(keys) = settings.parse(layer, &folder.keys) {
            out.push(Binding { keys, target: folder.target() });
        }
    }
    out
}

fn is_visual_only(binding: &Binding) -> bool {
    matches!(binding.target, Target::Command(c) if c.visual_only())
}

impl Keymap {
    pub fn new(settings: &KeymapSettings) -> Keymap {
        let (visual, neovim) = layer_bindings(settings, Layer::Neovim).into_iter().partition(is_visual_only);
        let standard = layer_bindings(settings, Layer::Standard).into_iter().filter(|b| !is_visual_only(b)).collect();
        Keymap { standard, neovim, visual, leader: settings.leader() }
    }

    fn layers(&self, mode: Mode) -> Vec<&[Binding]> {
        match mode {
            Mode::Standard => vec![&self.standard],
            Mode::Normal => vec![&self.neovim, &self.standard],
            Mode::Visual => vec![&self.visual, &self.neovim, &self.standard],
        }
    }

    /// The first shortcut that really reaches a command, for showing it in the interface.
    pub fn label(&self, mode: Mode, command: Command) -> Option<String> {
        let layers = self.layers(mode);
        layers.iter().enumerate().find_map(|(k, layer)| {
            layer.iter()
                .filter(|b| b.target == Target::Command(command))
                .find(|b| !layers[..k].iter().any(|upper| upper.iter().any(|u| u.keys[0] == b.keys[0])))
                .map(|b| keys_label(&b.keys))
        })
    }

    fn group_name(&self, mode: Mode, prefix: &[Chord], key: Chord) -> Option<&'static str> {
        if mode == Mode::Standard || prefix != self.leader.as_slice() { return None; }
        LEADER_GROUPS.iter().find(|(k, _)| Chord::plain(*k) == key).map(|(_, name)| *name)
    }

    /// The keys that can follow `prefix`, from the layer that owns it, in key order.
    pub fn hints(&self, mode: Mode, prefix: &[Chord]) -> Vec<Hint> {
        for layer in self.layers(mode) {
            let next: Vec<&Binding> = layer.iter().filter(|b| b.keys.len() > prefix.len() && b.keys.starts_with(prefix)).collect();
            if next.is_empty() { continue; }
            let mut hints: Vec<Hint> = Vec::new();
            for binding in &next {
                let key = binding.keys[prefix.len()];
                if hints.iter().any(|h| h.key == key) { continue; }
                let same: Vec<&&Binding> = next.iter().filter(|b| b.keys[prefix.len()] == key).collect();
                let label = match same.as_slice() {
                    [only] if only.keys.len() == prefix.len() + 1 => only.target.short_label(),
                    many => self.group_name(mode, prefix, key).map_or_else(|| format!("+{} more", many.len()), str::to_string),
                };
                hints.push(Hint { key, label });
            }
            hints.sort_by_key(|h| h.key.label().to_lowercase());
            return hints;
        }
        Vec::new()
    }

    /// What a single press would run, for keys held down and for Shift+movement.
    fn single(&self, mode: Mode, chord: Chord) -> Option<Target> {
        for layer in self.layers(mode) {
            let mut hit = layer.iter().filter(|b| b.keys[0] == chord);
            if let Some(first) = hit.next() {
                let exact = layer.iter().find(|b| b.keys.len() == 1 && b.keys[0] == chord);
                let longer = first.keys.len() > 1 || hit.any(|b| b.keys.len() > 1);
                return exact.filter(|_| !longer).map(|b| b.target.clone());
            }
        }
        None
    }

    fn extending(&self, mode: Mode, chord: Chord) -> Option<Target> {
        if !chord.shift || chord.is_typing() { return None; }
        let target = self.single(mode, Chord { shift: false, ..chord })?;
        matches!(target, Target::Command(c) if c.extends()).then_some(target)
    }

    fn starts_any(&self, mode: Mode, chord: Chord) -> bool {
        self.layers(mode).iter().any(|layer| layer.iter().any(|b| b.keys[0] == chord))
    }
}

/// A shortcut this one cannot live with: in `layer`, either the same keys or the start of them.
pub fn conflict(settings: &KeymapSettings, layer: Layer, keys: &[Chord], own: &Target) -> Option<Target> {
    let visual = matches!(own, Target::Command(c) if c.visual_only());
    layer_bindings(settings, layer).into_iter()
        .filter(|b| is_visual_only(b) == visual)
        .find(|b| clashes(&b.keys, keys) && (b.target != *own || b.keys != keys))
        .map(|b| b.target)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// No shortcut uses this key.
    Unbound,
    /// The key was taken: the start of a sequence, a count, or a held key that does not repeat.
    Waiting,
    /// A sequence was abandoned; the key does nothing else.
    Cancelled,
    Run { target: Target, count: Option<usize>, extend: bool },
}

/// One key that can come next in an unfinished shortcut.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub key: Chord,
    pub label: String,
}

/// How far a sequence of keys has got in the active layers.
enum Step {
    Longer,
    Exact(Target),
}

fn step(map: &Keymap, mode: Mode, keys: &[Chord]) -> Option<Step> {
    for layer in map.layers(mode) {
        let mut exact = None;
        let mut longer = false;
        for binding in layer.iter().filter(|b| b.keys.starts_with(keys)) {
            if binding.keys.len() == keys.len() { exact.get_or_insert(binding); } else { longer = true; }
        }
        // A longer shortcut wins, so one that starts another can never run early.
        if longer {
            return Some(Step::Longer);
        }
        if let Some(binding) = exact {
            return Some(Step::Exact(binding.target.clone()));
        }
    }
    None
}

/// Follows a key sequence across presses.
#[derive(Clone, Debug, Default)]
pub struct Matcher {
    keys: Vec<Chord>,
    count: Option<usize>,
    /// When the sequence in `keys` was started.
    since: Option<std::time::Instant>,
    /// The key that finished a sequence: while held, it must not run on its own.
    finished: Option<Key>,
}

impl Matcher {
    pub fn clear(&mut self) {
        self.keys.clear();
        self.count = None;
        self.finished = None;
    }

    /// The keys of the unfinished shortcut.
    pub fn keys(&self) -> &[Chord] {
        &self.keys
    }

    /// How long the unfinished shortcut has been waiting for its next key.
    pub fn waited(&self) -> Option<std::time::Duration> {
        self.since.filter(|_| !self.keys.is_empty()).map(|at| at.elapsed())
    }

    /// What has been typed of an unfinished shortcut.
    pub fn pending(&self) -> String {
        let count = self.count.map(|c| c.to_string()).unwrap_or_default();
        let keys = keys_label(&self.keys);
        format!("{count}{}{keys}", if !count.is_empty() && !keys.is_empty() { " " } else { "" })
    }

    pub fn feed(&mut self, map: &Keymap, mode: Mode, chord: Chord, repeat: bool) -> Outcome {
        if repeat {
            return self.feed_held(map, mode, chord);
        }
        self.finished = None;
        let plain = !chord.ctrl && !chord.alt && !chord.shift;
        if self.keys.is_empty() && mode != Mode::Standard && plain
            && let Some(d) = digit(chord.key).filter(|d| *d > 0 || self.count.is_some())
            && !map.starts_any(mode, chord)
        {
            self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d).min(99_999));
            return Outcome::Waiting;
        }
        self.keys.push(chord);
        let mut found = step(map, mode, &self.keys);
        if found.is_none() && self.keys.len() > 1 && chord.ctrl {
            // Neovim takes Ctrl+W Ctrl+H for Ctrl+W h, for fingers still on Ctrl.
            *self.keys.last_mut().expect("just pushed") = Chord { ctrl: false, ..chord };
            found = step(map, mode, &self.keys);
        }
        match found {
            Some(Step::Longer) => {
                if self.keys.len() == 1 {
                    self.since = Some(std::time::Instant::now());
                }
                return Outcome::Waiting;
            }
            Some(Step::Exact(target)) => {
                let count = self.count.take();
                if self.keys.len() > 1 {
                    self.finished = Some(chord.key);
                }
                self.keys.clear();
                // Esc after a count takes back the count, like Esc in a sequence.
                if count.is_some() && target == Target::Command(Command::Escape) {
                    return Outcome::Cancelled;
                }
                return Outcome::Run { target, count, extend: false };
            }
            None => {}
        }
        if self.keys.len() == 1 && let Some(target) = map.extending(mode, chord) {
            let count = self.count.take();
            self.keys.clear();
            return Outcome::Run { target, count, extend: true };
        }
        let abandoned = self.keys.len() > 1 || self.count.is_some();
        self.clear();
        if abandoned { Outcome::Cancelled } else { Outcome::Unbound }
    }

    fn feed_held(&mut self, map: &Keymap, mode: Mode, chord: Chord) -> Outcome {
        if !self.keys.is_empty() || self.finished == Some(chord.key) {
            return Outcome::Waiting;
        }
        let (target, extend) = match map.single(mode, chord) {
            Some(target) => (target, false),
            None => match map.extending(mode, chord) {
                Some(target) => (target, true),
                None => return if map.starts_any(mode, chord) { Outcome::Waiting } else { Outcome::Unbound },
            },
        };
        match &target {
            Target::Command(c) if c.info().repeat => Outcome::Run { target, count: None, extend },
            _ => Outcome::Waiting,
        }
    }
}

/// Keyboard input in the order it happened, in one form.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Press { chord: Chord, repeat: bool, text: Option<String> },
    Text(String),
}

/// Turns egui's events into presses and typed text. Copy, Cut and Paste arrive without a
/// key event, and a punctuation mark is identified by the character it produced, since the
/// keys that make it depend on the keyboard layout.
pub fn presses(events: &[Event]) -> Vec<Input> {
    let command = |key| Input::Press { chord: Chord::new(key, Modifiers::COMMAND), repeat: false, text: None };
    let mut out = Vec::new();
    // egui sends a key before the character it typed, so the character can use what the key said.
    let (mut repeating, mut skip_text) = (false, false);
    for event in events {
        match event {
            Event::Copy => out.push(command(Key::C)),
            Event::Cut => out.push(command(Key::X)),
            Event::Paste(_) => out.push(command(Key::V)),
            Event::Key { key, pressed: true, .. } if is_modifier(*key) => {}
            Event::Key { key, pressed: true, repeat, modifiers, .. } => {
                let chord = Chord::new(*key, *modifiers);
                repeating = *repeat;
                skip_text = false;
                if is_punctuation(*key) && !chord.ctrl && !chord.alt { continue; }
                // Alt lets the character through too, and it would count as a second press.
                skip_text = is_punctuation(*key) && chord.alt && !chord.ctrl;
                out.push(Input::Press { chord, repeat: *repeat, text: None });
            }
            Event::Text(_) if std::mem::take(&mut skip_text) => {}
            Event::Text(text) => match Key::from_name(text).filter(|k| is_punctuation(*k)) {
                Some(key) => out.push(Input::Press { chord: Chord::plain(key), repeat: repeating, text: Some(text.clone()) }),
                None => out.push(Input::Text(text.clone())),
            },
            _ => {}
        }
    }
    out
}

/// egui-winit turns every autorepeat of Ctrl+V into another paste without saying it is one,
/// so a paste counts only after the key was let go or the window lost the keyboard.
pub fn drop_held_pastes(events: &mut Vec<Event>, held: &mut bool) {
    events.retain(|event| match event {
        Event::Key { key: Key::V, pressed: false, .. } | Event::WindowFocused(false) => {
            *held = false;
            true
        }
        Event::Paste(_) => !std::mem::replace(held, true),
        _ => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<Chord> {
        parse_keys(text).unwrap_or_else(|| panic!("bad keys {text}"))
    }

    fn run(c: Command) -> Outcome {
        Outcome::Run { target: Target::Command(c), count: None, extend: false }
    }

    fn feed_all(matcher: &mut Matcher, map: &Keymap, mode: Mode, text: &str) -> Vec<Outcome> {
        keys(text).into_iter().map(|c| matcher.feed(map, mode, c, false)).collect()
    }

    fn last(map: &Keymap, mode: Mode, text: &str) -> Outcome {
        feed_all(&mut Matcher::default(), map, mode, text).pop().unwrap()
    }

    #[test]
    fn every_default_shortcut_is_valid_and_ids_are_unique() {
        let mut ids = std::collections::HashSet::new();
        for info in COMMANDS {
            assert!(ids.insert(info.id), "duplicate id {}", info.id);
            for (layer, texts) in [(Layer::Standard, info.standard), (Layer::Neovim, info.neovim)] {
                for text in texts {
                    assert!(KeymapSettings::default().parse(layer, text).is_some(), "{} has bad keys {text}", info.id);
                }
            }
        }
    }

    #[test]
    fn default_layers_have_no_clashes_inside_themselves() {
        let settings = KeymapSettings::default();
        for layer in [Layer::Standard, Layer::Neovim] {
            for info in COMMANDS {
                let own = Target::Command(info.command);
                for k in settings.keys_of(layer, info) {
                    assert_eq!(conflict(&settings, layer, &k, &own), None, "{} clashes", keys_label(&k));
                }
            }
        }
    }

    #[test]
    fn keys_survive_storing_and_parsing() {
        for text in ["ctrl+shift+n", "g h", "ctrl+w h", "shift+tab", "alt+up", "slash", "f10", "space"] {
            assert_eq!(stored_keys(&keys(text)), text);
        }
        assert_eq!(keys("/"), keys("slash"));
        assert_eq!(keys("CTRL+T"), keys("ctrl+t"));
        assert!(parse_keys("ctrl+nothing").is_none());
        assert!(parse_keys("a b c d e").is_none(), "too long");
        assert!(parse_keys("").is_none());
    }

    #[test]
    fn labels_read_like_shortcuts() {
        assert_eq!(keys_label(&keys("ctrl+shift+n")), "Ctrl+Shift+N");
        assert_eq!(keys_label(&keys("g h")), "g h");
        assert_eq!(keys_label(&keys("shift+g")), "Shift+G");
        assert_eq!(keys_label(&keys("ctrl+w h")), "Ctrl+W h");
        assert_eq!(keys_label(&keys("alt+up")), "Alt+Up");
        assert_eq!(keys_label(&keys("/")), "/");
        assert_eq!(keys_label(&keys("space")), "Space");
    }

    #[test]
    fn neovim_layer_covers_standard_shortcuts_only_when_on() {
        let map = Keymap::default_for_tests();
        assert_eq!(last(&map, Mode::Standard, "ctrl+d"), run(Command::Duplicate));
        assert_eq!(last(&map, Mode::Normal, "ctrl+d"), run(Command::HalfPageDown));
        assert_eq!(last(&map, Mode::Standard, "ctrl+w"), run(Command::CloseTab));
        assert_eq!(last(&map, Mode::Normal, "ctrl+w"), Outcome::Waiting);
        assert_eq!(last(&map, Mode::Normal, "ctrl+t"), run(Command::NewTab), "what Neovim leaves alone still works");
        assert_eq!(last(&map, Mode::Standard, "j"), Outcome::Unbound);
    }

    #[test]
    fn sequences_wait_finish_and_cancel() {
        let map = Keymap::default_for_tests();
        let mut m = Matcher::default();
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "g"), [Outcome::Waiting]);
        assert_eq!(m.pending(), "g");
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "h"), [run(Command::GoHome)]);
        assert_eq!(m.pending(), "");
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "g z j"), [Outcome::Waiting, Outcome::Cancelled, run(Command::CursorDown)]);
        assert_eq!(last(&map, Mode::Normal, "ctrl+w q"), run(Command::CloseTab));
    }

    #[test]
    fn a_second_key_may_keep_ctrl_held_as_in_neovim() {
        let map = Keymap::default_for_tests();
        assert_eq!(last(&map, Mode::Normal, "ctrl+w ctrl+l"), run(Command::FocusRight));
        assert_eq!(last(&map, Mode::Normal, "ctrl+w ctrl+w"), run(Command::NextPanel));
        assert_eq!(last(&map, Mode::Normal, "ctrl+w ctrl+x"), Outcome::Cancelled);
    }

    fn hints(map: &Keymap, mode: Mode, prefix: &str) -> Vec<(String, String)> {
        map.hints(mode, &keys(prefix)).into_iter().map(|h| (h.key.label(), h.label)).collect()
    }

    #[test]
    fn hints_list_what_can_follow_in_key_order() {
        let map = Keymap::default_for_tests();
        assert_eq!(hints(&map, Mode::Normal, "g"), [("g".to_string(), "First item".to_string()), ("h".to_string(), "Home folder".to_string())]);
        let window: Vec<String> = hints(&map, Mode::Normal, "ctrl+w").into_iter().map(|h| h.0).collect();
        assert_eq!(window, ["c", "h", "j", "k", "l", "q", "w"]);
        assert_eq!(hints(&map, Mode::Normal, "y"), [("y".to_string(), "Copy".to_string())]);
        assert!(hints(&map, Mode::Normal, "j").is_empty(), "a finished shortcut has no next key");
        assert!(hints(&map, Mode::Standard, "ctrl+w").is_empty(), "there it closes the tab at once");
    }

    #[test]
    fn hints_group_longer_shortcuts_and_name_folders_briefly() {
        let mut settings = KeymapSettings::default();
        settings.neovim.folders.push(FolderKey { keys: "g p".into(), path: PathBuf::from("/home/me/Projects"), new_tab: false });
        settings.neovim.folders.push(FolderKey { keys: "g x a".into(), path: PathBuf::from("/a"), new_tab: false });
        settings.neovim.folders.push(FolderKey { keys: "g x b".into(), path: PathBuf::from("/b"), new_tab: true });
        let map = Keymap::new(&settings);
        let got = hints(&map, Mode::Normal, "g");
        assert!(got.contains(&("p".to_string(), "Go to Projects".to_string())), "{got:?}");
        assert!(got.contains(&("x".to_string(), "+2 more".to_string())), "{got:?}");
        assert_eq!(hints(&map, Mode::Normal, "g x"), [("a".to_string(), "Go to a".to_string()), ("b".to_string(), "b in a new tab".to_string())]);
    }

    #[test]
    fn visual_mode_shortens_copy_and_cut() {
        let map = Keymap::default_for_tests();
        assert_eq!(last(&map, Mode::Visual, "y"), run(Command::VisualCopy));
        assert_eq!(last(&map, Mode::Visual, "d"), run(Command::VisualCut));
        assert_eq!(last(&map, Mode::Normal, "y"), Outcome::Waiting);
        assert_eq!(last(&map, Mode::Normal, "y y"), run(Command::Copy));
    }

    #[test]
    fn counts_multiply_only_in_neovim_mode() {
        let map = Keymap::default_for_tests();
        let mut m = Matcher::default();
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "1 2"), [Outcome::Waiting, Outcome::Waiting]);
        assert_eq!(m.pending(), "12");
        let down = Outcome::Run { target: Target::Command(Command::CursorDown), count: Some(12), extend: false };
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "j"), [down]);
        assert_eq!(last(&map, Mode::Normal, "0"), Outcome::Unbound, "a lone zero is no count");
        assert_eq!(last(&map, Mode::Standard, "5"), Outcome::Unbound, "digits filter in the standard layer");
        assert_eq!(last(&map, Mode::Normal, "5 x"), Outcome::Cancelled);
    }

    #[test]
    fn shift_extends_only_movement_by_non_letter_keys() {
        let map = Keymap::default_for_tests();
        let extended = |c| Outcome::Run { target: Target::Command(c), count: None, extend: true };
        assert_eq!(last(&map, Mode::Standard, "shift+down"), extended(Command::CursorDown));
        assert_eq!(last(&map, Mode::Standard, "shift+end"), extended(Command::Last));
        assert_eq!(last(&map, Mode::Standard, "shift+enter"), Outcome::Unbound);
        assert_eq!(last(&map, Mode::Normal, "shift+j"), Outcome::Unbound, "letters do not extend");
    }

    #[test]
    fn held_keys_repeat_only_what_may_repeat() {
        let map = Keymap::default_for_tests();
        let held = |text: &str, mode| Matcher::default().feed(&map, mode, keys(text)[0], true);
        assert_eq!(held("j", Mode::Normal), run(Command::CursorDown));
        assert_eq!(held("ctrl+d", Mode::Normal), run(Command::HalfPageDown));
        assert_eq!(held("ctrl+d", Mode::Standard), Outcome::Waiting, "held duplicate must not act again");
        assert_eq!(held("delete", Mode::Standard), Outcome::Waiting);
        assert_eq!(held("enter", Mode::Standard), Outcome::Waiting);
        assert_eq!(held("g", Mode::Normal), Outcome::Waiting, "a held key starts no sequence");
        assert_eq!(held("z", Mode::Normal), Outcome::Unbound);
    }

    #[test]
    fn changes_replace_defaults_and_can_unbind() {
        let mut settings = KeymapSettings::default();
        let info = Command::NewTab.info();
        settings.set_keys(Layer::Standard, info, &[keys("ctrl+y"), keys("g y")]);
        assert_eq!(settings.standard.bindings["new_tab"], "ctrl+y, g y");
        let map = Keymap::new(&settings);
        assert_eq!(last(&map, Mode::Standard, "ctrl+y"), run(Command::NewTab));
        assert_eq!(last(&map, Mode::Standard, "g y"), run(Command::NewTab));
        assert_eq!(last(&map, Mode::Standard, "ctrl+t"), Outcome::Unbound);

        settings.set_keys(Layer::Standard, info, &[]);
        assert_eq!(settings.standard.bindings["new_tab"], "");
        assert!(settings.keys_of(Layer::Standard, info).is_empty());

        settings.set_keys(Layer::Standard, info, &[keys("ctrl+t")]);
        assert!(settings.standard.bindings.is_empty(), "defaults are not stored");
    }

    #[test]
    fn unknown_commands_are_kept_for_other_versions() {
        let mut settings = KeymapSettings::default();
        settings.standard.bindings.insert("from_the_future".into(), "ctrl+y".into());
        settings.set_keys(Layer::Standard, Command::NewTab.info(), &[keys("ctrl+u")]);
        assert_eq!(settings.standard.bindings["from_the_future"], "ctrl+y");
        assert_eq!(last(&Keymap::new(&settings), Mode::Standard, "ctrl+y"), Outcome::Unbound);
    }

    #[test]
    fn user_folders_open_from_their_own_sequences() {
        let mut settings = KeymapSettings::default();
        settings.neovim.folders.push(FolderKey { keys: "g p".into(), path: PathBuf::from("/projects"), new_tab: true });
        let map = Keymap::new(&settings);
        let expected = Outcome::Run { target: Target::Folder { path: PathBuf::from("/projects"), new_tab: true }, count: None, extend: false };
        assert_eq!(last(&map, Mode::Normal, "g p"), expected);
    }

    #[test]
    fn conflicts_find_equal_keys_and_shared_starts() {
        let settings = KeymapSettings::default();
        let own = Target::Command(Command::Refresh);
        assert_eq!(conflict(&settings, Layer::Standard, &keys("ctrl+t"), &own), Some(Target::Command(Command::NewTab)));
        assert_eq!(conflict(&settings, Layer::Neovim, &keys("g"), &own), Some(Target::Command(Command::First)), "g starts g g");
        assert_eq!(conflict(&settings, Layer::Neovim, &keys("j x"), &own), Some(Target::Command(Command::CursorDown)), "j would end it early");
        assert_eq!(conflict(&settings, Layer::Neovim, &keys("g x"), &own), None);
        assert_eq!(conflict(&settings, Layer::Standard, &keys("f5"), &own), None, "a command does not clash with itself");
        let visual = Target::Command(Command::VisualCopy);
        assert_eq!(conflict(&settings, Layer::Neovim, &keys("y"), &visual), None, "visual shortcuts are apart");
    }

    #[test]
    fn replacing_takes_the_keys_from_their_old_owner() {
        let mut settings = KeymapSettings::default();
        let refresh = Target::Command(Command::Refresh);
        settings.clear_conflicts(Layer::Standard, &keys("ctrl+t"), &refresh);
        settings.assign(Layer::Standard, &refresh, keys("ctrl+t"));
        assert!(settings.keys_of(Layer::Standard, Command::NewTab.info()).is_empty());
        assert_eq!(settings.standard.bindings["refresh"], "f5, ctrl+t");
        let map = Keymap::new(&settings);
        assert_eq!(last(&map, Mode::Standard, "ctrl+t"), run(Command::Refresh));
        assert_eq!(conflict(&settings, Layer::Standard, &keys("ctrl+t"), &refresh), None);
    }

    #[test]
    fn a_folder_has_one_shortcut_and_replacing_can_remove_it() {
        let mut settings = KeymapSettings::default();
        let folder = Target::Folder { path: PathBuf::from("/projects"), new_tab: false };
        settings.assign(Layer::Neovim, &folder, keys("g p"));
        settings.assign(Layer::Neovim, &folder, keys("g o"));
        assert_eq!(settings.neovim.folders, [FolderKey { keys: "g o".into(), path: "/projects".into(), new_tab: false }]);
        let down = Target::Command(Command::CursorDown);
        settings.clear_conflicts(Layer::Neovim, &keys("g o"), &down);
        assert!(settings.neovim.folders.is_empty());
    }

    #[test]
    fn labels_skip_shortcuts_another_layer_covers() {
        let map = Keymap::default_for_tests();
        assert_eq!(map.label(Mode::Standard, Command::Duplicate).as_deref(), Some("Ctrl+D"));
        assert_eq!(map.label(Mode::Normal, Command::Duplicate).as_deref(), Some("Ctrl+B f c"));
        let mut settings = KeymapSettings::default();
        settings.set_keys(Layer::Neovim, Command::Duplicate.info(), &[]);
        assert_eq!(Keymap::new(&settings).label(Mode::Normal, Command::Duplicate), None, "Ctrl+D scrolls here");
        assert_eq!(map.label(Mode::Normal, Command::Refresh).as_deref(), Some("F5"));
        assert_eq!(map.label(Mode::Normal, Command::CloseTab).as_deref(), Some("Ctrl+B b d"));
    }

    #[test]
    fn holding_the_last_key_of_a_sequence_does_not_run_that_key_alone() {
        let map = Keymap::default_for_tests();
        let mut m = Matcher::default();
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "ctrl+b w h").pop().unwrap(), run(Command::FocusLeft));
        let h = keys("h")[0];
        assert_eq!(m.feed(&map, Mode::Normal, h, true), Outcome::Waiting);
        assert_eq!(m.feed(&map, Mode::Normal, keys("j")[0], true), run(Command::CursorDown), "other keys are unaffected");
        assert_eq!(m.feed(&map, Mode::Normal, h, false), run(Command::FolderOut), "a new press counts");
        assert_eq!(m.feed(&map, Mode::Normal, h, true), run(Command::FolderOut), "and that one may repeat");
    }

    #[test]
    fn escape_after_a_count_only_takes_back_the_count() {
        let map = Keymap::default_for_tests();
        let mut m = Matcher::default();
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "5 escape"), [Outcome::Waiting, Outcome::Cancelled]);
        assert_eq!(m.pending(), "");
        assert_eq!(last(&map, Mode::Normal, "escape"), run(Command::Escape));
    }

    #[test]
    fn a_longer_shortcut_of_the_same_command_is_a_conflict_to_confirm() {
        let mut settings = KeymapSettings::default();
        let close = Target::Command(Command::CloseTab);
        assert_eq!(conflict(&settings, Layer::Standard, &keys("ctrl+w x"), &close), Some(close.clone()));
        settings.clear_conflicts(Layer::Standard, &keys("ctrl+w x"), &close);
        settings.assign(Layer::Standard, &close, keys("ctrl+w x"));
        assert_eq!(settings.keys_of(Layer::Standard, Command::CloseTab.info()), [keys("ctrl+w x")]);
        assert_eq!(conflict(&settings, Layer::Standard, &keys("ctrl+w x"), &close), None, "the very same keys are no clash");
    }

    #[test]
    fn the_new_neovim_defaults_work() {
        let map = Keymap::default_for_tests();
        assert_eq!(last(&map, Mode::Normal, "ctrl+b f x"), run(Command::Delete));
        assert_eq!(last(&map, Mode::Normal, "ctrl+o"), run(Command::Back));
        assert_eq!(last(&map, Mode::Normal, "ctrl+i"), run(Command::Forward));
    }

    #[test]
    fn the_leader_starts_named_groups_and_does_not_need_ctrl_held() {
        let map = Keymap::default_for_tests();
        assert_eq!(last(&map, Mode::Normal, "ctrl+b b d"), run(Command::CloseTab));
        assert_eq!(last(&map, Mode::Normal, "ctrl+b ctrl+b ctrl+d"), run(Command::CloseTab), "with Ctrl still held");
        assert_eq!(last(&map, Mode::Normal, "ctrl+b z"), Outcome::Cancelled);
        assert_eq!(last(&map, Mode::Standard, "ctrl+b"), Outcome::Unbound, "only in the Neovim layer");
        assert_eq!(hints(&map, Mode::Normal, "ctrl+b"), [
            ("b".to_string(), "+Tabs".to_string()), ("f".to_string(), "+Files".to_string()),
            ("u".to_string(), "+View".to_string()), ("w".to_string(), "+Panels".to_string()),
        ]);
    }

    #[test]
    fn changing_the_leader_moves_every_shortcut_built_on_it() {
        let mut settings = KeymapSettings::default();
        let refresh = Target::Command(Command::Refresh);
        settings.assign(Layer::Neovim, &refresh, keys("ctrl+b r"));
        assert_eq!(settings.neovim.bindings["refresh"], "leader r");
        let folder = Target::Folder { path: PathBuf::from("/projects"), new_tab: false };
        settings.assign(Layer::Neovim, &folder, keys("ctrl+b o"));
        assert_eq!(settings.neovim.folders[0].keys, "leader o");

        settings.set_leader(keys("space")[0]);
        let map = Keymap::new(&settings);
        assert_eq!(last(&map, Mode::Normal, "space b d"), run(Command::CloseTab));
        assert_eq!(last(&map, Mode::Normal, "space r"), run(Command::Refresh));
        assert!(matches!(last(&map, Mode::Normal, "space o"), Outcome::Run { target: Target::Folder { .. }, .. }));
        assert_eq!(last(&map, Mode::Normal, "ctrl+b"), Outcome::Unbound, "the old key is free again");
        assert!(settings.neovim.bindings.get("close_tab").is_none(), "defaults stay unstored");
    }

    #[test]
    fn a_new_leader_replaces_only_the_shortcuts_in_its_way() {
        let mut settings = KeymapSettings::default();
        assert_eq!(settings.leader_clash(keys("space")[0]), None, "Space belongs to the standard layer");
        assert_eq!(settings.leader_clash(keys("ctrl+b")[0]), None, "its own sequences are no clash");
        let g = keys("g")[0];
        assert_eq!(settings.leader_clash(g), Some(Target::Command(Command::First)));
        settings.set_leader(g);
        let map = Keymap::new(&settings);
        assert_eq!(last(&map, Mode::Normal, "g b d"), run(Command::CloseTab));
        assert!(settings.keys_of(Layer::Neovim, Command::GoHome.info()).is_empty());
    }

    #[test]
    fn resetting_the_neovim_layer_restores_the_leader() {
        let mut settings = KeymapSettings::default();
        settings.set_leader(keys("space")[0]);
        settings.reset_layer(Layer::Standard);
        assert_eq!(settings.leader, "space");
        settings.reset_layer(Layer::Neovim);
        assert_eq!(settings, KeymapSettings::default());
    }

    fn key_event(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    #[test]
    fn clipboard_events_become_their_shortcuts() {
        let got = presses(&[Event::Copy, Event::Cut, Event::Paste("x".into())]);
        let want: Vec<Input> = ["ctrl+c", "ctrl+x", "ctrl+v"].iter().map(|t| Input::Press { chord: keys(t)[0], repeat: false, text: None }).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn punctuation_is_read_from_the_character_it_typed() {
        // On a German layout / is Shift+7, so the key event says nothing useful.
        let got = presses(&[key_event(Key::Slash, Modifiers::SHIFT), Event::Text("/".into())]);
        assert_eq!(got, [Input::Press { chord: keys("/")[0], repeat: false, text: Some("/".into()) }]);
        let with_ctrl = presses(&[key_event(Key::Slash, Modifiers::COMMAND)]);
        assert_eq!(with_ctrl, [Input::Press { chord: keys("ctrl+/")[0], repeat: false, text: None }]);
    }

    #[test]
    fn modifiers_alone_are_not_presses_and_old_recordings_recover() {
        let got = presses(&[key_event(Key::AltLeft, Modifiers::ALT), key_event(Key::L, Modifiers::ALT)]);
        assert_eq!(got, [Input::Press { chord: keys("alt+l")[0], repeat: false, text: None }]);
        assert_eq!(keys("altleft alt+l"), keys("alt+l"));
        assert!(parse_keys("controlleft").is_none());
    }

    #[test]
    fn pressing_ctrl_again_inside_a_sequence_does_not_abandon_it() {
        let map = Keymap::default_for_tests();
        let mut m = Matcher::default();
        assert_eq!(feed_all(&mut m, &map, Mode::Normal, "ctrl+w"), [Outcome::Waiting]);
        for input in presses(&[key_event(Key::ControlLeft, Modifiers::COMMAND), key_event(Key::H, Modifiers::COMMAND)]) {
            let Input::Press { chord, repeat, .. } = input else { panic!("a press") };
            assert_eq!(m.feed(&map, Mode::Normal, chord, repeat), run(Command::FocusLeft));
        }
    }

    #[test]
    fn a_held_punctuation_key_is_a_repeat() {
        let held = Event::Key { key: Key::Space, physical_key: None, pressed: true, repeat: true, modifiers: Modifiers::NONE };
        assert_eq!(presses(&[held, Event::Text(" ".into())]), [Input::Press { chord: keys("space")[0], repeat: true, text: Some(" ".into()) }]);
    }

    #[test]
    fn alt_and_punctuation_is_one_press() {
        let got = presses(&[key_event(Key::Slash, Modifiers::ALT), Event::Text("/".into())]);
        assert_eq!(got, [Input::Press { chord: keys("alt+/")[0], repeat: false, text: None }]);
    }

    #[test]
    fn a_paste_counts_once_until_the_key_is_released() {
        let paste = || Event::Paste("x".into());
        let release = Event::Key { key: Key::V, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE };
        let mut held = false;
        let mut run = |mut events: Vec<Event>| { drop_held_pastes(&mut events, &mut held); events.iter().filter(|e| matches!(e, Event::Paste(_))).count() };
        assert_eq!(run(vec![paste(), paste()]), 1);
        assert_eq!(run(vec![paste()]), 0, "still held");
        assert_eq!(run(vec![release, paste()]), 1);
        assert_eq!(run(vec![Event::WindowFocused(false), paste()]), 1, "focus loss forgets the key");
    }

    #[test]
    fn letters_stay_a_press_and_then_text() {
        let got = presses(&[key_event(Key::G, Modifiers::NONE), Event::Text("g".into())]);
        assert_eq!(got, [Input::Press { chord: keys("g")[0], repeat: false, text: None }, Input::Text("g".into())]);
    }

    impl Keymap {
        fn default_for_tests() -> Keymap {
            Keymap::new(&KeymapSettings::default())
        }
    }
}
