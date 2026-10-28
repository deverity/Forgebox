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
pnpm -C apps/desktop tauri icon src-tauri/app-icon.png # regenerate icons (delete icons/android and icons/ios)
```

The cold debug build of `src-tauri` takes about 3 minutes (2-4x more while other worktrees compile).
The binary lands in the workspace `target/`, not in `src-tauri/target`.

`src/ipc/generated.ts` and `src/ipc/methods.ts` are written by `cargo xtask gen-ts`; never edit
them (ESLint and Prettier skip them).

## Version pins

- `typescript` 6.0.x: the newest line supported by `typescript-eslint` (peer `<6.1.0`); TypeScript
  7.0 (native) is not used. Revisit when `typescript-eslint` supports 7.
- `eslint` 9.x: `eslint-plugin-jsx-a11y` supports up to 9. Revisit when it supports 10.
- `tauri` 2.12.x / `tauri-build` 2.7 (Rust) and `@tauri-apps/cli`/`@tauri-apps/api` 2.12.4. Never
  Tauri 3 alpha.

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
