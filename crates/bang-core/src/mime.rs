//! MIME parsing and text extraction.

use mail_parser::{MessageParser, PartRole};

use crate::error::{Error, Result};
use crate::message::{Attachment, EmailAddress};

/// A parsed email message.
#[derive(Debug, Clone)]
pub struct ParsedMessage {
    /// Sender.
    pub from: EmailAddress,
    /// Recipients.
    pub to: Vec<EmailAddress>,
    /// CC recipients.
    pub cc: Vec<EmailAddress>,
    /// Subject line.
    pub subject: String,
    /// RFC 5322 Message-ID (may be empty).
    pub message_id: String,
    /// Message-ID being replied to (may be empty).
    pub in_reply_to: String,
    /// Plain-text body.
    pub body_text: String,
    /// HTML body (may be empty).
    pub body_html: String,
    /// Attachments with their decoded data.
    pub attachments: Vec<AttachmentData>,
}

/// An attachment with its decoded content.
#[derive(Debug, Clone)]
pub struct AttachmentData {
    /// File name (may be empty).
    pub filename: String,
    /// MIME content type (e.g. `application/pdf`).
    pub content_type: String,
    /// Size in bytes.
    pub size: u64,
    /// Decoded content.
    pub data: Vec<u8>,
}

/// Parse a raw email message.
///
/// # Errors
/// Returns `Error::Mime` if the data is not a valid message.
pub fn parse(raw: &[u8]) -> Result<ParsedMessage> {
    let message = MessageParser::new()
        .parse(raw)
        .ok_or_else(|| Error::Mime("could not parse message".into()))?;

    let from = message
        .from()
        .and_then(|l| l.mailboxes().next())
        .map(convert_address)
        .unwrap_or(EmailAddress {
            name: String::new(),
            address: String::new(),
        });
    let to = message
        .to()
        .map(|l| l.mailboxes().map(convert_address).collect())
        .unwrap_or_default();
    let cc = message
        .cc()
        .map(|l| l.mailboxes().map(convert_address).collect())
        .unwrap_or_default();

    let subject = message.subject().unwrap_or_default().to_string();
    let message_id = message.message_id().unwrap_or_default().to_string();
    let in_reply_to = message
        .in_reply_to()
        .and_then(|l| l.iter().next())
        .unwrap_or_default()
        .to_string();

    let body_text = message
        .text_bodies()
        .collect::<Vec<_>>()
        .join("\n");
    let body_html = message
        .html_bodies()
        .collect::<Vec<_>>()
        .join("\n");

    let mut attachments = Vec::new();
    for part in message.attachments() {
        if part.role() == PartRole::Container {
            continue;
        }
        let data = part.decoded();
        let content_type = part
            .content_type()
            .map(|ct| {
                let sub = ct.subtype().unwrap_or("");
                format!("{}/{}", ct.ctype(), sub)
            })
            .unwrap_or_default();
        attachments.push(AttachmentData {
            filename: part.attachment_name().unwrap_or_default().to_string(),
            content_type,
            size: data.len() as u64,
            data: data.to_vec(),
        });
    }

    Ok(ParsedMessage {
        from,
        to,
        cc,
        subject,
        message_id,
        in_reply_to,
        body_text,
        body_html,
        attachments,
    })
}

/// Extract a short preview from a plain-text body.
///
/// Collapses whitespace and truncates to roughly `max_chars` characters.
pub fn extract_preview(body_text: &str, max_chars: usize) -> String {
    let collapsed: String = body_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Truncate on a char boundary.
    let mut end = max_chars.min(collapsed.len());
    while end > 0 && !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    collapsed[..end].to_string()
}

/// Convert a mail-parser mailbox to an `EmailAddress`.
fn convert_address(m: mail_parser::Mailbox<'_>) -> EmailAddress {
    EmailAddress {
        name: m.name().unwrap_or_default().to_string(),
        address: m.address().unwrap_or_default().to_string(),
    }
}

/// Convert a `ParsedMessage`'s attachments into store `Attachment` records.
///
/// # Arguments
/// * `message_id` — the local store id of the owning message.
pub fn to_attachment_records(
    message_id: i64,
    attachments: &[AttachmentData],
) -> Vec<Attachment> {
    attachments
        .iter()
        .map(|a| Attachment {
            id: 0, // assigned by the store
            message_id,
            filename: a.filename.clone(),
            content_type: a.content_type.clone(),
            size: a.size,
            path: String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_text_message() {
        let raw = b"From: Alice <alice@example.com>\r\n\
                    To: bob@example.com\r\n\
                    Subject: Hello\r\n\
                    Message-ID: <1@example.com>\r\n\
                    \r\n\
                    Hi there!\r\n";
        let msg = parse(raw).unwrap();
        assert_eq!(msg.from.name, "Alice");
        assert_eq!(msg.from.address, "alice@example.com");
        assert_eq!(msg.to.len(), 1);
        assert_eq!(msg.to[0].address, "bob@example.com");
        assert_eq!(msg.subject, "Hello");
        // mail-parser strips the angle brackets from Message-ID.
        assert_eq!(msg.message_id, "1@example.com");
        assert!(msg.body_text.contains("Hi there!"));
        // mail-parser converts text parts to HTML, so body_html holds the
        // converted markup; the UI prefers body_text when it is non-empty.
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn parse_multipart_alternative() {
        let raw = b"From: alice@example.com\r\n\
                    To: bob@example.com\r\n\
                    Subject: Multi\r\n\
                    MIME-Version: 1.0\r\n\
                    Content-Type: multipart/alternative; boundary=\"bnd\"\r\n\
                    \r\n\
                    --bnd\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    Plain body\r\n\
                    --bnd\r\n\
                    Content-Type: text/html\r\n\
                    \r\n\
                    <html><body>HTML body</body></html>\r\n\
                    --bnd--\r\n";
        let msg = parse(raw).unwrap();
        assert!(msg.body_text.contains("Plain body"));
        assert!(msg.body_html.contains("HTML body"));
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn parse_attachment() {
        // "hello" base64-encoded.
        let raw = b"From: alice@example.com\r\n\
                    To: bob@example.com\r\n\
                    Subject: File\r\n\
                    MIME-Version: 1.0\r\n\
                    Content-Type: multipart/mixed; boundary=\"bnd\"\r\n\
                    \r\n\
                    --bnd\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    Body\r\n\
                    --bnd\r\n\
                    Content-Type: application/octet-stream\r\n\
                    Content-Disposition: attachment; filename=\"note.txt\"\r\n\
                    Content-Transfer-Encoding: base64\r\n\
                    \r\n\
                    aGVsbG8=\r\n\
                    --bnd--\r\n";
        let msg = parse(raw).unwrap();
        assert_eq!(msg.attachments.len(), 1);
        let att = &msg.attachments[0];
        assert_eq!(att.filename, "note.txt");
        assert_eq!(att.content_type, "application/octet-stream");
        assert_eq!(att.data, b"hello");
        assert_eq!(att.size, 5);

        let records = to_attachment_records(7, &msg.attachments);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message_id, 7);
        assert_eq!(records[0].filename, "note.txt");
    }

    #[test]
    fn extract_preview_collapses_and_truncates() {
        let preview = extract_preview("Hello\n\nworld   foo", 11);
        assert_eq!(preview, "Hello world");
        // Truncation respects char boundaries ("héllo" is 6 bytes).
        let preview = extract_preview("héllo wörld", 6);
        assert_eq!(preview, "héllo");
    }
}
