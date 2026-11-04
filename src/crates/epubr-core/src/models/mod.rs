mod manifest;
#[cfg(test)]
mod tests;

use std::env;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use directories_next::ProjectDirs;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{CancellationToken, Progress};
use manifest::{Asset, FORMULA, STANDARD};

const SNAPSHOT: &str = "models-v1-f8a5d4fd8b08";
const RELEASES: &str = "https://github.com/docling-project/docling.rs/releases/download";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelProfile {
    /// Layout, tables, and multilingual OCR.
    Standard,
    /// Standard models plus CodeFormula enrichment.
    #[default]
    Technical,
}

impl fmt::Display for ModelProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Standard => "standard",
            Self::Technical => "technical",
        })
    }
}

impl FromStr for ModelProfile {
    type Err = ModelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standard" => Ok(Self::Standard),
            "technical" => Ok(Self::Technical),
            _ => Err(ModelError::UnknownProfile(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelConfig {
    pub profile: ModelProfile,
    /// A preinstalled root containing the pinned `.models/` directory.
    /// If omitted, missing assets are downloaded into the user cache.
    pub directory: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("unknown model profile {0:?}, expected standard or technical")]
    UnknownProfile(String),
    #[error("EPUBR_MODEL must be valid UTF-8")]
    InvalidProfileEncoding,
    #[error("could not determine the model cache directory")]
    NoCacheDirectory,
    #[error("model root must be valid UTF-8: {}", .0.display())]
    InvalidDirectoryEncoding(PathBuf),
    #[error("model preparation cancelled")]
    Cancelled,
    #[error("missing or invalid pinned model asset: {}", .0.display())]
    InvalidAsset(PathBuf),
    #[error("model cache I/O at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not download {asset}: {source}")]
    Download {
        asset: &'static str,
        #[source]
        source: Box<ureq::Error>,
    },
}

impl ModelConfig {
    /// Read settings from the process environment without modifying it.
    pub fn from_env() -> Result<Self, ModelError> {
        let profile = match env::var("EPUBR_MODEL") {
            Ok(value) => value.parse()?,
            Err(env::VarError::NotPresent) => ModelProfile::default(),
            Err(env::VarError::NotUnicode(_)) => return Err(ModelError::InvalidProfileEncoding),
        };
        Ok(Self {
            profile,
            directory: env::var_os("EPUBR_MODEL_DIR").map(PathBuf::from),
        })
    }

    /// Verify the requested snapshot, fetching missing or corrupt cached assets.
    /// The callback runs synchronously during downloads.
    pub fn prepare(
        &self,
        cancellation: &CancellationToken,
        on_progress: impl Fn(Progress),
    ) -> Result<PreparedModels, ModelError> {
        check_cancelled(cancellation)?;
        let managed = self.directory.is_none();
        let root = match &self.directory {
            Some(root) => root.clone(),
            None => ProjectDirs::from("", "", "epubr")
                .ok_or(ModelError::NoCacheDirectory)?
                .cache_dir()
                .join("models")
                .join(SNAPSHOT),
        };
        if managed {
            io_at(&root, fs::create_dir_all(&root))?;
        }
        let root = io_at(&root, root.canonicalize())?;
        if root.to_str().is_none() {
            return Err(ModelError::InvalidDirectoryEncoding(root));
        }

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(30)))
            .build()
            .into();
        let formula = match self.profile {
            ModelProfile::Standard => &[][..],
            ModelProfile::Technical => FORMULA,
        };
        let base = format!("{RELEASES}/{SNAPSHOT}");
        for asset in STANDARD.iter().chain(formula) {
            let path = root.join(".models").join(asset.path);
            if !is_verified(&path, asset, cancellation)? {
                if !managed {
                    return Err(ModelError::InvalidAsset(path));
                }
                download_asset(&agent, &base, asset, &path, cancellation, &on_progress)?;
            }
        }
        check_cancelled(cancellation)?;
        Ok(PreparedModels { root })
    }
}

/// A verified model root ready to use in a Docling worker process.
#[derive(Debug)]
pub struct PreparedModels {
    root: PathBuf,
}

impl PreparedModels {
    /// Configure a child command without changing the caller's cwd or environment.
    /// Pass an absolute input path to the worker.
    pub fn configure_command(&self, command: &mut Command) {
        let models = self.root.join(".models");
        command
            .current_dir(&self.root)
            .env("DOCLING_RS_MODELS_DIR", &models)
            .env("DOCLING_RS_EP", "cpu")
            .env("DOCLING_RS_FP32", "0")
            .env("DOCLING_RS_OCR_ENGINE", "rapidocr")
            .env("DOCLING_RS_OCR_TEXTLINE_ORIENTATION", "0");
        for (key, path) in [
            ("DOCLING_LAYOUT_ONNX", "layout_heron_int8.onnx"),
            ("DOCLING_OCR_DET_ONNX", "ocr_det.onnx"),
            ("DOCLING_OCR_REC_ONNX", "ocr_rec_v6.onnx"),
            ("DOCLING_OCR_DICT", "ocr_rec_v6_dict.txt"),
            (
                "DOCLING_TABLEFORMER_ENCODER",
                "tableformer/encoder_fp16.onnx",
            ),
            (
                "DOCLING_TABLEFORMER_DECODER",
                "tableformer/decoder_kv_layers.onnx",
            ),
            ("DOCLING_TABLEFORMER_BBOX", "tableformer/bbox.onnx"),
            ("DOCLING_CODE_FORMULA_DIR", "code_formula"),
        ] {
            command.env(key, models.join(path));
        }
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), ModelError> {
    if cancellation.is_cancelled() {
        return Err(ModelError::Cancelled);
    }
    Ok(())
}

fn io_at<T>(path: &Path, result: io::Result<T>) -> Result<T, ModelError> {
    result.map_err(|source| ModelError::Io {
        path: path.to_owned(),
        source,
    })
}

fn is_verified(
    path: &Path,
    asset: &Asset,
    cancellation: &CancellationToken,
) -> Result<bool, ModelError> {
    check_cancelled(cancellation)?;
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return io_at(path, Err(error)),
    };
    let metadata = io_at(path, file.metadata())?;
    if !metadata.is_file() || metadata.len() != asset.bytes {
        return Ok(false);
    }

    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancelled(cancellation)?;
        let count = io_at(path, file.read(&mut buffer))?;
        if count == 0 {
            return Ok(format!("{:x}", hash.finalize()) == asset.sha256);
        }
        hash.update(&buffer[..count]);
    }
}

fn download_asset(
    agent: &ureq::Agent,
    base: &str,
    asset: &Asset,
    path: &Path,
    cancellation: &CancellationToken,
    on_progress: &impl Fn(Progress),
) -> Result<(), ModelError> {
    let directory = path.parent().expect("asset path has a parent");
    io_at(directory, fs::create_dir_all(directory))?;
    let mut staging = io_at(directory, NamedTempFile::new_in(directory))?;

    let report = |downloaded| {
        on_progress(Progress::ModelDownload {
            asset: asset.name,
            downloaded,
            total: Some(asset.bytes),
        });
    };
    report(0);
    check_cancelled(cancellation)?;

    let response = agent
        .get(format!("{base}/{}", asset.name))
        .call()
        .map_err(|source| ModelError::Download {
            asset: asset.name,
            source: Box::new(source),
        })?;
    let mut reader = response.into_body().into_reader();
    let mut buffer = [0; 64 * 1024];
    let mut hash = Sha256::new();
    let mut downloaded = 0;
    loop {
        check_cancelled(cancellation)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|source| ModelError::Download {
                asset: asset.name,
                source: Box::new(ureq::Error::Io(source)),
            })?;
        if count == 0 {
            break;
        }
        downloaded += count as u64;
        if downloaded > asset.bytes {
            return Err(ModelError::InvalidAsset(path.to_owned()));
        }
        hash.update(&buffer[..count]);
        io_at(path, staging.write_all(&buffer[..count]))?;
        if downloaded < asset.bytes {
            report(downloaded);
        }
    }
    if downloaded != asset.bytes || format!("{:x}", hash.finalize()) != asset.sha256 {
        return Err(ModelError::InvalidAsset(path.to_owned()));
    }

    check_cancelled(cancellation)?;
    io_at(path, staging.as_file().sync_all())?;
    // Concurrent jobs can replace this path only with the same verified bytes.
    io_at(path, staging.persist(path).map_err(|error| error.error))?;
    report(downloaded);
    Ok(())
}
