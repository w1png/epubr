use std::ops::Range;
use std::path::Path;

use docling_pdf::{PdfPage, TextCell, textparse::PageTextParser};
use serde_json::Value;

use super::{escape_xml, math::comparable};

pub(super) struct Superscripts<'a> {
    input: &'a Path,
    parser: Option<Option<PageTextParser>>,
    page: Option<(usize, PdfPage)>,
}

impl<'a> Superscripts<'a> {
    pub(super) fn new(input: &'a Path) -> Self {
        Self {
            input,
            parser: None,
            page: None,
        }
    }

    pub(super) fn html(&mut self, item: &Value, text: &str) -> String {
        self.rewrite(item, text, escape_xml, |exponent| {
            format!("<sup>{}</sup>", escape_xml(exponent))
        })
    }

    pub(super) fn expression(&mut self, item: &Value, text: &str) -> String {
        self.rewrite(item, text, str::to_owned, |exponent| {
            format!("^{{{exponent}}}")
        })
    }

    fn rewrite(
        &mut self,
        item: &Value,
        text: &str,
        plain: impl Fn(&str) -> String,
        script: impl Fn(&str) -> String,
    ) -> String {
        let mut edits = Vec::new();
        let characters: Vec<_> = text
            .char_indices()
            .filter(|(_, ch)| !ch.is_whitespace())
            .map(|(offset, ch)| (offset, comparable(ch)))
            .collect();
        if let Some(regions) = item["prov"].as_array() {
            for region in regions {
                if let Some(page) = self.page(region)
                    && let Some(found) = edits_on_page(page, region, &characters, text)
                {
                    edits.extend(found);
                }
            }
        }
        edits.sort_unstable_by_key(|range| range.start);
        let mut output = String::new();
        let mut offset = 0;
        for range in edits {
            if range.start >= offset {
                output.push_str(&plain(&text[offset..range.start]));
                output.push_str(&script(text[range.clone()].trim_start()));
                offset = range.end;
            }
        }
        output.push_str(&plain(&text[offset..]));
        output
    }

    fn page(&mut self, region: &Value) -> Option<&PdfPage> {
        let index = usize::try_from(region["page_no"].as_u64()?)
            .ok()?
            .checked_sub(1)?;
        let parser = self
            .parser
            .get_or_insert_with(|| PageTextParser::open(&std::fs::read(self.input).ok()?))
            .as_mut()?;
        if self
            .page
            .as_ref()
            .is_none_or(|(previous, _)| *previous != index)
        {
            self.page = Some((index, parser.text_page(index)));
        }
        self.page.as_ref().map(|(_, page)| page)
    }
}

fn edits_on_page(
    page: &PdfPage,
    region: &Value,
    characters: &[(usize, char)],
    text: &str,
) -> Option<Vec<Range<usize>>> {
    let bounds = &region["bbox"];
    let left = bounds["l"].as_f64()? as f32;
    let right = bounds["r"].as_f64()? as f32;
    let (top, bottom) = (bounds["t"].as_f64()? as f32, bounds["b"].as_f64()? as f32);
    let (top, bottom) = match bounds["coord_origin"].as_str()? {
        "TOPLEFT" => (top, bottom),
        "BOTTOMLEFT" => (page.height - top, page.height - bottom),
        _ => return None,
    };
    let inside = |cell: &TextCell| {
        (left - 1.0..=right + 1.0).contains(&((cell.l + cell.r) / 2.0))
            && (top - 1.0..=bottom + 1.0).contains(&((cell.t + cell.b) / 2.0))
    };
    let words = &page.word_cells;
    let mut edits = Vec::new();
    for (index, pair) in words.windows(2).enumerate() {
        let [base, exponent] = pair else { continue };
        if !inside(base) || !inside(exponent) || !raised(base, exponent) {
            continue;
        }
        let on_baseline =
            |cell: &&TextCell| inside(cell) && (cell.b - base.b).abs() <= (base.b - base.t) * 0.20;
        let before = words[..index]
            .iter()
            .rev()
            .take_while(on_baseline)
            .take(3)
            .count();
        let after = words[index + 2..]
            .iter()
            .take_while(on_baseline)
            .take(3)
            .count();
        let compact = |cells: &[TextCell]| {
            cells
                .iter()
                .flat_map(|cell| cell.text.chars())
                .filter(|ch| !ch.is_whitespace())
                .map(comparable)
                .collect::<Vec<_>>()
        };
        let left = compact(&words[index - before..=index]);
        let base_len = base.text.chars().filter(|ch| !ch.is_whitespace()).count();
        let exponent = compact(&words[index + 1..index + 2]);
        let right = compact(&words[index + 2..index + 2 + after]);
        for context in [12, 6, 3] {
            let prefix = left.len().min(base_len + context);
            let suffix = right.len().min(context);
            let pattern = left[left.len() - prefix..]
                .iter()
                .chain(&exponent)
                .chain(&right[..suffix]);
            let length = prefix + exponent.len() + suffix;
            if length == 0 || (length == base_len + exponent.len() && length != characters.len()) {
                continue;
            }
            let mut matches = characters
                .windows(length)
                .filter(|window| window.iter().map(|(_, ch)| ch).eq(pattern.clone()));
            if let Some(window) = matches.next()
                && matches.next().is_none()
            {
                let start = window[prefix - 1].0;
                let end = window[prefix + exponent.len() - 1].0;
                let after_char =
                    |offset| offset + text[offset..].chars().next().unwrap().len_utf8();
                edits.push(after_char(start)..after_char(end));
                break;
            }
        }
    }
    Some(edits)
}

fn raised(base: &TextCell, exponent: &TextCell) -> bool {
    let base_text = base.text.trim();
    let script_text = exponent.text.trim();
    let unsigned = script_text
        .strip_prefix(['-', '−', '+'])
        .unwrap_or(script_text);
    let height = base.b - base.t;
    let shift = base.b - exponent.b;
    let gap = exponent.l - base.r;
    let base_token = base_text.bytes().all(|byte| byte.is_ascii_digit())
        || base_text.chars().count() == 1 && base_text.starts_with(char::is_alphabetic)
        || base_text.ends_with([')', ']']);
    let script_token = !unsigned.is_empty()
        && (unsigned.bytes().all(|byte| byte.is_ascii_digit())
            || unsigned.chars().count() == 1 && unsigned.starts_with(char::is_alphabetic));
    !base_text.is_empty()
        && base_token
        && script_token
        && (0.25 * height..=0.68 * height).contains(&shift)
        && (0.40 * height..=0.83 * height).contains(&(exponent.b - exponent.t))
        && (-0.12 * height..=0.16 * height).contains(&gap)
}
