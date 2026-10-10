//! SMTP sending via lettre.

use lettre::address::Address;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message};

use crate::account::{AuthMethod, SmtpConfig, TlsMode};
use crate::error::{Error, Result};
use crate::events::OutgoingAttachment;

/// Send an email over SMTP.
///
/// # Arguments
/// * `config` — SMTP endpoint.
/// * `auth` — credentials (password or OAuth2 access token).
/// * `from` — From address.
/// * `to`, `cc`, `bcc` — recipient lists.
/// * `subject` — subject line.
/// * `body` — plain-text body.
/// * `attachments` — files to attach (multipart/mixed when non-empty).
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
    attachments: &[OutgoingAttachment],
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

    let mail = build_message(
        from, to, cc, bcc, subject, body, attachments, in_reply_to, references,
    )?;
    transport.send(mail).await?;
    Ok(())
}

/// Build the outgoing message: a plain body, or multipart/mixed when
/// attachments are present.
///
/// # Errors
/// Returns `Error::Config` on a bad From address or body build failure.
fn build_message(
    from: &str,
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    attachments: &[OutgoingAttachment],
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> Result<Message> {
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
    Ok(if attachments.is_empty() {
        mail.body(body.to_string())
            .map_err(|e| Error::Config(format!("build message: {e}")))?
    } else {
        // multipart/mixed: the text body plus one part per attachment.
        let mut part = MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()));
        for att in attachments {
            let content_type = ContentType::parse(&att.content_type)
                .unwrap_or_else(|_| ContentType::parse("application/octet-stream").unwrap());
            part = part.singlepart(
                Attachment::new(att.filename.clone()).body(att.data.clone(), content_type),
            );
        }
        mail.multipart(part)
            .map_err(|e| Error::Config(format!("build message: {e}")))?
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(filename: &str, content_type: &str) -> OutgoingAttachment {
        OutgoingAttachment {
            filename: filename.into(),
            content_type: content_type.into(),
            data: b"file content".to_vec(),
        }
    }

    #[test]
    fn build_message_without_attachments_is_plain() {
        let mail = build_message(
            "alice@example.com",
            &["bob@example.com".into()],
            &[],
            &[],
            "Hi",
            "Hello",
            &[],
            None,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(mail.formatted()).unwrap();
        // No Content-Type header: the RFC 5322 default is text/plain.
        assert!(!raw.contains("multipart"));
        assert!(raw.contains("Hello"));
    }

    #[test]
    fn build_message_with_attachments_is_multipart_mixed() {
        let mail = build_message(
            "alice@example.com",
            &["bob@example.com".into()],
            &[],
            &[],
            "Hi",
            "Hello",
            &[attachment("report.pdf", "application/pdf")],
            None,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(mail.formatted()).unwrap();
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("report.pdf"));
        assert!(raw.contains("application/pdf"));
        assert!(raw.contains("Hello"));
    }

    #[test]
    fn build_message_falls_back_to_octet_stream_on_bad_type() {
        let mail = build_message(
            "alice@example.com",
            &["bob@example.com".into()],
            &[],
            &[],
            "Hi",
            "Hello",
            &[attachment("blob.bin", "not a valid type")],
            None,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(mail.formatted()).unwrap();
        assert!(raw.contains("application/octet-stream"));
    }
}
