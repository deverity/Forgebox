# Forgebox Desktop (`Forgebox.exe`)

Tauri 2 + WebView2 shell, React 19 + TypeScript + Tailwind 4 frontend (ADR-0002). The Rust side
(`src-tauri`, crate `forgebox-desktop`) is a thin host over the `forgeboxd` API: it may depend on
`forge-ipc` only, and not directly on `windows`/`windows-sys` (`cargo xtask arch-guard`).

## Two independent worlds

| What                                                                  | Needs Node? | Needs `dist/`?       |
| --------------------------------------------------------------------- | ----------- | -------------------- |
| `cargo build\|clippy\|test --workspace` (includes `forgebox-desktop`) | no          | no                   |
| `pnpm -C apps/desktop build\|lint\|typecheck\|test`                   | yes         | no                   |
| `pnpm -C apps/desktop tauri dev\|build`                               | yes         | built by the command |

Plain cargo commands compile `src-tauri` without the `custom-protocol` feature: Tauri runs in dev
mode, uses `devUrl` and embeds no frontend assets. `pnpm tauri build` adds the feature and embeds
`dist/` into `Forgebox.exe`.

## Node 24 (per shell, never global)

The frontend needs Node >= 24 and pnpm 10.7.1 (`packageManager`). Put Node on the PATH of **your
own shell only**; do not touch the global PATH or `~/.npmrc`, and do not add a project `.npmrc`.

```powershell
# PowerShell (Node 24 from a local folder; adjust the path)
$env:Path = 'C:\laragon\bin\nodejs\node-v24;' + $env:Path
node -v    # v24.x
pnpm -v    # 10.7.1
```

```bash
# Git Bash
export PATH="/c/laragon/bin/nodejs/node-v24:$PATH"
```

TLS interception (corporate proxy) makes `pnpm install` fail with certificate errors. Use the
system trust store for that process only:

```powershell
$env:NODE_OPTIONS = '--use-system-ca'
```

Never set `strict-ssl=false`.

pnpm 10 blocks dependency lifecycle scripts. `pnpm.onlyBuiltDependencies` in `package.json` is the
allowlist and is empty today (no dependency needs an install script). Add a package there only
with a reason.

## Commands

Always use the pinned CLI from `node_modules` (`pnpm tauri ...`), never a global `cargo-tauri`.

```powershell
pnpm -C apps/desktop install --frozen-lockfile
pnpm -C apps/desktop lint          # eslint 9 (flat) + typescript-eslint strict + react-hooks + jsx-a11y
pnpm -C apps/desktop typecheck     # tsc --noEmit (strict)
pnpm -C apps/desktop test          # vitest + Testing Library + jsdom
pnpm -C apps/desktop format:check  # prettier
pnpm -C apps/desktop build         # tsc + vite build -> dist/

pnpm -C apps/desktop tauri dev                      # Vite on :1420 + window "Forgebox"
pnpm -C apps/desktop tauri build --debug --no-bundle  # target/debug/Forgebox.exe (dist embedded)
pnpm -C apps/desktop icons          # regenerate src-tauri/icons (app + tray) from docs/design/brand
pnpm -C apps/desktop icons:check    # fail if a committed icon differs from the brand sources
```

Icons: never edit `src-tauri/icons/**` or run `tauri icon` by hand. `scripts/gen-icons.mjs` renders
the master SVG and the hand-pixeled grids of `docs/design/brand` (see its README), runs the pinned
`tauri icon` for the platform set, composes `icon.ico` and the tray icons and writes the contact
sheet. The Rust EXEs pick the new `icon.ico` up on the next `cargo build`.

The cold debug build of `src-tauri` takes about 3 minutes (2-4x more while other worktrees compile).
The binary lands in the workspace `target/`, not in `src-tauri/target`.

`src/ipc/generated.ts` and `src/ipc/methods.ts` are written by `cargo xtask gen-ts`; never edit
them (ESLint and Prettier skip them).

## Test harness (ADR-0036)

```powershell
pnpm -C apps/desktop test            # vitest (unit; includes the production-bundle sentinel scan)
pnpm -C apps/desktop e2e             # Playwright on system Edge against `vite build --mode qa` (headless)
pnpm -C apps/desktop dev:qa          # qa-mode dev server on :1421
powershell -NoProfile -File scripts/e2e-desktop.ps1     # real Forgebox.exe + forgeboxd over CDP (kill: starting on auto-restart, reconnecting on a 2nd kill within 60 s)
powershell -NoProfile -File scripts/qa/capture-window.ps1 -ProcessId <pid> -Png out.png
pnpm -C apps/desktop perf:startup    # release cold start: 1 discarded + 10 runs, exit 1 if median >= 800 ms
```

- `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1`: the tests use the installed Edge (`channel: "msedge"`);
  nothing is downloaded. Fallback: `chromium-headless-shell` with `PLAYWRIGHT_BROWSERS_PATH` set to
  a project-local folder.
- qa mode (`?scenario=empty|populated|heavy|errors|not-implemented|daemon-down|slow|remote`,
  `&theme=light|dark&vibrancy=on|off&motion=reduced&locale=pt-BR|en-US|pseudo&latency=<ms>`) builds
  into `dist-qa/` and is removed from production builds; `src/qa/sentinel.test.ts` proves it.
  Scenarios live in `qa/fixtures/scenarios` (typed with `satisfies` against `generated.ts`); the bridge
  is on `window.__FORGEBOX_QA__`.
- Helpers (`e2e/fixtures.ts`): `qaUrl`, `emulate` (scale factor, color scheme, reduced motion and
  transparency via CDP), `axeViolations`, `tabOrder`, `trackCls`, and an automatic fixture that fails
  any test whose page logs a console error.
- The real-app run opens the WebView2 debugging port through
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` in the test's own environment only (never in code or release
  config), on a temp portable root with DNS off and a fake Mailpit. It leaves no process or file
  outside the temp folder (the script fails otherwise).
- `perf:startup` options: `--exe`, `--runs`, `--budget-ms`, `--budget-factor` (CI uses 1.5), `--daemon
both|running|stopped`. Run it only on a quiet machine; the session type (rdp/console) is part of
  the output. Screenshots and captures go to `qa/out/` (gitignored); there is no pixel-diff gate in M2.

- Wiring point of the fake bridge: in `qa` mode `main.tsx` passes `handle.bridge` (also
  `window.__FORGEBOX_QA__.bridge`) to `<App bridge={...}/>`; the state layer (M2-E1-T10) must use that
  bridge instead of `createTauriBridge()` whenever the prop is set. Without it `?scenario=` changes nothing.

## CI (M2-E1-T20)

```powershell
# same steps as the `desktop` job of .github/workflows/ci.yml (install --frozen-lockfile, tokens:check,
# icons:check, lint, typecheck, vitest, Playwright fake-bridge suite, tauri debug build)
$env:FORGEBOX_NODE_DIR = '<folder with node 24>'   # optional, prepended to this process PATH only
powershell -NoProfile -File scripts/ci.ps1 -Desktop
actionlint .github/workflows/ci.yml                 # workflow lint (local binary, not system-wide)
```

- `ci.ps1 -Desktop` sets `NODE_OPTIONS=--use-system-ca` and `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` for its
  own process; no path is hardcoded (use `FORGEBOX_NODE_DIR`).
- The `rust` job still builds `src-tauri` without Node. The `desktop` job uploads the Playwright HTML
  report and failure screenshots/traces (`qa/out/playwright-report`, `qa/out/test-results`).
- `desktop-release` runs only on push to `develop`/`main`: release no-bundle build,
  `scripts/e2e-desktop.ps1 -SkipBuild`, and `perf:startup --budget-factor 1.5` (assert 1.5 x 800 ms)
  uploaded as the `perf-startup` artifact.

## Version pins

- `typescript` 6.0.x: the newest line supported by `typescript-eslint` (peer `<6.1.0`); TypeScript
  7.0 (native) is not used. Revisit when `typescript-eslint` supports 7.
- `eslint` 9.x: `eslint-plugin-jsx-a11y` supports up to 9. Revisit when it supports 10.
- `tauri` 2.12.x / `tauri-build` 2.7 (Rust) and `@tauri-apps/cli`/`@tauri-apps/api` 2.12.4. Never
  Tauri 3 alpha.
- `@resvg/resvg-js` 2.6.x (devDependency of `scripts/gen-icons.mjs`): prebuilt per-platform binary,
  no install script. Its PNG encoder makes the icon files byte-stable, so a bump means running
  `pnpm icons` and committing the result.

## Notes

- Tailwind: `src/index.css` imports `tailwindcss` with `source("./")`, so class scanning stays in
  `src/` and never walks `src-tauri/target`.
- CSP (`tauri.conf.json`): `default-src 'self'` with no remote origins; only `ipc:` /
  `http://ipc.localhost` for the Tauri IPC. `style-src` keeps `'unsafe-inline'` because Radix and
  Motion write inline `style` attributes. `tauri dev` serves the page from Vite, which the CSP
  does not constrain (HMR needs it).
- Capabilities: `capabilities/main.json` grants `core:default` to the `main` window only.
- Release profile: the root `Cargo.toml` has `lto = "thin"`, `codegen-units = 1`; `opt-level = "s"`
  is applied per package to `forgebox-desktop` and the Tauri/wry/tao crates (Cargo does not allow
  profiles or `lto` per package in a member manifest).

## Data layer (`src/state`)

Screens never call the bridge directly for live data: wrap the app in `ForgeStateProvider`
(`new StateLayer(bridge)`) and read through the hooks (`useInstance`, `useSites`, `useJob`,
`useMetricsPoll`, `useStatusTotals`, `useCapabilities`, `useForgeQuery`...). Events are reduced
once per animation frame; `lagged`/`reset`/stale reconnections refetch the snapshots once;
mutating user actions go through `runtime.queue` (held up to 30 s while reconnecting). Failures
are shown with `describeError` (`src/ipc/describeError.ts`): a localized cause plus actions, each
with its `forge` command from `renderCli` (`src/ipc/cli.ts`). Design and trade-offs: ADR-0034,
addendum M2-E1-T9.
