use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::Path;

use docling_pdf::textparse::PageTextParser;
use lopdf::{Dictionary, Document, Object, ObjectId, Outline, TocType, decode_text_string};
use serde_json::Value;

use super::{Block, NavEntry, clean_text, get_ref, item_text, label, page_no, resolve};

pub(super) fn pdf_outline(pdf: &Document) -> Vec<TocType> {
    let Ok(Some(outlines)) = pdf.get_outlines(None, None, &mut Default::default()) else {
        return Vec::new();
    };
    let pages = pdf
        .get_pages()
        .into_iter()
        .map(|(page, id)| (id, page))
        .collect();
    let mut bookmarks = Vec::new();
    collect_bookmarks(pdf, &outlines, &pages, 1, &mut bookmarks);
    bookmarks
}

fn collect_bookmarks(
    pdf: &Document,
    outlines: &[Outline],
    pages: &HashMap<ObjectId, u32>,
    level: usize,
    bookmarks: &mut Vec<TocType>,
) {
    for outline in outlines {
        match outline {
            Outline::Destination(destination) => {
                let Some(page) = destination
                    .page()
                    .and_then(lopdf::Object::as_reference)
                    .ok()
                    .and_then(|id| pages.get(&id))
                else {
                    continue;
                };
                let Some(title) = destination
                    .title()
                    .and_then(|object| decode_text_string(resolve(pdf, object)))
                    .ok()
                    .map(|title| clean_text(&title.replace(['\u{00ad}', '\u{feff}'], "")))
                    .filter(|title| !title.is_empty())
                else {
                    continue;
                };
                bookmarks.push(TocType {
                    level,
                    title,
                    page: *page as usize,
                });
            }
            Outline::SubOutlines(children) => {
                collect_bookmarks(pdf, children, pages, level + 1, bookmarks);
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct TableCell {
    pub text: String,
    pub rowspan: usize,
    pub colspan: usize,
    pub column_header: bool,
    pub row_header: bool,
}

#[derive(Clone)]
enum TableSlot {
    Empty,
    Covered,
    Cell(TableCell),
}

/// Translate Docling's anchors to XHTML rows, including empty slots but
/// excluding slots already covered by a neighboring cell's span.
pub(super) fn table_rows(item: &Value) -> Option<Vec<Vec<TableCell>>> {
    let data = &item["data"];
    let rows = number(&data["num_rows"])?;
    let columns = number(&data["num_cols"])?;
    let area = rows.checked_mul(columns)?;
    if area == 0 || area > 100_000 {
        return None;
    }
    let cells = data["table_cells"].as_array()?;
    if cells.is_empty() {
        return None;
    }
    let mut grid = vec![TableSlot::Empty; area];
    for cell in cells {
        let row = number(&cell["start_row_offset_idx"])?;
        let column = number(&cell["start_col_offset_idx"])?;
        let rowspan = number(&cell["row_span"]).unwrap_or(1);
        let colspan = number(&cell["col_span"]).unwrap_or(1);
        let end_row = row.checked_add(rowspan)?;
        let end_column = column.checked_add(colspan)?;
        if rowspan == 0 || colspan == 0 || end_row > rows || end_column > columns {
            return None;
        }
        for y in row..end_row {
            for x in column..end_column {
                let slot = &mut grid[y * columns + x];
                if !matches!(slot, TableSlot::Empty) {
                    return None;
                }
                *slot = TableSlot::Covered;
            }
        }
        grid[row * columns + column] = TableSlot::Cell(TableCell {
            text: item_text(cell),
            rowspan,
            colspan,
            column_header: cell["column_header"] == true,
            row_header: cell["row_header"] == true || cell["row_section"] == true,
        });
    }
    Some(
        grid.chunks(columns)
            .map(|row| {
                row.iter()
                    .filter_map(|slot| match slot {
                        TableSlot::Empty => Some(TableCell {
                            rowspan: 1,
                            colspan: 1,
                            ..TableCell::default()
                        }),
                        TableSlot::Covered => None,
                        TableSlot::Cell(cell) => Some(cell.clone()),
                    })
                    .collect()
            })
            .collect(),
    )
}

fn number(value: &Value) -> Option<usize> {
    usize::try_from(value.as_u64()?).ok()
}

pub(super) struct PrintedContents {
    hidden: HashSet<usize>,
    marker: Option<usize>,
    entries: Vec<PrintedEntry>,
    outline: Vec<TocType>,
    linked_entries: Option<HashSet<usize>>,
}

#[derive(Default)]
struct PrintedSection {
    start: usize,
    end: usize,
    entries: Vec<PrintedEntry>,
}

struct PrintedEntry {
    title: String,
    page: u64,
}

impl PrintedContents {
    pub(super) fn position(&self, index: usize) -> bool {
        self.marker == Some(index)
    }

    pub(super) fn contains(&self, index: usize) -> bool {
        self.hidden.contains(&index)
    }

    pub(super) fn navigation(
        &self,
        blocks: &mut Vec<Block>,
        headings: &[(u64, usize)],
        page_starts: &HashMap<u64, usize>,
    ) -> (Vec<NavEntry>, Option<Vec<NavEntry>>) {
        if self.outline.is_empty() {
            return (self.printed_navigation(blocks, headings), None);
        }
        let entries = bookmark_navigation(&self.outline, blocks, headings, page_starts);
        let in_book = self.linked_entries.as_ref().map(|linked| {
            entries
                .iter()
                .filter(|(index, _)| linked.contains(index))
                .map(|(_, entry)| entry.clone())
                .collect()
        });
        (
            entries.into_iter().map(|(_, entry)| entry).collect(),
            in_book,
        )
    }

    fn printed_navigation(&self, blocks: &[Block], headings: &[(u64, usize)]) -> Vec<NavEntry> {
        let mut by_title = HashMap::<_, Vec<_>>::new();
        for &(page, index) in headings {
            if let Block::Heading { level, id, text } = &blocks[index] {
                by_title
                    .entry(title_key(text))
                    .or_default()
                    .push((page, level, id));
            }
        }
        let page_offset = printed_page_offset(&self.entries, &by_title);
        let mut used = HashSet::new();
        self.entries
            .iter()
            .filter_map(|entry| {
                let key = title_key(&entry.title);
                let candidates = by_title.get(&key)?;
                let printed_page = entry.page.checked_add_signed(page_offset)?;
                let matching_page = candidates
                    .iter()
                    .filter(|(page, _, _)| *page == printed_page);
                let (_, level, id) = unique(matching_page).or(match candidates.as_slice() {
                    [candidate] => Some(candidate),
                    _ => None,
                })?;
                used.insert(*id).then(|| NavEntry {
                    level: **level,
                    id: (*id).clone(),
                    text: entry.title.clone(),
                })
            })
            .collect()
    }
}

pub(super) fn printed_contents(items: &[&Value], pdf: &Document, input: &Path) -> PrintedContents {
    if let Some(contents) = contents_from_bookmarks(items, pdf, input) {
        return contents;
    }
    let heading_titles: HashSet<_> = items
        .iter()
        .filter(|item| heading(item))
        .map(|item| title_key(&item_text(item)))
        .collect();
    let mut sections = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let page = page_no(item);
        if page == 0 || page > 40 || !heading(item) || !contents_title(&item_text(item)) {
            continue;
        }
        let mut section = PrintedSection {
            start: index,
            end: index + 1,
            ..PrintedSection::default()
        };
        for (table_index, next) in items.iter().enumerate().skip(index + 1) {
            if matches!(label(next), "page_header" | "page_footer") {
                continue;
            }
            if label(next) != "table" || page_no(next) > page + 16 {
                break;
            }
            let Some(entries) = printed_entries(next) else {
                break;
            };
            section.end = table_index + 1;
            section.entries.extend(entries);
        }
        let matched = section
            .entries
            .iter()
            .filter(|entry| heading_titles.contains(&title_key(&entry.title)))
            .count();
        // An explicit contents title, repeated dot-leader rows and real
        // destination headings are all required before removing source data.
        if matched >= 2 && matched * 4 >= section.entries.len() * 3 {
            sections.push(section);
        }
    }
    let hidden = sections
        .iter()
        .flat_map(|section| section.start..section.end)
        .collect();
    let primary = sections
        .into_iter()
        .max_by_key(|section| section.entries.len());
    let marker = primary.as_ref().map(|section| section.start);
    let entries = primary.map_or_else(Vec::new, |section| section.entries);
    PrintedContents {
        hidden,
        marker,
        entries,
        outline: Vec::new(),
        linked_entries: None,
    }
}

fn contents_from_bookmarks(
    items: &[&Value],
    pdf: &Document,
    input: &Path,
) -> Option<PrintedContents> {
    let toc = pdf_outline(pdf);
    let bookmark = toc.iter().enumerate().find(|(_, entry)| {
        (1..=40).contains(&entry.page)
            && (contents_title(&entry.title) || title_key(&entry.title) == "toc")
    });
    let (first, end) = if let Some((index, bookmark)) = bookmark {
        let end = toc[index + 1..]
            .iter()
            .find(|entry| entry.level <= bookmark.level && entry.page > bookmark.page)?
            .page;
        (bookmark.page, end)
    } else {
        let end = toc.first()?.page;
        let first = items.iter().find_map(|item| {
            let page = page_no(item) as usize;
            (page > 0 && page < end && heading(item) && contents_title(&item_text(item)))
                .then_some(page)
        })?;
        (first, end)
    };
    if first > 40 || !(1..=16).contains(&(end - first)) {
        return None;
    }
    let pages = first as u64..end as u64;
    let first_title = items.iter().position(|item| {
        page_no(item) == pages.start && heading(item) && contents_title(&item_text(item))
    })?;
    let marker = items
        .iter()
        .position(|item| {
            pages.contains(&page_no(item))
                && heading(item)
                && matches!(
                    title_key(&item_text(item)).as_str(),
                    "contents" | "tableofcontents" | "оглавление" | "содержание"
                )
        })
        .unwrap_or(first_title);
    if let Some((hidden, linked_entries)) = linked_contents(items, pdf, pages.clone(), marker, &toc)
    {
        return Some(PrintedContents {
            hidden,
            marker: Some(marker),
            entries: Vec::new(),
            outline: toc,
            linked_entries: Some(linked_entries),
        });
    }
    let outline: Vec<_> = toc.into_iter().filter(|entry| entry.page >= end).collect();
    if outline.len() < 4
        || !items.iter().any(|item| {
            !matches!(label(item), "page_header" | "page_footer")
                && outline
                    .iter()
                    .any(|entry| entry.page as u64 == page_no(item))
        })
    {
        return None;
    }
    // PDF bookmarks bound the candidate section. Confirm the actual printed
    // pages independently, since extraction can label them as pictures,
    // tables, or a mixture of unrelated-looking blocks.
    let bytes = std::fs::read(input).ok()?;
    let mut parser = PageTextParser::open(&bytes)?;
    let mut contents_pages = 0;
    for page in first..end {
        let source = parser.text_page(page - 1);
        if source.cells.is_empty() {
            continue;
        }
        let leaders = source
            .cells
            .iter()
            .filter(|cell| {
                let compact: String = cell.text.chars().filter(|ch| !ch.is_whitespace()).collect();
                compact.contains("...") || compact.contains('…')
            })
            .count();
        if leaders < 4 {
            return None;
        }
        contents_pages += 1;
    }
    if contents_pages < 2 {
        return None;
    }
    Some(PrintedContents {
        hidden: items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| pages.contains(&page_no(item)).then_some(index))
            .collect(),
        marker: Some(marker),
        entries: Vec::new(),
        outline,
        linked_entries: None,
    })
}

fn linked_contents(
    items: &[&Value],
    pdf: &Document,
    pages: Range<u64>,
    marker: usize,
    outline: &[TocType],
) -> Option<(HashSet<usize>, HashSet<usize>)> {
    let pdf_pages = pdf.get_pages();
    let destinations: HashMap<_, _> = outline
        .iter()
        .filter_map(|entry| {
            let page = u32::try_from(entry.page).ok()?;
            (u64::from(page) >= pages.end).then_some(())?;
            Some((*pdf_pages.get(&page)?, entry.page))
        })
        .collect();
    let links = pages
        .clone()
        .map(|page| {
            let id = *pdf_pages.get(&u32::try_from(page).ok()?)?;
            Some(linked_pages(pdf, id, &destinations))
        })
        .collect::<Option<Vec<_>>>()?;
    let linked_pages: HashSet<_> = links.iter().flatten().copied().collect();
    let entries: Vec<_> = outline
        .iter()
        .enumerate()
        .filter(|(_, entry)| linked_pages.contains(&entry.page))
        .map(|(index, entry)| (index, entry.page, title_key(&entry.title)))
        .collect();
    if entries.len() < 4
        || !items.iter().any(|item| {
            !matches!(label(item), "page_header" | "page_footer")
                && usize::try_from(page_no(item)).is_ok_and(|page| linked_pages.contains(&page))
        })
    {
        return None;
    }

    // Replace only complete blocks accounted for by the linked bookmark titles.
    // Other material on the same page remains in the book.
    let mut hidden = HashSet::from([marker]);
    let mut remaining = entries.as_slice();
    for (page, mut targets) in pages.zip(links) {
        let (mut expected, rest) = remaining.split_at_checked(targets.len())?;
        let mut expected_pages: Vec<_> = expected.iter().map(|(_, page, _)| *page).collect();
        targets.sort_unstable();
        expected_pages.sort_unstable();
        if targets != expected_pages || expected.is_empty() {
            return None;
        }
        for (index, item) in items.iter().enumerate().filter(|(index, item)| {
            *index != marker
                && page_no(item) == page
                && matches!(
                    label(item),
                    "text" | "paragraph" | "list_item" | "section_header" | "title"
                )
        }) {
            let key = title_key(&item_text(item));
            if let Some(count) = matching_titles(&key, expected) {
                hidden.insert(index);
                expected = &expected[count..];
            }
        }
        if !expected.is_empty() {
            return None;
        }
        remaining = rest;
    }
    remaining.is_empty().then(|| {
        (
            hidden,
            entries.into_iter().map(|(index, _, _)| index).collect(),
        )
    })
}

fn matching_titles(mut text: &str, entries: &[(usize, usize, String)]) -> Option<usize> {
    for (index, (_, _, title)) in entries.iter().enumerate() {
        if title.is_empty() {
            return None;
        }
        text = text.strip_prefix(title)?;
        if text.is_empty() {
            return Some(index + 1);
        }
    }
    None
}

fn linked_pages(
    pdf: &Document,
    page: ObjectId,
    destinations: &HashMap<ObjectId, usize>,
) -> Vec<usize> {
    let mut seen = Vec::new();
    pdf.get_page_annotations(page)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|annotation| {
            let destination = link_destination(pdf, annotation)?;
            let page = destinations.get(&destination.first()?.as_reference().ok()?)?;
            if seen.contains(&destination) {
                return None;
            }
            seen.push(destination);
            Some(*page)
        })
        .collect()
}

fn link_destination<'a>(pdf: &'a Document, annotation: &'a Dictionary) -> Option<&'a [Object]> {
    if annotation.get(b"Subtype").ok()?.as_name().ok()? != b"Link" {
        return None;
    }
    let destination = if let Ok(destination) = annotation.get(b"Dest") {
        destination
    } else {
        let action = pdf.get_dict_in_dict(annotation, b"A").ok()?;
        (action.get(b"S").ok()?.as_name().ok()? == b"GoTo").then_some(())?;
        action.get(b"D").ok()?
    };
    Some(resolve(pdf, destination).as_array().ok()?)
}

fn bookmark_navigation(
    outline: &[TocType],
    blocks: &mut Vec<Block>,
    headings: &[(u64, usize)],
    page_starts: &HashMap<u64, usize>,
) -> Vec<(usize, NavEntry)> {
    let mut anchors = HashSet::new();
    let entries = outline
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let page = entry.page as u64;
            let block = *page_starts.get(&page)?;
            let key = title_key(&entry.title);
            let short_key = title_key(without_section_number(&entry.title));
            let headings_on_page = headings.iter().filter_map(|&(source_page, index)| {
                if source_page == page
                    && let Block::Heading { text, id, .. } = &blocks[index]
                {
                    Some((text.as_str(), id))
                } else {
                    None
                }
            });
            let exact = unique(
                headings_on_page
                    .clone()
                    .filter(|(text, _)| title_key(text) == key),
            );
            let matched =
                exact
                    .or_else(|| {
                        unique(headings_on_page.clone().filter(|(text, _)| {
                            title_key(without_section_number(text)) == short_key
                        }))
                    })
                    .or_else(|| {
                        // A PDF outline can use a different wording for a
                        // numbered section on the same page.
                        let (number, _) = split_section_number(&entry.title)?;
                        number.contains('.').then_some(())?;
                        unique(headings_on_page.filter(|(text, _)| {
                            split_section_number(text).is_some_and(|(value, _)| value == number)
                        }))
                    });
            let id = matched.map_or_else(
                || {
                    anchors.insert((block, page));
                    format!("source-page-{page}")
                },
                |(_, id)| id.clone(),
            );
            Some((
                index,
                NavEntry {
                    level: entry.level.clamp(1, 6) as u8,
                    id,
                    text: super::clean_text(&entry.title),
                },
            ))
        })
        .collect();
    let mut anchors: Vec<_> = anchors.into_iter().collect();
    anchors.sort_unstable_by(|a, b| b.cmp(a));
    for (index, page) in anchors {
        blocks.insert(index, Block::PageAnchor { page });
    }
    entries
}

fn without_section_number(title: &str) -> &str {
    split_section_number(title).map_or(title, |(_, rest)| rest)
}

fn split_section_number(title: &str) -> Option<(&str, &str)> {
    let (number, rest) = title.split_once(char::is_whitespace)?;
    let number = number.trim_end_matches('.');
    number
        .split('.')
        .all(|component| !component.is_empty() && component.bytes().all(|b| b.is_ascii_digit()))
        .then_some((number, rest.trim_start()))
}

fn contents_title(title: &str) -> bool {
    matches!(
        title_key(title).as_str(),
        "contents"
            | "tableofcontents"
            | "briefcontents"
            | "оглавление"
            | "краткоеоглавление"
            | "содержание"
            | "краткоесодержание"
    )
}

fn heading(item: &Value) -> bool {
    matches!(label(item), "title" | "section_header")
}

fn title_key(title: &str) -> String {
    title
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn printed_entries(table: &Value) -> Option<Vec<PrintedEntry>> {
    let data = &table["data"];
    let row_count = number(&data["num_rows"])?;
    if !(2..=1_000).contains(&row_count) {
        return None;
    }
    let mut rows = vec![Vec::new(); row_count];
    for cell in data["table_cells"].as_array()? {
        let row = number(&cell["start_row_offset_idx"])?;
        let column = number(&cell["start_col_offset_idx"])?;
        rows.get_mut(row)?.push((column, item_text(cell)));
    }
    let entries: Vec<_> = rows
        .iter_mut()
        .filter_map(|cells| {
            cells.sort_unstable_by_key(|(column, _)| *column);
            let row = cells
                .iter()
                .map(|(_, text)| text.as_str())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let digit_start = row.trim_end_matches(|c: char| c.is_ascii_digit()).len();
            let page = row[digit_start..].parse::<u64>().ok()?;
            let title = row[..digit_start].trim_end_matches([' ', '.', '…']);
            let separator = &row[title.len()..digit_start];
            let has_leaders =
                separator.chars().filter(|c| *c == '.').count() >= 3 || separator.contains('…');
            (has_leaders && page > 0 && title.chars().any(char::is_alphabetic)).then(|| {
                PrintedEntry {
                    title: title.to_owned(),
                    page,
                }
            })
        })
        .collect();
    (entries.len() >= 2 && entries.len() * 2 >= row_count).then_some(entries)
}

fn printed_page_offset(
    entries: &[PrintedEntry],
    headings: &HashMap<String, Vec<(u64, &u8, &String)>>,
) -> i64 {
    let mut offsets = HashMap::<_, usize>::new();
    for entry in entries {
        if let Some([(page, _, _)]) = headings.get(&title_key(&entry.title)).map(Vec::as_slice)
            && let (Ok(page), Ok(printed)) = (i64::try_from(*page), i64::try_from(entry.page))
        {
            *offsets.entry(page - printed).or_default() += 1;
        }
    }
    offsets
        .iter()
        .filter(|(_, count)| **count >= 2 && **count * 2 > entries.len())
        .map(|(&offset, _)| offset)
        .next()
        .unwrap_or(0)
}

fn unique<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

/// A picture can have the wrong caption attached even though the real
/// caption is in its child list. Require a unique nearby numbered caption.
pub(super) fn caption_refs<'a>(doc: &Value, item: &'a Value) -> &'a [Value] {
    let current = item["captions"].as_array().map_or(&[][..], Vec::as_slice);
    if label(item) != "picture"
        || current
            .iter()
            .any(|reference| get_ref(doc, reference).is_some_and(numbered_figure_caption))
    {
        return current;
    }
    let page = page_no(item);
    let Some(bounds) = Bounds::from_item(item).filter(|_| page > 0) else {
        return current;
    };
    let candidates = item["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|reference| {
            get_ref(doc, reference).is_some_and(|child| {
                label(child) == "caption"
                    && page_no(child) == page
                    && numbered_figure_caption(child)
                    && Bounds::from_item(child).is_some_and(|caption| bounds.near(&caption))
            })
        });
    unique(candidates).map_or(current, std::slice::from_ref)
}

fn numbered_figure_caption(item: &Value) -> bool {
    let text = item_text(item);
    let mut words = text.split_whitespace();
    let Some(label) = words.next() else {
        return false;
    };
    matches!(
        label.trim_end_matches('.').to_lowercase().as_str(),
        "figure" | "fig" | "рис"
    ) && words
        .next()
        .is_some_and(|word| word.starts_with(|c: char| c.is_ascii_digit()))
}

/// Identify chart labels already represented in a retained picture. Require
/// both a Docling parent link and a bounding box within the actual crop.
pub(super) fn figure_labels(items: &[&Value]) -> HashSet<usize> {
    let indices: HashMap<_, _> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| Some((item["self_ref"].as_str()?, index)))
        .collect();
    let mut covered = HashSet::new();
    for picture in items.iter().filter(|item| label(item) == "picture") {
        let Some(children) = picture["children"].as_array().filter(|v| !v.is_empty()) else {
            continue;
        };
        if super::embedded_image(&picture["image"]).is_none() {
            continue;
        }
        let Some(picture_bounds) = Bounds::from_item(picture) else {
            continue;
        };
        for child in children {
            let Some(&index) = child["$ref"]
                .as_str()
                .and_then(|reference| indices.get(reference))
            else {
                continue;
            };
            let item = items[index];
            if matches!(label(item), "text" | "paragraph")
                && page_no(item) == page_no(picture)
                && Bounds::from_item(item).is_some_and(|bounds| picture_bounds.contains(bounds))
            {
                covered.insert(index);
            }
        }
    }
    covered
}

struct Bounds {
    x: (f64, f64),
    y: (f64, f64),
    top_left: bool,
}

impl Bounds {
    fn from_item(item: &Value) -> Option<Self> {
        let bbox = &item["prov"][0]["bbox"];
        let top_left = match bbox["coord_origin"].as_str()? {
            "TOPLEFT" => true,
            "BOTTOMLEFT" => false,
            _ => return None,
        };
        let top = bbox["t"].as_f64()?;
        let bottom = bbox["b"].as_f64()?;
        Some(Self {
            x: (bbox["l"].as_f64()?, bbox["r"].as_f64()?),
            y: (top.min(bottom), top.max(bottom)),
            top_left,
        })
    }

    fn contains(&self, other: Self) -> bool {
        const MARGIN: f64 = 2.0;
        self.top_left == other.top_left
            && other.x.0 >= self.x.0 - MARGIN
            && other.x.1 <= self.x.1 + MARGIN
            && other.y.0 >= self.y.0 - MARGIN
            && other.y.1 <= self.y.1 + MARGIN
    }

    fn near(&self, other: &Self) -> bool {
        const MAX_GAP: f64 = 24.0;
        self.top_left == other.top_left
            && self.x.0 < other.x.1
            && other.x.0 < self.x.1
            && self.y.0 - other.y.1 <= MAX_GAP
            && other.y.0 - self.y.1 <= MAX_GAP
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn recognizes_linked_contents_and_preserves_neighboring_text() {
        use lopdf::{Bookmark, Object, dictionary};

        let mut pdf = Document::new();
        let page_tree = pdf.new_object_id();
        let pages: Vec<_> = (0..5)
            .map(|_| pdf.add_object(dictionary! { "Type" => "Page", "Parent" => page_tree }))
            .collect();
        pdf.set_object(
            page_tree,
            dictionary! {
                "Type" => "Pages", "Kids" => pages.iter().copied().map(Object::from).collect::<Vec<_>>(),
                "Count" => 5_i64, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            },
        );
        let chapters = [
            "First chapter",
            "Second chapter",
            "Third chapter",
            "Fourth chapter",
        ];
        for (page, title) in [(0, "Preface"), (1, "Contents"), (2, "Epigraph")]
            .into_iter()
            .chain(
                chapters
                    .into_iter()
                    .zip([3, 3, 4, 4])
                    .map(|(title, page)| (page, title)),
            )
        {
            pdf.add_bookmark(Bookmark::new(title.into(), [0.0; 3], 0, pages[page]), None);
        }
        let outlines = pdf.build_outline().unwrap();
        let catalog = pdf.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => page_tree, "Outlines" => outlines },
        );
        pdf.trailer.set("Root", catalog);
        let links = [3, 3, 4, 4]
            .into_iter()
            .zip([700_i64, 500, 700, 500])
            .map(|(page, top)| {
                let destination = vec![
                    pages[page].into(),
                    Object::Name(b"XYZ".to_vec()),
                    0.into(),
                    top.into(),
                    Object::Null,
                ];
                if page == 3 {
                    dictionary! { "Subtype" => "Link", "Dest" => destination }.into()
                } else {
                    let action = dictionary! { "S" => "GoTo", "D" => destination };
                    dictionary! { "Subtype" => "Link", "A" => action }.into()
                }
            })
            .collect::<Vec<Object>>();
        pdf.get_object_mut(pages[1])
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Annots", links);

        let item =
            |page, label, text| json!({"label": label, "text": text, "prov": [{"page_no": page} ]});
        let texts = [
            item(1, "text", "Preface prose"),
            item(2, "section_header", "Contents"),
            item(
                2,
                "text",
                "First chapter Second chapter Third chapter Fourth chapter",
            ),
            item(2, "text", "A notice below the list."),
            item(3, "text", "Epigraph prose"),
            item(4, "section_header", chapters[0]),
            item(4, "section_header", chapters[1]),
            item(5, "section_header", chapters[2]),
            item(5, "section_header", chapters[3]),
        ];
        let children: Vec<_> = (0..texts.len())
            .map(|i| json!({"$ref": format!("#/texts/{i}")}))
            .collect();
        let mut doc = json!({"body": {"children": children}, "texts": texts});
        let normalize = |doc: &Value| {
            super::super::normalize(doc, &pdf, Path::new("missing.pdf"), Default::default())
        };
        let book = normalize(&doc);
        let reader = &book.navigation;
        let printed = book.contents_navigation.unwrap();
        assert_eq!(reader.len(), 7);
        assert_eq!(reader[1].id, "source-page-2");
        assert!(book.blocks.windows(3).any(|blocks| matches!(
            blocks,
            [Block::PageAnchor { page: 2 }, Block::Contents { title }, Block::Paragraph { html }]
                if title == "Contents" && html == "A notice below the list."
        )));
        assert_eq!(
            printed
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            chapters
        );
        assert_ne!(printed[0].id, printed[1].id);

        doc["texts"][2]["text"] = json!(
            "First chapter Second chapter Third chapter Fourth chapter A notice below the list."
        );
        assert!(normalize(&doc).contents_navigation.is_none());
    }

    #[test]
    fn decodes_pdf_bookmark_titles_at_each_depth() {
        use lopdf::{Destination, Object, StringFormat};

        let page = (1, 0);
        let destination = |title| {
            Outline::Destination(Destination::new(
                title,
                Object::Reference(page),
                Object::Name(b"Fit".to_vec()),
            ))
        };
        let outlines = [
            destination(Object::string_literal(b"Cryptography")),
            Outline::SubOutlines(vec![destination(Object::String(
                b"Goldwasser\x85Micali".to_vec(),
                StringFormat::Literal,
            ))]),
        ];
        let mut bookmarks = Vec::new();
        collect_bookmarks(
            &Document::new(),
            &outlines,
            &HashMap::from([(page, 10)]),
            1,
            &mut bookmarks,
        );
        assert_eq!(bookmarks[1].title, "Goldwasser–Micali");
        assert_eq!((bookmarks[1].level, bookmarks[1].page), (2, 10));
    }

    #[test]
    fn bookmarks_link_to_same_page_sections_and_fall_back_to_source_page() {
        let mut blocks = vec![
            Block::Heading {
                level: 1,
                id: "first".to_owned(),
                text: "First chapter".to_owned(),
            },
            Block::Heading {
                level: 2,
                id: "details".to_owned(),
                text: "1.1 Details".to_owned(),
            },
            Block::Paragraph {
                html: "Unlabelled section.".to_owned(),
            },
        ];
        let bookmark = |page, level, title: &str| TocType {
            level,
            title: title.to_owned(),
            page,
        };
        let entries = bookmark_navigation(
            &[
                bookmark(10, 1, "1 First chapter"),
                bookmark(10, 2, "1.1 Details for the chapter"),
                bookmark(12, 2, "1.2 Unlabelled section"),
                bookmark(14, 2, "Outside the selected range"),
            ],
            &mut blocks,
            &[(10, 0), (10, 1)],
            &HashMap::from([(10, 0), (12, 2)]),
        );
        assert_eq!(
            entries
                .iter()
                .map(|(_, entry)| (entry.level, entry.id.as_str()))
                .collect::<Vec<_>>(),
            [(1, "first"), (2, "details"), (2, "source-page-12")]
        );
        assert!(matches!(blocks[2], Block::PageAnchor { page: 12 }));
    }

    #[test]
    fn selects_only_a_unique_numbered_caption_near_the_picture() {
        let region = json!([{"page_no": 5, "bbox": {
            "coord_origin": "BOTTOMLEFT", "l": 50, "r": 400, "b": 83, "t": 220
        }}]);
        let mut doc = json!({"texts": [
            {"label": "caption", "text": "В листинге 6.1 показано решение задачи"},
            {"label": "caption", "text": "Рис. 6.24. Кольцевой буфер", "prov": region},
            {"label": "caption", "text": "Рис. 6.25. Другая схема", "prov": region}
        ]});
        let wrong = json!({"$ref": "#/texts/0"});
        let correct = json!({"$ref": "#/texts/1"});
        let picture = json!({
            "label": "picture", "prov": region,
            "captions": [wrong],
            "children": [wrong, correct, {"$ref": "#/texts/2"}]
        });
        assert_eq!(caption_refs(&doc, &picture), std::slice::from_ref(&wrong));

        doc["texts"][2]["prov"][0]["page_no"] = json!(6);
        assert_eq!(caption_refs(&doc, &picture), std::slice::from_ref(&correct));
    }
}
