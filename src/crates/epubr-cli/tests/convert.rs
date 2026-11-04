use std::path::Path;
use std::process::Command;

fn epubr() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_epubr"));
    command
        .env_remove("EPUBR_MODEL")
        .env_remove("EPUBR_MODEL_DIR");
    command
}

#[test]
fn convert_requires_a_destination() {
    let output = epubr().args(["convert", "input.pdf"]).output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--output"));
}

#[test]
fn model_and_ocr_flags_accept_the_documented_choices() {
    let output = epubr()
        .env("EPUBR_MODEL", "unknown-profile")
        .args([
            "convert",
            "missing.pdf",
            "-o",
            "output.epub",
            "--model",
            "standard",
            "--mode",
            "scan",
            "--pages",
            "1-2",
            "--quiet",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("input is not a readable PDF"), "{stderr}");
    assert!(!stderr.contains("Preparing"), "{stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn convert_preserves_an_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("existing.epub");
    std::fs::write(&destination, b"existing file").unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../epubr-core/tests/fixtures/basic.pdf");

    let output = epubr()
        .arg("convert")
        .arg(fixture)
        .arg("-o")
        .arg(&destination)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("destination already exists"));
    assert_eq!(std::fs::read(&destination).unwrap(), b"existing file");
}
