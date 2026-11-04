use std::path::Path;
use std::sync::LazyLock;

use docling_pdf::textparse::PageTextParser;
use lopdf::{Document, Object, decode_text_string};
use regex::Regex;
use roxmltree::Node;

use super::{clean_text, is_language_tag, resolve};
use crate::MetadataOverrides;

const DC: &str = "http://purl.org/dc/elements/1.1/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

static EDITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:,\s*|\.\s+|\s+)",
        r"([0-9]{1,3}(?:st|nd|rd|th|[-‑–]?е)?|first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth)",
        r"\s+(?:edition|ed\.|изд(?:ание)?\.?)\s*(\([^()]+\))?$"
    ))
    .unwrap()
});

static CATALOG_ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^[А-ЯЁ][0-9]{2}\s+(.+?)\s+[-–—]\s+[^:]{2,35}:\s*([^,]{2,70}),\s*",
        r"([0-9]{4})\.\s+[-–—]\s+[0-9]+\s*с\."
    ))
    .unwrap()
});

pub(super) fn infer(pdf: &Document, input: &Path) -> MetadataOverrides {
    let mut inferred = embedded(pdf);
    if (inferred.title.is_none()
        || inferred.authors.is_empty()
        || inferred.year.is_none()
        || inferred.edition.is_none()
        || inferred.publisher.is_none()
        || inferred.language.is_none())
        && let Some(front) = front_matter(input)
    {
        fill(&mut inferred, front);
    }
    if let Some(stem) = input.file_stem().and_then(|name| name.to_str()) {
        fill(&mut inferred, filename(stem));
    }
    inferred
}

pub(super) fn fill(fields: &mut MetadataOverrides, fallback: MetadataOverrides) {
    fields.title = fields.title.take().or(fallback.title);
    if fields.authors.is_empty() {
        fields.authors = fallback.authors;
    }
    fields.year = fields.year.or(fallback.year);
    fields.edition = fields.edition.take().or(fallback.edition);
    fields.language = fields.language.take().or(fallback.language);
    fields.publisher = fields.publisher.take().or(fallback.publisher);
    fields.description = fields.description.take().or(fallback.description);
}

fn embedded(pdf: &Document) -> MetadataOverrides {
    let catalog = pdf.catalog().ok();
    let xmp = catalog
        .and_then(|catalog| catalog.get_deref(b"Metadata", pdf).ok())
        .and_then(|object| object.as_stream().ok())
        .and_then(|stream| stream.get_plain_content_with_limit(2 * 1024 * 1024).ok());
    let xml = xmp
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .and_then(|text| roxmltree::Document::parse(text).ok());
    let property = |namespace, name| {
        xml.as_ref()?
            .descendants()
            .find(|node| node.has_tag_name((namespace, name)))
    };
    let dc = |name| property(DC, name).and_then(scalar);
    let info = pdf
        .trailer
        .get_deref(b"Info", pdf)
        .ok()
        .and_then(|object| object.as_dict().ok());
    let info_field = |name: &[u8]| pdf_string(pdf, info?.get(name).ok()?);
    let title = dc("title").or_else(|| info_field(b"Title"));
    let (title, title_edition) = title.map_or((None, None), |title| {
        let (title, edition) = title_and_edition(&title);
        (Some(title), edition)
    });
    let mut authors = property(DC, "creator").map(list).unwrap_or_default();
    if authors.is_empty() {
        authors.extend(info_field(b"Author"));
    }
    MetadataOverrides {
        title,
        authors,
        year: dc("date")
            .as_deref()
            .and_then(|date| year(date.split('-').next()?)),
        edition: property("https://schema.org/", "bookEdition")
            .or_else(|| property("http://schema.org/", "bookEdition"))
            .and_then(scalar)
            .or(title_edition),
        language: catalog
            .and_then(|catalog| pdf_string(pdf, catalog.get(b"Lang").ok()?))
            .filter(|lang| is_language_tag(lang))
            .or_else(|| dc("language").filter(|lang| is_language_tag(lang))),
        publisher: dc("publisher"),
        description: dc("description"),
    }
}

fn pdf_string(pdf: &Document, value: &Object) -> Option<String> {
    meaningful(&decode_text_string(resolve(pdf, value)).ok()?)
}

fn meaningful(text: &str) -> Option<String> {
    let text = clean_text(text);
    (!text.is_empty()
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "untitled" | "unknown" | "n/a"
        ))
    .then_some(text)
}

fn scalar(node: Node<'_, '_>) -> Option<String> {
    let mut items = node
        .descendants()
        .filter(|child| child.has_tag_name((RDF, "li")));
    let Some(first) = items.next() else {
        return meaningful(node.text()?);
    };
    if first.attribute((XML, "lang")) == Some("x-default") {
        return meaningful(first.text()?);
    }
    let Some(second) = items.next() else {
        return meaningful(first.text()?);
    };
    std::iter::once(second)
        .chain(items)
        .find(|item| item.attribute((XML, "lang")) == Some("x-default"))
        .and_then(|item| meaningful(item.text()?))
}

fn list(node: Node<'_, '_>) -> Vec<String> {
    let values: Vec<_> = node
        .descendants()
        .filter(|child| child.has_tag_name((RDF, "li")))
        .filter_map(|child| meaningful(child.text()?))
        .collect();
    if values.is_empty() {
        node.text().and_then(meaningful).into_iter().collect()
    } else {
        values
    }
}

fn filename(stem: &str) -> MetadataOverrides {
    let parsed = (|| {
        let (byline, rest) = stem.split_once(" - ")?;
        let (title, date) = rest.rsplit_once(" - ")?;
        let authors = initialed_authors(byline)?;
        let year = year(date)?;
        let (title, edition) = title_and_edition(title);
        (!title.is_empty()).then_some(MetadataOverrides {
            title: Some(title),
            authors,
            year: Some(year),
            edition,
            ..MetadataOverrides::default()
        })
    })();
    parsed.unwrap_or_else(|| {
        let (title, edition) = title_and_edition(stem);
        MetadataOverrides {
            title: meaningful(&title),
            edition,
            ..MetadataOverrides::default()
        }
    })
}

fn initialed_authors(byline: &str) -> Option<Vec<String>> {
    byline
        .split([',', ';'])
        .map(|author| {
            let author = author.trim();
            let mut has_initial = false;
            let mut has_name = false;
            for token in author.split_whitespace() {
                if let Some(letters) = token.strip_suffix('.') {
                    if letters.is_empty()
                        || !letters.split('.').all(|letter| {
                            letter.chars().count() == 1 && letter.starts_with(char::is_uppercase)
                        })
                    {
                        return None;
                    }
                    has_initial = true;
                } else if token.starts_with(char::is_uppercase)
                    && token
                        .chars()
                        .all(|ch| ch.is_alphabetic() || matches!(ch, '-' | '\'' | '’'))
                {
                    has_name = true;
                } else {
                    return None;
                }
            }
            (has_initial && has_name).then(|| author.to_owned())
        })
        .collect()
}

fn title_and_edition(title: &str) -> (String, Option<String>) {
    let title = title.trim();
    let Some(parts) = EDITION.captures(title) else {
        return (title.to_owned(), None);
    };
    let base = title[..parts.get(0).unwrap().start()].trim();
    if base.is_empty() {
        return (title.to_owned(), None);
    }
    let number = parts[1].to_lowercase();
    let edition = match number.as_str() {
        "first" => "1",
        "second" => "2",
        "third" => "3",
        "fourth" => "4",
        "fifth" => "5",
        "sixth" => "6",
        "seventh" => "7",
        "eighth" => "8",
        "ninth" => "9",
        "tenth" => "10",
        number => number.trim_end_matches(|ch: char| !ch.is_ascii_digit()),
    };
    let title = parts.get(2).map_or_else(
        || base.to_owned(),
        |tail| format!("{base} {}", tail.as_str()),
    );
    (title, Some(edition.to_owned()))
}

fn year(date: &str) -> Option<u16> {
    (date.len() == 4 && date.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| date.parse::<u16>().ok())
        .flatten()
        .filter(|year| (1000..=2999).contains(year))
}

fn front_matter(input: &Path) -> Option<MetadataOverrides> {
    let mut parser = PageTextParser::open(&std::fs::read(input).ok()?)?;
    let mut front = MetadataOverrides::default();
    for index in 0..parser.page_count().min(8) {
        let page = parser.text_page(index);
        let lines: Vec<_> = page.cells.iter().map(|cell| cell.text.trim()).collect();
        let Some(isbn) = lines.iter().position(|line| {
            (line.starts_with("ISBN") || line.starts_with("International Standard Book Number"))
                && line.bytes().filter(u8::is_ascii_digit).count() >= 10
        }) else {
            continue;
        };
        if let Some(record) = catalog_entry(&lines[isbn.saturating_sub(4)..isbn]) {
            fill(&mut front, record);
        }
        if front.language.is_none()
            && lines
                .iter()
                .any(|line| line.to_lowercase().contains("издание на русском языке"))
        {
            front.language = Some("ru".to_owned());
        }
        if front.publisher.is_none() {
            front.publisher = lines[..isbn].iter().find_map(|line| {
                let (imprint, _) = line.split_once(" is an imprint of ")?;
                lines[..isbn].contains(&imprint).then(|| imprint.to_owned())
            });
        }
    }
    Some(front)
}

fn catalog_entry(lines: &[&str]) -> Option<MetadataOverrides> {
    let (byline, record) = lines.windows(2).rev().find_map(|pair| {
        Some((
            initialed_authors(pair[0])?,
            CATALOG_ENTRY.captures(pair[1])?,
        ))
    })?;
    let (title, edition) = title_and_edition(record[1].trim_end_matches('.'));
    Some(MetadataOverrides {
        title: meaningful(&title),
        authors: byline,
        year: year(&record[3]),
        edition,
        publisher: meaningful(&record[2]),
        ..MetadataOverrides::default()
    })
}

#[cfg(test)]
mod tests {
    use lopdf::{Stream, dictionary};

    use super::*;

    #[test]
    fn extracts_only_explicit_filename_fields() {
        let fields = filename(
            "Таненбаум Э., Уэзэрол Д. - Компьютерные сети, 5 изд (Классика Computer Science) - 2012",
        );
        assert_eq!(fields.authors, ["Таненбаум Э.", "Уэзэрол Д."]);
        assert_eq!(
            fields.title.as_deref(),
            Some("Компьютерные сети (Классика Computer Science)")
        );
        assert_eq!(fields.edition.as_deref(), Some("5"));
        assert_eq!(fields.year, Some(2012));
        assert!(fields.publisher.is_none());

        let ambiguous = filename("Draft Notes - Final Copy - 2024");
        assert_eq!(
            ambiguous.title.as_deref(),
            Some("Draft Notes - Final Copy - 2024")
        );
        assert!(ambiguous.authors.is_empty());
        assert!(ambiguous.year.is_none());
    }

    #[test]
    fn uses_catalog_byline_and_publication_details() {
        let fields = catalog_entry(&[
            "Таненбаум Э., Бос Х.",
            "Т18 Современные операционные системы. 4-е изд. — СПб.: Питер, 2015. — 1120 с.: ил.",
        ])
        .unwrap();
        assert_eq!(
            fields.title.as_deref(),
            Some("Современные операционные системы")
        );
        assert_eq!(fields.authors, ["Таненбаум Э.", "Бос Х."]);
        assert_eq!(fields.edition.as_deref(), Some("4"));
        assert_eq!(fields.year, Some(2015));
        assert_eq!(fields.publisher.as_deref(), Some("Питер"));
    }

    #[test]
    fn reads_xmp_author_sequence_and_ignores_pdf_creation_date() {
        let mut pdf = Document::new();
        let xmp = pdf.add_object(Stream::new(dictionary! {}, br#"
          <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
                   xmlns:dc="http://purl.org/dc/elements/1.1/">
            <rdf:Description>
              <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Cryptography, Second Edition</rdf:li></rdf:Alt></dc:title>
              <dc:creator><rdf:Seq><rdf:li>J. Katz</rdf:li><rdf:li>Y. Lindell</rdf:li></rdf:Seq></dc:creator>
              <dc:publisher><rdf:Bag><rdf:li>CRC Press</rdf:li></rdf:Bag></dc:publisher>
            </rdf:Description>
          </rdf:RDF>"#.to_vec()));
        let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Metadata" => xmp });
        pdf.trailer.set("Root", root);
        pdf.trailer.set(
            "Info",
            dictionary! { "Author" => "J. Katz", "CreationDate" => "D:20140929" },
        );

        let fields = embedded(&pdf);
        assert_eq!(fields.title.as_deref(), Some("Cryptography"));
        assert_eq!(fields.authors, ["J. Katz", "Y. Lindell"]);
        assert_eq!(fields.edition.as_deref(), Some("2"));
        assert_eq!(fields.publisher.as_deref(), Some("CRC Press"));
        assert!(fields.year.is_none());
    }
}
