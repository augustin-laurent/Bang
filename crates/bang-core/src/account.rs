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
#[derive(Clone, Serialize, Deserialize)]
pub enum AuthMethod {
    /// Username and password (generic IMAP/SMTP).
    Password {
        /// IMAP/SMTP username (usually the email address).
        username: String,
        /// Outgoing username; empty preserves the incoming username.
        #[serde(default)]
        smtp_username: String,
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

impl std::fmt::Debug for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthMethod([redacted])")
    }
}

/// Public native-client registration which issued a user's OAuth grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthRegistration {
    /// Public client identifier.
    pub client_id: String,
    /// Google's optional installed-app parameter, never a web-client secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_parameter: Option<String>,
}

/// Authentication metadata safe to give to UI and persist in the mail cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StoredAuth {
    /// Password authentication; the password lives in secure storage or memory.
    Password {
        /// Incoming username.
        username: String,
        /// Outgoing username.
        #[serde(default)]
        smtp_username: String,
        /// Opaque secure-storage item, absent for session-only accounts.
        #[serde(default)]
        credential_ref: Option<String>,
    },
    /// OAuth authentication; tokens live in secure storage or memory.
    OAuth2 {
        /// Provider which issued the grant.
        provider: OAuthProvider,
        /// Issuing registration, absent for accounts predating grant binding.
        #[serde(default)]
        registration: Option<OAuthRegistration>,
        /// Opaque secure-storage item, absent for session-only accounts.
        #[serde(default)]
        credential_ref: Option<String>,
    },
}

impl StoredAuth {
    /// Opaque secure-storage reference, if saved between sessions.
    pub fn credential_ref(&self) -> Option<&str> {
        match self {
            Self::Password { credential_ref, .. } | Self::OAuth2 { credential_ref, .. } => {
                credential_ref.as_deref()
            }
        }
    }
}

/// Persisted account metadata. It cannot contain mailbox tokens or passwords.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAccount {
    /// Prior secure-storage items retained for idempotent cleanup after recovery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_credential_refs: Vec<String>,
    /// Stable account identifier.
    pub id: AccountId,
    /// Mailbox address.
    pub email: String,
    /// Incoming endpoint.
    pub imap: ImapConfig,
    /// Outgoing endpoint.
    pub smtp: SmtpConfig,
    /// Secret-free authentication metadata.
    pub auth: StoredAuth,
}

impl StoredAccount {
    /// Connection template containing no secrets; workers resolve them on demand.
    pub(crate) fn connection_template(&self) -> AccountConfig {
        let auth = match &self.auth {
            StoredAuth::Password {
                username,
                smtp_username,
                ..
            } => AuthMethod::Password {
                username: username.clone(),
                smtp_username: smtp_username.clone(),
                password: String::new(),
            },
            StoredAuth::OAuth2 { provider, .. } => AuthMethod::OAuth2 {
                provider: *provider,
                access_token: String::new(),
                refresh_token: None,
                expires_at: None,
            },
        };
        self.with_auth(auth)
    }
    /// Reconstitute a runtime configuration with separately resolved secrets.
    pub fn with_auth(&self, auth: AuthMethod) -> AccountConfig {
        AccountConfig {
            id: self.id.clone(),
            email: self.email.clone(),
            imap: self.imap.clone(),
            smtp: self.smtp.clone(),
            auth,
        }
    }
}

impl AccountConfig {
    /// Extract metadata, without retaining or serializing secrets.
    pub fn metadata(
        &self,
        registration: Option<OAuthRegistration>,
        credential_ref: Option<String>,
    ) -> StoredAccount {
        let auth = match &self.auth {
            AuthMethod::Password {
                username,
                smtp_username,
                ..
            } => StoredAuth::Password {
                username: username.clone(),
                smtp_username: smtp_username.clone(),
                credential_ref,
            },
            AuthMethod::OAuth2 { provider, .. } => StoredAuth::OAuth2 {
                provider: *provider,
                registration,
                credential_ref,
            },
        };
        StoredAccount {
            retired_credential_refs: Vec::new(),
            id: self.id.clone(),
            email: self.email.clone(),
            imap: self.imap.clone(),
            smtp: self.smtp.clone(),
            auth,
        }
    }
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
