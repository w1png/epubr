use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use epubr_core::{ExtractionOptions, OcrMode, extract};

#[derive(Parser)]
#[command(version, about = "Validate the linked Docling PDF extractor")]
struct Args {
    input: PathBuf,

    #[arg(long)]
    models: PathBuf,

    #[arg(long, value_enum, default_value = "auto")]
    mode: Mode,

    #[arg(long, value_parser = parse_pages)]
    pages: Option<(usize, usize)>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Auto,
    Text,
    Scan,
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
    let args = Args::parse();
    let result = run(args);
    match result {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<String, Box<dyn std::error::Error>> {
    let input = args.input.canonicalize()?;
    std::env::set_current_dir(args.models)?;
    let mode = match args.mode {
        Mode::Auto => OcrMode::Auto,
        Mode::Text => OcrMode::Text,
        Mode::Scan => OcrMode::Scan,
    };
    let options = ExtractionOptions {
        mode,
        pages: args.pages,
    };
    let result = extract(&input, options, |progress| {
        eprintln!("{progress:?}");
    })?;
    Ok(result.json)
}
