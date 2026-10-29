//! `terminal.*`: terminal sessions hosted by the daemon (the integrated terminal of the
//! GUI, docs/07 §5.10; ADR-0040).
//!
//! A session is a pseudo-console (ConPTY behind a `forge-core` trait) running a shell, or a
//! command, with the Forgebox environment. It lives in the daemon, not in the client: it
//! survives a GUI reload or a dropped connection, and a client reattaches with
//! `terminal.list` + `terminal.subscribe {resume_from}`. Every method needs `admin` (a
//! shell is full control of the machine) and none is `local_only`: the process runs on the
//! daemon's machine, so a remote admin gets a remote shell, like SSH (docs/13 §2).
//!
//! Output is a byte stream kept in a ring (1 MiB) per session. Its `seq` is a byte offset:
//! an `output` item's `seq` is the number of bytes the session had produced once its
//! `data` was written (the offset just past it), so `resume_from: N` resends from byte
//! `N`. A cursor older than the ring gets `terminal.lagged {missed}` (bytes) and then the
//! whole ring: the client resets the screen and replays. The final `exited` item has
//! `seq` = produced bytes + 1 and ends the stream.
//!
//! `terminal.input` is not `mutating` (the idempotency cache would keep every keystroke).
//! Retries are made safe by a key the client owns instead, because the IPC `SessionId`
//! changes when a retry is resent on a new connection and the client name is the same for
//! every GUI:
//! - `writer` ([`TerminalWriterId`]) is a ULID the client generates once per tab
//!   (attachment) and reuses for every input of that tab, across reconnects and retries;
//! - `input_seq` counts that writer's inputs from 1, by 1, with at most one input in flight
//!   (what is typed meanwhile is batched into the next one), so a retry is always the
//!   writer's newest input and order is kept.
//!
//! The daemon keeps the last accepted `input_seq` per `(terminal, writer)`. An input whose
//! `input_seq` is not greater is a retry: it is answered `Ack` and not written again. Two
//! clients typing into one session (local GUI + remote admin) are two writers and never drop
//! each other's input. A client that loses its counter (a GUI reload) takes a new writer
//! instead of restarting at 1. The map is bounded ([`TERMINAL_MAX_WRITERS`]; the writer idle
//! the longest is forgotten first) and is dropped with the session.

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use specta::Type;

use super::common::{Resumable, SiteRef};
use forge_core::Timestamp;
use forge_core::ids::{SiteId, SubscriptionId, TerminalId, TerminalWriterId};

/// Default size of a new session.
pub const TERMINAL_DEFAULT_COLS: u16 = 120;
pub const TERMINAL_DEFAULT_ROWS: u16 = 30;

/// Writers whose last `input_seq` a session remembers for deduplication; past it, the
/// writer idle the longest is forgotten (a GUI reload adds a writer, so the map must not
/// grow with the session's age). A forgotten writer's next input is accepted as new.
pub const TERMINAL_MAX_WRITERS: usize = 32;

fn default_cols() -> u16 {
    TERMINAL_DEFAULT_COLS
}

fn default_rows() -> u16 {
    TERMINAL_DEFAULT_ROWS
}

/// Params of `terminal.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalOpenParams {
    /// Working folder on the daemon's machine; the site root with `site`, the root
    /// otherwise.
    #[serde(default)]
    pub dir: Option<String>,
    /// Opens "in" a site: its root as the working folder and its PHP first in `PATH`.
    #[serde(default)]
    pub site: Option<SiteRef>,
    /// Tool → version overrides, as in `shell.env` (`{"php": "8.2"}`).
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
    /// `pwsh`, `powershell` or `cmd`; the configured integrated shell when `None`.
    #[serde(default)]
    pub shell: Option<String>,
    /// Runs this argv instead of an interactive shell (`db.shell`'s command); the session
    /// ends with it.
    #[serde(default)]
    pub command: Vec<String>,
    /// Extra variables on top of the Forgebox environment. May contain credentials: the
    /// daemon never logs them.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_cols")]
    pub cols: u16,
    #[serde(default = "default_rows")]
    pub rows: u16,
    /// Tab title; derived from the site, the command or the shell when `None`.
    #[serde(default)]
    pub title: Option<String>,
}

/// A terminal session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalDto {
    pub id: TerminalId,
    pub title: String,
    /// Shell that runs (`pwsh`), or the program of `command`.
    pub shell: String,
    /// Working folder it started in (informative).
    pub cwd: String,
    #[serde(default)]
    pub site_id: Option<SiteId>,
    pub cols: u16,
    pub rows: u16,
    /// Process id while it runs.
    pub pid: Option<u32>,
    pub started_at: Timestamp,
    /// Exit code once it ended (Windows `DWORD` bits as `i32`).
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub exited_at: Option<Timestamp>,
    /// Output bytes produced so far: the `seq` to resume from to get only what follows.
    #[specta(type = specta_typescript::Number)]
    pub last_seq: u64,
}

/// Result of `terminal.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalListDto {
    pub terminals: Vec<TerminalDto>,
}

/// Params of `terminal.close`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalIdParams {
    pub id: TerminalId,
}

/// Params of `terminal.input`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalInputParams {
    pub id: TerminalId,
    /// Who writes: generated by the client once per tab (attachment) and reused for every
    /// input of it, across reconnects and retries. A client that loses its `input_seq`
    /// counter (reload) generates a new one.
    pub writer: TerminalWriterId,
    /// Keystrokes / pasted text, as the terminal emulator produced them (UTF-8).
    pub data: String,
    /// Counts this writer's inputs from 1, by 1, with at most one input in flight. The
    /// daemon keeps the last accepted value per `(id, writer)`: an input whose `input_seq`
    /// is not greater is a retry, answered `Ack` without writing it again. `0` is refused.
    #[specta(type = specta_typescript::Number)]
    pub input_seq: NonZeroU64,
}

/// Params of `terminal.resize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalResizeParams {
    pub id: TerminalId,
    pub cols: u16,
    pub rows: u16,
}

/// Params of `terminal.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalSubscribeParams {
    pub id: TerminalId,
    /// Resend output from this byte offset (the last `seq` received); without it, the whole
    /// ring is sent first.
    #[serde(default)]
    #[specta(type = Option<specta_typescript::Number>)]
    pub resume_from: Option<u64>,
}

impl Resumable for TerminalSubscribeParams {
    fn resume_from(&self) -> Option<u64> {
        self.resume_from
    }
    fn set_resume_from(&mut self, seq: Option<u64>) {
        self.resume_from = seq;
    }
}

/// One item of a terminal stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalItemDto {
    /// Output bytes, UTF-8 (an incomplete trailing sequence is held until it completes).
    Output { data: String },
    /// The process ended; the last item of the stream.
    Exited { exit_code: Option<i32> },
}

/// Params of the `terminal.output` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TerminalEnvelope {
    pub subscription_id: SubscriptionId,
    /// Byte offset just past this item's output (`exited`: produced bytes + 1).
    #[specta(type = specta_typescript::Number)]
    pub seq: u64,
    pub item: TerminalItemDto,
}
