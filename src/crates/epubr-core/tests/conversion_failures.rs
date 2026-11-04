use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use epubr_core::{CancellationToken, ConversionError, ConversionOptions, Progress, convert};
use tempfile::tempdir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn rejects_missing_pdf_without_creating_an_epub() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let result = convert(
        &directory.path().join("missing.pdf"),
        &output,
        ConversionOptions::default(),
        &CancellationToken::default(),
        |_| {},
    );

    assert!(!output.exists());
    assert!(
        matches!(result, Err(ConversionError::InvalidInput)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
}

#[test]
fn rejects_non_pdf_bytes_with_a_pdf_extension() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("pretend.pdf");
    let output = directory.path().join("output.epub");
    let bytes = b"This file contains plain text.";
    fs::write(&input, bytes).unwrap();

    let result = convert(
        &input,
        &output,
        ConversionOptions::default(),
        &CancellationToken::default(),
        |_| {},
    );

    assert!(!output.exists());
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert!(
        matches!(result, Err(ConversionError::InvalidInput)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
}

#[test]
fn rejects_a_truncated_pdf_without_publishing_an_epub() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("truncated.pdf");
    let output = directory.path().join("output.epub");
    fs::write(&input, b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog").unwrap();

    let result = convert(
        &input,
        &output,
        ConversionOptions::default(),
        &CancellationToken::default(),
        |_| {},
    );

    assert!(!output.exists());
    assert!(
        matches!(result, Err(ConversionError::InvalidInput)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
}

#[test]
fn rejects_a_valid_pdf_with_no_convertible_content() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let result = convert(
        &fixture("blank.pdf"),
        &output,
        ConversionOptions::default(),
        &CancellationToken::default(),
        |_| {},
    );

    assert!(!output.exists());
    assert!(
        matches!(result, Err(ConversionError::NoContent)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
}

#[test]
fn rejects_zero_based_reversed_and_out_of_bounds_page_ranges() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let input = fixture("basic.pdf");
    let mut outcomes = Vec::new();
    for pages in [(0, 1), (2, 1), (1, 10), (10, 10)] {
        let options = ConversionOptions {
            pages: Some(pages),
            ..ConversionOptions::default()
        };
        let result = convert(
            &input,
            &output,
            options,
            &CancellationToken::default(),
            |_| {},
        );
        assert!(!output.exists(), "created an EPUB for range {pages:?}");
        outcomes.push((pages, result.err()));
    }
    assert!(
        outcomes
            .iter()
            .all(|(_, error)| matches!(error, Some(ConversionError::InvalidPageRange))),
        "unexpected page-range outcomes: {outcomes:?}",
    );
}

#[test]
fn preserves_an_existing_output_file() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let original = b"Pre-existing ebook bytes";
    fs::write(&output, original).unwrap();

    let result = convert(
        &fixture("basic.pdf"),
        &output,
        ConversionOptions::default(),
        &CancellationToken::default(),
        |_| {},
    );

    assert_eq!(fs::read(&output).unwrap(), original);
    assert!(
        matches!(result, Err(ConversionError::DestinationExists)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
}

#[test]
fn preserves_a_destination_created_during_conversion() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let competing_write = b"Output published by another process";
    let created = AtomicBool::new(false);
    let result = convert(
        &fixture("basic.pdf"),
        &output,
        ConversionOptions::default(),
        &CancellationToken::default(),
        |progress| {
            if matches!(progress, Progress::Page { success: true, .. })
                && !created.swap(true, Ordering::Relaxed)
            {
                fs::write(&output, competing_write).unwrap();
            }
        },
    );

    assert!(
        matches!(result, Err(ConversionError::DestinationExists)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
    assert_eq!(fs::read(&output).unwrap(), competing_write);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn leaves_a_pre_cancelled_destination_empty() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let cancellation = CancellationToken::default();
    cancellation.cancel();

    let result = convert(
        &fixture("basic.pdf"),
        &output,
        ConversionOptions::default(),
        &cancellation,
        |_| {},
    );

    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    assert!(
        matches!(result, Err(ConversionError::Cancelled)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
}

#[test]
fn cancellation_after_a_page_leaves_no_output_or_staging_files() {
    let directory = tempdir().unwrap();
    let output = directory.path().join("output.epub");
    let cancellation = CancellationToken::default();
    let page_reported = AtomicBool::new(false);
    let result = convert(
        &fixture("basic.pdf"),
        &output,
        ConversionOptions::default(),
        &cancellation,
        |progress| {
            if matches!(progress, Progress::Page { success: true, .. }) {
                page_reported.store(true, Ordering::Relaxed);
                cancellation.cancel();
            }
        },
    );

    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    assert!(
        matches!(result, Err(ConversionError::Cancelled)),
        "unexpected conversion error: {:?}",
        result.err(),
    );
    assert!(page_reported.load(Ordering::Relaxed));
}
