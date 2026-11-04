use std::collections::HashSet;

use serde_json::Value;

use super::{label, page_no};

const LAYOUT_MARK: char = '\u{008f}';
const SOFT_HYPHEN: char = '\u{00ad}';

/// Source-specific word evidence for distinguishing discretionary wraps from
/// compounds. Ambiguous hard hyphens stay in the book.
pub(super) struct Reflow {
    words: HashSet<String>,
}

impl Reflow {
    pub(super) fn new(doc: &Value) -> Self {
        let words = doc["texts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["text"].as_str())
            .flat_map(|text| text.split(|ch: char| !ch.is_alphabetic() && ch != '-'))
            .filter(|word| {
                word.chars().count() > 2 && !word.starts_with('-') && !word.ends_with('-')
            })
            .map(str::to_lowercase)
            .collect();
        Self { words }
    }

    pub(super) fn text(&self, item: &Value) -> String {
        let raw = item["text"].as_str().unwrap_or_default();
        if matches!(label(item), "code" | "formula") || !raw.contains([LAYOUT_MARK, SOFT_HYPHEN]) {
            return clean_text(raw);
        }

        clean_text(&self.repair_wraps(raw))
    }

    fn repair_wraps(&self, raw: &str) -> String {
        let mut text = String::with_capacity(raw.len());
        let mut remaining = raw;
        while let Some((offset, marker)) = remaining
            .char_indices()
            .find(|&(_, ch)| matches!(ch, LAYOUT_MARK | SOFT_HYPHEN))
        {
            let left = &remaining[..offset];
            let right = remaining[offset + marker.len_utf8()..]
                .trim_start_matches(|ch: char| ch.is_whitespace() || ch == LAYOUT_MARK);
            let had_space = left.ends_with(char::is_whitespace);
            text.push_str(left.trim_end());

            if let Some(first) = right.chars().next() {
                let has_hyphen = !had_space && text.ends_with('-') && first.is_alphabetic();
                if has_hyphen && self.discretionary_hyphen(&text, right) {
                    text.pop();
                }
                let joined_letters = !had_space
                    && first.is_lowercase()
                    && (marker == SOFT_HYPHEN || self.attested_word(&text, right));
                if !has_hyphen
                    && !joined_letters
                    && !matches!(first, '.' | ',' | ';' | ':' | '!' | '?')
                    && !text.is_empty()
                {
                    text.push(' ');
                }
            }
            remaining = right;
        }
        text.push_str(remaining);
        text
    }

    fn attested_word(&self, left: &str, right: &str) -> bool {
        let Some((stem, suffix)) = word_fragments(left, right) else {
            return false;
        };
        self.words
            .contains(&format!("{stem}{suffix}").to_lowercase())
    }

    fn discretionary_hyphen(&self, left: &str, right: &str) -> bool {
        let Some(left) = left.strip_suffix('-') else {
            return false;
        };
        let Some((stem, suffix)) = word_fragments(left, right) else {
            return false;
        };
        self.words
            .contains(&format!("{stem}{suffix}").to_lowercase())
            && !self
                .words
                .contains(&format!("{stem}-{suffix}").to_lowercase())
    }

    /// Append a paragraph fragment only when adjacent source pages have
    /// compatible text regions near the facing edges and the sentence is open.
    pub(super) fn join(
        &self,
        doc: &Value,
        previous: &Value,
        current: &Value,
        previous_html: &mut String,
        current_html: &str,
    ) -> bool {
        if !is_prose(previous) || !is_prose(current) {
            return false;
        }
        let previous_page = page_no(previous);
        let current_page = page_no(current);
        let previous_raw = previous["text"].as_str().unwrap_or_default().trim_end();
        let current_raw = current["text"].as_str().unwrap_or_default().trim_start();
        if previous_page == 0
            || previous_page.checked_add(1) != Some(current_page)
            || !previous_raw
                .ends_with(|ch: char| ch.is_alphanumeric() || matches!(ch, ',' | '-' | SOFT_HYPHEN))
            || !current_raw.starts_with(char::is_lowercase)
            || !facing_regions(doc, previous_page, current_page, previous, current)
        {
            return false;
        }

        if previous_raw.ends_with(SOFT_HYPHEN) {
            if previous_html.ends_with(SOFT_HYPHEN) {
                previous_html.pop();
            }
        } else if previous_html.ends_with('-')
            && self.discretionary_hyphen(previous_raw, current_raw)
        {
            previous_html.pop();
        } else {
            previous_html.push(' ');
        }
        previous_html.push_str(current_html);
        true
    }
}

fn is_prose(item: &Value) -> bool {
    matches!(label(item), "text" | "paragraph")
}

fn word_fragments<'a, 'b>(left: &'a str, right: &'b str) -> Option<(&'a str, &'b str)> {
    let stem = left.rsplit(|ch: char| !ch.is_alphabetic()).next()?;
    let suffix = right.split(|ch: char| !ch.is_alphabetic()).next()?;
    (stem.chars().count() >= 2
        && suffix.chars().count() >= 2
        && suffix.starts_with(char::is_lowercase))
    .then_some((stem, suffix))
}

struct Region {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

fn facing_regions(
    doc: &Value,
    previous_page: u64,
    current_page: u64,
    previous: &Value,
    current: &Value,
) -> bool {
    let (Some(previous), Some(current)) = (
        region(doc, previous_page, previous, true),
        region(doc, current_page, current, false),
    ) else {
        return false;
    };
    let overlap = previous.right.min(current.right) - previous.left.max(current.left);
    let narrower = (previous.right - previous.left).min(current.right - current.left);
    previous.bottom >= 0.0
        && previous.bottom <= 0.16
        && current.top >= 0.80
        && current.top <= 1.0
        && narrower >= 0.20
        && overlap >= 0.75 * narrower
}

fn region(doc: &Value, page: u64, item: &Value, use_last: bool) -> Option<Region> {
    let size = &doc["pages"][page.to_string()]["size"];
    let width = size["width"].as_f64().filter(|width| *width > 0.0)?;
    let height = size["height"].as_f64().filter(|height| *height > 0.0)?;
    let provenance = item["prov"].as_array()?;
    let source = if use_last {
        provenance.last()?
    } else {
        provenance.first()?
    };
    if source["page_no"].as_u64() != Some(page) {
        return None;
    }
    let bbox = &source["bbox"];
    let mut top = bbox["t"].as_f64()? / height;
    let mut bottom = bbox["b"].as_f64()? / height;
    match bbox["coord_origin"].as_str()? {
        "TOPLEFT" => {
            top = 1.0 - top;
            bottom = 1.0 - bottom;
        }
        "BOTTOMLEFT" => {}
        _ => return None,
    }
    Some(Region {
        left: bbox["l"].as_f64()? / width,
        right: bbox["r"].as_f64()? / width,
        top,
        bottom,
    })
}

/// Keep XML text and ordinary layout whitespace, while stripping PDF control
/// codes that some EPUB readers display as repeated vertical bars.
pub(super) fn clean_text(text: &str) -> String {
    let is_artifact = |ch: char| {
        (ch.is_control() && !matches!(ch, '\t' | '\n' | '\r'))
            || matches!(ch, '\u{fffe}' | '\u{ffff}')
    };
    if !text.contains(is_artifact) {
        return text.trim().to_owned();
    }
    let filtered: String = text.chars().filter(|&ch| !is_artifact(ch)).collect();
    filtered.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn removes_control_glyphs_and_keeps_only_attested_compound_hyphens() {
        let doc = json!({"texts":[{"text":"Закончена работа с кэш-памятью и кэшпамятью."}]});
        let reflow = Reflow::new(&doc);
        let input = json!({"label":"list_item","text":"закон-\u{8f} \u{8f} чена работа с кэш-\u{8f} \u{8f} памятью, CD-\u{8f} \u{8f} ROM и други\u{ad} ми; \u{8f} \u{8f}"});
        assert_eq!(
            reflow.text(&input),
            "закончена работа с кэш-памятью, CD-ROM и другими;"
        );
    }

    #[test]
    fn joins_open_paragraphs_across_adjacent_page_edges() {
        let doc = json!({"pages":{
            "58":{"size":{"height":666.14,"width":467.72}},
            "59":{"size":{"height":666.14,"width":467.72}}
        }});
        let previous = json!({"label":"text","text":"Данные доступны в любой момент и на","prov":[
            {"page_no":58,"bbox":{"l":51.16,"r":416.56,"t":137.91,"b":55.95,"coord_origin":"BOTTOMLEFT"}}
        ]});
        let mut next = json!({"label":"text","text":"любом устройстве.","prov":[
            {"page_no":59,"bbox":{"l":51.16,"r":416.56,"t":595.88,"b":549.05,"coord_origin":"BOTTOMLEFT"}}
        ]});
        let reflow = Reflow::new(&doc);
        let mut html = reflow.text(&previous);
        assert!(reflow.join(&doc, &previous, &next, &mut html, "любом устройстве."));
        assert_eq!(
            html,
            "Данные доступны в любой момент и на любом устройстве."
        );

        next["prov"][0]["bbox"]["t"] = json!(360.0);
        assert!(!reflow.join(&doc, &previous, &next, &mut html, "любом устройстве."));
    }
}
