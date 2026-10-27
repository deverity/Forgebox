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
//! `--crash-after-ms <ms>` (exit 3), `--spawn-child` (starts a stubborn copy of itself,
//! prints `child <pid>`), `--window` (hidden top-level window; exits 0 on WM_CLOSE).
//! One-shot modes for stop commands: `--touch <path>` (create the file, exit 0),
//! `--exit-code <n>` (exit at once with n).

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

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<HandlerRoutine>, add: i32) -> i32;
    fn GetModuleHandleW(name: *const u16) -> isize;
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
    spawn_child: bool,
    window: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut opts = Options {
        interval: 100,
        lines_per_tick: 1,
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
            "--spawn-child" => opts.spawn_child = true,
            "--window" => opts.window = true,
            "--touch" => {
                std::fs::write(value(), b"stop").unwrap();
                std::process::exit(0);
            }
            "--exit-code" => std::process::exit(value().parse().unwrap()),
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

    if opts.window {
        std::thread::spawn(move || ticks(&opts));
        run_window();
        println!("window closed");
        std::process::exit(0);
    }
    ticks(&opts);
}

fn ticks(opts: &Options) {
    let started = Instant::now();
    let mut n: u64 = 0;
    let mut next = Instant::now();
    loop {
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
                std::process::exit(3);
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
