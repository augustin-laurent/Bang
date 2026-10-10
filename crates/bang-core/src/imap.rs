//! IMAP client wrapper.
//!
//! Wraps the synchronous `imap` crate. Each account's syncer runs on its own
//! OS thread, where blocking calls (including IDLE) are fine.
//!
//! [`ImapSession`] is generic over the stream type: TLS sessions use
//! `native_tls::TlsStream<TcpStream>`, plaintext sessions use `TcpStream`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use imap::extensions::idle::{SetReadTimeout, WaitOutcome};
use imap::types::{Flag, Mailbox, NameAttribute};
use imap::{Authenticator, Client, Session};
use native_tls::TlsConnector;

use crate::account::{AuthMethod, ImapConfig, TlsMode};
use crate::error::{Error, Result};
use crate::message::{EmailAddress, MessageFlags};

/// A connected and authenticated IMAP session.
///
/// `T` is the underlying stream (`native_tls::TlsStream<TcpStream>` for TLS,
/// `TcpStream` for plaintext).
pub struct ImapSession<T: Read + Write> {
    session: Session<T>,
}

/// SASL XOAUTH2 authenticator (Gmail, Microsoft, …).
struct XOAuth2 {
    user: String,
    access_token: String,
}

impl Authenticator for XOAuth2 {
    type Response = String;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.access_token
        )
    }
}

/// Status of a selected mailbox.
#[derive(Debug, Clone, Copy, Default)]
pub struct MailboxStatus {
    /// Total messages in the mailbox.
    pub exists: u32,
    /// Next UID (one past the highest UID), if the server reports it.
    pub uid_next: Option<u32>,
    /// UIDVALIDITY, if the server reports it.
    pub uid_validity: Option<u32>,
}

/// Metadata for one message, as fetched without the body.
#[derive(Debug, Clone)]
pub struct MetaMessage {
    /// IMAP UID.
    pub uid: u32,
    /// IMAP flags.
    pub flags: MessageFlags,
    /// INTERNALDATE as Unix timestamp (seconds), 0 if absent.
    pub date: i64,
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
    /// RFC822.SIZE in bytes, 0 if absent.
    pub size: u32,
}

impl<T: Read + Write> ImapSession<T> {
    /// Authenticate an already-connected client.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    fn authenticate(client: Client<T>, auth: &AuthMethod, user: &str) -> Result<Self> {
        let session = match auth {
            AuthMethod::Password { username, password } => client
                .login(username, password)
                .map_err(|(e, _)| e)
                .map_err(Error::from),
            AuthMethod::OAuth2 { access_token, .. } => {
                let xoauth2 = XOAuth2 {
                    user: user.to_string(),
                    access_token: access_token.clone(),
                };
                client
                    .authenticate("XOAUTH2", &xoauth2)
                    .map_err(|(e, _)| e)
                    .map_err(Error::from)
            }
        }?;
        Ok(Self { session })
    }

    /// List all folder paths (selectable mailboxes only).
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn list_folders(&mut self) -> Result<Vec<String>> {
        let names = self.session.list(None, None)?;
        Ok(names
            .iter()
            .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
            .map(|n| n.name().to_string())
            .collect())
    }

    /// Select a folder and return its status.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn select(&mut self, path: &str) -> Result<MailboxStatus> {
        let mb: Mailbox = self.session.select(path)?;
        Ok(MailboxStatus {
            exists: mb.exists,
            uid_next: mb.uid_next,
            uid_validity: mb.uid_validity,
        })
    }

    /// Fetch metadata (UID, flags, date, envelope, size) for a UID set.
    ///
    /// `uid_set` is an IMAP sequence set, e.g. `"123:*"` or `"1:500"`.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn fetch_meta(&mut self, uid_set: &str) -> Result<Vec<MetaMessage>> {
        let fetches = self
            .session
            .uid_fetch(uid_set, "UID FLAGS INTERNALDATE ENVELOPE RFC822.SIZE")?;
        Ok(fetches.iter().filter_map(convert_fetch).collect())
    }

    /// Fetch the first `max_bytes` of the text part (for previews).
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn fetch_preview(&mut self, uid: u32, max_bytes: usize) -> Result<Option<String>> {
        let query = format!("UID BODY.PEEK[TEXT]<0.{max_bytes}>");
        let fetches = self.session.uid_fetch(&uid.to_string(), &query)?;
        Ok(fetches
            .iter()
            .find_map(|f| f.body().map(|b| String::from_utf8_lossy(b).into_owned())))
    }

    /// Fetch the full raw message for a UID.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn fetch_body(&mut self, uid: u32) -> Result<Vec<u8>> {
        let fetches = self.session.uid_fetch(&uid.to_string(), "UID BODY.PEEK[]")?;
        fetches
            .iter()
            .find_map(|f| f.body().map(|b| b.to_vec()))
            .ok_or_else(|| {
                Error::Imap(imap::Error::Parse(
                    imap::error::ParseError::Unexpected("no body in FETCH response".into()),
                ))
            })
    }

    /// Add or remove flags on a UID set.
    ///
    /// `flags` is a space-separated flag list, e.g. `"\Seen \Flagged"`.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn set_flags(&mut self, uid_set: &str, flags: &str, add: bool) -> Result<()> {
        let verb = if add { "+FLAGS" } else { "-FLAGS" };
        self.session
            .uid_store(uid_set, &format!("{verb} ({flags})"))?;
        Ok(())
    }

    /// Permanently remove a UID set from the selected mailbox.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn expunge(&mut self, uid_set: &str) -> Result<()> {
        self.session.uid_expunge(uid_set)?;
        Ok(())
    }

    /// Wait for mailbox changes (IMAP IDLE), up to `timeout`.
    ///
    /// Returns `true` if the mailbox changed, `false` on timeout.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn idle_wait(&mut self, timeout: Duration) -> Result<bool>
    where
        T: SetReadTimeout,
    {
        let handle = self.session.idle()?;
        match handle.wait_with_timeout(timeout) {
            Ok(WaitOutcome::TimedOut) => Ok(false),
            Ok(WaitOutcome::MailboxChanged) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }

    /// Close the selected mailbox (without expunging).
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn close(&mut self) -> Result<()> {
        self.session.close()?;
        Ok(())
    }

    /// Log out (implies CLOSE).
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn logout(&mut self) -> Result<()> {
        self.session.logout()?;
        Ok(())
    }
}

/// A connected IMAP session, TLS or plaintext.
pub enum ImapConn {
    /// TLS session (implicit TLS or after STARTTLS).
    Tls(ImapSession<native_tls::TlsStream<TcpStream>>),
    /// Plaintext session.
    Plain(ImapSession<TcpStream>),
}

impl ImapConn {
    /// List all folder paths (selectable mailboxes only).
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn list_folders(&mut self) -> Result<Vec<String>> {
        match self {
            ImapConn::Tls(s) => s.list_folders(),
            ImapConn::Plain(s) => s.list_folders(),
        }
    }

    /// Select a folder and return its status.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn select(&mut self, path: &str) -> Result<MailboxStatus> {
        match self {
            ImapConn::Tls(s) => s.select(path),
            ImapConn::Plain(s) => s.select(path),
        }
    }

    /// Fetch metadata for a UID set.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn fetch_meta(&mut self, uid_set: &str) -> Result<Vec<MetaMessage>> {
        match self {
            ImapConn::Tls(s) => s.fetch_meta(uid_set),
            ImapConn::Plain(s) => s.fetch_meta(uid_set),
        }
    }

    /// Fetch the full raw message for a UID.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn fetch_body(&mut self, uid: u32) -> Result<Vec<u8>> {
        match self {
            ImapConn::Tls(s) => s.fetch_body(uid),
            ImapConn::Plain(s) => s.fetch_body(uid),
        }
    }

    /// Add or remove flags on a UID set.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn set_flags(&mut self, uid_set: &str, flags: &str, add: bool) -> Result<()> {
        match self {
            ImapConn::Tls(s) => s.set_flags(uid_set, flags, add),
            ImapConn::Plain(s) => s.set_flags(uid_set, flags, add),
        }
    }

    /// Permanently remove a UID set from the selected mailbox.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn expunge(&mut self, uid_set: &str) -> Result<()> {
        match self {
            ImapConn::Tls(s) => s.expunge(uid_set),
            ImapConn::Plain(s) => s.expunge(uid_set),
        }
    }

    /// Wait for mailbox changes (IMAP IDLE), up to `timeout`.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn idle_wait(&mut self, timeout: Duration) -> Result<bool> {
        match self {
            ImapConn::Tls(s) => s.idle_wait(timeout),
            ImapConn::Plain(s) => s.idle_wait(timeout),
        }
    }

    /// Close the selected mailbox.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn close(&mut self) -> Result<()> {
        match self {
            ImapConn::Tls(s) => s.close(),
            ImapConn::Plain(s) => s.close(),
        }
    }

    /// Log out.
    ///
    /// # Errors
    /// Returns `Error::Imap` on failure.
    pub fn logout(&mut self) -> Result<()> {
        match self {
            ImapConn::Tls(s) => s.logout(),
            ImapConn::Plain(s) => s.logout(),
        }
    }

    /// Connect and authenticate to an IMAP server.
    ///
    /// # Errors
    /// Returns `Error::Imap` or `Error::Network` on failure.
    pub fn connect(config: &ImapConfig, auth: &AuthMethod, user: &str) -> Result<Self> {
        let addr = (config.host.as_str(), config.port);
        let domain = config.host.as_str();
        match config.tls {
            TlsMode::Tls => {
                let connector = TlsConnector::builder()
                    .build()
                    .map_err(|e| Error::Network(e.to_string()))?;
                let client = imap::connect(addr, domain, &connector)?;
                ImapSession::authenticate(client, auth, user).map(Self::Tls)
            }
            TlsMode::Starttls => {
                let connector = TlsConnector::builder()
                    .build()
                    .map_err(|e| Error::Network(e.to_string()))?;
                let client = imap::connect_starttls(addr, domain, &connector)?;
                ImapSession::authenticate(client, auth, user).map(Self::Tls)
            }
            TlsMode::Plaintext => {
                let tcp =
                    TcpStream::connect(addr).map_err(|e| Error::Network(e.to_string()))?;
                let mut client = Client::new(tcp);
                client.read_greeting()?;
                ImapSession::authenticate(client, auth, user).map(Self::Plain)
            }
        }
    }
}

/// Convert an `imap` Fetch into a `MetaMessage` (None if no UID).
fn convert_fetch(f: &imap::types::Fetch) -> Option<MetaMessage> {
    let uid = f.uid?;
    let mut flags = MessageFlags::empty();
    for flag in f.flags() {
        flags |= flag_to_flags(flag);
    }
    let date = f.internal_date().map(|d| d.timestamp()).unwrap_or(0);
    let mut message_id = String::new();
    let mut from = EmailAddress {
        name: String::new(),
        address: String::new(),
    };
    let mut to = Vec::new();
    let mut cc = Vec::new();
    let mut subject = String::new();
    if let Some(env) = f.envelope() {
        if let Some(mid) = env.message_id {
            message_id = String::from_utf8_lossy(mid).into_owned();
        }
        if let Some(subj) = env.subject {
            subject = String::from_utf8_lossy(subj).into_owned();
        }
        if let Some(froms) = &env.from {
            if let Some(a) = froms.first() {
                from = convert_address(a);
            }
        }
        if let Some(tos) = &env.to {
            to = tos.iter().map(convert_address).collect();
        }
        if let Some(ccs) = &env.cc {
            cc = ccs.iter().map(convert_address).collect();
        }
    }
    Some(MetaMessage {
        uid,
        flags,
        date,
        message_id,
        from,
        to,
        cc,
        subject,
        size: f.size.unwrap_or(0),
    })
}

/// Convert an IMAP flag to a `MessageFlags` bit.
fn flag_to_flags(flag: &Flag<'_>) -> MessageFlags {
    match flag {
        Flag::Seen => MessageFlags::SEEN,
        Flag::Flagged => MessageFlags::FLAGGED,
        Flag::Answered => MessageFlags::ANSWERED,
        Flag::Deleted => MessageFlags::DELETED,
        Flag::Draft => MessageFlags::DRAFT,
        Flag::Recent | Flag::MayCreate | Flag::Custom(_) => MessageFlags::empty(),
    }
}

/// Convert an IMAP address to an `EmailAddress`.
fn convert_address(a: &imap_proto::types::Address<'_>) -> EmailAddress {
    let name = a
        .name
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .unwrap_or_default();
    let address = match (&a.mailbox, &a.host) {
        (Some(m), Some(h)) => format!(
            "{}@{}",
            String::from_utf8_lossy(m),
            String::from_utf8_lossy(h)
        ),
        (Some(m), None) => String::from_utf8_lossy(m).into_owned(),
        _ => String::new(),
    };
    EmailAddress { name, address }
}
