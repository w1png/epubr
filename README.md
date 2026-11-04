# Epubr

Epubr is a Rust workspace for converting technical PDFs to reflowable EPUB 3.
The current milestone is the linked Docling extraction check.

- `epubr-core` contains the extraction API and owns the converter boundary.
- `epubr-cli` contains the command-line entry point. Its current executable,
  `epubr-docling-probe`, emits the extracted document as JSON.

To run the extraction check, pass a directory containing the pinned Docling
`.models/` assets. The probe reports extraction events on stderr.

```sh
cargo run --release --locked -p epubr-cli -- raw/input.pdf \
  --models /path/to/docling-assets --pages 24-30 --mode text > extraction.json
```

See the [technical-book acceptance cases](docs/acceptance.md) for the pages
used to check extraction and reader compatibility.

## LLM Wiki

Start at the [wiki index](wiki/index.md). The wiki uses ordinary Markdown and can be browsed on GitHub or opened in Obsidian by selecting this folder as a vault.

Put source files or pinned URL records in [`raw/`](raw/), then ask Codex to ingest them. Ask questions against the wiki, save an answer you want to reuse, or ask for a lint pass. The [agent schema](AGENTS.md) defines those workflows. The [activity log](wiki/log.md) records completed work.
