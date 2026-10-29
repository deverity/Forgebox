// Regenerates every brand icon from docs/design/brand (see docs/design/brand/README.md):
//   - apps/desktop/src-tauri/icons/*: the Tauri platform set from the pinned `tauri icon` run on
//     the master render; icon.ico (16/20/24/32 hand-pixeled + 40/48/64/256 from the master) and
//     32x32.png are composed here.
//   - apps/desktop/src-tauri/icons/tray/<state>-<light|dark>.ico (16/20/24/32 px) and the remote
//     overlay remote-<light|dark>.ico, for the tray (M2-E1-T15).
//   - docs/design/brand/forgebox-icon-1024.png and docs/design/brand/contact-sheet.png.
// `--check` writes nothing and exits 1 when a committed file differs from what the sources
// produce (or a stray file sits in icons/ or icons/tray/). No network and no Python: rendering is
// @resvg/resvg-js, the ICO/ICNS writers are below, `tauri icon` is the local devDependency.
import { spawnSync } from "node:child_process";
import {
    existsSync,
    mkdirSync,
    mkdtempSync,
    readdirSync,
    readFileSync,
    rmSync,
    statSync,
    unlinkSync,
    writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

const require = createRequire(import.meta.url);
const { Resvg } = require("@resvg/resvg-js");

const here = dirname(fileURLToPath(import.meta.url));
const appDir = resolve(here, "..");
const repo = resolve(appDir, "../..");
// Tests point these at temporary copies to prove that `--check` catches drift.
const brandDir = process.env.ICONS_BRAND_DIR ?? join(repo, "docs/design/brand");
const targetRoot = process.env.ICONS_TARGET_ROOT ?? repo;

const ICONS = "apps/desktop/src-tauri/icons";
const TRAY = `${ICONS}/tray`;
const BRAND = "docs/design/brand";

/** icon.ico entries: hand-pixeled below 40 px, rendered from the master above. */
export const APP_SIZES = [16, 20, 24, 32, 40, 48, 64, 256];
export const PIXEL_SIZES = [16, 20, 24, 32];
/** Tray sizes: 100/125/150/200 % of the 16 px notification-area icon. */
export const TRAY_SIZES = [16, 20, 24, 32];
export const STARTING_FRAMES = 8;
export const TRAY_STATES = [
    "base",
    "running",
    ...Array.from({ length: STARTING_FRAMES }, (_, i) => `starting-${i}`),
    "stopped",
    "updating",
    "warning",
    "error",
];
export const THEMES = ["light", "dark"];

// --- sources -------------------------------------------------------------------------------

function loadSources() {
    const palette = JSON.parse(readFileSync(join(brandDir, "palette.json"), "utf8"));
    const master = readFileSync(join(brandDir, "forgebox-icon.svg"), "utf8");
    const grids = Object.fromEntries(
        PIXEL_SIZES.map((n) => [
            n,
            parseGrid(readFileSync(join(brandDir, `pixel/forgebox-${n}.txt`), "utf8"), n),
        ]),
    );
    return { palette, master, grids };
}

function parseGrid(text, n) {
    const rows = text.split(/\r?\n/).filter((l) => l.length > 0 && !l.startsWith("#"));
    if (rows.length !== n || rows.some((r) => r.length !== n)) {
        throw new Error(`pixel/forgebox-${n}.txt must be ${n} rows of ${n} characters`);
    }
    return rows;
}

const hex = (h) => [1, 3, 5].map((i) => Number.parseInt(h.slice(i, i + 2), 16));

/** Straight-alpha RGBA of a pixel grid with the palette of `theme` (app | light | dark). */
function gridRgba(rows, palette, theme) {
    const colors = { ...palette.pixel, ...palette.themes[theme] };
    const n = rows.length;
    const out = Buffer.alloc(n * n * 4);
    rows.forEach((row, y) =>
        [...row].forEach((ch, x) => {
            if (!(ch in colors))
                throw new Error(`pixel grid ${n}: unknown character '${ch}' at ${x},${y}`);
            const c = colors[ch];
            if (c === null) return;
            out.set([...hex(c), 255], (y * n + x) * 4);
        }),
    );
    return { size: n, rgba: out };
}

// --- rendering -----------------------------------------------------------------------------

function resvg(svg, width) {
    const opts = { font: { loadSystemFonts: false } };
    if (width) opts.fitTo = { mode: "width", value: width };
    return new Resvg(svg, opts).render();
}

/** resvg pixels are premultiplied; ICO bitmaps and compositing want straight alpha. */
function demultiply(pre) {
    const out = Buffer.from(pre);
    for (let i = 0; i < out.length; i += 4) {
        const a = out[i + 3];
        if (a === 0 || a === 255) continue;
        for (let k = 0; k < 3; k++) out[i + k] = Math.min(255, Math.round((out[i + k] * 255) / a));
    }
    return out;
}

function renderSvg(svg, size) {
    const img = resvg(svg, size);
    return { size, rgba: demultiply(img.pixels), png: img.asPng() };
}

/** Straight-alpha "source over". */
function over(dst, src) {
    const out = Buffer.from(dst);
    for (let i = 0; i < out.length; i += 4) {
        const sa = src[i + 3] / 255;
        if (sa === 0) continue;
        const da = out[i + 3] / 255;
        const oa = sa + da * (1 - sa);
        for (let k = 0; k < 3; k++) {
            out[i + k] = Math.round((src[i + k] * sa + out[i + k] * da * (1 - sa)) / oa);
        }
        out[i + 3] = Math.round(oa * 255);
    }
    return out;
}

/** Removes the base where `mask` (straight RGBA) is opaque: the 1 px knock-out ring. */
function knockOut(dst, mask) {
    const out = Buffer.from(dst);
    for (let i = 0; i < out.length; i += 4) {
        out[i + 3] = Math.round(out[i + 3] * (1 - mask[i + 3] / 255));
        if (out[i + 3] === 0) out.fill(0, i, i + 3);
    }
    return out;
}

/** Encodes straight RGBA as PNG through resvg so that the bytes do not depend on Node's zlib. */
function pngOf(size, rgba) {
    const tmp = rawPng(size, size, rgba);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}"><image width="${size}" height="${size}" image-rendering="optimizeSpeed" href="data:image/png;base64,${tmp.toString("base64")}"/></svg>`;
    return resvg(svg).asPng();
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
});
function crc32(buf) {
    let c = 0xffffffff;
    for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "latin1"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([len, body, crc]);
}
/** Minimal RGBA PNG (only used as an intermediate data URI). */
function rawPng(w, h, rgba) {
    const ihdr = Buffer.alloc(13);
    ihdr.writeUInt32BE(w, 0);
    ihdr.writeUInt32BE(h, 4);
    ihdr[8] = 8;
    ihdr[9] = 6;
    const raw = Buffer.alloc((w * 4 + 1) * h);
    for (let y = 0; y < h; y++) rgba.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4);
    return Buffer.concat([
        Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
        chunk("IHDR", ihdr),
        chunk("IDAT", deflateSync(raw)),
        chunk("IEND", Buffer.alloc(0)),
    ]);
}

// --- ICO / ICNS ----------------------------------------------------------------------------

/**
 * ICO with one entry per image: 32-bit BMP (BGRA + AND mask) below 256 px for the widest
 * compatibility (shell, LoadImage, the tray), PNG for 256 px.
 */
export function encodeIco(images) {
    const sorted = [...images].sort((a, b) => a.size - b.size);
    const blobs = sorted.map((img) => (img.size >= 256 ? img.png : bmpEntry(img.size, img.rgba)));
    const header = Buffer.alloc(6 + 16 * sorted.length);
    header.writeUInt16LE(0, 0);
    header.writeUInt16LE(1, 2);
    header.writeUInt16LE(sorted.length, 4);
    let offset = header.length;
    sorted.forEach((img, i) => {
        const e = 6 + 16 * i;
        header[e] = img.size >= 256 ? 0 : img.size;
        header[e + 1] = img.size >= 256 ? 0 : img.size;
        header.writeUInt16LE(1, e + 4); // planes
        header.writeUInt16LE(32, e + 6); // bit count
        header.writeUInt32LE(blobs[i].length, e + 8);
        header.writeUInt32LE(offset, e + 12);
        offset += blobs[i].length;
    });
    return Buffer.concat([header, ...blobs]);
}

function bmpEntry(n, rgba) {
    const maskStride = Math.ceil(n / 32) * 4;
    const out = Buffer.alloc(40 + n * n * 4 + maskStride * n);
    out.writeUInt32LE(40, 0);
    out.writeInt32LE(n, 4);
    out.writeInt32LE(2 * n, 8); // XOR + AND bitmaps
    out.writeUInt16LE(1, 12);
    out.writeUInt16LE(32, 14);
    out.writeUInt32LE(n * n * 4 + maskStride * n, 20);
    for (let y = 0; y < n; y++) {
        const row = n - 1 - y; // bottom-up
        for (let x = 0; x < n; x++) {
            const s = (y * n + x) * 4;
            const d = 40 + (row * n + x) * 4;
            out[d] = rgba[s + 2];
            out[d + 1] = rgba[s + 1];
            out[d + 2] = rgba[s];
            out[d + 3] = rgba[s + 3];
            if (rgba[s + 3] === 0) {
                const m = 40 + n * n * 4 + row * maskStride + (x >> 3);
                out[m] |= 0x80 >> (x & 7);
            }
        }
    }
    return out;
}

/** `tauri icon` writes the ICNS entries in hash-map order; sort them so the file is stable. */
function normalizeIcns(buf) {
    const entries = [];
    for (let o = 8; o < buf.length;) {
        const len = buf.readUInt32BE(o + 4);
        entries.push({ type: buf.toString("latin1", o, o + 4), bytes: buf.subarray(o, o + len) });
        o += len;
    }
    entries.sort((a, b) => (a.type < b.type ? -1 : a.type > b.type ? 1 : 0));
    const head = Buffer.alloc(8);
    head.write("icns", 0, "latin1");
    head.writeUInt32BE(buf.length, 4);
    return Buffer.concat([head, ...entries.map((e) => e.bytes)]);
}

// --- badges --------------------------------------------------------------------------------

const rect = (x, y, w, h) => `M${x} ${y}h${w}v${h}h${-w}z`;

/**
 * Pixel-aligned white glyphs per badge diameter (d = icon/2), relative to the badge origin.
 * Hand-tuned so that every glyph keeps a clear shape at 16 px.
 */
const GLYPHS = {
    bang: {
        8: [
            [3, 1, 2, 4],
            [3, 6, 2, 1],
        ],
        10: [
            [4, 1, 2, 5],
            [4, 7, 2, 2],
        ],
        12: [
            [5, 2, 2, 5],
            [5, 8, 2, 2],
        ],
        16: [
            [7, 2, 2, 8],
            [7, 11, 2, 2],
        ],
    },
    // The warning triangle is narrow at the top: the "!" starts lower.
    bangTri: {
        8: [
            [3, 2, 2, 3],
            [3, 6, 2, 1],
        ],
        10: [
            [4, 3, 2, 3],
            [4, 7, 2, 2],
        ],
        12: [
            [5, 4, 2, 4],
            [5, 9, 2, 1],
        ],
        16: [
            [7, 5, 2, 5],
            [7, 11, 2, 2],
        ],
    },
    square: { 8: [[2, 2, 4, 4]], 10: [[3, 3, 4, 4]], 12: [[4, 4, 4, 4]], 16: [[5, 5, 6, 6]] },
    arrow: {
        8: [
            [3, 1, 2, 3],
            [1, 4, 6, 1],
            [2, 5, 4, 1],
            [3, 6, 2, 1],
        ],
        10: [
            [4, 1, 2, 4],
            [2, 5, 6, 1],
            [3, 6, 4, 1],
            [4, 7, 2, 1],
        ],
        12: [
            [5, 1, 2, 4],
            [3, 5, 6, 1],
            [4, 6, 4, 1],
            [5, 7, 2, 1],
            [3, 9, 6, 1],
        ],
        16: [
            [7, 2, 2, 6],
            [4, 8, 8, 1],
            [5, 9, 6, 1],
            [6, 10, 4, 1],
            [7, 11, 2, 1],
            [4, 13, 8, 1],
        ],
    },
};
const RING_WIDTH = { 8: 2, 10: 2, 12: 2.5, 16: 3 };

const svgDoc = (n, body) =>
    `<svg xmlns="http://www.w3.org/2000/svg" width="${n}" height="${n}" viewBox="0 0 ${n} ${n}">${body}</svg>`;

/** `{ mask, badge }` SVGs of a tray state at size n (null for the base icon). */
export function badgeSvg(state, n, theme, palette) {
    if (state === "base") return null;
    const b = palette.badges;
    const d = n / 2;
    const o = n - d; // badge origin (bottom-right quadrant)
    const c = n - d / 2;
    const r = d / 2;
    const glyph = (name, color) =>
        `<path shape-rendering="crispEdges" fill="${color}" d="${GLYPHS[name][d].map(([x, y, w, h]) => rect(o + x, o + y, w, h)).join("")}"/>`;
    const disc = (fill) => `<circle cx="${c}" cy="${c}" r="${r}" fill="${fill}"/>`;
    const tri = `M${o - 0.25} ${n} L${n + 0.25} ${n} L${c} ${o - 0.5}Z`;
    const ringMask = `<circle cx="${c}" cy="${c}" r="${r + 1}" fill="#000"/>`;
    if (state === "warning") {
        return {
            mask: svgDoc(
                n,
                `<path d="${tri}" fill="#000" stroke="#000" stroke-width="2" stroke-linejoin="round"/>`,
            ),
            badge: svgDoc(
                n,
                `<path d="${tri}" fill="${b.warning.fill}"/>${glyph("bangTri", b.warning.glyph)}`,
            ),
        };
    }
    let body;
    if (state === "running") body = disc(b.running);
    else if (state === "stopped") body = disc(b.stopped) + glyph("square", b.glyph);
    else if (state === "updating") body = disc(b.updating) + glyph("arrow", b.glyph);
    else if (state === "error") body = disc(b.error) + glyph("bang", b.glyph);
    else if (state.startsWith("starting-")) {
        const frame = Number(state.slice("starting-".length));
        const w = RING_WIDTH[d];
        const rr = r - w / 2;
        const circ = 2 * Math.PI * rr;
        const arc = (circ * 110) / 360;
        // Clockwise spinner: frame i starts at 12 o'clock + 45 deg * i.
        body =
            `<circle cx="${c}" cy="${c}" r="${rr}" fill="none" stroke="${b.starting[theme].track}" stroke-width="${w}"/>` +
            `<circle cx="${c}" cy="${c}" r="${rr}" fill="none" stroke="${b.starting[theme].arc}" stroke-width="${w}" ` +
            `stroke-dasharray="${arc.toFixed(3)} ${circ.toFixed(3)}" transform="rotate(${-90 + 45 * frame} ${c} ${c})"/>`;
    } else throw new Error(`unknown tray state ${state}`);
    return { mask: svgDoc(n, ringMask), badge: svgDoc(n, body) };
}

/** Remote-context overlay (docs/13 section 5): composited by the tray over any state icon. */
export function remoteSvg(n, theme, palette) {
    const rm = palette.remote;
    const d = { 16: 6, 20: 6, 24: 8, 32: 10 }[n];
    const dot = { 16: 2, 20: 2, 24: 2, 32: 4 }[n];
    const c = 1 + d / 2;
    return svgDoc(
        n,
        `<circle cx="${c}" cy="${c}" r="${d / 2 + 1}" fill="${rm.ring[theme]}"/>` +
            `<circle cx="${c}" cy="${c}" r="${d / 2}" fill="${rm.fill}"/>` +
            `<path shape-rendering="crispEdges" fill="${rm.dot}" d="${rect(c - dot / 2, c - dot / 2, dot, dot)}"/>`,
    );
}

// --- outputs -------------------------------------------------------------------------------

function buildIcons(src) {
    const { palette, master, grids } = src;
    const out = new Map();
    const pixel = (n, theme) => gridRgba(grids[n], palette, theme);

    // Master renders.
    const m1024 = renderSvg(master, 1024);
    out.set(`${BRAND}/forgebox-icon-1024.png`, m1024.png);
    const masterAt = Object.fromEntries([40, 48, 64, 256].map((n) => [n, renderSvg(master, n)]));

    // App icon: icon.ico for Forgebox.exe (Tauri resource 32512) and the CLI/daemon EXEs.
    const app = APP_SIZES.map((n) => (n < 40 ? pixel(n, "app") : masterAt[n]));
    out.set(`${ICONS}/icon.ico`, encodeIco(app));

    // Tray icons.
    const tray = {};
    for (const theme of THEMES) {
        for (const state of TRAY_STATES) {
            const images = TRAY_SIZES.map((n) => {
                let rgba = pixel(n, theme).rgba;
                const badge = badgeSvg(state, n, theme, palette);
                if (badge) {
                    rgba = knockOut(rgba, renderSvg(badge.mask).rgba);
                    rgba = over(rgba, renderSvg(badge.badge).rgba);
                }
                return { size: n, rgba };
            });
            tray[`${state}-${theme}`] = images;
            out.set(`${TRAY}/${state}-${theme}.ico`, encodeIco(images));
        }
        const remote = TRAY_SIZES.map((n) => ({
            size: n,
            rgba: renderSvg(remoteSvg(n, theme, palette)).rgba,
        }));
        tray[`remote-${theme}`] = remote;
        out.set(`${TRAY}/remote-${theme}.ico`, encodeIco(remote));
    }

    // Tauri platform set (macOS/Linux/Store PNGs, icns) from the master; the hand-pixeled 32 px
    // replaces the plain downscale.
    for (const [name, bytes] of tauriIcons(m1024.png)) out.set(`${ICONS}/${name}`, bytes);
    out.set(`${ICONS}/32x32.png`, pngOf(32, pixel(32, "app").rgba));

    out.set(`${BRAND}/contact-sheet.png`, contactSheet({ palette, app, tray }));
    return out;
}

function tauriIcons(masterPng) {
    const dir = mkdtempSync(join(tmpdir(), "forgebox-icons-"));
    try {
        const input = join(dir, "master.png");
        writeFileSync(input, masterPng);
        const cli = join(appDir, "node_modules/@tauri-apps/cli/tauri.js");
        const res = spawnSync(process.execPath, [cli, "icon", input, "-o", join(dir, "out")], {
            encoding: "utf8",
        });
        if (res.status !== 0) throw new Error(`tauri icon failed (${res.status}): ${res.stderr}`);
        const files = [];
        for (const name of readdirSync(join(dir, "out")).sort()) {
            const path = join(dir, "out", name);
            // Desktop set only (no android/ios); icon.ico is composed by this script.
            if (statSync(path).isDirectory() || name === "icon.ico") continue;
            const bytes = readFileSync(path);
            files.push([name, name.endsWith(".icns") ? normalizeIcns(bytes) : bytes]);
        }
        return files;
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
}

// --- contact sheet -------------------------------------------------------------------------

// 5x7 pixel font for the labels (no system fonts: the sheet must render the same everywhere).
const FONT = {
    0: "EHJLPHE",
    1: "4C4444E",
    2: "EH1248V",
    3: "V2421HE",
    4: "26AIV22",
    5: "VGU11HE",
    6: "68GUHHE",
    7: "V124888",
    8: "EHHEHHE",
    9: "EHHF12C",
    A: "EHHVHHH",
    B: "UHHUHHU",
    C: "EHGGGHE",
    D: "SIHHHIS",
    E: "VGGUGGV",
    F: "VGGUGGG",
    G: "EHGNHHF",
    H: "HHHVHHH",
    I: "E44444E",
    J: "72222IC",
    K: "HIKOKIH",
    L: "GGGGGGV",
    M: "HRLLHHH",
    N: "HHPLJHH",
    O: "EHHHHHE",
    P: "UHHUGGG",
    Q: "EHHHLID",
    R: "UHHUKIH",
    S: "FGGE11U",
    T: "V444444",
    U: "HHHHHHE",
    V: "HHHHHA4",
    W: "HHHLLLA",
    X: "HHA4AHH",
    Y: "HHHA444",
    Z: "V1248GV",
    "%": "OP248J3",
    "-": "000V000",
    ".": "00000CC",
    "/": "01248G0",
    "(": "2488842",
    ")": "8422248",
    " ": "0000000",
    ":": "0CC0CC0",
    "+": "044V440",
    ",": "0000CC8",
};
const FONT_DIGITS = "0123456789ABCDEFGHIJKLMNOPQRSTUV";
function glyphRows(ch) {
    const code = FONT[ch] ?? FONT[" "];
    return [...code].map((c) => FONT_DIGITS.indexOf(c));
}
function text(x, y, s, color = "#8a8f98", scale = 1) {
    let d = "";
    [...s.toUpperCase()].forEach((ch, i) => {
        glyphRows(ch).forEach((bits, row) => {
            for (let col = 0; col < 5; col++) {
                if (bits & (16 >> col))
                    d += rect(x + (i * 6 + col) * scale, y + row * scale, scale, scale);
            }
        });
    });
    return d ? `<path fill="${color}" shape-rendering="crispEdges" d="${d}"/>` : "";
}

function image(x, y, size, rgba, zoom = 1) {
    const data = rawPng(size, size, rgba).toString("base64");
    return `<image x="${x}" y="${y}" width="${size * zoom}" height="${size * zoom}" image-rendering="optimizeSpeed" href="data:image/png;base64,${data}"/>`;
}

function gray(rgba) {
    const out = Buffer.from(rgba);
    for (let i = 0; i < out.length; i += 4) {
        const l = Math.round(0.2126 * out[i] + 0.7152 * out[i + 1] + 0.0722 * out[i + 2]);
        out.fill(l, i, i + 3);
    }
    return out;
}

function contactSheet({ palette, app, tray }) {
    const tb = palette.taskbars;
    const W = 1480;
    let y = 16;
    let body = text(
        16,
        y,
        "FORGEBOX BRAND ICONS - GENERATED FROM DOCS/DESIGN/BRAND BY APPS/DESKTOP/SCRIPTS/GEN-ICONS.MJS",
        "#c8ccd4",
        2,
    );
    y += 30;

    // A. App icon entries at 1:1 on every taskbar colour, and the hand-pixeled sizes at 4x.
    body += text(
        16,
        y,
        "APP ICON (ICON.ICO, FORGEBOX.EXE AND THE CLI/DAEMON EXES) - 1:1",
        "#c8ccd4",
        2,
    );
    y += 24;
    for (const [name, color] of Object.entries(tb).filter(([k]) => !k.startsWith("$"))) {
        const h = 310;
        body += `<rect x="0" y="${y}" width="${W}" height="${h}" fill="${color}"/>`;
        const ink = hex(color)[0] > 128 ? "#5a5f68" : "#9aa0aa";
        body += text(16, y + 8, name, ink, 2);
        let x = 16;
        for (const img of app) {
            body += image(x, y + 34, img.size, img.rgba);
            body += text(x, y + 34 + img.size + 6, `${img.size}`, ink);
            x += img.size + 18;
        }
        for (const img of app.filter((a) => a.size <= 32)) {
            body += image(x, y + 34, img.size, img.rgba, 4);
            body += text(x, y + 34 + img.size * 4 + 6, `${img.size} X4`, ink);
            x += img.size * 4 + 14;
        }
        y += h;
    }

    // B. Tray: every state at every size (100/125/150/200 %) on the matching taskbars, 16 px at 4x.
    y += 12;
    body += text(
        16,
        y,
        "TRAY ICONS (ICONS/TRAY) - LIGHT VARIANT ON LIGHT TASKBARS, DARK VARIANT ON DARK TASKBARS - 1:1 AND 16 PX X4",
        "#c8ccd4",
        2,
    );
    y += 24;
    const rows = [...TRAY_STATES, "remote", "remote+running", "remote+starting-0"];
    const rowH = 70;
    const sizesW = TRAY_SIZES.reduce((a, n) => a + n + 12, 0);
    const subW = sizesW + 76; // 1:1 sizes, then the 16 px icon at 4x
    const blockW = W / 2;
    const top = y;
    const blockH = 34 + rows.length * rowH;
    const pick = (theme, row, si) => {
        const remote = tray[`remote-${theme}`][si].rgba;
        if (row === "remote") return remote;
        if (row.startsWith("remote+"))
            return over(tray[`${row.slice(7)}-${theme}`][si].rgba, remote);
        return tray[`${row}-${theme}`][si].rgba;
    };
    [
        ["light", ["win11-light", "win10-light"]],
        ["dark", ["win11-dark", "win10-dark"]],
    ].forEach(([theme, bgs], bi) => {
        const bx = bi * blockW;
        const ink = theme === "light" ? "#5a5f68" : "#9aa0aa";
        body += `<rect x="${bx}" y="${top}" width="${blockW}" height="${blockH}" fill="${tb[bgs[0]]}"/>`;
        body += text(bx + 10, top + 8, `${theme} variant`, ink);
        rows.forEach(
            (row, ri) =>
                (body += text(bx + 10, top + 34 + ri * rowH + 26, row.replace("+", " + "), ink)),
        );
        bgs.forEach((bg, k) => {
            const sx = bx + 150 + k * (subW + 16);
            body += `<rect x="${sx - 8}" y="${top}" width="${subW + 12}" height="${blockH}" fill="${tb[bg]}"/>`;
            body += text(sx, top + 8, bg, ink);
            let cx = sx;
            for (const n of TRAY_SIZES) {
                body += text(cx, top + 20, `${(n / 16) * 100}%`, ink);
                cx += n + 12;
            }
            body += text(cx, top + 20, "16 X4", ink);
            rows.forEach((row, ri) => {
                const ry = top + 34 + ri * rowH;
                let x = sx;
                TRAY_SIZES.forEach((n, si) => {
                    body += image(x, ry + 48 - n, n, pick(theme, row, si));
                    x += n + 12;
                });
                body += image(x, ry, 16, pick(theme, row, 0), 4);
            });
        });
    });
    y = top + blockH + 12;

    // C. Shape-only check: the 16 px tray icons in greyscale (badges must differ by shape).
    body += text(
        16,
        y,
        "SHAPES ONLY: 16 PX TRAY ICONS IN GREYSCALE, X4 (DARK VARIANT ON WIN10-DARK, LIGHT VARIANT ON WIN11-LIGHT)",
        "#c8ccd4",
        2,
    );
    y += 24;
    const shapeStates = [
        "base",
        "running",
        "starting-0",
        "stopped",
        "updating",
        "warning",
        "error",
    ];
    for (const [theme, bg] of [
        ["dark", tb["win10-dark"]],
        ["light", tb["win11-light"]],
    ]) {
        body += `<rect x="0" y="${y}" width="${W}" height="96" fill="${bg}"/>`;
        shapeStates.forEach((st, i) => {
            body += image(16 + i * 92, y + 8, 16, gray(tray[`${st}-${theme}`][0].rgba), 4);
            body += text(16 + i * 92, y + 76, st, theme === "dark" ? "#9aa0aa" : "#5a5f68");
        });
        y += 96;
    }
    y += 8;
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${W}" height="${y}"><rect width="${W}" height="${y}" fill="#16181d"/>${body}</svg>`;
    return resvg(svg).asPng();
}

// --- write / check -------------------------------------------------------------------------

function ownedDirs() {
    return [ICONS, TRAY];
}

function strayFiles(outputs) {
    const stray = [];
    for (const dir of ownedDirs()) {
        const abs = join(targetRoot, dir);
        if (!existsSync(abs)) continue;
        for (const name of readdirSync(abs)) {
            const rel = `${dir}/${name}`;
            if (statSync(join(abs, name)).isDirectory()) continue;
            if (!outputs.has(rel)) stray.push(rel);
        }
    }
    return stray;
}

function main() {
    const check = process.argv.includes("--check");
    const outputs = buildIcons(loadSources());
    const drift = [];
    for (const [rel, bytes] of outputs) {
        const abs = join(targetRoot, rel);
        const same = existsSync(abs) && readFileSync(abs).equals(bytes);
        if (same) continue;
        if (check) drift.push(rel);
        else {
            mkdirSync(dirname(abs), { recursive: true });
            writeFileSync(abs, bytes);
        }
    }
    for (const rel of strayFiles(outputs)) {
        if (check) drift.push(`${rel} (not produced by the generator)`);
        else unlinkSync(join(targetRoot, rel));
    }
    if (check && drift.length > 0) {
        process.stderr.write(
            `icons drift: ${drift.length} file(s) differ from docs/design/brand; run \`pnpm -C apps/desktop icons\`:\n` +
                drift.map((d) => `  ${d}\n`).join(""),
        );
        process.exit(1);
    }
    if (!check) {
        process.stdout.write(
            `icons: ${outputs.size} files from ${relative(repo, brandDir) || brandDir}\n`,
        );
    }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
