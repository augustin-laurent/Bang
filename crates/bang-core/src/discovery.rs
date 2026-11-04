//! Bounded, credential-free discovery of a domain's published mail settings.
use crate::{Error, ImapConfig, Result, SmtpConfig, TlsMode};
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const LIMIT: usize = 256 * 1024;

/// A provider's editable settings, without credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredSettings {
    /// Provider display name from the published document.
    pub name: String,
    /// Incoming endpoint.
    pub imap: ImapConfig,
    /// Outgoing endpoint.
    pub smtp: SmtpConfig,
    /// Incoming username after safe placeholder substitution.
    pub username: String,
    /// Outgoing username after safe placeholder substitution.
    pub smtp_username: String,
}

/// Discovery result; unsupported authentication must never become password auth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiscoveryResult {
    /// Complete settings, to be reviewed before any authentication.
    Found(DiscoveredSettings),
    /// No safe supported settings; show the manual editor.
    NotFound,
    /// The provider advertises an unsupported authentication mechanism.
    Unsupported,
}

fn domain(email: &str) -> Option<(&str, String)> {
    let (local, domain) = email.trim().rsplit_once('@')?;
    if local.is_empty() || local.contains('@') || email.chars().any(char::is_control) {
        return None;
    }
    let host = url::Host::parse(domain).ok()?;
    let url::Host::Domain(domain) = host else {
        return None;
    };
    if domain.is_empty() || !domain.contains('.') || domain.ends_with('.') {
        return None;
    }
    Some((local, domain))
}

/// Look up only the domain's two HTTPS autoconfig URLs, never the email address.
pub async fn discover(email: &str) -> Result<DiscoveryResult> {
    let (_, domain) =
        domain(email).ok_or_else(|| Error::Config("Enter a valid email address.".into()))?;
    discover_urls(
        email,
        &[
            format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
            format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
        ],
    )
    .await
}

async fn discover_urls(email: &str, urls: &[String]) -> Result<DiscoveryResult> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::Network("Could not start settings discovery.".into()))?;
    let mut unsupported = false;
    for url in urls {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut response = client.get(url).send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            if response.content_length().is_some_and(|n| n > LIMIT as u64) {
                return None;
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.ok()? {
                if bytes.len() + chunk.len() > LIMIT {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            parse(&bytes, email).ok()
        })
        .await
        .ok()
        .flatten();
        match result {
            Some(DiscoveryResult::Found(settings)) => return Ok(DiscoveryResult::Found(settings)),
            Some(DiscoveryResult::Unsupported) => unsupported = true,
            _ => {}
        }
    }
    Ok(if unsupported {
        DiscoveryResult::Unsupported
    } else {
        DiscoveryResult::NotFound
    })
}

#[derive(Default)]
struct Server {
    incoming: bool,
    host: String,
    port: String,
    socket: String,
    auth: Vec<String>,
    user: String,
}

/// Parse the small supported autoconfig subset; never process entities or DTDs.
fn parse(bytes: &[u8], email: &str) -> Result<DiscoveryResult> {
    let invalid = || Error::Config("Invalid published mail settings.".into());
    if bytes.len() > LIMIT {
        return Err(invalid());
    }
    let (local, email_domain) = domain(email).ok_or_else(invalid)?;
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut server: Option<Server> = None;
    let mut servers = Vec::new();
    let mut name = String::new();
    loop {
        match reader.read_event().map_err(|_| invalid())? {
            Event::Start(e) => {
                if stack.len() >= 16 {
                    return Err(invalid());
                }
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if stack.is_empty()
                    && (tag != "clientConfig"
                        || !e.attributes().any(|a| {
                            a.is_ok_and(|a| {
                                a.key.as_ref() == b"version" && a.value.as_ref() == b"1.1"
                            })
                        }))
                {
                    return Err(invalid());
                }
                if matches!(tag.as_str(), "incomingServer" | "outgoingServer") {
                    if server.is_some() {
                        return Err(invalid());
                    }
                    let incoming = tag == "incomingServer";
                    let expected: &[u8] = if incoming { b"imap" } else { b"smtp" };
                    if e.attributes().any(|a| {
                        a.is_ok_and(|a| a.key.as_ref() == b"type" && a.value.as_ref() == expected)
                    }) {
                        server = Some(Server {
                            incoming,
                            ..Default::default()
                        });
                    }
                }
                stack.push(tag);
            }
            Event::Text(e) => {
                let text = e.decode().map_err(|_| invalid())?.into_owned();
                if text.len() > 2048 {
                    return Err(invalid());
                }
                let tag = stack.last().map(String::as_str).unwrap_or_default();
                if let Some(s) = &mut server {
                    match tag {
                        "hostname" => s.host.push_str(&text),
                        "port" => s.port.push_str(&text),
                        "socketType" => s.socket.push_str(&text),
                        "authentication" => s.auth.push(text),
                        "username" => s.user.push_str(&text),
                        _ => {}
                    }
                } else if tag == "displayName" && name.is_empty() {
                    name = text;
                }
            }
            Event::End(e) => {
                if matches!(e.name().as_ref(), b"incomingServer" | b"outgoingServer") {
                    if let Some(s) = server.take() {
                        servers.push(s);
                    }
                }
                stack.pop().ok_or_else(invalid)?;
            }
            Event::DocType(_) | Event::GeneralRef(_) => return Err(invalid()),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(invalid());
    }
    let mut incoming = None;
    let mut outgoing = None;
    let mut unsupported = false;
    for s in servers {
        let tls = match s.socket.as_str() {
            "SSL" => TlsMode::Tls,
            "STARTTLS" => TlsMode::Starttls,
            _ => continue,
        };
        if !s
            .auth
            .iter()
            .any(|a| matches!(a.as_str(), "password-cleartext" | "password-encrypted"))
            || s.auth.iter().all(|a| a == "password-encrypted")
        {
            unsupported = true;
            continue;
        }
        let port = s.port.parse::<u16>().ok().filter(|p| *p > 0);
        // A hostname is a hostname, never a URL, path, userinfo or port.
        if s.host.is_empty()
            || s.host
                .chars()
                .any(|c| c.is_whitespace() || "/:@?#%".contains(c))
            || url::Host::parse(&s.host).is_err()
        {
            continue;
        }
        let Some(port) = port else {
            continue;
        };
        let user = if s.user.is_empty() {
            email.to_owned()
        } else {
            s.user
                .replace("%EMAILADDRESS%", email)
                .replace("%EMAILLOCALPART%", local)
                .replace("%EMAILDOMAIN%", &email_domain)
        };
        if user.contains('%') || user.chars().any(char::is_control) {
            continue;
        }
        if s.incoming && incoming.is_none() {
            incoming = Some((
                ImapConfig {
                    host: s.host,
                    port,
                    tls,
                },
                user,
            ));
        } else if !s.incoming && outgoing.is_none() {
            outgoing = Some((
                SmtpConfig {
                    host: s.host,
                    port,
                    tls,
                },
                user,
            ));
        }
    }
    Ok(match (incoming, outgoing) {
        (Some((imap, username)), Some((smtp, smtp_username))) => {
            DiscoveryResult::Found(DiscoveredSettings {
                name,
                imap,
                smtp,
                username,
                smtp_username,
            })
        }
        _ if unsupported => DiscoveryResult::Unsupported,
        _ => DiscoveryResult::NotFound,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn xml(auth: &str, socket: &str) -> String {
        format!(
            r#"<clientConfig version="1.1"><emailProvider><displayName>Example</displayName><incomingServer type="imap"><hostname>imap.example.test</hostname><port>993</port><socketType>{socket}</socketType><authentication>{auth}</authentication><username>%EMAILLOCALPART%</username></incomingServer><outgoingServer type="smtp"><hostname>smtp.example.test</hostname><port>587</port><socketType>STARTTLS</socketType><authentication>{auth}</authentication><username>%EMAILADDRESS%</username></outgoingServer></emailProvider></clientConfig>"#
        )
    }
    #[test]
    fn discovers_independent_usernames_without_credentials() {
        let DiscoveryResult::Found(s) = parse(
            xml("password-cleartext", "SSL").as_bytes(),
            "alice@example.test",
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(s.username, "alice");
        assert_eq!(s.smtp_username, "alice@example.test");
        assert_eq!(s.smtp.port, 587);
        assert!(matches!(
            parse(xml("OAuth2", "SSL").as_bytes(), "alice@example.test").unwrap(),
            DiscoveryResult::Unsupported
        ));
        assert!(matches!(
            parse(
                xml("password-cleartext", "plain").as_bytes(),
                "alice@example.test"
            )
            .unwrap(),
            DiscoveryResult::NotFound
        ));
    }
    #[test]
    fn rejects_entities_limits_and_invalid_domains() {
        assert!(parse(
            b"<!DOCTYPE x [<!ENTITY y SYSTEM 'file:///etc/passwd'>]><clientConfig/>",
            "a@b.test"
        )
        .is_err());
        assert!(parse(&vec![b' '; LIMIT + 1], "a@b.test").is_err());
        for e in [
            "a@b.test/path",
            "a@b.test:443",
            "a@b.test?x=y",
            "a\n@b.test",
        ] {
            assert!(domain(e).is_none(), "{e}");
        }
    }

    #[tokio::test]
    async fn network_discovery_is_ordered_does_not_send_email_or_follow_redirects_and_bounds_body()
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for path in ["/first", "/second", "/large"] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                assert!(
                    request.starts_with(&format!("GET {path} HTTP/1.1")),
                    "{request}"
                );
                assert!(
                    !request.contains("alice")
                        && !request.contains("emailaddress")
                        && !request.to_lowercase().contains("authorization:")
                );
                let response = match path {
                    "/first" => "HTTP/1.1 302 Found\r\nLocation: /must-not-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
                    "/large" => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", LIMIT + 1),
                    _ => { let body = xml("password-cleartext", "SSL"); format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()) },
                };
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        assert!(matches!(
            discover_urls(
                "alice@example.test",
                &[format!("{base}/first"), format!("{base}/second")]
            )
            .await
            .unwrap(),
            DiscoveryResult::Found(_)
        ));
        assert!(matches!(
            discover_urls("alice@example.test", &[format!("{base}/large")])
                .await
                .unwrap(),
            DiscoveryResult::NotFound
        ));
        server.await.unwrap();
    }
}
