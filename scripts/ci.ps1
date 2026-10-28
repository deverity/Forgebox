# Runs locally the same sequence as the `rust` job in .github/workflows/ci.yml.
# Default: `cargo xtask ci` (fmt, clippy, test, deny, arch-guard, lint-registry) plus build and the
# generated-artifacts drift check, which the workflow also runs.
# -OfflineDeny: for machines where the RustSec advisory DB cannot be fetched (e.g. TLS
# interception); runs the same steps individually with `cargo deny --offline` (cached DB).
param([switch]$OfflineDeny)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Step([string]$Label, [scriptblock]$Block) {
    Write-Host "==> $Label"
    & $Block
    if ($LASTEXITCODE -ne 0) { throw "step failed: $Label (exit $LASTEXITCODE)" }
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
