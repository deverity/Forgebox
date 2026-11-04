//! Seeds the store of a temporary root for the E2 live suite (`scripts/gui-e2e.ps1`,
//! M2-E2-T20): the `official` registry row points at a fixture registry folder, trusted
//! unsigned (the fixture is regenerated every run, so it cannot carry a minisign signature).
//! Release URLs must be https and the client only trusts real roots, so the fixture's
//! "slow" tool points at a local socket that never answers TLS; the catalog install of the
//! live suite hangs there until the test cancels it.
//!
//!   cargo run -p forge-daemon --example live_seed -- <root> <registry-dir>
//!
//! Development only: nothing here ships.

use std::path::PathBuf;

use forge_core::paths::{ForgeRoot, posix_string};
use forge_core::store::Store;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(root), Some(registry)) = (args.next(), args.next()) else {
        eprintln!("usage: live_seed <root> <registry-dir>");
        std::process::exit(2);
    };
    let root = ForgeRoot::new(PathBuf::from(root)).expect("root");
    std::fs::create_dir_all(root.state()).expect("state dir");
    let store = Store::open(&root.db()).expect("store");
    store
        .conn()
        .execute(
            "INSERT INTO registries (id, name, url, kind, official, enabled, priority, \
             trusted_unsigned, public_key, ttl_secs, created_at, updated_at) \
             VALUES ('reg-live', 'official', ?1, 'dir', 1, 1, 10, 1, NULL, 3600, 'now', 'now')",
            [posix_string(&PathBuf::from(registry))],
        )
        .expect("insert registry row");
    store.close().expect("close store");
}
