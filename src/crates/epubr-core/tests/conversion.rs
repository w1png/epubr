mod support;

use std::path::Path;
use std::sync::Mutex;

use epubr_core::{CancellationToken, ConversionOptions, OcrMode, Progress, convert};
use roxmltree::Node;
use support::{EPUB, Epub, element_text, parse_xml, resolve_href};

const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";

fn convert_fixture(
    fixture: &str,
    options: ConversionOptions,
    on_progress: impl Fn(Progress) + Sync,
) -> Epub {
    let input = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    assert!(input.is_file(), "missing fixture {}", input.display());
    let destination = tempfile::tempdir().unwrap();
    let output = destination.path().join("converted book.epub");
    convert(
        &input,
        &output,
        options,
        &CancellationToken::default(),
        on_progress,
    )
    .unwrap_or_else(|error| panic!("conversion of {fixture} should succeed: {error}"));
    Epub::open(&output)
}

fn text_mode() -> ConversionOptions {
    ConversionOptions {
        mode: OcrMode::Text,
        ..ConversionOptions::default()
    }
}

#[test]
fn creates_a_reflowable_epub_with_metadata_and_source_reading_order() {
    let epub = convert_fixture("basic.pdf", ConversionOptions::default(), |_| {});

    assert_eq!(epub.title, "Technical Notes");
    assert_eq!(epub.author.as_deref(), Some("Fixture Author"));
    assert_eq!(epub.language, "en");
    let text = epub.spine_text();
    let opening = text.find("First page opening.").expect("opening text");
    let closing = text.find("Second page closing.").expect("closing text");
    assert!(opening < closing, "EPUB spine reversed the source order");
}

#[test]
fn preserves_unicode_and_joins_only_the_wrapped_word() {
    let epub = convert_fixture("basic.pdf", text_mode(), |_| {});
    let text = epub.spine_text();

    assert!(text.contains("The café costs €5."));
    assert!(text.contains("The symbols × and · stay intact."));
    assert!(text.contains("Read the engineering notes on private-key encryption."));
    assert!(!text.contains('\u{fffd}'), "replacement character in EPUB");
}

#[test]
fn chapter_navigation_nests_subsections_and_links_to_readable_content() {
    let epub = convert_fixture("basic.pdf", text_mode(), |_| {});
    let navigation = parse_xml(&epub.navigation.xml, &epub.navigation.path);
    let toc = navigation
        .descendants()
        .find(|node| {
            node.has_tag_name("nav")
                && node
                    .attribute((EPUB, "type"))
                    .is_some_and(|kind| kind.split_whitespace().any(|word| word == "toc"))
        })
        .expect("EPUB table of contents");
    let link = |title: &str| {
        toc.descendants()
            .find(|node| node.has_tag_name("a") && element_text(*node).trim() == title)
            .unwrap_or_else(|| panic!("navigation is missing {title}"))
    };
    let first = link("1 First Chapter");
    let subsection = link("1.1 A Subsection");
    let second = link("2 Second Chapter");
    let first_item = first
        .ancestors()
        .find(|node| node.has_tag_name("li"))
        .unwrap();
    assert!(subsection.ancestors().any(|node| node == first_item));
    assert!(!second.ancestors().any(|node| node == first_item));

    for anchor in [first, subsection, second] {
        let href = anchor.attribute("href").expect("navigation link target");
        let (path, fragment) = resolve_href(&epub.navigation.path, href);
        let chapter = epub
            .spine
            .iter()
            .find(|item| item.path == path)
            .expect("navigation target is in spine");
        let document = parse_xml(&chapter.xml, &chapter.path);
        if let Some(fragment) = fragment {
            assert!(
                document
                    .descendants()
                    .any(|node| node.attribute("id") == Some(fragment.as_str())),
                "missing navigation fragment {fragment}"
            );
        }
        assert!(element_text(document.root_element()).contains(element_text(anchor).trim()));
    }
}

#[test]
fn figure_caption_and_embedded_image_are_readable_from_the_spine() {
    let epub = convert_fixture("basic.pdf", text_mode(), |_| {});

    let has_captioned_image = epub.spine.iter().any(|content| {
        let document = parse_xml(&content.xml, &content.path);
        if !element_text(document.root_element()).contains("Figure 1. A test diagram.") {
            return false;
        }
        document
            .descendants()
            .filter_map(|node| {
                node.has_tag_name("img")
                    .then(|| node.attribute("src"))
                    .flatten()
            })
            .any(|src| {
                let (path, _) = resolve_href(&content.path, src);
                epub.manifest.iter().any(|resource| {
                    resource.path == path && resource.media_type.starts_with("image/")
                }) && !epub.resource(&path).is_empty()
            })
    });
    assert!(
        has_captioned_image,
        "figure caption and packaged image should appear in the same spine document"
    );
}

#[test]
fn notes_link_to_the_correct_text_and_back_to_the_reference() {
    let epub = convert_fixture("notes.pdf", text_mode(), |_| {});
    let note_text = "This source note belongs to the claim.";
    let mut checked = false;

    for content in &epub.spine {
        let document = parse_xml(&content.xml, &content.path);
        for reference in document.descendants().filter(|node| {
            node.has_tag_name("a")
                && element_text(*node).trim().trim_matches(['[', ']']) == "1"
                && node.ancestors().any(|parent| {
                    parent.has_tag_name("p")
                        && element_text(parent).contains("The claim has a source")
                })
        }) {
            let (note_path, note_id) =
                resolve_href(&content.path, reference.attribute("href").unwrap());
            let note_id = note_id.expect("reference links to a specific note");
            let note_source = std::str::from_utf8(epub.resource(&note_path)).unwrap();
            let note_doc = parse_xml(note_source, &note_path);
            let note = note_doc
                .descendants()
                .find(|node| node.attribute("id") == Some(note_id.as_str()))
                .expect("linked note anchor exists");
            let note = note
                .ancestors()
                .find(|node| element_text(*node).contains(note_text))
                .expect("linked note text exists");
            assert!(
                !element_text(note).contains("Unrelated body text."),
                "note contains unrelated body prose"
            );
            let has_return = note
                .descendants()
                .filter(|node| node.has_tag_name("a"))
                .filter_map(|node| node.attribute("href"))
                .any(|href| {
                    let (path, fragment) = resolve_href(&note_path, href);
                    path == content.path
                        && fragment.as_deref().is_some_and(|fragment| {
                            reference
                                .ancestors()
                                .any(|node| node.attribute("id") == Some(fragment))
                        })
                });
            assert!(has_return, "note has no backlink to its reference");
            checked = true;
        }
    }
    assert!(checked, "no linked note reference found in the claim");
    assert_eq!(epub.spine_text().matches(note_text).count(), 1);
}

#[test]
fn mathml_preserves_the_negation_overbar_and_raw_expression() {
    let epub = convert_fixture("math.pdf", text_mode(), |_| {});

    let has_overbar = epub.spine.iter().any(|content| {
        let document = parse_xml(&content.xml, &content.path);
        document.descendants().any(|math| {
            math.has_tag_name((MATHML_NS, "math"))
                && math
                    .descendants()
                    .filter(|node| node.has_tag_name((MATHML_NS, "mo")) || node.has_tag_name((MATHML_NS, "mi")))
                    .map(element_text)
                    .collect::<String>()
                    .contains("+B=C")
                && math.descendants().any(|node| {
                    if !node.has_tag_name((MATHML_NS, "mover")) {
                        return false;
                    }
                    let parts: Vec<_> = node.children().filter(Node::is_element).collect();
                    matches!(parts.as_slice(), [base, accent]
                        if element_text(*base).trim() == "A"
                            && element_text(*accent).contains(['¯', '‾', '\u{0305}', 'ˉ', '\u{0304}', '―', '─']))
                })
        })
    });
    assert!(
        has_overbar,
        "expected Presentation MathML with an overbar above A"
    );
    let text = epub.spine_text();
    let raw_expression =
        text.contains(r"\overline") || text.contains(r"\bar") || text.contains("A\u{0305}");
    assert!(raw_expression, "EPUB lost the readable source expression");
}

#[test]
fn selected_page_range_excludes_the_other_pages() {
    let epub = convert_fixture(
        "basic.pdf",
        ConversionOptions {
            pages: Some((2, 2)),
            ..text_mode()
        },
        |_| {},
    );
    let text = epub.spine_text();

    assert!(text.contains("Second page closing."));
    assert!(!text.contains("First page opening."));
}

#[test]
fn forced_ocr_creates_searchable_text_in_the_epub() {
    let epub = convert_fixture(
        "ocr-image-only.pdf",
        ConversionOptions {
            mode: OcrMode::Scan,
            ..ConversionOptions::default()
        },
        |_| {},
    );
    let text = epub.spine_text();

    assert!(text.contains("SCAN FIXTURE"));
    assert!(text.contains("The quick brown fox jumps over the lazy dog."));
}

#[test]
fn reports_measured_progress_through_completion() {
    let progress = Mutex::new(Vec::new());
    convert_fixture("basic.pdf", text_mode(), |event| {
        progress.lock().unwrap().push(event)
    });
    let events = progress.into_inner().unwrap();

    assert_eq!(events.first(), Some(&Progress::Started));
    assert_eq!(events.last(), Some(&Progress::Finished));
    for page in [1, 2] {
        assert!(
            events.iter().any(|event| matches!(event, Progress::Page {
            completed: 1..=2, total: 2, page: actual_page, success: true,
        } if *actual_page == page)),
            "missing measured progress for source page {page}"
        );
    }
    assert!(events.iter().any(|event| matches!(
        event,
        Progress::Page {
            completed: 2,
            total: 2,
            success: true,
            ..
        }
    )));
}
