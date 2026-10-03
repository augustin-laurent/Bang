//! Bang mail engine.
//!
//! Provides IMAP/SMTP clients, OAuth2, a per-account sync engine, and a
//! SQLite-backed store. This crate has no UI dependencies.

#![warn(missing_docs)]

pub mod account;
pub mod error;
pub mod events;
pub mod imap;
pub mod message;
pub mod oauth;
pub mod smtp;
pub mod store;

pub use imap::{ImapConn, ImapSession, MailboxStatus, MetaMessage};
pub use store::Store;

pub use account::{
    AccountConfig, AccountId, AuthMethod, ImapConfig, OAuthProvider, SmtpConfig, TlsMode,
};
pub use error::{Error, Result};
pub use events::{MailEvent, SendRequest, UiCommand};
pub use message::{
    AccountStatus, Attachment, EmailAddress, Folder, MessageFlags, MessageFull, MessageSummary,
};
