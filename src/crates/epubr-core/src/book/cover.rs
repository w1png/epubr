use std::collections::HashSet;
use std::path::Path;

use docling_pdf::{pdf_meta::PdfMeta, render::Renderer};
use lopdf::Document;
use serde_json::Value;

use super::page_no;

pub(super) fn render(doc: &Value, pdf: &Document, input: &Path, title: &str) -> Option<Vec<u8>> {
    let size = &doc["pages"]["1"]["size"];
    let width = size["width"].as_f64()?;
    let height = size["height"].as_f64()?;
    let page = pdf.get_dictionary(*pdf.get_pages().get(&1)?).ok()?;
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
    if !cropped_spread && !illustrated {
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
    // A sparse first page with matching title text distinguishes a book
    // cover from an ordinary title page, full-page illustration, or scan.
    if cover_words.len() > 45 || matched < 2 || matched * 3 < title_words.len() * 2 {
        return None;
    }
    let meta = PdfMeta::open(&std::fs::read(input).ok()?)?;
    let scale = (1600.0 / width.max(height)).min(3.0);
    let image = Renderer::new(&meta).render_scaled(0, scale)?;
    super::math::encode_png(&image)
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
