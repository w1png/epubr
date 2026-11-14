use std::ffi::OsString;
use std::fs;
use std::process::Command;

fn epubr() -> Command {
    Command::new(env!("CARGO_BIN_EXE_epubr"))
}

#[test]
fn optimize_defaults_to_a_sibling_and_preserves_existing_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Архитектура компьютера.epub");
    let default_output = dir.path().join("Архитектура компьютера-optimized.epub");
    let custom_output = dir.path().join("custom.epub");
    fs::write(&input, b"input file").unwrap();

    let mut equals_input = OsString::from("--input=");
    equals_input.push(&input);
    for (input_args, target, override_path) in [
        (
            vec![OsString::from("--input"), input.clone().into_os_string()],
            "--optimizeFor=xteink_x4_pro",
            None,
        ),
        (
            vec![equals_input],
            "--optimize-for=xteink_x4_classic",
            Some(&custom_output),
        ),
    ] {
        let expected_output = override_path.unwrap_or(&default_output);
        fs::write(expected_output, b"existing file").unwrap();

        let mut command = epubr();
        command.arg("optimize").args(input_args).arg(target);
        if let Some(path) = override_path {
            command.arg("-o").arg(path);
        }
        let output = command.output().unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("destination already exists"), "{stderr}");
        assert_eq!(fs::read(expected_output).unwrap(), b"existing file");
        fs::remove_file(expected_output).unwrap();
    }

    assert_eq!(fs::read(&input).unwrap(), b"input file");
}
