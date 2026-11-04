use clap::ValueEnum;
use epubr_core::OcrMode;

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

pub(crate) fn parse_pages(value: &str) -> Result<(usize, usize), String> {
    let (first, last) = value.split_once('-').unwrap_or((value, value));
    let first = first.parse().map_err(|_| "invalid first page")?;
    let last = last.parse().map_err(|_| "invalid last page")?;
    if first == 0 || first > last {
        return Err("expected a one-based page or ascending range".into());
    }
    Ok((first, last))
}
