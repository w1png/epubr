mod code;
mod cover;
mod lists;
mod math;
mod metadata;
mod notes;
mod order;
mod reflow;
mod structure;
mod superscripts;
mod symbols;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD};
use lopdf::{Document, Object};
use serde_json::Value;

use crate::{MathMlMode, MetadataOverrides};

pub(crate) use math::{SourceCrops, locate_placeholders};
use reflow::clean_text;
pub(crate) use structure::TableCell;
pub(crate) use symbols::repair_symbols;

pub(crate) struct Book {
    pub title: String,
    pub cover: Option<Vec<u8>>,
    pub authors: Vec<String>,
    pub language: String,
    pub year: Option<u16>,
    pub edition: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub blocks: Vec<Block>,
    pub navigation: Vec<NavEntry>,
    pub contents_navigation: Option<Vec<NavEntry>>,
    pub warnings: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct NavEntry {
    pub level: u8,
    pub id: String,
    pub text: String,
}

pub(crate) struct ListItem {
    pub content: ListItemContent,
    pub number: Option<u64>,
}

pub(crate) enum ListItemContent {
    Html(String),
    Formula(Formula),
}

pub(crate) struct SourceImage {
    pub media_type: String,
    pub data: Vec<u8>,
}

pub(crate) enum Formula {
    MathMl {
        markup: String,
        fallback: Option<SourceImage>,
    },
    Image(SourceImage),
}

pub(crate) enum Block {
    PageAnchor {
        page: u64,
    },
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
    List {
        ordered: bool,
        items: Vec<ListItem>,
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
    Formula(Formula),
    Code {
        text: String,
        caption: String,
    },
    Table {
        rows: Vec<Vec<TableCell>>,
    },
}

pub(crate) fn recover_source_regions(
    doc: &mut Value,
    pdf: &Document,
    input: &Path,
    decode_formulas: bool,
) {
    let Some(texts) = doc["texts"].as_array_mut() else {
        return;
    };
    let mut geometry = math::PageGeometry::new(pdf, input);
    let mut code = code::SourceCode::new(input);
    for item in texts {
        let source = match label(item) {
            "formula" if decode_formulas => geometry.recover_formula(item),
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

pub fn is_language_tag(value: &str) -> bool {
    value.split('-').all(|subtag| {
        !subtag.is_empty() && subtag.len() <= 8 && subtag.bytes().all(|c| c.is_ascii_alphanumeric())
    })
}

pub(crate) fn normalize(
    doc: &Value,
    pdf: &Document,
    input: &Path,
    mut overrides: MetadataOverrides,
    math_ml: MathMlMode,
) -> Book {
    let inferred = metadata::infer(pdf, input);
    let cover = cover::render(
        doc,
        pdf,
        input,
        inferred.title.as_deref().unwrap_or("Untitled"),
    );
    metadata::fill(&mut overrides, inferred);

    let mut book = Book {
        cover,
        title: overrides.title.unwrap_or_else(|| "Untitled".to_owned()),
        authors: overrides.authors,
        language: overrides.language.unwrap_or_else(|| "und".to_owned()),
        year: overrides.year,
        edition: overrides.edition,
        publisher: overrides.publisher,
        description: overrides.description,
        blocks: Vec::new(),
        navigation: Vec::new(),
        contents_navigation: None,
        warnings: Vec::new(),
    };
    let mut items = ordered_items(doc);
    if book.cover.is_some() {
        items.retain(|item| page_no(item) != 1);
    }
    order::repair(&mut items);
    let reflow = reflow::Reflow::new(doc);
    let mut texts: Vec<_> = items.iter().map(|item| reflow.text(item)).collect();
    let numbering = lists::recover_numbering(&items, &mut texts);
    let mut superscripts = superscripts::Superscripts::new(input);
    let notes = notes::link_notes(&items, &texts, &mut book.warnings, |item, text| {
        superscripts.html(item, text)
    });
    let contents = structure::printed_contents(&items, pdf, input);
    let figure_labels = structure::figure_labels(&items);
    let mut geometry = math::PageGeometry::new(pdf, input);
    let mut crops = math::SourceCrops::new(input);
    let mut source_code = code::SourceCode::new(input);
    let mut headings = Vec::new();
    let mut page_starts = HashMap::new();
    let mut previous_prose: Option<(&Value, usize)> = None;
    let mut list_run = None;
    for (index, (item, text)) in items.iter().zip(texts).enumerate() {
        if matches!(label(item), "page_header" | "page_footer") {
            continue;
        }
        let previous = previous_prose.take();
        let previous_list = list_run.take();
        let page = page_no(item);
        if contents.position(index) {
            page_starts.entry(page).or_insert(book.blocks.len());
            book.blocks.push(Block::Contents {
                title: text.clone(),
            });
        }
        if contents.contains(index) || figure_labels.contains(&index) {
            continue;
        }
        if page > 0 {
            page_starts.entry(page).or_insert(book.blocks.len());
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
                previous_prose = previous.filter(|(prose, _)| page_no(prose) == page_no(item));
            }
            "formula" => {
                let markup = (math_ml != MathMlMode::Disable)
                    .then(|| formula_mathml(item, &text, &mut geometry))
                    .flatten();
                if math_ml != MathMlMode::Disable && markup.is_none() {
                    book.warnings.push(format!(
                        "Page {}: formula could not be represented as MathML",
                        page
                    ));
                }
                if let Some(formula) = formula_with_image(math_ml, markup, true, || {
                    formula_image(&mut crops, doc, item, &mut book.warnings)
                }) {
                    book.blocks.push(Block::Formula(formula));
                } else if math_ml == MathMlMode::Disable
                    && !math::placeholder(&text)
                    && item["epubr_formula_undecoded"] != true
                {
                    paragraph(&mut book.blocks, &text);
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
            "list_item" if !text.is_empty() => {
                let recovered_number = numbering.get(&index).copied();
                let ordered = item["enumerated"] == true || recovered_number.is_some();
                let number = recovered_number.or_else(|| {
                    item["marker"]
                        .as_str()?
                        .trim()
                        .strip_suffix('.')?
                        .parse()
                        .ok()
                });
                let group = (item["parent"]["$ref"].as_str(), ordered);
                let reference = notes.references.get(&index);
                let formula = if reference.is_none() && math::standalone_equation(&text) {
                    let (_, markup) =
                        standalone_mathml(math_ml, item, &text, &mut geometry, &mut superscripts);
                    formula_with_image(math_ml, markup, false, || {
                        formula_image(&mut crops, doc, item, &mut book.warnings)
                    })
                } else {
                    None
                };
                let entry = ListItem {
                    content: formula.map_or_else(
                        || {
                            ListItemContent::Html(prose_html(
                                item,
                                text,
                                reference,
                                &mut geometry,
                                &mut superscripts,
                            ))
                        },
                        ListItemContent::Formula,
                    ),
                    number,
                };
                if previous_list == Some(group)
                    && let Some(Block::List { items, .. }) = book.blocks.last_mut()
                {
                    items.push(entry);
                } else {
                    book.blocks.push(Block::List {
                        ordered,
                        items: vec![entry],
                    });
                }
                list_run = Some(group);
            }
            _ if !text.is_empty() => {
                let reference = notes.references.get(&index);
                if reference.is_none() && math::standalone_equation(&text) {
                    let (raw, markup) =
                        standalone_mathml(math_ml, item, &text, &mut geometry, &mut superscripts);
                    if let Some(formula) = formula_with_image(math_ml, markup, false, || {
                        formula_image(&mut crops, doc, item, &mut book.warnings)
                    }) {
                        book.blocks.push(Block::Formula(formula));
                    } else {
                        book.blocks.push(Block::Paragraph {
                            html: superscripts.html(item, &raw),
                        });
                    }
                    continue;
                }
                let html = prose_html(item, text, reference, &mut geometry, &mut superscripts);
                if let Some((previous_item, previous_block)) = previous
                    && let Block::Paragraph { html: prior } = &mut book.blocks[previous_block]
                    && reflow.join(doc, previous_item, item, prior, &html)
                {
                    previous_prose = Some((*item, previous_block));
                } else {
                    previous_prose = Some((*item, book.blocks.len()));
                    book.blocks.push(Block::Paragraph { html });
                }
            }
            _ => {}
        }
    }
    (book.navigation, book.contents_navigation) =
        contents.navigation(&mut book.blocks, &headings, &page_starts);
    if book.navigation.is_empty() {
        book.navigation = outline_navigation(pdf, &book.blocks, &headings);
    }
    book
}

fn formula_mathml(
    item: &Value,
    text: &str,
    geometry: &mut math::PageGeometry<'_>,
) -> Option<String> {
    let raw = if math::placeholder(text) || item["epubr_formula_undecoded"] == true {
        ""
    } else {
        text
    };
    let raw = if item["epubr_source_text"] == true {
        Cow::Borrowed(raw)
    } else if item["epubr_enriched"] == true {
        Cow::Owned(math::compact_decoded_numbers(raw))
    } else {
        geometry
            .recover_formula(item)
            .or_else(|| geometry.recover_overbar(item, raw))
            .map_or(Cow::Borrowed(raw), Cow::Owned)
    };
    math::presentation(&raw).filter(|markup| {
        item["epubr_enriched"] != true
            || item["epubr_formula_source_ink"]
                .as_str()
                .is_none_or(|source| math::consistent_with_source(markup, source))
    })
}

fn standalone_mathml<'a>(
    mode: MathMlMode,
    item: &Value,
    text: &'a str,
    geometry: &mut math::PageGeometry<'_>,
    superscripts: &mut superscripts::Superscripts<'_>,
) -> (Cow<'a, str>, Option<String>) {
    if mode == MathMlMode::Disable {
        return (Cow::Borrowed(text), None);
    }
    let raw = geometry
        .recover_overbar(item, text)
        .map_or(Cow::Borrowed(text), Cow::Owned);
    let markup = math::presentation(&superscripts.expression(item, &raw));
    (raw, markup)
}

fn formula_with_image(
    mode: MathMlMode,
    markup: Option<String>,
    image_when_undecodable: bool,
    image: impl FnOnce() -> Option<SourceImage>,
) -> Option<Formula> {
    match markup {
        Some(markup) => Some(Formula::MathMl {
            markup,
            fallback: (mode == MathMlMode::Fallback).then(image).flatten(),
        }),
        None if image_when_undecodable || mode != MathMlMode::Enable => image().map(Formula::Image),
        None => None,
    }
}

fn formula_image(
    crops: &mut SourceCrops<'_>,
    doc: &Value,
    item: &Value,
    warnings: &mut Vec<String>,
) -> Option<SourceImage> {
    if let Some((media_type, data)) = crops.crop(doc, item) {
        return Some(SourceImage { media_type, data });
    }
    warnings.push(format!(
        "Page {}: no source crop is available for the formula",
        page_no(item)
    ));
    None
}

fn prose_html(
    item: &Value,
    text: String,
    note_reference: Option<&String>,
    geometry: &mut math::PageGeometry<'_>,
    superscripts: &mut superscripts::Superscripts<'_>,
) -> String {
    if let Some(reference) = note_reference {
        return reference.clone();
    }
    superscripts.html(item, &geometry.recover_inline(item, &text).unwrap_or(text))
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

fn outline_navigation(
    pdf: &Document,
    blocks: &[Block],
    headings: &[(u64, usize)],
) -> Vec<NavEntry> {
    let mut entries = Vec::new();
    let mut used = HashSet::new();
    for entry in structure::pdf_outline(pdf) {
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
            let captions = structure::caption_refs(doc, item);
            stack.extend(
                children
                    .iter()
                    .rev()
                    .filter(|child| !captions.contains(child))
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
    structure::caption_refs(doc, item)
        .iter()
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn joins_page_fragments_across_a_footnote_and_keeps_its_reference() {
        let doc = json!({
            "body": {"children": [
                {"$ref": "#/texts/0"}, {"$ref": "#/texts/1"}, {"$ref": "#/texts/2"}
            ]},
            "pages": {
                "1": {"size": {"width": 500, "height": 600}},
                "2": {"size": {"width": 500, "height": 600}}
            },
            "texts": [
                {"label":"text", "text":"The result [1] depends on", "prov":[
                    {"page_no":1,"bbox":{"l":50,"r":450,"t":150,"b":78,"coord_origin":"BOTTOMLEFT"}}
                ]},
                {"label":"footnote", "text":"1 A source note.", "prov":[{"page_no":1}]},
                {"label":"text", "text":"the chosen architecture.", "prov":[
                    {"page_no":2,"bbox":{"l":50,"r":450,"t":540,"b":480,"coord_origin":"BOTTOMLEFT"}}
                ]}
            ]
        });

        let book = normalize(
            &doc,
            &Document::new(),
            Path::new("source.pdf"),
            MetadataOverrides::default(),
            MathMlMode::default(),
        );
        let [Block::Paragraph { html }, Block::Note { id, html: note }] = book.blocks.as_slice()
        else {
            panic!("expected one continued paragraph followed by the note");
        };
        assert!(html.ends_with("depends on the chosen architecture."));
        assert!(html.contains(&format!("href=\"#{id}\"")));
        assert!(note.contains("href=\"#noteref-2\""));
    }
}
