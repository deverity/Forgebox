//! Fake service for the forge-supervisor tests (Windows only, no dependencies).
//!
//! Prints `ready`, then `tick <n>` lines on stdout every `--interval-ms` (and `err <n>` on
//! stderr with `--stderr`). Console control behaviour:
//! * default: Windows default handling (CTRL_BREAK ends the process);
//! * `--stubborn`: ignores CTRL_C / CTRL_BREAK / CTRL_CLOSE (handler returns TRUE);
//! * `--polite`: on CTRL_BREAK (or CTRL_C) prints `ctrl-break` and exits 0.
//!
//! Other switches: `--lines-per-tick <k>`, `--print-env <NAME>` (repeatable),
//! `--print-cwd`, `--stop-file <path>` (exit 0 once the file exists),
//! `--crash-after-ms <ms>` (exit 3, or `--crash-code <n>`), `--spawn-child` (starts a
//! stubborn copy of itself, prints `child <pid>`), `--window` (hidden top-level window;
//! exits 0 on WM_CLOSE).
//! Health and metrics fixtures: `--listen <port>` (accepts TCP connections on
//! 127.0.0.1:<port>, prints `listening <port>`), `--listen-delay-ms <ms>` (bind only
//! after that), `--unlisten-after-ms <ms>` (close the listener again, prints
//! `unlistened`), `--burn <pct>` (one thread using <pct> % of one CPU, metered by its own
//! CPU time), `--alloc <MiB>` (allocates and keeps touching that much memory, prints
//! `allocated <bytes>`).
//! One-shot modes for stop and check commands: `--touch <path>` (create the file, exit
//! 0), `--exit-code <n>` (exit at once with n; `0x…` accepted). Switches are handled in
//! order, so these combine with `--say <text>` / `--say-err <text>` (print a line on
//! stdout / stderr) and `--fail-if-exists <path>` (print `config broken: <path>` on stderr
//! and exit 1 when the file exists), e.g. a configuration check:
//! `--fail-if-exists flag --say "syntax ok" --exit-code 0`.

use std::ffi::c_void;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static POLITE_STOP: AtomicBool = AtomicBool::new(false);

const CTRL_C_EVENT: u32 = 0;
const CTRL_BREAK_EVENT: u32 = 1;
const WM_CLOSE: u32 = 0x0010;
const WM_DESTROY: u32 = 0x0002;
const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;

type HandlerRoutine = unsafe extern "system" fn(u32) -> i32;
type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: Option<WndProc>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: isize,
    icon: isize,
    cursor: isize,
    background: isize,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
struct Msg {
    hwnd: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
    private: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FileTime {
    low: u32,
    high: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
    fn GetModuleHandleW(name: *const u16) -> isize;
    fn GetCurrentThread() -> isize;
    fn GetThreadTimes(
        thread: isize,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    #[allow(clippy::too_many_arguments)]
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        title: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: isize,
        menu: isize,
        instance: isize,
        param: *mut c_void,
    ) -> isize;
    fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: isize, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn DestroyWindow(hwnd: isize) -> i32;
    fn PostQuitMessage(code: i32);
}

unsafe extern "system" fn ignore_everything(_ctrl: u32) -> i32 {
    1
}

unsafe extern "system" fn polite(ctrl: u32) -> i32 {
    if ctrl == CTRL_BREAK_EVENT || ctrl == CTRL_C_EVENT {
        POLITE_STOP.store(true, Ordering::SeqCst);
        return 1;
    }
    0
}

unsafe extern "system" fn window_proc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match msg {
        WM_CLOSE => {
            println!("wm_close");
            // SAFETY: our own window.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // SAFETY: plain call on the window's thread.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: default processing of a message of our window.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Default)]
struct Options {
    stubborn: bool,
    polite: bool,
    interval: u64,
    lines_per_tick: u64,
    stderr: bool,
    print_env: Vec<String>,
    print_cwd: bool,
    stop_file: Option<PathBuf>,
    crash_after: Option<u64>,
    crash_code: i32,
    spawn_child: bool,
    window: bool,
    listen: Option<u16>,
    listen_delay: u64,
    unlisten_after: Option<u64>,
    burn: Option<u32>,
    alloc_mib: Option<usize>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut opts = Options {
        interval: 100,
        lines_per_tick: 1,
        crash_code: 3,
        ..Options::default()
    };
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("missing value");
        match arg.as_str() {
            "--stubborn" => opts.stubborn = true,
            "--polite" => opts.polite = true,
            "--interval-ms" => opts.interval = value().parse().unwrap(),
            "--lines-per-tick" => opts.lines_per_tick = value().parse().unwrap(),
            "--stderr" => opts.stderr = true,
            "--print-env" => opts.print_env.push(value()),
            "--print-cwd" => opts.print_cwd = true,
            "--stop-file" => opts.stop_file = Some(value().into()),
            "--crash-after-ms" => opts.crash_after = Some(value().parse().unwrap()),
            "--crash-code" => opts.crash_code = parse_code(&value()),
            "--listen" => opts.listen = Some(value().parse().unwrap()),
            "--listen-delay-ms" => opts.listen_delay = value().parse().unwrap(),
            "--unlisten-after-ms" => opts.unlisten_after = Some(value().parse().unwrap()),
            "--burn" => opts.burn = Some(value().trim_end_matches('%').parse().unwrap()),
            "--alloc" => {
                opts.alloc_mib = Some(value().trim_end_matches("MB").parse().unwrap());
            }
            "--spawn-child" => opts.spawn_child = true,
            "--window" => opts.window = true,
            "--touch" => {
                std::fs::write(value(), b"stop").unwrap();
                std::process::exit(0);
            }
            "--exit-code" => std::process::exit(parse_code(&value())),
            "--say" => println!("{}", value()),
            "--say-err" => eprintln!("{}", value()),
            "--fail-if-exists" => {
                let path = value();
                if std::path::Path::new(&path).exists() {
                    eprintln!("config broken: {path}");
                    std::process::exit(1);
                }
            }
            other => panic!("unknown argument {other}"),
        }
    }

    // SAFETY: handlers are plain functions valid for the life of the process.
    unsafe {
        if opts.stubborn {
            SetConsoleCtrlHandler(Some(ignore_everything), 1);
        } else if opts.polite {
            SetConsoleCtrlHandler(Some(polite), 1);
        }
    }

    println!("ready");
    for name in &opts.print_env {
        match std::env::var_os(name) {
            Some(value) => println!("env {name}={}", value.to_string_lossy()),
            None => println!("env {name} unset"),
        }
    }
    if opts.print_cwd {
        println!("cwd {}", std::env::current_dir().unwrap().display());
    }
    if opts.spawn_child {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--stubborn", "--interval-ms", "1000"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        println!("child {}", child.id());
    }

    if let Some(port) = opts.listen {
        let delay = opts.listen_delay;
        let unlisten = opts.unlisten_after;
        std::thread::spawn(move || listen(port, delay, unlisten));
    }
    if let Some(pct) = opts.burn {
        std::thread::spawn(move || burn(pct));
    }

    if opts.window {
        std::thread::spawn(move || ticks(&opts));
        run_window();
        println!("window closed");
        std::process::exit(0);
    }
    ticks(&opts);
}

/// `n`, `-n` or `0x…` (Windows status codes such as `0xC0000135`).
fn parse_code(text: &str) -> i32 {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).unwrap() as i32,
        None => text.parse().unwrap(),
    }
}

/// Accepts (and drops) connections on 127.0.0.1:`port` after `delay_ms`, until
/// `unlisten_after_ms` (measured from the start) if given.
fn listen(port: u16, delay_ms: u64, unlisten_after_ms: Option<u64>) {
    let started = Instant::now();
    std::thread::sleep(Duration::from_millis(delay_ms));
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    println!("listening {port}");
    loop {
        if let Some(ms) = unlisten_after_ms {
            if started.elapsed() >= Duration::from_millis(ms) {
                drop(listener);
                println!("unlistened");
                return;
            }
        }
        match listener.accept() {
            Ok((stream, _)) => drop(stream),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(err) => panic!("accept: {err}"),
        }
    }
}

/// CPU time of the calling thread.
fn thread_cpu() -> Duration {
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: pseudo handle of the current thread; four writable FILETIMEs.
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    assert_ne!(ok, 0, "GetThreadTimes failed");
    let ticks = |t: FileTime| (u64::from(t.high) << 32) | u64::from(t.low);
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

/// Uses `pct` % of one CPU: in every 1 s period, spins until this thread got `pct` % of
/// it as CPU time (or the period is over), then sleeps. Metering by CPU time keeps the
/// load right even when the machine is busy; the long period keeps the 15.6 ms
/// granularity of `GetThreadTimes` below 2 % of the budget.
fn burn(pct: u32) {
    let period = Duration::from_secs(1);
    let budget = period * pct.min(100) / 100;
    loop {
        let start = Instant::now();
        let cpu_start = thread_cpu();
        let mut x = 0_u64;
        while thread_cpu() - cpu_start < budget && start.elapsed() < period {
            for _ in 0..10_000 {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        }
        if let Some(rest) = period.checked_sub(start.elapsed()) {
            std::thread::sleep(rest);
        }
    }
}

/// Writes one byte per 4 KiB page so every page is in the working set.
fn touch(memory: &mut [u8], round: u8) {
    for page in memory.chunks_mut(4096) {
        page[0] = round;
    }
}

fn ticks(opts: &Options) {
    let started = Instant::now();
    let mut memory = opts.alloc_mib.map(|mib| {
        let mut memory = vec![0_u8; mib * 1024 * 1024];
        touch(&mut memory, 1);
        println!("allocated {}", memory.len());
        memory
    });
    let mut last_touch = Instant::now();
    let mut n: u64 = 0;
    let mut next = Instant::now();
    loop {
        if let Some(memory) = memory.as_mut() {
            if last_touch.elapsed() >= Duration::from_millis(500) {
                touch(memory, (n % 251) as u8 + 1);
                last_touch = Instant::now();
            }
        }
        if POLITE_STOP.load(Ordering::SeqCst) {
            println!("ctrl-break");
            std::process::exit(0);
        }
        if let Some(file) = &opts.stop_file {
            if file.exists() {
                println!("stop-file");
                std::process::exit(0);
            }
        }
        if let Some(ms) = opts.crash_after {
            if started.elapsed() >= Duration::from_millis(ms) {
                eprintln!("crashing");
                std::process::exit(opts.crash_code);
            }
        }
        if Instant::now() >= next {
            let mut out = std::io::stdout().lock();
            for _ in 0..opts.lines_per_tick {
                n += 1;
                writeln!(out, "tick {n}").unwrap();
            }
            drop(out);
            if opts.stderr {
                eprintln!("err {n}");
            }
            next += Duration::from_millis(opts.interval);
        }
        std::thread::sleep(Duration::from_millis(opts.interval.clamp(1, 20)));
    }
}

fn run_window() {
    let class_name = wide("ForgeboxFakeService");
    let title = wide("fake service");
    // SAFETY: Win32 window creation with valid, NUL-terminated strings that outlive the
    // calls; the message loop runs on this (creating) thread.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let class = WndClassW {
            style: 0,
            wnd_proc: Some(window_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
        };
        assert_ne!(RegisterClassW(&class), 0, "RegisterClassW failed");
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            200,
            100,
            0,
            0,
            instance,
            std::ptr::null_mut(),
        );
        assert_ne!(hwnd, 0, "CreateWindowExW failed");
        println!("window");
        let mut msg = std::mem::zeroed::<Msg>();
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
