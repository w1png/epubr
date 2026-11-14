mod support;

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use epubr_core::{OptimizationTarget, OptimizeError, optimize_epub};
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgb, RgbImage};
use serde_json::{Value, json};
use support::{Content, Epub, parse_xml, resolve_href};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const TARGET: OptimizationTarget = OptimizationTarget::XteinkX4Pro;
const OPTIMIZER: &str = "META-INF/crossink/optimizer-v1.json";
const LOCATIONS: &str = "META-INF/x-locations.json";
const BACKGROUND: &str = "Library/Book/Images/background.png";
const CSS_PATH: &str = "Library/Book/Styles/reader.css";
const CSS: &str = ".banner { background-image: url('../Images/background.png'); }";

#[test]
fn optimizes_an_existing_epub_in_spine_order_and_keeps_links_resolvable() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("source.epub");
    let output = directory.path().join("optimized.epub");
    write_source_epub(&input);
    let source = fs::read(&input).unwrap();

    optimize_epub(&input, &output, TARGET).unwrap();

    assert_eq!(fs::read(&input).unwrap(), source);
    let epub = Epub::open(&output);
    assert_eq!(epub.title, "Example Book");
    assert_eq!(epub.language, "en");
    assert_eq!(epub.author.as_deref(), Some("Example Author"));
    let [first, second, third, notes] = epub.spine.as_slice() else {
        panic!("expected three parts of the first chapter and the notes document");
    };
    for (part, marker) in [
        (first, "alpha"),
        (second, "beta"),
        (third, "gamma"),
        (notes, "A note"),
    ] {
        assert!(
            part.xml.contains(marker),
            "missing {marker} from {}",
            part.path
        );
    }
    let text = epub.spine_text();
    for word in ["alpha", "beta", "gamma"] {
        assert_eq!(
            text.split_whitespace()
                .filter(|candidate| *candidate == word)
                .count(),
            4200
        );
    }

    let has_id = |part: &Content, id: &str| {
        parse_xml(&part.xml, &part.path)
            .descendants()
            .any(|node| node.attribute("id") == Some(id))
    };
    let details = [second, third]
        .into_iter()
        .find(|part| has_id(part, "details:α"))
        .expect("subheading should be in a continuation document");
    let details_target = (details.path.clone(), Some("details:α".into()));
    let nav = parse_xml(&epub.navigation.xml, &epub.navigation.path);
    let nav_destinations = nav
        .descendants()
        .filter(|node| node.has_tag_name("a"))
        .map(|node| resolve_href(&epub.navigation.path, node.attribute("href").unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(
        nav_destinations,
        [
            (first.path.clone(), Some("chapter".into())),
            details_target.clone(),
            (notes.path.clone(), Some("notes".into())),
        ]
    );
    assert_eq!(link_destination(first, "Skip to details"), details_target);
    let ncx_path = "Library/Book/Nav/toc.ncx";
    let ncx_xml = std::str::from_utf8(epub.resource(ncx_path)).unwrap();
    let ncx = parse_xml(ncx_xml, ncx_path);
    let ncx_details = ncx
        .descendants()
        .find(|node| node.attribute("id") == Some("ncx-details"))
        .unwrap()
        .children()
        .find(|node| node.has_tag_name("content"))
        .unwrap();
    assert_eq!(
        resolve_href(ncx_path, ncx_details.attribute("src").unwrap()),
        details_target
    );
    let reference = [second, third]
        .into_iter()
        .find(|part| has_id(part, "ref-1"))
        .expect("note reference should be in a continuation document");
    assert!(has_id(notes, "note-1"));
    assert_eq!(
        link_destination(reference, "1"),
        (notes.path.clone(), Some("note-1".into()))
    );
    assert_eq!(
        link_destination(notes, "↩"),
        (reference.path.clone(), Some("ref-1".into()))
    );

    let first_doc = parse_xml(&first.xml, &first.path);
    let image_href = first_doc
        .descendants()
        .find(|node| node.has_tag_name("img"))
        .unwrap()
        .attribute("src")
        .unwrap();
    let (image_path, _) = resolve_href(&first.path, image_href);
    let image_item = epub
        .manifest
        .iter()
        .find(|item| item.path == image_path)
        .unwrap();
    assert_eq!(image_item.media_type, "image/jpeg");
    let image = image::load_from_memory(epub.resource(&image_path)).unwrap();
    assert_eq!((image.width(), image.height()), (800, 160));
    assert_eq!(image.color(), ColorType::L8);
    assert_eq!(epub.resource(CSS_PATH), CSS.as_bytes());
    assert_eq!(epub.resource(BACKGROUND), raster_png());
    assert!(
        epub.manifest
            .iter()
            .any(|item| { item.path == BACKGROUND && item.media_type == "image/png" })
    );

    let optimizer: Value = serde_json::from_slice(epub.resource(OPTIMIZER)).unwrap();
    assert_eq!(optimizer["target"]["device"], "x4-pro");
    assert_eq!(optimizer["features"]["xLocations"], true);
    let locations: Value = serde_json::from_slice(epub.resource(LOCATIONS)).unwrap();
    assert_eq!(
        locations["chapterGroups"],
        json!([{ "index": 0, "firstSpineIndex": 0, "lastSpineIndex": 2 }])
    );
    let location_spine = locations["spine"].as_array().unwrap();
    assert_eq!(location_spine.len(), epub.spine.len());
    for (index, (entry, part)) in location_spine.iter().zip(&epub.spine).enumerate() {
        assert_eq!(entry["href"], part.path);
        assert_eq!(entry["index"], index);
    }
}

#[test]
fn refuses_to_replace_the_source_or_an_existing_destination() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("source.epub");
    let output = directory.path().join("occupied.epub");
    write_source_epub(&input);
    let source = fs::read(&input).unwrap();
    fs::write(&output, b"user data").unwrap();

    for destination in [&input, &output] {
        assert!(matches!(
            optimize_epub(&input, destination, TARGET),
            Err(OptimizeError::DestinationExists)
        ));
    }

    assert_eq!(fs::read(&input).unwrap(), source);
    assert_eq!(fs::read(&output).unwrap(), b"user data");
}

#[test]
fn rejects_invalid_epub_without_publishing_output() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("broken.epub");
    let output = directory.path().join("optimized.epub");
    fs::write(&input, b"invalid zip data").unwrap();

    assert!(optimize_epub(&input, &output, TARGET).is_err());
    assert!(!output.exists());
}

fn link_destination(document: &Content, label: &str) -> (String, Option<String>) {
    let xml = parse_xml(&document.xml, &document.path);
    let link = xml
        .descendants()
        .find(|node| node.has_tag_name("a") && node.text() == Some(label))
        .unwrap_or_else(|| panic!("missing link {label:?} in {}", document.path));
    resolve_href(&document.path, link.attribute("href").unwrap())
}

fn write_source_epub(path: &Path) {
    let container = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
        <rootfiles><rootfile full-path="Library/Book/package.opf" media-type="application/oebps-package+xml"/></rootfiles>
    </container>"#;
    let package = r#"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="pub-id">
        <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:identifier id="pub-id">urn:uuid:f98bc3a8-0e5d-4ec6-a734-785c7c50a415</dc:identifier>
            <dc:title>Example Book</dc:title><dc:language>en</dc:language><dc:creator>Example Author</dc:creator>
            <meta property="dcterms:modified">2025-01-01T00:00:00Z</meta>
        </metadata>
        <manifest>
            <item id="toc" href="Nav/toc.xhtml" media-type="application/xhtml+xml" properties="nav"/>
            <item id="ncx" href="Nav/toc.ncx" media-type="application/x-dtbncx+xml"/>
            <item id="notes" href="Reference/notes.xhtml" media-type="application/xhtml+xml"/>
            <item id="chapter" href="Chapters/first.xhtml" media-type="application/xhtml+xml"/>
            <item id="picture" href="Images/diagram.png" media-type="image/png"/>
            <item id="background" href="Images/background.png" media-type="image/png"/>
            <item id="style" href="Styles/reader.css" media-type="text/css"/>
        </manifest>
        <spine toc="ncx"><itemref idref="chapter"/><itemref idref="notes"/></spine>
    </package>"#;
    let nav = xhtml(
        "Contents",
        r#"<nav epub:type="toc"><ol>
            <li><a href="../Chapters/first.xhtml#chapter">Chapter</a>
                <ol><li><a href="../Chapters/first.xhtml#details%3A%CE%B1">Details</a></li></ol>
            </li>
            <li><a href="../Reference/notes.xhtml#notes">Notes</a></li>
        </ol></nav>"#,
    );
    let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
        <head><meta name="dtb:uid" content="urn:uuid:f98bc3a8-0e5d-4ec6-a734-785c7c50a415"/></head>
        <docTitle><text>Example Book</text></docTitle><navMap>
        <navPoint id="ncx-chapter" playOrder="1"><navLabel><text>Chapter</text></navLabel>
            <content src="../Chapters/first.xhtml#chapter"/>
            <navPoint id="ncx-details" playOrder="2"><navLabel><text>Details</text></navLabel>
                <content src="../Chapters/first.xhtml#details%3A%CE%B1"/>
            </navPoint>
        </navPoint>
        <navPoint id="ncx-notes" playOrder="3"><navLabel><text>Notes</text></navLabel>
            <content src="../Reference/notes.xhtml#notes"/>
        </navPoint></navMap></ncx>"#;
    let chapter = xhtml(
        "Chapter",
        &format!(
            r##"<h1 id="chapter" class="banner">Chapter</h1><img src="../Images/diagram.png" alt="Diagram"/>
            <p><a href="#details%3A%CE%B1">Skip to details</a></p>
            <p> {}</p><p> {}</p><h2 id="details:α">Details</h2>
            <p>See note <a epub:type="noteref" id="ref-1" href="../Reference/notes.xhtml#note-1">1</a>.</p>
            <p> {}</p>"##,
            "alpha ".repeat(4200),
            "beta ".repeat(4200),
            "gamma ".repeat(4200),
        ),
    );
    let notes = xhtml(
        "Notes",
        r#"<h1 id="notes">Notes</h1><aside epub:type="footnote" id="note-1"><p>A note <a href="../Chapters/first.xhtml#ref-1">↩</a></p></aside>"#,
    );
    let chapter = chapter.replace(
        "</head>",
        r#"<link rel="stylesheet" href="../Styles/reader.css"/></head>"#,
    );
    let png = raster_png();

    let mut archive = ZipWriter::new(File::create(path).unwrap());
    for (path, bytes) in [
        ("mimetype", b"application/epub+zip".as_slice()),
        ("META-INF/container.xml", container.as_bytes()),
        ("Library/Book/package.opf", package.as_bytes()),
        ("Library/Book/Nav/toc.xhtml", nav.as_bytes()),
        ("Library/Book/Nav/toc.ncx", ncx.as_bytes()),
        ("Library/Book/Chapters/first.xhtml", chapter.as_bytes()),
        ("Library/Book/Reference/notes.xhtml", notes.as_bytes()),
        ("Library/Book/Images/diagram.png", &png),
        (BACKGROUND, &png),
        (CSS_PATH, CSS.as_bytes()),
    ] {
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        archive.start_file(path, options).unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive.finish().unwrap();
}

fn raster_png() -> Vec<u8> {
    let image = RgbImage::from_pixel(900, 180, Rgb([32, 120, 220]));
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ColorType::Rgb8.into(),
        )
        .unwrap();
    png
}

fn xhtml(title: &str, body: &str) -> String {
    format!(
        r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="en" xml:lang="en">
        <head><title>{title}</title></head><body>{body}</body></html>"#
    )
}
