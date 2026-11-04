use std::process::Command;

#[test]
fn rejects_invalid_page_range_as_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_epubr-docling-probe"))
        .args(["example.pdf", "--models", ".", "--pages", "5-4"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("one-based page or ascending range"));
}
