//! Error types for the mail core.

use thiserror::Error;

/// Errors from the mail core.
#[derive(Debug, Error)]
pub enum Error {
    /// IMAP protocol or connection error.
    #[error("IMAP error: {0}")]
    Imap(#[from] imap::Error),

    /// SMTP protocol or connection error.
    #[error("SMTP error: {0}")]
    Smtp(#[from] lettre::error::Error),

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
