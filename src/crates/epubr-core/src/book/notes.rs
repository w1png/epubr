use std::collections::HashMap;
use std::ops::Range;

use serde_json::Value;

use super::{escape_xml, label, page_no};

pub(super) struct LinkedNotes {
    pub references: HashMap<usize, String>,
    pub backlinks: HashMap<usize, String>,
}

pub(super) fn link_notes(
    items: &[&Value],
    texts: &[String],
    warnings: &mut Vec<String>,
) -> LinkedNotes {
    let mut references = HashMap::new();
    let mut backlinks = HashMap::new();
    let mut edits: HashMap<usize, Vec<(Range<usize>, usize)>> = HashMap::new();
    let mut notes = HashMap::<(u64, String), Vec<usize>>::new();
    for (index, item) in items.iter().enumerate() {
        if label(item) != "footnote" {
            continue;
        }
        let page = page_no(item);
        if let Some(marker) = note_marker(&texts[index]) {
            notes.entry((page, marker)).or_default().push(index);
        } else {
            warnings.push(format!("Page {page}: note has no identifiable marker"));
        }
    }

    for ((page, marker), indices) in notes {
        let mut candidates = Vec::new();
        if let [note_index] = indices.as_slice() {
            for (index, item) in items.iter().enumerate() {
                if page_no(item) == page
                    && matches!(label(item), "text" | "paragraph" | "list_item")
                {
                    candidates.extend(
                        reference_markers(&texts[index], &marker)
                            .into_iter()
                            .map(|range| (index, range)),
                    );
                }
            }
            if let [(index, range)] = candidates.as_slice() {
                edits
                    .entry(*index)
                    .or_default()
                    .push((range.clone(), *note_index));
                continue;
            }
        }
        warnings.push(format!(
            "Page {page}: could not unambiguously link note {marker}"
        ));
    }

    for (index, mut edits) in edits {
        edits.sort_unstable_by_key(|(range, _)| range.start);
        let text = &texts[index];
        let mut html = String::new();
        let mut offset = 0;
        for (range, note_index) in edits {
            if range.start < offset {
                continue;
            }
            let id = format!("noteref-{note_index}");
            html.push_str(&escape_xml(&text[offset..range.start]));
            html.push_str(&format!(
                "<a id=\"{id}\" href=\"#note-{note_index}\" epub:type=\"noteref\">{}</a>",
                escape_xml(&text[range.clone()])
            ));
            offset = range.end;
            backlinks.insert(note_index, id);
        }
        html.push_str(&escape_xml(&text[offset..]));
        references.insert(index, html);
    }
    warnings.sort();
    LinkedNotes {
        references,
        backlinks,
    }
}

fn note_marker(text: &str) -> Option<String> {
    let marker: String = text
        .trim_start_matches(['[', '('])
        .chars()
        .take_while(|character| character.is_ascii_digit() || superscript(*character).is_some())
        .map(|character| superscript(character).unwrap_or(character))
        .collect();
    (!marker.is_empty() && marker.len() <= 3).then_some(marker)
}

fn reference_markers(text: &str, marker: &str) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let superscripts: String = marker
        .bytes()
        .map(|digit| "⁰¹²³⁴⁵⁶⁷⁸⁹".chars().nth((digit - b'0') as usize).unwrap())
        .collect();
    let bracketed = format!("[{marker}]");
    for matched in [&bracketed, &superscripts] {
        for (start, _) in text.match_indices(matched) {
            let end = start + matched.len();
            if !text[..start].ends_with(|c| superscript(c).is_some())
                && !text[end..].starts_with(|c| superscript(c).is_some())
            {
                result.push(start..end);
            }
        }
    }
    for (start, _) in text.match_indices(marker) {
        let end = start + marker.len();
        let left = &text[..start];
        let right = &text[end..];
        let preceding = left.chars().next_back();
        if left.trim().is_empty() || right.starts_with(|c: char| c.is_alphanumeric()) {
            continue;
        }
        // A section or figure number such as 2.2 is not a pair of markers.
        if left
            .strip_suffix(['.', ','])
            .is_some_and(|prefix| prefix.ends_with(|c: char| c.is_ascii_digit()))
            || right
                .strip_prefix(['.', ','])
                .is_some_and(|suffix| suffix.starts_with(|c: char| c.is_ascii_digit()))
        {
            continue;
        }
        // Docling commonly flattens a superscript before a punctuation mark
        // into a separated number, or attaches it to sentence punctuation.
        if (preceding.is_some_and(char::is_whitespace)
            && right.trim_start().starts_with(['.', ',', ';', ':', ')']))
            || (left.ends_with(['.', ',', ';', '!', '?']) && right.starts_with(char::is_whitespace))
        {
            result.push(start..end);
        }
    }
    result
}

fn superscript(character: char) -> Option<char> {
    "⁰¹²³⁴⁵⁶⁷⁸⁹"
        .chars()
        .position(|digit| digit == character)
        .and_then(|digit| char::from_digit(digit as u32, 10))
}
