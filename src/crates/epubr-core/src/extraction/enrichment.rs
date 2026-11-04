use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use docling_pdf::enrich::{CodeFormula, CodeFormulaKind, extract_code_language};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{EXTRACTION_WORKER_FLAG, Worker};
use crate::{CancellationToken, ConversionError, PreparedModels, Progress, book::SourceCrops};

pub(super) const FLAG: &str = "--enrich";
const REGION_BUDGET: Duration = Duration::from_secs(12);
const DOCUMENT_BUDGET: Duration = Duration::from_secs(180);

#[derive(Deserialize, Serialize)]
enum Response {
    Decoded(String),
    Unreadable,
    Unavailable,
}

pub(super) fn enrich(
    document: &mut Value,
    input: &Path,
    worker: &Path,
    models: &PreparedModels,
    cancellation: &CancellationToken,
    on_progress: impl Fn(Progress),
) -> Result<Vec<String>, ConversionError> {
    let Some(texts) = document["texts"].as_array_mut() else {
        return Ok(Vec::new());
    };
    let regions = texts.iter_mut().filter(|item| {
        matches!(item["label"].as_str(), Some("formula" | "code"))
            && item["epubr_source_text"] != true
            && item["prov"]
                .as_array()
                .is_some_and(|regions| !regions.is_empty())
    });
    let mut regions: Vec<_> = regions.collect();
    // Source-backed listings already retain indentation. Give undecoded
    // formulas first use of the remaining model budget.
    regions.sort_by_key(|item| item["label"] != "formula");
    let total = regions.len();
    if total == 0 {
        return Ok(Vec::new());
    }

    on_progress(Progress::Enrichment {
        completed: 0,
        total,
    });
    let deadline = Instant::now() + DOCUMENT_BUDGET;
    let mut decoder = None;
    let mut warnings = Vec::new();
    for (index, item) in regions.into_iter().enumerate() {
        cancellation.check()?;
        if Instant::now() >= deadline {
            warnings.push(format!(
                "Code and formula decoding reached its time limit with {} regions remaining",
                total - index
            ));
            break;
        }

        if decoder.is_none() {
            match Decoder::start(input, worker, models) {
                Ok(started) => decoder = Some(started),
                Err(error) => {
                    warnings.push(format!(
                        "Could not start code and formula decoding: {error}"
                    ));
                    break;
                }
            }
        }
        let request = json!({ "label": item["label"], "prov": [item["prov"][0]] });
        let response = decoder.as_mut().expect("decoder started").request(
            &request,
            (Instant::now() + REGION_BUDGET).min(deadline),
            cancellation,
        )?;
        let page = item["prov"][0]["page_no"].as_u64().unwrap_or_default();
        match response {
            Ok(Some(Response::Decoded(text))) if !text.trim().is_empty() => {
                if item["label"] == "code" {
                    let (text, _) = extract_code_language(&text);
                    if !text.trim().is_empty() {
                        item["text"] = Value::String(text);
                        item["epubr_enriched"] = Value::Bool(true);
                    }
                } else {
                    item["text"] = Value::String(text);
                    item["epubr_formula_undecoded"] = Value::Bool(false);
                    item["epubr_enriched"] = Value::Bool(true);
                }
            }
            Ok(Some(Response::Unavailable)) => {
                warnings.push("The code and formula model could not be loaded".to_owned());
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                warnings.push(format!("Page {page}: code or formula decoding timed out"));
                decoder = None;
            }
            Err(error) => {
                warnings.push(format!(
                    "Page {page}: code or formula decoding failed: {error}"
                ));
                decoder = None;
            }
        }
        on_progress(Progress::Enrichment {
            completed: index + 1,
            total,
        });
    }
    Ok(warnings)
}

struct Decoder {
    worker: Worker,
    responses: mpsc::Receiver<io::Result<Response>>,
}

impl Decoder {
    fn start(input: &Path, worker: &Path, models: &PreparedModels) -> io::Result<Self> {
        let mut command = Command::new(worker);
        command
            .args([EXTRACTION_WORKER_FLAG, FLAG])
            .arg(input)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            command.process_group(0);
        }
        models.configure_command(&mut command);
        Self::spawn(&mut command)
    }

    fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut worker = Worker(command.spawn()?);
        let stdout = worker.0.stdout.take().expect("piped decoder stdout");
        let (sender, responses) = mpsc::sync_channel(1);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let response = line.and_then(|line| Ok(serde_json::from_str(&line)?));
                let failed = response.is_err();
                if sender.send(response).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self { worker, responses })
    }

    fn request(
        &mut self,
        request: &Value,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<io::Result<Option<Response>>, ConversionError> {
        cancellation.check()?;
        let stdin = self.worker.0.stdin.as_mut().expect("piped decoder stdin");
        if let Err(error) = serde_json::to_writer(&mut *stdin, request)
            .map_err(io::Error::from)
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
        {
            return Ok(Err(error));
        }
        loop {
            cancellation.check()?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Ok(None));
            }
            match self
                .responses
                .recv_timeout(remaining.min(Duration::from_millis(50)))
            {
                Ok(response) => return Ok(response.map(Some)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Ok(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "decoder exited",
                    )));
                }
            }
        }
    }
}

pub(super) fn worker_main(input: &OsStr) -> Result<(), Box<dyn std::error::Error>> {
    let (sender, requests) = mpsc::sync_channel(1);
    // A separate reader observes parent loss even while the decoder is in ONNX.
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                return;
            }
        }
        std::process::exit(1);
    });

    let mut crops = SourceCrops::new(Path::new(input));
    let threads = thread::available_parallelism().map_or(1, |count| count.get().min(4));
    let mut model = None;
    let mut stdout = io::stdout().lock();
    for line in requests {
        let item: Value = serde_json::from_str(&line)?;
        let kind = if item["label"] == "code" {
            CodeFormulaKind::Code
        } else {
            CodeFormulaKind::Formula
        };
        let response = if let Some(crop) = crops.raster(&item, 0.18) {
            if model.is_none() {
                model = CodeFormula::load_with(threads);
            }
            match &mut model {
                Some(model) => model
                    .predict(&crop, kind)
                    .map_or(Response::Unreadable, Response::Decoded),
                None => Response::Unavailable,
            }
        } else {
            Response::Unreadable
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        if matches!(response, Response::Unavailable) {
            break;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn unresponsive_decoder_returns_control_within_the_region_budget() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "IFS= read -r request; exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut decoder = Decoder::spawn(&mut command).unwrap();
        let start = Instant::now();
        let result = decoder.request(
            &Value::Null,
            start + Duration::from_millis(100),
            &CancellationToken::default(),
        );
        assert!(matches!(result, Ok(Ok(None))));
        drop(decoder);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
