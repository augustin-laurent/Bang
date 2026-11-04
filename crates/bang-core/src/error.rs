//! Error types for the mail core.

use thiserror::Error;

/// Errors from the mail core.
#[derive(Debug, Error)]
pub enum Error {
    /// Access was revoked or no credentials exist for this session.
    #[error("Reconnect this account to continue.")]
    ReconnectRequired,
    /// The desktop keyring is locked, missing or unavailable.
    #[error("Secure storage is unavailable. Unlock your keyring or continue for this session.")]
    SecureStorage,
    /// A superseded operation must not publish credentials or account state.
    #[error("Account operation cancelled.")]
    Cancelled,
    /// User declined authorization.
    #[error("Sign-in was declined. You can try again.")]
    Declined,
    /// Bounded setup or authorization operation expired.
    #[error("Connection timed out. Check your network and try again.")]
    Timeout,
    /// IMAP protocol or connection error.
    #[error("IMAP error: {0}")]
    Imap(#[from] imap::Error),

    /// SMTP protocol or connection error.
    #[error("SMTP error: {0}")]
    Smtp(#[from] lettre::transport::smtp::Error),

    /// MIME parsing error.
    #[error("MIME parse error: {0}")]
    Mime(String),

    /// SQLite store error.
    #[error("store error: {0}")]
    Store(String),

    /// Authentication error (bad credentials, token expired, …).
    #[error("auth error: {0}")]
    Auth(String),

    /// Configuration error (missing client id, bad config file, …).
    #[error("config error: {0}")]
    Config(String),

    /// Network error that is not covered by a more specific variant.
    #[error("network error: {0}")]
    Network(String),
}

/// Result alias for the mail core.
pub type Result<T> = std::result::Result<T, Error>;
