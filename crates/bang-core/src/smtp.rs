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

/// Check TLS and SMTP authentication without sending a message.
///
/// # Errors
/// Returns a configuration, network or authentication error.
pub async fn test_connection(config: &SmtpConfig, auth: &AuthMethod, email: &str) -> Result<()> {
    let (username, secret, mechanisms) = match auth {
        AuthMethod::Password { username, password } => (
            username.clone(),
            password.clone(),
            vec![Mechanism::Plain, Mechanism::Login],
        ),
        AuthMethod::OAuth2 { access_token, .. } => (
            email.to_owned(),
            access_token.clone(),
            vec![Mechanism::Xoauth2],
        ),
    };
    let transport: AsyncSmtpTransport<lettre::Tokio1Executor> =
        AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous(&config.host)
            .port(config.port)
            .tls(tls_mode(config)?)
            .timeout(Some(std::time::Duration::from_secs(15)))
            .credentials(Credentials::new(username, secret))
            .authentication(mechanisms)
            .build();
    if !transport.test_connection().await? {
        return Err(Error::Network(
            "SMTP server did not accept the connection".into(),
        ));
    }
    Ok(())
}

/// Send an email over SMTP.
///
/// # Arguments
/// * `config` — SMTP endpoint.
/// * `auth` — credentials (password or OAuth2 access token).
/// * `from` — From address.
/// * `to`, `cc`, `bcc` — recipient lists.
/// * `subject` — subject line.
/// * `body` — plain-text body.
/// * `body_html` — optional HTML alternative.
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
    body_html: Option<&str>,
    attachments: &[OutgoingAttachment],
    in_reply_to: Option<&str>,
    references: Option<&str>,
) -> Result<()> {
    let mut builder = AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous(&config.host)
        .port(config.port)
        // A blackholed connection must not hang the send forever; the
        // failure is retried on the next dispatch tick.
        .timeout(Some(std::time::Duration::from_secs(30)))
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
        from,
        to,
        cc,
        bcc,
        subject,
        body,
        body_html,
        attachments,
        in_reply_to,
        references,
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
    body_html: Option<&str>,
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
    let parse = |addr: &str| {
        addr.parse::<Mailbox>()
            .map_err(|e| Error::Config(format!("bad recipient address: {e}")))
    };
    for addr in to {
        mail = mail.to(parse(addr)?);
    }
    for addr in cc {
        mail = mail.cc(parse(addr)?);
    }
    for addr in bcc {
        mail = mail.bcc(parse(addr)?);
    }
    if let Some(id) = in_reply_to {
        mail = mail.in_reply_to(id.to_string());
    }
    if let Some(refs) = references {
        mail = mail.references(refs.to_string());
    }
    let content = match body_html {
        Some(html) => MultiPart::alternative()
            .singlepart(SinglePart::plain(body.to_string()))
            .singlepart(SinglePart::html(html.to_string())),
        None => MultiPart::mixed().singlepart(SinglePart::plain(body.to_string())),
    };
    if attachments.is_empty() && body_html.is_none() {
        return mail
            .singlepart(SinglePart::plain(body.to_string()))
            .map_err(|e| Error::Config(format!("build message: {e}")));
    }
    let content = if attachments.is_empty() {
        content
    } else {
        let mut mixed = if body_html.is_some() {
            MultiPart::mixed().multipart(content)
        } else {
            MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()))
        };
        for att in attachments {
            let content_type = ContentType::parse(&att.content_type)
                .unwrap_or(ContentType::parse("application/octet-stream").unwrap());
            mixed = mixed.singlepart(
                Attachment::new(att.filename.clone()).body(att.data.clone(), content_type),
            );
        }
        mixed
    };
    mail.multipart(content)
        .map_err(|e| Error::Config(format!("build message: {e}")))
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
            None,
            &[],
            None,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(mail.formatted()).unwrap();
        // Explicit plain-text content is preserved.
        assert!(!raw.contains("multipart"));
        assert!(raw.contains("text/plain"));
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
            None,
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
            None,
            &[attachment("blob.bin", "not a valid type")],
            None,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(mail.formatted()).unwrap();
        assert!(raw.contains("application/octet-stream"));
    }
    #[test]
    fn rich_body_and_attachments_roundtrip() {
        let mail = build_message(
            "alice@example.com",
            &["bob@example.com".into()],
            &[],
            &["private@example.com".into()],
            "Bonjour",
            "Été en gras",
            Some("<p>Été en <strong>gras</strong></p>"),
            &[attachment("maquette.pdf", "application/pdf")],
            None,
            None,
        )
        .unwrap();
        let raw = mail.formatted();
        let wire = String::from_utf8(raw.clone()).unwrap();
        assert!(wire.contains("multipart/mixed"));
        assert!(wire.contains("multipart/alternative"));
        assert!(!wire.contains("Bcc:"));
        let parsed = crate::mime::parse(&raw).unwrap();
        assert!(parsed.body_text.contains("Été en gras"));
        assert!(parsed.body_html.contains("<strong>gras</strong>"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].data, b"file content");
    }

    #[test]
    fn html_without_files_is_alternative_and_invalid_recipients_fail() {
        let mail = build_message(
            "alice@example.com",
            &["Bob <bob@example.com>".into()],
            &[],
            &[],
            "Hi",
            "Hello",
            Some("<p>Hello</p>"),
            &[],
            None,
            None,
        )
        .unwrap();
        let raw = String::from_utf8(mail.formatted()).unwrap();
        assert!(raw.contains("multipart/alternative"));
        assert!(!raw.contains("multipart/mixed"));
        assert!(build_message(
            "alice@example.com",
            &["invalid".into()],
            &[],
            &[],
            "Hi",
            "Hello",
            None,
            &[],
            None,
            None
        )
        .is_err());
    }

    #[tokio::test]
    async fn local_smtp_receives_rich_mail_and_reports_rejection() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        for reject in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                write.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
                let mut data = String::new();
                let mut in_data = false;
                loop {
                    let mut line = String::new();
                    if read.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    if in_data {
                        if line == ".\r\n" {
                            write
                                .write_all(if reject {
                                    b"554 message rejected\r\n"
                                } else {
                                    b"250 queued\r\n"
                                })
                                .await
                                .unwrap();
                            break;
                        }
                        data.push_str(&line);
                        continue;
                    }
                    let reply: &[u8] = if line.starts_with("EHLO") {
                        b"250-localhost\r\n250 AUTH PLAIN\r\n"
                    } else if line.starts_with("AUTH") {
                        b"235 authenticated\r\n"
                    } else if line.starts_with("DATA") {
                        in_data = true;
                        b"354 go ahead\r\n"
                    } else {
                        b"250 OK\r\n"
                    };
                    write.write_all(reply).await.unwrap();
                }
                data
            });
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                send(
                    &SmtpConfig {
                        host: "127.0.0.1".into(),
                        port,
                        tls: TlsMode::Plaintext,
                    },
                    &AuthMethod::Password {
                        username: "local".into(),
                        password: "test".into(),
                    },
                    "alice@example.com",
                    &["bob@example.com".into()],
                    &[],
                    &[],
                    "Local test",
                    "Hello",
                    Some("<p><b>Hello</b></p>"),
                    &[attachment("file.txt", "text/plain")],
                    None,
                    None,
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.is_err(), reject);
            let data = server.await.unwrap();
            assert!(data.contains("multipart/alternative"));
            assert!(data.contains("file.txt"));
        }
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    #[tokio::test]
    async fn account_check_authenticates_without_sending_mail() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        for reject in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                write.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
                let mut authenticated = false;
                loop {
                    let mut line = String::new();
                    if read.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    assert!(
                        !line.starts_with("MAIL")
                            && !line.starts_with("RCPT")
                            && !line.starts_with("DATA")
                    );
                    let response: &[u8] = if line.starts_with("EHLO") {
                        b"250-localhost\r\n250 AUTH PLAIN\r\n"
                    } else if line.starts_with("AUTH") {
                        if reject {
                            b"535 credentials rejected\r\n"
                        } else {
                            authenticated = true;
                            b"235 authenticated\r\n"
                        }
                    } else if line.starts_with("NOOP") {
                        assert!(authenticated);
                        b"250 OK\r\n"
                    } else {
                        b"221 Bye\r\n"
                    };
                    if write.write_all(response).await.is_err() {
                        break;
                    }
                }
                authenticated
            });
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                test_connection(
                    &SmtpConfig {
                        host: "127.0.0.1".into(),
                        port,
                        tls: TlsMode::Plaintext,
                    },
                    &AuthMethod::Password {
                        username: "local".into(),
                        password: "test".into(),
                    },
                    "local@example.test",
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.is_err(), reject);
            assert_eq!(server.await.unwrap(), !reject);
        }
    }
}
