//! Message, folder, and account-status domain types.

use serde::{Deserialize, Serialize};

bitflags::bitflags! {
    /// IMAP message flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    pub struct MessageFlags: u32 {
        /// `\Seen` — the message has been read.
        const SEEN = 1 << 0;
        /// `\Flagged` — starred.
        const FLAGGED = 1 << 1;
        /// `\Answered` — replied to.
        const ANSWERED = 1 << 2;
        /// `\Deleted` — marked for deletion.
        const DELETED = 1 << 3;
        /// `\Draft`.
        const DRAFT = 1 << 4;
    }
}

impl MessageFlags {
    /// Whether the message is unread.
    pub fn is_unread(self) -> bool {
        !self.contains(Self::SEEN)
    }

    /// Whether the message is starred.
    pub fn is_flagged(self) -> bool {
        self.contains(Self::FLAGGED)
    }
}

/// A single email address (display name + address).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    /// Display name (may be empty).
    pub name: String,
    /// Address (e.g. `user@example.com`).
    pub address: String,
}

/// Summary of a message, as shown in the message list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSummary {
    /// Local store id.
    pub id: i64,
    /// Owning account.
    pub account_id: String,
    /// Owning folder.
    pub folder_id: i64,
    /// IMAP UID within the folder.
    pub uid: u32,
    /// RFC 5322 Message-ID (may be empty).
    pub message_id: String,
    /// Sender.
    pub from: EmailAddress,
    /// Recipients.
    pub to: Vec<EmailAddress>,
    /// CC recipients.
    pub cc: Vec<EmailAddress>,
    /// Subject line.
    pub subject: String,
    /// Date as Unix timestamp (seconds).
    pub date: i64,
    /// IMAP flags.
    pub flags: MessageFlags,
    /// Plain-text preview (first ~200 chars of the text body).
    pub preview: String,
    /// Whether the message has attachments.
    pub has_attachments: bool,
}

/// Attachment metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Local store id.
    pub id: i64,
    /// Owning message (local store id).
    pub message_id: i64,
    /// File name as declared by the sender.
    pub filename: String,
    /// MIME content type.
    pub content_type: String,
    /// Size in bytes.
    pub size: u64,
    /// Local path once downloaded (empty until then).
    pub path: String,
}

/// A fully loaded message, as shown in the reading pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageFull {
    /// Message summary.
    pub summary: MessageSummary,
    /// Plain-text body.
    pub body_text: String,
    /// HTML body (may be empty).
    pub body_html: String,
    /// Attachments.
    pub attachments: Vec<Attachment>,
}

/// A folder in the account's mailbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    /// Local store id.
    pub id: i64,
    /// Owning account.
    pub account_id: String,
    /// Display name (last path component).
    pub name: String,
    /// IMAP path (e.g. `INBOX`, `[Gmail]/Sent Mail`).
    pub path: String,
    /// Unread message count.
    pub unread: u32,
}

/// A saved draft, for listing in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    /// Local store id.
    pub id: i64,
    /// Account the draft was composed for.
    pub account_id: String,
    /// Recipients (display string).
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Unix timestamp (seconds) of the last edit.
    pub updated_at: i64,
}

/// A saved template, for listing in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    /// Local store id.
    pub id: i64,
    /// Template name.
    pub name: String,
    /// Subject line.
    pub subject: String,
}

/// A send-later email waiting to be dispatched.
///
/// Lightweight view of a pending send, for listing in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSend {
    /// Local store id.
    pub id: i64,
    /// Account that will send.
    pub account_id: String,
    /// Unix timestamp (seconds) at which the email is sent.
    pub send_at: i64,
    /// From address.
    pub from: String,
    /// Subject line.
    pub subject: String,
}

/// Connection status of an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    /// Not connected yet.
    Disconnected,
    /// Syncing folders and messages.
    Syncing,
    /// Connected and idle, receiving push updates.
    Connected,
    /// Connection failed; retrying with backoff.
    Reconnecting,
}

impl std::fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Disconnected => "disconnected",
            Self::Syncing => "syncing…",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting…",
        };
        f.write_str(s)
    }
}
