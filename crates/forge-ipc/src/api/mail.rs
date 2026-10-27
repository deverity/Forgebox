//! `mail.*`: captured e-mail (Mailpit, docs/09 §mail).

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
}

/// Params of `mail.show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailShowParams {
    pub id: String,
}

/// Result of `mail.show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailMessageDto {
    pub summary: MailSummaryDto,
    pub headers: BTreeMap<String, Vec<String>>,
    pub text: Option<String>,
    pub html: Option<String>,
}

/// Params of `mail.clear`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct MailClearParams {
    /// Destructive: must be `true`.
    #[serde(default)]
    pub confirm: bool,
}
