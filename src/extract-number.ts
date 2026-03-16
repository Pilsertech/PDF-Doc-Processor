import * as fs from 'fs';
import * as path from 'path';
import { fileURLToPath } from 'url';
import OpenAI from 'openai';
import * as dotenv from 'dotenv';

dotenv.config();

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const inputDir = process.env.INPUT_DIR || path.join(__dirname, '../Docs/output');
const outputDir = process.env.OUTPUT_DIR || path.join(__dirname, '../Docs/renamed');
const model = process.env.XAI_MODEL || 'grok-4-1-fast-non-reasoning';

const openai = new OpenAI({
  apiKey: process.env.XAI_API_KEY,
  baseURL: 'https://api.x.ai/v1',
});

async function extractNumberFromImage(imagePath: string): Promise<string> {
  const imageBuffer = fs.readFileSync(imagePath);
  const base64Image = imageBuffer.toString('base64');
  const mimeType = path.extname(imagePath).slice(1).toLowerCase();
  
  const response = await openai.chat.completions.create({
    model: model,
    messages: [
      {
        role: 'user',
        content: [
          {
            type: 'text',
            text: 'Extract the Student\'s Application Number from this image. Return ONLY the number with no other text.',
          },
          {
            type: 'image_url',
            image_url: {
              url: `data:image/${mimeType};base64,${base64Image}`,
            },
          },
        ],
      },
    ],
  });

  const result = response.choices[0]?.message?.content?.trim() || '';
  const numberMatch = result.match(/\d+/);
  
  if (!numberMatch) {
    throw new Error(`No number found in image: ${imagePath}`);
  }
  
  return numberMatch[0];
}

async function processFiles(): Promise<void> {
  if (!fs.existsSync(outputDir)) {
    fs.mkdirSync(outputDir, { recursive: true });
  }

  const files = fs.readdirSync(inputDir);
  const pngFiles = files.filter(f => f.endsWith('.png'));

  console.log(`Found ${pngFiles.length} PNG files to process\n`);

  for (const pngFile of pngFiles) {
    const baseName = path.basename(pngFile, '.png');
    const pdfFile = `${baseName}.pdf`;
    const pdfPath = path.join(inputDir, pdfFile);

    if (!fs.existsSync(pdfPath)) {
      console.warn(`⚠️  PDF not found for ${pngFile}, skipping...`);
      continue;
    }

    try {
      console.log(`Processing: ${pngFile}`);
      const extractedNumber = await extractNumberFromImage(path.join(inputDir, pngFile));
      console.log(`  Extracted number: ${extractedNumber}`);

      const outputPath = path.join(outputDir, `${extractedNumber}.pdf`);
      fs.copyFileSync(pdfPath, outputPath);
      console.log(`  ✓ Saved: ${extractedNumber}.pdf\n`);
    } catch (error) {
      console.error(`  ✗ Error processing ${pngFile}:`, error);
    }
  }

  console.log('Done!');
}

processFiles();
