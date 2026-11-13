use std::fs::File;
use std::io::Read;

use roxmltree::{Document, Node};
use serde_json::{Value, json};
use zip::ZipArchive;

use super::write;
use crate::OptimizationTarget;
use crate::book::{Block, Book, ListItem, NavEntry};

fn write_book(book: &Book, optimization: Option<OptimizationTarget>) -> ZipArchive<File> {
    let mut output = tempfile::tempfile().unwrap();
    write(&mut output, book, optimization).unwrap();
    ZipArchive::new(output).unwrap()
}

fn read_package_file(archive: &mut ZipArchive<File>, path: &str) -> String {
    let mut xml = String::new();
    archive
        .by_name(&format!("EPUB/{path}"))
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

fn anchor<'a, 'input>(doc: &'a Document<'input>, id: &str) -> Node<'a, 'input> {
    doc.descendants()
        .find(|node| node.attribute("id") == Some(id))
        .unwrap_or_else(|| panic!("missing anchor {id}"))
}

fn link<'a, 'input>(doc: &'a Document<'input>, text: &str) -> Node<'a, 'input> {
    doc.descendants()
        .find(|node| node.has_tag_name("a") && node.text() == Some(text))
        .unwrap_or_else(|| panic!("missing link {text}"))
}

fn spine_items<'a, 'input>(package: &'a Document<'input>) -> Vec<Node<'a, 'input>> {
    let manifest_item = |id| {
        package
            .descendants()
            .find(|node| node.has_tag_name("item") && node.attribute("id") == Some(id))
            .unwrap()
    };
    package
        .descendants()
        .filter(|node| node.has_tag_name("itemref"))
        .map(|node| manifest_item(node.attribute("idref").unwrap()))
        .collect()
}

#[test]
fn chapter_documents_keep_navigation_and_cross_chapter_notes() {
    let heading = |level, id: &str, text: &str| Block::Heading {
        level,
        id: id.into(),
        text: text.into(),
    };
    let entry = |level, id: &str, text: &str| NavEntry {
        level,
        id: id.into(),
        text: text.into(),
    };
    let reference = |number| {
        format!(
            r##"<a id="noteref-{number}" href="#note-{number}" epub:type="noteref">{number}</a>"##
        )
    };
    let note = |number| Block::Note {
        id: format!("note-{number}"),
        html: format!(r##"Note {number} <a href="#noteref-{number}">↩</a>"##),
    };
    let book = Book {
        title: "Sample book".into(),
        cover: None,
        authors: Vec::new(),
        language: "en".into(),
        year: None,
        edition: None,
        publisher: None,
        description: None,
        blocks: vec![
            Block::Contents {
                title: "Contents".into(),
            },
            heading(1, "heading-1", "Preface"),
            heading(2, "heading-2", "Background"),
            Block::Paragraph {
                html: format!("First reference{}", reference(1)),
            },
            Block::List {
                ordered: false,
                items: vec![ListItem {
                    html: format!("Second reference{}", reference(2)),
                    has_mathml: false,
                    number: None,
                }],
            },
            Block::PageAnchor { page: 19 },
            Block::Formula {
                mathml: r#"<math xmlns="http://www.w3.org/1998/Math/MathML"><mi>x</mi></math>"#
                    .into(),
            },
            note(1),
            note(2),
        ],
        navigation: vec![
            entry(1, "heading-1", "Preface"),
            entry(2, "heading-2", "Background"),
            entry(1, "source-page-19", "Chapter 1"),
        ],
        contents_navigation: Some(vec![
            entry(1, "heading-1", "Preface"),
            entry(1, "source-page-19", "Chapter One"),
        ]),
        warnings: Vec::new(),
    };
    let mut archive = write_book(&book, None);

    let package = read_package_file(&mut archive, "book.opf");
    let package = Document::parse(&package).unwrap();
    let spine = spine_items(&package);
    let [front, preface, chapter] = spine.as_slice() else {
        panic!("expected front matter and two chapter documents");
    };
    assert_eq!(front.attribute("properties"), None);
    assert_eq!(preface.attribute("properties"), None);
    assert_eq!(chapter.attribute("properties"), Some("mathml"));

    let front_path = front.attribute("href").unwrap();
    let preface_path = preface.attribute("href").unwrap();
    let chapter_path = chapter.attribute("href").unwrap();
    let preface = read_package_file(&mut archive, preface_path);
    let preface = Document::parse(&preface).unwrap();
    let chapter = read_package_file(&mut archive, chapter_path);
    let chapter = Document::parse(&chapter).unwrap();
    assert_eq!(anchor(&preface, "heading-1").text(), Some("Preface"));
    assert_eq!(anchor(&preface, "heading-2").text(), Some("Background"));
    assert!(anchor(&chapter, "source-page-19").has_tag_name("span"));
    assert!(chapter.descendants().any(|node| node.has_tag_name("math")));

    for (number, container) in [(1, "p"), (2, "li")] {
        let noteref_id = format!("noteref-{number}");
        let note_id = format!("note-{number}");
        let noteref = anchor(&preface, &noteref_id);
        assert!(noteref.parent().unwrap().has_tag_name(container));
        assert_eq!(
            noteref.attribute("href").unwrap(),
            format!("{chapter_path}#{note_id}")
        );
        let backlink = anchor(&chapter, &note_id)
            .descendants()
            .find(|node| node.has_tag_name("a"))
            .unwrap();
        assert_eq!(
            backlink.attribute("href").unwrap(),
            format!("{preface_path}#{noteref_id}")
        );
    }

    let nav = read_package_file(&mut archive, "nav.xhtml");
    let front = read_package_file(&mut archive, front_path);
    let nav = Document::parse(&nav).unwrap();
    let front = Document::parse(&front).unwrap();
    let background = link(&nav, "Background");
    assert!(
        background
            .ancestors()
            .any(|node| node == link(&nav, "Preface").parent().unwrap())
    );
    for (link, target) in [
        (link(&nav, "Preface"), format!("{preface_path}#heading-1")),
        (background, format!("{preface_path}#heading-2")),
        (
            link(&nav, "Chapter 1"),
            format!("{chapter_path}#source-page-19"),
        ),
        (link(&front, "Preface"), format!("{preface_path}#heading-1")),
        (
            link(&front, "Chapter One"),
            format!("{chapter_path}#source-page-19"),
        ),
    ] {
        assert_eq!(link.attribute("href"), Some(target.as_str()));
    }
}

#[test]
fn optimization_splits_long_chapter_and_keeps_its_locations_grouped() {
    let first = "first ".repeat(3500);
    let second = "second ".repeat(3000);
    let third = "third ".repeat(3500);
    let book = Book {
        title: "Sample book".into(),
        cover: None,
        authors: Vec::new(),
        language: "en".into(),
        year: None,
        edition: None,
        publisher: None,
        description: None,
        blocks: vec![
            Block::Heading {
                level: 1,
                id: "chapter".into(),
                text: "Chapter".into(),
            },
            Block::Paragraph {
                html: first.clone(),
            },
            Block::Paragraph {
                html: second.clone(),
            },
            Block::Heading {
                level: 2,
                id: "details".into(),
                text: "Details".into(),
            },
            Block::Paragraph {
                html: third.clone(),
            },
        ],
        navigation: Vec::new(),
        contents_navigation: None,
        warnings: Vec::new(),
    };
    let optimizer_path = "META-INF/crossink/optimizer-v1.json";
    let locations_path = "META-INF/x-locations.json";
    let mut default = write_book(&book, None);
    let package = read_package_file(&mut default, "book.opf");
    assert_eq!(spine_items(&Document::parse(&package).unwrap()).len(), 1);
    assert!(default.by_name(optimizer_path).is_err());
    assert!(default.by_name(locations_path).is_err());

    let mut optimized = write_book(&book, Some(OptimizationTarget::XteinkX4));
    let optimizer: Value =
        serde_json::from_reader(optimized.by_name(optimizer_path).unwrap()).unwrap();
    assert_eq!(optimizer["target"]["device"], "x4");
    assert_eq!(optimizer["features"]["xLocations"], true);
    let locations: Value =
        serde_json::from_reader(optimized.by_name(locations_path).unwrap()).unwrap();
    assert_eq!(
        locations["chapterGroups"],
        json!([{ "index": 0, "firstSpineIndex": 0, "lastSpineIndex": 2 }])
    );
    let package = read_package_file(&mut optimized, "book.opf");
    let package = Document::parse(&package).unwrap();
    let spine = spine_items(&package);
    let location_spine = locations["spine"].as_array().unwrap();
    assert_eq!(spine.len(), 3);
    assert_eq!(location_spine.len(), 3);
    for (index, (location, text)) in location_spine
        .iter()
        .zip([first, second, third])
        .enumerate()
    {
        let path = spine[index].attribute("href").unwrap();
        let content = read_package_file(&mut optimized, path);
        assert!(content.contains(&text));
        assert_eq!(location["href"], format!("EPUB/{path}"));
        assert_eq!(location["chapterGroup"], 0);
    }
    for adjacent in location_spine.windows(2) {
        assert_eq!(
            adjacent[1]["startLocation"].as_u64().unwrap(),
            adjacent[0]["endLocation"].as_u64().unwrap() + 1
        );
    }
    let nav = read_package_file(&mut optimized, "nav.xhtml");
    let nav = Document::parse(&nav).unwrap();
    let path = spine[1].attribute("href").unwrap();
    assert_eq!(
        link(&nav, "Details").attribute("href").unwrap(),
        format!("{path}#details")
    );
    let continuation = read_package_file(&mut optimized, path);
    assert_eq!(
        anchor(&Document::parse(&continuation).unwrap(), "details").text(),
        Some("Details")
    );
}
