//! SMTP sending via lettre.

use lettre::address::Address;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message};

use crate::account::{AuthMethod, SmtpConfig, TlsMode};
use crate::error::{Error, Result};

/// Send an email over SMTP.
///
/// # Arguments
/// * `config` — SMTP endpoint.
/// * `auth` — credentials (password or OAuth2 access token).
/// * `from` — From address.
/// * `to`, `cc`, `bcc` — recipient lists.
/// * `subject` — subject line.
/// * `body` — plain-text body.
/// * `in_reply_to` — Message-ID being replied to, if any.
/// * `references` — References header value, if any.
///
/// # Errors
/// Returns `Error::Smtp` on failure.
pub async fn send(
    config: &SmtpConfig,
    auth: &AuthMethod,
    from: &str,
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> Result<()> {
    let mut builder = AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous(&config.host)
        .port(config.port)
        .tls(tls_mode(config)?);

    match auth {
        AuthMethod::Password { username, password } => {
            builder = builder
                .credentials(Credentials::new(username.clone(), password.clone()))
                .authentication(vec![Mechanism::Plain, Mechanism::Login]);
        }
        AuthMethod::OAuth2 { access_token, .. } => {
            // XOAUTH2: identity is the account email, secret is the access token.
            let identity = match from.contains('@') {
                true => from.to_string(),
                false => String::new(),
            };
            builder = builder
                .credentials(Credentials::new(identity, access_token.clone()))
                .authentication(vec![Mechanism::Xoauth2]);
        }
    }
    let transport: AsyncSmtpTransport<lettre::Tokio1Executor> = builder.build();

    let from_addr: Address = from
        .parse()
        .map_err(|e| Error::Config(format!("bad From address: {e}")))?;
    let mut mail = Message::builder()
        .from(Mailbox::new(Some(from.to_string()), from_addr))
        .subject(subject);
    for addr in to {
        if let Ok(a) = addr.parse::<Address>() {
            mail = mail.to(Mailbox::new(Some(addr.to_string()), a));
        }
    }
    for addr in cc {
        if let Ok(a) = addr.parse::<Address>() {
            mail = mail.cc(Mailbox::new(Some(addr.to_string()), a));
        }
    }
    for addr in bcc {
        if let Ok(a) = addr.parse::<Address>() {
            mail = mail.bcc(Mailbox::new(Some(addr.to_string()), a));
        }
    }
    if let Some(id) = in_reply_to {
        mail = mail.in_reply_to(id.to_string());
    }
    if let Some(refs) = references {
        mail = mail.references(refs.to_string());
    }
    let mail = mail
        .body(body.to_string())
        .map_err(|e| Error::Config(format!("build message: {e}")))?;

    transport.send(mail).await?;
    Ok(())
}

/// Map a `TlsMode` to a lettre `Tls` value.
fn tls_mode(config: &SmtpConfig) -> Result<Tls> {
    let domain = config.host.clone();
    Ok(match config.tls {
        TlsMode::Tls => Tls::Wrapper(
            TlsParameters::builder(domain)
                .build_rustls()
                .map_err(|e| Error::Config(format!("TLS params: {e}")))?,
        ),
        TlsMode::Starttls => Tls::Required(
            TlsParameters::builder(domain)
                .build_rustls()
                .map_err(|e| Error::Config(format!("TLS params: {e}")))?,
        ),
        TlsMode::Plaintext => Tls::None,
    })
}
