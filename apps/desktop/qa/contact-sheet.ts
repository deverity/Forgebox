// Contact sheets and index of `pnpm qa:shots` (M2-E4-T2): one downscaled PNG per screen, so an
// agent reviews a whole screen with a single Read call, plus index.html and manifest.json.
// Runs as the Playwright global teardown, over the `.cells/*.json` files the spec wrote; the
// sheets are rendered with resvg (already a devDependency for the icon pipeline).
import { Resvg } from "@resvg/resvg-js";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

export interface CellRecord {
  screen: string;
  file: string;
  name: string;
  informative: boolean;
  width: number;
  height: number;
  sha256: string;
}

const THUMB_W = 400;
const GAP = 16;
const LABEL_H = 34;
const COLUMNS = 4;

export function readCells(runDir: string): CellRecord[] {
  const dir = path.join(runDir, ".cells");
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.endsWith(".json"))
    .sort()
    .map((name) => JSON.parse(readFileSync(path.join(dir, name), "utf8")) as CellRecord);
}

function escapeXml(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** SVG of one screen's sheet: a grid of downscaled stills, each labelled with its cell name. */
export function sheetSvg(runDir: string, cells: readonly CellRecord[]): string {
  const rows: CellRecord[][] = [];
  for (let i = 0; i < cells.length; i += COLUMNS) rows.push(cells.slice(i, i + COLUMNS));
  const thumbH = (cell: CellRecord) => Math.round((THUMB_W * cell.height) / cell.width);
  const rowHeights = rows.map((row) => Math.max(...row.map(thumbH)) + LABEL_H);
  const width = GAP + COLUMNS * (THUMB_W + GAP);
  const height = GAP + rowHeights.reduce((sum, h) => sum + h + GAP, 0);
  const parts: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="${String(width)}" height="${String(height)}">`,
    `<rect width="100%" height="100%" fill="#808080"/>`,
  ];
  let y = GAP;
  rows.forEach((row, r) => {
    row.forEach((cell, c) => {
      const x = GAP + c * (THUMB_W + GAP);
      const data = readFileSync(path.join(runDir, cell.file)).toString("base64");
      parts.push(
        `<image x="${String(x)}" y="${String(y)}" width="${String(THUMB_W)}" height="${String(thumbH(cell))}" href="data:image/png;base64,${data}"/>`,
        `<text x="${String(x)}" y="${String(y + thumbH(cell) + 14)}" font-family="Consolas, monospace" font-size="11" fill="#ffffff">${escapeXml(cell.name)}</text>`,
        `<text x="${String(x)}" y="${String(y + thumbH(cell) + 28)}" font-family="Consolas, monospace" font-size="10" fill="#e0e0e0">${String(cell.width)}x${String(cell.height)} px${cell.informative ? " (informative)" : ""}</text>`,
      );
    });
    y += (rowHeights[r] ?? 0) + GAP;
  });
  parts.push("</svg>");
  return parts.join("");
}

/** Writes sheets/<screen>.png, manifest.json and index.html; returns the sheet paths. */
export function buildReport(runDir: string): string[] {
  const cells = readCells(runDir);
  const screens = [...new Set(cells.map((c) => c.screen))].sort();
  const sheetsDir = path.join(runDir, "sheets");
  mkdirSync(sheetsDir, { recursive: true });
  const sheets: string[] = [];
  for (const screen of screens) {
    const own = cells
      .filter((c) => c.screen === screen)
      .sort((a, b) => a.name.localeCompare(b.name));
    const svg = sheetSvg(runDir, own);
    const png = new Resvg(svg, { font: { loadSystemFonts: true } }).render().asPng();
    const file = path.join(sheetsDir, `${screen}.png`);
    writeFileSync(file, png);
    sheets.push(file);
  }
  const manifest = Object.fromEntries(cells.map((c) => [c.file, c.sha256]));
  writeFileSync(path.join(runDir, "manifest.json"), JSON.stringify(manifest, null, 2));
  const body = screens
    .map((screen) => {
      const own = cells.filter((c) => c.screen === screen);
      const items = own
        .map(
          (c) =>
            `<li><a href="${escapeXml(c.file)}">${escapeXml(c.name)}</a> ${String(c.width)}x${String(c.height)} <code>${c.sha256.slice(0, 12)}</code></li>`,
        )
        .join("");
      return `<h2>${escapeXml(screen)}</h2><p><a href="sheets/${escapeXml(screen)}.png"><img src="sheets/${escapeXml(screen)}.png" width="960" alt="contact sheet ${escapeXml(screen)}"></a></p><ul>${items}</ul>`;
    })
    .join("");
  writeFileSync(
    path.join(runDir, "index.html"),
    `<!doctype html><meta charset="utf-8"><title>Forgebox qa:shots ${escapeXml(path.basename(runDir))}</title><h1>qa:shots ${escapeXml(path.basename(runDir))} (${String(cells.length)} stills)</h1>${body}`,
  );
  return sheets;
}

/** Playwright global teardown. */
export default function globalTeardown(): void {
  const runDir = process.env.QA_SHOTS_DIR;
  if (runDir === undefined) return;
  const sheets = buildReport(runDir);
  console.log(`qa:shots -> ${runDir} (${String(sheets.length)} contact sheets, index.html)`);
}
