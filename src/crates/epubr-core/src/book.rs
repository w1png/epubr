mod code;
mod math;
mod notes;
mod reflow;
mod structure;

use std::collections::HashSet;
use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD};
use lopdf::{Document, Object, decode_text_string};
use serde_json::Value;

pub(crate) use math::{SourceCrops, locate_placeholders};
use reflow::clean_text;
pub(crate) use structure::TableCell;

pub(crate) struct Book {
    pub title: String,
    pub author: Option<String>,
    pub language: String,
    pub blocks: Vec<Block>,
    pub navigation: Vec<NavEntry>,
    pub warnings: Vec<String>,
}

pub(crate) struct NavEntry {
    pub level: u8,
    pub id: String,
    pub text: String,
}

pub(crate) enum Block {
    Contents {
        title: String,
    },
    Heading {
        level: u8,
        id: String,
        text: String,
    },
    Paragraph {
        html: String,
    },
    Figure {
        media_type: String,
        data: Vec<u8>,
        caption: String,
    },
    Note {
        id: String,
        html: String,
    },
    Formula {
        mathml: String,
    },
    Code {
        text: String,
        caption: String,
    },
    Table {
        rows: Vec<Vec<TableCell>>,
    },
}

pub(crate) fn recover_source_regions(doc: &mut Value, pdf: &Document, input: &Path) {
    let Some(texts) = doc["texts"].as_array_mut() else {
        return;
    };
    let mut geometry = math::PageGeometry::new(pdf, input);
    let mut code = code::SourceCode::new(input);
    for item in texts {
        let source = match label(item) {
            "formula" => geometry.recover_formula(item),
            "code" => code.recover(item),
            _ => None,
        };
        if let Some(source) = source {
            item["text"] = Value::String(source);
            item["epubr_source_text"] = Value::Bool(true);
            if label(item) == "formula" {
                item["epubr_formula_undecoded"] = Value::Bool(false);
            }
        }
    }
}

pub(crate) fn normalize(doc: &Value, pdf: &Document, input: &Path) -> Book {
    let info = pdf
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|obj| resolve(pdf, obj).as_dict().ok());
    let title = info
        .and_then(|info| metadata(pdf, info.get(b"Title").ok()))
        .or_else(|| {
            input
                .file_stem()
                .and_then(|name| name.to_str())
                .map(clean_text)
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Untitled".to_owned());
    let author = info.and_then(|info| metadata(pdf, info.get(b"Author").ok()));
    let language = pdf
        .catalog()
        .ok()
        .and_then(|catalog| metadata(pdf, catalog.get(b"Lang").ok()))
        .filter(|lang| {
            lang.split('-').all(|subtag| {
                !subtag.is_empty()
                    && subtag.len() <= 8
                    && subtag.bytes().all(|c| c.is_ascii_alphanumeric())
            })
        })
        .unwrap_or_else(|| "und".to_owned());

    let mut book = Book {
        title,
        author,
        language,
        blocks: Vec::new(),
        navigation: Vec::new(),
        warnings: Vec::new(),
    };
    let items = ordered_items(doc);
    let notes = notes::link_notes(&items, &mut book.warnings);
    let contents = structure::printed_contents(&items);
    let figure_labels = structure::figure_labels(&items);
    let reflow = reflow::Reflow::new(doc);
    let mut geometry = math::PageGeometry::new(pdf, input);
    let mut crops = math::SourceCrops::new(input);
    let mut source_code = code::SourceCode::new(input);
    let mut headings = Vec::new();
    let mut previous_prose = None;
    for (index, item) in items.iter().enumerate() {
        if matches!(label(item), "page_header" | "page_footer") {
            continue;
        }
        let previous = previous_prose.take();
        let text = reflow.text(item);
        if contents.position(index) {
            book.blocks.push(Block::Contents {
                title: text.clone(),
            });
        }
        if contents.contains(index) || figure_labels.contains(&index) {
            continue;
        }
        match label(item) {
            "section_header" | "title" if !text.is_empty() => {
                let level = item["level"].as_u64().unwrap_or(1).clamp(1, 6) as u8;
                let id = format!("heading-{index}");
                headings.push((page_no(item), book.blocks.len()));
                book.blocks.push(Block::Heading { level, id, text });
            }
            "picture" => {
                let caption = caption(doc, item);
                if let Some((media_type, data)) = embedded_image(&item["image"]) {
                    book.blocks.push(Block::Figure {
                        media_type,
                        data,
                        caption,
                    });
                } else {
                    book.warnings.push(format!(
                        "Page {}: figure has no embedded image",
                        page_no(item)
                    ));
                    paragraph(&mut book.blocks, &caption);
                }
            }
            "table" => {
                let caption = caption(doc, item);
                if let Some(rows) = structure::table_rows(item) {
                    paragraph(&mut book.blocks, &caption);
                    book.blocks.push(Block::Table { rows });
                } else if let Some((media_type, data)) = crops.crop(doc, item) {
                    book.blocks.push(Block::Figure {
                        media_type,
                        data,
                        caption,
                    });
                } else {
                    book.warnings.push(format!(
                        "Page {}: table layout could not be recovered",
                        page_no(item)
                    ));
                    paragraph(&mut book.blocks, &caption);
                    if let Some(cells) = item["data"]["table_cells"].as_array() {
                        for cell in cells {
                            paragraph(&mut book.blocks, &item_text(cell));
                        }
                    }
                }
            }
            "footnote" if !text.is_empty() => {
                let id = format!("note-{index}");
                let mut html = escape_xml(&text);
                if let Some(reference) = notes.backlinks.get(&index) {
                    html.push_str(&format!(
                        " <a href=\"#{reference}\" aria-label=\"Return to note reference\">↩</a>"
                    ));
                }
                book.blocks.push(Block::Note { id, html });
            }
            "formula" => {
                let raw = if math::placeholder(&text) || item["epubr_formula_undecoded"] == true {
                    String::new()
                } else {
                    text
                };
                let raw = if item["epubr_source_text"] == true {
                    raw
                } else {
                    geometry
                        .recover_formula(item)
                        .or_else(|| geometry.recover_overbar(item, &raw))
                        .unwrap_or(raw)
                };
                if let Some(mathml) = math::presentation(&raw) {
                    book.blocks.push(Block::Formula { mathml });
                } else {
                    book.warnings.push(format!(
                        "Page {}: formula could not be represented as MathML",
                        page_no(item)
                    ));
                    if let Some((media_type, data)) = crops.crop(doc, item) {
                        book.blocks.push(Block::Figure {
                            media_type,
                            data,
                            caption: raw,
                        });
                    } else {
                        paragraph(&mut book.blocks, &raw);
                    }
                }
            }
            "code" => {
                let caption = caption(doc, item);
                let code = if item["epubr_source_text"] == true {
                    item["text"].as_str().map(str::to_owned)
                } else {
                    source_code.recover(item).or_else(|| {
                        (item["epubr_enriched"] == true || text.contains('\n'))
                            .then(|| text.clone())
                    })
                };
                if let Some(text) = code {
                    book.blocks.push(Block::Code { text, caption });
                } else if let Some((media_type, data)) = crops.crop(doc, item) {
                    book.blocks.push(Block::Figure {
                        media_type,
                        data,
                        caption,
                    });
                } else {
                    paragraph(&mut book.blocks, &text);
                    paragraph(&mut book.blocks, &caption);
                }
            }
            _ if math::placeholder(&text) => {
                book.warnings.push(format!(
                    "Page {}: formula could not be located in the PDF",
                    page_no(item)
                ));
            }
            _ if !text.is_empty() => {
                if let Some(html) = notes.references.get(&index) {
                    book.blocks.push(Block::Paragraph {
                        html: html.to_owned(),
                    });
                } else if math::standalone_equation(&text) {
                    let raw = geometry.recover_overbar(item, &text).unwrap_or(text);
                    if let Some(mathml) = math::presentation(&raw) {
                        book.blocks.push(Block::Formula { mathml });
                    } else {
                        paragraph(&mut book.blocks, &raw);
                    }
                } else {
                    let text = geometry.recover_inline(item, &text).unwrap_or(text);
                    let html = escape_xml(&text);
                    let joined = match previous.zip(book.blocks.last_mut()) {
                        Some((previous, Block::Paragraph { html: prior })) => {
                            reflow.join(doc, previous, item, prior, &html)
                        }
                        _ => false,
                    };
                    if !joined {
                        book.blocks.push(Block::Paragraph { html });
                    }
                    previous_prose = Some(*item);
                }
            }
            _ => {}
        }
    }
    book.navigation = contents.navigation(&book.blocks, &headings);
    if book.navigation.is_empty() {
        book.navigation = outline_navigation(pdf, &book.blocks, &headings);
    }
    book
}

fn paragraph(blocks: &mut Vec<Block>, text: &str) {
    if !text.is_empty() {
        blocks.push(Block::Paragraph {
            html: escape_xml(text),
        });
    }
}

fn resolve<'a>(pdf: &'a Document, obj: &'a Object) -> &'a Object {
    obj.as_reference()
        .ok()
        .and_then(|id| pdf.get_object(id).ok())
        .unwrap_or(obj)
}

fn metadata(pdf: &Document, object: Option<&Object>) -> Option<String> {
    decode_text_string(resolve(pdf, object?))
        .ok()
        .map(|value| clean_text(&value))
        .filter(|value| !value.is_empty())
}

fn outline_navigation(
    pdf: &Document,
    blocks: &[Block],
    headings: &[(u64, usize)],
) -> Vec<NavEntry> {
    let Ok(toc) = pdf.get_toc() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut used = HashSet::new();
    for entry in toc.toc {
        let page = entry.page as u64;
        let title = clean_text(&entry.title);
        // Match the actual heading on that source page. A page-only bookmark
        // cannot distinguish multiple sections that start on the same page.
        let mut candidates = headings.iter().filter_map(|(source_page, index)| {
            if *source_page == page
                && let Block::Heading { id, text, .. } = &blocks[*index]
                && text.split_whitespace().eq(title.split_whitespace())
            {
                Some(id)
            } else {
                None
            }
        });
        match candidates.next() {
            Some(id) if candidates.next().is_none() && used.insert(id) => {
                entries.push(NavEntry {
                    level: entry.level.clamp(1, 6) as u8,
                    id: id.clone(),
                    text: title,
                });
            }
            None if !headings.iter().any(|(source_page, _)| *source_page == page) => {}
            _ => return Vec::new(),
        }
    }
    entries
}

fn get_ref<'a>(doc: &'a Value, value: &Value) -> Option<&'a Value> {
    let pointer = value["$ref"].as_str()?.strip_prefix('#')?;
    doc.pointer(pointer)
}

fn ordered_items(doc: &Value) -> Vec<&Value> {
    let mut items = Vec::new();
    let mut stack = vec![&doc["body"]];
    let mut seen = HashSet::new();
    while let Some(item) = stack.pop() {
        if let Some(reference) = item["self_ref"].as_str()
            && !seen.insert(reference)
        {
            continue;
        }
        items.push(item);
        if let Some(children) = item["children"].as_array() {
            let captions = item["captions"].as_array();
            stack.extend(
                children
                    .iter()
                    .rev()
                    .filter(|child| captions.is_none_or(|captions| !captions.contains(child)))
                    .filter_map(|child| get_ref(doc, child)),
            );
        }
    }
    items
}

fn label(item: &Value) -> &str {
    item["label"].as_str().unwrap_or_default()
}

fn page_no(item: &Value) -> u64 {
    item["prov"][0]["page_no"].as_u64().unwrap_or_default()
}

fn item_text(item: &Value) -> String {
    clean_text(item["text"].as_str().unwrap_or_default())
}

fn caption(doc: &Value, item: &Value) -> String {
    item["captions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|reference| get_ref(doc, reference))
        .map(item_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn embedded_image(value: &Value) -> Option<(String, Vec<u8>)> {
    let (format, encoded) = value["uri"]
        .as_str()?
        .strip_prefix("data:")?
        .split_once(";base64,")?;
    if !matches!(
        format,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return None;
    }
    let bytes = STANDARD.decode(encoded).ok()?;
    (!bytes.is_empty()).then(|| (format.to_owned(), bytes))
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
