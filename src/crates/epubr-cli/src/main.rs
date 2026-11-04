use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Mutex;

use clap::Parser;
use epubr_core::{
    CancellationToken, ExtractionOptions, ModelConfig, ModelProfile, extract_profile,
};

mod common;

use common::{Mode, ProgressReporter, parse_pages};

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
        let options = ExtractionOptions {
            mode: args.mode.into(),
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
        .arg("--mode")
        .arg(args.mode.to_string());
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
