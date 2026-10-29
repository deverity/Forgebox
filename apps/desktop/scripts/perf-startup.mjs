// Cold start of the release Forgebox.exe (`pnpm perf:startup`, ADR-0036).
//
// Definition: release exe, warm WebView2 profile (kept across the runs in a temp GUI data dir),
// no Forgebox GUI running; measured from the OS creation time of the process to the `ready`
// command (double requestAnimationFrame after the first shell commit, when the window is shown)
// -- exactly what `Forgebox.exe --perf-startup` prints as `first_frame_ms`. One discarded
// warm-up run plus N (default 10), with the daemon running and with it stopped.
//
//   node scripts/perf-startup.mjs [--exe <path>] [--runs 10] [--budget-ms 800]
//                                 [--daemon both|running|stopped] [--budget-factor 1]
//
// Prints one JSON object on stdout: {median, p90, p95, runs, session, webview2_version, rss_mb,
// budget_ms, passed, modes: {daemon_running: {...}, daemon_stopped: {...}}}; the top-level
// numbers are those of the slowest mode. Exit 1 when any median is >= the budget (CI passes
// --budget-factor 1.5 and reports the number: shared runners are noisy). Run it in a quiet
// machine window; the session type (rdp/console) is recorded because it shifts the numbers.
import { spawn } from "node:child_process";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";
import {
    createRoot,
    killTree,
    median,
    percentile,
    repoRoot,
    webview2Version,
    workingSetMb,
} from "./qa-root.mjs";

function arg(name, fallback) {
    const i = process.argv.indexOf(`--${name}`);
    return i > 0 && i + 1 < process.argv.length ? process.argv[i + 1] : fallback;
}

const exe = resolve(arg("exe", join(repoRoot, "target", "release", "Forgebox.exe")));
const daemonExe = resolve(arg("daemon-exe", join(exe, "..", "forgeboxd.exe")));
const runs = Number(arg("runs", "10"));
const budget = Number(arg("budget-ms", "800")) * Number(arg("budget-factor", "1"));
const modes = arg("daemon", "both") === "both" ? ["running", "stopped"] : [arg("daemon", "both")];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

if (!existsSync(exe)) {
    console.error(
        `release exe not found: ${exe} (cargo build --release -p forgebox-desktop --features custom-protocol)`,
    );
    process.exit(2);
}

function childEnv(ctx) {
    const env = { ...process.env, FORGEBOX_ROOT: ctx.root, FORGE_GUI_DATA_DIR: ctx.gui };
    env.FORGEBOX_OFFICIAL_REGISTRY = ctx.registry;
    env.FORGE_DAEMON_EXE = daemonExe;
    delete env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS;
    delete env.FORGE_GUI_EFFECT;
    return env;
}

function daemonPid(ctx) {
    try {
        return JSON.parse(readFileSync(join(ctx.root, "state", "locks", "forgeboxd.lock"), "utf8"))
            .pid;
    } catch {
        return null;
    }
}

async function startDaemon(ctx) {
    const child = spawn(daemonExe, ["--root", ctx.root], {
        env: childEnv(ctx),
        stdio: "ignore",
        detached: false,
    });
    for (let i = 0; i < 200; i++) {
        if (daemonPid(ctx)) return child;
        await sleep(100);
    }
    killTree(child.pid);
    throw new Error("forgeboxd did not start in 20 s");
}

function stopDaemon(ctx, child) {
    const pid = daemonPid(ctx);
    if (pid) killTree(pid);
    if (child?.pid) killTree(child.pid);
}

/** "stopped" mode: no daemon may survive a previous run (it outlives the GUI that started it). */
async function ensureDaemonStopped(ctx) {
    const lock = join(ctx.root, "state", "locks", "forgeboxd.lock");
    const pid = daemonPid(ctx);
    if (pid) killTree(pid);
    for (let i = 0; i < 50; i++) {
        if (!existsSync(lock)) return;
        await sleep(100);
        // The lock file is kept after a kill on some paths; a dead pid means it is stale.
        const current = daemonPid(ctx);
        if (!current || current === pid) {
            try {
                process.kill(current ?? pid, 0);
            } catch {
                rmSync(lock, { force: true });
            }
        }
    }
    if (existsSync(lock)) throw new Error("forgeboxd.lock is still present: the daemon is running");
}

function oneRun(ctx) {
    return new Promise((ok, fail) => {
        const child = spawn(exe, ["--perf-startup"], {
            env: childEnv(ctx),
            stdio: ["ignore", "pipe", "pipe"],
        });
        let out = "";
        child.stdout.on("data", (d) => (out += d));
        const timer = setTimeout(() => {
            killTree(child.pid);
            fail(new Error("perf-startup did not exit in 60 s"));
        }, 60_000);
        child.on("exit", (code) => {
            clearTimeout(timer);
            const line = out.trim().split(/\r?\n/).filter(Boolean).at(-1) ?? "";
            try {
                const json = JSON.parse(line);
                if (code !== 0 || typeof json.first_frame_ms !== "number") {
                    fail(new Error(`run failed: exit ${code}: ${line}`));
                } else ok(json);
            } catch {
                fail(new Error(`run printed no JSON (exit ${code}): ${line}`));
            }
        });
    });
}

/** Starts the GUI normally, lets it settle and sums the working set of its process tree. */
async function measureRss(ctx) {
    const child = spawn(exe, [], { env: childEnv(ctx), stdio: "ignore" });
    try {
        await sleep(4000);
        return workingSetMb(ctx.gui, [child.pid]);
    } finally {
        killTree(child.pid);
        await sleep(500);
    }
}

async function measureMode(mode) {
    const ctx = await createRoot({ fakeMailpit: false, tag: `perf-${mode}` });
    let daemon = null;
    try {
        if (mode === "running") daemon = await startDaemon(ctx);
        if (mode === "stopped") await ensureDaemonStopped(ctx);
        await oneRun(ctx); // warm-up, discarded: creates the WebView2 profile
        const samples = [];
        let session = "unknown";
        for (let i = 0; i < runs; i++) {
            if (mode === "stopped") await ensureDaemonStopped(ctx);
            const line = await oneRun(ctx);
            samples.push(line.first_frame_ms);
            session = line.session ?? session;
            await sleep(300);
        }
        const rss_mb = await measureRss(ctx);
        return {
            median: median(samples),
            p90: percentile(samples, 0.9),
            p95: percentile(samples, 0.95),
            runs: samples,
            session,
            rss_mb,
        };
    } finally {
        stopDaemon(ctx, daemon);
        await sleep(300);
        rmSync(ctx.base, { recursive: true, force: true });
    }
}

const result = {};
for (const mode of modes) result[`daemon_${mode}`] = await measureMode(mode);
const worst = Object.values(result).reduce((a, b) => (b.median > a.median ? b : a));
const passed = Object.values(result).every((m) => m.median < budget);
console.log(
    JSON.stringify({
        median: worst.median,
        p90: worst.p90,
        p95: worst.p95,
        runs: worst.runs,
        session: worst.session,
        webview2_version: webview2Version(),
        rss_mb: worst.rss_mb,
        budget_ms: budget,
        passed,
        modes: result,
    }),
);
process.exit(passed ? 0 : 1);
