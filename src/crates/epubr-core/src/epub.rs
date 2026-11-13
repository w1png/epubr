mod locations;
mod optimizer;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Write};
use std::iter::Peekable;
use std::ops::Range;
use std::sync::LazyLock;

use chrono::{SecondsFormat, Utc};
use regex::{Captures, Regex};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::OptimizationTarget;
use crate::book::{Block, Book, NavEntry};

const CONTAINER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="EPUB/book.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"#;
const STYLES: &str = r#"body { line-height: 1.4; }
figure { margin: 1em 0; }
img { max-width: 100%; height: auto; }
body.cover { margin: 0; text-align: center; }
body.cover img { max-height: 100vh; object-fit: contain; }
.formula-image { text-align: center; }
figcaption, aside { font-size: 0.9em; }
pre { font-family: monospace; font-size: 0.88em; white-space: pre-wrap; overflow-wrap: break-word; }
table { border-collapse: collapse; max-width: 100%; }
td, th { padding: 0.2em 0.4em; border: 1px solid; overflow-wrap: anywhere; }
th { text-align: start; }
.contents ol, #toc ol { list-style: none; padding-inline-start: 1.5em; }
.contents li { margin-block: 0.35em; }
"#;
const CONTENT: &str = "content.xhtml";
const OPTIMIZED_SECTION_BYTES: usize = 32 * 1024;

// The normalizer generates these IDs and fragment links when pairing notes.
static NOTE_REFERENCE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<a id="(noteref-[0-9]+)""#).unwrap());
static NOTE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r##"(<a (?:id="noteref-[0-9]+" )?href=")#((?:note|noteref)-[0-9]+)"##).unwrap()
});

struct Heading<'a> {
    level: u8,
    id: &'a str,
    title: &'a str,
}

struct RenderedBlock {
    html: String,
    has_mathml: bool,
}

struct Section<'a> {
    path: String,
    title: &'a str,
    blocks: Range<usize>,
    chapter_group: usize,
}

pub(crate) fn write(
    file: &mut File,
    book: &Book,
    optimization: Option<OptimizationTarget>,
) -> io::Result<()> {
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;
    zip.start_file("META-INF/container.xml", deflated)?;
    zip.write_all(CONTAINER.as_bytes())?;
    zip.start_file("EPUB/book.css", deflated)?;
    zip.write_all(STYLES.as_bytes())?;

    let headings = navigation_headings(book);
    let contents_headings = book.contents_navigation.as_deref().map(entry_headings);
    let mut manifest = String::new();
    let mut spine = String::new();
    let mut image_count = 0;
    let mut fingerprint = Sha256::new();
    let mut locations = optimization.map(|_| locations::Locations::default());

    if let Some(cover) = &book.cover {
        let optimized =
            optimization.and_then(|target| optimizer::optimize_image(target, "image/png", cover));
        let (media_type, data) = optimized
            .as_ref()
            .map_or(("image/png", cover.as_slice()), |(kind, bytes)| {
                (*kind, bytes.as_slice())
            });
        let extension = image_extension(media_type)?;
        zip.start_file(format!("EPUB/cover.{extension}"), stored)?;
        zip.write_all(data)?;
        fingerprint.update(Sha256::digest(data));
        let start = xhtml_start(&book.title, &book.language)
            .replace("<body>", "<body class=\"cover\" epub:type=\"cover\">");
        let xhtml = format!(
            "{start}<img src=\"cover.{extension}\" alt=\"Cover of {}\"/></body></html>\n",
            escape(&book.title)
        );
        zip.start_file("EPUB/cover.xhtml", deflated)?;
        zip.write_all(xhtml.as_bytes())?;
        if let Some(locations) = &mut locations {
            locations.push("EPUB/cover.xhtml", &xhtml, 0)?;
        }
        writeln!(
            manifest,
            "<item id=\"cover-page\" href=\"cover.xhtml\" media-type=\"application/xhtml+xml\"/>\n\
             <item id=\"cover-image\" href=\"cover.{extension}\" media-type=\"{media_type}\" properties=\"cover-image\"/>",
        ).unwrap();
        spine.push_str("<itemref idref=\"cover-page\"/>\n");
    }
    let mut rendered = Vec::with_capacity(book.blocks.len());
    for block in &book.blocks {
        let mut content = String::new();
        let mut has_mathml = false;
        match block {
            Block::PageAnchor { page } => {
                writeln!(content, "<span id=\"source-page-{page}\"></span>").unwrap();
            }
            Block::Heading { level, id, text } => {
                let level = (*level).clamp(1, 6);
                writeln!(
                    content,
                    "<h{level} id=\"{}\">{}</h{level}>",
                    escape(id),
                    escape(text)
                )
                .unwrap();
            }
            Block::Contents { .. } => {} // Contents links need the final document paths.
            Block::Paragraph { html } => {
                writeln!(content, "<p>{html}</p>").unwrap();
            }
            Block::List { ordered, items } => {
                let tag = if *ordered { "ol" } else { "ul" };
                writeln!(content, "<{tag}>").unwrap();
                for item in items {
                    has_mathml |= item.has_mathml;
                    if let Some(number) = item.number.filter(|_| *ordered) {
                        writeln!(content, "<li value=\"{number}\">{}</li>", item.html).unwrap();
                    } else {
                        writeln!(content, "<li>{}</li>", item.html).unwrap();
                    }
                }
                writeln!(content, "</{tag}>").unwrap();
            }
            Block::Code { text, caption } => {
                content.push_str("<figure>\n");
                if !caption.is_empty() {
                    writeln!(content, "<figcaption>{}</figcaption>", escape(caption)).unwrap();
                }
                writeln!(content, "<pre><code>{}</code></pre></figure>", escape(text)).unwrap();
            }
            Block::Figure {
                media_type, data, ..
            }
            | Block::FormulaImage { media_type, data } => {
                let optimized = optimization
                    .and_then(|target| optimizer::optimize_image(target, media_type, data));
                let (media_type, data) = optimized
                    .as_ref()
                    .map_or((media_type.as_str(), data.as_slice()), |(kind, bytes)| {
                        (*kind, bytes.as_slice())
                    });
                let extension = image_extension(media_type)?;
                image_count += 1;
                let href = format!("images/image-{image_count}.{extension}");
                zip.start_file(format!("EPUB/{href}"), stored)?;
                zip.write_all(data)?;
                fingerprint.update(Sha256::digest(data));
                writeln!(
                    manifest,
                    "<item id=\"image-{image_count}\" href=\"{href}\" media-type=\"{media_type}\"/>"
                )
                .unwrap();
                if let Block::Figure { caption, .. } = block {
                    let caption = escape(caption);
                    let alt = if caption.is_empty() {
                        "Figure"
                    } else {
                        &caption
                    };
                    writeln!(content, "<figure><img src=\"{href}\" alt=\"{alt}\"/>").unwrap();
                    if !caption.is_empty() {
                        writeln!(content, "<figcaption>{caption}</figcaption>").unwrap();
                    }
                    content.push_str("</figure>\n");
                } else {
                    writeln!(content, "<div class=\"formula-image\"><img src=\"{href}\" alt=\"Equation from source PDF\"/></div>").unwrap();
                }
            }
            Block::Note { id, html } => {
                writeln!(
                    content,
                    "<aside epub:type=\"footnote\" id=\"{}\"><p>{html}</p></aside>",
                    escape(id)
                )
                .unwrap();
            }
            Block::Formula { mathml } => {
                has_mathml = true;
                writeln!(content, "<div class=\"formula\">\n{mathml}\n</div>").unwrap();
            }
            Block::Table { rows } => {
                content.push_str("<table><tbody>\n");
                for row in rows {
                    content.push_str("<tr>");
                    for cell in row {
                        let tag = if cell.column_header || cell.row_header {
                            "th"
                        } else {
                            "td"
                        };
                        write!(content, "<{tag}").unwrap();
                        if cell.column_header && !cell.row_header {
                            content.push_str(" scope=\"col\"");
                        } else if cell.row_header && !cell.column_header {
                            content.push_str(" scope=\"row\"");
                        }
                        if cell.rowspan > 1 {
                            write!(content, " rowspan=\"{}\"", cell.rowspan).unwrap();
                        }
                        if cell.colspan > 1 {
                            write!(content, " colspan=\"{}\"", cell.colspan).unwrap();
                        }
                        write!(content, ">{}</{tag}>", escape(&cell.text)).unwrap();
                    }
                    content.push_str("</tr>\n");
                }
                content.push_str("</tbody></table>\n");
            }
        }
        rendered.push(RenderedBlock {
            html: content,
            has_mathml,
        });
    }

    let sections = plan_sections(book, &headings, &rendered, optimization.is_some());
    let targets = index_targets(book, &rendered, &sections);
    for section in &sections {
        let mut content = xhtml_start(section.title, &book.language);
        let mut has_mathml = false;
        for index in section.blocks.clone() {
            let block = &rendered[index];
            has_mathml |= block.has_mathml;
            if let Block::Contents { title } = &book.blocks[index] {
                writeln!(
                    content,
                    "<nav class=\"contents\" aria-labelledby=\"contents-heading\"><h2 id=\"contents-heading\">{}</h2>",
                    escape(title)
                )
                .unwrap();
                contents_list(
                    &mut content,
                    book,
                    contents_headings.as_deref().unwrap_or(&headings),
                    &targets,
                    &section.path,
                );
                content.push_str("</nav>\n");
            } else {
                let html = NOTE_LINK.replace_all(&block.html, |captures: &Captures<'_>| {
                    let id = &captures[2];
                    let target = targets.get(id).copied().unwrap_or(&section.path);
                    format!(
                        "{}{}#{id}",
                        &captures[1],
                        relative_path(&section.path, target)
                    )
                });
                content.push_str(&html);
            }
        }
        content.push_str("</body></html>\n");
        fingerprint.update(content.as_bytes());
        let path = format!("EPUB/{}", section.path);
        zip.start_file(&path, deflated)?;
        zip.write_all(content.as_bytes())?;
        if let Some(locations) = &mut locations {
            locations.push(&path, &content, section.chapter_group)?;
        }
        let id = section.path.trim_end_matches(".xhtml");
        let properties = if has_mathml {
            r#" properties="mathml""#
        } else {
            ""
        };
        writeln!(
            manifest,
            "<item id=\"{id}\" href=\"{}\" media-type=\"application/xhtml+xml\"{properties}/>",
            section.path
        )
        .unwrap();
        writeln!(spine, "<itemref idref=\"{id}\"/>").unwrap();
    }
    for (index, author) in book.authors.iter().enumerate() {
        if index > 0 {
            fingerprint.update([0]);
        }
        fingerprint.update(author.as_bytes());
    }
    zip.start_file("EPUB/nav.xhtml", deflated)?;
    zip.write_all(navigation(book, &headings, &targets).as_bytes())?;
    zip.start_file("EPUB/book.opf", deflated)?;
    zip.write_all(package(book, &identifier(fingerprint), &manifest, &spine).as_bytes())?;
    if let (Some(target), Some(locations)) = (optimization, locations) {
        let data = locations.finish()?;
        zip.start_file(locations::OPTIMIZER_PATH, stored)?;
        zip.write_all(&locations::optimizer_manifest(target, data.is_some())?)?;
        if let Some(data) = data {
            zip.start_file(locations::LOCATION_PATH, stored)?;
            zip.write_all(&data)?;
        }
    }
    zip.finish()?;
    Ok(())
}

fn plan_sections<'a>(
    book: &'a Book,
    headings: &[Heading<'a>],
    rendered: &[RenderedBlock],
    optimize: bool,
) -> Vec<Section<'a>> {
    let mut root_level = u8::MAX;
    let mut chapter_titles = HashMap::new();
    for heading in headings {
        // Match the root entries emitted by nav_list, including a TOC whose
        // first entries start at a deeper level than the later ones.
        if heading.level <= root_level {
            root_level = heading.level;
            chapter_titles.entry(heading.id).or_insert(heading.title);
        }
    }

    let mut sections = Vec::new();
    let mut title = book.title.as_str();
    let mut chapter_group = 1;
    let mut start = 0;
    let mut bytes = 0;
    for (index, (block, rendered)) in book.blocks.iter().zip(rendered).enumerate() {
        let chapter = block_id(block).and_then(|id| chapter_titles.get(id.as_ref()).copied());
        if index > start
            && (chapter.is_some()
                || (optimize && bytes + rendered.html.len() > OPTIMIZED_SECTION_BYTES))
        {
            sections.push(Section {
                path: section_path(sections.len()),
                title,
                blocks: start..index,
                chapter_group,
            });
            start = index;
            bytes = 0;
        }
        if let Some(chapter) = chapter {
            title = chapter;
            chapter_group += 1;
        }
        bytes += rendered.html.len();
    }
    sections.push(Section {
        path: section_path(sections.len()),
        title,
        blocks: start..book.blocks.len(),
        chapter_group,
    });
    sections
}

fn section_path(index: usize) -> String {
    if index == 0 {
        CONTENT.to_owned()
    } else {
        format!("chapter-{index}.xhtml")
    }
}

fn block_id(block: &Block) -> Option<Cow<'_, str>> {
    match block {
        Block::Heading { id, .. } | Block::Note { id, .. } => Some(Cow::Borrowed(id)),
        Block::PageAnchor { page } => Some(Cow::Owned(format!("source-page-{page}"))),
        _ => None,
    }
}

fn index_targets<'a>(
    book: &Book,
    rendered: &[RenderedBlock],
    sections: &'a [Section<'_>],
) -> HashMap<String, &'a str> {
    let mut targets = HashMap::new();
    for section in sections {
        for index in section.blocks.clone() {
            if let Some(id) = block_id(&book.blocks[index]) {
                targets.insert(id.into_owned(), section.path.as_str());
            }
            for captures in NOTE_REFERENCE_ID.captures_iter(&rendered[index].html) {
                targets.insert(captures[1].to_owned(), section.path.as_str());
            }
        }
    }
    targets
}

fn relative_path<'a>(from: &str, to: &'a str) -> &'a str {
    if from == to { "" } else { to }
}

fn image_extension(media_type: &str) -> io::Result<&'static str> {
    match media_type {
        "image/png" => Ok("png"),
        "image/jpeg" => Ok("jpg"),
        "image/gif" => Ok("gif"),
        "image/webp" => Ok("webp"),
        "image/svg+xml" => Ok("svg"),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported EPUB image media type {media_type:?}"),
        )),
    }
}

fn xhtml_start(title: &str, language: &str) -> String {
    let title = escape(title);
    let language = escape(language);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="{language}" lang="{language}">
<head><title>{title}</title><link rel="stylesheet" href="book.css"/></head>
<body>
"#
    )
}

fn navigation(book: &Book, headings: &[Heading<'_>], targets: &HashMap<String, &str>) -> String {
    let mut nav = xhtml_start(&book.title, &book.language);
    nav.push_str("<nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1>\n");
    contents_list(&mut nav, book, headings, targets, "nav.xhtml");
    nav.push_str("</nav>\n</body></html>\n");
    nav
}

fn navigation_headings(book: &Book) -> Vec<Heading<'_>> {
    if book.navigation.is_empty() {
        book.blocks
            .iter()
            .filter_map(|block| match block {
                Block::Heading { level, id, text } => Some(Heading {
                    level: (*level).clamp(1, 6),
                    id,
                    title: text,
                }),
                _ => None,
            })
            .collect()
    } else {
        entry_headings(&book.navigation)
    }
}

fn entry_headings(entries: &[NavEntry]) -> Vec<Heading<'_>> {
    entries
        .iter()
        .map(|entry| Heading {
            level: entry.level.clamp(1, 6),
            id: &entry.id,
            title: &entry.text,
        })
        .collect()
}

fn contents_list(
    nav: &mut String,
    book: &Book,
    headings: &[Heading<'_>],
    targets: &HashMap<String, &str>,
    current_path: &str,
) {
    if headings.is_empty() {
        writeln!(
            nav,
            "<ol><li><a href=\"{CONTENT}\">{}</a></li></ol>",
            escape(&book.title)
        )
        .unwrap();
    } else {
        nav_list(
            nav,
            &mut headings.iter().peekable(),
            targets,
            current_path,
            0,
        );
    }
}

fn nav_list(
    nav: &mut String,
    headings: &mut Peekable<std::slice::Iter<'_, Heading<'_>>>,
    targets: &HashMap<String, &str>,
    current_path: &str,
    parent_level: u8,
) {
    nav.push_str("<ol>\n");
    while headings
        .peek()
        .is_some_and(|heading| heading.level > parent_level)
    {
        let heading = headings.next().expect("checked next heading");
        let target = targets.get(heading.id).copied().unwrap_or(CONTENT);
        let path = relative_path(current_path, target);
        writeln!(
            nav,
            "<li><a href=\"{path}#{}\">{}</a>",
            escape(heading.id),
            escape(heading.title)
        )
        .unwrap();
        if headings
            .peek()
            .is_some_and(|next| next.level > heading.level)
        {
            nav_list(nav, headings, targets, current_path, heading.level);
        }
        nav.push_str("</li>\n");
    }
    nav.push_str("</ol>\n");
}

fn package(book: &Book, id: &str, manifest: &str, spine: &str) -> String {
    let title = escape(&book.title);
    let language = escape(&book.language);
    let mut metadata = String::new();
    for author in &book.authors {
        writeln!(metadata, "  <dc:creator>{}</dc:creator>", escape(author)).unwrap();
    }
    if let Some(year) = book.year {
        writeln!(metadata, "  <dc:date>{year:04}</dc:date>").unwrap();
    }
    if let Some(edition) = &book.edition {
        writeln!(
            metadata,
            "  <meta property=\"schema:bookEdition\">{}</meta>",
            escape(edition)
        )
        .unwrap();
    }
    if let Some(publisher) = &book.publisher {
        writeln!(
            metadata,
            "  <dc:publisher>{}</dc:publisher>",
            escape(publisher)
        )
        .unwrap();
    }
    if let Some(description) = &book.description {
        writeln!(
            metadata,
            "  <dc:description>{}</dc:description>",
            escape(description)
        )
        .unwrap();
    }
    let modified = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="publication-id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:identifier id="publication-id">{id}</dc:identifier>
  <dc:title>{title}</dc:title>
  <dc:language>{language}</dc:language>
{metadata}  <meta property="dcterms:modified">{modified}</meta>
</metadata>
<manifest>
  <item id="navigation" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  <item id="stylesheet" href="book.css" media-type="text/css"/>
  {manifest}
</manifest>
<spine>{spine}</spine>
</package>
"#
    )
}

fn identifier(fingerprint: Sha256) -> String {
    let digest = fingerprint.finalize();
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("128-bit UUID prefix");
    // RFC 9562 UUID version 8 permits application-defined hash-derived IDs.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::from("urn:uuid:");
    for (index, byte) in bytes.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        write!(id, "{byte:02x}").unwrap();
    }
    id
}

fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\u{0}'..='\u{8}'
            | '\u{b}'
            | '\u{c}'
            | '\u{e}'..='\u{1f}'
            | '\u{fffe}'
            | '\u{ffff}' => {}
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
#[path = "epub/tests/chapter_documents.rs"]
mod chapter_documents;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use lopdf::{Document, Object, dictionary};
    use serde_json::json;

    use super::*;
    use crate::{MetadataOverrides, book};

    #[test]
    fn package_metadata_uses_overrides_and_retains_the_source_language() {
        let mut pdf = Document::new();
        let catalog = pdf.add_object(dictionary! {
            "Type" => "Catalog",
            "Lang" => Object::string_literal("fr"),
        });
        pdf.trailer.set("Root", catalog);
        pdf.trailer.set(
            "Info",
            dictionary! {
                "Title" => Object::string_literal("Original title"),
                "Author" => Object::string_literal("Original author"),
            },
        );
        let book = book::normalize(
            &json!({}),
            &pdf,
            Path::new("source.pdf"),
            MetadataOverrides {
                title: Some("Notes de géométrie <révisées>".into()),
                authors: vec!["A & B".into(), "C. Author".into()],
                year: Some(2015),
                edition: Some("Revised & expanded".into()),
                publisher: Some("Press & Co.".into()),
                description: Some("An introduction to <math>.".into()),
                ..MetadataOverrides::default()
            },
        );
        let xml = package(&book, "test", "", "");
        let opf = roxmltree::Document::parse(&xml).unwrap();
        let values = |name| {
            opf.descendants()
                .filter(|node| node.has_tag_name(("http://purl.org/dc/elements/1.1/", name)))
                .filter_map(|node| node.text())
                .collect::<Vec<_>>()
        };
        assert_eq!(values("title"), ["Notes de géométrie <révisées>"]);
        assert_eq!(values("creator"), ["A & B", "C. Author"]);
        assert_eq!(values("date"), ["2015"]);
        assert_eq!(
            opf.descendants()
                .find(|node| node.attribute("property") == Some("schema:bookEdition"))
                .and_then(|node| node.text()),
            Some("Revised & expanded")
        );
        assert_eq!(values("publisher"), ["Press & Co."]);
        assert_eq!(values("description"), ["An introduction to <math>."]);
        assert_eq!(values("language"), ["fr"]);
    }
}
