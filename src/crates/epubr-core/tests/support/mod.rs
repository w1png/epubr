use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use percent_encoding::percent_decode_str;
use roxmltree::{Document, Node, ParsingOptions};
use zip::{CompressionMethod, ZipArchive};

const CONTAINER: &str = "urn:oasis:names:tc:opendocument:xmlns:container";
const OPF: &str = "http://www.idpf.org/2007/opf";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const MATHML: &str = "http://www.w3.org/1998/Math/MathML";
pub const XHTML: &str = "http://www.w3.org/1999/xhtml";
pub const EPUB: &str = "http://www.idpf.org/2007/ops";

pub struct Epub {
    pub title: String,
    pub language: String,
    pub author: Option<String>,
    pub spine: Vec<Content>,
    pub navigation: Content,
    pub manifest: Vec<Resource>,
    files: HashMap<String, Vec<u8>>,
}

pub struct Content {
    pub path: String,
    pub xml: String,
}

pub struct Resource {
    pub path: String,
    pub media_type: String,
    pub properties: String,
}

impl Epub {
    pub fn open(path: &Path) -> Self {
        let files = read_archive(path);
        let container = parse_xml(xml_file(&files, "META-INF/container.xml"), "container.xml");
        let root = container.root_element();
        assert!(root.has_tag_name((CONTAINER, "container")));
        let rootfiles = child(root, CONTAINER, "rootfiles");
        let rootfile = child(rootfiles, CONTAINER, "rootfile");
        assert_eq!(
            rootfile.attribute("media-type"),
            Some("application/oebps-package+xml")
        );
        let package_path = attribute(rootfile, "full-path");
        let package = parse_xml(xml_file(&files, package_path), package_path);
        let root = package.root_element();
        assert!(root.has_tag_name((OPF, "package")));
        assert_eq!(root.attribute("version"), Some("3.0"));

        let metadata = child(root, OPF, "metadata");
        let title = element_text(child(metadata, DC, "title"));
        let language = element_text(child(metadata, DC, "language"));
        let author = metadata
            .children()
            .find(|node| node.has_tag_name((DC, "creator")))
            .map(element_text);
        validate_required_metadata(root, metadata);
        assert!(!title.is_empty(), "EPUB title is empty");
        assert!(!language.is_empty(), "EPUB language is empty");

        let manifest = child(root, OPF, "manifest");
        let (manifest, manifest_ids) = read_manifest(manifest, package_path, &files);
        let mut nav_items = manifest
            .iter()
            .filter(|item| item.properties.split_whitespace().any(|p| p == "nav"));
        let nav_item = nav_items.next().expect("missing EPUB navigation resource");
        assert!(nav_items.next().is_none(), "multiple navigation resources");
        let navigation = content(&files, nav_item);
        validate_navigation(&navigation, &manifest, &files);

        let spine = child(root, OPF, "spine")
            .children()
            .filter(|node| node.has_tag_name((OPF, "itemref")))
            .map(|node| {
                assert!(
                    !has_property(
                        node.attribute("properties"),
                        "rendition:layout-pre-paginated"
                    ),
                    "spine item requests fixed layout"
                );
                let idref = attribute(node, "idref");
                let index = manifest_ids
                    .get(idref)
                    .unwrap_or_else(|| panic!("spine references undeclared item {idref:?}"));
                content(&files, &manifest[*index])
            })
            .collect::<Vec<_>>();
        assert!(!spine.is_empty(), "EPUB spine is empty");

        Self {
            title,
            language,
            author,
            spine,
            navigation,
            manifest,
            files,
        }
    }

    pub fn resource(&self, path: &str) -> &[u8] {
        file(&self.files, path)
    }

    pub fn spine_text(&self) -> String {
        self.spine
            .iter()
            .map(|content| {
                let xml = parse_xml(&content.xml, &content.path);
                element_text(child(xml.root_element(), XHTML, "body"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn read_archive(path: &Path) -> HashMap<String, Vec<u8>> {
    let source =
        File::open(path).unwrap_or_else(|error| panic!("opening {}: {error}", path.display()));
    let mut zip = ZipArchive::new(source)
        .unwrap_or_else(|error| panic!("reading {} as EPUB ZIP: {error}", path.display()));
    let mut files = HashMap::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).expect("reading EPUB ZIP entry");
        let name = entry
            .name()
            .expect("reading EPUB ZIP entry name")
            .into_owned();
        if index == 0 {
            assert_eq!(name, "mimetype", "first EPUB ZIP entry");
            assert_eq!(entry.header_start(), 0, "first EPUB ZIP local header");
            assert_eq!(entry.compression(), CompressionMethod::Stored);
            assert_eq!(
                entry.data_start(),
                Some(38),
                "mimetype ZIP header has extra data"
            );
        }
        assert!(
            matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            ),
            "unsupported ZIP compression for {name:?}"
        );
        assert!(!entry.encrypted(), "encrypted EPUB ZIP entry {name:?}");
        assert!(entry.enclosed_name().is_some(), "invalid ZIP path {name:?}");
        if entry.is_dir() {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .unwrap_or_else(|error| panic!("reading {name:?}: {error}"));
        assert!(
            files.insert(name.clone(), bytes).is_none(),
            "duplicate EPUB ZIP entry {name:?}"
        );
    }
    assert_eq!(file(&files, "mimetype"), b"application/epub+zip");
    files
}

fn validate_required_metadata(package: Node<'_, '_>, metadata: Node<'_, '_>) {
    for layout in metadata.children().filter(|node| {
        node.has_tag_name((OPF, "meta")) && node.attribute("property") == Some("rendition:layout")
    }) {
        assert_eq!(element_text(layout), "reflowable", "unexpected EPUB layout");
    }
    let id = attribute(package, "unique-identifier");
    assert!(
        metadata.children().any(|node| {
            node.has_tag_name((DC, "identifier"))
                && node.attribute("id") == Some(id)
                && !element_text(node).is_empty()
        }),
        "unique-identifier must reference a nonempty dc:identifier"
    );
    let modified = metadata
        .children()
        .filter(|node| {
            node.has_tag_name((OPF, "meta"))
                && node.attribute("property") == Some("dcterms:modified")
        })
        .collect::<Vec<_>>();
    assert_eq!(modified.len(), 1, "expected one dcterms:modified value");
    assert!(
        element_text(modified[0]).ends_with('Z'),
        "dcterms:modified must specify a UTC timestamp"
    );
}

fn read_manifest<'a, 'input>(
    manifest: Node<'a, 'input>,
    package_path: &str,
    files: &HashMap<String, Vec<u8>>,
) -> (Vec<Resource>, HashMap<&'a str, usize>) {
    let mut resources = Vec::new();
    let mut ids = HashMap::new();
    let mut paths = HashSet::new();
    for item in manifest
        .children()
        .filter(|node| node.has_tag_name((OPF, "item")))
    {
        let id = attribute(item, "id");
        let href = attribute(item, "href");
        let (path, fragment) = resolve_href(package_path, href);
        assert!(fragment.is_none(), "manifest href has a fragment {href:?}");
        assert!(
            ids.insert(id, resources.len()).is_none(),
            "duplicate manifest id {id:?}"
        );
        assert!(
            paths.insert(path.clone()),
            "duplicate manifest path {path:?}"
        );
        file(files, &path);
        resources.push(Resource {
            path,
            media_type: attribute(item, "media-type").to_owned(),
            properties: item.attribute("properties").unwrap_or_default().to_owned(),
        });
    }
    (resources, ids)
}

fn validate_navigation(
    navigation: &Content,
    manifest: &[Resource],
    files: &HashMap<String, Vec<u8>>,
) {
    let xml = parse_xml(&navigation.xml, &navigation.path);
    let mut toc = xml.descendants().filter(|node| {
        node.has_tag_name((XHTML, "nav"))
            && node
                .attribute((EPUB, "type"))
                .is_some_and(|types| types.split_whitespace().any(|kind| kind == "toc"))
    });
    let list = child(
        toc.next().expect("missing nav with epub:type toc"),
        XHTML,
        "ol",
    );
    assert!(toc.next().is_none(), "multiple nav elements of type toc");
    let mut link_count = 0;
    for link in list
        .descendants()
        .filter(|node| node.has_tag_name((XHTML, "a")))
    {
        link_count += 1;
        let href = attribute(link, "href");
        let (path, fragment) = resolve_href(&navigation.path, href);
        let item = manifest
            .iter()
            .find(|item| item.path == path)
            .unwrap_or_else(|| panic!("navigation points to undeclared resource {path:?}"));
        if let Some(id) = fragment {
            let target = content(files, item);
            let xml = parse_xml(&target.xml, &path);
            assert!(
                xml.descendants()
                    .any(|node| node.attribute("id") == Some(&id)),
                "missing navigation target {href:?}"
            );
        }
    }
    assert!(link_count > 0, "EPUB table of contents has no links");
}

fn content(files: &HashMap<String, Vec<u8>>, item: &Resource) -> Content {
    assert_eq!(item.media_type, "application/xhtml+xml", "{}", item.path);
    let xml = xml_file(files, &item.path);
    let document = parse_xml(xml, &item.path);
    assert!(
        document.root_element().has_tag_name((XHTML, "html")),
        "{} is not XHTML",
        item.path
    );
    if document
        .descendants()
        .any(|node| node.has_tag_name((MATHML, "math")))
    {
        assert!(
            has_property(Some(&item.properties), "mathml"),
            "{} contains MathML but lacks the mathml manifest property",
            item.path
        );
    }
    Content {
        path: item.path.clone(),
        xml: xml.to_owned(),
    }
}

fn has_property(properties: Option<&str>, expected: &str) -> bool {
    properties.is_some_and(|properties| properties.split_whitespace().any(|p| p == expected))
}

pub fn resolve_href(base_path: &str, href: &str) -> (String, Option<String>) {
    let (target, fragment) = href
        .split_once('#')
        .map_or((href, None), |(path, id)| (path, Some(decode(id))));
    let target = target.split_once('?').map_or(target, |(path, _)| path);
    if target.is_empty() {
        return (base_path.to_owned(), fragment);
    }
    assert!(
        !target.starts_with('/') && !target.split('/').next().unwrap_or_default().contains(':'),
        "expected a relative EPUB resource href, got {href:?}"
    );
    let mut parts = base_path.rsplit_once('/').map_or(Vec::new(), |(dir, _)| {
        dir.split('/').map(str::to_owned).collect()
    });
    for part in target.split('/') {
        let part = decode(part);
        match part.as_str() {
            "." => {}
            ".." => assert!(parts.pop().is_some(), "EPUB href escapes archive {href:?}"),
            _ => parts.push(part),
        }
    }
    (parts.join("/"), fragment)
}

pub fn parse_xml<'input>(xml: &'input str, path: &str) -> Document<'input> {
    let options = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    Document::parse_with_options(xml, options)
        .unwrap_or_else(|error| panic!("invalid XML in {path:?}: {error}"))
}

fn decode(value: &str) -> String {
    percent_decode_str(value)
        .decode_utf8()
        .unwrap_or_else(|error| panic!("invalid UTF-8 in URL value {value:?}: {error}"))
        .into_owned()
}

fn file<'a>(files: &'a HashMap<String, Vec<u8>>, path: &str) -> &'a [u8] {
    files
        .get(path)
        .unwrap_or_else(|| panic!("missing EPUB resource {path:?}"))
}

fn xml_file<'a>(files: &'a HashMap<String, Vec<u8>>, path: &str) -> &'a str {
    std::str::from_utf8(file(files, path))
        .unwrap_or_else(|error| panic!("{path:?} is not UTF-8: {error}"))
}

fn child<'a, 'input>(node: Node<'a, 'input>, namespace: &str, name: &str) -> Node<'a, 'input> {
    node.children()
        .find(|child| child.has_tag_name((namespace, name)))
        .unwrap_or_else(|| panic!("missing {name:?} in {:?}", node.tag_name().name()))
}

fn attribute<'a>(node: Node<'a, '_>, name: &str) -> &'a str {
    node.attribute(name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("missing {name:?} attribute on {:?}", node.tag_name().name()))
}

pub fn element_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|node| node.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
