use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::{Args, Parser, Subcommand};
use epubr_core::{
    CancellationToken, ConversionError, ConversionOptions, EXTRACTION_WORKER_FLAG, ModelConfig,
    ModelProfile, Progress, convert_with_worker, run_extraction_worker,
};

#[path = "../common.rs"]
mod common;

use common::{Mode, ProgressReporter, parse_pages};

#[derive(Parser)]
#[command(version, about = "Convert technical PDFs to reflowable EPUB 3")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a PDF to EPUB.
    Convert(ConvertArgs),
}

#[derive(Args)]
struct ConvertArgs {
    #[arg(value_name = "INPUT")]
    input: PathBuf,

    #[arg(short = 'o', long, value_name = "OUTPUT")]
    output: PathBuf,

    #[arg(long, value_enum, default_value = "auto")]
    mode: Mode,

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

    /// Convert a one-based page or inclusive page range.
    #[arg(long, value_parser = parse_pages)]
    pages: Option<(usize, usize)>,

    /// Suppress progress messages.
    #[arg(long)]
    quiet: bool,
}

fn main() -> ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some(EXTRACTION_WORKER_FLAG.as_ref()) {
        return run_extraction_worker();
    }
    let Command::Convert(args) = Cli::parse().command;
    match convert(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(ConversionError::Cancelled) => {
            eprintln!("Conversion cancelled");
            ExitCode::from(130)
        }
        Err(error) => {
            eprintln!("Conversion failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn convert(args: ConvertArgs) -> Result<(), ConversionError> {
    let cancellation = Arc::new(CancellationToken::default());
    let interrupted = Arc::clone(&cancellation);
    ctrlc::set_handler(move || interrupted.cancel()).map_err(std::io::Error::other)?;

    let worker = std::env::current_exe()?;
    let models = ModelConfig {
        profile: args.model,
        directory: args.models,
    };
    let reporter = Mutex::new(ProgressReporter::default());
    let result = convert_with_worker(
        &args.input,
        &args.output,
        ConversionOptions {
            mode: args.mode.into(),
            pages: args.pages,
        },
        &models,
        &worker,
        &cancellation,
        |progress| {
            if !args.quiet {
                match progress {
                    Progress::Started => {
                        eprintln!("Preparing {} models", models.profile);
                    }
                    Progress::Finished => eprintln!("Wrote {}", args.output.display()),
                    progress => reporter.lock().unwrap().report(progress),
                }
            }
        },
    );
    let report = match result {
        Err(_) if cancellation.is_cancelled() => return Err(ConversionError::Cancelled),
        result => result?,
    };
    for warning in report.warnings {
        eprintln!("Warning: {warning}");
    }
    Ok(())
}
