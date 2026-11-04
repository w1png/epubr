use std::fmt::Arguments;
use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use epubr_core::{ModelProfile, Progress};
use unicode_width::UnicodeWidthChar;

pub(crate) enum Reporter {
    Debug,
    Terminal(Mutex<ProgressLine>),
    Plain,
}

impl Reporter {
    pub(crate) fn new(debug: bool, quiet: bool) -> Self {
        if debug {
            Self::Debug
        } else if !quiet
            && io::stderr().is_terminal()
            && std::env::var_os("TERM").as_deref() != Some("dumb".as_ref())
        {
            Self::Terminal(Mutex::new(ProgressLine::default()))
        } else {
            Self::Plain
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal(_))
    }

    pub(crate) fn report(&self, progress: Progress, profile: ModelProfile) {
        match progress {
            Progress::Started => self.show(None, format_args!("Preparing {profile} models")),
            Progress::ModelDownload {
                asset,
                downloaded,
                total,
            } => {
                let position = total.map(|total| (downloaded, total));
                let message = format_args!("Downloading {asset}: {downloaded} bytes");
                if let Self::Terminal(bar) = self {
                    let mut bar = bar.lock().unwrap();
                    if downloaded == 0
                        || total == Some(downloaded)
                        || bar
                            .drawn_at
                            .is_none_or(|time| time.elapsed() >= Duration::from_millis(80))
                    {
                        bar.position = position;
                        bar.draw(message);
                    }
                } else if let Self::Debug = self {
                    match total {
                        Some(total) => eprintln!("Downloading {asset}: {downloaded}/{total} bytes"),
                        None => eprintln!("{message}"),
                    }
                }
            }
            Progress::Page {
                completed,
                total,
                page,
                success,
            } => {
                let position = Some((completed as u64, total as u64));
                self.show(
                    position,
                    format_args!("Extracting PDF: {completed}/{total} pages processed"),
                );
                if !success {
                    self.show(position, format_args!("Page {page} extraction failed"));
                }
            }
            Progress::Enrichment { completed, total } => self.show(
                Some((completed as u64, total as u64)),
                format_args!("Decoding code and formulas: {completed}/{total} regions processed"),
            ),
            Progress::Finished => {}
        }
    }

    fn show(&self, position: Option<(u64, u64)>, message: Arguments<'_>) {
        match self {
            Self::Debug => eprintln!("{message}"),
            Self::Terminal(bar) => {
                let mut bar = bar.lock().unwrap();
                bar.position = position;
                bar.draw(message);
            }
            Self::Plain => {}
        }
    }

    pub(crate) fn diagnostic(&self, line: &str) {
        if let Self::Terminal(bar) = self
            && !line.is_empty()
        {
            let plain = anstream::adapter::strip_str(line);
            bar.lock().unwrap().draw(format_args!("{plain}"));
        }
    }

    pub(crate) fn clear(&self) {
        if let Self::Terminal(bar) = self
            && bar.lock().unwrap().drawn_at.is_some()
        {
            let _ = write!(io::stderr(), "\r\x1b[2K");
        }
    }
}

#[derive(Default)]
pub(crate) struct ProgressLine {
    position: Option<(u64, u64)>,
    drawn_at: Option<Instant>,
}

impl ProgressLine {
    fn draw(&mut self, message: Arguments<'_>) {
        let stderr = io::stderr();
        let columns = terminal_columns(&stderr)
            .filter(|width| *width > 0)
            .unwrap_or(80);
        let mut line = String::new();
        if columns >= 24 {
            let bar_width = (columns / 4).min(24);
            let fraction = self.position.and_then(|(done, total)| {
                (total > 0).then(|| done.min(total) as f64 / total as f64)
            });
            line = match fraction {
                Some(fraction) => {
                    let filled = (bar_width as f64 * fraction).floor() as usize;
                    format!(
                        "[{}{}] {:>3}% ",
                        "━".repeat(filled),
                        "─".repeat(bar_width - filled),
                        (fraction * 100.0) as usize
                    )
                }
                None => format!("[{}]      ", "·".repeat(bar_width)),
            };
        }
        let mut space = columns.saturating_sub(line.chars().count() + 1);
        for character in message.to_string().chars().filter(|c| !c.is_control()) {
            let width = character.width().unwrap_or(0);
            if width > space {
                break;
            }
            line.push(character);
            space -= width;
        }
        let _ = write!(stderr.lock(), "\r\x1b[2K{line}");
        self.drawn_at = Some(Instant::now());
    }
}

#[cfg(unix)]
fn terminal_columns(stderr: &io::Stderr) -> Option<usize> {
    let size = rustix::termios::tcgetwinsize(stderr).ok()?;
    Some(usize::from(size.ws_col))
}

#[cfg(windows)]
fn terminal_columns(stderr: &io::Stderr) -> Option<usize> {
    use std::os::windows::io::AsHandle;
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo,
    };

    let mut size = CONSOLE_SCREEN_BUFFER_INFO::default();
    // The borrowed handle stays open during the call and size is writable.
    let result =
        unsafe { GetConsoleScreenBufferInfo(stderr.as_handle().as_raw_handle(), &mut size) };
    (result != 0)
        .then(|| (i32::from(size.srWindow.Right) - i32::from(size.srWindow.Left) + 1) as usize)
}

#[cfg(not(any(unix, windows)))]
fn terminal_columns(_: &io::Stderr) -> Option<usize> {
    None
}
