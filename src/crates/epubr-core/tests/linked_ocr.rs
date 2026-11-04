use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use epubr_core::{ExtractionError, ExtractionOptions, OcrMode, Progress, extract};

#[test]
fn rejects_invalid_page_ranges_before_loading_input() {
    for pages in [(0, 1), (3, 2)] {
        let result = extract(
            Path::new("not-opened.pdf"),
            ExtractionOptions {
                pages: Some(pages),
                ..ExtractionOptions::default()
            },
            |_| panic!("invalid requests must produce no progress"),
        );
        assert!(matches!(result, Err(ExtractionError::InvalidPageRange)));
    }
}

#[test]
#[ignore = "requires the pinned Docling model cache in EPUBR_TEST_MODEL_ROOT"]
fn extracts_text_from_a_raster_only_pdf() {
    const CHILD: &str = "EPUBR_LINKED_OCR_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = std::env::var_os("EPUBR_TEST_MODEL_ROOT")
            .expect("set EPUBR_TEST_MODEL_ROOT to the directory containing .models");
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "extracts_text_from_a_raster_only_pdf",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(root)
            .env(CHILD, "1")
            .env("DOCLING_RS_EP", "cpu")
            .status()
            .unwrap();
        assert!(status.success(), "linked OCR probe failed: {status}");
        return;
    }

    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ocr-image-only.pdf");
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let result = extract(
        &input,
        ExtractionOptions {
            mode: OcrMode::Scan,
            pages: None,
        },
        move |event| observed_events.lock().unwrap().push(event),
    )
    .unwrap();

    assert_eq!(result.pages, 1);
    assert!(result.json.contains("SCAN FIXTURE"));
    assert!(
        result
            .json
            .contains("The quick brown fox jumps over the lazy dog.")
    );
    assert_eq!(
        events.lock().unwrap().as_slice(),
        [
            Progress::Started,
            Progress::Page {
                completed: 1,
                total: 1,
                page: 1,
                success: true,
            },
            Progress::Finished,
        ]
    );
}
