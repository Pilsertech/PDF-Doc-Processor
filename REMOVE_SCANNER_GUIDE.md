# Guide: Remove Scanner, Keep Only PDF Merger

## Overview

This document explains which code to remove to keep only the PDF merging functionality, removing:
- File watcher (scanner)
- OCR processing
- Pair matching logic

The remaining functionality will simply merge 2 input PDFs into 1 output PDF.

---

## Files to Modify

### 1. `src/main.rs` - REMOVE these lines

**Lines 1-7**: Remove module declarations for scanner-related code:
```rust
// DELETE THESE LINES (1-7):
mod config;
mod error;
mod ocr;          // <-- REMOVE (OCR not needed)
mod processor;    // <-- REMOVE (contains scanner logic)
mod splitter;     // <-- REMOVE (scanner split logic)
mod utils;
mod watcher;      // <-- REMOVE (file watcher)
```

Replace with simplified modules for just merging:
```rust
mod config;
mod error;
mod merger;       // <-- NEW: simple PDF merger
```

**Lines 14**: Remove watcher import:
```rust
// DELETE line 14:
use crate::watcher::run_daemon;
```

**Lines 111-187**: Remove all config loading and watcher logic, replace with simple arguments:
```rust
// DELETE lines 111-187 (config loading + run_daemon)
```

Add simple argument parsing instead:
```rust
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("Usage: {} <input_a.pdf> <input_b.pdf> <output.pdf>", args[0]);
        std::process::exit(1);
    }
    
    let file_a = std::path::Path::new(&args[1]);
    let file_b = std::path::Path::new(&args[2]);
    let output = std::path::Path::new(&args[3]);
    
    merger::merge_pdfs(file_a, file_b, output)?;
    println!("Merged: {}", output.display());
    Ok(())
}
```

---

### 2. Create NEW file: `src/merger.rs`

```rust
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, Stream};
use std::path::Path;
use std::io::Cursor;

pub fn merge_pdfs(file_a: &Path, file_b: &Path, output: &Path) -> anyhow::Result<()> {
    // Load both PDFs
    let doc_a = lopdf::Document::load(file_a)?;
    let doc_b = lopdf::Document::load(file_b)?;
    
    // Get all pages from both documents
    let pages_a = get_all_pages(&doc_a);
    let pages_b = get_all_pages(&doc_b);
    
    let mut new_doc = Document::with_version("1.5");
    let pages_id = new_doc.new_object_id();
    let mut page_ids: Vec<Object> = Vec::new();
    
    // Add all pages in order: A pages then B pages
    for page_ref in pages_a.into_iter().chain(pages_b.into_iter()) {
        // Clone the page into new document (simplified version)
        // For full implementation, need to clone page objects
        let page_id = clone_page_to_doc(&mut new_doc, &doc_a, page_ref)?;
        page_ids.push(Object::Reference(page_id));
    }
    
    // Create pages node
    let pages_dict = Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Pages".to_vec())),
        ("Count", Object::Integer(page_ids.len() as i64)),
        ("Kids", Object::Array(page_ids)),
    ]);
    new_doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
    
    // Create catalog
    let catalog = Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Catalog".to_vec())),
        ("Pages", Object::Reference(pages_id)),
    ]);
    let catalog_id = new_doc.add_object(catalog);
    new_doc.trailer.set("Root", Object::Reference(catalog_id));
    
    new_doc.save(output)?;
    Ok(())
}

fn get_all_pages(doc: &lopdf::Document) -> Vec<lopdf::ObjectId> {
    let pages_id = doc.get_pages().keys().cloned().collect()
}
```

**Note**: The full merger implementation is complex. Consider using a simpler approach with `pdfium_render` to render pages to images and reassemble, similar to the existing `processor.rs` `assemble_pdf` function.

---

### 3. `src/processor.rs` - KEEP ONLY `assemble_pdf` function

Extract lines 144-266 (the `assemble_pdf` function) to a new `merger.rs` file. This function handles combining images into a PDF.

---

### 4. Delete these files entirely:

- `src/watcher.rs` - file watcher (DELETE)
- `src/ocr.rs` - OCR processing (DELETE)  
- `src/splitter.rs` - A3 splitting logic (may need simplified version)
- `src/utils.rs` - may contain scanner utilities (review and keep if needed)

---

### 5. `src/config.rs` - Can simplify or delete

Much of this is for scanner config (ROI, page_order). Can be simplified or removed.

---

## Alternative: Simpler Approach

Instead of modifying all this code, create a simple standalone merger:

1. Keep only `assemble_pdf()` from processor.rs
2. Create simple main that takes 2 PDF files as arguments
3. Use pdfium to render each page, then combine using assemble_pdf

---

## Summary of Changes

| Action | File | What to do |
|--------|------|------------|
| CREATE | `src/merger.rs` | New simple PDF merger |
| DELETE | `src/watcher.rs` | Remove file watcher |
| DELETE | `src/ocr.rs` | Remove OCR |
| MODIFY | `src/main.rs` | Replace with simple arg parsing |
| KEEP | `processor.rs` | Only `assemble_pdf()` function |
| DELETE | `src/splitter.rs` | Unless needed for merger |
| SIMPLIFY | `src/config.rs` | Remove scanner-specific config |
