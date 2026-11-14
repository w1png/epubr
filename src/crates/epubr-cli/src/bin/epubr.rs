use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, ExitCode, Stdio};
use std::sync::Arc;
use std::thread;

use clap::{
    Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum,
    error::{ContextKind, ContextValue, ErrorKind},
};
use epubr_core::{
    CancellationToken, ConversionError, ConversionOptions, ConversionSettings,
    EXTRACTION_WORKER_FLAG, ExtractionWorker, MetadataOverrides, ModelConfig, ModelProfile,
    OptimizationTarget, convert_with_worker_metadata, is_language_tag, optimize_epub,
    run_extraction_worker,
};

#[path = "../common.rs"]
mod common;
#[path = "../progress.rs"]
mod progress;

use common::{Mode, parse_pages};
use progress::Reporter;

const DOCLING_DEBUG: &str = "DOCLING_RS_DEBUG";

#[derive(Parser)]
#[command(
    version,
    about = "Convert technical PDFs and optimize EPUBs for e-readers",
    disable_help_subcommand = true
)]
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
    /// Optimize an existing EPUB for a target e-reader.
    Optimize(OptimizeArgs),
}

#[derive(Args)]
struct OptimizeArgs {
    /// Existing EPUB to optimize.
    #[arg(long, value_name = "INPUT")]
    input: PathBuf,

    /// Output path. Defaults to <input-stem>-optimized.epub beside the input.
    #[arg(short = 'o', long, value_name = "OUTPUT")]
    output: Option<PathBuf>,

    /// Select the target e-reader.
    #[arg(long = "optimizeFor", alias = "optimize-for", value_enum)]
    optimize_for: ReaderTarget,

    /// Suppress the completion message.
    #[arg(long)]
    quiet: bool,
}

#[derive(Args)]
struct ConvertArgs {
    /// PDF to convert.
    #[arg(long, value_name = "INPUT")]
    input: PathBuf,

    /// Destination EPUB path.
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

    /// Optimize images and long sections for a target e-reader.
    #[arg(long = "optimizeFor", alias = "optimize-for", value_enum)]
    optimize_for: Option<ReaderTarget>,

    /// Suppress progress messages.
    #[arg(long)]
    quiet: bool,

    #[command(flatten, next_help_heading = "Book metadata")]
    metadata: MetadataArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReaderTarget {
    #[value(name = "xteink_x3")]
    XteinkX3,
    #[value(name = "xteink_x4", alias = "xteink_x4_classic")]
    XteinkX4,
    #[value(name = "xteink_x4_pro")]
    XteinkX4Pro,
    #[value(name = "seeed_studio_sticky", alias = "seeed_sticky")]
    SeeedStudioSticky,
}

impl From<ReaderTarget> for OptimizationTarget {
    fn from(value: ReaderTarget) -> Self {
        match value {
            ReaderTarget::XteinkX3 => Self::XteinkX3,
            ReaderTarget::XteinkX4 => Self::XteinkX4,
            ReaderTarget::XteinkX4Pro => Self::XteinkX4Pro,
            ReaderTarget::SeeedStudioSticky => Self::SeeedStudioSticky,
        }
    }
}

#[derive(Args)]
struct MetadataArgs {
    /// Override the book title.
    #[arg(long, value_parser = parse_metadata_text)]
    title: Option<String>,

    /// Override the book author. Repeat for multiple authors.
    #[arg(long = "author", value_name = "NAME", value_parser = parse_metadata_text)]
    authors: Vec<String>,

    /// Override the publication year.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..=9999))]
    year: Option<u16>,

    /// Override the book edition.
    #[arg(long, value_parser = parse_metadata_text)]
    edition: Option<String>,

    /// Override the book language using a tag such as en or pt-BR.
    #[arg(long, value_name = "TAG", value_parser = parse_language)]
    language: Option<String>,

    /// Override the publisher name.
    #[arg(long, value_parser = parse_metadata_text)]
    publisher: Option<String>,

    /// Override the book description.
    #[arg(long, value_parser = parse_metadata_text)]
    description: Option<String>,
}

fn parse_metadata_text(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Err("expected non-blank text");
    }
    if value.chars().any(|character| {
        (character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
            || matches!(character, '\u{fffe}' | '\u{ffff}')
    }) {
        return Err("metadata contains an unsupported character");
    }
    Ok(value.to_owned())
}

fn parse_language(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    let primary = value.split('-').next().unwrap_or_default();
    let has_special_prefix = value.contains('-')
        && (primary.eq_ignore_ascii_case("i") || primary.eq_ignore_ascii_case("x"));
    if is_language_tag(value)
        && primary.bytes().all(|byte| byte.is_ascii_alphabetic())
        && (primary.len() >= 2 || has_special_prefix)
    {
        Ok(value.to_owned())
    } else {
        Err("expected a language tag such as en or pt-BR")
    }
}

fn parse_cli() -> Cli {
    let mut command = Cli::command();
    let mut matches = match command.try_get_matches_from_mut(std::env::args_os()) {
        Ok(matches) => matches,
        Err(error) => {
            // The only global option accepted before a subcommand is --debug.
            let subcommand = std::env::args_os().skip(1).find(|arg| arg != "--debug");
            if error.kind() == ErrorKind::MissingRequiredArgument
                && let Some(ContextValue::Strings(missing)) = error.get(ContextKind::InvalidArg)
                && let Some(help) = subcommand.and_then(|name| command.find_subcommand_mut(name))
            {
                let _ = writeln!(
                    io::stderr().lock(),
                    "error: the following required arguments were not provided:\n  {}\n\n{}",
                    missing.join("\n  "),
                    help.render_help(),
                );
                std::process::exit(error.exit_code());
            }
            error.exit();
        }
    };
    Cli::from_arg_matches_mut(&mut matches)
        .unwrap_or_else(|error| error.format(&mut command).exit())
}

fn main() -> ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some(EXTRACTION_WORKER_FLAG.as_ref()) {
        if std::env::var_os(DOCLING_DEBUG).as_deref() == Some("1".as_ref()) {
            enable_debug_logs();
        }
        return run_extraction_worker();
    }
    let cli = parse_cli();
    let quiet = match &cli.command {
        Command::Convert(args) => args.quiet,
        Command::Optimize(args) => args.quiet,
    };
    if cli.debug && quiet {
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
    match cli.command {
        Command::Convert(args) => match convert(args, cli.debug) {
            Ok(()) => ExitCode::SUCCESS,
            Err(ConversionError::Cancelled) => {
                eprintln!("Conversion cancelled");
                ExitCode::from(130)
            }
            Err(error) => {
                eprintln!("Conversion failed: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Optimize(args) => optimize(args),
    }
}

fn optimize(args: OptimizeArgs) -> ExitCode {
    let output = args.output.or_else(|| {
        let mut filename = args.input.file_stem()?.to_os_string();
        filename.push("-optimized.epub");
        Some(args.input.with_file_name(filename))
    });
    let Some(output) = output else {
        eprintln!("Optimization failed: input path has no filename");
        return ExitCode::FAILURE;
    };
    if let Err(error) = optimize_epub(&args.input, &output, args.optimize_for.into()) {
        eprintln!("Optimization failed: {error}");
        return ExitCode::FAILURE;
    }
    if !args.quiet {
        eprintln!("Wrote {}", output.display());
    }
    ExitCode::SUCCESS
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

        convert_with_worker_metadata(
            &args.input,
            &args.output,
            ConversionSettings {
                options: ConversionOptions {
                    mode: args.mode.into(),
                    pages: args.pages,
                },
                optimization: args.optimize_for.map(OptimizationTarget::from),
                metadata: MetadataOverrides {
                    title: args.metadata.title,
                    authors: args.metadata.authors,
                    year: args.metadata.year,
                    edition: args.metadata.edition,
                    language: args.metadata.language,
                    publisher: args.metadata.publisher,
                    description: args.metadata.description,
                },
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
