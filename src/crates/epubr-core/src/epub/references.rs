use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

// Escape path delimiters and colons, which can turn a leading segment into a URI scheme.
const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');
const FRAGMENT: &AsciiSet = &PATH_SEGMENT
    .remove(b'!')
    .remove(b'$')
    .remove(b'&')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')')
    .remove(b'*')
    .remove(b'+')
    .remove(b',')
    .remove(b';')
    .remove(b'=')
    .remove(b':')
    .remove(b'@')
    .remove(b'/')
    .remove(b'?');

/// Resolve a URI reference to a decoded ZIP path and fragment.
pub(super) fn resolve_href(from: &str, href: &str) -> Option<(String, Option<String>)> {
    let href = href.trim_ascii();
    if href.starts_with("//") || has_scheme(href) {
        return None;
    }
    let (target, fragment) = if let Some((target, fragment)) = href.split_once('#') {
        let fragment = percent_decode_str(fragment)
            .decode_utf8()
            .ok()?
            .into_owned();
        (target, Some(fragment))
    } else {
        (href, None)
    };
    let path = target.split_once('?').map_or(target, |(path, _)| path);
    if path.is_empty() {
        return Some((from.to_owned(), fragment));
    }

    let directory = if path.starts_with('/') {
        ""
    } else {
        parent(from)
    };
    let mut resolved: Vec<_> = components(directory)
        .map(std::borrow::Cow::Borrowed)
        .collect();
    for segment in components(path) {
        let segment = percent_decode_str(segment).decode_utf8().ok()?;
        match segment.as_ref() {
            "." => continue,
            ".." => {
                resolved.pop()?;
            }
            segment if segment.contains(['/', '\0']) => return None,
            _ => resolved.push(segment),
        }
    }
    Some((resolved.join("/"), fragment))
}

/// Build a URI reference between two decoded ZIP paths.
pub(super) fn relative_href(from: &str, to: &str, fragment: Option<&str>) -> String {
    let mut href = String::new();
    if from != to || fragment.is_none() {
        let source: Vec<_> = components(parent(from)).collect();
        let target: Vec<_> = components(to).collect();
        let shared = source
            .iter()
            .zip(&target[..target.len().saturating_sub(1)])
            .take_while(|(left, right)| left == right)
            .count();
        for _ in shared..source.len() {
            href.push_str("../");
        }
        for (index, segment) in target[shared..].iter().enumerate() {
            if index != 0 {
                href.push('/');
            }
            href.extend(utf8_percent_encode(segment, PATH_SEGMENT));
        }
    }
    if let Some(fragment) = fragment {
        href.push('#');
        href.extend(utf8_percent_encode(fragment, FRAGMENT));
    }
    href
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty())
}

fn has_scheme(href: &str) -> bool {
    href.split_once(':').is_some_and(|(scheme, _)| {
        let mut bytes = scheme.bytes();
        bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    })
}

#[cfg(test)]
mod tests {
    use super::{relative_href, resolve_href};

    #[test]
    fn moves_an_escaped_link_between_documents() {
        let from = "EPUB/text/one.xhtml";
        let path = "EPUB/new/re: % #.xhtml";
        let fragment = "ref: §#%";
        let href = relative_href(from, path, Some(fragment));
        assert_eq!(href, "../new/re%3A%20%25%20%23.xhtml#ref:%20%C2%A7%23%25");
        assert_eq!(
            resolve_href(from, &href),
            Some((path.to_owned(), Some(fragment.to_owned())))
        );
    }

    #[test]
    fn resolves_queries_and_same_document_anchors() {
        let from = "EPUB/text/one.xhtml";
        assert_eq!(relative_href(from, from, Some("page/3")), "#page/3");
        assert_eq!(
            resolve_href(from, "?ignored=1#p%3F3"),
            Some((from.to_owned(), Some("p?3".to_owned())))
        );
        assert_eq!(
            resolve_href(from, "/EPUB/./text/../two.xhtml?ignored=1"),
            Some(("EPUB/two.xhtml".to_owned(), None))
        );
    }

    #[test]
    fn skips_external_links_and_escaping_the_archive() {
        let from = "EPUB/text/one.xhtml";
        for href in [
            "https://example.org/#here",
            "//example.org/two.xhtml",
            "mailto:example@example.org",
            "../../../two.xhtml",
            "../%2Ftwo.xhtml",
        ] {
            assert_eq!(resolve_href(from, href), None, "{href}");
        }
    }
}
