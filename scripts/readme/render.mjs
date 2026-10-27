// Takes the README's screenshots of Loski's cards and setup page.
//
//   1. LOSKI_README_OUT=<out> LOSKI_README_COVERS=<covers> cargo test readme -- --ignored
//   2. node scripts/readme/render.mjs <out>        (needs `npm i playwright`)
//
// Step 1 writes the cards as JSON (plus the artwork, stats and Wrapped
// pictures); this serves them with the repo's icons and fonts, draws each one
// with render.html like Discord would, and saves <out>/<card>.png.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, resolve } from "node:path";
import { chromium } from "playwright";

const out = resolve(process.argv[2] ?? "docs/images");
// The repo, for its icons and fonts (LOSKI_REPO when this runs from elsewhere).
const repo = resolve(process.env.LOSKI_REPO ?? new URL("../..", import.meta.url).pathname.replace(/^\/(\w:)/, "$1"));
const types = { ".html": "text/html", ".json": "application/json", ".png": "image/png", ".jpg": "image/jpeg", ".ttf": "font/ttf" };

// The page, the repo's icons and fonts, and what step 1 made.
const server = createServer(async (req, res) => {
  const path = decodeURIComponent(new URL(req.url, "http://x").pathname);
  const file = path === "/render.html" ? join(repo, "scripts/readme/render.html")
    : path.startsWith("/assets/") ? join(repo, path)
    : join(out, path);
  try {
    res.writeHead(200, { "Content-Type": types[extname(file)] ?? "application/octet-stream" });
    res.end(await readFile(file));
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise(r => server.listen(8123, r));

const browser = await chromium.launch();
const page = await browser.newPage({ deviceScaleFactor: 2, viewport: { width: 700, height: 900 } });
const cards = ["panel", "compact", "lyrics", "queue", "requests", "recap", "settings", "friends", "eq"];
for (const card of cards) {
  await page.goto(`http://localhost:8123/render.html?card=${card}`);
  await page.waitForSelector("body[data-ready=yes]");
  await page.locator("#message").screenshot({ path: join(out, `${card}.png`) });
  console.log(`${card}.png`);
}
// The setup page, before and after the keys are saved.
for (const name of ["setup", "setup-saved"]) {
  await page.setViewportSize({ width: 760, height: 900 });
  await page.goto(`http://localhost:8123/cards/${name}.html`);
  await page.screenshot({ path: join(out, `${name}.png`), fullPage: true });
  console.log(`${name}.png`);
}
await browser.close();
server.close();
