use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use roxmltree::{Document, ParsingOptions};

use super::references::resolve_href;

const XML: &str = "http://www.w3.org/XML/1998/namespace";
const SVG: &str = "http://www.w3.org/2000/svg";

static CSS_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix) \b url\( (?: [\t\r\n\x0c\x20]*
        (?: "([^"]*)" | '([^']*)' | ([^"'()\t\r\n\x0c\x20]*) ) [\t\r\n\x0c\x20]* \) )?"#,
    )
    .expect("CSS url pattern")
});
static OTHER_CSS_URL: LazyLock<Regex> = LazyLock::new(|| {
    // These functions can use strings or variables to specify a URL.
    Regex::new(r"(?i)\b(?:image(?:-set)?|src)\(").expect("CSS image function pattern")
});

/// Find images that are referenced outside the attributes rewritten during optimization.
pub(super) fn protected_images(
    documents: &HashMap<String, String>,
    stylesheets: &HashMap<String, String>,
    image_paths: &HashSet<&str>,
) -> HashSet<String> {
    if image_paths.is_empty() {
        return HashSet::new();
    }
    let mut protected = HashSet::new();
    let mut record = |from: &str, href: &str| {
        if let Some((path, _)) = resolve_href(from, href)
            && image_paths.contains(path.as_str())
        {
            protected.insert(path);
        }
    };
    for (path, stylesheet) in stylesheets {
        if !css_references(path, stylesheet, &mut record) {
            return image_paths.iter().map(|path| (*path).to_owned()).collect();
        }
    }
    for (path, xml) in documents {
        if !document_references(path, xml, &mut record) {
            return image_paths.iter().map(|path| (*path).to_owned()).collect();
        }
    }
    protected
}

fn document_references(path: &str, xml: &str, record: &mut impl FnMut(&str, &str)) -> bool {
    let options = ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let Ok(document) = Document::parse_with_options(xml, options) else {
        return false;
    };
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        let tag = node.tag_name();
        if matches!(tag.name(), "script" | "base") || node.attribute((XML, "base")).is_some() {
            return false;
        }
        if tag.name() == "style" {
            let css: String = node.children().filter_map(|child| child.text()).collect();
            if !css_references(path, &css, record) {
                return false;
            }
        }
        for attribute in node.attributes() {
            let value = attribute.value();
            match attribute.name() {
                "srcset" | "imagesrcset" => srcset_urls(value).for_each(|url| record(path, url)),
                "href" | "src"
                    if value
                        .trim_ascii()
                        .to_ascii_lowercase()
                        .starts_with("javascript:") =>
                {
                    return false;
                }
                "background" => record(path, value),
                name if name.starts_with("on") => return false,
                name if (name == "style" || tag.namespace() == Some(SVG))
                    && !css_references(path, value, record) =>
                {
                    return false;
                }
                _ => {}
            }
        }
        if node
            .attribute("type")
            .is_some_and(|mime| mime.starts_with("image/"))
        {
            for attr in ["data", "src", "href"] {
                if let Some(url) = node.attribute(attr) {
                    record(path, url);
                }
            }
        }
    }
    true
}

fn css_references(path: &str, css: &str, record: &mut impl FnMut(&str, &str)) -> bool {
    if css.contains('\\') || OTHER_CSS_URL.is_match(css) {
        return false;
    }
    for captures in CSS_URL.captures_iter(css) {
        let Some(url) = captures.iter().skip(1).flatten().next() else {
            return false;
        };
        record(path, url.as_str());
    }
    true
}

fn srcset_urls(mut input: &str) -> impl Iterator<Item = &str> {
    // A URL token can contain commas, including the separator inside a data URI.
    // A trailing comma finishes a URL without descriptors.
    std::iter::from_fn(move || {
        input = input.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if input.is_empty() {
            return None;
        }
        let url_end = input
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(input.len());
        let (url, rest) = input.split_at(url_end);
        input = rest;
        if url.ends_with(',') {
            return Some(url.trim_end_matches(','));
        }
        let mut in_parens = false;
        let separator = input.find(|character| match character {
            '(' => {
                in_parens = true;
                false
            }
            ')' => {
                in_parens = false;
                false
            }
            ',' => !in_parens,
            _ => false,
        });
        input = separator.map_or("", |index| &input[index + 1..]);
        Some(url)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_only_images_referenced_by_css_and_srcset() {
        let documents = HashMap::from([(
            "EPUB/text/a.xhtml".to_owned(),
            r#"<html><head><style>.x{background:url(../style.png)}</style></head><body>
            <img style="background:url('../inline%20image.png')" src="../ordinary.png"
                 srcset="data:image/png;base64,AAAA 1x, ../medium.png 2x, ../a,b.png,
                         ../large.png 1000w"/>
            </body></html>"#
                .to_owned(),
        )]);
        let stylesheets = HashMap::from([(
            "EPUB/css/book.css".to_owned(),
            r#".a { background: URL( "../quote)s.png" ) }"#.to_owned(),
        )]);
        let expected = [
            "style.png",
            "inline image.png",
            "medium.png",
            "a,b.png",
            "large.png",
            "quote)s.png",
        ];
        let expected: HashSet<_> = expected
            .into_iter()
            .map(|name| format!("EPUB/{name}"))
            .collect();
        let mut available: HashSet<_> = expected.iter().map(String::as_str).collect();
        available.insert("EPUB/ordinary.png");
        assert_eq!(
            protected_images(&documents, &stylesheets, &available),
            expected
        );
    }

    #[test]
    fn preserves_all_images_when_static_references_are_ambiguous() {
        let known = HashSet::from(["EPUB/cover.png"]);
        let expected = HashSet::from(["EPUB/cover.png".to_owned()]);
        for xml in [
            "<html xml:base='../' />",
            "<html><script/></html>",
            "<img onload='update()' />",
        ] {
            let documents = HashMap::from([("EPUB/a.xhtml".to_owned(), xml.to_owned())]);
            assert_eq!(
                protected_images(&documents, &HashMap::new(), &known),
                expected
            );
        }
    }
}
