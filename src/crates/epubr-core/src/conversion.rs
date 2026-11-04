use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{OcrMode, Progress};

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
}

#[derive(Debug)]
pub struct ConversionReport {
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum ConversionError {
    InvalidInput,
    InvalidPageRange,
    DestinationExists,
    Cancelled,
    NoContent,
    Io(io::Error),
    NotImplemented,
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => f.write_str("input is not a readable PDF"),
            Self::InvalidPageRange => f.write_str("page range is invalid for the input PDF"),
            Self::DestinationExists => f.write_str("destination already exists"),
            Self::Cancelled => f.write_str("conversion cancelled"),
            Self::NoContent => f.write_str("PDF has no convertible content"),
            Self::Io(error) => error.fmt(f),
            Self::NotImplemented => f.write_str("PDF-to-EPUB conversion is not implemented"),
        }
    }
}

impl std::error::Error for ConversionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Convert a PDF into a new EPUB file.
///
/// A successful conversion publishes a complete EPUB at `output`. An
/// existing destination is preserved, and failure or cancellation leaves
/// no partial output. Progress can be delivered from extraction threads.
pub fn convert(
    _input: &Path,
    _output: &Path,
    _options: ConversionOptions,
    _cancellation: &CancellationToken,
    _on_progress: impl Fn(Progress) + Sync,
) -> Result<ConversionReport, ConversionError> {
    Err(ConversionError::NotImplemented)
}
