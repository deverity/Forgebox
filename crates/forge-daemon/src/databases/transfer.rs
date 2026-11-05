//! `db.export` / `db.import` (M2-E3-T6, docs/09 §db, docs/03 §7.4): long jobs that stream
//! a database through the `dump` / `restore` clients of `spec.database`, with no code per
//! tool and never a whole dump in memory.
//!
//! * **Export**: the dump client runs in the one-shot runner
//!   ([`forge_supervisor::oneshot`]); its stdout flows through a bounded channel
//!   ([`CHANNEL_CHUNKS`] chunks of 64 KiB, so at most 1 MiB in flight) to a blocking
//!   writer that compresses by the file name (`.sql.zst` zstd level [`ZSTD_LEVEL`],
//!   `.sql.gz` gzip, `.sql` plain) into `<output>.partial`, flushes it to disk and only
//!   then renames it to `<output>`. Progress: uncompressed bytes and the average speed
//!   (the total is unknown). Failure or cancellation kills the job of the client and
//!   deletes the `.partial` file.
//! * **Import**: a blocking reader decompresses `.sql`, `.sql.gz` (all members) or
//!   `.sql.zst` and feeds the restore client's stdin through the same kind of channel;
//!   progress is the share of the **input file** read (monotonic, 100 at its end). A
//!   corrupt input cancels the client before its stdin closes, so it never commits a
//!   half file as if it were whole. A non-zero exit fails with the last 20 stderr lines.
//! * A slow disk or a slow server only slows the job: every wait is on a bounded channel
//!   or a pipe, nothing runs on an async worker but the pumps (ADR-0011).

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use forge_core::Timestamp;
use forge_core::error::{ForgeError, Result};
use forge_core::jobs::JobProgress;
use forge_core::runtime::spawn_blocking_timeout;
use forge_ipc::api::{Ack, DbExportDto};
use forge_manifest::database::RenderedCommand;
use forge_supervisor::oneshot::{self, CHUNK, Oneshot, OneshotOutcome};
use forge_supervisor::{JobTree, SpawnSpec};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{Target, render_error};

/// Chunks in flight between a client and the file (64 KiB each).
pub const CHANNEL_CHUNKS: usize = 16;
/// zstd level of `.sql.zst` exports (the library default: fast, about 3-4x on SQL).
pub const ZSTD_LEVEL: i32 = 3;
/// A progress report after this many bytes...
pub const PROGRESS_STEP: u64 = 1 << 20;
/// ...or after this long with some progress (a well compressed input is read slowly).
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Budget of a file system step around a transfer (create, rename, delete, stat).
const FS_BUDGET: Duration = Duration::from_secs(10);
/// Buffer of the compressed file writer and reader.
const FILE_BUFFER: usize = 1 << 20;

/// Where a transfer reports progress (the job's [`forge_core::jobs::JobContext`]).
pub type ProgressSink = Arc<dyn Fn(JobProgress) + Send + Sync>;

/// Compression of a dump file, from its name (case-insensitive extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpFormat {
    Sql,
    Gzip,
    Zstd,
}

impl DumpFormat {
    /// `.sql`, `.sql.gz` or `.sql.zst`; anything else is `invalid_input`.
    pub fn of(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if name.ends_with(".sql.zst") {
            Ok(Self::Zstd)
        } else if name.ends_with(".sql.gz") {
            Ok(Self::Gzip)
        } else if name.ends_with(".sql") {
            Ok(Self::Sql)
        } else {
            Err(ForgeError::InvalidInput(format!(
                "{}: the file name must end in .sql, .sql.gz or .sql.zst",
                path.display()
            )))
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sql => "SQL",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
        }
    }
}

/// A validated export or import: the client, the file and its format. Nothing started.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// `mysql/default`.
    pub label: String,
    pub database: String,
    /// The `dump` or `restore` client of the manifest.
    pub spec: SpawnSpec,
    /// Output (export) or input (import) file on the daemon's machine.
    pub path: PathBuf,
    pub format: DumpFormat,
}

fn require_absolute(path: &Path) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(ForgeError::InvalidInput(format!(
            "{}: the path must be absolute on the daemon's machine",
            path.display()
        )))
    }
}

impl Target {
    /// The client as a [`SpawnSpec`]: in the version folder, its own folder on `PATH`
    /// (DLLs), the deterministic environment of the supervisor.
    fn client_spec(&self, command: RenderedCommand) -> Result<SpawnSpec> {
        let dir = self.version_dir()?.to_path_buf();
        let exe = dir.join(command.exe.replace('/', "\\"));
        let mut spec = SpawnSpec::new(&exe, &dir).args(command.args);
        if let Some(parent) = exe.parent() {
            spec = spec.path_entry(parent);
        }
        spec.new_process_group = false;
        Ok(spec)
    }

    /// `backups/<tool>/<instance>/<db>-<UTC yyyymmddThhmmssZ>.sql.zst`.
    pub fn default_export_path(&self, backups: &Path, database: &str, now: Timestamp) -> PathBuf {
        let (tool, instance) = self
            .label
            .split_once('/')
            .unwrap_or((&self.label, "default"));
        let stamp = now.strftime("%Y%m%dT%H%M%SZ");
        backups
            .join(tool)
            .join(instance)
            .join(format!("{database}-{stamp}.sql.zst"))
    }

    /// Validates an export of `database` to `output` (name and command rendered first,
    /// then the file name, then the state of the instance).
    pub fn prepare_export(&self, database: &str, output: &Path) -> Result<Transfer> {
        let dump = self.spec.dump.as_ref().ok_or_else(|| {
            ForgeError::InvalidInput(format!("{} declares no dump client", self.label))
        })?;
        let command = self
            .spec
            .render_command(dump, &self.vars(Some(database), None))
            .map_err(render_error)?;
        require_absolute(output)?;
        let format = DumpFormat::of(output)?;
        self.require_running()?;
        Ok(Transfer {
            label: self.label.clone(),
            database: database.to_owned(),
            spec: self.client_spec(command)?,
            path: output.to_path_buf(),
            format,
        })
    }

    /// Validates an import of `input` into `database` (system databases refused).
    pub fn prepare_import(&self, database: &str, input: &Path) -> Result<Transfer> {
        let restore = self.spec.restore.as_ref().ok_or_else(|| {
            ForgeError::InvalidInput(format!("{} declares no restore client", self.label))
        })?;
        let command = self
            .spec
            .render_command(restore, &self.vars(Some(database), None))
            .map_err(render_error)?;
        self.refuse_system(database)?;
        require_absolute(input)?;
        let format = DumpFormat::of(input)?;
        self.require_running()?;
        Ok(Transfer {
            label: self.label.clone(),
            database: database.to_owned(),
            spec: self.client_spec(command)?,
            path: input.to_path_buf(),
            format,
        })
    }
}

/// `<path>.partial`.
pub fn partial_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".partial");
    PathBuf::from(name)
}

/// Maps a failed client run (non-zero exit) to an error carrying its stderr tail.
fn stream_failure(label: &str, what: &str, outcome: &OneshotOutcome) -> ForgeError {
    let text = outcome.stderr_tail.join("\n");
    let lower = text.to_ascii_lowercase();
    let detail = if text.trim().is_empty() {
        "(nothing on stderr)".to_owned()
    } else {
        text
    };
    // Exit codes are bit patterns (NTSTATUS); show the signed value like `run_probe`.
    let code = outcome.exit_code as i32;
    if lower.contains("unknown database")
        || lower.contains("does not exist")
        || lower.contains("doesn't exist")
    {
        ForgeError::NotFound(format!("{label}: {detail}"))
    } else {
        ForgeError::Conflict(format!(
            "{label}: the {what} client failed (exit {code}):\n{detail}"
        ))
    }
}

/// Throttled progress: a report every [`PROGRESS_STEP`] bytes (or [`PROGRESS_EVERY`]),
/// with the average speed and, when the total is known, the percentage and the ETA.
struct Meter {
    started: Instant,
    last: Instant,
    total: Option<u64>,
    reported: u64,
    message: String,
}

impl Meter {
    fn new(total: Option<u64>, message: String) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last: now,
            total,
            reported: 0,
            message,
        }
    }

    fn update(&mut self, done: u64, progress: &ProgressSink) {
        if done >= self.reported + PROGRESS_STEP
            || (done > self.reported && self.last.elapsed() >= PROGRESS_EVERY)
        {
            self.report(done, progress);
        }
    }

    fn report(&mut self, done: u64, progress: &ProgressSink) {
        self.reported = done;
        self.last = Instant::now();
        let secs = self.started.elapsed().as_secs_f64().max(0.001);
        let speed = (done as f64 / secs) as u64;
        let mut update = JobProgress::new()
            .message(self.message.clone())
            .bytes(done, self.total);
        let eta = self.total.and_then(|total| {
            let left = total.saturating_sub(done);
            (speed > 0).then(|| left / speed)
        });
        update = update.rate(speed, eta);
        if let Some(total) = self.total {
            let pct = if total == 0 {
                100.0
            } else {
                (done as f64 * 100.0 / total as f64) as f32
            };
            update = update.pct(pct);
        }
        progress(update);
    }
}

// ------------------------------------------------------------------ export

/// The compressing writer of an export.
enum Sink {
    Sql(BufWriter<File>),
    Gzip(flate2::write::GzEncoder<BufWriter<File>>),
    Zstd(zstd::Encoder<'static, BufWriter<File>>),
}

impl Sink {
    fn new(file: File, format: DumpFormat) -> io::Result<Self> {
        let buffered = BufWriter::with_capacity(FILE_BUFFER, file);
        Ok(match format {
            DumpFormat::Sql => Self::Sql(buffered),
            DumpFormat::Gzip => Self::Gzip(flate2::write::GzEncoder::new(
                buffered,
                flate2::Compression::default(),
            )),
            DumpFormat::Zstd => Self::Zstd(zstd::Encoder::new(buffered, ZSTD_LEVEL)?),
        })
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Sql(w) => w.write_all(bytes),
            Self::Gzip(w) => w.write_all(bytes),
            Self::Zstd(w) => w.write_all(bytes),
        }
    }

    /// Ends the stream (frame/trailer), flushes and syncs the file to disk.
    fn finish(self) -> io::Result<()> {
        let buffered = match self {
            Self::Sql(w) => w,
            Self::Gzip(w) => w.finish()?,
            Self::Zstd(w) => w.finish()?,
        };
        let file = buffered
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        file.sync_all()
    }
}

/// Blocking: the chunks of `rx` into `file`. A write error cancels the run (`abort`).
/// Returns the uncompressed bytes written.
fn write_dump(
    file: File,
    format: DumpFormat,
    mut rx: mpsc::Receiver<Vec<u8>>,
    progress: &ProgressSink,
    abort: &CancellationToken,
    meter: &mut Meter,
    partial: &Path,
) -> Result<u64> {
    let failed = |err: io::Error| {
        abort.cancel();
        ForgeError::io(format!("writing {}", partial.display()), err)
    };
    let mut sink = Sink::new(file, format).map_err(failed)?;
    let mut done = 0u64;
    while let Some(chunk) = rx.blocking_recv() {
        sink.write_all(&chunk).map_err(failed)?;
        done += chunk.len() as u64;
        meter.update(done, progress);
    }
    if abort.is_cancelled() {
        // The run is being cancelled or failed: the file is going away.
        return Ok(done);
    }
    sink.finish()
        .map_err(|e| ForgeError::io(format!("finishing {}", partial.display()), e))?;
    Ok(done)
}

async fn remove_partial(partial: PathBuf) {
    let shown = partial.display().to_string();
    let removed = spawn_blocking_timeout("deleting a partial export", FS_BUDGET, move || {
        match std::fs::remove_file(&partial) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ForgeError::io_at("deleting", &partial, e)),
        }
    })
    .await;
    if let Err(err) = removed {
        tracing::warn!(file = %shown, %err, "a partial export was left behind");
    }
}

/// Runs a prepared export to its end (or until `cancel`).
pub async fn export(
    transfer: Transfer,
    jobs: &JobTree,
    progress: ProgressSink,
    cancel: &CancellationToken,
) -> Result<DbExportDto> {
    let Transfer {
        label,
        database,
        spec,
        path: output,
        format,
    } = transfer;
    let partial = partial_path(&output);
    let file = {
        let partial = partial.clone();
        spawn_blocking_timeout("creating the export file", FS_BUDGET, move || {
            if let Some(parent) = partial.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ForgeError::io_at("creating", parent, e))?;
            }
            File::create(&partial).map_err(|e| ForgeError::io_at("creating", &partial, e))
        })
        .await?
    };
    let (tx, rx) = mpsc::channel(CHANNEL_CHUNKS);
    let abort = cancel.child_token();
    let writer = {
        let abort = abort.clone();
        let progress = Arc::clone(&progress);
        let partial = partial.clone();
        let mut meter = Meter::new(None, format!("exporting {database} from {label}"));
        tokio::task::spawn_blocking(move || {
            let done = write_dump(file, format, rx, &progress, &abort, &mut meter, &partial)?;
            Ok::<_, ForgeError>((done, meter))
        })
    };
    let run = oneshot::run(jobs, Oneshot::new(spec).stdout(tx), &abort).await;
    let written = writer
        .await
        .unwrap_or_else(|e| Err(ForgeError::Internal(format!("export writer: {e}"))));
    let settled = (|| {
        // A writer error caused the cancellation of the run: it is the real cause.
        let (done, meter) = written?;
        let outcome = run?;
        if !outcome.success() {
            return Err(stream_failure(&label, "dump", &outcome));
        }
        Ok((done, meter))
    })();
    let (done, mut meter) = match settled {
        Ok(ok) => ok,
        Err(err) => {
            remove_partial(partial).await;
            return Err(err);
        }
    };
    let size_bytes = {
        let output = output.clone();
        let partial = partial.clone();
        spawn_blocking_timeout("publishing the export file", FS_BUDGET, move || {
            std::fs::rename(&partial, &output)
                .map_err(|e| ForgeError::io_at("renaming", &partial, e))?;
            std::fs::metadata(&output)
                .map(|m| m.len())
                .map_err(|e| ForgeError::io_at("reading", &output, e))
        })
        .await
    };
    let size_bytes = match size_bytes {
        Ok(size) => size,
        Err(err) => {
            remove_partial(partial).await;
            return Err(err);
        }
    };
    meter.total = Some(done);
    meter.report(done, &progress);
    Ok(DbExportDto {
        path: forge_core::paths::plain_path(&output)
            .to_string_lossy()
            .into_owned(),
        size_bytes,
    })
}

// ------------------------------------------------------------------ import

/// Counts the bytes read from the file under a decoder.
struct Counting<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// How the reader of an import ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadEnd {
    /// Every decompressed byte was handed to the client.
    Complete,
    /// The client (or the run) stopped taking input first.
    Stopped,
}

/// Fills `buf` as far as the source allows (a decoder may return short reads).
fn read_chunk(source: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match source.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Blocking: decompresses `path` into `tx`. A read or decode error cancels the run
/// (`abort`) **before** the channel closes.
fn read_dump(
    path: &Path,
    format: DumpFormat,
    tx: mpsc::Sender<Vec<u8>>,
    progress: &ProgressSink,
    abort: &CancellationToken,
    message: String,
) -> Result<ReadEnd> {
    let failed = |err: io::Error| {
        abort.cancel();
        match err.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof | io::ErrorKind::Other => {
                ForgeError::InvalidInput(format!(
                    "{}: not a valid {} file: {err}",
                    path.display(),
                    format.label()
                ))
            }
            _ => ForgeError::io(format!("reading {}", path.display()), err),
        }
    };
    let file = File::open(path).map_err(|e| {
        abort.cancel();
        ForgeError::io_at("opening", path, e)
    })?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let count = Arc::new(AtomicU64::new(0));
    let counted = BufReader::with_capacity(
        FILE_BUFFER,
        Counting {
            inner: file,
            count: Arc::clone(&count),
        },
    );
    let mut source: Box<dyn Read> = match format {
        DumpFormat::Sql => Box::new(counted),
        DumpFormat::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(counted)),
        DumpFormat::Zstd => Box::new(zstd::Decoder::with_buffer(counted).map_err(failed)?),
    };
    let mut meter = Meter::new(Some(size), message);
    loop {
        let mut buf = vec![0u8; CHUNK];
        let n = read_chunk(source.as_mut(), &mut buf).map_err(failed)?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        if tx.blocking_send(buf).is_err() {
            return Ok(ReadEnd::Stopped);
        }
        meter.update(count.load(Ordering::Relaxed), progress);
    }
    meter.report(size, progress);
    Ok(ReadEnd::Complete)
}

/// Runs a prepared import to its end (or until `cancel`).
pub async fn import(
    transfer: Transfer,
    jobs: &JobTree,
    progress: ProgressSink,
    cancel: &CancellationToken,
) -> Result<Ack> {
    let Transfer {
        label,
        database,
        spec,
        path: input,
        format,
    } = transfer;
    let (tx, rx) = mpsc::channel(CHANNEL_CHUNKS);
    let abort = cancel.child_token();
    let reader = {
        let abort = abort.clone();
        let progress = Arc::clone(&progress);
        let input = input.clone();
        let message = format!("importing {} into {database} on {label}", input.display());
        tokio::task::spawn_blocking(move || {
            read_dump(&input, format, tx, &progress, &abort, message)
        })
    };
    let run = oneshot::run(jobs, Oneshot::new(spec).stdin(rx), &abort).await;
    let read = reader
        .await
        .unwrap_or_else(|e| Err(ForgeError::Internal(format!("import reader: {e}"))));
    // A reader error cancelled the run: it is the real cause.
    let end = read?;
    let outcome = run?;
    if !outcome.success() {
        return Err(stream_failure(&label, "restore", &outcome));
    }
    if end == ReadEnd::Stopped {
        return Err(ForgeError::Conflict(format!(
            "{label}: the restore client exited before reading all of {}",
            input.display()
        )));
    }
    Ok(Ack::with_message(format!(
        "{} imported into {database}",
        input.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_come_from_the_file_name() {
        assert_eq!(
            DumpFormat::of(Path::new(r"C:\b\loja.sql")).unwrap(),
            DumpFormat::Sql
        );
        assert_eq!(
            DumpFormat::of(Path::new(r"C:\b\loja.SQL.GZ")).unwrap(),
            DumpFormat::Gzip
        );
        assert_eq!(
            DumpFormat::of(Path::new(r"C:\b\loja-1.sql.zst")).unwrap(),
            DumpFormat::Zstd
        );
        for bad in [r"C:\b\loja.zst", r"C:\b\loja.txt", r"C:\b\", r"C:\b\sql"] {
            assert_eq!(
                DumpFormat::of(Path::new(bad)).unwrap_err().code(),
                "invalid_input",
                "{bad}"
            );
        }
        assert_eq!(
            partial_path(Path::new(r"C:\b\loja.sql.zst")),
            PathBuf::from(r"C:\b\loja.sql.zst.partial")
        );
    }

    #[test]
    fn failures_carry_the_stderr_tail() {
        let outcome = |lines: &[&str]| OneshotOutcome {
            pid: 1,
            exit_code: 1,
            stderr_tail: lines.iter().map(|l| (*l).to_owned()).collect(),
            stdin_bytes: 0,
            stdout_bytes: 0,
        };
        let err = stream_failure("mysql/default", "restore", &outcome(&["a", "b"]));
        assert_eq!(err.code(), "conflict");
        assert!(err.to_string().ends_with("(exit 1):\na\nb"), "{err}");
        let err = stream_failure(
            "mysql/default",
            "dump",
            &outcome(&["mysqldump: Got error: 1049: Unknown database 'x'"]),
        );
        assert_eq!(err.code(), "not_found");
        let err = stream_failure("mysql/default", "dump", &outcome(&[]));
        assert!(err.to_string().contains("(nothing on stderr)"), "{err}");
    }

    #[test]
    fn the_meter_reports_each_step_with_pct_when_the_total_is_known() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: ProgressSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |p| seen.lock().unwrap().push(p))
        };
        let mut meter = Meter::new(Some(4 * PROGRESS_STEP), "x".into());
        for done in (0..=4 * PROGRESS_STEP).step_by(64 * 1024) {
            meter.update(done, &sink);
        }
        let seen = seen.lock().unwrap();
        let pcts: Vec<f32> = seen.iter().map(|p| p.pct.unwrap()).collect();
        assert_eq!(pcts, [25.0, 50.0, 75.0, 100.0]);
        assert!(seen.iter().all(|p| p.speed_bps.is_some()));
        assert_eq!(seen[0].bytes_total, Some(4 * PROGRESS_STEP));
    }
}
