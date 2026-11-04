// `pnpm qa:perf` (M2-E4-T4): runs the perf specs RUNS times (default 3, env FORGE_PERF_RUNS),
// each run writing its JSON to qa/out/perf/run-<n>/, then writes qa/out/perf/summary.json with the
// mean and coefficient of variation (CV = stddev / mean) of every metric. A metric whose CV is
// above 15 % that passes in at least one run is flagged (over the threshold in every run it is a budget miss, never informative): list it in qa/perf/informative.json (as "<spec>.<metric>") and document
// it in the PR; it is then recorded but no longer fails. The exit code is the worst spec run.
import { spawnSync } from "node:child_process";
import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const desktop = resolve(here, "..");
const out = join(desktop, "qa", "out", "perf");
const CV_LIMIT = 0.15;

/** Mean and CV of `values` (population standard deviation). */
export function summarize(values) {
    const mean = values.reduce((a, b) => a + b, 0) / values.length;
    const variance = values.reduce((a, b) => a + (b - mean) ** 2, 0) / values.length;
    const cv = mean === 0 ? 0 : Math.sqrt(variance) / Math.abs(mean);
    return { mean, cv };
}

function main() {
    const runs = Number(process.env.FORGE_PERF_RUNS ?? 3);
    rmSync(out, { recursive: true, force: true });
    let status = 0;
    for (let run = 1; run <= runs; run += 1) {
        const dir = join(out, `run-${String(run)}`);
        mkdirSync(dir, { recursive: true });
        console.log(`qa:perf run ${String(run)}/${String(runs)}`);
        const result = spawnSync("pnpm", ["qa:perf:once"], {
            cwd: desktop,
            stdio: "inherit",
            shell: true,
            env: { ...process.env, FORGE_PERF_OUT: dir, PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD: "1" },
        });
        status = Math.max(status, result.status ?? 1);
    }

    const series = new Map();
    for (let run = 1; run <= runs; run += 1) {
        const dir = join(out, `run-${String(run)}`);
        for (const file of readdirSync(dir).filter((f) => f.endsWith(".json"))) {
            const report = JSON.parse(readFileSync(join(dir, file), "utf8"));
            for (const m of report.metrics) {
                const key = `${report.spec}.${m.name}`;
                const entry = series.get(key) ?? {
                    unit: m.unit,
                    bound: m.bound,
                    threshold: m.threshold,
                    values: [],
                };
                entry.values.push(m.value);
                series.set(key, entry);
            }
        }
    }
    const summary = {};
    for (const [key, entry] of series) {
        const { mean, cv } = summarize(entry.values);
        // Variance may only relax a metric that passes in some runs; a metric over its threshold
        // in every run is a budget miss, never "noise".
        const passes = entry.values.map((v) => (entry.bound === "max" ? v <= entry.threshold : v >= entry.threshold));
        const failsAlways = passes.every((p) => !p);
        summary[key] = { ...entry, mean, cv, failsAlways, noisy: cv > CV_LIMIT && !failsAlways };
    }
    writeFileSync(join(out, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
    for (const [key, s] of Object.entries(summary)) {
        const flag = s.failsAlways
            ? "  <- over the threshold in every run: budget miss, not informative"
            : s.noisy
              ? "  <- CV > 15 %: make informative"
              : "";
        console.log(
            `${key}: mean ${s.mean.toFixed(2)} ${s.unit}, CV ${(s.cv * 100).toFixed(1)} %${flag}`,
        );
    }
    process.exitCode = status;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
