//! `mail.*`: captured e-mail (Mailpit, docs/09 §mail).
//!
//! The backend is chosen by the optional `spec.mail` block of a manifest (API family
//! `mailpit-v1`, port names, `.env` variables; ADR-0040), never by tool id. Message ids
//! are the backend's own ids (opaque strings). `mail.changed` events carry the counts, so
//! clients never poll.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use forge_core::Timestamp;
use forge_core::ids::InstanceId;

/// Result of `mail.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailStatusDto {
    pub running: bool,
    pub instance_id: Option<InstanceId>,
    pub smtp_port: Option<u16>,
    /// Web UI (`http://127.0.0.1:8025`).
    pub ui_url: Option<String>,
    pub messages: u32,
    pub unread: u32,
}

/// Params of `mail.list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailListParams {
    #[serde(default)]
    pub unread: bool,
    /// Default 50.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Search text in the backend's syntax (Mailpit: `from:`, `subject:`, free text).
    /// API 1.1.
    #[serde(default)]
    pub query: Option<String>,
    /// Messages to skip (newest first), for paging. API 1.1.
    #[serde(default)]
    pub offset: Option<u32>,
}

/// A captured message (summary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailSummaryDto {
    /// Mailpit message id.
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub received_at: Timestamp,
    pub read: bool,
    #[specta(type = specta_typescript::Number)]
    pub size_bytes: u64,
    pub attachments: u32,
}

/// Result of `mail.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailListDto {
    pub messages: Vec<MailSummaryDto>,
    /// Messages matching the request (`query`, `unread`): there are more pages while
    /// `offset + messages.len() < total`. API 1.1 (0 from 1.0 daemons).
    #[serde(default)]
    pub total: u32,
    /// Unread messages in the whole mailbox (the badge). API 1.1.
    #[serde(default)]
    pub unread: u32,
}

/// Params of `mail.show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailShowParams {
    pub id: String,
    /// Marks the message read (default `true`, what showing always did). Honoured only for
    /// `operator` and above: a `viewer` reads without changing the mailbox. API 1.1.
    #[serde(default = "default_true")]
    pub mark_read: bool,
}

fn default_true() -> bool {
    true
}

/// An attachment (or inline part) of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailAttachmentDto {
    /// Backend part id, for `mail.attachment`.
    pub part_id: String,
    pub file_name: String,
    pub content_type: String,
    #[specta(type = specta_typescript::Number)]
    pub size_bytes: u64,
    /// Inline part (an image referenced by `cid:` in the HTML).
    #[serde(default)]
    pub inline: bool,
    /// `Content-ID` without the angle brackets (`cid:` references).
    #[serde(default)]
    pub content_id: Option<String>,
}

/// Result of `mail.show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailMessageDto {
    pub summary: MailSummaryDto,
    pub headers: BTreeMap<String, Vec<String>>,
    pub text: Option<String>,
    /// Untrusted HTML: clients render it sandboxed (no scripts, no same-origin, remote
    /// images blocked; docs/11).
    pub html: Option<String>,
    /// API 1.1 (empty from 1.0 daemons), like the fields below.
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    #[serde(default)]
    pub reply_to: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<MailAttachmentDto>,
}

/// Params of `mail.mark_read` (`forge mail read <id>... [--unread]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailMarkReadParams {
    pub ids: Vec<String>,
    /// `false` marks them unread. Default `true`.
    #[serde(default = "default_true")]
    pub read: bool,
}

/// Params of `mail.delete` (`forge mail delete <id>...`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailDeleteParams {
    pub ids: Vec<String>,
    /// Destructive: must be `true`.
    #[serde(default)]
    pub confirm: bool,
}

/// Params of `mail.attachment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailAttachmentParams {
    /// Message id.
    pub id: String,
    /// [`MailAttachmentDto::part_id`].
    pub part_id: String,
}

/// Largest attachment `mail.attachment` returns (base64 grows it by 4/3, and a frame
/// holds 8 MiB); bigger ones answer `invalid_input` pointing at the Mailpit UI.
pub const MAIL_ATTACHMENT_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Result of `mail.attachment`: the decoded size is at most
/// [`MAIL_ATTACHMENT_MAX_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailAttachmentContentDto {
    pub file_name: String,
    pub content_type: String,
    #[specta(type = specta_typescript::Number)]
    pub size_bytes: u64,
    /// The content, base64 (standard alphabet, padded).
    pub content_base64: String,
}

/// Params of `mail.clear`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailClearParams {
    /// Destructive: must be `true`.
    #[serde(default)]
    pub confirm: bool,
}
