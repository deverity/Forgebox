// Real-window checks of the shell (M2-E1-T7, ADR-0035) against a built Forgebox.exe
// (`pnpm tauri build`, embeds dist/). Dev script; the Win32 side is scripts/qa/window-probe.ps1.
//
//   node scripts/window-qa.mjs [--exe <path>] [--checks perf,startup,single,placement,flash,pacing]
//                              [--runs N]
//
// * perf    `Forgebox.exe --perf-startup` N times: valid JSON on stdout, exit 0 (informative
//           numbers; the formal cold start is the quiet-slot gate, ADR-0036).
// * startup the window stays hidden until `ready`: the probe starts polling before the process
//           exists, captures the first visible frame (no white or black flash) and the
//           visible time is compared with `ready_unix_ms` (perf_startup); data-theme/vibrancy
//           are read over CDP.
// * single  a second launch exits 0 and restores + shows the first window (minimized first).
// * placement maximize, close (without a tray the GUI exits), reopen: comes back maximized.
// * minimized `--minimized` stays hidden (6 s, well past `ready` and the watchdog); a plain
//           second launch shows it.
// * flash   N launches alternating the system and the dark theme: how the first visible frame
//           looks (none / black / white / empty-canvas) and 100 ms later.
// * pacing  frame pacing with FORGE_GUI_EFFECT=none and =acrylic: rAF intervals in the webview
//           (CDP) plus SetWindowPos latency and DWM counters while the probe drags/resizes it.
//
// Every run uses a temp FORGE_GUI_DATA_DIR and checks that Tauri's default folders
// (%APPDATA%\dev.forgebox.app, %LOCALAPPDATA%\dev.forgebox.app) are not created or touched.
// The CDP port is opened with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS in this script's child
// environment only (never in code or release config). Teardown kills every process it started.
import { execFileSync, spawn } from "node:child_process";
import {
    copyFileSync,
    existsSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    readdirSync,
    rmSync,
    statSync,
    writeFileSync,
} from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const probe = join(repo, "scripts", "qa", "window-probe.ps1");

function arg(name, fallback) {
    const i = process.argv.indexOf(`--${name}`);
    return i > 0 && i + 1 < process.argv.length ? process.argv[i + 1] : fallback;
}
const exe = resolve(arg("exe", join(repo, "target", "release", "Forgebox.exe")));
const checks = new Set(arg("checks", "perf,startup,single,placement,minimized,flash").split(","));
const runs = Number(arg("runs", "5"));
const pids = new Set();
const temps = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function freePort() {
    return new Promise((ok, fail) => {
        const srv = createServer();
        srv.once("error", fail);
        srv.listen(0, "127.0.0.1", () => {
            const { port } = srv.address();
            srv.close(() => ok(port));
        });
    });
}

function tempDir(tag) {
    const dir = mkdtempSync(join(tmpdir(), `forgebox-qa-${tag}-`));
    temps.push(dir);
    return dir;
}

function env(extra) {
    // Never inherit a debugging port or an effect override from the caller.
    const base = { ...process.env };
    delete base.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS;
    delete base.FORGE_GUI_EFFECT;
    return { ...base, ...extra };
}

function runProbe(args, childEnv) {
    const out = execFileSync("powershell", ["-NoProfile", "-File", probe, ...args], {
        env: childEnv,
        encoding: "utf8",
        timeout: 120_000,
    });
    const line = out.trim().split(/\r?\n/).filter(Boolean).at(-1);
    return JSON.parse(line);
}

/** Snapshot of Tauri's default app folders (must not change: everything goes to the data dir). */
function tauriDefaultDirs() {
    const dirs = [
        join(process.env.APPDATA ?? "", "dev.forgebox.app"),
        join(process.env.LOCALAPPDATA ?? "", "dev.forgebox.app"),
    ];
    return dirs.map((d) => {
        if (!existsSync(d)) return `${d}: absent`;
        // Recursive: a write deep inside (e.g. an old EBWebView profile) must show too.
        let files = 0;
        let latest = statSync(d).mtimeMs;
        for (const rel of readdirSync(d, { recursive: true })) {
            try {
                const st = statSync(join(d, String(rel)));
                files += 1;
                latest = Math.max(latest, st.mtimeMs);
            } catch {
                /* vanished while listing */
            }
        }
        return `${d}: ${files} entries, latest ${new Date(latest).toISOString()}`;
    });
}

async function cdp(port) {
    let target;
    for (let i = 0; i < 200 && !target; i++) {
        try {
            const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
            target = list.find((t) => t.type === "page" && t.url.includes("tauri.localhost"));
        } catch {
            /* not up yet */
        }
        if (!target) await sleep(50);
    }
    if (!target) throw new Error(`no CDP page on port ${port}`);
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((ok, fail) => {
        ws.onopen = ok;
        ws.onerror = fail;
    });
    let id = 0;
    const waiting = new Map();
    ws.onmessage = (msg) => {
        const data = JSON.parse(msg.data);
        if (data.id && waiting.has(data.id)) {
            waiting.get(data.id)(data);
            waiting.delete(data.id);
        }
    };
    async function evaluate(expression) {
        const n = ++id;
        const answer = new Promise((ok) => waiting.set(n, ok));
        ws.send(
            JSON.stringify({
                id: n,
                method: "Runtime.evaluate",
                params: { expression, awaitPromise: true, returnByValue: true },
            }),
        );
        const res = await answer;
        if (res.result?.exceptionDetails) {
            throw new Error(JSON.stringify(res.result.exceptionDetails));
        }
        return res.result?.result?.value;
    }
    return { evaluate, close: () => ws.close() };
}

function kill(pid) {
    try {
        execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
    } catch {
        /* already gone */
    }
    pids.delete(pid);
}

async function launchWithProbe(tag, extraEnv = {}, prefs = null) {
    const port = await freePort();
    const dataDir = join(tempDir(tag), "gui");
    if (prefs) {
        mkdirSync(dataDir, { recursive: true });
        writeFileSync(join(dataDir, "prefs.json"), JSON.stringify(prefs));
    }
    const png = join(dataDir, "..", "first-visible.png");
    const childEnv = env({
        FORGE_GUI_DATA_DIR: dataDir,
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port}`,
        ...extraEnv,
    });
    const res = runProbe(["-Action", "Launch", "-Exe", exe, "-Png", png], childEnv);
    lastPng = png;
    pids.add(res.pid);
    const page = await cdp(port);
    return { res, page, port, dataDir, png, childEnv };
}

/** What the first visible frame looked like (probe stats). */
function flash(frame) {
    if (!frame?.capture) return "unknown";
    if (frame.content_ratio > 0.0005) return "none";
    if (frame.mode_luma < 10) return "black";
    if (frame.mode_luma >= 254) return "white";
    return "empty-canvas";
}

function stats(values) {
    const v = [...values].sort((a, b) => a - b);
    if (v.length === 0) return null;
    const q = (p) => v[Math.min(v.length - 1, Math.round(p * (v.length - 1)))];
    const r = (x) => Math.round(x * 100) / 100;
    return {
        n: v.length,
        p50: r(q(0.5)),
        p95: r(q(0.95)),
        p99: r(q(0.99)),
        max: r(v[v.length - 1]),
        over_25ms: v.filter((x) => x > 25).length,
    };
}

const report = { exe, checks: [...checks] };
// Captures worth a look go to apps/desktop/qa/out/window-qa (gitignored); temp dirs are removed.
const outDir = join(here, "..", "qa", "out", "window-qa");
let lastPng = "";
function keepPng(name, suffix) {
    if (!lastPng || !existsSync(lastPng)) return null;
    mkdirSync(outDir, { recursive: true });
    const target = join(outDir, `${name}-${suffix}.png`);
    copyFileSync(lastPng, target);
    return target;
}

async function checkPerf() {
    const lines = [];
    // One profile for every run (the first one creates it: a warm-up, reported apart).
    const dataDir = join(tempDir("perf"), "gui");
    for (let i = 0; i <= runs; i++) {
        const started = Date.now();
        const out = await new Promise((ok, fail) => {
            const child = spawn(exe, ["--perf-startup"], {
                env: env({ FORGE_GUI_DATA_DIR: dataDir }),
                stdio: ["ignore", "pipe", "pipe"],
            });
            pids.add(child.pid);
            let stdout = "";
            child.stdout.on("data", (d) => (stdout += d));
            const timer = setTimeout(
                () => fail(new Error("perf-startup did not exit in 60 s")),
                60_000,
            );
            child.on("exit", (code) => {
                clearTimeout(timer);
                pids.delete(child.pid);
                ok({ code, stdout, wall_ms: Date.now() - started });
            });
        });
        const json = JSON.parse(out.stdout.trim());
        if (out.code !== 0 || typeof json.first_frame_ms !== "number") {
            throw new Error(`--perf-startup run ${i}: exit ${out.code}, ${out.stdout}`);
        }
        lines.push({ ...json, exit: out.code, wall_ms: out.wall_ms });
        await sleep(500);
    }
    const ff = lines.slice(1).map((l) => l.first_frame_ms);
    report.perf = {
        warm_up_first_frame_ms: lines[0].first_frame_ms,
        runs: lines,
        first_frame_ms: stats(ff),
    };
}

async function checkStartupAndSingle() {
    // The pinned dark theme first (prefs.json), then the system theme, which stays open for
    // the single-instance check.
    const dark = await launchWithProbe("startup-dark", {}, { theme: "dark" });
    const darkHtml = await dark.page.evaluate(
        `({theme: document.documentElement.dataset.theme, vibrancy: document.documentElement.dataset.vibrancy})`,
    );
    report.startup_dark = {
        probe: dark.res,
        html: darkHtml,
        first_frame_flash: flash(dark.res),
        after_100ms: flash(dark.res.after_100ms),
        first_frame_png: dark.res.capture ? keepPng("first-visible", "dark-pref") : null,
    };
    dark.page.close();
    kill(dark.res.pid);
    if (darkHtml.theme !== "dark" || report.startup_dark.first_frame_flash !== "none") {
        throw new Error(`dark start: ${JSON.stringify(report.startup_dark)}`);
    }
    await sleep(1000);

    const { res, page, childEnv } = await launchWithProbe("startup");
    const state = await page.evaluate(`(async () => ({
      theme: document.documentElement.dataset.theme ?? null,
      vibrancy: document.documentElement.dataset.vibrancy ?? null,
      effect: document.documentElement.dataset.effect ?? null,
      boot: window.__FORGEBOX_SHELL__ ?? null,
      perf: await window.__TAURI_INTERNALS__.invoke("perf_startup"),
      appearance: await window.__TAURI_INTERNALS__.invoke("appearance_get"),
    }))()`);
    const shownAfterReady = res.visible_unix_ms >= state.perf.ready_unix_ms;
    report.startup = {
        probe: res,
        html: { theme: state.theme, vibrancy: state.vibrancy, effect: state.effect },
        appearance: state.appearance,
        perf: state.perf,
        shown_after_ready: shownAfterReady,
        first_frame_flash: flash(res),
        after_100ms: flash(res.after_100ms),
        first_frame_png: res.capture ? keepPng("first-visible", state.theme) : null,
    };
    if (!shownAfterReady) throw new Error("window visible before ready");
    if (report.startup.first_frame_flash !== "none") {
        throw new Error(`the first visible frame is ${report.startup.first_frame_flash}`);
    }

    if (checks.has("single")) {
        await page.evaluate(
            `window.__TAURI_INTERNALS__.invoke("plugin:window|minimize", { label: "main" })`,
        );
        await sleep(500);
        const before = runProbe(["-Action", "Info", "-ProcessId", String(res.pid)], childEnv);
        const second = await new Promise((ok) => {
            const started = Date.now();
            const child = spawn(exe, [], { env: childEnv, stdio: "ignore" });
            pids.add(child.pid);
            child.on("exit", (code) => {
                pids.delete(child.pid);
                ok({ code, ms: Date.now() - started });
            });
        });
        await sleep(700);
        const after = runProbe(["-Action", "Info", "-ProcessId", String(res.pid)], childEnv);
        report.single = { before, second_launch: second, after };
        if (second.code !== 0 || !after.visible || after.minimized) {
            throw new Error(`single instance: ${JSON.stringify(report.single)}`);
        }
    }
    page.close();
    kill(res.pid);
}

/** Maximize, close (no tray yet: the GUI exits), reopen with the same data dir: maximized. */
async function checkPlacement() {
    const first = await launchWithProbe("placement");
    await first.page.evaluate(
        `window.__TAURI_INTERNALS__.invoke("plugin:window|toggle_maximize", { label: "main" })`,
    );
    await sleep(500);
    const maximized = runProbe(
        ["-Action", "Info", "-ProcessId", String(first.res.pid)],
        first.childEnv,
    );
    // Closing resolves only if the window survives it, so it is not awaited.
    void first.page
        .evaluate(`window.__TAURI_INTERNALS__.invoke("plugin:window|close", { label: "main" })`)
        .catch(() => undefined);
    let exited = false;
    for (let i = 0; i < 100 && !exited; i++) {
        await sleep(100);
        try {
            process.kill(first.res.pid, 0);
        } catch {
            exited = true;
        }
    }
    first.page.close();
    if (exited) pids.delete(first.res.pid);
    const saved = JSON.parse(readFileSync(join(first.dataDir, "window-state.json"), "utf8"));
    const second = runProbe(
        ["-Action", "Launch", "-Exe", exe],
        env({ FORGE_GUI_DATA_DIR: first.dataDir }),
    );
    pids.add(second.pid);
    await sleep(500);
    const restored = runProbe(
        ["-Action", "Info", "-ProcessId", String(second.pid)],
        first.childEnv,
    );
    kill(second.pid);
    report.placement = { maximized, closed_and_exited: exited, saved, restored };
    if (!exited || !saved.placements?.[0]?.maximized || !restored.maximized) {
        throw new Error(`placement: ${JSON.stringify(report.placement)}`);
    }
}

/** `--minimized` keeps the window hidden past `ready`; a plain second launch shows it. */
async function checkMinimized() {
    const dataDir = join(tempDir("minimized"), "gui");
    const childEnv = env({ FORGE_GUI_DATA_DIR: dataDir });
    const res = runProbe(
        ["-Action", "Launch", "-Exe", exe, "-ExeArgs", "--minimized", "-TimeoutMs", "6000"],
        childEnv,
    );
    pids.add(res.pid);
    const second = await new Promise((ok) => {
        const child = spawn(exe, [], { env: childEnv, stdio: "ignore" });
        child.on("exit", (code) => ok(code));
    });
    await sleep(700);
    const after = runProbe(["-Action", "Info", "-ProcessId", String(res.pid)], childEnv);
    kill(res.pid);
    report.minimized = {
        hidden_for_6s: res.visible === false && res.window_found === true,
        second_launch_exit: second,
        after,
    };
    if (!report.minimized.hidden_for_6s || second !== 0 || !after.visible) {
        throw new Error(`minimized: ${JSON.stringify(report.minimized)}`);
    }
}

/** N launches (light/system and dark alternating): how the first visible frame looks. */
async function checkFlash() {
    const frames = [];
    for (let i = 0; i < runs; i++) {
        const theme = i % 2 === 0 ? "system" : "dark";
        const dataDir = join(tempDir("flash"), "gui");
        mkdirSync(dataDir, { recursive: true });
        writeFileSync(join(dataDir, "prefs.json"), JSON.stringify({ theme }));
        const res = runProbe(
            ["-Action", "Launch", "-Exe", exe, "-Png", join(dataDir, "..", "f.png")],
            env({ FORGE_GUI_DATA_DIR: dataDir }),
        );
        pids.add(res.pid);
        frames.push({
            theme,
            visible_ms: res.visible_ms,
            first: flash(res),
            first_mode_luma: res.mode_luma,
            after_100ms: flash(res.after_100ms),
        });
        if (flash(res) !== "none") lastPng = join(dataDir, "..", "f.png");
        if (flash(res) !== "none") keepPng(`flash-${i}`, flash(res));
        kill(res.pid);
        await sleep(800);
    }
    const count = (k) => frames.filter((f) => f.first === k).length;
    report.flash = {
        runs: frames,
        first_frame: {
            none: count("none"),
            black: count("black"),
            white: count("white"),
            empty: count("empty-canvas"),
        },
    };
}

async function checkPacing() {
    report.pacing = {};
    for (const effect of ["none", "acrylic"]) {
        const { res, page, childEnv } = await launchWithProbe(`pacing-${effect}`, {
            FORGE_GUI_EFFECT: effect,
        });
        await sleep(1500);
        const html = await page.evaluate(
            `({vibrancy: document.documentElement.dataset.vibrancy, effect: document.documentElement.dataset.effect})`,
        );
        await page.evaluate(`(() => {
      window.__fp = [];
      const tick = (t) => { window.__fp.push(t); requestAnimationFrame(tick); };
      requestAnimationFrame(tick);
      return true;
    })()`);
        const phases = {};
        for (const mode of ["idle", "drag", "resize"]) {
            await page.evaluate(`(window.__fp.length = 0, true)`);
            const motion = runProbe(
                [
                    "-Action",
                    "Motion",
                    "-ProcessId",
                    String(res.pid),
                    "-Mode",
                    mode,
                    "-DurationMs",
                    "4000",
                ],
                childEnv,
            );
            const stamps = await page.evaluate(`window.__fp.slice()`);
            const intervals = stamps.slice(1).map((t, i) => t - stamps[i]);
            phases[mode] = { raf_interval_ms: stats(intervals), ...motion };
            await sleep(300);
        }
        report.pacing[effect] = { html, phases };
        page.close();
        kill(res.pid);
        await sleep(1000);
    }
}

const dirsBefore = tauriDefaultDirs();
let failure;
try {
    if (!existsSync(exe)) throw new Error(`${exe} not found (pnpm tauri build)`);
    if (checks.has("perf")) await checkPerf();
    if (checks.has("startup") || checks.has("single")) await checkStartupAndSingle();
    if (checks.has("placement")) await checkPlacement();
    if (checks.has("minimized")) await checkMinimized();
    if (checks.has("flash")) await checkFlash();
    if (checks.has("pacing")) await checkPacing();
} catch (error) {
    failure = error;
} finally {
    for (const pid of [...pids]) kill(pid);
    await sleep(1500);
    report.tauri_default_dirs_untouched =
        JSON.stringify(tauriDefaultDirs()) === JSON.stringify(dirsBefore);
    report.tauri_default_dirs = tauriDefaultDirs();
    for (const dir of temps) {
        try {
            rmSync(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 });
        } catch {
            /* WebView2 may hold a file for a moment */
        }
    }
}
console.log(JSON.stringify(report, null, 2));
if (failure) {
    console.error(failure);
    process.exit(1);
}
if (!report.tauri_default_dirs_untouched) {
    console.error("Tauri's default app folders changed during the run");
    process.exit(1);
}
