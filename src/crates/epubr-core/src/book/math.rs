use std::collections::{BTreeMap, HashMap};
use std::iter::Peekable;
use std::ops::Range;
use std::path::Path;
use std::str::Chars;

use docling_pdf::{pdf_meta::PdfMeta, render::Renderer, textparse::PageTextParser};
use image::{ColorType, ImageEncoder, RgbImage, codecs::png::PngEncoder, imageops};
use lopdf::{Document, ObjectId, content::Content};
use serde_json::{Value, json};

use super::{embedded_image, escape_xml, ordered_items, page_no};

pub(super) struct PageGeometry<'a> {
    pdf: &'a Document,
    input: &'a Path,
    pages: BTreeMap<u32, ObjectId>,
    lines: HashMap<u32, Vec<HorizontalRule>>,
    bytes: Option<Vec<u8>>,
    glyphs: Option<(u32, Vec<SourceGlyph>)>,
}

#[derive(Clone, Copy)]
struct HorizontalRule {
    left: f64,
    right: f64,
    y: f64,
}

#[derive(Clone, Copy)]
struct SourceGlyph {
    text: char,
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
}

struct Bounds {
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
}

pub(super) fn placeholder(text: &str) -> bool {
    text.trim() == "<!-- formula-not-decoded -->"
}

/// Docling omits provenance when its decoder is disabled or fails. Recover
/// the source ink between neighboring regions around a single missing equation.
pub(crate) fn locate_placeholders(doc: &mut Value, input: &Path) {
    let items = ordered_items(doc);
    let is_placeholder = |item: &Value| item["text"].as_str().is_some_and(placeholder);
    let mut parser = None;
    let mut cached_page = None;
    let mut updates = Vec::new();
    for (index, item) in items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_placeholder(item))
    {
        let Some(pointer) = item["self_ref"]
            .as_str()
            .and_then(|path| path.strip_prefix('#'))
        else {
            continue;
        };
        let located = |candidate: &&Value| page_no(candidate) > 0 || is_placeholder(candidate);
        let before = items[..index].iter().rev().copied().find(located);
        let after = items[index + 1..].iter().copied().find(located);
        let located_region = (|| {
            let (before, after) = (before?, after?);
            let page = page_no(before);
            let next_page = page_no(after);
            if page == 0 || next_page == 0 {
                return None;
            }
            let (above, below) = (Bounds::from_item(before)?, Bounds::from_item(after)?);
            // A display equation can be the last item before the page turns.
            // Only search below its lead-in, on that same source page.
            let at_page_end = page.checked_add(1) == Some(next_page);
            if page != next_page && !at_page_end {
                return None;
            }
            let mut bounds = Bounds {
                left: above.left.min(below.left),
                right: above.right.max(below.right),
                top: above.bottom,
                bottom: if at_page_end {
                    (above.bottom - 72.0).max(0.0)
                } else {
                    below.top
                },
            };
            if !(3.0..=96.0).contains(&(bounds.top - bounds.bottom)) {
                return None;
            }
            let page_index = usize::try_from(page).ok()?.checked_sub(1)?;
            let mut source = String::new();
            if let Some(parser) = parser
                .get_or_insert_with(|| PageTextParser::open(&std::fs::read(input).ok()?))
                .as_mut()
            {
                if cached_page
                    .as_ref()
                    .is_none_or(|(cached, _)| *cached != page_index)
                {
                    cached_page = Some((page_index, parser.text_page(page_index)));
                }
                if let Some((_, parsed)) = &cached_page {
                    let height = f64::from(parsed.height);
                    let inside = |cell: &&docling_pdf::TextCell| {
                        let x = f64::from((cell.l + cell.r) / 2.0);
                        let y = height - f64::from((cell.t + cell.b) / 2.0);
                        (bounds.left..=bounds.right).contains(&x)
                            && (bounds.bottom..=bounds.top).contains(&y)
                            && !cell.text.trim().is_empty()
                    };
                    let to_bounds = |cell: &docling_pdf::TextCell| Bounds {
                        left: f64::from(cell.l),
                        right: f64::from(cell.r),
                        top: height - f64::from(cell.t),
                        bottom: height - f64::from(cell.b),
                    };
                    // Docling can attach the next page's opening prose to the
                    // preceding page. Exclude lines already present in that
                    // prose, but retain every row and script in the display.
                    let previous_text = items[..index]
                        .iter()
                        .rev()
                        .find(|item| {
                            matches!(super::label(item), "text" | "paragraph")
                                && !is_placeholder(item)
                        })
                        .and_then(|item| item["text"].as_str())
                        .map(without_whitespace)
                        .unwrap_or_default();
                    let previous_lines: Vec<_> = parsed
                        .cells
                        .iter()
                        .filter(|cell| {
                            let line = without_whitespace(&cell.text);
                            line.len() >= 12 && previous_text.contains(&line)
                        })
                        .collect();
                    // Prose line assembly can drop disconnected script runs
                    // on the left side of a display. Use smaller source runs
                    // for the crop, excluding lines already emitted as prose.
                    if let Some(ink) = parsed
                        .code_cells
                        .iter()
                        .filter(inside)
                        .filter(|cell| {
                            !previous_lines.iter().any(|line| {
                                cell.l >= line.l - 0.5
                                    && cell.r <= line.r + 0.5
                                    && cell.t >= line.t - 0.5
                                    && cell.b <= line.b + 0.5
                            })
                        })
                        .inspect(|cell| source.push_str(&cell.text))
                        .map(to_bounds)
                        .reduce(|a, b| Bounds {
                            left: a.left.min(b.left),
                            right: a.right.max(b.right),
                            top: a.top.max(b.top),
                            bottom: a.bottom.min(b.bottom),
                        })
                    {
                        // Tall delimiters can be painted separately from the
                        // recognized runs. Allow room on both sides.
                        bounds.left = bounds.left.max(ink.left - 5.0);
                        bounds.right = bounds.right.min(ink.right + 5.0);
                        bounds.top = bounds.top.min(ink.top + 2.0);
                        bounds.bottom = bounds.bottom.max(ink.bottom - 2.0);
                    }
                }
            }
            if at_page_end && source.is_empty() {
                return None;
            }
            Some((
                json!([{
                    "page_no": page,
                    "bbox": {
                        "coord_origin": "BOTTOMLEFT",
                        "l": bounds.left, "r": bounds.right,
                        "t": bounds.top, "b": bounds.bottom
                    }
                }]),
                source,
            ))
        })();
        updates.push((pointer.to_owned(), located_region));
    }
    for (pointer, located_region) in updates {
        if let Some(item) = doc.pointer_mut(&pointer) {
            item["label"] = json!("formula");
            item["text"] = json!("");
            item["orig"] = json!("");
            item["epubr_formula_undecoded"] = json!(true);
            if let Some((provenance, source)) = located_region {
                item["prov"] = provenance;
                item["epubr_formula_source_ink"] = json!(source);
            }
        }
    }
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// Compare decoded tokens with source ink where a text layer is available.
/// This catches a partial or hallucinated decode before publishing MathML.
pub(super) fn consistent_with_source(mathml: &str, source: &str) -> bool {
    let tokens: String = mathml
        .split('<')
        .filter_map(|tag| tag.split_once('>').map(|(_, token)| token))
        .collect();
    let tokens = tokens
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let signature = |text: &str| -> BTreeMap<char, usize> {
        let mut counts = BTreeMap::new();
        for ch in text.chars() {
            let ch = match ch {
                '`' => 'ℓ',
                '⋯' | '…' => '·',
                other => comparable(other),
            };
            if ch.is_alphanumeric() || "+-=≤≥≈≠∧∨¬∑".contains(ch) {
                *counts.entry(ch).or_default() += 1;
            }
        }
        counts
    };
    let expected = signature(source);
    if expected.is_empty() {
        return true;
    }
    let actual = signature(&tokens);
    let missing: usize = expected
        .iter()
        .map(|(ch, count)| count.saturating_sub(actual.get(ch).copied().unwrap_or_default()))
        .sum();
    let unexpected: usize = actual
        .iter()
        .map(|(ch, count)| count.saturating_sub(expected.get(ch).copied().unwrap_or_default()))
        .sum();
    let total: usize = expected.values().sum();
    missing + unexpected <= (total / 20).min(2)
}

impl Bounds {
    fn from_item(item: &Value) -> Option<Self> {
        let bounds = &item["prov"][0]["bbox"];
        if bounds["coord_origin"] != "BOTTOMLEFT" {
            return None;
        }
        Some(Self {
            left: bounds["l"].as_f64()?,
            right: bounds["r"].as_f64()?,
            bottom: bounds["b"].as_f64()?,
            top: bounds["t"].as_f64()?,
        })
    }
}

impl<'a> PageGeometry<'a> {
    pub fn new(pdf: &'a Document, input: &'a Path) -> Self {
        Self {
            pdf,
            input,
            pages: pdf.get_pages(),
            lines: HashMap::new(),
            bytes: None,
            glyphs: None,
        }
    }

    pub fn recover_inline(&mut self, item: &Value, text: &str) -> Option<String> {
        if !text.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return None;
        }
        let (glyphs, bars) = self.bar_matches(item, false)?;
        let book_chars: Vec<_> = text
            .char_indices()
            .filter(|(_, c)| !c.is_whitespace())
            .map(|(offset, c)| (offset, comparable(c)))
            .collect();
        let mut edits = Vec::new();
        for bar in bars {
            let anchor = glyphs[bar.start];
            let line: Vec<_> = glyphs
                .iter()
                .enumerate()
                .filter(|(_, glyph)| {
                    (glyph.bottom - anchor.bottom).abs() <= 0.65 && !glyph.text.is_whitespace()
                })
                .collect();
            let Some(start) = line.iter().position(|(index, _)| *index == bar.start) else {
                continue;
            };
            let Some(end) = line.iter().rposition(|(index, _)| bar.contains(index)) else {
                continue;
            };
            for context in [22, 12, 6] {
                let left = start.saturating_sub(context);
                let right = (end + 1 + context).min(line.len());
                let pattern: Vec<_> = line[left..right]
                    .iter()
                    .map(|(_, glyph)| comparable(glyph.text))
                    .collect();
                if pattern.len() < 6 {
                    continue;
                }
                let mut matches = book_chars
                    .windows(pattern.len())
                    .filter(|candidate| candidate.iter().map(|(_, c)| c).eq(pattern.iter()));
                if let Some(found) = matches.next()
                    && matches.next().is_none()
                {
                    edits.extend(
                        found[start - left..=end - left]
                            .iter()
                            .map(|(offset, _)| *offset),
                    );
                    break;
                }
            }
        }
        if edits.is_empty() {
            return None;
        }
        edits.sort_unstable();
        let mut output = String::with_capacity(text.len() + 2 * edits.len());
        for (offset, c) in text.char_indices() {
            output.push(c);
            if edits.binary_search(&offset).is_ok() {
                output.push('\u{0305}');
            }
        }
        Some(output)
    }

    pub fn recover_formula(&mut self, item: &Value) -> Option<String> {
        let (glyphs, bars) = self.bar_matches(item, true)?;
        let baseline = glyphs.first()?.bottom;
        if glyphs
            .iter()
            .any(|glyph| (glyph.bottom - baseline).abs() > 0.65)
        {
            return None;
        }
        let raw: String = glyphs.iter().map(|glyph| glyph.text).collect();
        if !standalone_equation(&raw) {
            return None;
        }
        let mut marked = String::new();
        for (index, glyph) in glyphs.iter().enumerate() {
            if bars.iter().any(|bar| bar.start == index) {
                marked.push_str("\\overline{");
            }
            marked.push(glyph.text);
            if bars.iter().any(|bar| bar.end == index + 1) {
                marked.push('}');
            }
        }
        Some(marked)
    }

    fn bar_matches(
        &mut self,
        item: &Value,
        require_all: bool,
    ) -> Option<(Vec<SourceGlyph>, Vec<Range<usize>>)> {
        let bounds = Bounds::from_item(item)?;
        let page = u32::try_from(page_no(item)).ok()?;
        let id = *self.pages.get(&page)?;
        let rules = self
            .lines
            .entry(page)
            .or_insert_with(|| horizontal_rules(self.pdf, id));
        let relevant: Vec<_> = rules
            .iter()
            .filter(|line| {
                (2.0..=55.0).contains(&(line.right - line.left))
                    && (bounds.bottom..=bounds.top + 3.0).contains(&line.y)
                    && line.left >= bounds.left - 1.0
                    && line.right <= bounds.right + 3.0
            })
            .collect();
        if relevant.is_empty() {
            return None;
        }
        if self
            .glyphs
            .as_ref()
            .is_none_or(|(cached, _)| *cached != page)
        {
            if self.bytes.is_none() {
                self.bytes = Some(std::fs::read(self.input).ok()?);
            }
            let glyphs =
                docling_pdf::textparse::debug_glyphs(self.bytes.as_ref()?, page as usize - 1)
                    .into_iter()
                    .map(|(text, left, right, bottom, top)| SourceGlyph {
                        text,
                        left: f64::from(left),
                        right: f64::from(right),
                        bottom: f64::from(bottom),
                        top: f64::from(top),
                    })
                    .collect();
            self.glyphs = Some((page, glyphs));
        }
        let mut glyphs: Vec<_> = self
            .glyphs
            .as_ref()?
            .1
            .iter()
            .copied()
            .filter(|glyph| {
                (bounds.left - 1.0..=bounds.right + 1.0)
                    .contains(&((glyph.left + glyph.right) / 2.0))
                    && (bounds.bottom - 1.0..=bounds.top + 1.0)
                        .contains(&((glyph.bottom + glyph.top) / 2.0))
            })
            .collect();
        // Quantize baselines only for ordering. Exact values are used to
        // associate a bar with glyphs on the same physical line.
        glyphs.sort_by(|a, b| {
            (b.bottom * 2.0)
                .round()
                .total_cmp(&(a.bottom * 2.0).round())
                .then_with(|| a.left.total_cmp(&b.left))
        });
        let rule_count = relevant.len();
        let mut bars = Vec::new();
        for line in relevant {
            let mut covered = glyphs.iter().enumerate().filter(|(_, glyph)| {
                let height = glyph.top - glyph.bottom;
                (line.left..=line.right).contains(&((glyph.left + glyph.right) / 2.0))
                    && (-0.20 * height..=0.27 * height).contains(&(line.y - glyph.top))
                    && glyph.text.is_ascii_uppercase()
            });
            let Some((start, first)) = covered.next() else {
                continue;
            };
            let (last_index, last) = covered.next_back().unwrap_or((start, first));
            if (first.bottom - last.bottom).abs() <= 0.65
                && (line.left - first.left).abs() <= 1.1
                && (line.right - last.right).abs() <= 2.6
                && glyphs[start..=last_index]
                    .iter()
                    .all(|glyph| glyph.text.is_ascii_uppercase())
                && bars
                    .iter()
                    .all(|bar: &Range<usize>| bar.end <= start || bar.start > last_index)
            {
                bars.push(start..last_index + 1);
            }
        }
        (!bars.is_empty() && (!require_all || bars.len() == rule_count)).then_some((glyphs, bars))
    }

    pub fn recover_overbar(&mut self, item: &Value, text: &str) -> Option<String> {
        // Detect a narrow drawn rule over the first single-letter operand of
        // a standalone equation. Avoid interpreting underlines, table rules,
        // and general shapes as mathematical operators.
        let mut tokens = text.split_whitespace();
        let first = tokens.next()?;
        if first.chars().count() != 1
            || !first.starts_with(char::is_alphabetic)
            || !text.contains('=')
            || !tokens.all(|token| {
                token.chars().count() == 1 && is_math_character(token.chars().next().unwrap())
            })
        {
            return None;
        }
        let bounds = Bounds::from_item(item)?;
        let height = bounds.top - bounds.bottom;
        if height <= 0.0 || bounds.right - bounds.left < 2.0 * height {
            return None;
        }
        let page = u32::try_from(page_no(item)).ok()?;
        let id = self.pages.get(&page)?;
        let rules = self
            .lines
            .entry(page)
            .or_insert_with(|| horizontal_rules(self.pdf, *id));
        let mut matches = rules.iter().filter(|line| {
            (line.left - bounds.left).abs() <= height * 0.12
                && (height * 0.30..=height * 0.85).contains(&(line.right - line.left))
                && (height * 0.02..=height * 0.30).contains(&(line.y - bounds.top))
        });
        (matches.next().is_some() && matches.next().is_none())
            .then(|| format!("\\overline{{{first}}}{}", &text[first.len()..]))
    }
}

pub(super) fn comparable(c: char) -> char {
    match c {
        '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
        _ => super::symbols::comparison_character(c),
    }
}

fn horizontal_rules(pdf: &Document, id: ObjectId) -> Vec<HorizontalRule> {
    let Ok(bytes) = pdf.get_page_content_with_limit(id, 64 * 1024 * 1024) else {
        return Vec::new();
    };
    let Ok(content) = Content::decode(&bytes) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let mut start = None;
    let mut segment = None;
    let mut transform = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mut stack = Vec::new();
    for operation in content.operations {
        let operands = &operation.operands;
        let operand = |index: usize| operands.get(index)?.as_float().ok().map(f64::from);
        let point = || {
            let (x, y) = (operand(0)?, operand(1)?);
            let [a, b, c, d, e, f] = transform;
            Some((a * x + c * y + e, b * x + d * y + f))
        };
        match operation.operator.as_str() {
            "q" => stack.push(transform),
            "Q" => {
                if let Some(saved) = stack.pop() {
                    transform = saved;
                }
            }
            "cm" => {
                let Some([a, b, c, d, e, f]) = (|| {
                    Some([
                        operand(0)?,
                        operand(1)?,
                        operand(2)?,
                        operand(3)?,
                        operand(4)?,
                        operand(5)?,
                    ])
                })() else {
                    continue;
                };
                let [u, v, w, x, y, z] = transform;
                transform = [
                    u * a + w * b,
                    v * a + x * b,
                    u * c + w * d,
                    v * c + x * d,
                    u * e + w * f + y,
                    v * e + x * f + z,
                ];
            }
            "m" => {
                start = point();
                segment = None;
            }
            "l" => {
                if let (Some((x1, y1)), Some((x2, y2))) = (start.take(), point()) {
                    segment = ((y1 - y2).abs() < 0.1).then(|| HorizontalRule {
                        left: x1.min(x2),
                        right: x1.max(x2),
                        y: (y1 + y2) / 2.0,
                    });
                } else {
                    segment = None;
                }
            }
            "S" | "s" => {
                if let Some(rule) = segment.take() {
                    lines.push(rule);
                }
                start = None;
            }
            "re" | "h" | "c" | "v" | "y" | "n" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                start = None;
                segment = None;
            }
            _ => {}
        }
    }
    lines
}

pub(crate) struct SourceCrops<'a> {
    input: &'a Path,
    meta: Option<Option<PdfMeta>>,
    raster: Option<(usize, RgbImage)>,
}

impl<'a> SourceCrops<'a> {
    pub(crate) fn new(input: &'a Path) -> Self {
        Self {
            input,
            meta: None,
            raster: None,
        }
    }

    pub fn crop(&mut self, doc: &Value, formula: &Value) -> Option<(String, Vec<u8>)> {
        if let Some(figure) = picture_crop(doc, formula) {
            return Some(figure);
        }

        let crop = self.raster(formula, 0.0)?;
        Some(("image/png".to_owned(), encode_png(&crop)?))
    }

    /// Rasterize the source region, expanding each edge by the specified
    /// fraction of the smaller region dimension.
    pub(crate) fn raster(&mut self, item: &Value, expansion: f64) -> Option<RgbImage> {
        let bounds = Bounds::from_item(item)?;
        let smaller = (bounds.right - bounds.left).min(bounds.top - bounds.bottom);
        if !smaller.is_finite() || smaller <= 0.0 {
            return None;
        }
        let margin = smaller * expansion;
        let page = usize::try_from(page_no(item)).ok()?.checked_sub(1)?;
        let meta = self
            .meta
            .get_or_insert_with(|| PdfMeta::open(&std::fs::read(self.input).ok()?))
            .as_ref()?;
        let geometry = meta.geometry(page)?;
        if geometry.rotation != 0
            || !geometry.width.is_finite()
            || !geometry.height.is_finite()
            || geometry.width <= 0.0
            || geometry.height <= 0.0
        {
            return None;
        }
        let width = f64::from(geometry.width);
        let height = f64::from(geometry.height);
        if self
            .raster
            .as_ref()
            .is_none_or(|(previous, _)| *previous != page)
        {
            let scale = 2.0_f64.min((16_000_000.0 / (width * height)).sqrt());
            self.raster = Some((page, Renderer::new(meta).render_scaled(page, scale)?));
        }

        let raster = &self.raster.as_ref()?.1;
        let scale_x = f64::from(raster.width()) / width;
        let scale_y = f64::from(raster.height()) / height;
        let left = ((bounds.left - 2.0 - margin).clamp(0.0, width) * scale_x).floor() as u32;
        let right = ((bounds.right + 2.0 + margin).clamp(0.0, width) * scale_x).ceil() as u32;
        let top =
            ((height - bounds.top - 3.0 - margin).clamp(0.0, height) * scale_y).floor() as u32;
        let bottom =
            ((height - bounds.bottom + 2.0 + margin).clamp(0.0, height) * scale_y).ceil() as u32;
        let right = right.min(raster.width());
        let bottom = bottom.min(raster.height());
        if right <= left || bottom <= top {
            return None;
        }
        Some(imageops::crop_imm(raster, left, top, right - left, bottom - top).to_image())
    }
}

pub(super) fn encode_png(image: &RgbImage) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ColorType::Rgb8.into(),
        )
        .ok()?;
    Some(bytes)
}

fn picture_crop(doc: &Value, formula: &Value) -> Option<(String, Vec<u8>)> {
    let target = Bounds::from_item(formula)?;
    let area = (target.right - target.left) * (target.top - target.bottom);
    if area <= 0.0 {
        return None;
    }
    doc["pictures"]
        .as_array()?
        .iter()
        .filter(|picture| page_no(picture) == page_no(formula))
        .find_map(|picture| {
            let bounds = Bounds::from_item(picture)?;
            let overlap = (target.right.min(bounds.right) - target.left.max(bounds.left)).max(0.0)
                * (target.top.min(bounds.top) - target.bottom.max(bounds.bottom)).max(0.0);
            let picture_area = (bounds.right - bounds.left) * (bounds.top - bounds.bottom);
            if overlap >= area * 0.65 && picture_area <= area * 2.0 {
                embedded_image(&picture["image"])
            } else {
                None
            }
        })
}

pub(super) fn presentation(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches('$');
    let mut chars = raw.chars().peekable();
    let body = sequence(&mut chars, 0, false)?;
    (!body.is_empty()).then(|| format!(
        "<math xmlns=\"http://www.w3.org/1998/Math/MathML\" display=\"block\"><mrow>{body}</mrow></math>"
    ))
}

/// The decoder can insert whitespace between the digits of a number.
pub(super) fn compact_decoded_numbers(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for part in text.split_whitespace() {
        let follows_number = result.ends_with(|ch: char| ch.is_ascii_digit())
            && part.starts_with(|ch: char| ch.is_ascii_digit() || ch == '.');
        let follows_decimal = result
            .strip_suffix('.')
            .is_some_and(|prefix| prefix.ends_with(|ch: char| ch.is_ascii_digit()))
            && part.starts_with(|ch: char| ch.is_ascii_digit());
        if !result.is_empty() && !follows_number && !follows_decimal {
            result.push(' ');
        }
        result.push_str(part);
    }
    result
}

pub(super) fn standalone_equation(text: &str) -> bool {
    let Some((left, right)) = text.split_once('=') else {
        return false;
    };
    !left.trim().is_empty()
        && !right.trim().is_empty()
        && text.chars().all(|character| {
            character.is_whitespace()
                || is_math_character(character)
                || matches!(character, '\u{0304}' | '\u{0305}')
        })
        && text
            .split(|character: char| !character.is_alphabetic())
            .all(|word| {
                word.chars().count() <= 1
                    || (word.chars().count() <= 3 && word.chars().all(char::is_uppercase))
            })
}

fn sequence(chars: &mut Peekable<Chars<'_>>, depth: usize, grouped: bool) -> Option<String> {
    if depth > 32 {
        return None;
    }
    let mut mathml = String::new();
    while let Some(&character) = chars.peek() {
        if character == '}' {
            chars.next();
            return grouped.then_some(mathml);
        }
        if character.is_whitespace() {
            chars.next();
            continue;
        }
        let base = atom(chars, depth)?;
        let mut subscript = None;
        let mut superscript = None;
        loop {
            skip_space(chars);
            let script = match chars.peek() {
                Some('_') if subscript.is_none() => &mut subscript,
                Some('^') if superscript.is_none() => &mut superscript,
                _ => break,
            };
            chars.next();
            *script = Some(argument(chars, depth)?);
        }
        match (subscript, superscript) {
            (Some(sub), Some(sup)) => {
                mathml.push_str(&format!("<msubsup>{base}{sub}{sup}</msubsup>"))
            }
            (Some(sub), None) => mathml.push_str(&format!("<msub>{base}{sub}</msub>")),
            (None, Some(sup)) => mathml.push_str(&format!("<msup>{base}{sup}</msup>")),
            (None, None) => mathml.push_str(&base),
        }
    }
    (!grouped).then_some(mathml)
}

fn skip_space(chars: &mut Peekable<Chars<'_>>) {
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
}

fn argument(chars: &mut Peekable<Chars<'_>>, depth: usize) -> Option<String> {
    if depth > 32 {
        return None;
    }
    skip_space(chars);
    atom(chars, depth + 1)
}

fn atom(chars: &mut Peekable<Chars<'_>>, depth: usize) -> Option<String> {
    let character = chars.next()?;
    match character {
        '{' => Some(format!(
            "<mrow>{}</mrow>",
            sequence(chars, depth + 1, true)?
        )),
        '\\' => {
            let mut name = String::new();
            while chars.peek().is_some_and(char::is_ascii_alphabetic) {
                name.push(chars.next()?);
            }
            match name.as_str() {
                "bar" | "overline" => Some(format!(
                    "<mover accent=\"true\">{}<mo>¯</mo></mover>",
                    argument(chars, depth)?
                )),
                "frac" => Some(format!(
                    "<mfrac>{}{}</mfrac>",
                    argument(chars, depth)?,
                    argument(chars, depth)?
                )),
                "sqrt" => Some(format!("<msqrt>{}</msqrt>", argument(chars, depth)?)),
                "text" | "mathrm" => {
                    skip_space(chars);
                    if chars.next()? != '{' {
                        return None;
                    }
                    let mut text = String::new();
                    loop {
                        match chars.next()? {
                            '}' => break,
                            '{' | '\\' => return None,
                            character => text.push(character),
                        }
                    }
                    Some(format!("<mtext>{}</mtext>", escape_xml(&text)))
                }
                "left" | "right" => argument(chars, depth),
                "Pr" => Some("<mi mathvariant=\"normal\">Pr</mi>".to_owned()),
                "quad" => Some("<mspace width=\"1em\"/>".to_owned()),
                "qquad" => Some("<mspace width=\"2em\"/>".to_owned()),
                "" => match chars.next()? {
                    ' ' | ',' | ';' | ':' => Some("<mspace width=\"0.25em\"/>".to_owned()),
                    delimiter @ ('{' | '}' | '|') => Some(format!("<mo>{delimiter}</mo>")),
                    _ => None,
                },
                operator => {
                    let symbol = match operator {
                        "cdot" => "·",
                        "cdots" => "⋯",
                        "ldots" | "dots" => "…",
                        "times" => "×",
                        "div" => "÷",
                        "pm" => "±",
                        "mp" => "∓",
                        "neq" | "ne" => "≠",
                        "approx" => "≈",
                        "equiv" => "≡",
                        "le" | "leq" => "≤",
                        "ge" | "geq" => "≥",
                        "in" => "∈",
                        "cup" => "∪",
                        "cap" => "∩",
                        "lor" | "vee" => "∨",
                        "land" | "wedge" => "∧",
                        "neg" | "lnot" => "¬",
                        "oplus" => "⊕",
                        "otimes" => "⊗",
                        "to" | "rightarrow" => "→",
                        "leftarrow" => "←",
                        "infty" => "∞",
                        "sum" => "∑",
                        "prod" => "∏",
                        "int" => "∫",
                        "bmod" | "mod" => "mod",
                        "colon" => ":",
                        "ell" => return Some("<mi>ℓ</mi>".to_owned()),
                        "alpha" => return Some("<mi>α</mi>".to_owned()),
                        "beta" => return Some("<mi>β</mi>".to_owned()),
                        "gamma" => return Some("<mi>γ</mi>".to_owned()),
                        "pi" => return Some("<mi>π</mi>".to_owned()),
                        _ => return None,
                    };
                    Some(format!("<mo>{symbol}</mo>"))
                }
            }
        }
        character if character.is_alphabetic() => {
            let base = format!("<mi>{character}</mi>");
            if matches!(chars.peek(), Some('\u{0304}' | '\u{0305}')) {
                chars.next();
                Some(format!("<mover accent=\"true\">{base}<mo>¯</mo></mover>"))
            } else {
                Some(base)
            }
        }
        character if character.is_ascii_digit() => {
            let mut digits = String::from(character);
            loop {
                let mut rest = chars.clone();
                if rest.peek() == Some(&'.') {
                    rest.next();
                    if !digits.contains('.') && rest.peek().is_some_and(char::is_ascii_digit) {
                        digits.push('.');
                    } else {
                        break;
                    }
                }
                if !rest.peek().is_some_and(char::is_ascii_digit) {
                    break;
                }
                digits.push(rest.next()?);
                *chars = rest;
            }
            Some(format!("<mn>{digits}</mn>"))
        }
        character if is_math_character(character) => {
            Some(format!("<mo>{}</mo>", escape_xml(&character.to_string())))
        }
        _ => None,
    }
}

fn is_math_character(character: char) -> bool {
    character.is_alphanumeric() || "+-=−×·÷±∓≤≥≈≠≡∈∉∪∩∧∨¬⊕⊗∑∫∞.,:;!′()[]|<>/%*".contains(character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_shift_cipher_and_probability_notation_from_the_decoder() {
        let decoded = compact_decoded_numbers(
            r"\text {Enc} _ { k } ( m _ { 1 } \cdots m _ { \ell } ) = c _ { 1 } \cdots c _ { \ell } , \quad \text {where } c _ { i } = [ ( m _ { i } + k ) \bmod 2 6 ] .",
        );
        let mathml = presentation(&decoded).expect("shift cipher MathML");
        assert!(mathml.contains("<mn>26</mn>"));
        assert!(mathml.contains("<msub>"));
        assert!(consistent_with_source(
            &mathml,
            "Enck(m1 · · · m`) = c1 · · · c`, where ci = [(mi + k) mod 26]."
        ));

        let probability = presentation(&compact_decoded_numbers(r"\Pr[M=a]=0 . 7")).unwrap();
        assert!(probability.contains("<mn>0.7</mn>"));
        assert!(!consistent_with_source(
            &probability,
            "Pr[M=a]=0.7 and Pr[M=z]=0.3"
        ));
    }

    #[test]
    fn locates_undecoded_formula_between_source_regions() {
        let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/math.pdf");
        let location = |top, bottom| {
            json!([{"page_no": 1, "bbox": {
                "coord_origin": "BOTTOMLEFT", "l": 48, "r": 365,
                "t": top, "b": bottom
            }}])
        };
        let mut doc = json!({
            "body": {"children": [
                {"$ref": "#/texts/0"}, {"$ref": "#/texts/1"}, {"$ref": "#/texts/2"}
            ]},
            "texts": [
                {"self_ref": "#/texts/0", "label": "text",
                    "text": "The following formula negates only A.", "prov": location(510, 485)},
                {"self_ref": "#/texts/1", "text": "<!-- formula-not-decoded -->"},
                {"self_ref": "#/texts/2", "text": "After", "prov": location(391, 380)}
            ]
        });
        locate_placeholders(&mut doc, &input);
        let formula = &doc["texts"][1];
        assert_eq!(formula["label"], "formula");
        assert_eq!(formula["text"], "");
        assert_eq!(page_no(formula), 1);
        assert!(SourceCrops::new(&input).raster(formula, 0.18).is_some());
        let pdf = Document::load(&input).unwrap();
        let raw = PageGeometry::new(&pdf, &input)
            .recover_formula(formula)
            .unwrap();
        assert_eq!(raw, "\\overline{A} + B = C");
    }
}
