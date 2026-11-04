//! PDF extraction and conversion interfaces for Epubr.

mod book;
mod conversion;
mod epub;
mod extraction;
mod models;

pub use conversion::{
    CancellationToken, ConversionError, ConversionOptions, ConversionReport, convert,
    convert_with_models, convert_with_worker,
};
#[doc(hidden)]
pub use extraction::{EXTRACTION_WORKER_FLAG, run_extraction_worker};
pub use models::{ModelConfig, ModelError, ModelProfile, PreparedModels};

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use docling::{ConversionStatus, DocumentConverter, InputFormat, ProgressKind, SourceDocument};

/// Selects how Docling uses the PDF's text layer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OcrMode {
    #[default]
    Auto,
    Text,
    Scan,
}

/// Settings for a linked extraction probe.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExtractionOptions {
    pub mode: OcrMode,
    /// Inclusive, one-based source page range.
    pub pages: Option<(usize, usize)>,
}

/// Observed progress from the extractor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Progress {
    ModelDownload {
        asset: &'static str,
        downloaded: u64,
        total: Option<u64>,
    },
    Started,
    Page {
        completed: usize,
        total: usize,
        page: usize,
        success: bool,
    },
    Enrichment {
        completed: usize,
        total: usize,
    },
    Finished,
}

/// Diagnostic output from the linked extraction probe.
pub struct Extracted {
    pub json: String,
    pub pages: usize,
}

#[derive(Debug)]
pub enum ExtractionError {
    InvalidPageRange,
    NotPdf,
    Engine(String),
    Incomplete { status: String, details: String },
}

impl fmt::Display for ExtractionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageRange => write!(formatter, "page range must be one-based and ordered"),
            Self::NotPdf => write!(formatter, "input must be a PDF"),
            Self::Engine(message) => formatter.write_str(message),
            Self::Incomplete { status, details } if details.is_empty() => {
                write!(formatter, "extraction ended with status {status}")
            }
            Self::Incomplete { status, details } => {
                write!(
                    formatter,
                    "extraction ended with status {status}: {details}"
                )
            }
        }
    }
}

impl std::error::Error for ExtractionError {}

/// Extract a PDF with the linked Docling CPU pipeline.
///
/// Docling resolves models from its own process configuration. Progress can
/// arrive from multiple extraction threads.
pub fn extract(
    input: &Path,
    options: ExtractionOptions,
    on_progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<Extracted, ExtractionError> {
    extract_profile(input, options, ModelProfile::default(), on_progress)
}

/// Run extraction in a process configured by
/// [`PreparedModels::configure_command`].
pub fn extract_profile(
    input: &Path,
    options: ExtractionOptions,
    profile: ModelProfile,
    on_progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<Extracted, ExtractionError> {
    if let Some((first, last)) = options.pages
        && (first == 0 || first > last)
    {
        return Err(ExtractionError::InvalidPageRange);
    }

    let source = SourceDocument::from_file(input)
        .map_err(|error| ExtractionError::Engine(error.to_string()))?;
    if source.format != InputFormat::Pdf {
        return Err(ExtractionError::NotPdf);
    }

    let pages = Arc::new(AtomicUsize::new(0));
    let observed_pages = Arc::clone(&pages);
    let mut converter = DocumentConverter::new()
        .no_ocr(options.mode == OcrMode::Text)
        .force_full_page_ocr(options.mode == OcrMode::Scan)
        .heading_hierarchy(true)
        .keep_picture_images(true)
        .do_formula_enrichment(profile == ModelProfile::Technical)
        .progress_callback(Arc::new(move |event| match event.kind {
            ProgressKind::DocumentStarted => on_progress(Progress::Started),
            ProgressKind::PageCompleted {
                page_no,
                completed_pages,
                total_pages,
                success,
            } => {
                observed_pages.fetch_max(completed_pages, Ordering::Relaxed);
                on_progress(Progress::Page {
                    completed: completed_pages,
                    total: total_pages,
                    page: page_no,
                    success,
                });
            }
            ProgressKind::DocumentCompleted { .. } => on_progress(Progress::Finished),
            _ => {}
        }));
    if let Some((first, last)) = options.pages {
        converter = converter.page_range(first, last);
    }

    let result = converter
        .convert(source)
        .map_err(|error| ExtractionError::Engine(error.to_string()))?;
    if result.status != ConversionStatus::Success {
        let details = result
            .errors
            .iter()
            .map(|error| error.describe())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ExtractionError::Incomplete {
            status: result.status.as_str().to_owned(),
            details,
        });
    }

    Ok(Extracted {
        json: result.document.export_to_json(),
        pages: pages.load(Ordering::Relaxed),
    })
}
