//! Text chosen with the mouse in the preview window, and what of it goes to the clipboard.
//!
//! Views draw only what is on screen, so a selection is kept as places in the content
//! itself and not in what was drawn: it stays when scrolled away and copies all of it.

use egui::text::CharIndex;
use egui::{Galley, Rect};
use serde::{Deserialize, Serialize};

use super::load::{Block, BlockKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Selection {
    #[default]
    None,
    /// Byte offsets into the shown text.
    Text { anchor: u32, head: u32 },
    /// Block and byte offset in its text.
    Doc { anchor: (u32, u32), head: (u32, u32) },
    /// Opposite corners, as row and column.
    Cells { anchor: (u32, u32), head: (u32, u32) },
    /// Places between the characters of one page, as indices into its `PageText::chars`.
    Page { page: u32, anchor: u32, head: u32 },
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        match *self {
            Selection::None => true,
            Selection::Text { anchor, head } | Selection::Page { anchor, head, .. } => anchor == head,
            Selection::Doc { anchor, head } => anchor == head,
            Selection::Cells { .. } => false,
        }
    }
}

/// The text of a PDF page, with where each of its characters is drawn.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PageText {
    pub text: String,
    /// Where in `text` each drawn character starts, and its box as parts of the page:
    /// left, top, right, bottom, from 0 to 1. Spaces and line ends put in between have none.
    pub chars: Vec<(u32, [f32; 4])>,
}

impl PageText {
    pub fn bytes(&self) -> usize {
        self.text.len() + self.chars.len() * 20
    }

    /// The byte where the place before character `i` is.
    fn byte(&self, i: u32) -> usize {
        self.chars.get(i as usize).map_or(self.text.len(), |c| c.0 as usize).min(self.text.len())
    }
}

pub fn ordered<T: Ord>(a: T, b: T) -> (T, T) {
    if a <= b { (a, b) } else { (b, a) }
}

fn boundary(text: &str, at: usize) -> usize {
    text.floor_char_boundary(at.min(text.len()))
}

/// `text[from..to]`, with the ends in either order and kept on characters.
pub fn text_between(text: &str, anchor: u32, head: u32) -> Option<String> {
    let (a, b) = ordered(boundary(text, anchor as usize), boundary(text, head as usize));
    (a < b).then(|| text[a..b].to_owned())
}

/// Paragraphs apart by an empty line, list items and code lines by a line end.
pub fn blocks_between(blocks: &[Block], anchor: (u32, u32), head: (u32, u32)) -> Option<String> {
    let (a, b) = ordered(anchor, head);
    if a == b || blocks.is_empty() { return None; }
    let last = (b.0 as usize).min(blocks.len() - 1);
    let mut out = String::new();
    let mut previous: Option<BlockKind> = None;
    for (i, block) in blocks.iter().enumerate().take(last + 1).skip(a.0 as usize) {
        if block.kind == BlockKind::Rule { continue; }
        let from = if i == a.0 as usize { boundary(&block.text, a.1 as usize) } else { 0 };
        let to = if i == b.0 as usize { boundary(&block.text, b.1 as usize) } else { block.text.len() };
        if let Some(kind) = previous {
            let close = kind == block.kind && matches!(kind, BlockKind::Item | BlockKind::Code);
            out.push_str(if close { "\n" } else { "\n\n" });
        }
        out.push_str(&block.text[from..to.max(from)]);
        previous = Some(block.kind);
    }
    (!out.is_empty()).then_some(out)
}

/// Rows by line ends and cells by tabs, as spreadsheets paste it.
pub fn cells_between(rows: &[Vec<String>], anchor: (u32, u32), head: (u32, u32)) -> Option<String> {
    let (top, bottom) = ordered(anchor.0, head.0);
    let (left, right) = ordered(anchor.1, head.1);
    let quoted = |cell: &str| if cell.contains(['\t', '\n', '\r', '"']) { format!("\"{}\"", cell.replace('"', "\"\"")) } else { cell.to_owned() };
    let lines: Vec<String> = rows.iter().take(bottom as usize + 1).skip(top as usize).map(|row| {
        (left..=right).map(|c| row.get(c as usize).map_or(String::new(), |cell| quoted(cell))).collect::<Vec<_>>().join("\t")
    }).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub fn page_between(page: &PageText, anchor: u32, head: u32) -> Option<String> {
    let (a, b) = ordered(anchor, head);
    let (from, to) = (page.byte(a), page.byte(b));
    // Up to the last chosen character, not into the line end after it.
    let to = if b as usize >= page.chars.len() { page.text.len() } else { to };
    let text = page.text.get(from..to)?.trim_end();
    (!text.is_empty()).then(|| text.to_owned())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word, run of spaces or single mark around byte `at`, as a double click chooses it.
pub fn word_at(text: &str, at: usize) -> (usize, usize) {
    let at = boundary(text, at);
    let Some(c) = text[at..].chars().next() else { return (at, at) };
    let same = |d: char| if is_word(c) { is_word(d) } else if c.is_whitespace() { d.is_whitespace() && d != '\n' } else { d == c };
    let start = text[..at].char_indices().rev().take_while(|&(_, d)| same(d)).last().map_or(at, |(i, _)| i);
    let end = text[at..].char_indices().find(|&(_, d)| !same(d)).map_or(text.len(), |(i, _)| at + i);
    (start, end)
}

/// The character before which byte `at` of the page text lies.
pub fn page_char(page: &PageText, at: usize) -> u32 {
    page.chars.partition_point(|c| (c.0 as usize) < at) as u32
}

/// The byte offset of the `n`th character of `text`.
pub fn byte_of_char(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(i, _)| i)
}

/// The place in a galley of `text` nearest to `pos`, as a byte offset into `text`.
pub fn byte_at(galley: &Galley, text: &str, pos: egui::Vec2) -> usize {
    byte_of_char(text, galley.cursor_from_pos(pos).index.0)
}

/// Rectangles, relative to the galley, over its characters `from..to`. A chosen line
/// end shows as a little room past the row.
pub fn galley_spans(galley: &Galley, from: usize, to: usize) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut at = 0;
    for placed in &galley.rows {
        let count = placed.char_count_including_newline().0;
        let (a, b) = (from.max(at), to.min(at + count));
        if a < b {
            let x0 = placed.row.x_offset(CharIndex(a - at));
            let past = b - at > placed.row.char_count_excluding_newline().0;
            let x1 = if past { placed.row.size.x + 4.0 } else { placed.row.x_offset(CharIndex(b - at)) };
            let r = placed.rect();
            out.push(Rect::from_x_y_ranges(placed.pos.x + x0..=placed.pos.x + x1, r.y_range()));
        }
        at += count;
    }
    out
}

/// The character count of `text[..byte]`, for galley cursors.
pub fn chars_before(text: &str, byte: usize) -> usize {
    text[..boundary(text, byte)].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_on_characters_whichever_way_it_was_chosen() {
        let text = "zażółć gęślą";
        assert_eq!(text_between(text, 0, 3).as_deref(), Some("za"), "the middle of ż goes back to its start");
        assert_eq!(text_between(text, 9, 0).as_deref(), Some("zażół"), "chosen from right to left, the middle of ć cut back");
        assert_eq!(text_between(text, 5, 5), None);
        assert_eq!(text_between(text, 0, 999).as_deref(), Some(text));
    }

    #[test]
    fn blocks_keep_paragraphs_apart_and_list_items_together() {
        let block = |kind, text: &str| Block { kind, text: text.into() };
        let blocks = [block(BlockKind::H1, "Title"), block(BlockKind::Paragraph, "First para"), block(BlockKind::Item, "one"), block(BlockKind::Item, "two"), block(BlockKind::Rule, ""), block(BlockKind::Paragraph, "End")];
        assert_eq!(blocks_between(&blocks, (0, 2), (1, 5)).as_deref(), Some("tle\n\nFirst"));
        assert_eq!(blocks_between(&blocks, (5, 3), (2, 0)).as_deref(), Some("one\ntwo\n\nEnd"));
        assert_eq!(blocks_between(&blocks, (1, 0), (1, 0)), None);
    }

    #[test]
    fn cells_become_tab_separated_rows() {
        let rows = vec![vec!["a".to_string(), "b".into(), "c".into()], vec!["d".into(), "say \"hi\"\tthere".into()], vec!["g".into()]];
        assert_eq!(cells_between(&rows, (1, 1), (0, 0)).as_deref(), Some("a\tb\nd\t\"say \"\"hi\"\"\tthere\""));
        assert_eq!(cells_between(&rows, (2, 0), (2, 2)).as_deref(), Some("g\t\t"), "missing cells stay empty");
        assert_eq!(cells_between(&rows, (0, 0), (9, 0)).as_deref(), Some("a\nd\ng"));
    }

    #[test]
    fn a_page_gives_the_text_between_its_characters() {
        let text = "Hello world\nNext".to_string();
        let mut chars: Vec<(u32, [f32; 4])> = text.char_indices().filter(|(_, c)| !c.is_whitespace()).map(|(i, _)| (i as u32, [0.0; 4])).collect();
        chars.shrink_to_fit();
        let page = PageText { text, chars };
        assert_eq!(page_between(&page, 0, 5).as_deref(), Some("Hello"));
        assert_eq!(page_between(&page, 10, 5).as_deref(), Some("world"), "the line end after it is left out");
        assert_eq!(page_between(&page, 0, page.chars.len() as u32).as_deref(), Some("Hello world\nNext"));
        assert_eq!(page_char(&page, 6), 5, "w is the sixth drawn character");
    }

    #[test]
    fn a_double_click_chooses_the_word() {
        let text = "let ferrous_x = 42;\nnext";
        assert_eq!(word_at(text, 6), (4, 13));
        assert_eq!(word_at(text, 14), (14, 15), "a mark alone");
        assert_eq!(word_at(text, 3), (3, 4), "a space alone");
        assert_eq!(word_at(text, text.len()), (text.len(), text.len()));
        assert_eq!(word_at("zażółć", 2), (0, "zażółć".len()));
    }
}
