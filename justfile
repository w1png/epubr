set positional-arguments

# Build the release workspace.
release:
    cargo build --locked --release --workspace

# Run the CLI in debug mode and forward its arguments.
debug *args:
    cargo run --locked -p epubr-cli -- "$@"

# Run the workspace tests.
test *args:
    cargo test --locked --workspace "$@"
