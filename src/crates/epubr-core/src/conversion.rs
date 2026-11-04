use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::NamedTempFile;
use thiserror::Error;

use crate::extraction::extract_for_conversion;
use crate::{ExtractionError, ExtractionOptions, ModelConfig, ModelError, OcrMode, Progress};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConversionOptions {
    pub mode: OcrMode,
    /// Inclusive, one-based source page range. Defaults to the full document.
    pub pages: Option<(usize, usize)>,
}

#[derive(Debug, Default)]
pub struct CancellationToken(AtomicBool);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn check(&self) -> Result<(), ConversionError> {
        if self.is_cancelled() {
            return Err(ConversionError::Cancelled);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ConversionReport {
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("input is not a readable PDF")]
    InvalidInput,
    #[error("page range is invalid for the input PDF")]
    InvalidPageRange,
    #[error("destination already exists")]
    DestinationExists,
    #[error("conversion cancelled")]
    Cancelled,
    #[error("PDF has no convertible content")]
    NoContent,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("could not prepare Docling models: {0}")]
    Models(#[source] ModelError),
    #[error("Docling extraction failed: {0}")]
    Extraction(#[source] ExtractionError),
    #[error("invalid Docling document: {0}")]
    Document(#[source] serde_json::Error),
}

/// Convert a PDF into a new EPUB using the environment's model configuration.
///
/// A successful conversion publishes a complete EPUB at `output`. An
/// existing destination is preserved, and failure or cancellation leaves
/// no partial output.
pub fn convert(
    input: &Path,
    output: &Path,
    options: ConversionOptions,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress) + Sync,
) -> Result<ConversionReport, ConversionError> {
    convert_inner(
        input,
        output,
        options,
        None,
        None,
        cancellation,
        on_progress,
    )
}

/// Convert using a caller-selected model profile and model directory.
pub fn convert_with_models(
    input: &Path,
    output: &Path,
    options: ConversionOptions,
    models: &ModelConfig,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress) + Sync,
) -> Result<ConversionReport, ConversionError> {
    convert_inner(
        input,
        output,
        options,
        Some(models),
        None,
        cancellation,
        on_progress,
    )
}

/// Convert using a host executable that implements the extraction worker protocol.
pub fn convert_with_worker(
    input: &Path,
    output: &Path,
    options: ConversionOptions,
    models: &ModelConfig,
    worker: &Path,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress) + Sync,
) -> Result<ConversionReport, ConversionError> {
    convert_inner(
        input,
        output,
        options,
        Some(models),
        Some(worker),
        cancellation,
        on_progress,
    )
}

fn convert_inner(
    input: &Path,
    output: &Path,
    options: ConversionOptions,
    models: Option<&ModelConfig>,
    worker: Option<&Path>,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress) + Sync,
) -> Result<ConversionReport, ConversionError> {
    cancellation.check()?;
    match output.symlink_metadata() {
        Ok(_) => return Err(ConversionError::DestinationExists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let pdf = read_pdf(input)?;
    let page_count = pdf.get_pages().len();
    if let Some((first, last)) = options.pages
        && (first == 0 || first > last || last > page_count)
    {
        return Err(ConversionError::InvalidPageRange);
    }
    if page_count == 0 {
        return Err(ConversionError::NoContent);
    }

    let env_models;
    let models = match models {
        Some(models) => models,
        None => {
            env_models = ModelConfig::from_env().map_err(ConversionError::Models)?;
            &env_models
        }
    };
    cancellation.check()?;
    on_progress(Progress::Started);
    let extracted = extract_for_conversion(
        input,
        ExtractionOptions {
            mode: options.mode,
            pages: options.pages,
        },
        models,
        worker,
        cancellation,
        &on_progress,
    )?;
    cancellation.check()?;
    let book =
        crate::book::normalize(&extracted.json, &pdf, input).map_err(ConversionError::Document)?;
    if book.blocks.is_empty() {
        return Err(ConversionError::NoContent);
    }

    cancellation.check()?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged = NamedTempFile::new_in(parent)?;
    crate::epub::write(staged.as_file_mut(), &book)?;
    staged.as_file().sync_all()?;
    cancellation.check()?;
    staged.persist_noclobber(output).map_err(|failure| {
        if failure.error.kind() == io::ErrorKind::AlreadyExists {
            ConversionError::DestinationExists
        } else {
            ConversionError::Io(failure.error)
        }
    })?;
    on_progress(Progress::Finished);
    Ok(ConversionReport {
        warnings: book.warnings,
    })
}

fn read_pdf(input: &Path) -> Result<lopdf::Document, ConversionError> {
    if !input.is_file() {
        return Err(ConversionError::InvalidInput);
    }
    let mut file = File::open(input).map_err(|_| ConversionError::InvalidInput)?;
    let mut prefix = Vec::new();
    file.by_ref()
        .take(1024)
        .read_to_end(&mut prefix)
        .map_err(|_| ConversionError::InvalidInput)?;
    if !prefix.windows(5).any(|window| window == b"%PDF-") {
        return Err(ConversionError::InvalidInput);
    }

    file.rewind().map_err(|_| ConversionError::InvalidInput)?;
    let pdf = lopdf::Document::load_from(file).map_err(|_| ConversionError::InvalidInput)?;
    pdf.catalog().map_err(|_| ConversionError::InvalidInput)?;
    Ok(pdf)
}
