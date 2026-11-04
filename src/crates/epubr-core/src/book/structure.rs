use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{Block, NavEntry, item_text, label, page_no};

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

    pub(super) fn navigation(&self, blocks: &[Block], headings: &[(u64, usize)]) -> Vec<NavEntry> {
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

pub(super) fn printed_contents(items: &[&Value]) -> PrintedContents {
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
    }
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
}
