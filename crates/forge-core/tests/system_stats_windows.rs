//! Real Windows values of `platform::SystemStats` and of the process image names
//! (M2-E5-T10), checked against sources that share no code with the implementation:
//!
//! * `mem_total` against `Win32_ComputerSystem.TotalPhysicalMemory` (exact) and
//!   `mem_avail` against `Win32_OperatingSystem.FreePhysicalMemory` (±10 %);
//! * `volume(path)` against `Get-PSDrive` (`Used + Free` and `Free`, ±1 %);
//! * the machine CPU % against the CPU this test burns itself (the machine is at least as
//!   busy as one of its processes, on the same 0–100 scale);
//! * the image name of this test process against `std::env::current_exe`.
//!
//! Memory and free space move while other builds run, so our values are read right
//! before and right after the PowerShell reading and the reference must fall inside that
//! bracket widened by the tolerance. The burning-service variant of the CPU check (the
//! acceptance with the fake service at 50 %) lives with the fake service, in
//! `forge-supervisor/tests/lifecycle_windows.rs`.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use forge_core::platform::{self, ImageNameCache, MachineSample, VolumeSpace};

fn powershell(script: &str) -> String {
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("running powershell.exe");
    assert!(
        output.status.success(),
        "powershell failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Whitespace-separated unsigned numbers of one PowerShell answer.
fn numbers(text: &str) -> Vec<u64> {
    text.split_whitespace()
        .map(|n| {
            n.parse()
                .unwrap_or_else(|_| panic!("not a number: {n:?} in {text:?}"))
        })
        .collect()
}

fn sample() -> MachineSample {
    platform::system_stats().sample().unwrap()
}

/// Whether `reference` lies within `[min(a, b), max(a, b)]` widened by `tolerance`.
fn bracketed(reference: u64, a: u64, b: u64, tolerance: f64) -> bool {
    let low = a.min(b) as f64 * (1.0 - tolerance);
    let high = a.max(b) as f64 * (1.0 + tolerance);
    (low..=high).contains(&(reference as f64))
}

#[test]
fn memory_matches_wmi() {
    let before = sample();
    let wmi = numbers(&powershell(
        "$cs = Get-CimInstance Win32_ComputerSystem; $os = Get-CimInstance Win32_OperatingSystem; \
         '{0} {1}' -f $cs.TotalPhysicalMemory, $os.FreePhysicalMemory",
    ));
    let after = sample();
    let (total, free_kib) = (wmi[0], wmi[1]);
    let avail = free_kib * 1024;
    println!(
        "mem_total {} (WMI {total}); mem_avail {} .. {} (WMI {avail}); used {:.1} %",
        before.mem_total,
        before.mem_avail,
        after.mem_avail,
        after.mem_used_pct().unwrap()
    );
    assert_eq!(before.mem_total, total, "TotalPhysicalMemory");
    assert_eq!(after.mem_total, total);
    assert!(
        bracketed(avail, before.mem_avail, after.mem_avail, 0.10),
        "FreePhysicalMemory {avail} outside {} .. {} ±10 %",
        before.mem_avail,
        after.mem_avail
    );
    assert!(before.mem_avail <= before.mem_total);
}

/// `Get-PSDrive` of the drive holding `path`: `(Used + Free, Free)`.
fn ps_drive(path: &Path) -> (u64, u64) {
    let letter = path
        .to_str()
        .and_then(|p| p.chars().next())
        .filter(char::is_ascii_alphabetic)
        .expect("a path on a drive letter");
    let answer = numbers(&powershell(&format!(
        "$d = Get-PSDrive -Name {letter} -PSProvider FileSystem; '{{0}} {{1}}' -f $d.Used, $d.Free"
    )));
    (answer[0] + answer[1], answer[1])
}

fn check_volume(path: &Path) -> VolumeSpace {
    let stats = platform::system_stats();
    let before = stats.volume(path).unwrap();
    let (ps_total, ps_free) = ps_drive(path);
    let after = stats.volume(path).unwrap();
    println!(
        "volume of {}: total {} (Get-PSDrive {ps_total}), free {} .. {} (Get-PSDrive {ps_free}), available {}",
        path.display(),
        before.total,
        before.free,
        after.free,
        after.available
    );
    assert!(
        bracketed(ps_total, before.total, after.total, 0.01),
        "size {ps_total} vs {}",
        before.total
    );
    assert!(
        bracketed(ps_free, before.free, after.free, 0.01),
        "free {ps_free} vs {} .. {}",
        before.free,
        after.free
    );
    assert!(after.available <= after.free);
    assert!(after.used() > 0);
    after
}

#[test]
fn volume_of_the_root_matches_get_psdrive() {
    // The worktree stands in for a Forgebox root; the temp folder is where tests put
    // portable roots.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let space = check_volume(&root);
    check_volume(&std::env::temp_dir());

    let stats = platform::system_stats();
    // A file answers for the volume holding it; so do `/` separators and the bare drive.
    let file = root.join("Cargo.toml");
    let of_file = stats.volume(&file).unwrap();
    assert_eq!(of_file.total, space.total);
    let drive = PathBuf::from(&root.to_str().unwrap()[..2]);
    assert_eq!(
        stats.volume(&drive).unwrap().total,
        space.total,
        "{drive:?}"
    );
    let slashed = PathBuf::from(root.to_str().unwrap().replace('\\', "/"));
    assert_eq!(stats.volume(&slashed).unwrap().total, space.total);
    let verbatim = PathBuf::from(format!(r"\\?\{}", root.display()));
    assert_eq!(stats.volume(&verbatim).unwrap().total, space.total);

    let missing = root.join("no-such-folder-forgebox").join("deeper");
    assert_eq!(stats.volume(&missing).unwrap_err().code(), "not_found");
}

/// CPU time of this process (all threads), from the platform's own process counters.
fn own_cpu() -> Duration {
    platform::process_stats()
        .sample(std::process::id())
        .unwrap()
        .cpu_total
}

#[test]
fn machine_cpu_covers_what_this_process_burns() {
    let stats = platform::system_stats();
    let first = stats.sample().unwrap();
    assert!(first.logical_cpus >= 1);
    assert_eq!(
        first.logical_cpus,
        platform::process_stats().logical_cpus(),
        "same processor count as the process counters"
    );
    assert!(first.cpu_busy_total > Duration::ZERO);
    assert!(first.cpu_idle_total > Duration::ZERO);

    // Burn one full core for 2 s in a thread of this process.
    let cpu_before = own_cpu();
    let a = stats.sample().unwrap();
    let burner = std::thread::spawn(|| {
        let started = Instant::now();
        let mut x = 0_u64;
        while started.elapsed() < Duration::from_secs(2) {
            x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
        }
    });
    burner.join().unwrap();
    let b = stats.sample().unwrap();
    let cpu_after = own_cpu();

    let machine = b.cpu_pct_since(&a).expect("time elapsed");
    let ours = b
        .cpu_share_since(&a, cpu_after - cpu_before)
        .expect("time elapsed");
    let one_core = 100.0 / f64::from(b.logical_cpus);
    println!(
        "machine busy {machine:.2} % over {:?} of capacity; this process {ours:.2} % (one core = {one_core:.2} %)",
        (b.cpu_busy_total + b.cpu_idle_total) - (a.cpu_busy_total + a.cpu_idle_total)
    );
    assert!((0.0..=100.0).contains(&machine), "{machine}");
    // The machine is at least as busy as this process, give or take one clock tick per
    // processor in each counter (15.6 ms over 2 s of each processor: < 1 %).
    assert!(machine + 1.0 >= ours, "machine {machine} < process {ours}");
    // The burning thread got most of a core unless the machine was saturated.
    assert!(
        machine >= one_core * 0.9 || machine > 95.0,
        "machine {machine} below one core {one_core}"
    );
}

#[test]
fn the_image_name_of_this_process_is_its_executable() {
    let pid = std::process::id();
    let exe = std::env::current_exe().unwrap();
    let exe_name = exe.file_name().unwrap().to_str().unwrap();
    assert!(exe_name.ends_with(".exe"), "{exe_name}");

    let processes = platform::process_stats();
    let info = processes.info(pid).unwrap();
    assert_eq!(info.image_path.as_deref(), Some(exe.as_path()));
    assert_eq!(info.image_name.as_deref(), Some(exe_name));

    let cache = ImageNameCache::new(processes.clone());
    let me = processes.sample(pid).unwrap();
    let started = Instant::now();
    assert_eq!(cache.name_of(&me).as_deref(), Some(exe_name));
    let miss = started.elapsed();
    let started = Instant::now();
    assert_eq!(cache.name_of(&me).as_deref(), Some(exe_name));
    let hit = started.elapsed();
    println!("image name {exe_name}: miss {miss:?}, hit {hit:?}");
    assert_eq!(cache.len(), 1);
    // Another identity for the same PID (a process that no longer exists) gets nothing.
    let older = me.started_at - Duration::from_secs(1);
    assert_eq!(cache.name(pid, older), None);
}

#[test]
fn sampling_is_cheap() {
    let stats = platform::system_stats();
    stats.sample().unwrap();
    let mut times: Vec<Duration> = (0..21)
        .map(|_| {
            let started = Instant::now();
            stats.sample().unwrap();
            started.elapsed()
        })
        .collect();
    times.sort();
    let median = times[times.len() / 2];
    println!(
        "sample(): median {median:?}, max {:?}",
        times.last().unwrap()
    );
    // A hang detector, not a benchmark: other builds share the machine.
    assert!(median < Duration::from_millis(50), "{median:?}");
}

#[tokio::test]
async fn async_helpers_reach_the_os() {
    let stats = platform::system_stats();
    let machine = platform::sample_machine(stats.clone()).await.unwrap();
    assert!(machine.mem_total > 0);
    let space = platform::volume_space(stats, std::env::temp_dir())
        .await
        .unwrap();
    assert!(space.total > space.free);
}
