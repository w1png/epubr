use std::collections::HashMap;

use clap::ValueEnum;
use epubr_core::{OcrMode, Progress};

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Mode {
    Auto,
    Text,
    Scan,
}

impl From<Mode> for OcrMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Auto => Self::Auto,
            Mode::Text => Self::Text,
            Mode::Scan => Self::Scan,
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Scan => "scan",
        })
    }
}

#[derive(Default)]
pub(crate) struct ProgressReporter {
    downloads: HashMap<&'static str, u64>,
    pages: usize,
}

impl ProgressReporter {
    pub(crate) fn report(&mut self, progress: Progress) {
        const DOWNLOAD_INTERVAL: u64 = 16 * 1024 * 1024;
        const PAGE_INTERVAL: usize = 10;

        match progress {
            Progress::ModelDownload {
                asset,
                downloaded,
                total,
            } => {
                let previous = self.downloads.get(asset).copied();
                let finished = total == Some(downloaded) && previous != Some(downloaded);
                let advanced = previous
                    .is_none_or(|bytes| downloaded.saturating_sub(bytes) >= DOWNLOAD_INTERVAL);
                if finished || advanced {
                    self.downloads.insert(asset, downloaded);
                    match total {
                        Some(total) => eprintln!("Downloading {asset}: {downloaded}/{total} bytes"),
                        None => eprintln!("Downloading {asset}: {downloaded} bytes"),
                    }
                }
            }
            Progress::Started => eprintln!("Extracting PDF"),
            Progress::Page {
                completed,
                total,
                page,
                success,
            } => {
                if !success {
                    eprintln!("Page {page} extraction failed");
                }
                let due = self.pages == 0
                    || completed == total
                    || completed >= self.pages + PAGE_INTERVAL;
                if completed > self.pages && due {
                    self.pages = completed;
                    eprintln!("Extracting PDF: {completed}/{total} pages processed");
                }
            }
            Progress::Finished => eprintln!("Extraction finished"),
            Progress::Enrichment { completed, total } => {
                if completed == total || completed.is_multiple_of(PAGE_INTERVAL) {
                    eprintln!("Decoding code and formulas: {completed}/{total} regions processed");
                }
            }
        }
    }
}

pub(crate) fn parse_pages(value: &str) -> Result<(usize, usize), String> {
    let (first, last) = value.split_once('-').unwrap_or((value, value));
    let first = first.parse().map_err(|_| "invalid first page")?;
    let last = last.parse().map_err(|_| "invalid last page")?;
    if first == 0 || first > last {
        return Err("expected a one-based page or ascending range".into());
    }
    Ok((first, last))
}
