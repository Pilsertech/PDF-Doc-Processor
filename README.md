# PDF Split-Scan Processor — Full Rust Implementation

## Project Structure

```
scanner_processor/
├── Cargo.toml
└── src/
    ├── main.rs        ← CLI entry point (clap)
    ├── config.rs      ← Config + RoiConfig structs
    ├── error.rs       ← thiserror error types
    ├── utils.rs       ← filename parsing (extract_counter)
    ├── splitter.rs    ← pdfium-render: PDF→image + A3→A4 split
    ├── ocr.rs         ← image crop + adaptive threshold + leptess/Tesseract
    ├── processor.rs   ← pair pipeline + lopdf PDF assembly
    └── watcher.rs     ← notify watcher + crossbeam pair-matching loop
```

---

## Crate Responsibilities

| Crate | Version | Role |
|---|---|---|
| `pdfium-render` | 0.8 | PDF → RGB image at any DPI (PDFium = Chrome's PDF engine) |
| `image` | 0.25 | Crop A3→A4 halves, resize, encode JPEG |
| `leptess` | 0.14 | Tesseract OCR bindings (replaces EasyOCR) |
| `lopdf` | 0.34 | Assemble 4 A4 images into output PDF |
| `notify` | 6 | File system watcher (better than Python watchdog) |
| `crossbeam-channel` | 0.5 | Zero-overhead channel: watcher → processor |
| `regex` | 1 | Extract student number from OCR text |
| `clap` | 4 | CLI args with `--derive` macro |
| `tracing` | 0.1 | Structured logging |
| `anyhow` + `thiserror` | 1 | Error handling |

---

## System Dependencies (install before `cargo build`)

### Ubuntu / Debian:
```bash
# Tesseract OCR engine + English training data
sudo apt install libtesseract-dev tesseract-ocr tesseract-ocr-eng libleptonica-dev

# Clang (needed by leptess bindgen)
sudo apt install clang libclang-dev

# Verify tesseract works
tesseract --version
```

### macOS:
```bash
brew install tesseract leptonica llvm
export LLVM_CONFIG_PATH=$(brew --prefix llvm)/bin/llvm-config
```

### PDFium binary (all platforms):
```bash
# Download prebuilt PDFium shared library from:
# https://github.com/bblanchon/pdfium-binaries/releases

# Linux example:
wget https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-linux-x64.tgz
tar -xzf pdfium-linux-x64.tgz
# Copy libpdfium.so to your project root or set --pdfium-lib path
cp lib/libpdfium.so ./libpdfium.so
```

---

## Build & Run

```bash
# Debug build
cargo build

# Release build (much faster at runtime)
cargo build --release

# Run with defaults (watch ./input, output to ./output)
./target/release/scanner_processor

# Full options
./target/release/scanner_processor \
  --watch-dir  /mnt/scanner/inbox \
  --output-dir /mnt/scanner/output \
  --dpi 300 \
  --tessdata /usr/share/tessdata \
  --pdfium-lib ./libpdfium.so \
  --roi-y-start 0.155 \
  --roi-y-end   0.235 \
  --debug-roi        # ← saves _debug_roi.png to calibrate ROI coords
```

---

## ROI Coordinate Reference

The APPLICATION NO. box position on the Nyunga Foundation form (at 300 DPI):

```
A4 right half (2480 × 3508 px):
┌──────────────────────────────────┐  y=0
│          HEADER / LOGO           │
│                                  │
│  ┌────────────────────────────┐  │  y ≈ 544  (15.5%)
│  │  DATE  | MONTH  |  YEAR   │  │
│  │  APPLICATION NO.  21143   │  │  ← ROI (only this strip is OCR'd)
│  └────────────────────────────┘  │  y ≈ 824  (23.5%)
│                                  │
│   [rest of form — never scanned] │
└──────────────────────────────────┘  y=3508
   x≈198 (8%)              x≈2282 (92%)
```

**To recalibrate:** run with `--debug-roi` and inspect `output/_debug_roi.png`.
Adjust `--roi-y-start` / `--roi-y-end` until the number is clearly visible in the strip.

---

## Page Order Logic

```
file_A  (counter N,   outer sheet):
    .right → page 1   ← ONLY page that gets OCR'd
    .left  → page 4

file_B  (counter N+1, inner sheet):
    .right → page 2
    .left  → page 3

Output PDF: [page1, page2, page3, page4]
```

---

## OCR Pipeline Detail

```
page1 image (2480×3508)
    │
    ▼ crop_roi()         → ~280×2084 px  (8% of page)
    │
    ▼ to_luma8()         → grayscale
    │
    ▼ resize ×2          → ~560×4168 px  (Lanczos3)
    │
    ▼ adaptive_threshold → binary black/white (window=31, C=10)
    │
    ▼ leptess (Tesseract) PSM=6, whitelist=digits+relevant chars
    │
    ▼ regex \b(\d{4,12})\b → "21143"
```

**Why adaptive threshold instead of simple threshold?**
Scanner output has uneven illumination across the page (brighter in center, darker at edges).
Adaptive thresholding computes a local mean for each pixel's neighborhood, making it
robust to this gradient — the same ink looks consistently black regardless of position.

---

## Error Handling

All errors use `thiserror` typed variants in `error.rs`:

| Error | Behaviour |
|---|---|
| Filename can't be parsed | Skip file, log warning |
| PDFium render fails | Propagate, log error for that pair |
| OCR finds no number | Use `UNKNOWN_NNNNNN` fallback name — document is never lost |
| PDF assembly fails | Log error, pair skipped |

---

## Environment Variables

```bash
# Control log verbosity
RUST_LOG=debug   ./scanner_processor   # verbose
RUST_LOG=info    ./scanner_processor   # normal (default)
RUST_LOG=warn    ./scanner_processor   # quiet
```

---

## Running as a systemd Service

```ini
# /etc/systemd/system/scanner-processor.service
[Unit]
Description=PDF Split-Scan Processor
After=network.target

[Service]
Type=simple
User=scanner
ExecStart=/usr/local/bin/scanner_processor \
  --watch-dir /mnt/scanner/inbox \
  --output-dir /mnt/scanner/output \
  --tessdata /usr/share/tessdata \
  --pdfium-lib /usr/local/lib/libpdfium.so
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now scanner-processor
sudo journalctl -u scanner-processor -f   # follow logs
```

---

## Tests

```bash
# Run unit tests (filename parsing, ROI math, regex extraction)
cargo test

# Test a single pair manually (one-shot mode — add to main.rs if needed)
cargo run -- --watch-dir ./test_input --debug-roi
# then copy doc004883*.pdf and doc004884*.pdf into ./test_input
```
