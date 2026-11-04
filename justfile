set positional-arguments

# Build the workspace.
build:
    cargo build --locked --workspace

# Build the release workspace.
release:
    cargo build --locked --release --workspace

# Run the workspace tests.
test *args:
    cargo test --locked --workspace "$@"

# Install the CLI.
install *args:
    cargo install --locked --path src/crates/epubr-cli --bin epubr "$@"

# Run the CLI with debug logs and forward its arguments.
debug *args:
    cargo run --locked -p epubr-cli -- --debug "$@"
