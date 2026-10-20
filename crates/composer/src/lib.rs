//! JSON-lines protocol between Bang's Slint process and the local rich composer.
//! Account credentials and attachment contents never cross this boundary.
#![warn(missing_docs)]
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
/// A sender choice, containing only the public account identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sender {
    /// Stable identity assigned by the parent process.
    pub id: String,
    /// Public email address.
    pub email: String,
}
/// A staged attachment's public metadata. The parent owns the file contents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    /// Stable identity assigned by the parent process.
    pub id: u64,
    /// Display filename, without a local path.
    pub name: String,
    /// File size in bytes.
    pub size: u64,
}
/// Messages delivered to the isolated composer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// Initialize the editor after its ready event.
    Init {
        /// Available sender identities.
        accounts: Vec<Sender>,
        /// Resolved UI language: fr or en.
        language: String,
        /// Whether the dark appearance is active.
        dark: bool,
        /// Shared French catalogue keyed by English source strings.
        translations: BTreeMap<String, String>,
    },
    /// Update appearance and language without replacing content.
    Preferences {
        /// Resolved UI language: fr or en.
        language: String,
        /// Whether the dark appearance is active.
        dark: bool,
    },
    /// Prefill the draft fields (reply / forward).
    Prefill {
        /// Recipients to prefill in the To field.
        to: Vec<String>,
        /// Subject to prefill.
        subject: String,
        /// Body to prefill (quoted original), as HTML.
        body_html: String,
        /// Message-ID being replied to, if any.
        in_reply_to: Option<String>,
        /// References header value, if any.
        references: Option<String>,
    },
    /// Replace staged attachment metadata.
    Attachments {
        /// Staged attachment metadata.
        files: Vec<Attachment>,
    },
    /// Return the result of the in-flight send.
    SendResult {
        /// Whether SMTP accepted the message.
        ok: bool,
        /// Failure detail, when present.
        error: Option<String>,
    },
    /// Report a recoverable error.
    Error {
        /// User-facing error detail.
        message: String,
    },
    /// Raise the existing composer window.
    Focus,
    /// Ask the user before abandoning unsent edits.
    RequestClose,
}
/// Messages emitted by the editor; names and paths are never evaluated as code.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditorMessage {
    /// The local editor has loaded and can receive initialization.
    Ready,
    /// Open the native file picker.
    Attach,
    /// Return file selections to the parent.
    AttachPaths {
        /// Files selected by the native file picker.
        paths: Vec<String>,
    },
    /// Remove one staged file by identity.
    RemoveAttachment {
        /// Stable identity assigned by the parent process.
        id: u64,
    },
    /// Request sending through the parent mail engine.
    Send {
        /// Selected account identity; the parent resolves its credentials.
        account: String,
        /// To recipients as individual addresses.
        to: Vec<String>,
        /// Cc recipients as individual addresses.
        cc: Vec<String>,
        /// Bcc recipients as individual addresses.
        bcc: Vec<String>,
        /// Message subject.
        subject: String,
        /// Plain-text alternative exported from the editor document.
        body_text: String,
        /// HTML alternative exported from the same document.
        body_html: String,
    },
    /// Report the document edit state.
    Dirty {
        /// Whether unsent edits exist.
        dirty: bool,
    },
    /// Close the composer after confirmation or a successful send.
    Close,
    /// The user kept an unsent draft open.
    CloseCancelled,
}
#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip_unicode_and_untrusted_strings() {
        let msg = super::HostMessage::Error {
            message: "é\n</script> ` ${test}".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains('\n'));
        assert!(matches!(
            serde_json::from_str::<super::HostMessage>(&json).unwrap(),
            super::HostMessage::Error { .. }
        ));
    }
}
