//! Protocol of `forgebox-helper`, the only elevated binary (docs/11 §1, ADR-0022). Shared
//! by the daemon (client side, [`HelperClient`]) and the helper itself (M1-E5-T4).
//!
//! * [`HelperOp`]: the closed allowlist of privileged operations, one variant per row of
//!   docs/11 §1 plus `fs.set_case_sensitive` (docs/15 §3). Anything else does not even
//!   parse. [`HelperOp::validate`] checks every parameter against a [`HelperPolicy`]
//!   (configured TLDs, root, parked directories): namespaces and hosts only under the
//!   configured TLDs **and** under suffixes reserved for local use
//!   ([`crate::dns_name::LOCAL_USE_SUFFIXES`]), loopback addresses only, executables only in
//!   `<root>/bin`, firewall rules only named `Forgebox-*`, Defender exclusions only for the
//!   root itself.
//! * [`HelperRequest`] / [`HelperResponse`]: one JSON document each way, versioned (`v`),
//!   carrying the [`RequestId`](crate::ids::RequestId) of the API call that caused it.
//! * Transport ([`crate::platform::Elevation`]): one helper process per request, started
//!   with `ShellExecuteExW` verb `runas` (one UAC prompt). Standard handles do not cross
//!   that boundary, so the request and the response travel through two files that the
//!   daemon creates in `<root>/state/locks/` before starting the helper
//!   (`helper-<request_id>.request.json` and `.response.json`, see [`request_path`] and
//!   [`response_path`]), with a protected DACL granting the current user and
//!   `BUILTIN\Administrators` only. The helper reads the first, writes the second (opening
//!   it, never creating it), and exits; the daemon waits for the process, reads the
//!   answer and deletes both files.
//! * [`hosts`]: the managed block of the system `hosts` file (fallback of the NRPT rule,
//!   docs/06 §5.1), rendered and parsed here so that the daemon (diagnosis) and the helper
//!   (writing) agree byte for byte.
//!
//! Requirements on the helper side (M1-E5-T4), part of the protocol:
//! 1. `request.root` must be the helper's own root (the folder of `forgebox-helper.exe`);
//!    the policy (TLDs, parked directories) is read by the helper from that root's
//!    `etc/forgebox.toml`, never taken from the request.
//! 2. The request file must be `<root>/state/locks/helper-<request_id>.request.json`; the
//!    response file is opened with `OPEN_EXISTING` and refused when it is a reparse point
//!    or has more than one hard link (no write through a planted link).
//! 3. [`HelperRequest::from_json`] then [`HelperOp::validate`]; paths are canonicalized
//!    again right before acting (a junction inside `bin/` must not lead outside it).
//! 4. Every operation (accepted or refused) is appended to `logs/helper.log`; the daemon
//!    publishes `helper.op {name, ok}`.

mod client;
pub mod hosts;
mod op;
mod protocol;

pub use client::{FakeElevation, HelperClient};
pub use hosts::{HOSTS_BLOCK_NAME, HostsEntry};
pub use op::{
    FIREWALL_RULE_PREFIX, HOSTS_ALLOWED_IPS, HelperOp, HelperPolicy, MAX_HOSTS_ENTRIES,
    NRPT_NAMESERVERS,
};
pub use protocol::{
    HELPER_EXE_NAME, HELPER_PROTOCOL_VERSION, HelperRequest, HelperResponse, request_path,
    response_path,
};
