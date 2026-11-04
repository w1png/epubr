# Epubr

Epubr is a Rust workspace for converting technical PDFs to reflowable EPUB 3.

The CLI downloads and verifies pinned Docling assets on first use, then reuses
the verified cache offline. It reports progress on stderr and publishes the EPUB
when conversion succeeds. Press Ctrl-C to cancel.

```sh
just release
./target/release/epubr convert raw/hard.pdf -o hard.epub --model technical
```

- `--mode auto` uses OCR as needed, `--mode text` uses the PDF's text layer,
  and `--mode scan` forces OCR. Use `--pages 24-30` for an inclusive range.
- `--model standard` selects layout, tables, and multilingual OCR (about 295 MiB).
  `--model technical` adds code and formula enrichment (about 1.01 GiB total) and is the
  default. Both profiles share cached assets. Technical conversion reports
  extraction and decoding separately. If decoding exceeds its time budget,
  Epubr preserves the affected region as a source crop where available.
- `--models DIR` uses a preinstalled directory containing the pinned
  `.models/` assets. Epubr verifies this directory and reports missing or
  invalid assets. With no override, it uses the OS user cache (on Linux,
  `$XDG_CACHE_HOME/epubr` or `~/.cache/epubr`, and on macOS,
  `~/Library/Caches/epubr`).
- `EPUBR_MODEL` and `EPUBR_MODEL_DIR` set defaults for the corresponding flags.
- `--quiet` suppresses progress output.

Use `just debug convert raw/hard.pdf -o hard-preview.epub --pages 172-176`
for a short debug run. The separate `epubr-docling-probe` binary still emits
Docling JSON for extraction diagnostics.

## Core conversion

Use `epubr_core::convert` to read model settings from the environment or
`epubr_core::convert_with_models` to pass a `ModelConfig` explicitly.
Both accept a cancellation token and a progress callback. Library consumers can
build the companion `epubr-extract-worker` or set `EPUBR_EXTRACTION_WORKER` to
its path. The `epubr` CLI uses the same executable for its worker.

The end-to-end tests use the pinned model cache. To run them with a
preinstalled snapshot, set `EPUBR_MODEL_DIR` to the directory containing
`.models/`.

See the [technical-book acceptance cases](docs/acceptance.md) for the pages
used to check extraction and reader compatibility.

## LLM Wiki

Start at the [wiki index](wiki/index.md). The wiki uses ordinary Markdown and can be browsed on GitHub or opened in Obsidian by selecting this folder as a vault.

Put source files or pinned URL records in [`raw/`](raw/), then ask Codex to ingest them. Ask questions against the wiki, save an answer you want to reuse, or ask for a lint pass. The [agent schema](AGENTS.md) defines those workflows. The [activity log](wiki/log.md) records completed work.
