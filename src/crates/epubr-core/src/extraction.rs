mod enrichment;

use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    CancellationToken, ConversionError, ConversionSettings, Extracted, ExtractionError,
    ExtractionOptions, MathMlMode, ModelConfig, ModelError, ModelProfile, OcrMode, PreparedModels,
    Progress, extract_profile,
};

const WORKER_NAME: &str = "epubr-extract-worker";
pub const EXTRACTION_WORKER_FLAG: &str = "--epubr-extraction-worker";

pub struct ExtractionWorker<'a> {
    pub executable: &'a Path,
    /// Configure stderr and environment for each worker process.
    pub setup: &'a dyn Fn(&mut Command) -> io::Result<()>,
}

impl ExtractionWorker<'_> {
    fn command(&self, models: &PreparedModels) -> io::Result<Command> {
        let mut command = Command::new(self.executable);
        models.configure_command(&mut command);
        (self.setup)(&mut command)?;
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            command.process_group(0);
        }
        Ok(command)
    }
}

#[derive(Deserialize, Serialize)]
enum WorkerEvent {
    Page {
        completed: usize,
        total: usize,
        page: usize,
        success: bool,
    },
    Finished {
        pages: usize,
    },
    Failed {
        error: String,
    },
}

enum WorkerMessage {
    Progress(Progress),
    Complete(Extracted),
}

pub(crate) fn extract_for_conversion(
    input: &Path,
    pdf: &lopdf::Document,
    settings: &ConversionSettings,
    models: &ModelConfig,
    worker: Option<&ExtractionWorker<'_>>,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress) + Sync,
) -> Result<(Value, Vec<String>), ConversionError> {
    let options = settings.options;
    let input = input.canonicalize().map_err(ConversionError::Io)?;
    let executable = match worker {
        Some(worker) => worker.executable.canonicalize()?,
        None => worker_path()?,
    };
    let worker = ExtractionWorker {
        executable: &executable,
        setup: worker.map_or(&|_| Ok(()), |worker| worker.setup),
    };
    let prepared = models
        .prepare(cancellation, &on_progress)
        .map_err(|error| match error {
            ModelError::Cancelled => ConversionError::Cancelled,
            error => ConversionError::Models(error),
        })?;
    let mut command = worker.command(&prepared)?;
    let mode = match options.mode {
        OcrMode::Auto => "auto",
        OcrMode::Text => "text",
        OcrMode::Scan => "scan",
    };
    command.arg(EXTRACTION_WORKER_FLAG).arg(&input).arg(mode);
    if let Some((first, last)) = options.pages {
        command.arg(first.to_string()).arg(last.to_string());
    }
    cancellation.check()?;
    let extracted = receive_progress(&mut command, cancellation, &on_progress)?;
    let mut document: Value =
        serde_json::from_str(&extracted.json).map_err(ConversionError::Document)?;
    crate::book::locate_placeholders(&mut document, &input);
    let warnings = if models.profile == ModelProfile::Technical {
        let decode_formulas = settings.math_ml != MathMlMode::Disable;
        crate::book::recover_source_regions(&mut document, pdf, &input, decode_formulas);
        enrichment::enrich(
            &mut document,
            &input,
            &worker,
            &prepared,
            decode_formulas,
            cancellation,
            on_progress,
        )?
    } else {
        Vec::new()
    };
    crate::book::repair_symbols(&mut document, pdf);
    Ok((document, warnings))
}

fn worker_path() -> Result<PathBuf, ConversionError> {
    if let Some(path) = env::var_os("EPUBR_EXTRACTION_WORKER") {
        return PathBuf::from(path)
            .canonicalize()
            .map_err(ConversionError::Io);
    }
    let executable = env::current_exe().map_err(ConversionError::Io)?;
    let directory = executable
        .parent()
        .ok_or_else(|| extraction_error("cannot locate the Docling worker"))?;
    let worker_name = format!("{WORKER_NAME}{}", env::consts::EXE_SUFFIX);
    // Cargo integration tests run from target/<profile>/deps while the
    // companion executable lives in target/<profile>.
    let directory = if directory.ends_with("deps") {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    };
    let worker = directory.join(worker_name);
    if !worker.is_file() {
        return Err(extraction_error(format!(
            "Docling worker is missing at {}. Set EPUBR_EXTRACTION_WORKER to its executable path.",
            worker.display()
        )));
    }
    Ok(worker)
}

fn receive_progress(
    command: &mut Command,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress),
) -> Result<Extracted, ConversionError> {
    thread::scope(|scope| {
        let mut worker = Worker(command.spawn().map_err(ConversionError::Io)?);
        let stdout = worker.0.stdout.take().expect("piped worker stdout");
        let (sender, receiver) = mpsc::sync_channel(8);
        scope.spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = match read_message(&mut reader) {
                    Ok(Some(message)) => Ok(message),
                    Ok(None) => break,
                    Err(error) => Err(error),
                };
                let finished = !matches!(message, Ok(WorkerMessage::Progress(_)));
                if sender.send(message).is_err() || finished {
                    break;
                }
            }
        });

        let extracted = loop {
            cancellation.check()?;
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(WorkerMessage::Progress(progress))) => on_progress(progress),
                Ok(Ok(WorkerMessage::Complete(extracted))) => break Some(extracted),
                Ok(Err(error)) => return Err(error),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break None,
            }
        };
        cancellation.check()?;
        let status = worker.0.wait().map_err(ConversionError::Io)?;
        match (status.success(), extracted) {
            (true, Some(extracted)) => Ok(extracted),
            _ => Err(extraction_error(format!(
                "Docling worker exited without a completed extraction ({status})"
            ))),
        }
    })
}

fn read_message(reader: &mut impl BufRead) -> Result<Option<WorkerMessage>, ConversionError> {
    let mut line = String::new();
    if reader.read_line(&mut line).map_err(ConversionError::Io)? == 0 {
        return Ok(None);
    }
    let event = serde_json::from_str(&line)
        .map_err(|error| extraction_error(format!("invalid Docling worker event: {error}")))?;
    let message = match event {
        WorkerEvent::Page {
            completed,
            total,
            page,
            success,
        } => WorkerMessage::Progress(Progress::Page {
            completed,
            total,
            page,
            success,
        }),
        WorkerEvent::Finished { pages } => {
            let mut json = String::new();
            reader
                .read_to_string(&mut json)
                .map_err(ConversionError::Io)?;
            WorkerMessage::Complete(Extracted { json, pages })
        }
        WorkerEvent::Failed { error } => return Err(extraction_error(error)),
    };
    Ok(Some(message))
}

struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn extraction_error(error: impl Into<String>) -> ConversionError {
    ConversionError::Extraction(ExtractionError::Engine(error.into()))
}

/// Run the private extraction protocol in a dedicated Docling process.
#[doc(hidden)]
pub fn run_extraction_worker() -> ExitCode {
    match worker_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = send_event(&WorkerEvent::Failed {
                error: error.to_string(),
            });
            ExitCode::FAILURE
        }
    }
}

fn worker_main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() != Some(EXTRACTION_WORKER_FLAG.as_ref()) {
        return Err("invalid extraction worker invocation".into());
    }
    let input = args.next().ok_or("missing worker input")?;
    if input == enrichment::FLAG {
        return enrichment::worker_main(&args.next().ok_or("missing enrichment input")?);
    }
    let mode = match args.next().as_ref().and_then(|value| value.to_str()) {
        Some("auto") => OcrMode::Auto,
        Some("text") => OcrMode::Text,
        Some("scan") => OcrMode::Scan,
        _ => return Err("invalid worker OCR mode".into()),
    };
    let pages = match (args.next(), args.next()) {
        (None, None) => None,
        (Some(first), Some(last)) => Some((
            first.to_str().ok_or("invalid first worker page")?.parse()?,
            last.to_str().ok_or("invalid last worker page")?.parse()?,
        )),
        _ => return Err("incomplete worker page range".into()),
    };

    // The parent keeps its end of stdin open for the life of the job. EOF
    // therefore means the parent disappeared, including between page events.
    thread::spawn(|| {
        let _ = io::stdin().read(&mut [0]);
        std::process::exit(1);
    });

    let extracted = extract_profile(
        Path::new(&input),
        ExtractionOptions { mode, pages },
        ModelProfile::Standard,
        |progress| {
            if let Progress::Page {
                completed,
                total,
                page,
                success,
            } = progress
                && send_event(&WorkerEvent::Page {
                    completed,
                    total,
                    page,
                    success,
                })
                .is_err()
            {
                std::process::exit(1);
            }
        },
    )?;
    let mut stdout = io::stdout().lock();
    write_event(
        &mut stdout,
        &WorkerEvent::Finished {
            pages: extracted.pages,
        },
    )?;
    stdout.write_all(extracted.json.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn send_event(event: &WorkerEvent) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    write_event(&mut stdout, event)?;
    stdout.flush()
}

fn write_event(stdout: &mut impl Write, event: &WorkerEvent) -> io::Result<()> {
    serde_json::to_writer(&mut *stdout, event)?;
    stdout.write_all(b"\n")
}
