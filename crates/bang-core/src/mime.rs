//! MIME parsing and text extraction.

use dom_query::{Document, NodeData, NodeRef};
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

/// Render a common HTML subset as structured plain text.
///
/// Maps paragraphs, headings, lists, blockquotes, links, tables, and line
/// breaks to a text layout. `script`/`style`/`noscript` content is dropped.
/// Unknown elements render their content inline.
pub fn render_html_subset(html: &str) -> String {
    let doc = Document::fragment(html);
    // Drop non-content elements before rendering.
    for tag in ["script", "style", "noscript", "template"] {
        for group in doc.select(tag).iter().collect::<Vec<_>>() {
            if let Some(node) = group.get(0) {
                node.remove_from_parent();
            }
        }
    }
    // The fragment parser wraps content in a bare <html> element (a <body>
    // tag in the input is ignored); its children are the actual content.
    let root = doc
        .root()
        .children()
        .into_iter()
        .find(|c| c.node_name().is_some())
        .unwrap_or_else(|| doc.root());
    let mut out = String::new();
    for child in root.children() {
        if child
            .node_name()
            .map(|n| n.to_string().to_ascii_lowercase())
            .as_deref()
            == Some("head")
        {
            continue;
        }
        render_node(&child, &mut out, 0);
    }

    // Collapse runs of blank lines and trim the ends.
    let mut result = String::new();
    let mut blank = 0;
    for line in out.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank <= 1 {
                result.push('\n');
            }
        } else {
            blank = 0;
            result.push_str(line.trim_end());
            result.push('\n');
        }
    }
    result.trim().to_string()
}

/// Recursively render a node and its children.
fn render_node(node: &NodeRef, out: &mut String, quote_depth: usize) {
    // Text node.
    if let Some(text) = node.query_or(None, |n| match &n.data {
        NodeData::Text { contents } => Some(contents.to_string()),
        _ => None,
    }) {
        let collapsed = collapse_ws(&text);
        if !collapsed.is_empty() {
            for _ in 0..quote_depth {
                out.push_str("> ");
            }
            out.push_str(&collapsed);
        }
        return;
    }
    let Some(name) = node.node_name().map(|n| n.to_string().to_ascii_lowercase()) else {
        return;
    };
    match name.as_str() {
        "br" => out.push('\n'),
        "hr" => out.push_str("\n\n"),
        "p" | "div" | "section" | "article" | "header" | "footer" | "main" | "figure" => {
            render_children(node, out, quote_depth);
            out.push_str("\n\n");
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let text = node.text().to_string();
            if !text.trim().is_empty() {
                out.push_str(&text);
                out.push_str("\n\n");
            }
        }
        "blockquote" => {
            out.push('\n');
            render_children(node, out, quote_depth + 1);
            out.push('\n');
        }
        "ul" => render_list(node, out, quote_depth, false),
        "ol" => render_list(node, out, quote_depth, true),
        "a" => {
            let text = node.text().to_string();
            let href = node.attr("href").map(|h| h.to_string());
            match href {
                Some(h) if !h.is_empty() && !h.starts_with('#') && h != text => {
                    out.push_str(&text);
                    out.push_str(&format!(" ({h})"));
                }
                _ => out.push_str(&text),
            }
        }
        "img" => {
            if let Some(alt) = node.attr("alt").map(|a| a.to_string()) {
                if !alt.trim().is_empty() {
                    out.push_str(&format!("[image: {alt}]"));
                }
            }
        }
        "table" => {
            render_children(node, out, quote_depth);
            out.push_str("\n\n");
        }
        "tr" => {
            out.push('\n');
            render_children(node, out, quote_depth);
        }
        "td" | "th" => {
            render_children(node, out, quote_depth);
            out.push_str("  ");
        }
        _ => render_children(node, out, quote_depth),
    }
}

/// Render the children of a node.
fn render_children(node: &NodeRef, out: &mut String, quote_depth: usize) {
    for child in node.children() {
        render_node(&child, out, quote_depth);
    }
}

/// Render a `ul`/`ol` list with bullet or numbered markers.
fn render_list(node: &NodeRef, out: &mut String, quote_depth: usize, ordered: bool) {
    let mut n = 0;
    for child in node.children() {
        if child
            .node_name()
            .map(|n| n.to_string().to_ascii_lowercase())
            .as_deref()
            == Some("li")
        {
            n += 1;
            if ordered {
                out.push_str(&format!("{n}. "));
            } else {
                out.push_str("- ");
            }
            render_children(&child, out, quote_depth);
            out.push('\n');
        }
    }
    out.push('\n');
}

/// Collapse all whitespace runs to single spaces.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
    fn render_html_subset_blocks_and_breaks() {
        let html = "<html><body>\
                    <p>Hello   world</p>\
                    <p>Second line<br>after break</p>\
                    </body></html>";
        let text = render_html_subset(html);
        assert_eq!(text, "Hello world\n\nSecond line\nafter break");
    }

    #[test]
    fn render_html_subset_lists_blockquote_and_links() {
        let html = "<div>\
                    <ul><li>one</li><li>two</li></ul>\
                    <ol><li>first</li><li>second</li></ol>\
                    <blockquote>quoted<br>text</blockquote>\
                    <p>See <a href=\"https://example.com\">the site</a> and \
                    <a href=\"#top\">anchor</a>.</p>\
                    </div>";
        let text = render_html_subset(html);
        assert!(text.contains("- one\n- two"));
        assert!(text.contains("1. first\n2. second"));
        assert!(text.contains("> quoted\n> text"));
        assert!(text.contains("the site (https://example.com)"));
        // Anchor links keep their text without a URL.
        assert!(text.contains("anchor."));
        assert!(!text.contains("#top"));
    }

    #[test]
    fn render_html_subset_drops_scripts_and_renders_tables() {
        let html = "<body>\
                    <script>var x = 1;</script>\
                    <style>.a { color: red; }</style>\
                    <table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>\
                    <h2>Title</h2>\
                    </body>";
        let text = render_html_subset(html);
        assert!(!text.contains("var x"));
        assert!(!text.contains("color"));
        assert!(text.contains("a  b"));
        assert!(text.contains("c  d"));
        assert!(text.contains("Title"));
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
