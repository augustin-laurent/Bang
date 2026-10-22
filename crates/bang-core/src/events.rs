//! Commands and events crossing the core/UI boundary.

use crate::account::{AccountId, AccountConfig};
use crate::message::{AccountStatus, Folder, MessageFlags, MessageFull, MessageSummary};
use serde::{Deserialize, Serialize};

/// A command from the UI to the mail core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UiCommand {
    /// Add an account and start syncing it.
    AddAccount(AccountConfig),
    /// Run an interactive OAuth2 login and add the resulting account.
    OAuthLogin {
        /// Provider to authenticate against.
        provider: crate::account::OAuthProvider,
        /// OAuth2 client id from the local config.
        client_id: String,
    },
    /// Remove an account and stop its syncer.
    RemoveAccount(AccountId),
    /// Select a folder (loads its messages).
    SelectFolder {
        /// Account to select in.
        account: AccountId,
        /// Folder to select.
        folder_id: i64,
    },
    /// Open a message (fetches the full body if not cached).
    OpenMessage {
        /// Account the message belongs to.
        account: AccountId,
        /// Local store id of the message.
        message_id: i64,
    },
    /// Add or remove flags on messages.
    SetFlags {
        /// Account the messages belong to.
        account: AccountId,
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
        /// Flags to add or remove.
        flags: MessageFlags,
        /// `true` to add the flags, `false` to remove them.
        add: bool,
    },
    /// Delete messages (expunge on the server, remove from the local store).
    Delete {
        /// Account the messages belong to.
        account: AccountId,
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
    },
    /// Snooze messages until the given Unix timestamp (seconds).
    ///
    /// Snoozed messages are hidden from folder listings until the time passes.
    Snooze {
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
        /// Unix timestamp (seconds) at which the messages reappear.
        until: i64,
    },
    /// Clear the snooze on messages so they reappear immediately.
    Unsnooze {
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
    },
    /// Schedule a follow-up reminder on messages at the given Unix timestamp.
    SetFollowUp {
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
        /// Unix timestamp (seconds) at which the reminder fires.
        at: i64,
    },
    /// Clear the follow-up reminder on messages.
    ClearFollowUp {
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
    },
    /// Schedule an email to be sent at a later time.
    ScheduleSend {
        /// Unix timestamp (seconds) at which the email is sent.
        send_at: i64,
        /// The email to send.
        request: SendRequest,
    },
    /// Cancel a pending send by local store id.
    CancelSend {
        /// Local store id of the pending send.
        id: i64,
    },
    /// Save a draft.
    SaveDraft(SendRequest),
    /// Delete a draft by local store id.
    DeleteDraft {
        /// Local store id of the draft.
        id: i64,
    },
    /// Save a named template.
    SaveTemplate {
        /// Template name.
        name: String,
        /// The template content.
        request: SendRequest,
    },
    /// Delete a template by local store id.
    DeleteTemplate {
        /// Local store id of the template.
        id: i64,
    },
    /// Send an email.
    SendEmail(SendRequest),
    /// Search messages in an account.
    Search {
        /// Account to search in.
        account: AccountId,
        /// Search query.
        query: String,
    },
    /// Shut down the service.
    Quit,
}

/// An attachment to include in an outgoing email.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    /// File name (used for the Content-Disposition header).
    pub filename: String,
    /// MIME content type (e.g. `application/pdf`).
    pub content_type: String,
    /// File content.
    pub data: Vec<u8>,
}

/// Request to send an email.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRequest {
    /// Account to send from.
    pub account: AccountId,
    /// From address.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
    /// CC recipients.
    pub cc: Vec<String>,
    /// BCC recipients.
    pub bcc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body_text: String,
    /// Optional HTML alternative to the plain-text body.
    #[serde(default)]
    pub body_html: Option<String>,
    /// Attachments.
    pub attachments: Vec<OutgoingAttachment>,
    /// Message-ID being replied to, if any.
    pub in_reply_to: Option<String>,
    /// References header value, if any.
    pub references: Option<String>,
}

/// An event from the mail core to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MailEvent {
    /// Account connection status changed.
    AccountStatus {
        /// Account that changed.
        account: AccountId,
        /// New status.
        status: AccountStatus,
    },
    /// Folder list for an account changed.
    FoldersUpdated {
        /// Account whose folders changed.
        account: AccountId,
        /// New folder list.
        folders: Vec<Folder>,
    },
    /// New messages appended to a folder.
    MessagesAppended {
        /// Account the folder belongs to.
        account: AccountId,
        /// Folder the messages were added to.
        folder_id: i64,
        /// New messages.
        messages: Vec<MessageSummary>,
    },
    /// Existing messages changed (flags, preview, …).
    MessagesUpdated {
        /// Account the folder belongs to.
        account: AccountId,
        /// Folder the messages are in.
        folder_id: i64,
        /// Local store ids of the changed messages.
        message_ids: Vec<i64>,
    },
    /// A full message was loaded.
    MessageLoaded {
        /// The loaded message.
        message: MessageFull,
    },
    /// A scheduled follow-up reminder is due.
    FollowUpDue {
        /// Account the message belongs to.
        account: AccountId,
        /// Local store id of the message.
        message_id: i64,
        /// Message subject (for the notification).
        subject: String,
    },
    /// Search results.
    SearchResults {
        /// Account that was searched.
        account: AccountId,
        /// Matching messages.
        messages: Vec<MessageSummary>,
    },
    /// Result of a send request.
    SendResult {
        /// Account that sent.
        account: AccountId,
        /// Whether the send succeeded.
        ok: bool,
        /// Error message if it failed.
        error: Option<String>,
    },
    /// An email was scheduled for later sending.
    SendScheduled {
        /// Account that will send.
        account: AccountId,
        /// Unix timestamp (seconds) at which the email is sent.
        send_at: i64,
    },
    /// The pending-sends list changed; the UI should reload it.
    SendsUpdated,
    /// The drafts list changed; the UI should reload it.
    DraftsUpdated,
    /// The templates list changed; the UI should reload it.
    TemplatesUpdated,
    /// A non-fatal error to display.
    Error {
        /// Account the error relates to, if any.
        account: Option<AccountId>,
        /// Human-readable error message.
        message: String,
    },
}
