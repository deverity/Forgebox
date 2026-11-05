// `pnpm size` (M2-E1-T21): the size budgets of the shipped GUI (docs/07 section 10, docs/08 section 1).
//
//   node scripts/bundle-size.mjs [--dist dist] [--exe <path to Forgebox.exe>] [--no-fail]
//
// Initial JS = the entry <script> of dist/index.html plus every JS <link rel=modulepreload> it
// lists (what the WebView fetches before the first paint; lazy route chunks are not in it), each
// measured as gzip -9. The exe is the release Forgebox.exe (custom-protocol: dist is embedded).
// Prints one JSON object; exit 1 when a budget is exceeded (unless --no-fail).
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");

/**
 * Re-baselined in M2-E1-T21 (docs/07 section 10): the ~70 kB plan target is below the floor of
 * React 19 + react-dom (~65 kB gzip) plus i18next; measured 163 kB initial JS (KiB, gzip -9),
 * xterm 80 kB as the largest lazy chunk and a 16.2 MiB exe. The exe budget only stops it from
 * growing: docs/08 targets ~8 MB and the reduction is a follow-up.
 */
export const BUDGETS = {
    initialJsGzipKb: 175,
    lazyChunkGzipKb: 100,
    exeMb: 17,
};

/** The exe target of docs/08 section 1 ("~8 MB"); reported next to the budget. */
export const EXE_TARGET_MB = 8;

/**
 * The JS the page loads before first paint: the module entry scripts and the JS modulepreload
 * links of index.html, in document order, without duplicates (paths as written, leading `/` kept).
 */
export function initialAssets(html) {
    const out = [];
    const tags = html.match(/<(script|link)\b[^>]*>/gi) ?? [];
    for (const tag of tags) {
        const attr = (name) => new RegExp(`\\b${name}\\s*=\\s*"([^"]*)"`, "i").exec(tag)?.[1];
        const isScript = /^<script/i.test(tag);
        const url = isScript ? attr("src") : attr("href");
        if (!url || !url.endsWith(".js")) continue;
        if (isScript && attr("type") !== "module") continue;
        if (!isScript && attr("rel") !== "modulepreload") continue;
        if (!out.includes(url)) out.push(url);
    }
    return out;
}

/** gzip -9 size in bytes. */
export function gzipBytes(buffer) {
    return gzipSync(buffer, { level: 9 }).length;
}

const kb = (bytes) => Math.round((bytes / 1024) * 10) / 10;
const mb = (bytes) => Math.round((bytes / (1024 * 1024)) * 100) / 100;

/**
 * Sizes of a built dist folder: the initial set (with its parts) and the largest lazy chunks.
 * `exeBytes` (optional) adds the exe line. Budgets are checked against BUDGETS.
 */
export function measure(dist, exeBytes = null) {
    const html = readFileSync(join(dist, "index.html"), "utf8");
    const initial = initialAssets(html);
    const parts = initial.map((url) => {
        const bytes = readFileSync(join(dist, url.replace(/^\//, "")));
        return {
            file: url.replace(/^\/assets\//, ""),
            raw_kb: kb(bytes.length),
            gzip_kb: kb(gzipBytes(bytes)),
        };
    });
    const initialSet = new Set(initial.map((url) => url.replace(/^\/assets\//, "")));
    const lazy = readdirSync(join(dist, "assets"))
        .filter((f) => f.endsWith(".js") && !initialSet.has(f))
        .map((f) => ({ file: f, gzip_kb: kb(gzipBytes(readFileSync(join(dist, "assets", f)))) }))
        .sort((a, b) => b.gzip_kb - a.gzip_kb);
    const initialGzip = Math.round(parts.reduce((s, p) => s + p.gzip_kb, 0) * 10) / 10;
    const largestLazy = lazy[0]?.gzip_kb ?? 0;
    const failures = [];
    if (initialGzip > BUDGETS.initialJsGzipKb) {
        failures.push(`initial JS ${initialGzip} kB gzip > ${BUDGETS.initialJsGzipKb} kB`);
    }
    if (largestLazy > BUDGETS.lazyChunkGzipKb) {
        failures.push(
            `lazy chunk ${lazy[0].file} ${largestLazy} kB gzip > ${BUDGETS.lazyChunkGzipKb} kB`,
        );
    }
    const exeMb = exeBytes === null ? null : mb(exeBytes);
    if (exeMb !== null && exeMb > BUDGETS.exeMb) {
        failures.push(`Forgebox.exe ${exeMb} MB > ${BUDGETS.exeMb} MB`);
    }
    return {
        initial_js_gzip_kb: initialGzip,
        initial_js_raw_kb: Math.round(parts.reduce((s, p) => s + p.raw_kb, 0) * 10) / 10,
        initial_parts: parts.sort((a, b) => b.gzip_kb - a.gzip_kb),
        lazy_chunks: lazy.length,
        largest_lazy: lazy.slice(0, 5),
        budgets: BUDGETS,
        /** MiB; null without --exe. */
        exe_mb: exeMb,
        exe_target_mb: EXE_TARGET_MB,
        failures,
        passed: failures.length === 0,
    };
}

function arg(name) {
    const i = process.argv.indexOf(`--${name}`);
    return i > 0 && i + 1 < process.argv.length ? process.argv[i + 1] : undefined;
}

function main() {
    const dist = resolve(desktop, arg("dist") ?? "dist");
    if (!existsSync(join(dist, "index.html"))) {
        console.error(`no ${join(dist, "index.html")}: run \`pnpm build\` first`);
        process.exit(2);
    }
    const exe = arg("exe");
    if (exe !== undefined && !existsSync(exe)) {
        console.error(`exe not found: ${exe}`);
        process.exit(2);
    }
    const result = measure(dist, exe === undefined ? null : statSync(exe).size);
    console.log(JSON.stringify(result, null, 2));
    if (!result.passed && !process.argv.includes("--no-fail")) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
