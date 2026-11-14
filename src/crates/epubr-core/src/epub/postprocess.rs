use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use roxmltree::{Document, Node, ParsingOptions};
use tempfile::NamedTempFile;
use thiserror::Error;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::OptimizationTarget;

use super::image_references::protected_images;
use super::locations::{self, Locations};
use super::optimizer::optimize_image;
use super::references::{relative_href, resolve_href};
use super::{OPTIMIZED_SECTION_BYTES, escape};

const XHTML: &str = "http://www.w3.org/1999/xhtml";
const OPF: &str = "http://www.idpf.org/2007/opf";
const EPUB: &str = "http://www.idpf.org/2007/ops";
const CONTAINER: &str = "META-INF/container.xml";
const MIME: &str = "application/epub+zip";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Debug, Error)]
pub enum OptimizeError {
    #[error("destination already exists")]
    DestinationExists,
    #[error("invalid EPUB: {0}")]
    Invalid(String),
    #[error("unsupported EPUB: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Zip(#[from] ZipError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Apply a device preset to an existing EPUB without opening a PDF or loading models.
/// Publishes a new archive atomically and refuses to replace an existing file.
pub fn optimize_epub(
    input: &Path,
    output: &Path,
    target: OptimizationTarget,
) -> Result<(), OptimizeError> {
    match output.symlink_metadata() {
        Ok(_) => return Err(OptimizeError::DestinationExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut source = ZipArchive::new(File::open(input)?)?;
    let parent = output.parent().filter(|path| !path.as_os_str().is_empty());
    let mut staged = NamedTempFile::new_in(parent.unwrap_or(Path::new(".")))?;
    optimize_archive(&mut source, staged.as_file_mut(), target)?;
    staged.as_file().sync_all()?;
    staged.persist_noclobber(output).map_err(|failure| {
        if failure.error.kind() == io::ErrorKind::AlreadyExists {
            OptimizeError::DestinationExists
        } else {
            failure.error.into()
        }
    })?;
    Ok(())
}

struct Resource<'a> {
    item: Node<'a, 'a>,
    path: String,
    media_type: &'a str,
}

struct Part {
    path: String,
    id: Option<String>,
    body: Range<usize>,
    group: usize,
}

struct Chapter {
    body: Range<usize>,
    parts: Vec<Part>,
    anchors: HashMap<String, usize>,
}

struct ArchiveNames {
    paths: HashSet<String>,
    ids: HashSet<String>,
    next: usize,
}

impl ArchiveNames {
    fn part_id(&mut self) -> String {
        loop {
            let id = format!("epubr-part-{}", self.next);
            self.next += 1;
            if self.ids.insert(id.clone()) {
                return id;
            }
        }
    }

    fn new_path(&mut self, path: &str, extension: &str) -> String {
        let (parent, filename) = path.rsplit_once('/').map_or(("", path), |(parent, name)| {
            (&path[..parent.len() + 1], name)
        });
        let stem = filename.rsplit_once('.').map_or(filename, |(name, _)| name);
        loop {
            let result = format!("{parent}{stem}-epubr-{}.{extension}", self.next);
            self.next += 1;
            if self.paths.insert(result.clone()) {
                return result;
            }
        }
    }
}

type Edits = Vec<(Range<usize>, String)>;

fn optimize_archive(
    source: &mut ZipArchive<File>,
    output: &mut File,
    target: OptimizationTarget,
) -> Result<(), OptimizeError> {
    let mut names = HashSet::new();
    for index in 0..source.len() {
        let member = source.by_index_raw(index)?;
        let name = member.name()?;
        if member.enclosed_name().is_none()
            || member.is_symlink()
            || name.starts_with('/')
            || name.split('/').any(|part| matches!(part, "." | ".."))
            || name.contains('\\')
            || !names.insert(name.into_owned())
        {
            return Err(OptimizeError::Invalid(
                "invalid or duplicate archive path".into(),
            ));
        }
        if member.encrypted() {
            return Err(OptimizeError::Unsupported("encrypted archive entries"));
        }
    }
    if read_text(source, "mimetype")? != MIME {
        return Err(OptimizeError::Invalid("incorrect mimetype".into()));
    }
    if names.contains("META-INF/signatures.xml") {
        return Err(OptimizeError::Unsupported("signed publications"));
    }

    let container = read_text(source, CONTAINER)?;
    let container = parse(&container, CONTAINER)?;
    let mut rootfiles = container.descendants().filter_map(|node| {
        node.has_tag_name("rootfile")
            .then(|| node.attribute("full-path"))
            .flatten()
    });
    let package_path = rootfiles
        .next()
        .ok_or_else(|| invalid(CONTAINER, "missing package path"))?;
    if rootfiles.next().is_some() {
        return Err(OptimizeError::Unsupported("multiple renditions"));
    }
    let package_xml = read_text(source, package_path)?;
    let package = parse(&package_xml, package_path)?;
    let root = package.root_element();
    if !root.has_tag_name((OPF, "package")) {
        return Err(invalid(package_path, "missing OPF package"));
    }
    if root
        .descendants()
        .any(|node| node.attribute((XML, "base")).is_some())
    {
        return Err(OptimizeError::Unsupported(
            "custom URL bases in the package document",
        ));
    }
    let manifest = child(root, "manifest", package_path)?;
    let spine = child(root, "spine", package_path)?;
    let mut resources = BTreeMap::new();
    let mut resource_paths = HashSet::new();
    let reserved_ids = package
        .descendants()
        .filter_map(|node| node.attribute("id").map(str::to_owned))
        .collect();
    for item in manifest
        .children()
        .filter(|node| node.has_tag_name((OPF, "item")))
    {
        let id = required(item, "id", package_path)?;
        let href = required(item, "href", package_path)?;
        let media_type = required(item, "media-type", package_path)?;
        if let Some((path, _)) = resolve_href(package_path, href) {
            if !names.contains(&path) || !resource_paths.insert(path.clone()) {
                return Err(invalid(
                    package_path,
                    &format!("missing or duplicate resource {href}"),
                ));
            }
            resources.insert(
                id,
                Resource {
                    item,
                    path,
                    media_type,
                },
            );
        }
    }

    let items: Vec<_> = spine
        .children()
        .filter(|node| node.has_tag_name((OPF, "itemref")))
        .map(|item| {
            let id = required(item, "idref", package_path)?;
            let resource = resources
                .get(id)
                .ok_or_else(|| invalid(package_path, &format!("invalid spine item {id}")))?;
            Ok((item, resource))
        })
        .collect::<Result<_, OptimizeError>>()?;
    if items.is_empty() {
        return Err(invalid(package_path, "empty spine"));
    }

    let documents = read_documents(source, &resources)?;
    let chapter_roots = chapter_roots(&documents, &resources, spine)?;
    let previous_groups = read_previous_groups(source, &names, &items)?;
    let encrypted = encrypted_resources(source, &names)?;
    let fixed_layout = root.descendants().any(|node| {
        node.has_tag_name((OPF, "meta"))
            && node.attribute("property") == Some("rendition:layout")
            && node
                .text()
                .is_some_and(|text| text.trim() == "pre-paginated")
    });
    let has_fixed_pages = fixed_layout
        || items
            .iter()
            .any(|(item, _)| property(*item, "rendition:layout-pre-paginated"));
    let scripted = resources.values().any(|resource| {
        property(resource.item, "scripted")
            || matches!(
                resource.media_type,
                "text/javascript" | "application/javascript"
            )
    });
    let images_protected = if scripted || has_fixed_pages {
        HashSet::new()
    } else {
        let stylesheets = resources
            .values()
            .filter(|resource| resource.media_type == "text/css")
            .map(|resource| Ok((resource.path.clone(), read_text(source, &resource.path)?)))
            .collect::<Result<HashMap<_, _>, OptimizeError>>()?;
        let candidates = resources
            .values()
            .filter(|resource| supported_image(resource.media_type))
            .map(|resource| resource.path.as_str())
            .collect();
        protected_images(&documents, &stylesheets, &candidates)
    };
    let mut chapters = HashMap::new();
    let mut names = ArchiveNames {
        paths: names,
        ids: reserved_ids,
        next: 1,
    };
    let mut group = 0;
    for (index, (item, resource)) in items.iter().enumerate() {
        if chapters.contains_key(&resource.path) {
            continue;
        }
        if index == 0
            || !previous_groups[index]
                .is_some_and(|previous| previous_groups[index - 1] == Some(previous))
        {
            group += 1;
        }
        let Some(xml) = documents.get(&resource.path) else {
            continue;
        };
        if resource.media_type != "application/xhtml+xml" {
            continue;
        }
        let document = parse(xml, &resource.path)?;
        let can_split = !fixed_layout
            && !property(*item, "rendition:layout-pre-paginated")
            && !property(resource.item, "nav")
            && !scripted
            && resource.item.attribute("media-overlay").is_none()
            && !document.descendants().any(|node| {
                node.is_element()
                    && (node.attribute((XML, "base")).is_some()
                        || matches!(node.tag_name().name(), "base" | "script" | "form"))
            });
        if let Some(chapter) = plan_chapter(
            &document,
            &resource.path,
            chapter_roots.get(&resource.path).filter(|_| can_split),
            can_split,
            &mut names,
            &mut group,
        ) {
            chapters.insert(resource.path.clone(), chapter);
        }
    }

    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(output);
    zip.start_file("mimetype", stored)?;
    zip.write_all(MIME.as_bytes())?;

    let mut image_paths = HashMap::new();
    let mut package_edits = Vec::new();
    let mut replaced = HashSet::new();
    if !has_fixed_pages && !scripted {
        for resource in resources
            .values()
            .filter(|resource| resource.media_type.starts_with("image/"))
        {
            if encrypted.contains(&resource.path)
                || !supported_image(resource.media_type)
                || images_protected.contains(&resource.path)
            {
                continue;
            }
            let data = read_member(source, &resource.path)?;
            let Some((format, bytes)) = optimize_image(target, resource.media_type, &data) else {
                continue;
            };
            let path = if format == resource.media_type {
                resource.path.clone()
            } else {
                names.new_path(&resource.path, "jpg")
            };
            zip.start_file(&path, stored)?;
            zip.write_all(&bytes)?;
            replace_attr(resource.item, "media-type", format, &mut package_edits);
            if path != resource.path {
                replace_attr(
                    resource.item,
                    "href",
                    &relative_href(package_path, &path, None),
                    &mut package_edits,
                );
                image_paths.insert(resource.path.clone(), path);
            }
            replaced.insert(resource.path.as_str());
        }
    }

    for guide in root
        .children()
        .filter(|node| node.has_tag_name((OPF, "guide")))
    {
        package_edits.extend(link_edits(guide, package_path, &chapters, &image_paths));
    }

    let mut location_entries = HashMap::new();
    let mut documents_in_order: Vec<_> = documents.iter().collect();
    documents_in_order.sort_unstable_by_key(|(path, _)| *path);
    for (path, xml) in documents_in_order {
        let document = parse(xml, path)?;
        let chapter = chapters.get(path);
        let edits = link_edits(document.root_element(), path, &chapters, &image_paths);
        if let Some(chapter) = chapter.filter(|chapter| chapter.parts.len() > 1) {
            for part in &chapter.parts {
                let mut part_edits = edits
                    .iter()
                    .filter(|(range, _)| {
                        range.end <= chapter.body.start
                            || range.start >= chapter.body.end
                            || (range.start >= part.body.start && range.end <= part.body.end)
                    })
                    .cloned()
                    .collect::<Edits>();
                part_edits.push((chapter.body.start..part.body.start, String::new()));
                part_edits.push((part.body.end..chapter.body.end, String::new()));
                let content = apply_edits(xml, part_edits);
                zip.start_file(&part.path, deflated)?;
                zip.write_all(content.as_bytes())?;
                location_entries.insert(part.path.clone(), content);
            }
        } else {
            let content = apply_edits(xml, edits);
            zip.start_file(path, deflated)?;
            zip.write_all(content.as_bytes())?;
            location_entries.insert(path.clone(), content);
        }
    }

    for (itemref, resource) in &items {
        if let Some(chapter) = chapters
            .get(&resource.path)
            .filter(|chapter| chapter.parts.len() > 1)
        {
            let mut additions = String::new();
            for part in chapter.parts.iter().skip(1) {
                let id = part.id.as_deref().expect("continuation ID");
                additions.push_str(&clone_itemref(*itemref, &package_xml, id));
            }
            let end = itemref.range().end;
            package_edits.push((end..end, additions));
        }
    }
    for resource in resources.values() {
        if let Some(chapter) = chapters
            .get(&resource.path)
            .filter(|chapter| chapter.parts.len() > 1)
        {
            let mut additions = String::new();
            for part in &chapter.parts {
                let content = &location_entries[&part.path];
                let document = parse(content, &part.path)?;
                let has_mathml = document
                    .descendants()
                    .any(|node| node.has_tag_name(("http://www.w3.org/1998/Math/MathML", "math")));
                let item = resource.item;
                if let Some(id) = &part.id {
                    let mut edits = Vec::new();
                    replace_attr(item, "id", id, &mut edits);
                    replace_attr(
                        item,
                        "href",
                        &relative_href(package_path, &part.path, None),
                        &mut edits,
                    );
                    adjust_mathml(item, has_mathml, &mut edits);
                    additions.push_str(&apply_subtree(&package_xml, item.range(), edits));
                } else {
                    adjust_mathml(item, has_mathml, &mut package_edits);
                }
            }
            let end = resource.item.range().end;
            package_edits.push((end..end, additions));
        }
    }
    if let Some(modified) = root
        .descendants()
        .find(|node| {
            node.has_tag_name((OPF, "meta"))
                && node.attribute("property") == Some("dcterms:modified")
        })
        .and_then(|node| node.first_child())
        .filter(|node| node.is_text())
    {
        package_edits.push((
            modified.range(),
            Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        ));
    }
    zip.start_file(package_path, deflated)?;
    zip.write_all(apply_edits(&package_xml, package_edits).as_bytes())?;

    for index in 0..source.len() {
        let member = source.by_index_raw(index)?;
        let name = member.name()?;
        if matches!(
            name.as_ref(),
            "mimetype" | locations::LOCATION_PATH | locations::OPTIMIZER_PATH
        ) || name == "META-INF/crossink/optimizer-images-v1.idx"
            || name.starts_with("META-INF/crossink/pxc/")
            || name == package_path
            || documents.contains_key(name.as_ref())
            || replaced.contains(name.as_ref())
        {
            continue;
        }
        zip.raw_copy_file(member)?;
    }

    let mut locations = Locations::default();
    for (index, (_, resource)) in items.iter().enumerate() {
        if let Some(chapter) = chapters.get(&resource.path) {
            for part in &chapter.parts {
                locations.push(&part.path, &location_entries[&part.path], part.group)?;
            }
        } else {
            let content = location_entries
                .get(&resource.path)
                .map_or("<html><body/></html>", String::as_str);
            locations.push(&resource.path, content, usize::MAX - index)?;
        }
    }
    super::write_location_files(&mut zip, target, locations, stored)?;
    zip.finish()?;
    Ok(())
}

fn plan_chapter(
    document: &Document<'_>,
    path: &str,
    roots: Option<&HashSet<String>>,
    can_split: bool,
    names: &mut ArchiveNames,
    group: &mut usize,
) -> Option<Chapter> {
    let body = document
        .root_element()
        .children()
        .find(|node| node.has_tag_name((XHTML, "body")))?;
    let mut children = body.children();
    let start = children.next()?.range().start;
    let end = body.last_child()?.range().end;
    let body_range = start..end;
    let mut parts = Vec::new();
    let mut anchors = HashMap::new();
    let mut part_start = start;
    let mut part_id = None;
    let mut part_path = path.to_owned();
    let mut has_content = false;
    let mut previous_heading = false;
    for node in body.children() {
        let range = node.range();
        let heading = node.is_element()
            && matches!(
                node.tag_name().name(),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            );
        let root = roots.is_some_and(|ids| anchor_ids(node).any(|id| ids.contains(id)));
        if can_split
            && has_content
            && (root
                || (!previous_heading
                    && node.is_element()
                    && range.end - part_start > OPTIMIZED_SECTION_BYTES))
        {
            parts.push(Part {
                path: part_path,
                id: part_id,
                body: part_start..range.start,
                group: *group,
            });
            part_start = range.start;
            part_path = names.new_path(path, "xhtml");
            part_id = Some(names.part_id());
            has_content = false;
            if root {
                *group += 1;
            }
        }
        for id in anchor_ids(node) {
            anchors.entry(id.to_owned()).or_insert(parts.len());
        }
        if node.is_element() || node.text().is_some_and(|text| !text.trim().is_empty()) {
            previous_heading = heading;
            has_content = true;
        }
    }
    parts.push(Part {
        path: part_path,
        id: part_id,
        body: part_start..end,
        group: *group,
    });
    Some(Chapter {
        body: body_range,
        parts,
        anchors,
    })
}

fn anchor_ids<'a>(node: Node<'a, 'a>) -> impl Iterator<Item = &'a str> {
    node.descendants().filter_map(|child| {
        child
            .attribute("id")
            .or_else(|| child.attribute((XML, "id")))
            .or_else(|| {
                child
                    .has_tag_name((XHTML, "a"))
                    .then(|| child.attribute("name"))
                    .flatten()
            })
    })
}

fn chapter_roots(
    documents: &HashMap<String, String>,
    resources: &BTreeMap<&str, Resource<'_>>,
    spine: Node<'_, '_>,
) -> Result<HashMap<String, HashSet<String>>, OptimizeError> {
    let nav = resources
        .values()
        .find(|resource| property(resource.item, "nav"))
        .or_else(|| spine.attribute("toc").and_then(|id| resources.get(id)));
    let Some(nav) = nav else {
        return Ok(HashMap::new());
    };
    let Some(xml) = documents.get(&nav.path) else {
        return Ok(HashMap::new());
    };
    let document = parse(xml, &nav.path)?;
    let mut roots = HashMap::<_, HashSet<_>>::new();
    let mut add = |uri| {
        if let Some((path, Some(id))) = resolve_href(&nav.path, uri) {
            roots.entry(path).or_default().insert(id);
        }
    };
    if nav.media_type == "application/x-dtbncx+xml" {
        if let Some(map) = document
            .descendants()
            .find(|node| node.has_tag_name("navMap"))
        {
            for point in map.children().filter(|node| node.has_tag_name("navPoint")) {
                if let Some(src) = point
                    .children()
                    .find(|node| node.has_tag_name("content"))
                    .and_then(|node| node.attribute("src"))
                {
                    add(src);
                }
            }
        }
    } else if let Some(list) = document
        .descendants()
        .find(|node| {
            node.has_tag_name((XHTML, "nav"))
                && node
                    .attribute((EPUB, "type"))
                    .is_some_and(|kind| kind.split_whitespace().any(|kind| kind == "toc"))
        })
        .and_then(|node| {
            node.children()
                .find(|node| node.has_tag_name((XHTML, "ol")))
        })
    {
        for item in list
            .children()
            .filter(|node| node.has_tag_name((XHTML, "li")))
        {
            if let Some(href) = item
                .children()
                .find(|node| node.has_tag_name((XHTML, "a")))
                .and_then(|node| node.attribute("href"))
            {
                add(href);
            }
        }
    }
    Ok(roots)
}

fn link_edits(
    root: Node<'_, '_>,
    path: &str,
    chapters: &HashMap<String, Chapter>,
    images: &HashMap<String, String>,
) -> Edits {
    let mut edits = Vec::new();
    let chapter = chapters.get(path);
    for node in root.descendants().filter(Node::is_element) {
        let from = chapter
            .and_then(|chapter| {
                chapter
                    .parts
                    .iter()
                    .find(|part| part.body.contains(&node.range().start))
            })
            .map_or(path, |part| &part.path);
        for attr in node.attributes().filter(|attr| {
            matches!(attr.name(), "href" | "src" | "poster" | "textref")
                || (attr.name() == "data" && node.has_tag_name((XHTML, "object")))
        }) {
            let value = attr.value();
            let Some((to, fragment)) = resolve_href(path, value) else {
                continue;
            };
            let mut destination = to.as_str();
            if let Some(image) = images.get(&to) {
                destination = image;
            } else if let Some(target) = chapters.get(&to)
                && let Some(index) = fragment.as_ref().and_then(|id| target.anchors.get(id))
            {
                destination = &target.parts[*index].path;
            }
            if destination == to && (from == path || !value.trim_start().starts_with(['#', '?'])) {
                continue;
            }
            let mut href = relative_href(from, destination, fragment.as_deref());
            if let Some((_, query)) = value.split('#').next().and_then(|uri| uri.split_once('?')) {
                let index = href.find('#').unwrap_or(href.len());
                href.insert_str(index, &format!("?{query}"));
            }
            edits.push((attr.range_value(), escape_attr(&href)));
        }
    }
    edits
}

fn adjust_mathml(item: Node<'_, '_>, has_mathml: bool, edits: &mut Edits) {
    if property(item, "mathml") && !has_mathml {
        let value = item.attribute("properties").expect("mathml property");
        let properties = value
            .split_whitespace()
            .filter(|kind| *kind != "mathml")
            .collect::<Vec<_>>()
            .join(" ");
        if properties.is_empty() {
            let attr = item
                .attribute_node("properties")
                .expect("properties attribute");
            edits.push((attr.range(), String::new()));
        } else {
            replace_attr(item, "properties", &properties, edits);
        }
    }
}

fn clone_itemref(item: Node<'_, '_>, xml: &str, id: &str) -> String {
    let mut edits = Vec::new();
    replace_attr(item, "idref", id, &mut edits);
    if let Some(attr) = item.attribute_node("id") {
        edits.push((attr.range(), String::new()));
    }
    apply_subtree(xml, item.range(), edits)
}

fn read_documents(
    source: &mut ZipArchive<File>,
    resources: &BTreeMap<&str, Resource<'_>>,
) -> Result<HashMap<String, String>, OptimizeError> {
    resources
        .values()
        .filter(|resource| {
            matches!(
                resource.media_type,
                "application/xhtml+xml"
                    | "application/x-dtbncx+xml"
                    | "application/smil+xml"
                    | "image/svg+xml"
            )
        })
        .map(|resource| Ok((resource.path.clone(), read_text(source, &resource.path)?)))
        .collect()
}

fn read_previous_groups(
    source: &mut ZipArchive<File>,
    names: &HashSet<String>,
    items: &[(Node<'_, '_>, &Resource<'_>)],
) -> Result<Vec<Option<u64>>, OptimizeError> {
    let mut groups = vec![None; items.len()];
    if names.contains(locations::LOCATION_PATH) {
        let member = source.by_name(locations::LOCATION_PATH)?;
        if member.size() <= 64 * 1024 {
            let Ok(value) = serde_json::from_reader::<_, serde_json::Value>(member) else {
                return Ok(groups);
            };
            if let Some(spine) = value["spine"]
                .as_array()
                .filter(|spine| spine.len() == items.len())
            {
                for ((slot, entry), (_, resource)) in groups.iter_mut().zip(spine).zip(items) {
                    if entry["href"] == resource.path {
                        *slot = entry["chapterGroup"].as_u64();
                    }
                }
            }
        }
    }
    Ok(groups)
}

fn encrypted_resources(
    source: &mut ZipArchive<File>,
    names: &HashSet<String>,
) -> Result<HashSet<String>, OptimizeError> {
    let path = "META-INF/encryption.xml";
    let mut resources = HashSet::new();
    if !names.contains(path) {
        return Ok(resources);
    }
    let xml = read_text(source, path)?;
    let document = parse(&xml, path)?;
    for entry in document
        .descendants()
        .filter(|node| node.has_tag_name("EncryptedData"))
    {
        let algorithm = entry
            .descendants()
            .find(|node| node.has_tag_name("EncryptionMethod"))
            .and_then(|node| node.attribute("Algorithm"));
        if !matches!(
            algorithm,
            Some("http://www.idpf.org/2008/embedding" | "http://ns.adobe.com/pdf/enc#RC")
        ) {
            return Err(OptimizeError::Unsupported("encrypted publications"));
        }
        if let Some(uri) = entry
            .descendants()
            .find(|node| node.has_tag_name("CipherReference"))
            .and_then(|node| node.attribute("URI"))
            && let Some((path, _)) = resolve_href("", uri)
        {
            resources.insert(path);
        }
    }
    Ok(resources)
}

fn read_text(source: &mut ZipArchive<File>, path: &str) -> Result<String, OptimizeError> {
    String::from_utf8(read_member(source, path)?).map_err(|_| invalid(path, "expected UTF-8 XML"))
}

fn read_member(source: &mut ZipArchive<File>, path: &str) -> Result<Vec<u8>, OptimizeError> {
    let mut member = source.by_name(path)?;
    let mut data = Vec::new();
    member.read_to_end(&mut data)?;
    Ok(data)
}

fn parse<'a>(xml: &'a str, path: &str) -> Result<Document<'a>, OptimizeError> {
    let options = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    Document::parse_with_options(xml, options).map_err(|error| invalid(path, &error.to_string()))
}

fn child<'a>(node: Node<'a, 'a>, name: &str, path: &str) -> Result<Node<'a, 'a>, OptimizeError> {
    node.children()
        .find(|node| node.has_tag_name((OPF, name)))
        .ok_or_else(|| invalid(path, &format!("missing {name}")))
}

fn required<'a>(node: Node<'a, 'a>, attr: &str, path: &str) -> Result<&'a str, OptimizeError> {
    node.attribute(attr)
        .ok_or_else(|| invalid(path, &format!("missing {attr}")))
}

fn invalid(path: &str, message: &str) -> OptimizeError {
    OptimizeError::Invalid(format!("{path}: {message}"))
}

fn property(node: Node<'_, '_>, property: &str) -> bool {
    node.attribute("properties")
        .is_some_and(|value| value.split_whitespace().any(|kind| kind == property))
}

fn supported_image(media_type: &str) -> bool {
    matches!(
        media_type,
        "image/gif" | "image/jpeg" | "image/png" | "image/webp"
    )
}

fn replace_attr(node: Node<'_, '_>, name: &str, value: &str, edits: &mut Edits) {
    let attr = node.attribute_node(name).expect("required attribute");
    edits.push((attr.range_value(), escape_attr(value)));
}

fn escape_attr(value: &str) -> String {
    escape(value).replace('\'', "&apos;")
}

fn apply_subtree(xml: &str, range: Range<usize>, mut edits: Edits) -> String {
    for (edit, _) in &mut edits {
        *edit = edit.start - range.start..edit.end - range.start;
    }
    apply_edits(&xml[range], edits)
}

fn apply_edits(xml: &str, mut edits: Edits) -> String {
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    let mut output = String::with_capacity(xml.len());
    let mut start = 0;
    for (range, replacement) in edits {
        output.push_str(&xml[start..range.start]);
        output.push_str(&replacement);
        start = range.end;
    }
    output.push_str(&xml[start..]);
    output
}
