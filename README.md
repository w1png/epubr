# Epubr

Epubr is a Rust workspace for converting technical PDFs to reflowable EPUB 3.

The CLI downloads and verifies pinned Docling assets on first use, then reuses
the verified cache offline. It shows a progress bar with the latest status or
diagnostic on terminal stderr and publishes the EPUB when conversion succeeds.
Press Ctrl-C to cancel.

```sh
just build
just test
just install
```

## Examples

For a formula-heavy technical book, use the technical model for formula
enrichment and automatic OCR for pages that need it.

```sh
epubr convert raw/hard.pdf -o hard-technical.epub --model technical --mode auto
```

To check equation fidelity before converting the whole book, preview a
formula-heavy range and compare the EPUB with the PDF.

```sh
epubr convert raw/hard.pdf -o hard-technical-preview.epub --model technical --mode auto --pages 172-176
```

Set book metadata when converting a PDF. Repeat `--author` to list multiple
authors in order. Omitted title, author, and language fields use the
converter's existing PDF metadata and fallbacks.

```sh
epubr convert input.pdf -o output.epub \
  --title "Applied Cryptography Notes" --author "Alex Rivera" --author "Sam Chen" \
  --year 2024 --language en --publisher "Example Press" \
  --description "A reference for engineers."
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
- `--debug` streams progress and Docling diagnostics to stderr. Use it before
  or after `convert`.
- `--quiet` suppresses progress and the completion message. It conflicts with
  `--debug`.

Use `just release` to build optimized binaries in `target/release`.
`just debug convert raw/hard.pdf -o hard-preview.epub --pages 172-176` runs a
short conversion with debug logs. The separate `epubr-docling-probe` binary
emits Docling JSON for extraction diagnostics.

## Core conversion

Use `epubr_core::convert` to read model settings from the environment or
`epubr_core::convert_with_models` to pass a `ModelConfig` explicitly.
Both accept a cancellation token and a progress callback. Library consumers can
build the companion `epubr-extract-worker` or set `EPUBR_EXTRACTION_WORKER` to
its path. The `epubr` CLI uses the same executable for its worker.

The end-to-end tests use the pinned model cache. To run them with a
preinstalled snapshot, set `EPUBR_MODEL_DIR` to the directory containing
`.models/`.
