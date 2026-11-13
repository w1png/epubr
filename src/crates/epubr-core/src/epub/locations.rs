use std::io;

use roxmltree::Document;
use serde::Serialize;

use crate::OptimizationTarget;

use super::optimizer::image_bounds;

pub(super) const LOCATION_PATH: &str = "META-INF/x-locations.json";
pub(super) const OPTIMIZER_PATH: &str = "META-INF/crossink/optimizer-v1.json";

const WORDS_PER_LOCATION: usize = 64;
const CHARACTERS_PER_PAGE: usize = 1500;
const MAX_LOCATION_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Locations {
    total_words: usize,
    total_characters: usize,
    total_locations: usize,
    spine: Vec<SpineEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpineEntry {
    index: usize,
    href: String,
    word_start: usize,
    word_count: usize,
    character_start: usize,
    character_count: usize,
    start_location: usize,
    end_location: usize,
    start_reference_page: usize,
    end_reference_page: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    chapter_group: Option<usize>,
    #[serde(skip)]
    section_group: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChapterGroup {
    index: usize,
    first_spine_index: usize,
    last_spine_index: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocationManifest {
    format: &'static str,
    version: u8,
    generator: &'static str,
    reference_page_unit: &'static str,
    words_per_location: usize,
    characters_per_reference_page: usize,
    total_words: usize,
    total_characters: usize,
    total_locations: usize,
    total_reference_pages: usize,
    spine: Vec<SpineEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    chapter_groups: Vec<ChapterGroup>,
}

impl Locations {
    pub(super) fn push(&mut self, path: &str, xhtml: &str, chapter_group: usize) -> io::Result<()> {
        let text = visible_text(xhtml)?;
        let mut words = 0_usize;
        let mut characters = 0;
        for word in text.split_whitespace() {
            characters += word.chars().count() + usize::from(words != 0);
            words += 1;
        }
        let location_count = words.div_ceil(WORDS_PER_LOCATION);
        let (start_location, end_location) = if location_count == 0 {
            (0, 0)
        } else {
            (
                self.total_locations + 1,
                self.total_locations + location_count,
            )
        };
        let (start_reference_page, end_reference_page) = if characters == 0 {
            (0, 0)
        } else {
            (
                self.total_characters / CHARACTERS_PER_PAGE + 1,
                (self.total_characters + characters).div_ceil(CHARACTERS_PER_PAGE),
            )
        };
        self.spine.push(SpineEntry {
            index: self.spine.len(),
            href: path.to_owned(),
            word_start: self.total_words,
            word_count: words,
            character_start: self.total_characters,
            character_count: characters,
            start_location,
            end_location,
            start_reference_page,
            end_reference_page,
            chapter_group: None,
            section_group: chapter_group,
        });
        self.total_words += words;
        self.total_characters += characters;
        self.total_locations += location_count;
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<Option<Vec<u8>>> {
        if self.total_locations == 0 {
            return Ok(None);
        }
        let mut chapter_groups = Vec::new();
        for entries in self
            .spine
            .chunk_by_mut(|left, right| left.section_group == right.section_group)
            .filter(|entries| entries.len() > 1)
        {
            let index = chapter_groups.len();
            chapter_groups.push(ChapterGroup {
                index,
                first_spine_index: entries[0].index,
                last_spine_index: entries[entries.len() - 1].index,
            });
            for entry in entries {
                entry.chapter_group = Some(index);
            }
        }
        let data = serde_json::to_vec(&LocationManifest {
            format: "x-locations",
            version: 1,
            generator: "epubr",
            reference_page_unit: "character",
            words_per_location: WORDS_PER_LOCATION,
            characters_per_reference_page: CHARACTERS_PER_PAGE,
            total_words: self.total_words,
            total_characters: self.total_characters,
            total_locations: self.total_locations,
            total_reference_pages: self.total_characters.div_ceil(CHARACTERS_PER_PAGE),
            spine: self.spine,
            chapter_groups,
        })?;
        Ok((data.len() <= MAX_LOCATION_BYTES).then_some(data))
    }
}

fn visible_text(xhtml: &str) -> io::Result<String> {
    let document = Document::parse(xhtml).map_err(io::Error::other)?;
    let mut text = String::new();
    if let Some(body) = document
        .root_element()
        .children()
        .find(|node| node.has_tag_name("body"))
    {
        for node in body.descendants().filter(|node| node.is_text()) {
            if !node.ancestors().any(|ancestor| {
                ancestor.is_element()
                    && matches!(ancestor.tag_name().name(), "script" | "style" | "nav")
            }) {
                text.push_str(node.text().expect("text node"));
            }
        }
    }
    Ok(text)
}

pub(super) fn optimizer_manifest(
    target: OptimizationTarget,
    has_locations: bool,
) -> io::Result<Vec<u8>> {
    let (width, height) = image_bounds(target);
    let device = match target {
        OptimizationTarget::XteinkX3 => "x3",
        OptimizationTarget::XteinkX4 => "x4",
        OptimizationTarget::XteinkX4Pro => "x4-pro",
        OptimizationTarget::SeeedStudioSticky => "sticky",
    };
    Ok(serde_json::to_vec(&serde_json::json!({
        "format": "crossink-optimizer",
        "version": 1,
        "generator": "epubr",
        "target": { "device": device, "width": width, "height": height },
        "features": { "xLocations": has_locations, "prebuiltPxc": false },
    }))?)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn location_manifest_counts_visible_text_and_groups_chapter_parts() {
        let mut locations = Locations::default();
        locations
            .push("EPUB/cover.xhtml", "<html><body><img/></body></html>", 0)
            .unwrap();
        locations
            .push(
                "EPUB/content.xhtml",
                &format!(
                    "<html><head><title>Title</title></head><body><nav>Contents</nav><p>{}</p></body></html>",
                    "œ ".repeat(65)
                ),
                1,
            )
            .unwrap();
        locations
            .push(
                "EPUB/chapter-1.xhtml",
                &format!(
                    "<html><body><p>book<sup>1</sup> &amp; {}</p></body></html>",
                    "語".repeat(1400)
                ),
                1,
            )
            .unwrap();
        let manifest: Value =
            serde_json::from_slice(&locations.finish().unwrap().unwrap()).unwrap();
        assert_eq!(manifest["format"], "x-locations");
        assert_eq!(manifest["version"], 1);
        assert_eq!(manifest["totalWords"], 68);
        assert_eq!(manifest["totalCharacters"], 1537);
        assert_eq!(manifest["totalLocations"], 3);
        assert_eq!(manifest["totalReferencePages"], 2);
        let spine = manifest["spine"].as_array().unwrap();
        assert_eq!(spine[0]["startLocation"], 0);
        assert_eq!(spine[1]["endLocation"], 2);
        assert_eq!(spine[2]["startLocation"], 3);
        assert_eq!(spine[2]["startReferencePage"], 1);
        assert_eq!(spine[2]["endReferencePage"], 2);
        assert_eq!(spine[2]["chapterGroup"], 0);
        assert_eq!(
            manifest["chapterGroups"],
            json!([{ "index": 0, "firstSpineIndex": 1, "lastSpineIndex": 2 }])
        );
    }
}
