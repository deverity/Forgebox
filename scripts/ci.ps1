# Runs locally the same sequence as the `rust` job in .github/workflows/ci.yml.
# Default: `cargo xtask ci` (fmt, clippy, test, deny, arch-guard, lint-registry) plus build and the
# generated-artifacts drift check, which the workflow also runs.
# -OfflineDeny: for machines where the RustSec advisory DB cannot be fetched (e.g. TLS
# interception); runs the same steps individually with `cargo deny --offline` (cached DB).
# -Desktop: the desktop steps only (apps/desktop, Node >= 24): install, generated-file drift
# (design tokens, brand icons), lint, typecheck, vitest, the Playwright fake-bridge suite (system
# Edge, PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1) and `tauri build --debug --no-bundle`. FORGEBOX_NODE_DIR, when set, is put
# first on the PATH of this process only; NODE_OPTIONS=--use-system-ca is set for this process.
# Mirrors the `desktop` job of the workflow; the develop-only desktop-release job is not run here.
param([switch]$OfflineDeny, [switch]$Desktop)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Step([string]$Label, [scriptblock]$Block) {
    Write-Host "==> $Label"
    & $Block
    if ($LASTEXITCODE -ne 0) { throw "step failed: $Label (exit $LASTEXITCODE)" }
}

if ($Desktop) {
    if ($env:FORGEBOX_NODE_DIR) { $env:Path = "$env:FORGEBOX_NODE_DIR;$env:Path" }
    $env:NODE_OPTIONS = '--use-system-ca'
    $env:PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = '1'
    Invoke-Step 'pnpm install' { pnpm -C apps/desktop install --frozen-lockfile }
    Invoke-Step 'tokens:check' { pnpm -C apps/desktop tokens:check }
    Invoke-Step 'icons:check' { pnpm -C apps/desktop icons:check }
    Invoke-Step 'lint' { pnpm -C apps/desktop lint }
    Invoke-Step 'typecheck' { pnpm -C apps/desktop typecheck }
    Invoke-Step 'test' { pnpm -C apps/desktop test }
    Invoke-Step 'playwright fake-bridge suite' { pnpm -C apps/desktop e2e }
    Invoke-Step 'tauri debug build' { pnpm -C apps/desktop tauri build --debug --no-bundle }
    Write-Host 'CI Desktop OK'
    exit 0
}

Invoke-Step 'cargo build --workspace' { cargo build --workspace }
if ($OfflineDeny) {
    Invoke-Step 'cargo fmt' { cargo fmt --all -- --check }
    Invoke-Step 'cargo clippy' { cargo clippy --workspace --all-targets -- -D warnings }
    Invoke-Step 'cargo test' { cargo test --workspace }
    Invoke-Step 'cargo deny --offline check' { cargo deny --offline check }
    Invoke-Step 'cargo xtask arch-guard' { cargo xtask arch-guard }
    Invoke-Step 'cargo xtask lint-registry' { cargo xtask lint-registry }
} else {
    Invoke-Step 'cargo xtask ci' { cargo xtask ci }
}
Invoke-Step 'cargo xtask schema' { cargo xtask schema }
Invoke-Step 'cargo xtask gen-ts' { cargo xtask gen-ts }
Invoke-Step 'generated artifacts are committed' { git diff --exit-code }
Write-Host 'CI OK'
