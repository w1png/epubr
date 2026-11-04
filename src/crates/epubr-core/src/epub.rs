use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Write};
use std::iter::Peekable;

use chrono::{SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::book::{Block, Book};

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

struct Heading<'a> {
    level: u8,
    id: &'a str,
    title: &'a str,
}

pub(crate) fn write(file: &mut File, book: &Book) -> io::Result<()> {
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;
    zip.start_file("META-INF/container.xml", deflated)?;
    zip.write_all(CONTAINER.as_bytes())?;
    zip.start_file("EPUB/book.css", deflated)?;
    zip.write_all(STYLES.as_bytes())?;

    let mut content = xhtml_start(&book.title, &book.language);
    let headings = navigation_headings(book);
    let mut image_manifest = String::new();
    let mut image_count = 0;
    let mut has_mathml = false;
    let mut fingerprint = Sha256::new();

    if let Some(cover) = &book.cover {
        zip.start_file("EPUB/cover.png", stored)?;
        zip.write_all(cover)?;
        fingerprint.update(Sha256::digest(cover));
        let xhtml = xhtml_start(&book.title, &book.language)
            .replace("<body>", "<body class=\"cover\" epub:type=\"cover\">");
        zip.start_file("EPUB/cover.xhtml", deflated)?;
        zip.write_all(
            format!(
                "{xhtml}<img src=\"cover.png\" alt=\"Cover of {}\"/></body></html>\n",
                escape(&book.title)
            )
            .as_bytes(),
        )?;
        image_manifest.push_str(
            "<item id=\"cover-page\" href=\"cover.xhtml\" media-type=\"application/xhtml+xml\"/>\n\
             <item id=\"cover-image\" href=\"cover.png\" media-type=\"image/png\" properties=\"cover-image\"/>\n",
        );
    }
    for block in &book.blocks {
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
            Block::Contents { title } => {
                writeln!(
                    content,
                    "<nav class=\"contents\" aria-labelledby=\"contents-heading\"><h2 id=\"contents-heading\">{}</h2>",
                    escape(title)
                )
                .unwrap();
                contents_list(&mut content, book, &headings, "");
                content.push_str("</nav>\n");
            }
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
                let extension = image_extension(media_type)?;
                image_count += 1;
                let href = format!("images/image-{image_count}.{extension}");
                zip.start_file(format!("EPUB/{href}"), stored)?;
                zip.write_all(data)?;
                fingerprint.update(Sha256::digest(data));
                writeln!(
                    image_manifest,
                    "<item id=\"image-{image_count}\" href=\"{href}\" media-type=\"{media_type}\"/>"
                )
                .unwrap();
                let Block::Figure { caption, .. } = block else {
                    writeln!(content, "<div class=\"formula-image\"><img src=\"{href}\" alt=\"Equation from source PDF\"/></div>").unwrap();
                    continue;
                };
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
    }

    content.push_str("</body></html>\n");
    fingerprint.update(content.as_bytes());
    for (index, author) in book.authors.iter().enumerate() {
        if index > 0 {
            fingerprint.update([0]);
        }
        fingerprint.update(author.as_bytes());
    }
    zip.start_file(format!("EPUB/{CONTENT}"), deflated)?;
    zip.write_all(content.as_bytes())?;

    zip.start_file("EPUB/nav.xhtml", deflated)?;
    zip.write_all(navigation(book, &headings).as_bytes())?;
    zip.start_file("EPUB/book.opf", deflated)?;
    zip.write_all(package(book, &identifier(fingerprint), &image_manifest, has_mathml).as_bytes())?;
    zip.finish()?;
    Ok(())
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

fn navigation(book: &Book, headings: &[Heading<'_>]) -> String {
    let mut nav = xhtml_start(&book.title, &book.language);
    nav.push_str("<nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1>\n");
    contents_list(&mut nav, book, headings, CONTENT);
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
        book.navigation
            .iter()
            .map(|entry| Heading {
                level: entry.level.clamp(1, 6),
                id: &entry.id,
                title: &entry.text,
            })
            .collect()
    }
}

fn contents_list(nav: &mut String, book: &Book, headings: &[Heading<'_>], path: &str) {
    if headings.is_empty() {
        writeln!(
            nav,
            "<ol><li><a href=\"{path}\">{}</a></li></ol>",
            escape(&book.title)
        )
        .unwrap();
    } else {
        nav_list(nav, &mut headings.iter().peekable(), path, 0);
    }
}

fn nav_list(
    nav: &mut String,
    headings: &mut Peekable<std::slice::Iter<'_, Heading<'_>>>,
    path: &str,
    parent_level: u8,
) {
    nav.push_str("<ol>\n");
    while headings
        .peek()
        .is_some_and(|heading| heading.level > parent_level)
    {
        let heading = headings.next().expect("checked next heading");
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
            nav_list(nav, headings, path, heading.level);
        }
        nav.push_str("</li>\n");
    }
    nav.push_str("</ol>\n");
}

fn package(book: &Book, id: &str, image_manifest: &str, has_mathml: bool) -> String {
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
    let properties = if has_mathml {
        r#" properties="mathml""#
    } else {
        ""
    };
    let cover = if book.cover.is_some() {
        r#"<itemref idref="cover-page"/>"#
    } else {
        ""
    };
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
  <item id="content" href="{CONTENT}" media-type="application/xhtml+xml"{properties}/>
  <item id="navigation" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  <item id="stylesheet" href="book.css" media-type="text/css"/>
  {image_manifest}
</manifest>
<spine>{cover}<itemref idref="content"/></spine>
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
        let xml = package(&book, "test", "", false);
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
