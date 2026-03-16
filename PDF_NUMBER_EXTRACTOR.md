# PDF Number Extractor - xAI Integration

This script processes pairs of PDF and PNG files. It sends the PNG to xAI's Grok model to extract a number, then renames the corresponding PDF file using that extracted number.

## Setup

1. **Install Node.js dependencies** (if not already done):
   ```bash
   npm init -y
   npm install typescript ts-node @types/node dotenv openai
   ```

2. **Set environment variable**:
   ```bash
   export XAI_API_KEY="your-api-key-here"
   ```

3. **Configure**: Edit `config.ts` to set:
   - `inputDir`: Directory containing PDF/PNG pairs (default: `Docs/output`)
   - `outputDir`: Directory for renamed PDFs (default: `Docs/renamed`)
   - `model`: xAI model to use (default: `grok-4-1-fast-non-reasoning`)

## Usage

```bash
npx ts-node src/extract-number.ts
```

## How It Works

1. Scans `inputDir` for PNG files
2. For each PNG file:
   - Sends the image to xAI API
   - Extracts the number from the model's response
   - Finds the corresponding PDF (same filename, different extension)
   - Copies the PDF to `outputDir` with the new name: `{extracted_number}.pdf`
3. Original files remain unchanged

## File Structure

```
PDF-Doc-Processor/
├── src/
│   └── extract-number.ts    # Main script
├── Docs/
│   ├── output/               # Input files (PDF + PNG pairs)
│   └── renamed/              # Output files (renamed PDFs)
├── config.ts                # Configuration
└── package.json
```

## Example

Input:
- `Docs/output/004884.pdf`
- `Docs/output/004884.png` (contains number "12345")

Output:
- `Docs/renamed/12345.pdf` (copy of original PDF)
