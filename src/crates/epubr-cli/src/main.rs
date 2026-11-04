use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Mutex;

use clap::{Parser, ValueEnum};
use epubr_core::{
    CancellationToken, ExtractionOptions, ModelConfig, ModelProfile, OcrMode, Progress,
    extract_profile,
};

#[derive(Parser)]
#[command(version, about = "Validate the linked Docling PDF extractor")]
struct Args {
    input: PathBuf,

    /// Model profile for PDF extraction.
    #[arg(
        long,
        value_name = "standard|technical",
        env = "EPUBR_MODEL",
        default_value_t
    )]
    model: ModelProfile,

    /// Use preinstalled model assets in this directory.
    #[arg(long, value_name = "DIR", env = "EPUBR_MODEL_DIR")]
    models: Option<PathBuf>,

    #[arg(long, value_enum, default_value = "auto")]
    mode: Mode,

    #[arg(long, value_parser = parse_pages)]
    pages: Option<(usize, usize)>,

    #[arg(long, hide = true)]
    worker: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Auto,
    Text,
    Scan,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Text => "text",
            Self::Scan => "scan",
        }
    }
}

#[derive(Default)]
struct ProgressReporter {
    downloads: HashMap<&'static str, u64>,
    pages: usize,
}

impl ProgressReporter {
    fn report(&mut self, progress: Progress) {
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
        }
    }
}

fn parse_pages(value: &str) -> Result<(usize, usize), String> {
    let (first, last) = value.split_once('-').unwrap_or((value, value));
    let first = first.parse().map_err(|_| "invalid first page")?;
    let last = last.parse().map_err(|_| "invalid last page")?;
    if first == 0 || first > last {
        return Err("expected a one-based page or ascending range".into());
    }
    Ok((first, last))
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let reporter = Mutex::new(ProgressReporter::default());
    if args.worker {
        let mode = match args.mode {
            Mode::Auto => OcrMode::Auto,
            Mode::Text => OcrMode::Text,
            Mode::Scan => OcrMode::Scan,
        };
        let options = ExtractionOptions {
            mode,
            pages: args.pages,
        };
        let result = extract_profile(&args.input, options, args.model, move |progress| {
            reporter.lock().unwrap().report(progress);
        })?;
        println!("{}", result.json);
        return Ok(ExitCode::SUCCESS);
    }

    let input = args.input.canonicalize()?;
    if !input.is_file() {
        return Err(format!("input is not a file: {}", input.display()).into());
    }
    let models = ModelConfig {
        profile: args.model,
        directory: args.models,
    };
    eprintln!("Preparing {} models", models.profile);
    let prepared = models.prepare(&CancellationToken::default(), |progress| {
        reporter.lock().unwrap().report(progress);
    })?;

    let mut worker = Command::new(std::env::current_exe()?);
    worker
        .arg("--worker")
        .arg("--model")
        .arg(args.model.to_string())
        .args(["--mode", args.mode.as_str()]);
    if let Some((first, last)) = args.pages {
        worker.arg("--pages").arg(format!("{first}-{last}"));
    }
    worker.arg("--").arg(input);
    prepared.configure_command(&mut worker);
    let status = worker.status()?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from))
}
