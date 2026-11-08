//! Text in any common encoding, code with its colours, Markdown and raw bytes.

use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use super::load::{Block, BlockKind, Colours, Loaded};
use crate::fsx;

pub const TEXT_MAX: usize = 2 * 1024 * 1024;
const HEX_MAX: usize = 64 * 1024;
/// Text shows at once; colours are for as much of it as this allows.
const COLOUR_BUDGET: Duration = Duration::from_millis(300);
const COLOUR_LINE_MAX: usize = 2000;
/// Formatted text is laid out whole, again whenever the window changes width.
const MARKDOWN_MAX: usize = 300 * 1024;

pub const CLASS_COMMENT: u8 = 1;
pub const CLASS_STRING: u8 = 2;
pub const CLASS_NUMBER: u8 = 3;
pub const CLASS_KEYWORD: u8 = 4;
pub const CLASS_FUNCTION: u8 = 5;
pub const CLASS_TYPE: u8 = 6;
pub const CLASS_TAG: u8 = 7;

/// `None` for contents that are not text.
pub fn decode(bytes: &[u8], known_text: bool) -> Option<String> {
    let utf16 = |bytes: &[u8], encoding: &'static encoding_rs::Encoding| encoding.decode(bytes).0.into_owned();
    if bytes.starts_with(&[0xFF, 0xFE]) { return Some(utf16(bytes, encoding_rs::UTF_16LE)); }
    if bytes.starts_with(&[0xFE, 0xFF]) { return Some(utf16(bytes, encoding_rs::UTF_16BE)); }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let has_nul = bytes[..bytes.len().min(8192)].contains(&0);
    if has_nul && !known_text { return None; }
    match std::str::from_utf8(body) {
        Ok(text) => return Some(text.to_owned()),
        // Only the cut at the end of the read part: a character split in two.
        Err(e) if e.error_len().is_none() => return Some(String::from_utf8_lossy(&body[..e.valid_up_to()]).into_owned()),
        Err(_) => {}
    }
    let sample = &bytes[..bytes.len().min(8192)];
    let control = sample.iter().filter(|&&b| b < 9 || (b > 13 && b < 32) || b == 127).count();
    if !known_text && control * 100 > sample.len() { return None; }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(bytes, false);
    Some(detector.guess(None, chardetng::Utf8Detection::Allow).decode(bytes).0.into_owned())
}

pub fn load(p: &Path, ext: &str, len: u64) -> Loaded {
    use std::io::Read;
    let mut bytes = Vec::new();
    let read = std::fs::File::open(p).and_then(|f| f.take(TEXT_MAX as u64).read_to_end(&mut bytes));
    if let Err(e) = read { return Loaded::Error(fsx::friendly_err(&e)); }
    let Some(text) = decode(&bytes, fsx::is_text_extension(ext)) else {
        bytes.truncate(HEX_MAX);
        return Loaded::Hex { bytes, total: len };
    };
    let trimmed = len > bytes.len() as u64;
    if matches!(ext, "md" | "markdown" | "mdown") {
        let (blocks, cut) = markdown(&text);
        return Loaded::Doc { blocks, source: Some(text), trimmed: trimmed || cut };
    }
    let colours = colours(&text, ext, COLOUR_BUDGET);
    Loaded::Text { text, trimmed, colours }
}

fn syntaxes() -> &'static syntect::parsing::SyntaxSet {
    static SET: OnceLock<syntect::parsing::SyntaxSet> = OnceLock::new();
    SET.get_or_init(syntect::parsing::SyntaxSet::load_defaults_newlines)
}

/// A theme whose "colours" are class numbers, so the window can paint them from its palette.
fn classes() -> &'static syntect::highlighting::Theme {
    use syntect::highlighting::{Color, ScopeSelectors, StyleModifier, Theme, ThemeItem};
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let rule = |scope: &str, class: u8| ThemeItem {
            scope: ScopeSelectors::from_str(scope).expect("valid selector"),
            style: StyleModifier { foreground: Some(Color { r: class, g: 0, b: 0, a: 255 }), background: None, font_style: None },
        };
        let mut theme = Theme::default();
        theme.settings.foreground = Some(Color { r: 0, g: 0, b: 0, a: 255 });
        theme.scopes = vec![
            rule("comment", CLASS_COMMENT),
            rule("string", CLASS_STRING),
            rule("constant", CLASS_NUMBER),
            rule("keyword, storage", CLASS_KEYWORD),
            rule("entity.name.function, support.function", CLASS_FUNCTION),
            rule("entity.name.type, entity.name.class, support.type, support.class", CLASS_TYPE),
            rule("entity.name.tag, entity.other.attribute-name", CLASS_TAG),
        ];
        theme
    })
}

fn colours(text: &str, ext: &str, budget: Duration) -> Colours {
    use syntect::easy::HighlightLines;
    let set = syntaxes();
    let first = text.lines().next().unwrap_or("");
    let Some(syntax) = set.find_syntax_by_extension(ext).or_else(|| set.find_syntax_by_first_line(first)) else { return Colours::default() };
    if syntax.name == "Plain Text" { return Colours::default(); }
    let mut highlighter = HighlightLines::new(syntax, classes());
    let started = Instant::now();
    let mut colours = Colours::default();
    for line in text.split_inclusive('\n') {
        if started.elapsed() > budget || line.len() > COLOUR_LINE_MAX { break; }
        let Ok(pieces) = highlighter.highlight_line(line, set) else { break };
        let line_start = colours.spans.len();
        for (style, piece) in pieces {
            let class = style.foreground.r;
            match colours.spans[line_start..].last_mut() {
                Some(last) if last.1 == class => last.0 += piece.len() as u32,
                _ => colours.spans.push((piece.len() as u32, class)),
            }
        }
        colours.ends.push(colours.spans.len() as u32);
    }
    colours
}

fn markdown(text: &str) -> (Vec<Block>, bool) {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut quote = 0usize;
    let flush = |blocks: &mut Vec<Block>, current: &mut String, kind| {
        let text = current.trim_end().to_string();
        current.clear();
        if !text.is_empty() { blocks.push(Block { kind, text }); }
    };
    let mut in_item = 0usize;
    let (mut size, mut counted) = (0, 0);
    for event in Parser::new_ext(text, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        size += blocks[counted..].iter().map(|b: &Block| b.text.len()).sum::<usize>();
        counted = blocks.len();
        if size > MARKDOWN_MAX { return (blocks, true); }
        match event {
            Event::Start(Tag::BlockQuote(_)) => quote += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote = quote.saturating_sub(1),
            Event::Start(Tag::Item) => { flush(&mut blocks, &mut current, BlockKind::Item); in_item += 1; }
            Event::End(TagEnd::Item) => { flush(&mut blocks, &mut current, BlockKind::Item); in_item = in_item.saturating_sub(1); }
            Event::End(TagEnd::Heading(level)) => {
                let kind = match level { HeadingLevel::H1 => BlockKind::H1, HeadingLevel::H2 => BlockKind::H2, _ => BlockKind::H3 };
                flush(&mut blocks, &mut current, kind);
            }
            Event::End(TagEnd::Paragraph) => {
                let kind = if in_item > 0 { BlockKind::Item } else if quote > 0 { BlockKind::Quote } else { BlockKind::Paragraph };
                flush(&mut blocks, &mut current, kind);
            }
            Event::End(TagEnd::CodeBlock) => flush(&mut blocks, &mut current, BlockKind::Code),
            Event::End(TagEnd::TableRow | TagEnd::TableHead) => flush(&mut blocks, &mut current, BlockKind::Code),
            Event::End(TagEnd::TableCell) => current.push_str("   "),
            Event::Rule => blocks.push(Block { kind: BlockKind::Rule, text: String::new() }),
            Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => current.push_str(&t),
            Event::SoftBreak => current.push(' '),
            Event::HardBreak => current.push('\n'),
            _ => {}
        }
    }
    flush(&mut blocks, &mut current, BlockKind::Paragraph);
    (blocks, false)
}

#[cfg(test)]
mod tests {
    use super::super::load::{Options, load as load_file, scratch, whole};
    use super::*;

    #[test]
    fn encodings_are_recognised() {
        assert_eq!(decode("żółw".as_bytes(), false).unwrap(), "żółw");
        let (bytes, ..) = encoding_rs::WINDOWS_1250.encode("Zażółć gęślą jaźń, zażółć gęślą jaźń");
        assert!(decode(&bytes, false).unwrap().contains("gęślą"));
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("żółw".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode(&utf16, false).unwrap(), "żółw");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend(b"text");
        assert_eq!(decode(&bom, false).unwrap(), "text");
        assert_eq!(decode(&"żółw".as_bytes()[..3], false).unwrap(), "ż", "a character cut by the read limit");
    }

    #[test]
    fn binary_contents_go_to_the_hex_view() {
        assert!(decode(&[0, 1, 2, 3], false).is_none());
        assert!(decode(&[1, 2, 3, 200, 201, 4, 5, 6], false).is_none());
        let root = scratch("hex");
        let file = root.join("blob.bin");
        std::fs::write(&file, vec![0u8; HEX_MAX * 2]).unwrap();
        assert!(matches!(load_file(&file, whole(64)).0, Loaded::Hex { bytes, total } if bytes.len() == HEX_MAX && total == HEX_MAX as u64 * 2));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn long_text_is_trimmed_and_code_gets_colours() {
        let root = scratch("text");
        let long = root.join("long.txt");
        std::fs::write(&long, "x".repeat(TEXT_MAX + 1)).unwrap();
        assert!(matches!(load_file(&long, whole(64)).0, Loaded::Text { text, trimmed: true, .. } if text.len() == TEXT_MAX));
        let code = root.join("main.rs");
        std::fs::write(&code, "// note\nfn main() { let a = \"s\"; }\n").unwrap();
        assert!(matches!(load_file(&code, whole(64)).0, Loaded::Text { .. }));
        // The time limit depends on the machine and on what else is running.
        let text = std::fs::read_to_string(&code).unwrap();
        let spans = colours(&text, "rs", Duration::from_secs(120));
        assert_eq!(spans.ends.len(), 2);
        assert_eq!(spans.spans.iter().map(|s| s.0 as usize).sum::<usize>(), text.len(), "spans cover the text");
        let (first, second) = (spans.line(0).unwrap(), spans.line(1).unwrap());
        assert!(first.iter().any(|s| s.1 == CLASS_COMMENT));
        assert!(second.iter().any(|s| s.1 == CLASS_KEYWORD));
        assert!(second.iter().any(|s| s.1 == CLASS_STRING));
        assert!(spans.line(2).is_none());
        let lying = Colours { spans: vec![(1, 1)], ends: vec![5, 0] };
        assert!(lying.line(0).is_none() && lying.line(1).is_none(), "numbers from the helper are not trusted");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn very_long_markdown_is_cut() {
        let (blocks, cut) = markdown(&"A paragraph of words.\n\n".repeat(40_000));
        assert!(cut);
        assert!(blocks.iter().map(|b| b.text.len()).sum::<usize>() <= MARKDOWN_MAX * 2);
    }

    #[test]
    fn markdown_becomes_blocks_and_keeps_its_source() {
        let root = scratch("markdown");
        let file = root.join("notes.md");
        std::fs::write(&file, "# Title\n\nSome *text*.\n\n- one\n- two\n\n> quoted\n\n```\ncode\n```\n\n---\n").unwrap();
        let Loaded::Doc { blocks, source, .. } = load_file(&file, Options { max_side: 64, page: 0, crop: None, view: None }).0 else { panic!("not a document") };
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BlockKind::H1, BlockKind::Paragraph, BlockKind::Item, BlockKind::Item, BlockKind::Quote, BlockKind::Code, BlockKind::Rule]);
        assert_eq!(blocks[1].text, "Some text.");
        assert!(source.unwrap().starts_with("# Title"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
