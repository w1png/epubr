# Epubr

Epubr is a Rust workspace for converting technical PDFs to reflowable EPUB 3.

PDF conversion downloads and verifies pinned Docling assets on first use, then
reuses the verified cache offline. It shows a progress bar with the latest
status or diagnostic on terminal stderr and publishes the EPUB when conversion
succeeds. Press Ctrl-C to cancel.

```sh
just build
just test
just install
```

## Examples

Pass the source path with `--input PATH` or `--input=PATH`. Use `--output`
or `-o` to set the destination. `convert` requires both paths, and `optimize`
can infer the destination from the input filename.

Use `--help` or `-h` to see the options for each command. If you omit a required
option, Epubr prints the full command help to stderr and exits with status 2.
Use `--version` or `-V` to see the installed version.

```sh
epubr --help
epubr convert --help
epubr optimize --help
```

For a formula-heavy technical book, use the technical model for formula
enrichment and automatic OCR for pages that need it.

```sh
epubr convert --input raw/hard.pdf --output hard-technical.epub --model technical --mode auto
```

To check equation fidelity before converting the whole book, preview a
formula-heavy range and compare the EPUB with the PDF.

```sh
epubr convert --input raw/hard.pdf --output hard-technical-preview.epub \
  --model technical --mode auto --pages 172-176
```

For an e-ink reader, use the device preset to resize raster images, convert
them to four-level grayscale, and split long chapters at block boundaries.
EPUBs with extractable text also include stable reference page data when
the location file fits within CrossInk’s 64 KiB limit.

```sh
epubr convert --input raw/hard.pdf --output hard-x4-pro.epub --optimizeFor=xteink_x4_pro
```

The available presets are `xteink_x3`, `xteink_x4`, `xteink_x4_pro`, and
`seeed_studio_sticky`. `xteink_x4_classic` is an alias for `xteink_x4`.
`--optimize-for` is an alias for `--optimizeFor`. Optimization runs locally.
Top-level chapters are separate reading-order documents in every conversion.

Use `optimize` with any of the same presets on an existing EPUB. This works
directly on the EPUB and needs no PDF or Docling models.

```sh
epubr optimize --input hard.epub --optimizeFor=xteink_x4_pro
epubr optimize --input hard.epub --output hard-sticky.epub --optimizeFor=seeed_studio_sticky
```

When you omit `--output`, `optimize` writes `hard-optimized.epub` beside
`hard.epub`. The optimizer writes a new file and reports an error if the
destination already exists.

The optimizer preserves images whose filenames are needed by stylesheets or
responsive image markup. It supports unencrypted, unsigned EPUB files.

Book metadata is automatic by default. Epubr uses embedded PDF metadata,
recognizable catalog entries and imprint statements in the opening pages,
and structured filenames such as `Katz J., Lindell Y. - Book, 2nd Edition - 2015.pdf`.
It omits optional fields when those sources provide no reliable value.
Each metadata flag overrides just that field. Repeat `--author` to supply
the complete author list in order.

```sh
epubr convert --input input.pdf --output output.epub \
  --title "Applied Cryptography Notes" --author "Alex Rivera" --author "Sam Chen" \
  --year 2024 --edition 2 --language en --publisher "Example Press" \
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
For a short conversion with debug logs, run `just debug` with a page range.

```sh
just debug convert --input raw/hard.pdf --output hard-preview.epub --pages 172-176
```

The separate `epubr-docling-probe` binary emits Docling JSON for extraction
diagnostics.

## Core conversion

Use `epubr_core::convert` to read model settings from the environment or
`epubr_core::convert_with_models` to pass a `ModelConfig` explicitly.
Both accept a cancellation token and a progress callback. Library consumers can
build the companion `epubr-extract-worker` or set `EPUBR_EXTRACTION_WORKER` to
its path. The `epubr` CLI uses the same executable for its worker.

Use `epubr_core::optimize_epub` with input and output paths and an
`OptimizationTarget` to optimize an existing EPUB.

The end-to-end tests use the pinned model cache. To run them with a
preinstalled snapshot, set `EPUBR_MODEL_DIR` to the directory containing
`.models/`.
