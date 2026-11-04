use std::collections::{BTreeMap, HashMap};
use std::iter::Peekable;
use std::path::Path;
use std::str::Chars;

use docling_pdf::{pdf_meta::PdfMeta, render::Renderer};
use image::{ColorType, ImageEncoder, RgbImage, codecs::png::PngEncoder, imageops};
use lopdf::{Document, ObjectId, content::Content};
use serde_json::Value;

use super::{embedded_image, escape_xml, page_no};

pub(super) struct PageGeometry<'a> {
    pdf: &'a Document,
    pages: BTreeMap<u32, ObjectId>,
    lines: HashMap<u32, Vec<HorizontalRule>>,
}

#[derive(Clone, Copy)]
struct HorizontalRule {
    left: f64,
    right: f64,
    y: f64,
}

struct Bounds {
    left: f64,
    right: f64,
    bottom: f64,
    top: f64,
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
    pub fn new(pdf: &'a Document) -> Self {
        Self {
            pdf,
            pages: pdf.get_pages(),
            lines: HashMap::new(),
        }
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

pub(super) struct SourceCrops<'a> {
    input: &'a Path,
    meta: Option<Option<PdfMeta>>,
    raster: Option<(usize, RgbImage)>,
}

impl<'a> SourceCrops<'a> {
    pub fn new(input: &'a Path) -> Self {
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

        let bounds = Bounds::from_item(formula)?;
        let page = usize::try_from(page_no(formula)).ok()?.checked_sub(1)?;
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
        let left = ((bounds.left - 2.0).clamp(0.0, width) * scale_x).floor() as u32;
        let right = ((bounds.right + 2.0).clamp(0.0, width) * scale_x).ceil() as u32;
        let top = ((height - bounds.top - 3.0).clamp(0.0, height) * scale_y).floor() as u32;
        let bottom = ((height - bounds.bottom + 2.0).clamp(0.0, height) * scale_y).ceil() as u32;
        let right = right.min(raster.width());
        let bottom = bottom.min(raster.height());
        if right <= left || bottom <= top {
            return None;
        }
        let crop = imageops::crop_imm(raster, left, top, right - left, bottom - top).to_image();
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(
                crop.as_raw(),
                crop.width(),
                crop.height(),
                ColorType::Rgb8.into(),
            )
            .ok()?;
        Some(("image/png".to_owned(), bytes))
    }
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
            while chars.peek().is_some_and(char::is_ascii_digit) {
                digits.push(chars.next()?);
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
