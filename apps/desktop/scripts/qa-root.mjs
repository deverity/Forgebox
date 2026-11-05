// Helpers shared by the real-app QA scripts (ADR-0036): temp portable roots (the
// FORGEBOX_ROOT / portable.flag pattern of crates/forge-cli/tests/common, DNS off, a fake
// Mailpit as the only installed tool), free ports, process-tree cleanup and statistics.
//
//   node scripts/qa-root.mjs create [--repo <dir>]   prints {base, root, gui, repo, ...} as JSON
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const repoRoot = resolve(here, "../../..");

export function freePort() {
    return new Promise((ok, fail) => {
        const srv = createServer();
        srv.once("error", fail);
        srv.listen(0, "127.0.0.1", () => {
            const { port } = srv.address();
            srv.close(() => ok(port));
        });
    });
}

/** Compiles the daemon tests' fake Mailpit (no dependencies) to `out`. */
export function compileFakeMailpit(out, repo = repoRoot) {
    const src = join(repo, "crates", "forge-daemon", "tests", "fixtures", "fake_mailpit.rs");
    execFileSync("rustc", ["--edition", "2024", "-O", src, "-o", out], { stdio: "pipe" });
}

/**
 * A temp base folder with a portable, initialized root (`state/`), a private GUI data dir and,
 * with `fakeMailpit`, `bin/mailpit/1.32.0/mailpit.exe` (polite: stops quickly).
 */
export async function createRoot({ repo = repoRoot, fakeMailpit = true, tag = "e2e" } = {}) {
    const base = mkdtempSync(join(tmpdir(), `forgebox-${tag}-`));
    const root = join(base, "root");
    const gui = join(base, "gui");
    mkdirSync(join(root, "etc"), { recursive: true });
    mkdirSync(join(root, "state"), { recursive: true });
    mkdirSync(gui, { recursive: true });
    writeFileSync(join(root, "portable.flag"), "");
    const ports = { tlsCrl: await freePort(), internal: await freePort() };
    let toml = `[dns]\nenabled = false\n\n[ports.ranges]\ntls-crl = [${ports.tlsCrl}]\ninternal = [${ports.internal}]\n`;
    if (fakeMailpit) {
        const mail = [await freePort(), await freePort()];
        toml += `mail = [${mail[0]}, ${mail[1]}]\n`;
        const dir = join(root, "bin", "mailpit", "1.32.0");
        mkdirSync(dir, { recursive: true });
        const exe = join(base, "fake_mailpit.exe");
        compileFakeMailpit(exe, repo);
        copyFileSync(exe, join(dir, "mailpit.exe"));
        writeFileSync(join(dir, "fake-mode.txt"), "polite");
    }
    writeFileSync(join(root, "etc", "forgebox.toml"), toml);
    return { base, root, gui, repo, registry: join(repo, "registry") };
}

/** `taskkill /T /F`; silent when the process is already gone. */
export function killTree(pid) {
    try {
        execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
    } catch {
        /* already gone */
    }
}

/** PIDs of processes whose command line mentions `needle` (WebView2 children carry the GUI data dir). */
export function pidsMatching(needle) {
    const escaped = needle.replace(/'/g, "''");
    const script = `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.Contains('${escaped}') } | ForEach-Object { $_.ProcessId }`;
    const out = execFileSync("powershell", ["-NoProfile", "-Command", script], {
        encoding: "utf8",
    });
    return out
        .split(/\r?\n/)
        .map((l) => Number(l.trim()))
        .filter((n) => n > 0 && n !== process.pid);
}

/** Working set in MB of every process whose command line mentions `needle` plus `extraPids`. */
export function workingSetMb(needle, extraPids = []) {
    const pids = new Set([...pidsMatching(needle), ...extraPids]);
    if (pids.size === 0) return 0;
    const list = [...pids].join(",");
    const script = `(Get-Process -Id ${list} -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Sum).Sum`;
    const out = execFileSync("powershell", ["-NoProfile", "-Command", script], {
        encoding: "utf8",
    });
    return Math.round(Number(out.trim()) / 1024 / 1024);
}

/** Installed WebView2 runtime version (`pv` of the EdgeUpdate client), or null. */
export function webview2Version() {
    const key = String.raw`SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`;
    for (const hive of ["HKLM", "HKCU"]) {
        try {
            const out = execFileSync("reg", ["query", `${hive}\\${key}`, "/v", "pv"], {
                encoding: "utf8",
                stdio: ["ignore", "pipe", "ignore"],
            });
            const m = /pv\s+REG_SZ\s+(\S+)/.exec(out);
            if (m) return m[1];
        } catch {
            /* try the next hive */
        }
    }
    return null;
}

/** Nearest-rank percentile of a numeric list. */
export function percentile(values, p) {
    const sorted = [...values].sort((a, b) => a - b);
    if (sorted.length === 0) return null;
    const rank = Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1));
    return sorted[rank];
}

/** Median (mean of the two middle values for an even count). */
export function median(values) {
    const sorted = [...values].sort((a, b) => a - b);
    if (sorted.length === 0) return null;
    const mid = sorted.length >> 1;
    return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const [cmd, ...rest] = process.argv.slice(2);
    if (cmd === "create") {
        const i = rest.indexOf("--repo");
        const repo = i >= 0 ? resolve(rest[i + 1]) : repoRoot;
        console.log(JSON.stringify(await createRoot({ repo })));
    } else {
        console.error("usage: node scripts/qa-root.mjs create [--repo <dir>]");
        process.exit(2);
    }
}
