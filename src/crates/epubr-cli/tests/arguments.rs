use std::path::Path;
use std::process::Command;

fn probe() -> Command {
    let mut probe = Command::new(env!("CARGO_BIN_EXE_epubr-docling-probe"));
    probe
        .env_remove("EPUBR_MODEL")
        .env_remove("EPUBR_MODEL_DIR");
    probe
}

#[test]
fn rejects_invalid_page_range_as_usage_error() {
    let output = probe()
        .args(["example.pdf", "--models", ".", "--pages", "5-4"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("one-based page or ascending range"));
}

#[test]
fn rejects_an_unknown_model_profile_before_opening_input() {
    let output = probe()
        .args(["missing.pdf", "--model", "unknown-profile"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown-profile"));
    assert!(stderr.contains("--model"));
}

#[test]
fn reads_model_profile_from_environment() {
    let output = probe()
        .env("EPUBR_MODEL", "unknown-profile")
        .arg("missing.pdf")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown-profile"));
}

#[test]
fn explicit_model_profile_overrides_environment_and_model_directory_is_optional() {
    let output = probe()
        .env("EPUBR_MODEL", "unknown-profile")
        .args(["missing.pdf", "--model", "standard"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("unknown-profile"));
}

#[test]
fn explicit_model_directory_overrides_environment() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../epubr-core/tests/fixtures/basic.pdf");
    let output = probe()
        .env("EPUBR_MODEL_DIR", "missing-models-from-env")
        .args(["--models", "missing-models-from-flag"])
        .arg(fixture)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing-models-from-flag"));
    assert!(!stderr.contains("missing-models-from-env"));
}

#[test]
fn help_displays_environment_overrides_and_hides_the_worker_flag() {
    let output = probe().arg("--help").output().unwrap();

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("EPUBR_MODEL"));
    assert!(help.contains("EPUBR_MODEL_DIR"));
    assert!(help.contains("[default: technical]"));
    assert!(!help.contains("--worker"));
}
