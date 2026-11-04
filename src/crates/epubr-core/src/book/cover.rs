use std::collections::HashSet;
use std::path::Path;

use docling_pdf::{pdf_meta::PdfMeta, render::Renderer};
use lopdf::{Document, ObjectId, content::Content};
use serde_json::Value;

use super::{page_no, symbols};

pub(super) fn render(doc: &Value, pdf: &Document, input: &Path, title: &str) -> Option<Vec<u8>> {
    let size = &doc["pages"]["1"]["size"];
    let width = size["width"].as_f64()?;
    let height = size["height"].as_f64()?;
    let page_id = *pdf.get_pages().get(&1)?;
    let page = pdf.get_dictionary(page_id).ok()?;
    let media = page.get(b"MediaBox").ok().and_then(dimensions);
    let cropped_spread = media.is_some_and(|(w, h)| w > h && width < 0.55 * w && height > width)
        && page.has(b"CropBox");
    let illustrated = doc["pictures"].as_array().is_some_and(|pictures| {
        pictures
            .iter()
            .filter(|picture| page_no(picture) == 1)
            .any(|picture| {
                let bbox = &picture["prov"][0]["bbox"];
                let span = |a: &str, b: &str| Some((bbox[a].as_f64()? - bbox[b].as_f64()?).abs());
                span("r", "l").is_some_and(|w| w > 0.60 * width)
                    && span("t", "b").is_some_and(|h| h > 0.50 * height)
            })
    });
    let small_first_page = height > width
        && doc["pages"]["2"]["size"]["width"]
            .as_f64()
            .is_some_and(|next_width| width < 0.5 * next_width)
        && doc["pages"]["2"]["size"]["height"]
            .as_f64()
            .is_some_and(|next_height| height < 0.5 * next_height);
    let has_bookmarks = pdf.catalog().is_ok_and(|catalog| catalog.has(b"Outlines"));
    let raster_cover = (small_first_page || has_bookmarks)
        && !doc["pages"]["2"].is_null()
        && single_raster(pdf, page_id, width, height).is_some();
    if !cropped_spread && !illustrated && !raster_cover {
        return None;
    }
    let cover_words: Vec<_> = doc["texts"]
        .as_array()?
        .iter()
        .filter(|item| page_no(item) == 1)
        .filter_map(|item| item["text"].as_str())
        .flat_map(words)
        .collect();
    let title_words: HashSet<_> = words(title).collect();
    let matched = title_words
        .iter()
        .filter(|word| cover_words.contains(word))
        .count();
    // A cover can be a full-page raster or a sparse illustrated page with
    // matching title text. Use the PDF operators for the raster case because
    // OCR can misidentify parts of the image as text.
    let title_matches =
        cover_words.len() <= 45 && matched >= 2 && matched * 3 >= title_words.len() * 2;
    if !raster_cover && !title_matches {
        return None;
    }
    let meta = PdfMeta::open(&std::fs::read(input).ok()?)?;
    let scale = (1600.0 / width.max(height)).min(3.0);
    let image = Renderer::new(&meta).render_scaled(0, scale)?;
    super::math::encode_png(&image)
}

fn single_raster(pdf: &Document, page_id: ObjectId, width: f64, height: f64) -> Option<()> {
    let content = pdf.get_page_content_with_limit(page_id, 1024 * 1024).ok()?;
    let content = Content::decode(&content).ok()?;
    let mut painted = content
        .operations
        .iter()
        .filter(|op| !matches!(op.operator.as_str(), "q" | "Q"));
    let transform = painted.next()?;
    let image = painted.next()?;
    if transform.operator != "cm" || image.operator != "Do" || painted.next().is_some() {
        return None;
    }
    let [a, b, c, d, x, y] = transform.operands.as_slice() else {
        return None;
    };
    let fills_page = [
        (a, width),
        (b, 0.0),
        (c, 0.0),
        (d, height),
        (x, 0.0),
        (y, 0.0),
    ]
    .into_iter()
    .all(|(value, expected)| {
        value
            .as_float()
            .is_ok_and(|actual| (f64::from(actual) - expected).abs() < 1.0)
    });
    let name = image.operands.first()?.as_name().ok()?;
    (fills_page && symbols::is_image(pdf, page_id, name)).then_some(())
}

fn dimensions(value: &lopdf::Object) -> Option<(f64, f64)> {
    let [left, bottom, right, top] = value.as_array().ok()?.as_slice() else {
        return None;
    };
    let coordinate = |point: &lopdf::Object| point.as_float().ok().map(f64::from);
    Some((
        coordinate(right)? - coordinate(left)?,
        coordinate(top)? - coordinate(bottom)?,
    ))
}

fn words(text: &str) -> impl Iterator<Item = String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(str::to_lowercase)
}
