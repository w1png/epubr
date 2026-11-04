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
figcaption, aside, .formula-source { font-size: 0.9em; }
.formula-source { white-space: pre-wrap; overflow-wrap: break-word; }
table { border-collapse: collapse; }
td { padding: 0.2em 0.4em; border: 1px solid; }
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
    let mut headings = Vec::new();
    let mut image_manifest = String::new();
    let mut image_count = 0;
    let mut has_mathml = false;
    let mut fingerprint = Sha256::new();

    for block in &book.blocks {
        match block {
            Block::Heading { level, id, text } => {
                let level = (*level).clamp(1, 6);
                headings.push(Heading {
                    level,
                    id,
                    title: text,
                });
                writeln!(
                    content,
                    "<h{level} id=\"{}\">{}</h{level}>",
                    escape(id),
                    escape(text)
                )
                .unwrap();
            }
            Block::Paragraph { html } => {
                writeln!(content, "<p>{html}</p>").unwrap();
            }
            Block::Figure {
                media_type,
                data,
                caption,
            } => {
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
            Block::Formula { mathml, raw } => {
                content.push_str("<div class=\"formula\">\n");
                if !mathml.is_empty() {
                    has_mathml = true;
                    writeln!(content, "{mathml}").unwrap();
                }
                if !raw.is_empty() {
                    writeln!(content, "<p class=\"formula-source\">{}</p>", escape(raw)).unwrap();
                }
                content.push_str("</div>\n");
            }
            Block::Table { rows } => {
                content.push_str("<table><tbody>\n");
                for row in rows {
                    content.push_str("<tr>");
                    for cell in row {
                        write!(content, "<td>{}</td>", escape(cell)).unwrap();
                    }
                    content.push_str("</tr>\n");
                }
                content.push_str("</tbody></table>\n");
            }
        }
    }

    content.push_str("</body></html>\n");
    fingerprint.update(content.as_bytes());
    fingerprint.update(book.author.as_deref().unwrap_or_default().as_bytes());
    zip.start_file(format!("EPUB/{CONTENT}"), deflated)?;
    zip.write_all(content.as_bytes())?;

    zip.start_file("EPUB/nav.xhtml", deflated)?;
    zip.write_all(navigation(book, headings).as_bytes())?;
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

fn navigation(book: &Book, headings: Vec<Heading<'_>>) -> String {
    let mut nav = xhtml_start(&book.title, &book.language);
    nav.push_str("<nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1>\n");
    let headings = if book.navigation.is_empty() {
        headings
    } else {
        book.navigation
            .iter()
            .map(|entry| Heading {
                level: entry.level.clamp(1, 6),
                id: &entry.id,
                title: &entry.text,
            })
            .collect()
    };
    if headings.is_empty() {
        writeln!(
            nav,
            "<ol><li><a href=\"{CONTENT}\">{}</a></li></ol>",
            escape(&book.title)
        )
        .unwrap();
    } else {
        nav_list(&mut nav, &mut headings.into_iter().peekable(), 0);
    }
    nav.push_str("</nav>\n</body></html>\n");
    nav
}

fn nav_list<'a>(
    nav: &mut String,
    headings: &mut Peekable<impl Iterator<Item = Heading<'a>>>,
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
            "<li><a href=\"{CONTENT}#{}\">{}</a>",
            escape(heading.id),
            escape(heading.title)
        )
        .unwrap();
        if headings
            .peek()
            .is_some_and(|next| next.level > heading.level)
        {
            nav_list(nav, headings, heading.level);
        }
        nav.push_str("</li>\n");
    }
    nav.push_str("</ol>\n");
}

fn package(book: &Book, id: &str, image_manifest: &str, has_mathml: bool) -> String {
    let title = escape(&book.title);
    let language = escape(&book.language);
    let author = book.author.as_ref().map_or_else(String::new, |author| {
        format!("<dc:creator>{}</dc:creator>", escape(author))
    });
    let modified = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let properties = if has_mathml {
        r#" properties="mathml""#
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
  {author}
  <meta property="dcterms:modified">{modified}</meta>
</metadata>
<manifest>
  <item id="content" href="{CONTENT}" media-type="application/xhtml+xml"{properties}/>
  <item id="navigation" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  <item id="stylesheet" href="book.css" media-type="text/css"/>
  {image_manifest}
</manifest>
<spine><itemref idref="content"/></spine>
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
