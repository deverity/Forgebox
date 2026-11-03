//! Unit tests of the terminal sessions with a fake pseudo console (the real ConPTY runs in
//! `tests/terminal.rs` against `forgeboxd`).

use std::io::Write;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use forge_core::ids::SubscriptionId;
use forge_core::platform::{ExitSource, GroupOptions, ProcessCommand, PtySession, SpawnedProcess};
use tokio_util::sync::CancellationToken;

use super::*;

// ------------------------------------------------------------------ pure pieces

#[test]
fn ring_keeps_the_newest_bytes_at_absolute_offsets() {
    let mut ring = Ring::new(8);
    ring.push(b"abcde");
    assert_eq!((ring.start, ring.end()), (0, 5));
    ring.push(b"fghij");
    assert_eq!((ring.start, ring.end()), (2, 10));
    assert_eq!(ring.copy(0, 100), (2, b"cdefghij".to_vec()));
    assert_eq!(ring.copy(4, 3), (4, b"efg".to_vec()));
    assert_eq!(ring.copy(10, 3), (10, Vec::new()));
    // A push larger than the ring keeps its tail.
    ring.push(b"0123456789AB");
    assert_eq!((ring.start, ring.end()), (14, 22));
    assert_eq!(ring.copy(0, 100).1, b"456789AB".to_vec());
}

#[test]
fn incomplete_utf8_tails_are_found() {
    let text = "ação ✓ 😀".as_bytes();
    assert_eq!(incomplete_utf8_tail(text), 0);
    assert_eq!(incomplete_utf8_tail(b"abc"), 0);
    assert_eq!(incomplete_utf8_tail(b""), 0);
    // 'ç' = C3 A7, '✓' = E2 9C 93, '😀' = F0 9F 98 80
    assert_eq!(incomplete_utf8_tail(b"a\xC3"), 1);
    assert_eq!(incomplete_utf8_tail(b"a\xE2\x9C"), 2);
    assert_eq!(incomplete_utf8_tail(b"a\xE2"), 1);
    assert_eq!(incomplete_utf8_tail(b"a\xF0\x9F\x98"), 3);
    assert_eq!(incomplete_utf8_tail(b"a\xF0\x9F\x98\x80"), 0);
    // Bytes that never complete are passed through (shown as U+FFFD).
    assert_eq!(incomplete_utf8_tail(b"a\xFF"), 0);
    assert_eq!(incomplete_utf8_tail(b"a\x80\x80\x80"), 0);
    assert_eq!(leading_continuations(b"\x9C\x93ok"), 2);
    assert_eq!(leading_continuations(b"ok"), 0);
}

#[test]
fn attach_plans() {
    // No cursor: the whole ring; the floor is the ring start.
    assert_eq!(
        plan_attach(None, 100, 500, false),
        Attach {
            last_seq: 100,
            cursor: Some(100)
        }
    );
    // A cursor inside (or before) the ring resumes from it.
    assert_eq!(plan_attach(Some(300), 100, 500, false).cursor, Some(300));
    assert_eq!(plan_attach(Some(10), 100, 500, false).cursor, Some(10));
    // `exited` was already received.
    assert_eq!(
        plan_attach(Some(501), 100, 500, true),
        Attach {
            last_seq: 501,
            cursor: None
        }
    );
    // Past the output (another daemon's session): restart from the end.
    assert_eq!(
        plan_attach(Some(9_000), 100, 500, false),
        Attach {
            last_seq: 500,
            cursor: Some(500)
        }
    );
}

#[test]
fn writers_drop_retries_and_forget_the_idlest() {
    let mut w = Writers::default();
    let a = TerminalWriterId::new();
    let b = TerminalWriterId::new();
    assert!(w.is_new(a, 1));
    w.accept(a, 1);
    assert!(!w.is_new(a, 1));
    assert!(w.is_new(a, 2));
    // Another writer has its own counter.
    assert!(w.is_new(b, 1));
    w.accept(b, 1);
    // Bounded: the writer idle the longest goes first.
    for _ in 0..TERMINAL_MAX_WRITERS {
        w.touch(a);
        w.accept(TerminalWriterId::new(), 1);
    }
    assert_eq!(w.last.len(), TERMINAL_MAX_WRITERS);
    assert!(!w.is_new(a, 1), "the active writer is kept");
    assert!(w.is_new(b, 1), "the idle writer was forgotten");
}

#[cfg(windows)]
fn lookup_with<'a>(path: &'a OsStr, is_file: &'a dyn Fn(&Path) -> bool) -> Lookup<'a> {
    Lookup {
        system32: Path::new(r"C:\Windows\System32"),
        path,
        pathext: None,
        program_files: Some(Path::new(r"C:\Program Files")),
        comspec: None,
        is_file,
    }
}

#[cfg(windows)]
#[test]
fn shells_are_found_in_order() {
    let path = OsString::from(r"C:\tools;C:\Windows\System32");
    let all = |p: &Path| {
        [
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\Windows\System32\cmd.exe",
        ]
        .iter()
        .any(|f| Path::new(f) == p)
    };
    let lookup = lookup_with(&path, &all);
    assert_eq!(find_shell(None, None, &lookup).unwrap().0, "pwsh");
    assert_eq!(find_shell(None, Some("cmd"), &lookup).unwrap().0, "cmd");
    let (name, exe) = find_shell(Some("powershell"), None, &lookup).unwrap();
    assert_eq!(name, "powershell");
    assert!(exe.ends_with("powershell.exe"));
    assert_eq!(
        find_shell(Some("bash"), None, &lookup).unwrap_err().code(),
        "invalid_input"
    );

    // Without PowerShell 7: the configured pwsh falls back; asking for it fails.
    let no_pwsh = |p: &Path| all(p) && !p.ends_with("pwsh.exe");
    let lookup = lookup_with(&path, &no_pwsh);
    assert_eq!(
        find_shell(None, Some("pwsh"), &lookup).unwrap().0,
        "powershell"
    );
    assert_eq!(
        find_shell(Some("pwsh"), None, &lookup).unwrap_err().code(),
        "not_found"
    );
    // A pwsh on the PATH wins over Program Files.
    let in_path = |p: &Path| p == Path::new(r"C:\tools\pwsh.exe") || all(p);
    let lookup = lookup_with(&path, &in_path);
    assert_eq!(
        find_shell(None, None, &lookup).unwrap().1,
        PathBuf::from(r"C:\tools\pwsh.exe")
    );
}

#[test]
fn powershell_gets_forge_completions() {
    assert!(shell_args("cmd", Some(Path::new(r"C:\fb\forge.exe"))).is_empty());
    assert_eq!(shell_args("pwsh", None), vec![OsString::from("-NoLogo")]);
    let args = shell_args("powershell", Some(Path::new(r"C:\it's\forge.exe")));
    assert_eq!(args[1], "-NoExit");
    assert_eq!(
        args[3],
        r"& 'C:\it''s\forge.exe' completions powershell | Out-String | Invoke-Expression"
    );
}

#[cfg(windows)]
#[test]
fn commands_resolve_like_a_shell() {
    let path = OsString::from(r"C:\fb\bin\shims;C:\fb\bin\mysql\current\bin");
    let files = |p: &Path| {
        [
            r"C:\fb\bin\shims\composer.cmd",
            r"C:\fb\bin\mysql\current\bin\mysql.exe",
            r"C:\proj\run.exe",
            r"C:\Windows\System32\cmd.exe",
        ]
        .iter()
        .any(|f| Path::new(f) == p)
    };
    let lookup = lookup_with(&path, &files);
    let cwd = Path::new(r"C:\proj");
    let argv = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let (exe, args) = resolve_command(&argv(&["mysql", "-u", "root"]), cwd, &lookup).unwrap();
    assert_eq!(exe, PathBuf::from(r"C:\fb\bin\mysql\current\bin\mysql.exe"));
    assert_eq!(args, vec![OsString::from("-u"), OsString::from("root")]);
    // Batch shims run through cmd.exe.
    let (exe, args) = resolve_command(&argv(&["composer", "install"]), cwd, &lookup).unwrap();
    assert_eq!(exe, PathBuf::from(r"C:\Windows\System32\cmd.exe"));
    assert_eq!(
        args,
        ["/d", "/c", r"C:\fb\bin\shims\composer.cmd", "install"]
            .map(OsString::from)
            .to_vec()
    );
    // Relative paths from the working folder; absolute as given.
    assert_eq!(
        resolve_command(&argv(&[r".\run.exe"]), cwd, &lookup)
            .unwrap()
            .0,
        PathBuf::from(r"C:\proj\.\run.exe")
    );
    assert_eq!(
        resolve_command(&argv(&["nope"]), cwd, &lookup)
            .unwrap_err()
            .code(),
        "not_found"
    );
    assert_eq!(
        resolve_command(&[], cwd, &lookup).unwrap_err().code(),
        "invalid_input"
    );
}

#[cfg(windows)]
#[test]
fn session_env_puts_forgebox_first_and_extras_last() {
    let base = vec![
        (
            OsString::from("Path"),
            OsString::from(r"C:\Windows\System32"),
        ),
        (OsString::from("USERNAME"), OsString::from("dev")),
    ];
    let vars = BTreeMap::from([("PHPRC".to_owned(), r"C:\fb\etc\php".to_owned())]);
    let extra = BTreeMap::from([("MYSQL_PWD".to_owned(), "s3cret".to_owned())]);
    let (env, path) = session_env(base, &[r"C:\fb\bin\php\8.3".to_owned()], &vars, &extra);
    assert_eq!(
        path,
        OsString::from(r"C:\fb\bin\php\8.3;C:\Windows\System32")
    );
    let names: Vec<_> = env
        .iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "USERNAME",
            "PATH",
            "PHPRC",
            "FORGEBOX_TERMINAL",
            "MYSQL_PWD"
        ]
    );
    // A PATH in the extras wins (and is the one programs are looked up in).
    let extra = BTreeMap::from([("Path".to_owned(), r"D:\only".to_owned())]);
    let (_, path) = session_env(Vec::new(), &[], &BTreeMap::new(), &extra);
    assert_eq!(path, OsString::from(r"D:\only"));
}

// ------------------------------------------------------------------ pump

#[derive(Debug, PartialEq, Eq)]
enum Got {
    Out(u64, String),
    Lagged(u64),
    Exited(u64, Option<i32>),
}

#[derive(Clone)]
struct FakeSink {
    id: SubscriptionId,
    tx: mpsc::UnboundedSender<Got>,
    closed: CancellationToken,
}

impl FakeSink {
    fn new() -> (Self, mpsc::UnboundedReceiver<Got>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                id: SubscriptionId::new(),
                tx,
                closed: CancellationToken::new(),
            },
            rx,
        )
    }
}

impl TerminalSink for FakeSink {
    fn subscription(&self) -> SubscriptionId {
        self.id
    }

    async fn send(&self, item: TerminalEnvelope) -> bool {
        if self.closed.is_cancelled() {
            return false;
        }
        let got = match item.item {
            TerminalItemDto::Output { data } => Got::Out(item.seq, data),
            TerminalItemDto::Exited { exit_code } => Got::Exited(item.seq, exit_code),
        };
        self.tx.send(got).is_ok()
    }

    fn lagged(&self, missed: u64) {
        let _ = self.tx.send(Got::Lagged(missed));
    }

    async fn closed(&self) {
        self.closed.cancelled().await;
    }

    fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }
}

/// Skips output until `exited`; its exit code.
async fn until_exited(rx: &mut mpsc::UnboundedReceiver<Got>) -> Option<i32> {
    loop {
        if let Got::Exited(_, code) = next(rx).await {
            return code;
        }
    }
}

/// A subscriber whose connection never takes anything (it stops reading).
struct Stuck(SubscriptionId);

impl TerminalSink for Stuck {
    fn subscription(&self) -> SubscriptionId {
        self.0
    }
    async fn send(&self, _: TerminalEnvelope) -> bool {
        std::future::pending().await
    }
    fn lagged(&self, _: u64) {}
    async fn closed(&self) {
        std::future::pending::<()>().await;
    }
    fn is_closed(&self) -> bool {
        false
    }
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Got>) -> Got {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("an item in time")
        .expect("the pump is alive")
}

/// Text received until `seq` reaches `upto`.
async fn text_until(rx: &mut mpsc::UnboundedReceiver<Got>, upto: u64) -> String {
    let mut text = String::new();
    loop {
        match next(rx).await {
            Got::Out(seq, data) => {
                text.push_str(&data);
                if seq >= upto {
                    return text;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn resume_gets_exactly_the_missing_bytes_and_old_cursors_lag() {
    let output = Arc::new(Output::new(16));
    output.append(b"0123456789");
    // From byte 4: exactly what follows.
    let (sink, mut rx) = FakeSink::new();
    tokio::spawn(pump(Arc::clone(&output), sink.clone(), 4));
    assert_eq!(text_until(&mut rx, 10).await, "456789");
    output.append(b"abc");
    assert_eq!(next(&mut rx).await, Got::Out(13, "abc".into()));
    sink.closed.cancel();

    // The ring keeps 16 bytes: after 30, a cursor at 2 lost 12 (14 is the ring start).
    output.append(b"ABCDEFGHIJKLMNOPQ");
    let (start, end) = output.bounds();
    assert_eq!((start, end), (14, 30));
    let (sink, mut rx) = FakeSink::new();
    tokio::spawn(pump(Arc::clone(&output), sink.clone(), 2));
    assert_eq!(next(&mut rx).await, Got::Lagged(12));
    assert_eq!(text_until(&mut rx, 30).await, "BCDEFGHIJKLMNOPQ");
    sink.closed.cancel();
}

#[tokio::test]
async fn a_ring_cut_inside_a_character_skips_its_rest() {
    let output = Arc::new(Output::new(4));
    // "ab✓z" = 61 62 E2 9C 93 7A: a ring of 4 keeps bytes 2..6 (E2 9C 93 7A).
    output.append("ab✓".as_bytes());
    output.append(b"z");
    let (start, end) = output.bounds();
    assert_eq!((start, end), (2, 6));
    let (sink, mut rx) = FakeSink::new();
    tokio::spawn(pump(Arc::clone(&output), sink.clone(), 0));
    // Bytes 2..6 are E2 9C 93 7A: starts on a character, lag = 2.
    assert_eq!(next(&mut rx).await, Got::Lagged(2));
    assert_eq!(next(&mut rx).await, Got::Out(6, "✓z".into()));
    sink.closed.cancel();

    output.append(b"yy");
    // Now 4..8 = 93 7A 79 79: the ring cut '✓'.
    let (sink, mut rx) = FakeSink::new();
    tokio::spawn(pump(Arc::clone(&output), sink.clone(), 0));
    // One continuation byte skipped: 5 bytes missed in all.
    assert_eq!(next(&mut rx).await, Got::Lagged(5));
    assert_eq!(next(&mut rx).await, Got::Out(8, "zyy".into()));
    sink.closed.cancel();
}

#[tokio::test]
async fn delivery_is_coalesced_and_ends_with_exited() {
    let output = Arc::new(Output::new(RING_BYTES));
    let (sink, mut rx) = FakeSink::new();
    tokio::spawn(pump(Arc::clone(&output), sink.clone(), 0));
    tokio::task::yield_now().await;
    // After a pause, the first bytes go out at once.
    output.append(b"a");
    assert_eq!(next(&mut rx).await, Got::Out(1, "a".into()));
    let sent_a = Instant::now();
    // Bytes arriving within 16 ms of the last item wait and go out together.
    output.append(b"b");
    tokio::task::yield_now().await;
    output.append(b"c");
    match next(&mut rx).await {
        Got::Out(3, data) => assert_eq!(data, "bc"),
        // Only when the machine stalled the test past the window.
        Got::Out(2, data) => {
            assert_eq!(data, "b");
            assert!(sent_a.elapsed() >= DELIVERY_INTERVAL);
            assert_eq!(next(&mut rx).await, Got::Out(3, "c".into()));
        }
        other => panic!("{other:?}"),
    }
    // A big burst goes out in 32 KiB items right away.
    output.append(&vec![b'x'; DELIVERY_BYTES * 2 + 10]);
    let mut sizes = Vec::new();
    while sizes.iter().sum::<usize>() < DELIVERY_BYTES * 2 + 10 {
        match next(&mut rx).await {
            Got::Out(_, data) => sizes.push(data.len()),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(sizes[..2], [DELIVERY_BYTES, DELIVERY_BYTES]);
    output.mark_eof();
    output.end(Some(0));
    let end = output.bounds().1;
    assert_eq!(next(&mut rx).await, Got::Exited(end + 1, Some(0)));
    // The pump is done: it no longer counts as a watcher.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(*output.watchers.borrow(), 0);
}

#[tokio::test]
async fn items_never_split_a_character() {
    let output = Arc::new(Output::new(RING_BYTES));
    // 'ç' (2 bytes) straddles the 32 KiB boundary.
    let mut data = vec![b'x'; DELIVERY_BYTES - 1];
    data.extend("ç".as_bytes());
    data.extend(b"end");
    output.append(&data);
    output.mark_eof();
    output.end(None);
    let (sink, mut rx) = FakeSink::new();
    tokio::spawn(pump(Arc::clone(&output), sink, 0));
    let Got::Out(seq, first) = next(&mut rx).await else {
        panic!()
    };
    assert_eq!(seq, (DELIVERY_BYTES - 1) as u64);
    assert!(!first.contains('\u{FFFD}'));
    assert_eq!(text_until(&mut rx, data.len() as u64).await, "çend");
    assert_eq!(
        next(&mut rx).await,
        Got::Exited(data.len() as u64 + 1, None)
    );
}

// ------------------------------------------------------------------ fake console

/// A fake process: echoes its input, exits with 0 on `exit`, prints `N` bytes on
/// `flood N`; closing its console ends it with 1.
struct FakeProc {
    output: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    exit: Mutex<(Option<u32>, Option<Waker>)>,
    written: Mutex<Vec<u8>>,
}

impl FakeProc {
    fn emit(&self, data: &[u8]) {
        if let Some(tx) = self.output.lock().unwrap().as_ref() {
            let _ = tx.send(data.to_vec());
        }
    }

    fn exit(&self, code: u32) {
        let waker = {
            let mut exit = self.exit.lock().unwrap();
            if exit.0.is_none() {
                exit.0 = Some(code);
            }
            exit.1.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct FakeReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl Read for FakeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.rx.recv() {
                Ok(data) => self.pending = data,
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

struct FakeWriter(Arc<FakeProc>);

impl Write for FakeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.written.lock().unwrap().extend_from_slice(buf);
        let text = String::from_utf8_lossy(buf).into_owned();
        self.0.emit(buf);
        if let Some(n) = text.trim().strip_prefix("flood ") {
            let n: usize = n.parse().unwrap();
            for _ in 0..n / 1000 {
                self.0.emit(&[b'#'; 1000]);
            }
        }
        if text.trim() == "exit" {
            self.0.exit(0);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct FakeExit(Arc<FakeProc>);

impl ExitSource for FakeExit {
    fn poll_exit(&mut self, cx: &mut Context<'_>) -> Poll<Result<u32>> {
        let mut exit = self.0.exit.lock().unwrap();
        match exit.0 {
            Some(code) => Poll::Ready(Ok(code)),
            None => {
                exit.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    fn try_exit_code(&self) -> Result<Option<u32>> {
        Ok(self.0.exit.lock().unwrap().0)
    }
}

#[derive(Debug)]
struct FakeConsole {
    proc: Arc<FakeProcHandle>,
    size: Mutex<PtySize>,
}

/// `Debug` wrapper so the console can be `Debug`.
struct FakeProcHandle(Arc<FakeProc>);

impl std::fmt::Debug for FakeProcHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeProc")
    }
}

impl PtyConsole for FakeConsole {
    fn resize(&self, size: PtySize) -> Result<()> {
        *self.size.lock().unwrap() = size;
        Ok(())
    }

    fn size(&self) -> PtySize {
        *self.size.lock().unwrap()
    }

    fn close_within(&self, _: Duration) -> Result<()> {
        self.proc.0.exit(1);
        self.proc.0.output.lock().unwrap().take();
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.proc.0.output.lock().unwrap().is_none()
    }
}

#[derive(Debug, Default)]
struct FakeSpawner {
    procs: Mutex<Vec<Arc<FakeProcHandle>>>,
}

impl FakeSpawner {
    fn proc(&self, i: usize) -> Arc<FakeProc> {
        Arc::clone(&self.procs.lock().unwrap()[i].0)
    }
}

impl PtySpawner for FakeSpawner {
    fn spawn(&self, _: &dyn ProcessGroup, cmd: &PtyCommand) -> Result<PtySession> {
        let (tx, rx) = std::sync::mpsc::channel();
        let proc = Arc::new(FakeProc {
            output: Mutex::new(Some(tx)),
            exit: Mutex::new((None, None)),
            written: Mutex::new(Vec::new()),
        });
        proc.emit(b"$ ");
        let handle = Arc::new(FakeProcHandle(Arc::clone(&proc)));
        self.procs.lock().unwrap().push(Arc::clone(&handle));
        Ok(PtySession {
            pid: 4242,
            reader: Box::new(FakeReader {
                rx,
                pending: Vec::new(),
            }),
            writer: Box::new(FakeWriter(Arc::clone(&proc))),
            exit: ProcessExit::new(4242, Box::new(FakeExit(Arc::clone(&proc)))),
            console: Arc::new(FakeConsole {
                proc: handle,
                size: Mutex::new(cmd.size()?),
            }),
        })
    }
}

#[derive(Debug)]
struct FakeGroup;

impl ProcessGroup for FakeGroup {
    fn create_child(&self, _: GroupOptions) -> Result<Arc<dyn ProcessGroup>> {
        Ok(Arc::new(FakeGroup))
    }
    fn spawn(&self, _: &ProcessCommand) -> Result<SpawnedProcess> {
        unreachable!()
    }
    fn assign(&self, _: u32) -> Result<()> {
        Ok(())
    }
    fn pids(&self) -> Result<Vec<u32>> {
        Ok(Vec::new())
    }
    fn terminate_all(&self, _: u32) -> Result<()> {
        Ok(())
    }
}

fn manager(options: TerminalOptions) -> (TerminalManager, Arc<FakeSpawner>, EventBus) {
    let spawner = Arc::new(FakeSpawner::default());
    let bus = EventBus::new();
    let manager = TerminalManager::new(
        bus.clone(),
        Arc::new(|| Ok(Arc::new(FakeGroup) as Arc<dyn ProcessGroup>)),
        Arc::clone(&spawner) as Arc<dyn PtySpawner>,
        options,
    );
    (manager, spawner, bus)
}

fn spec(title: &str) -> TerminalSpec {
    TerminalSpec {
        title: title.into(),
        shell: "cmd".into(),
        site_id: None,
        command: PtyCommand::new(r"C:\Windows\System32\cmd.exe", std::env::temp_dir()),
    }
}

fn subscribe(
    manager: &TerminalManager,
    id: TerminalId,
    resume_from: Option<u64>,
) -> (FakeSink, mpsc::UnboundedReceiver<Got>, u64) {
    let (sink, rx) = FakeSink::new();
    let last = manager
        .subscribe(id, resume_from, || Ok(sink.clone()))
        .unwrap();
    (sink, rx, last)
}

#[tokio::test]
async fn a_session_echoes_input_once_per_input_seq_and_reports_its_exit() {
    let (manager, spawner, bus) = manager(TerminalOptions::default());
    let mut events = bus.subscribe();
    let dto = manager.open(spec("t"), 16).await.unwrap();
    assert_eq!((dto.cols, dto.rows, dto.pid), (80, 24, Some(4242)));
    let (_sink, mut rx, last) = subscribe(&manager, dto.id, None);
    assert_eq!(last, 0);
    assert_eq!(text_until(&mut rx, 2).await, "$ ");

    let writer = TerminalWriterId::new();
    manager
        .input(dto.id, writer, 1, "echo hi\r".into())
        .await
        .unwrap();
    // A retry of the same input (a new connection) is not written again.
    manager
        .input(dto.id, writer, 1, "echo hi\r".into())
        .await
        .unwrap();
    assert_eq!(text_until(&mut rx, 10).await, "echo hi\r");
    // Another writer starts at 1 too.
    manager
        .input(dto.id, TerminalWriterId::new(), 1, "x".into())
        .await
        .unwrap();
    assert_eq!(text_until(&mut rx, 11).await, "x");
    assert_eq!(
        String::from_utf8(spawner.proc(0).written.lock().unwrap().clone()).unwrap(),
        "echo hi\rx"
    );

    manager.resize(dto.id, 120, 40).await.unwrap();
    assert_eq!(
        manager.resize(dto.id, 0, 40).await.unwrap_err().code(),
        "invalid_input"
    );
    let listed = manager.list();
    assert_eq!((listed[0].cols, listed[0].rows), (120, 40));

    manager
        .input(dto.id, writer, 2, "exit".into())
        .await
        .unwrap();
    assert_eq!(text_until(&mut rx, 15).await, "exit");
    assert_eq!(next(&mut rx).await, Got::Exited(16, Some(0)));
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = events.recv().await.unwrap();
            if e.kind == EventKind::TerminalExited {
                return e;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(event.payload["terminal_id"], dto.id.to_string());
    assert_eq!(event.payload["exit_code"], 0);
    // Still listed (exited) until closed or reaped; input is refused.
    let listed = manager.list();
    assert_eq!(
        (listed[0].exit_code, listed[0].pid, listed[0].last_seq),
        (Some(0), None, 15)
    );
    assert_eq!(
        manager
            .input(dto.id, writer, 3, "x".into())
            .await
            .unwrap_err()
            .code(),
        "conflict"
    );
    manager.close(dto.id).await.unwrap();
    assert!(manager.is_empty());
    assert_eq!(manager.close(dto.id).await.unwrap_err().code(), "not_found");
}

#[tokio::test]
async fn sessions_are_limited_and_close_ends_them_without_exit_code() {
    let (manager, _spawner, _bus) = manager(TerminalOptions::default());
    let a = manager.open(spec("a"), 2).await.unwrap();
    let b = manager.open(spec("b"), 2).await.unwrap();
    let err = manager.open(spec("c"), 2).await.unwrap_err();
    assert_eq!(err.code(), "conflict");
    assert!(err.to_string().contains("terminal.max_sessions"), "{err}");
    let (_sink, mut rx, _) = subscribe(&manager, a.id, None);
    manager.close(a.id).await.unwrap();
    assert_eq!(until_exited(&mut rx).await, None);
    // The slot is free again.
    let c = manager.open(spec("c"), 2).await.unwrap();
    let ids: Vec<_> = manager.list().iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![b.id, c.id]);
    // Shutdown closes everything and refuses new sessions.
    manager.close_all(Duration::from_secs(5)).await;
    assert!(manager.is_empty());
    assert_eq!(
        manager.open(spec("d"), 2).await.unwrap_err().code(),
        "conflict"
    );
}

#[tokio::test]
async fn exited_sessions_are_reaped_only_when_unwatched() {
    let reap_after = Duration::from_millis(500);
    let (manager, spawner, _bus) = manager(TerminalOptions {
        reap_after,
        ..TerminalOptions::default()
    });
    let watched = manager.open(spec("w"), 16).await.unwrap();
    let alone = manager.open(spec("a"), 16).await.unwrap();
    // A subscriber that never reads keeps watching `watched`.
    manager
        .subscribe(watched.id, None, || Ok(Stuck(SubscriptionId::new())))
        .unwrap();
    let (sink, mut rx, _) = subscribe(&manager, alone.id, None);
    spawner.proc(0).exit(3);
    spawner.proc(1).exit(4);
    assert_eq!(until_exited(&mut rx).await, Some(4));
    drop(sink);
    let deadline = Instant::now() + Duration::from_secs(5);
    while manager.list().iter().any(|t| t.exit_code.is_none()) {
        assert!(Instant::now() < deadline, "both exit");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(reap_after * 4).await;
    let left: Vec<_> = manager.list().iter().map(|t| (t.id, t.exit_code)).collect();
    assert_eq!(
        left,
        vec![(watched.id, Some(3))],
        "only the unwatched one is reaped"
    );
    assert_eq!(
        manager
            .subscribe(alone.id, None, || Ok(FakeSink::new().0))
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test]
async fn a_flood_never_waits_for_a_subscriber_that_does_not_read() {
    let (manager, spawner, _bus) = manager(TerminalOptions {
        ring_bytes: 64 * 1024,
        ..TerminalOptions::default()
    });
    let dto = manager.open(spec("f"), 16).await.unwrap();
    manager
        .subscribe(dto.id, None, || Ok(Stuck(SubscriptionId::new())))
        .unwrap();
    let (_sink, mut rx, _) = subscribe(&manager, dto.id, None);
    manager
        .input(dto.id, TerminalWriterId::new(), 1, "flood 1000000".into())
        .await
        .unwrap();
    let produced = 2 + "flood 1000000".len() as u64 + 1_000_000;
    // The reading subscriber falls behind the 64 KiB ring at most, never blocks the flood.
    let mut seen = 0;
    while seen < produced {
        match next(&mut rx).await {
            Got::Out(seq, _) => seen = seq,
            Got::Lagged(_) => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(manager.list()[0].last_seq, produced);
    let _ = spawner;
}
