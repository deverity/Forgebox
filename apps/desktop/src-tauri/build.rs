fn main() {
    // tauri-build embeds icons/icon.ico as the Windows icon resource (id 32512) but does not
    // watch it: without this line a `cargo build` after `pnpm icons` keeps the old icon
    // (docs/design/brand/README.md). tests/exe_icon.rs checks the embedded images.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    // Without the `custom-protocol` feature (plain `cargo build/clippy/test --workspace`) Tauri
    // runs in dev mode: `devUrl` is used and `frontendDist` is NOT embedded, so neither Node nor
    // `apps/desktop/dist` is needed. `pnpm tauri build` enables the feature and embeds `dist`.
    tauri_build::build();
}
