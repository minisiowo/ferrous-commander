//! Spreadsheets as tables, and office documents and e-books as plain structured text.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::load::{Block, BlockKind, Loaded, Options, Picture, Reply, failed, plain};
use super::text;
use crate::fsx;

const MAX_ROWS: usize = 20_000;
const MAX_COLUMNS: usize = 200;
const MAX_CELL: usize = 500;
const MAX_TEXT: usize = 300 * 1024;
/// A zip entry's own claim about its size cannot be trusted, so reading stops here.
const MAX_ENTRY: u64 = 64 * 1024 * 1024;
const MAX_COVER: u32 = 900;

fn cell(text: String) -> String {
    if text.len() <= MAX_CELL { return text; }
    text.chars().take(MAX_CELL).collect()
}

pub fn workbook(p: &Path, options: Options) -> Reply {
    use calamine::{Reader as _, open_workbook_auto};
    let Ok(mut book) = open_workbook_auto(p) else { return failed("This spreadsheet could not be read") };
    let sheets = book.sheet_names().to_vec();
    if sheets.is_empty() { return failed("This spreadsheet has no sheets"); }
    let page = (options.page as usize).min(sheets.len() - 1);
    let Ok(range) = book.worksheet_range(&sheets[page]) else { return failed("This sheet could not be read") };
    let rows: Vec<Vec<String>> = range.rows().take(MAX_ROWS)
        .map(|row| row.iter().take(MAX_COLUMNS).map(|c| cell(c.to_string())).collect()).collect();
    let trimmed = range.height() > rows.len();
    plain(Loaded::Table { rows, sheets, page: page as u32, trimmed })
}

pub fn separated(p: &Path, ext: &str, len: u64) -> Reply {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(p).and_then(|f| f.take(text::TEXT_MAX as u64).read_to_end(&mut bytes));
    if let Err(e) = read { return failed(fsx::friendly_err(&e)); }
    let Some(content) = text::decode(&bytes, true) else { return failed("This table could not be read") };
    let rows = parse_separated(&content, ext);
    let trimmed = rows.len() == MAX_ROWS || len > bytes.len() as u64;
    plain(Loaded::Table { rows, sheets: Vec::new(), page: 0, trimmed })
}

/// Quoted fields may hold separators and line breaks. Excel in Polish writes semicolons.
fn parse_separated(content: &str, ext: &str) -> Vec<Vec<String>> {
    let first = content.lines().next().unwrap_or("");
    let separator = if ext == "tsv" { '\t' } else {
        [',', ';', '\t'].into_iter().max_by_key(|&c| first.matches(c).count()).unwrap_or(',')
    };
    let mut rows = Vec::new();
    let (mut row, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => { chars.next(); field.push('"'); }
            '"' if quoted || field.is_empty() => quoted = !quoted,
            '\r' if !quoted => {}
            '\n' if !quoted => {
                row.push(cell(std::mem::take(&mut field)));
                rows.push(std::mem::take(&mut row));
                if rows.len() == MAX_ROWS { return rows; }
            }
            c if c == separator && !quoted => {
                if row.len() < MAX_COLUMNS { row.push(cell(std::mem::take(&mut field))); } else { field.clear(); }
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(cell(field));
        rows.push(row);
    }
    rows
}

type Zip = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

fn open_zip(p: &Path) -> Result<Zip, String> {
    let file = std::fs::File::open(p).map_err(|e| fsx::friendly_err(&e))?;
    zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|_| "This document could not be read".to_string())
}

fn entry_bytes(zip: &mut Zip, name: &str) -> Option<Vec<u8>> {
    let entry = zip.by_name(name).ok()?;
    let mut bytes = Vec::new();
    entry.take(MAX_ENTRY).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn entry_text(zip: &mut Zip, name: &str) -> Option<String> {
    String::from_utf8(entry_bytes(zip, name)?).ok()
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

pub(super) fn attribute(e: &BytesStart, wanted: &str) -> Option<String> {
    e.attributes().flatten().find(|a| local(a.key.as_ref()) == wanted)
        .map(|a| a.value.into_owned())
}

/// How one document format marks its paragraphs and text.
struct Rules {
    /// The element starting a block, and its kind.
    block: fn(&BytesStart) -> Option<BlockKind>,
    /// The element holding text, when text elsewhere is not content.
    text_in: Option<&'static str>,
    /// Elements inside a block that change its kind, like a style reference.
    restyle: fn(&BytesStart) -> Option<BlockKind>,
    /// Elements whose contents are not shown.
    skip: &'static [&'static str],
    /// Elements that mean a space or a new line within the text.
    space: &'static [&'static str],
}

struct Collected {
    blocks: Vec<Block>,
    size: usize,
}

impl Collected {
    fn push(&mut self, kind: BlockKind, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() { return; }
        self.size += text.len();
        self.blocks.push(Block { kind, text });
    }

    fn full(&self) -> bool {
        self.size >= MAX_TEXT
    }
}

fn collect(xml: &str, rules: &Rules, out: &mut Collected) {
    let mut reader = Reader::from_str(xml);
    let (mut kind, mut current, mut inside_text, mut skipping) = (None, String::new(), 0usize, 0usize);
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => event,
        };
        match event {
            Event::Start(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if skipping > 0 || rules.skip.contains(&name) { skipping += 1; continue; }
                if let Some(started) = (rules.block)(&e) {
                    if let Some(open) = kind.take() { out.push(open, std::mem::take(&mut current)); }
                    kind = Some(started);
                } else if kind.is_some() && let Some(restyled) = (rules.restyle)(&e) {
                    kind = Some(restyled);
                }
                if rules.text_in == Some(name) { inside_text += 1; }
            }
            Event::Empty(e) => {
                let name = e.local_name();
                if skipping > 0 { continue; }
                if rules.space.contains(&name.as_ref()) { current.push(' '); }
                if kind.is_some() && let Some(restyled) = (rules.restyle)(&e) { kind = Some(restyled); }
            }
            Event::End(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if skipping > 0 { skipping -= 1; continue; }
                if rules.text_in == Some(name) { inside_text = inside_text.saturating_sub(1); }
                let closes = BytesStart::new(name.to_string());
                if (rules.block)(&closes).is_some() && let Some(open) = kind.take() {
                    out.push(open, std::mem::take(&mut current));
                    if out.full() { return; }
                }
            }
            Event::Text(t) if skipping == 0 && kind.is_some() && (rules.text_in.is_none() || inside_text > 0) => {
                current.push_str(&t.xml10_content());
            }
            Event::CData(t) if skipping == 0 && kind.is_some() => current.push_str(&t),
            Event::GeneralRef(r) if skipping == 0 && kind.is_some() && (rules.text_in.is_none() || inside_text > 0) => {
                if let Ok(Some(c)) = r.resolve_char_ref() { current.push(c); }
                else if let Some(known) = quick_xml::escape::resolve_predefined_entity(&r) { current.push_str(known); }
                else if &*r == "nbsp" { current.push(' '); }
            }
            _ => {}
        }
    }
    if let Some(open) = kind.take() { out.push(open, current); }
}

fn no_restyle(_: &BytesStart) -> Option<BlockKind> { None }

fn docx_block(e: &BytesStart) -> Option<BlockKind> {
    (e.local_name().as_ref() == "p").then_some(BlockKind::Paragraph)
}

fn docx_style(e: &BytesStart) -> Option<BlockKind> {
    if e.local_name().as_ref() != "pStyle" { return None; }
    let style = attribute(e, "val")?.to_ascii_lowercase();
    if style.ends_with("heading1") || style == "title" { Some(BlockKind::H1) }
    else if style.ends_with("heading2") { Some(BlockKind::H2) }
    else if style.contains("heading") { Some(BlockKind::H3) }
    else if style.contains("list") { Some(BlockKind::Item) }
    else { None }
}

fn odt_block(e: &BytesStart) -> Option<BlockKind> {
    match e.local_name().as_ref() {
        "p" => Some(BlockKind::Paragraph),
        "h" => Some(match attribute(e, "outline-level").as_deref() {
            Some("1") => BlockKind::H1,
            Some("2") => BlockKind::H2,
            _ => BlockKind::H3,
        }),
        _ => None,
    }
}

fn html_block(e: &BytesStart) -> Option<BlockKind> {
    match e.local_name().as_ref() {
        "p" | "div" => Some(BlockKind::Paragraph),
        "h1" => Some(BlockKind::H1),
        "h2" => Some(BlockKind::H2),
        "h3" | "h4" | "h5" | "h6" => Some(BlockKind::H3),
        "li" => Some(BlockKind::Item),
        "blockquote" => Some(BlockKind::Quote),
        "pre" => Some(BlockKind::Code),
        _ => None,
    }
}

const WORD_RULES: Rules = Rules { block: docx_block, text_in: Some("t"), restyle: docx_style, skip: &[], space: &["tab", "br"] };
const SLIDE_RULES: Rules = Rules { block: docx_block, text_in: Some("t"), restyle: no_restyle, skip: &[], space: &["br"] };
const ODT_RULES: Rules = Rules { block: odt_block, text_in: None, restyle: no_restyle, skip: &[], space: &["s", "tab", "line-break"] };
const HTML_RULES: Rules = Rules { block: html_block, text_in: None, restyle: no_restyle, skip: &["head", "script", "style"], space: &["br"] };

fn document(blocks: Collected) -> Reply {
    if blocks.blocks.is_empty() { return plain(Loaded::Error("This document has no text to show".into())); }
    let trimmed = blocks.full();
    plain(Loaded::Doc { blocks: blocks.blocks, source: None, trimmed })
}

fn empty() -> Collected {
    Collected { blocks: Vec::new(), size: 0 }
}

pub fn word(p: &Path) -> Reply {
    let mut zip = match open_zip(p) { Ok(z) => z, Err(e) => return failed(e) };
    let Some(xml) = entry_text(&mut zip, "word/document.xml") else { return failed("This document could not be read") };
    let mut out = empty();
    collect(&xml, &WORD_RULES, &mut out);
    document(out)
}

pub fn opendocument(p: &Path) -> Reply {
    let mut zip = match open_zip(p) { Ok(z) => z, Err(e) => return failed(e) };
    let Some(xml) = entry_text(&mut zip, "content.xml") else { return failed("This document could not be read") };
    let mut out = empty();
    collect(&xml, &ODT_RULES, &mut out);
    document(out)
}

pub fn slides(p: &Path) -> Reply {
    let mut zip = match open_zip(p) { Ok(z) => z, Err(e) => return failed(e) };
    let number = |name: &str| name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse::<u32>().ok();
    let mut names: Vec<(u32, String)> = zip.file_names().filter_map(|n| {
        let n = n.ok()?;
        Some((number(&n)?, n.into_owned()))
    }).collect();
    names.sort();
    let mut out = empty();
    for (n, name) in names {
        if out.full() { break; }
        let Some(xml) = entry_text(&mut zip, &name) else { continue };
        out.push(BlockKind::H2, format!("Slide {n}"));
        collect(&xml, &SLIDE_RULES, &mut out);
    }
    document(out)
}

/// Joins a path inside the book to the folder of the file that mentions it.
fn beside(base: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let mut parts: Vec<&str> = base.rsplit_once('/').map_or(Vec::new(), |(dir, _)| dir.split('/').collect());
    for part in href.split('/') {
        match part {
            ".." => { parts.pop(); }
            "." | "" => {}
            part => parts.push(part),
        }
    }
    parts.join("/")
}

pub fn epub(p: &Path) -> Reply {
    let mut zip = match open_zip(p) { Ok(z) => z, Err(e) => return failed(e) };
    let Some(container) = entry_text(&mut zip, "META-INF/container.xml") else { return failed("This book could not be read") };
    let mut reader = Reader::from_str(&container);
    let mut package = None;
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == "rootfile" => {
                package = attribute(&e, "full-path");
                break;
            }
            _ => {}
        }
    }
    let Some(package) = package else { return failed("This book could not be read") };
    let Some(opf) = entry_text(&mut zip, &package) else { return failed("This book could not be read") };
    let (mut manifest, mut spine, mut cover_id, mut cover_href) = (HashMap::new(), Vec::new(), None, None);
    let mut reader = Reader::from_str(&opf);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e) | Event::Empty(e)) => match e.local_name().as_ref() {
                "item" => {
                    let (Some(id), Some(href)) = (attribute(&e, "id"), attribute(&e, "href")) else { continue };
                    if attribute(&e, "properties").is_some_and(|p| p.contains("cover-image")) { cover_href = Some(href.clone()); }
                    manifest.insert(id, href);
                }
                "itemref" => spine.extend(attribute(&e, "idref")),
                "meta" if attribute(&e, "name").as_deref() == Some("cover") => cover_id = attribute(&e, "content"),
                _ => {}
            },
            _ => {}
        }
    }
    let cover = cover_href.or_else(|| manifest.get(&cover_id?).cloned());
    let picture = cover.and_then(|href| {
        let bytes = entry_bytes(&mut zip, &beside(&package, &href))?;
        let img = image::load_from_memory(&bytes).ok()?;
        let img = if img.width().max(img.height()) > MAX_COVER { img.thumbnail(MAX_COVER, MAX_COVER) } else { img }.into_rgba8();
        Some(Picture { size: [img.width(), img.height()], delays: Vec::new(), rgba: img.into_raw() })
    });
    let mut out = empty();
    for id in spine {
        if out.full() { break; }
        let Some(href) = manifest.get(&id) else { continue };
        let Some(xml) = entry_text(&mut zip, &beside(&package, href)) else { continue };
        collect(&xml, &HTML_RULES, &mut out);
    }
    let (loaded, _) = document(out);
    (loaded, picture)
}

#[cfg(test)]
mod tests {
    use super::super::load::{Options, load, scratch, whole};
    use super::*;
    use std::io::Write;

    fn zip_file(path: &Path, entries: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, content) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    fn texts(path: &Path) -> Vec<(BlockKind, String)> {
        let Loaded::Doc { blocks, .. } = load(path, whole(64)).0 else { panic!("not a document: {path:?}") };
        blocks.into_iter().map(|b| (b.kind, b.text)).collect()
    }

    #[test]
    fn separated_values_handle_quotes_and_semicolons() {
        assert_eq!(parse_separated("a,\"b,c\",\"d\"\"e\"\n1,2,3", "csv"), vec![vec!["a", "b,c", "d\"e"], vec!["1", "2", "3"]]);
        assert_eq!(parse_separated("a;b;c\r\n1;2;3\r\n", "csv"), vec![vec!["a", "b", "c"], vec!["1", "2", "3"]]);
        assert_eq!(parse_separated("a\t\"x\ny\"\n", "tsv"), vec![vec!["a", "x\ny"]]);
    }

    #[test]
    fn csv_files_open_as_tables() {
        let root = scratch("csv");
        let file = root.join("data.csv");
        std::fs::write(&file, "name,age\nAla,30\n").unwrap();
        assert!(matches!(load(&file, whole(64)).0, Loaded::Table { rows, trimmed: false, .. } if rows.len() == 2 && rows[1][0] == "Ala"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workbooks_show_one_sheet_at_a_time() {
        let root = scratch("xlsx");
        let file = root.join("book.xlsx");
        let sheet = |text: &str| format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>{text}</t></is></c><c r="B1"><v>7</v></c></row></sheetData></worksheet>"#);
        let (first, second) = (sheet("żółw"), sheet("drugi"));
        zip_file(&file, &[
            ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#),
            ("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Dane" sheetId="1" r:id="r1"/><sheet name="Inne" sheetId="2" r:id="r2"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="r2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", &first),
            ("xl/worksheets/sheet2.xml", &second),
        ]);
        let Loaded::Table { rows, sheets, page: 0, .. } = load(&file, whole(64)).0 else { panic!("not a table") };
        assert_eq!(sheets, ["Dane", "Inne"]);
        assert_eq!(rows, [["żółw", "7"]]);
        let Loaded::Table { rows, page: 1, .. } = load(&file, Options { max_side: 64, page: 1, crop: None, view: None }).0 else { panic!("not a table") };
        assert_eq!(rows[0][0], "drugi");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn word_documents_keep_headings() {
        let root = scratch("docx");
        let file = root.join("a.docx");
        let xml = r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Tytuł</w:t></w:r></w:p>
            <w:p><w:r><w:t>Ala </w:t></w:r><w:r><w:t>ma &amp; kota</w:t></w:r></w:p></w:body></w:document>"#;
        zip_file(&file, &[("word/document.xml", xml)]);
        assert_eq!(texts(&file), [(BlockKind::H1, "Tytuł".to_string()), (BlockKind::Paragraph, "Ala ma & kota".to_string())]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opendocument_and_slides_read_in_order() {
        let root = scratch("odt");
        let odt = root.join("a.odt");
        zip_file(&odt, &[("content.xml", r#"<o xmlns:text="t"><text:h text:outline-level="2">Head</text:h><text:p>Body<text:s/>text</text:p></o>"#)]);
        assert_eq!(texts(&odt), [(BlockKind::H2, "Head".to_string()), (BlockKind::Paragraph, "Body text".to_string())]);
        let pptx = root.join("a.pptx");
        let slide = |t: &str| format!(r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#);
        let (two, ten) = (slide("two"), slide("ten"));
        zip_file(&pptx, &[("ppt/slides/slide10.xml", &ten), ("ppt/slides/slide2.xml", &two)]);
        let got: Vec<String> = texts(&pptx).into_iter().map(|t| t.1).collect();
        assert_eq!(got, ["Slide 2", "two", "Slide 10", "ten"], "numeric order, not by name");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn epub_follows_its_spine() {
        let root = scratch("epub");
        let file = root.join("a.epub");
        zip_file(&file, &[
            ("META-INF/container.xml", r#"<container><rootfiles><rootfile full-path="OEBPS/book.opf"/></rootfiles></container>"#),
            ("OEBPS/book.opf", r#"<package><manifest><item id="b" href="b.xhtml"/><item id="a" href="text/a.xhtml"/></manifest><spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#),
            ("OEBPS/text/a.xhtml", "<html><head><title>no</title></head><body><h1>One</h1><p>First&#160;line</p></body></html>"),
            ("OEBPS/b.xhtml", "<html><body><p>Second</p></body></html>"),
        ]);
        let got: Vec<String> = texts(&file).into_iter().map(|t| t.1).collect();
        assert_eq!(got, ["One", "First\u{a0}line", "Second"]);
        assert_eq!(beside("OEBPS/text/a.xhtml", "../img/c.png#x"), "OEBPS/img/c.png");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn broken_office_files_are_reported() {
        let root = scratch("office_broken");
        for name in ["a.docx", "a.odt", "a.pptx", "a.epub", "a.xlsx", "a.ods"] {
            let file = root.join(name);
            std::fs::write(&file, b"PK\x03\x04 not really").unwrap();
            let options = Options { max_side: 64, page: 0, crop: None, view: None };
            assert!(matches!(load(&file, options).0, Loaded::Error(_)), "{name}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

