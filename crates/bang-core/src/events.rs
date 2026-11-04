//! Commands and events crossing the core/UI boundary.

use crate::account::{AccountConfig, AccountId};
use crate::message::{AccountStatus, Folder, MessageFlags, MessageFull, MessageSummary};
use serde::{Deserialize, Serialize};

/// A command from the UI to the mail core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UiCommand {
    /// Restore metadata and resolve credentials inside the core.
    RestoreAccounts,
    /// Retry secure-storage access without opening a browser or discarding tokens.
    UnlockAccount(AccountId),
    /// Discover settings without sending mailbox credentials.
    DiscoverSettings {
        /// Mailbox address used only for local domain/username processing.
        email: String,
        /// Correlate discovery with the visible wizard.
        attempt: i32,
    },
    /// Retry saving validated credentials, or keep them only in memory.
    RetryAccountStorage {
        /// Attempt which owns the retained validated credentials.
        attempt: i32,
        /// Explicitly avoid persistent secret storage.
        session_only: bool,
    },
    /// Add an account and start syncing it.
    AddAccount(AccountConfig),
    /// Validate a new account before saving it; correlated with the open wizard.
    ConnectAccount {
        /// Wizard attempt number.
        attempt: i32,
        /// Account settings to validate.
        config: AccountConfig,
    },
    /// Cancel the active account setup, including browser sign-in.
    CancelAccountSetup,
    /// Run an interactive OAuth2 login and add the resulting account.
    OAuthLogin {
        /// Existing account being reconnected; preserve its local cache.
        replace: Option<AccountId>,
        /// Provider to authenticate against.
        provider: crate::account::OAuthProvider,
        /// Wizard attempt number.
        attempt: i32,
        /// Mailbox address, required for Microsoft and used as a login hint.
        email: String,
        /// Optional desktop client secret required by some Google registrations.
        client_secret: Option<String>,
        /// Resolved public client ID, usually from the bundled registration.
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
    /// Set a category correction. None restores automatic classification at this scope.
    SetCategory {
        /// Target message.
        message_id: i64,
        /// Correction or automatic.
        category: Option<crate::organization::MailCategory>,
        /// Apply a sender rule across accounts instead of a message override.
        sender: bool,
    },
    /// Create or edit a local label.
    SaveLabel {
        /// None creates a label.
        id: Option<i64>,
        /// Display name.
        name: String,
        /// RGB color.
        color: String,
    },
    /// Remove a label without removing mail.
    DeleteLabel {
        /// Label id.
        id: i64,
    },
    /// Assign or unassign a local label.
    SetMessageLabel {
        /// Message id.
        message_id: i64,
        /// Label id.
        label_id: i64,
        /// Desired assignment state.
        assigned: bool,
    },
    /// Request an avatar for a normalized sender address.
    RequestAvatar(String),
    /// Enable or disable online avatar requests.
    SetOnlineAvatars(bool),
    /// Shut down the service.
    Quit,
}

/// Progress of the currently visible account setup attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SetupStage {
    /// The system browser is being launched.
    OpeningBrowser,
    /// Waiting for provider authorization.
    WaitingForAuthorization,
    /// Authenticating both mail transports.
    CheckingAccount,
    /// Resolving public provider settings.
    FindingSettings,
    /// Saving credentials to the system keyring.
    SavingCredentials,
}

/// Actionable setup errors; UI translations never inspect raw protocol text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SetupFailure {
    /// Authorization was declined.
    Declined,
    /// Setup exceeded its deadline.
    Timeout,
    /// The keyring needs unlocking or a session-only choice.
    SecureStorage,
    /// Credentials or provider access policy rejected mail authentication.
    Authentication,
    /// Temporary network failure.
    Network,
    /// The publisher registration is unavailable or the inputs are invalid.
    Configuration,
}

impl SetupFailure {
    /// Classify a core error without parsing provider error strings.
    pub fn from_error(error: &crate::Error) -> Self {
        match error {
            crate::Error::Declined => Self::Declined,
            crate::Error::Timeout => Self::Timeout,
            crate::Error::SecureStorage => Self::SecureStorage,
            crate::Error::Auth(_) | crate::Error::ReconnectRequired => Self::Authentication,
            crate::Error::Imap(imap::Error::No(_)) | crate::Error::Imap(imap::Error::Bad(_)) => {
                Self::Authentication
            }
            crate::Error::Smtp(e) if e.is_permanent() => Self::Authentication,
            crate::Error::Config(_) => Self::Configuration,
            _ => Self::Network,
        }
    }
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
    /// Stage of the current setup or settings-discovery attempt.
    AccountSetupProgress {
        /// Wizard attempt number.
        attempt: i32,
        /// Current stage.
        stage: SetupStage,
    },
    /// Settings discovery finished without authenticating.
    AccountSettingsDiscovered {
        /// Wizard attempt number.
        attempt: i32,
        /// Safe editable settings or fallback outcome.
        result: crate::discovery::DiscoveryResult,
    },
    /// Local account removal completed or needs secure-storage recovery.
    AccountRemoved {
        /// Removed account.
        account: AccountId,
        /// Typed failure, if removal could not complete.
        error: Option<SetupFailure>,
    },
    /// Sign-in completed; incoming/outgoing server validation is starting.
    AccountSetupChecking {
        /// Wizard attempt number.
        attempt: i32,
    },
    /// Result of a single wizard attempt, independent from background account sync.
    AccountSetupFinished {
        /// Wizard attempt number.
        attempt: i32,
        /// Failure message; absent after the validated account has been saved.
        error: Option<SetupFailure>,
    },
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
    /// A local organization operation failed without changing the reading state.
    OrganizationError {
        /// A user-facing English translation key.
        message: String,
    },
    /// Local categories or labels changed; reload affected views.
    OrganizationChanged,
    /// A validated local avatar is available.
    AvatarReady {
        /// Normalized sender identity, never a message selection id.
        address: String,
        /// Validated cached PNG path.
        path: String,
    },
    /// A non-fatal error to display.
    Error {
        /// Account the error relates to, if any.
        account: Option<AccountId>,
        /// Human-readable error message.
        message: String,
    },
}
