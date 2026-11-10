//! The quick palette as data: what can be picked, and how typed text ranks it.
//! Nothing here draws or acts, so all of it can be tested without a window.

use std::path::PathBuf;

use crate::fsx::SortBy;
use crate::keymap::Command;
use crate::theme::ThemeName;

/// What the palette searches in. The first character of the field's text chooses it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Folders and files to go to.
    Places,
    /// Commands and quick settings.
    Commands,
    /// Folders visited before, the latest first.
    History,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Places, Mode::Commands, Mode::History];

    pub fn prefix(self) -> &'static str {
        match self {
            Mode::Places => "",
            Mode::Commands => ">",
            Mode::History => "@",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Places => "Go to",
            Mode::Commands => "Commands",
            Mode::History => "History",
        }
    }

    /// What stands for the mode beside the field. See `icons::face`.
    pub fn face(self) -> &'static str {
        match self {
            Mode::Places => "🔍",
            Mode::Commands => "#next",
            Mode::History => "#clock",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Mode::Places => "Go to a folder or file. Type > for commands, @ for history",
            Mode::Commands => "Run a command or change a setting",
            Mode::History => "Go back to a folder you visited",
        }
    }

    /// The mode a press of Tab leads to.
    pub fn next(self, back: bool) -> Mode {
        let at = Mode::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Mode::ALL[(at + if back { Mode::ALL.len() - 1 } else { 1 }) % Mode::ALL.len()]
    }
}

/// The field's text as its mode and what is searched for.
pub fn split(text: &str) -> (Mode, &str) {
    let text = text.trim_start();
    for mode in [Mode::Commands, Mode::History] {
        if let Some(rest) = text.strip_prefix(mode.prefix()) {
            return (mode, rest.trim());
        }
    }
    (Mode::Places, text.trim())
}

/// A setting the palette changes itself, because no shortcut command does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    Extensions, AgeColors, Mebibytes, FoldersOnTop, Sidebar, PreviewPane, Tree, ColumnsLayout,
    NeovimKeys, WhichKey, HistoryCard, FontLarger, FontSmaller,
    OpenSettings, OpenShortcuts, ConfigFolder,
}

/// What choosing an item does.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Folder(PathBuf),
    File(PathBuf),
    /// A path as it was typed, not yet known to exist.
    Typed(String),
    Command(Command),
    Setting(Setting),
    Theme(ThemeName),
    Sort(SortBy),
}

impl Pick {
    /// Names the item among the recently used ones, so it must never change.
    pub fn id(&self) -> Option<String> {
        Some(match self {
            Pick::Command(command) => command.info().id.to_string(),
            Pick::Setting(setting) => format!("setting:{setting:?}"),
            Pick::Theme(theme) => format!("theme:{theme:?}"),
            Pick::Sort(by) => format!("sort:{by:?}"),
            Pick::Folder(_) | Pick::File(_) | Pick::Typed(_) => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub title: String,
    /// Said more quietly beside the title: where a file is, or what a command belongs to.
    pub detail: String,
    /// At the right edge: a shortcut, or where the item comes from.
    pub badge: String,
    /// For a setting that is on or off.
    pub state: Option<bool>,
    pub pick: Pick,
    /// Added to the score, so that a likelier source wins among equal matches.
    pub boost: i32,
    title_lower: Box<[char]>,
    detail_lower: Box<[char]>,
}

/// One lower-case character for each character, so positions stay those of the text shown.
fn lower(text: &str) -> Box<[char]> {
    text.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

impl Item {
    pub fn new(title: impl Into<String>, detail: impl Into<String>, badge: impl Into<String>, pick: Pick) -> Item {
        let (title, detail) = (title.into(), detail.into());
        Item { title_lower: lower(&title), detail_lower: lower(&detail), title, detail, badge: badge.into(), state: None, pick, boost: 0 }
    }

    pub fn state(mut self, on: bool) -> Item {
        self.state = Some(on);
        self
    }

    pub fn boost(mut self, boost: i32) -> Item {
        self.boost = boost;
        self
    }
}

/// An item that matches what was typed.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Index into the items that were ranked.
    pub item: usize,
    pub score: i32,
    /// The characters of the title and of the detail that matched, for marking them.
    pub title: Vec<usize>,
    pub detail: Vec<usize>,
}

fn starts_word(text: &[char], at: usize) -> bool {
    at == 0 || !text[at - 1].is_alphanumeric()
}

/// Long names say less about what was typed.
fn length_cost(text: &[char]) -> i32 {
    text.len().min(120) as i32 / 4
}

/// `word` as one unbroken run in `text`: best at the very start, then at the start of a word.
fn find_run(word: &[char], text: &[char]) -> Option<(i32, Vec<usize>)> {
    if word.is_empty() || word.len() > text.len() { return None; }
    let mut best: Option<(i32, usize)> = None;
    for at in 0..=text.len() - word.len() {
        if text[at..at + word.len()] != *word { continue; }
        let place = if at == 0 { 60 } else if starts_word(text, at) { 40 } else { 0 };
        if best.is_none_or(|(score, _)| place > score) { best = Some((place, at)); }
        if place == 60 { break; }
    }
    let (place, at) = best?;
    let whole = if word.len() == text.len() { 50 } else { 0 };
    Some((200 + place + whole - length_cost(text), (at..at + word.len()).collect()))
}

/// The letters of `word` in order but apart, as close together as they come.
fn find_scattered(word: &[char], text: &[char]) -> Option<(i32, Vec<usize>)> {
    let mut next = 0;
    let end = text.iter().position(|c| {
        if *c == word[next] { next += 1; }
        next == word.len()
    })?;
    // Back from the end: the latest start gives the shortest stretch that still matches.
    let mut places = vec![0; word.len()];
    let (mut left, mut at) = (word.len(), end + 1);
    while left > 0 {
        at -= 1;
        if text[at] == word[left - 1] {
            left -= 1;
            places[left] = at;
        }
    }
    let mut score = -length_cost(text);
    for (n, &place) in places.iter().enumerate() {
        score += 10 + if starts_word(text, place) { 15 } else { 0 };
        if n > 0 {
            let gap = place - places[n - 1] - 1;
            score += if gap == 0 { 12 } else { -(gap.min(8) as i32) };
        }
    }
    Some((score, places))
}

fn find(word: &[char], text: &[char]) -> Option<(i32, Vec<usize>)> {
    if word.is_empty() || word.len() > text.len() { return None; }
    find_run(word, text).or_else(|| find_scattered(word, text))
}

/// Every word must be found: in the title, or failing that as a run in the detail.
fn matched(item: &Item, words: &[Box<[char]>]) -> Option<(i32, Vec<usize>, Vec<usize>)> {
    let (mut score, mut title, mut detail) = (item.boost, Vec::new(), Vec::new());
    for word in words {
        if let Some((points, places)) = find(word, &item.title_lower) {
            score += points * 2;
            title.extend(places);
        } else {
            let (points, places) = find_run(word, &item.detail_lower)?;
            score += points / 2;
            detail.extend(places);
        }
    }
    Some((score, title, detail))
}

/// The best `limit` items for the typed text, best first. Items that match equally well keep
/// the order they came in, and with nothing typed they are simply the first ones.
pub fn rank(items: &[Item], text: &str, limit: usize) -> Vec<Hit> {
    let words: Vec<Box<[char]>> = text.split_whitespace().map(lower).collect();
    if words.is_empty() {
        return (0..items.len().min(limit)).map(|item| Hit { item, score: 0, title: Vec::new(), detail: Vec::new() }).collect();
    }
    let mut hits: Vec<Hit> = items.iter().enumerate()
        .filter_map(|(item, it)| matched(it, &words).map(|(score, title, detail)| Hit { item, score, title, detail }))
        .collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, parent: &str) -> Item {
        Item::new(name, parent, "", Pick::Folder(PathBuf::from(parent).join(name)))
    }

    fn titles(items: &[Item], text: &str) -> Vec<String> {
        rank(items, text, 50).into_iter().map(|hit| items[hit.item].title.clone()).collect()
    }

    #[test]
    fn the_first_character_chooses_the_mode() {
        assert_eq!(split("docs"), (Mode::Places, "docs"));
        assert_eq!(split("> new tab "), (Mode::Commands, "new tab"));
        assert_eq!(split("@proj"), (Mode::History, "proj"));
        assert_eq!(split("  >"), (Mode::Commands, ""));
        assert_eq!(split(""), (Mode::Places, ""));
        assert_eq!(split("a > b"), (Mode::Places, "a > b"), "only the first character counts");
    }

    #[test]
    fn tab_goes_round_the_modes_both_ways() {
        assert_eq!(Mode::Places.next(false), Mode::Commands);
        assert_eq!(Mode::History.next(false), Mode::Places);
        assert_eq!(Mode::Places.next(true), Mode::History);
    }

    #[test]
    fn nothing_typed_keeps_the_given_order() {
        let items = [folder("zeta", "/a"), folder("alpha", "/a"), folder("mid", "/a")];
        assert_eq!(titles(&items, ""), ["zeta", "alpha", "mid"]);
        assert_eq!(rank(&items, "  ", 2).len(), 2, "the limit holds");
    }

    #[test]
    fn a_run_at_the_start_beats_one_inside_and_scattered_letters() {
        let items = [folder("my documents", "/a"), folder("dxoxc", "/a"), folder("documents", "/a"), folder("old-docs", "/a")];
        assert_eq!(titles(&items, "doc"), ["documents", "old-docs", "my documents", "dxoxc"]);
    }

    #[test]
    fn letters_may_be_apart_but_must_keep_their_order() {
        let items = [folder("ferrous-commander", "/dev"), folder("commander-ferrous", "/dev")];
        assert_eq!(titles(&items, "fcom"), ["ferrous-commander"]);
        assert!(titles(&items, "xyz").is_empty());
    }

    #[test]
    fn every_word_must_match_and_the_folder_counts_too() {
        let items = [folder("src", "/home/me/ferrous"), folder("src", "/home/me/other"), folder("ferrous notes", "/tmp")];
        assert_eq!(titles(&items, "src ferrous"), ["src"]);
        let hit = &rank(&items, "src ferrous", 5)[0];
        assert_eq!((hit.item, hit.title.as_slice()), (0, &[0, 1, 2][..]));
        assert_eq!(hit.detail, (9..16).collect::<Vec<_>>(), "the run in the folder is marked");
        assert!(titles(&items, "src fro").is_empty(), "in the folder the letters must be together");
    }

    #[test]
    fn case_and_accents_of_the_alphabet_do_not_stop_a_match() {
        let items = [folder("Zażółć", "/a"), folder("README", "/a")];
        assert_eq!(titles(&items, "ŻÓŁ"), ["Zażółć"]);
        assert_eq!(rank(&items, "żół", 5)[0].title, [2, 3, 4]);
        assert_eq!(titles(&items, "readme"), ["README"]);
    }

    #[test]
    fn scattered_letters_are_marked_where_they_are_closest() {
        let items = [folder("a-b-ab", "/x")];
        assert_eq!(rank(&items, "ab", 5)[0].title, [4, 5], "the run wins");
        let items = [folder("aaxb", "/x")];
        assert_eq!(rank(&items, "ab", 5)[0].title, [1, 3], "the latest start");
    }

    #[test]
    fn a_likelier_source_wins_among_equal_matches_only() {
        let items = [folder("notes", "/a"), folder("notes", "/b").boost(30), folder("notes-old", "/c").boost(5)];
        let got: Vec<usize> = rank(&items, "notes", 5).into_iter().map(|h| h.item).collect();
        assert_eq!(got, [1, 0, 2]);
    }

    #[test]
    fn recent_ids_exist_only_for_commands_and_settings() {
        assert_eq!(Pick::Command(Command::NewTab).id().as_deref(), Some("new_tab"));
        assert_eq!(Pick::Setting(Setting::Sidebar).id().as_deref(), Some("setting:Sidebar"));
        assert_eq!(Pick::Theme(ThemeName::Gruvbox).id().as_deref(), Some("theme:Gruvbox"));
        assert_eq!(Pick::Folder("/a".into()).id(), None);
    }
}
