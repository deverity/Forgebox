//! Port allocator against the real OS TCP table and real sockets (Windows), and the
//! persistence of reservations across store reopenings.
#![cfg(windows)]

use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;

use forge_core::config::{PortSet, PortsConfig};
use forge_core::model::port::{OwnerKind, PortState, ReservationOwner};
use forge_core::platform::{PortOwner, PortTable, port_table};
use forge_core::store::Store;
use forge_net::{AllocatorOptions, PortAllocator};

fn owner(id: &str) -> ReservationOwner {
    ReservationOwner::new(OwnerKind::Instance, id, "main").unwrap()
}

/// `[ports]` with one range `test` of `len + 1` ports from `start` (shifted down when it
/// would pass 65535).
fn config_around(start: u16, len: u16) -> (PortsConfig, u16, u16) {
    config_starting_at(start.min(65535 - len), len)
}

/// `[ports]` with one range `test` = `first..=first + len` (capped at 65535).
fn config_starting_at(first: u16, len: u16) -> (PortsConfig, u16, u16) {
    let last = first.saturating_add(len);
    let mut config = PortsConfig {
        reserved: Vec::new(),
        ..PortsConfig::default()
    };
    let set: PortSet = format!("{first}-{last}").parse().unwrap();
    config.ranges.insert("test".to_owned(), set);
    (config, first, last)
}

fn exe_name() -> String {
    std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// A table that never reports anything: only the bind check can see busy ports.
#[derive(Debug)]
struct BlindTable;

impl PortTable for BlindTable {
    fn listeners(&self) -> forge_core::Result<Vec<PortOwner>> {
        Ok(Vec::new())
    }

    fn established_count(&self, _: u16) -> forge_core::Result<u32> {
        Ok(0)
    }
}

#[tokio::test]
async fn allocation_skips_a_port_with_a_real_listener() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let busy = listener.local_addr().unwrap().port();
    // The busy port is the first candidate of the range.
    let (config, first, last) = config_starting_at(busy, 40);
    let store = Store::open_in_memory().unwrap();
    let ports = PortAllocator::new(&store, port_table(), config);
    let got = ports.allocate("test", owner("a")).await.unwrap();
    assert_ne!(got.port, busy, "the listened port is skipped");
    assert!((first..=last).contains(&got.port), "{} in range", got.port);
    drop(listener);
}

#[tokio::test]
async fn the_bind_check_catches_ports_the_table_does_not_show() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let busy = listener.local_addr().unwrap().port();
    let (config, _, _) = config_starting_at(busy, 20);
    let store = Store::open_in_memory().unwrap();
    let ports = PortAllocator::new(&store, Arc::new(BlindTable), config);
    let got = ports.allocate("test", owner("a")).await.unwrap();
    assert_ne!(got.port, busy, "bind check refused the busy port");
    let all = ports.list().await.unwrap();
    assert_eq!(all.len(), 1, "the refused reservation was removed: {all:?}");

    let without_check = PortAllocator::new(&store, Arc::new(BlindTable), {
        let mut c = PortsConfig::default();
        c.ranges
            .insert("one".to_owned(), busy.to_string().parse().unwrap());
        c
    })
    .with_options(AllocatorOptions {
        bind_check: false,
        ..AllocatorOptions::default()
    });
    let blind = without_check.allocate("one", owner("b")).await.unwrap();
    assert_eq!(
        blind.port, busy,
        "without the check the busy port is handed out"
    );
    drop(listener);
}

#[tokio::test]
async fn conflicts_name_the_real_process_holding_a_reserved_port() {
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let start = probe.local_addr().unwrap().port();
    drop(probe);
    let (config, _, _) = config_around(start, 40);
    let store = Store::open_in_memory().unwrap();
    let ports = PortAllocator::new(&store, port_table(), config);
    let reservation = ports.allocate("test", owner("svc")).await.unwrap();
    assert!(ports.conflicts(|_, _| false).await.unwrap().is_empty());

    // Something outside the owner takes the port.
    let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, reservation.port)).unwrap();
    let found = ports.conflicts(|_, _| false).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let conflict = &found[0];
    assert_eq!(conflict.reservation.id, reservation.id);
    assert_eq!(conflict.holder.pid, std::process::id());
    let exe = conflict.holder.exe_display().unwrap();
    assert!(exe.contains(&exe_name()), "{exe}");
    assert!(conflict.newly_detected);
    let stored = ports.get(&owner("svc")).await.unwrap().unwrap();
    assert_eq!(stored.state, PortState::Conflict);
    assert_eq!(stored.conflict.as_ref().unwrap().pid, std::process::id());

    // The same process seen as the owner: bound.
    let me = std::process::id();
    assert!(
        ports
            .conflicts(|_, pid| pid == me)
            .await
            .unwrap()
            .is_empty()
    );
    let stored = ports.get(&owner("svc")).await.unwrap().unwrap();
    assert_eq!(stored.state, PortState::Bound);

    drop(squatter);
    assert!(ports.conflicts(|_, _| false).await.unwrap().is_empty());
    let stored = ports.get(&owner("svc")).await.unwrap().unwrap();
    assert_eq!((stored.state, stored.conflict), (PortState::Reserved, None));
}

#[tokio::test]
async fn reservations_survive_reopening_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("forgebox.db");
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let start = probe.local_addr().unwrap().port();
    drop(probe);
    let (config, _, _) = config_around(start, 40);

    let (a, b) = {
        let store = Store::open(&path).unwrap();
        let ports = PortAllocator::new(&store, port_table(), config.clone());
        let a = ports.allocate("test", owner("a")).await.unwrap();
        let b = ports.allocate("test", owner("b")).await.unwrap();
        assert_ne!(a.port, b.port);
        tokio::task::spawn_blocking(move || store.close())
            .await
            .unwrap()
            .unwrap();
        (a, b)
    };

    let store = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || Store::open(&path))
            .await
            .unwrap()
            .unwrap()
    };
    let ports = PortAllocator::new(&store, port_table(), config);
    let listed = ports.list().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&a) && listed.contains(&b), "{listed:?}");
    assert_eq!(ports.allocate("test", owner("a")).await.unwrap(), a);
    let c = ports.allocate("test", owner("c")).await.unwrap();
    assert!(c.port != a.port && c.port != b.port);
    assert_eq!(ports.release(&owner("b")).await.unwrap().unwrap().id, b.id);
    tokio::task::spawn_blocking(move || store.close())
        .await
        .unwrap()
        .unwrap();
}
