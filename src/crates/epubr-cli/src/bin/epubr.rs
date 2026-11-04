use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::sync::Arc;
use std::thread;

use clap::{Args, CommandFactory, Parser, Subcommand, error::ErrorKind};
use epubr_core::{
    CancellationToken, ConversionError, ConversionOptions, EXTRACTION_WORKER_FLAG,
    ExtractionWorker, ModelConfig, ModelProfile, convert_with_worker_setup, run_extraction_worker,
};

#[path = "../common.rs"]
mod common;
#[path = "../progress.rs"]
mod progress;

use common::{Mode, parse_pages};
use progress::Reporter;

const DOCLING_DEBUG: &str = "DOCLING_RS_DEBUG";

#[derive(Parser)]
#[command(version, about = "Convert technical PDFs to reflowable EPUB 3")]
struct Cli {
    /// Stream all progress and diagnostic logs to stderr.
    #[arg(long, global = true)]
    debug: bool,

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
        if std::env::var_os(DOCLING_DEBUG).as_deref() == Some("1".as_ref()) {
            enable_debug_logs();
        }
        return run_extraction_worker();
    }
    let cli = Cli::parse();
    let Command::Convert(args) = cli.command;
    if cli.debug && args.quiet {
        Cli::command()
            .error(
                ErrorKind::ArgumentConflict,
                "the argument '--quiet' cannot be used with '--debug'",
            )
            .exit();
    }
    if cli.debug {
        enable_debug_logs();
    }
    match convert(args, cli.debug) {
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

fn enable_debug_logs() {
    tracing_subscriber::fmt()
        .with_max_level(tracing_subscriber::filter::LevelFilter::TRACE)
        .without_time()
        .with_ansi(false)
        .with_writer(io::stderr)
        .init();
}

fn convert(args: ConvertArgs, debug: bool) -> Result<(), ConversionError> {
    let cancellation = Arc::new(CancellationToken::default());
    let interrupted = Arc::clone(&cancellation);
    ctrlc::set_handler(move || interrupted.cancel()).map_err(std::io::Error::other)?;

    let worker = std::env::current_exe()?;
    let models = ModelConfig {
        profile: args.model,
        directory: args.models,
    };
    let reporter = Reporter::new(debug, args.quiet);
    let result = thread::scope(|scope| {
        let stderr = if reporter.is_terminal() {
            let (reader, writer) = io::pipe()?;
            let reporter = &reporter;
            scope.spawn(move || {
                let mut reader = BufReader::new(reader);
                let mut line = Vec::new();
                while let Ok(1..) = reader.read_until(b'\n', &mut line) {
                    reporter.diagnostic(String::from_utf8_lossy(&line).trim());
                    line.clear();
                }
            });
            Some(writer)
        } else {
            None
        };
        let setup = |command: &mut ProcessCommand| {
            if debug {
                command.env(DOCLING_DEBUG, "1");
            } else if let Some(stderr) = &stderr {
                command.stderr(stderr.try_clone()?);
            } else {
                command.stderr(Stdio::null());
            }
            Ok(())
        };

        convert_with_worker_setup(
            &args.input,
            &args.output,
            ConversionOptions {
                mode: args.mode.into(),
                pages: args.pages,
            },
            &models,
            &ExtractionWorker {
                executable: &worker,
                setup: &setup,
            },
            &cancellation,
            |progress| reporter.report(progress, models.profile),
        )
    });
    reporter.clear();
    let report = match result {
        Err(_) if cancellation.is_cancelled() => return Err(ConversionError::Cancelled),
        result => result?,
    };
    if !args.quiet {
        eprintln!("Wrote {}", args.output.display());
    }
    for warning in report.warnings {
        eprintln!("Warning: {warning}");
    }
    Ok(())
}
