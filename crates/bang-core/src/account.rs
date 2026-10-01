//! Account configuration and authentication.

use serde::{Deserialize, Serialize};

/// Unique identifier for a mail account.
///
/// Wraps a string so it can be used as a map key and serialized.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId(pub String);

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// TLS mode for an IMAP or SMTP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// Connect in plaintext, then upgrade with STARTTLS.
    Starttls,
    /// Connect directly over TLS (implicit TLS).
    #[default]
    Tls,
    /// No encryption. Only for trusted local servers.
    Plaintext,
}

/// IMAP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImapConfig {
    /// Server hostname.
    pub host: String,
    /// Server port (993 for TLS, 143 for STARTTLS).
    pub port: u16,
    /// TLS mode.
    pub tls: TlsMode,
}

/// SMTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    /// Server hostname.
    pub host: String,
    /// Server port (465 for TLS, 587 for STARTTLS).
    pub port: u16,
    /// TLS mode.
    pub tls: TlsMode,
}

/// OAuth2 provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    /// Google (Gmail).
    Google,
    /// Microsoft (Outlook).
    Microsoft,
}

/// Authentication method for an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthMethod {
    /// Username and password (generic IMAP/SMTP).
    Password {
        /// IMAP/SMTP username (usually the email address).
        username: String,
        /// IMAP/SMTP password or app password.
        password: String,
    },
    /// OAuth2 (Gmail or Microsoft).
    OAuth2 {
        /// Which provider issued the tokens.
        provider: OAuthProvider,
        /// Current access token (used as XOAUTH2 credential).
        access_token: String,
        /// Refresh token, if the provider issued one.
        refresh_token: Option<String>,
        /// Unix timestamp (seconds) when the access token expires, if known.
        expires_at: Option<i64>,
    },
}

/// Full configuration for a mail account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// Stable unique identifier.
    pub id: AccountId,
    /// Account email address (also the default From address).
    pub email: String,
    /// IMAP endpoint.
    pub imap: ImapConfig,
    /// SMTP endpoint.
    pub smtp: SmtpConfig,
    /// How to authenticate.
    pub auth: AuthMethod,
}
