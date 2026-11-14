use std::process::{Command, Output};

fn epubr(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_epubr"))
        .env_remove("EPUBR_MODEL")
        .env_remove("EPUBR_MODEL_DIR")
        .args(args)
        .output()
        .unwrap()
}

fn successful_help(args: &[&str]) -> String {
    let output = epubr(args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn explicit_help_and_shortcut_print_to_stdout() {
    let root = successful_help(&["--help"]);
    assert_eq!(root, successful_help(&["-h"]));
    assert!(root.contains("Usage: epubr"));
    assert!(root.contains("convert") && root.contains("optimize"));
    assert!(
        !root
            .lines()
            .any(|line| line.trim_start().starts_with("help "))
    );

    for (command, specific_option) in [("convert", "--mode"), ("optimize", "--optimizeFor")] {
        let help = successful_help(&[command, "--help"]);
        assert_eq!(help, successful_help(&[command, "-h"]));
        assert!(help.contains(&format!("Usage: epubr {command}")));
        for option in ["--input", "--output", "-o", specific_option] {
            assert!(help.contains(option), "{help}");
        }
    }
}

#[test]
fn missing_required_options_print_a_diagnostic_then_full_help() {
    for (command, examples) in [
        (
            "convert",
            [
                (&[][..], &["--input", "--output"][..]),
                (&["--input=input.pdf"][..], &["--output"][..]),
                (&["--output=output.epub"][..], &["--input"][..]),
            ],
        ),
        (
            "optimize",
            [
                (&[][..], &["--input", "--optimizeFor"][..]),
                (&["--input=input.epub"][..], &["--optimizeFor"][..]),
                (&["--optimizeFor=xteink_x4_pro"][..], &["--input"][..]),
            ],
        ),
    ] {
        let help = successful_help(&[command, "--help"]);
        for (options, required) in examples {
            let args = [&[command][..], options].concat();
            let output = epubr(&args);
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
            let diagnostic = stderr
                .trim_end()
                .strip_suffix(help.trim_end())
                .unwrap_or_else(|| {
                    panic!("{args:?}: expected full help after the error:\n{stderr}")
                });
            assert!(diagnostic.starts_with("error:"), "{args:?}: {stderr}");
            for option in required {
                assert!(diagnostic.contains(option), "{args:?}: {stderr}");
            }
        }
    }
}

#[test]
fn positional_input_and_help_subcommand_are_rejected() {
    for (args, rejected) in [
        (
            &["convert", "input.pdf", "-o", "output.epub"][..],
            "input.pdf",
        ),
        (
            &["optimize", "input.epub", "--optimizeFor=xteink_x4_pro"][..],
            "input.epub",
        ),
        (&["help", "convert"][..], "help"),
    ] {
        let output = epubr(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(
            stderr.starts_with("error:") && stderr.contains(rejected),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn version_shortcut_matches_long_option() {
    let long = epubr(&["--version"]);
    let short = epubr(&["-V"]);
    for output in [&long, &short] {
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    assert_eq!(short.stdout, long.stdout);
    assert!(
        String::from_utf8(long.stdout)
            .unwrap()
            .trim_end()
            .ends_with(concat!(" ", env!("CARGO_PKG_VERSION")))
    );
}
