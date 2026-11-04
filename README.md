# Epubr

Epubr is a Rust workspace for converting technical PDFs to reflowable EPUB 3.
The current milestone is the linked Docling extraction check.

- `epubr-core` contains the extraction API and owns the converter boundary.
- `epubr-cli` contains the command-line entry point. Its current executable,
  `epubr-docling-probe`, emits the extracted document as JSON.

The probe downloads and verifies pinned Docling assets on first use, then
reuses the verified cache offline. It reports download bytes and extraction
progress on stderr and writes JSON to stdout.

```sh
cargo run --release --locked -p epubr-cli -- raw/input.pdf \
  --model standard --pages 24-30 --mode text > extraction.json
```

- `--model standard` selects layout, tables, and multilingual OCR (about 295 MiB).
  `--model technical` adds formula enrichment (about 1.01 GiB total) and is the
  default. Both profiles share cached assets.
- `--models DIR` uses a preinstalled directory containing the pinned
  `.models/` assets. Epubr verifies this directory and reports missing or
  invalid assets. With no override, it uses the OS user cache (on Linux,
  `$XDG_CACHE_HOME/epubr` or `~/.cache/epubr`, and on macOS,
  `~/Library/Caches/epubr`).
- `EPUBR_MODEL` and `EPUBR_MODEL_DIR` set defaults for the corresponding flags.
  A future web entry point can read the same settings with
  `epubr_core::ModelConfig::from_env()`.

See the [technical-book acceptance cases](docs/acceptance.md) for the pages
used to check extraction and reader compatibility.

## LLM Wiki

Start at the [wiki index](wiki/index.md). The wiki uses ordinary Markdown and can be browsed on GitHub or opened in Obsidian by selecting this folder as a vault.

Put source files or pinned URL records in [`raw/`](raw/), then ask Codex to ingest them. Ask questions against the wiki, save an answer you want to reuse, or ask for a lint pass. The [agent schema](AGENTS.md) defines those workflows. The [activity log](wiki/log.md) records completed work.
