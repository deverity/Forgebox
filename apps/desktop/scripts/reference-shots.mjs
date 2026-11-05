// Curated reference screens in docs/design/screens (M2-E4-T7, docs/design/README.md).
//
//   node scripts/reference-shots.mjs                       # check (default)
//   node scripts/reference-shots.mjs --copy <qa:shots run dir>
//
// --copy takes, for every screen of a `pnpm qa:shots` run, the core cells at 100 % (DPR 1),
// 1280x800, pt-BR, vibrancy on, full motion, in light and dark, and writes them as
// docs/design/screens/<screen>-{light,dark}.png. The check validates every PNG there: real PNG,
// 1280x800 px (100 %), at most 250 KB each and 8 MB in total, and both themes of every screen.
// Prints one JSON object; exit 1 on any problem.
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const SCREENS_DIR = resolve(here, "../../../docs/design/screens");

export const LIMITS = {
    width: 1280,
    height: 800,
    maxFileBytes: 250 * 1024,
    maxTotalBytes: 8 * 1024 * 1024,
};

const NAME = /^([a-z0-9-]+)-(light|dark)\.png$/;

/** Width and height from the IHDR chunk; null when the buffer is not a PNG. */
export function pngSize(buffer) {
    const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if (buffer.length < 24 || signature.some((byte, i) => buffer[i] !== byte)) return null;
    return { width: buffer.readUInt32BE(16), height: buffer.readUInt32BE(20) };
}

/**
 * Problems of a set of reference files (`{name, bytes, size}`; `size` from `pngSize`). Empty when
 * every file is a `<screen>-{light,dark}.png` of 1280x800 within the size limits, every screen has
 * both themes and `expected` (screen ids, optional) are all present.
 */
export function referenceProblems(files, expected = []) {
    const problems = [];
    const themes = new Map();
    let total = 0;
    for (const file of files) {
        total += file.bytes;
        const match = NAME.exec(file.name);
        if (!match) {
            problems.push(`${file.name}: name is not <screen>-{light,dark}.png`);
            continue;
        }
        const [, screen, theme] = match;
        themes.set(screen, [...(themes.get(screen) ?? []), theme]);
        if (file.size === null) {
            problems.push(`${file.name}: not a PNG`);
        } else if (file.size.width !== LIMITS.width || file.size.height !== LIMITS.height) {
            problems.push(
                `${file.name}: ${file.size.width}x${file.size.height}, expected ${LIMITS.width}x${LIMITS.height} (100 %)`,
            );
        }
        if (file.bytes > LIMITS.maxFileBytes) {
            problems.push(`${file.name}: ${file.bytes} bytes > ${LIMITS.maxFileBytes}`);
        }
    }
    for (const [screen, list] of themes) {
        for (const theme of ["light", "dark"]) {
            if (!list.includes(theme)) problems.push(`${screen}: missing ${screen}-${theme}.png`);
        }
    }
    for (const screen of expected) {
        if (!themes.has(screen)) problems.push(`${screen}: no reference screen`);
    }
    if (total > LIMITS.maxTotalBytes) {
        problems.push(`total ${total} bytes > ${LIMITS.maxTotalBytes}`);
    }
    return problems;
}

/** The PNG files of a directory as `{name, bytes, size}`. */
export function readReferences(dir = SCREENS_DIR) {
    if (!existsSync(dir)) return [];
    return readdirSync(dir)
        .filter((name) => name.endsWith(".png"))
        .sort()
        .map((name) => {
            const path = join(dir, name);
            return { name, bytes: statSync(path).size, size: pngSize(readFileSync(path)) };
        });
}

/** Copies the core 100 % cells (`<scenario>-<theme>-1x-vibon-full-pt-BR-1280x800.png`) of a run. */
function copyFromRun(runDir) {
    const copied = [];
    mkdirSync(SCREENS_DIR, { recursive: true });
    for (const screen of readdirSync(runDir)) {
        if (screen.startsWith(".") || screen === "sheets") continue;
        if (!statSync(join(runDir, screen)).isDirectory()) continue;
        for (const theme of ["light", "dark"]) {
            const cell = readdirSync(join(runDir, screen)).find((name) =>
                name.endsWith(`-${theme}-1x-vibon-full-pt-BR-1280x800.png`),
            );
            if (cell === undefined) continue;
            copyFileSync(join(runDir, screen, cell), join(SCREENS_DIR, `${screen}-${theme}.png`));
            copied.push(`${screen}/${cell}`);
        }
    }
    return copied;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const at = process.argv.indexOf("--copy");
    const copied = at === -1 ? [] : copyFromRun(resolve(process.argv[at + 1] ?? "."));
    const files = readReferences();
    const problems = referenceProblems(files);
    const total = files.reduce((sum, f) => sum + f.bytes, 0);
    console.log(
        JSON.stringify(
            {
                dir: SCREENS_DIR,
                copied: copied.length,
                files: files.length,
                totalBytes: total,
                maxFileBytes: Math.max(0, ...files.map((f) => f.bytes)),
                problems,
            },
            null,
            2,
        ),
    );
    if (problems.length > 0) process.exit(1);
}
