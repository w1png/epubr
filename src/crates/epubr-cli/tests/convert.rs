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

#[test]
fn debug_streams_progress_with_redirected_stderr() {
    let root = tempfile::tempdir().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../epubr-core/tests/fixtures/basic.pdf");

    for debug in [false, true] {
        let mut command = epubr();
        if debug {
            command.arg("--debug");
        }
        let output = command
            .arg("convert")
            .arg(&fixture)
            .arg("-o")
            .arg(root.path().join("out.epub"))
            .args(["--model", "standard", "--models"])
            .arg(root.path().join("missing-models"))
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains("could not prepare Docling models"),
            "{stderr}"
        );
        assert_eq!(
            stderr.contains("Preparing standard models"),
            debug,
            "{stderr}"
        );
        assert!(!stderr.contains('\x1b'), "{stderr}");
    }
}

#[test]
fn debug_conflicts_with_quiet_before_and_after_the_subcommand() {
    let convert = ["convert", "missing.pdf", "-o", "output.epub", "--quiet"];
    for arguments in [
        [&["--debug"][..], &convert].concat(),
        [&convert[..], &["--debug"]].concat(),
    ] {
        let output = epubr().args(arguments).output().unwrap();

        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
    }
}

#[test]
fn convert_accepts_book_metadata_with_multiple_authors() {
    let root = tempfile::tempdir().unwrap();
    let output = epubr()
        .arg("convert")
        .arg(root.path().join("missing.pdf"))
        .arg("-o")
        .arg(root.path().join("book.epub"))
        .args([
            "--title",
            "Algebra & Logic",
            "--author",
            "A. Writer",
            "--author",
            "B. Writer",
            "--year",
            "2024",
            "--language",
            "sr-Latn-RS",
            "--publisher",
            "Example Press",
            "--description",
            "An introduction to formal reasoning.",
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("input is not a readable PDF"), "{stderr}");
}

#[test]
fn convert_rejects_invalid_book_metadata_before_opening_input() {
    for (flag, value) in [
        ("--title", " \t "),
        ("--author", " \t "),
        ("--publisher", " \t "),
        ("--description", " \t "),
        ("--year", "0"),
        ("--year", "10000"),
        ("--year", "unknown"),
        ("--language", "en_US"),
        ("--language", "123"),
    ] {
        let output = epubr()
            .args(["convert", "missing.pdf", "-o", "output.epub", flag, value])
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{flag} {value:?}: {stderr}");
        assert!(stderr.contains(flag), "{stderr}");
    }
}
