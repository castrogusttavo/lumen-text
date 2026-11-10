// Generates the site's raster assets into public/img (committed). Needs sharp (devDependency).
//   SHOTS_DIR=<folder with editor.png and light.png from `lumen-text --snap`> pnpm images
import sharp from "sharp";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";

const out = new URL("../public/img/", import.meta.url).pathname;
const noise = (id, freq, op) => `<filter id="${id}" x="0" y="0" width="100%" height="100%"><feTurbulence type="fractalNoise" baseFrequency="${freq}" numOctaves="2" stitchTiles="stitch"/><feColorMatrix type="saturate" values="0"/><feComponentTransfer><feFuncA type="linear" slope="${op}"/></feComponentTransfer></filter>`;

async function aurora() {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080">
  <defs>
    <filter id="b" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="120"/></filter>${noise("n", 0.9, 0.5)}
  </defs>
  <rect width="1920" height="1080" fill="#07070a"/>
  <g filter="url(#b)" opacity=".9">
    <ellipse cx="520" cy="200" rx="420" ry="190" fill="#7ea800" opacity=".55"/>
    <ellipse cx="1180" cy="120" rx="520" ry="150" fill="#2f5bff" opacity=".42"/>
    <ellipse cx="1560" cy="330" rx="360" ry="190" fill="#c25cff" opacity=".32"/>
    <ellipse cx="900" cy="330" rx="460" ry="110" fill="#d6f65b" opacity=".22"/>
  </g>
  <rect width="1920" height="1080" filter="url(#n)" opacity=".22" style="mix-blend-mode:overlay"/>
</svg>`;
  await sharp(Buffer.from(svg)).webp({ quality: 74, effort: 6 }).toFile(out + "hero-aurora.webp");
}

async function grain() {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080">
  <defs><filter id="b" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="90"/></filter>${noise("n", 1.1, 0.9)}</defs>
  <rect width="1920" height="1080" fill="#9bb7ff"/>
  <g filter="url(#b)">
    <ellipse cx="330" cy="700" rx="520" ry="420" fill="#6fa3ff"/>
    <ellipse cx="1560" cy="760" rx="520" ry="380" fill="#8fe0b4"/>
    <ellipse cx="900" cy="130" rx="700" ry="260" fill="#ffb347"/>
    <ellipse cx="1350" cy="220" rx="380" ry="170" fill="#ff6b6b"/>
    <ellipse cx="480" cy="220" rx="340" ry="190" fill="#ffe66d"/>
  </g>
  <rect width="1920" height="1080" filter="url(#n)" opacity=".5" style="mix-blend-mode:soft-light"/>
</svg>`;
  await sharp(Buffer.from(svg)).webp({ quality: 72, effort: 6 }).toFile(out + "gradient-grain.webp");
}

async function shots() {
  const dir = process.env.SHOTS_DIR;
  if (!dir) return console.log("SHOTS_DIR not set, keeping existing screenshots");
  for (const [src, name] of [["editor.png", "editor"], ["light.png", "light"], ["git.png", "git"], ["md.png", "md"], ["split.png", "split"], ["palette.png", "palette"]]) {
    if (!existsSync(`${dir}/${src}`)) continue;
    const img = sharp(`${dir}/${src}`).resize(1920, 1200);
    await img.clone().avif({ quality: 55, effort: 6 }).toFile(`${out}shot-${name}.avif`);
    await img.clone().webp({ quality: 80, effort: 6 }).toFile(`${out}shot-${name}.webp`);
  }
}

async function og() {
  const mark = (await readFile(new URL("../public/mark.svg", import.meta.url), "utf8")).replace(/<svg[^>]*>/, "").replace("</svg>", "");
  const bg = await sharp(out + "hero-aurora.webp").resize(1200, 630, { fit: "cover" }).png().toBuffer();
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630">
  <rect width="1200" height="630" fill="#07070a" opacity=".35"/>
  <g transform="translate(80 90) scale(0.052)" fill="#fff">${mark}</g>
  <text x="170" y="138" font-family="Helvetica, Arial, sans-serif" font-size="40" font-weight="600" fill="#f4f4f6">Lumen</text>
  <text x="80" y="340" font-family="Helvetica, Arial, sans-serif" font-size="86" font-weight="600" fill="#f4f4f6" letter-spacing="-3">The lean editor</text>
  <text x="80" y="440" font-family="Helvetica, Arial, sans-serif" font-size="86" font-weight="600" letter-spacing="-3"><tspan fill="#f4f4f6">for </tspan><tspan fill="#d6f65b">{TypeScript}</tspan></text>
  <text x="80" y="548" font-family="Helvetica, Arial, sans-serif" font-size="30" fill="#a1a1ab">Native, CPU-rendered, about 100 MB. Open source, MIT.</text>
</svg>`;
  await sharp(bg).composite([{ input: Buffer.from(svg) }]).png({ compressionLevel: 9 }).toFile(new URL("../public/og.png", import.meta.url).pathname);
}

await aurora(); await grain(); await shots(); await og();
console.log("images done");
