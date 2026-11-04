use std::collections::{BTreeMap, HashMap};

use lopdf::{Document, Object, ObjectId, content::Content};
use serde_json::Value;

const STRESS_MARK: char = '\u{e088}';
// The PDF encodes these glyphs in each font's private-use space.
const SYMBOLS: [(char, &[u8], char); 4] = [
    ('\u{f0cd}', b"Wingdings2", '×'),
    ('\u{f0ce}', b"Wingdings2", '×'),
    ('\u{f09e}', b"Wingdings-Regular", '·'),
    (STRESS_MARK, b"SchoolBookAC", '\u{0301}'),
];
const MATCHING_FONT: u8 = 1;
const OTHER_FONT: u8 = 2;
type GlyphUses = [u8; SYMBOLS.len()];

pub(super) fn comparison_character(character: char) -> char {
    SYMBOLS
        .iter()
        .find_map(|&(codepoint, _, replacement)| {
            (codepoint == character && codepoint != STRESS_MARK).then_some(replacement)
        })
        .unwrap_or(character)
}

pub(crate) fn repair_symbols(document: &mut Value, pdf: &Document) {
    let mut evidence = FontEvidence {
        pdf,
        pages: pdf.get_pages(),
        cache: HashMap::new(),
    };
    if let Some(texts) = document["texts"].as_array_mut() {
        for item in texts {
            if let Some(text) = item["text"].as_str()
                && let Some(repaired) = evidence.repair(text, &item["prov"])
            {
                item["text"] = Value::String(repaired);
            }
        }
    }
    if let Some(tables) = document["tables"].as_array_mut() {
        for table in tables {
            let provenance = table["prov"].clone();
            if let Some(cells) = table["data"]["table_cells"].as_array_mut() {
                for cell in cells {
                    if let Some(text) = cell["text"].as_str()
                        && let Some(repaired) = evidence.repair(text, &provenance)
                    {
                        cell["text"] = Value::String(repaired);
                    }
                }
            }
        }
    }
}

struct FontEvidence<'a> {
    pdf: &'a Document,
    pages: BTreeMap<u32, ObjectId>,
    cache: HashMap<u32, Option<GlyphUses>>,
}

impl FontEvidence<'_> {
    fn repair(&mut self, text: &str, provenance: &Value) -> Option<String> {
        if !SYMBOLS
            .iter()
            .any(|(codepoint, _, _)| text.contains(*codepoint))
        {
            return None;
        }
        let mut fonts = GlyphUses::default();
        for region in provenance.as_array()? {
            let page = u32::try_from(region["page_no"].as_u64()?).ok()?;
            let evidence = (*self
                .cache
                .entry(page)
                .or_insert_with(|| page_fonts(self.pdf, *self.pages.get(&page)?)))?;
            for (combined, observed) in fonts.iter_mut().zip(evidence) {
                *combined |= observed;
            }
        }
        fonts.contains(&MATCHING_FONT).then(|| replace(text, fonts))
    }
}

fn replace(text: &str, fonts: GlyphUses) -> String {
    let mut repaired = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        let replacement =
            SYMBOLS
                .iter()
                .zip(fonts)
                .find_map(|(&(codepoint, _, replacement), usage)| {
                    (codepoint == character && usage == MATCHING_FONT).then_some(replacement)
                });
        if character == STRESS_MARK && replacement.is_some() {
            // SchoolBookAC draws the accent over the following vowel. A
            // Unicode combining mark needs to follow the vowel instead.
            if characters
                .peek()
                .is_some_and(|vowel| "АЕЁИОУЫЭЮЯаеёиоуыэюя".contains(*vowel))
            {
                repaired.extend(characters.next());
            } else {
                repaired.push(character);
                continue;
            }
        }
        repaired.push(replacement.unwrap_or(character));
    }
    repaired
}

fn page_fonts(pdf: &Document, page: ObjectId) -> Option<GlyphUses> {
    let fonts = pdf.get_page_fonts(page).ok()?;
    let bytes = pdf
        .get_page_content_with_limit(page, 64 * 1024 * 1024)
        .ok()?;
    let content = Content::decode(&bytes).ok()?;
    let mut uses = GlyphUses::default();
    let mut active = None;
    let mut saved = Vec::new();
    let mut encodings = HashMap::new();
    for operation in &content.operations {
        let operands = &operation.operands;
        let text = match operation.operator.as_str() {
            "q" => {
                saved.push(active);
                continue;
            }
            "Q" => {
                active = saved.pop()?;
                continue;
            }
            "Tf" => {
                active = Some(operands.first()?.as_name().ok()?);
                continue;
            }
            // A form can carry its own fonts. Without inspecting that scope,
            // we cannot establish exclusive ownership of the codepoints.
            "Do" if !is_image(pdf, page, operands.first()?.as_name().ok()?) => return None,
            "Tj" | "'" | "\"" => std::slice::from_ref(operands.last()?),
            "TJ" => operands.first()?.as_array().ok()?.as_slice(),
            _ => continue,
        };
        let active = active?;
        let (family, encoding) = match encodings.entry(active) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let font = fonts.get(active)?;
                let name = font
                    .get(b"BaseFont")
                    .and_then(Object::as_name)
                    .unwrap_or_default();
                entry.insert((
                    font_family(name),
                    font.get_font_encoding_with_limit(pdf, 4 * 1024 * 1024)
                        .ok()?,
                ))
            }
        };
        for operand in text {
            if let Ok(bytes) = operand.as_str() {
                let decoded = Document::decode_text(encoding, bytes).ok()?;
                for ((codepoint, expected, _), seen) in SYMBOLS.iter().zip(&mut uses) {
                    if decoded.contains(*codepoint) {
                        *seen |= if family == expected {
                            MATCHING_FONT
                        } else {
                            OTHER_FONT
                        };
                    }
                }
            }
        }
    }
    Some(uses)
}

fn font_family(name: &[u8]) -> &[u8] {
    match name.split_at_checked(7) {
        Some((prefix, family))
            if prefix[6] == b'+' && prefix[..6].iter().all(u8::is_ascii_uppercase) =>
        {
            family
        }
        _ => name,
    }
}

fn is_image(pdf: &Document, page: ObjectId, name: &[u8]) -> bool {
    let Ok((direct, inherited)) = pdf.get_page_resources(page) else {
        return false;
    };
    direct
        .into_iter()
        .chain(
            inherited
                .into_iter()
                .filter_map(|id| pdf.get_dictionary(id).ok()),
        )
        .find_map(|resources| {
            let objects = resources.get_deref(b"XObject", pdf).ok()?.as_dict().ok()?;
            objects.get_deref(name, pdf).ok()
        })
        .and_then(|object| object.as_stream().ok())
        .is_some_and(|stream| {
            stream.dict.has_type(b"XObject")
                && stream.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image")
        })
}

#[cfg(test)]
mod tests {
    use lopdf::{Stream, dictionary};

    use super::*;

    #[test]
    fn repairs_only_font_proven_glyphs_and_places_stress_on_the_vowel() {
        let proven = [
            MATCHING_FONT,
            MATCHING_FONT | OTHER_FONT,
            MATCHING_FONT,
            MATCHING_FONT,
        ];
        assert_eq!(
            replace(
                "2 \u{f0cd} 2, \u{f0ce}, CS\u{f09e}RD, временн\u{e088}ая, \u{e088}x",
                proven
            ),
            "2 × 2, \u{f0ce}, CS·RD, временна́я, \u{e088}x"
        );
    }

    #[test]
    fn checks_the_font_that_actually_shows_the_private_character() {
        let mut pdf = Document::new();
        let cmap = pdf.add_object(Stream::new(
            dictionary! {},
            b"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<00AF> <F0CD>
endbfchar
endcmap CMapName currentdict /CMap defineresource pop end end
"
            .to_vec(),
        ));
        let font = |base| {
            dictionary! {
                "Type" => "Font",
                "Subtype" => "Type0",
                "BaseFont" => base,
                "Encoding" => "Identity-H",
                "ToUnicode" => cmap,
            }
        };
        let wingdings = pdf.add_object(font("ABCDEF+Wingdings2"));
        let other = pdf.add_object(font("OtherFont"));
        for (commands, expected) in [
            (&b"BT /W 12 Tf <00AF> Tj ET"[..], MATCHING_FONT),
            (
                &b"BT /W 12 Tf <00AF> Tj /X 12 Tf [<00AF>] TJ ET"[..],
                MATCHING_FONT | OTHER_FONT,
            ),
            (&b"BT /X 12 Tf <00AF> Tj ET"[..], OTHER_FONT),
        ] {
            let content = pdf.add_object(Stream::new(dictionary! {}, commands.to_vec()));
            let page = pdf.add_object(dictionary! {
                "Type" => "Page",
                "Resources" => dictionary! { "Font" => dictionary! { "W" => wingdings, "X" => other } },
                "Contents" => content,
            });
            assert_eq!(page_fonts(&pdf, page).unwrap()[0], expected);
        }
    }
}
