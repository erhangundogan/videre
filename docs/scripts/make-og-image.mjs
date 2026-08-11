// Generates the social card at public/og.png.
//
// Every page declares twitter:card=summary_large_image, which renders a large
// empty box on X, LinkedIn, Slack and Discord unless an og:image exists. One
// static card is a large improvement over none; per-page cards would need a
// rendering dependency this project does not otherwise want.
//
// Run with: yarn og
import sharp from 'sharp';
import { readFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

const W = 1200;
const H = 630;
const BG = '#12141a'; // matches the site's dark surface

// The logo already carries the VIDERE wordmark, so the card must not repeat it
// as text. Mark on the left, wording on the right, which also fills the 1200x630
// frame rather than leaving half of it empty.
const LOGO_W = 260;
const logo = await sharp(join(root, 'src/assets/logo-dark.svg'))
	.resize({ width: LOGO_W })
	.png()
	.toBuffer();
const logoH = (await sharp(logo).metadata()).height;

const TEXT_X = 420;
const text = Buffer.from(`
<svg width="${W}" height="${H}" xmlns="http://www.w3.org/2000/svg">
  <rect width="${W}" height="${H}" fill="${BG}"/>
  <text x="${TEXT_X}" y="290" font-family="Helvetica Neue, Helvetica, Arial, sans-serif"
        font-size="52" font-weight="700" fill="#ffffff">Local-first photo and</text>
  <text x="${TEXT_X}" y="352" font-family="Helvetica Neue, Helvetica, Arial, sans-serif"
        font-size="52" font-weight="700" fill="#ffffff">video library CLI</text>
  <rect x="${TEXT_X}" y="392" width="80" height="4" fill="#5b6cff"/>
  <text x="${TEXT_X}" y="446" font-family="Helvetica Neue, Helvetica, Arial, sans-serif"
        font-size="28" fill="#a6adba">duplicates &#183; semantic search &#183; faces &#183; places</text>
  <text x="${TEXT_X}" y="492" font-family="Helvetica Neue, Helvetica, Arial, sans-serif"
        font-size="26" fill="#6b7280">docs.videre.sh</text>
</svg>`);

mkdirSync(join(root, 'public'), { recursive: true });

await sharp(text)
	.composite([{ input: logo, top: Math.round((H - logoH) / 2), left: 110 }])
	.png({ compressionLevel: 9 })
	.toFile(join(root, 'public/og.png'));

console.log('wrote public/og.png');
