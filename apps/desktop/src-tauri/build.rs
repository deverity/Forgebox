fn main() {
    // Without the `custom-protocol` feature (plain `cargo build/clippy/test --workspace`) Tauri
    // runs in dev mode: `devUrl` is used and `frontendDist` is NOT embedded, so neither Node nor
    // `apps/desktop/dist` is needed. `pnpm tauri build` enables the feature and embeds `dist`.
    tauri_build::build();
}
