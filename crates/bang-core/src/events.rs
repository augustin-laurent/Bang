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
    /// A non-fatal error to display.
    Error {
        /// Account the error relates to, if any.
        account: Option<AccountId>,
        /// Human-readable error message.
        message: String,
    },
}
