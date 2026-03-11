#!/bin/bash
DIR="$(cd "$(dirname "$0")" && pwd)"

# Set environment variables for Tesseract and PDFium
export TESSDATA_PREFIX="/usr/share/tesseract-ocr/5/tessdata"
export LD_LIBRARY_PATH="$DIR:$LD_LIBRARY_PATH"

exec "$DIR/scanner_processor" "$@"
