//! MIME parsing and text extraction.

use dom_query::{Document, NodeData, NodeRef};
use mail_parser::{Charset, HeaderForm, MessageParser, PartKind, PartRole};

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
    /// Content-ID for embedded resources (without angle brackets).
    pub content_id: String,
    /// Content-Disposition, or inferred inline disposition.
    pub disposition: String,
}

/// Decode an unstructured RFC 2047 header with its declared charset.
pub fn decode_header(bytes: &[u8]) -> String {
    HeaderForm::Text
        .parse(bytes)
        .value()
        .as_text()
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned())
}

/// Decode an IMAP modified UTF-7 display name; preserve invalid sequences.
/// Server paths must retain their original wire representation.
pub fn decode_folder_name(name: &str) -> String {
    let mut result = String::new();
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('-') else { break };
        if end == 1 {
            result.push('&');
        } else {
            let encoded = &rest[1..end];
            if !encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b',')
            {
                result.push_str(&rest[..=end]);
            } else {
                let standard = format!("+{}-", encoded.replace(',', "/"));
                let (decoded, malformed) = Charset::Utf7.decode_checked(standard.as_bytes());
                if malformed {
                    result.push_str(&rest[..=end]);
                } else {
                    result.push_str(&decoded);
                }
            }
        }
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    result
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

    let body_text = message.text_bodies().collect::<Vec<_>>().join("\n");
    let body_html = message
        .html_body()
        .filter(|p| matches!(p.kind(), PartKind::Html))
        .filter_map(|p| p.text())
        .collect::<Vec<_>>()
        .join("\n");

    let mut attachments = Vec::new();
    for part in message.root().parts().filter(|p| {
        p.is_attachment() || (!p.is_text() && (p.is_inline() || p.content_id().is_some()))
    }) {
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
            content_id: part
                .content_id()
                .unwrap_or_default()
                .trim_matches(['<', '>'])
                .to_string(),
            disposition: part
                .content_disposition()
                .map(|d| d.ctype().to_string())
                .unwrap_or_else(|| {
                    if part.is_inline() {
                        "inline".into()
                    } else {
                        String::new()
                    }
                }),
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
    let collapsed: String = body_text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(max_chars).collect()
}

const MAX_RENDER_DEPTH: usize = 64;

/// Render a common HTML subset as structured plain text.
///
/// Maps paragraphs, headings, lists, blockquotes, links, tables, and line
/// breaks to a text layout. `script`/`style`/`noscript` content is dropped.
/// Unknown elements render their content inline. Subtrees beyond 64 levels
/// are replaced with an ellipsis, bounding stack use for untrusted input.
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
        render_node(&child, &mut out, 0, 0);
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

fn bounded_text(node: &NodeRef, depth: usize) -> String {
    if depth >= MAX_RENDER_DEPTH {
        return "…".into();
    }
    if let Some(text) = node.query_or(None, |n| match &n.data {
        NodeData::Text { contents } => Some(contents.to_string()),
        _ => None,
    }) {
        return text;
    }
    node.children()
        .into_iter()
        .map(|child| bounded_text(&child, depth + 1))
        .collect()
}

/// Recursively render a node and its children.
fn render_node(node: &NodeRef, out: &mut String, quote_depth: usize, depth: usize) {
    if depth >= MAX_RENDER_DEPTH {
        out.push('…');
        return;
    }
    // Text node.
    if let Some(text) = node.query_or(None, |n| match &n.data {
        NodeData::Text { contents } => Some(contents.to_string()),
        _ => None,
    }) {
        let collapsed = collapse_ws(&text);
        if !collapsed.is_empty() {
            if text.starts_with(char::is_whitespace)
                && !out.is_empty()
                && !out.ends_with(char::is_whitespace)
            {
                out.push(' ');
            }
            for _ in 0..quote_depth {
                out.push_str("> ");
            }
            out.push_str(&collapsed);
            if text.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else if !text.is_empty() && !out.is_empty() && !out.ends_with(char::is_whitespace) {
            out.push(' ');
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
            render_children(node, out, quote_depth, depth);
            out.push_str("\n\n");
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let text = bounded_text(node, depth);
            if !text.trim().is_empty() {
                out.push_str(&text);
                out.push_str("\n\n");
            }
        }
        "blockquote" => {
            out.push('\n');
            render_children(node, out, quote_depth + 1, depth);
            out.push('\n');
        }
        "ul" => render_list(node, out, quote_depth, depth, false),
        "ol" => render_list(node, out, quote_depth, depth, true),
        "a" => {
            let text = bounded_text(node, depth);
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
            render_children(node, out, quote_depth, depth);
            out.push_str("\n\n");
        }
        "tr" => {
            out.push('\n');
            render_children(node, out, quote_depth, depth);
        }
        "td" | "th" => {
            render_children(node, out, quote_depth, depth);
            out.push_str("  ");
        }
        _ => render_children(node, out, quote_depth, depth),
    }
}

/// Render the children of a node.
fn render_children(node: &NodeRef, out: &mut String, quote_depth: usize, depth: usize) {
    for child in node.children() {
        render_node(&child, out, quote_depth, depth + 1);
    }
}

/// Render a `ul`/`ol` list with bullet or numbered markers.
fn render_list(node: &NodeRef, out: &mut String, quote_depth: usize, depth: usize, ordered: bool) {
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
            render_children(&child, out, quote_depth, depth + 1);
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
pub fn to_attachment_records(message_id: i64, attachments: &[AttachmentData]) -> Vec<Attachment> {
    attachments
        .iter()
        .map(|a| Attachment {
            id: 0, // assigned by the store
            message_id,
            filename: a.filename.clone(),
            content_type: a.content_type.clone(),
            size: a.size,
            path: String::new(),
            content_id: a.content_id.clone(),
            disposition: a.disposition.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_link_text_obey_render_depth_limit() {
        for tag in ["h1", "a"] {
            let html = format!(
                "<{tag}>{}hidden{} visible</{tag}>",
                "<span>".repeat(100),
                "</span>".repeat(100)
            );
            let rendered = render_html_subset(&html);
            assert!(!rendered.contains("hidden"));
            assert!(rendered.contains('…') && rendered.contains("visible"));
        }
    }

    #[test]
    fn decode_encoded_headers_and_folder_names() {
        assert_eq!(
            decode_header(b"=?UTF-8?Q?Oph=C3=A9lie?= =?UTF-8?B?IMOJdMOp?="),
            "Ophélie Été"
        );
        assert_eq!(
            decode_header(b"=?windows-1252?Q?Jusqu=92=E0_30_=80?="),
            "Jusqu’à 30 €"
        );
        assert_eq!(
            decode_header(b"=?UTF-8?Q?Feuille_de_calcul?=\r\n =?UTF-8?Q?_partag=C3=A9e?="),
            "Feuille de calcul partagée"
        );
        assert_eq!(
            decode_header("Bonjour déjà prêt".as_bytes()),
            "Bonjour déjà prêt"
        );
        assert_eq!(
            decode_header(b"broken =?UTF-8?Q?unterminated"),
            "broken =?UTF-8?Q?unterminated"
        );
        assert_eq!(
            decode_folder_name("Messages envoy&AOk-s"),
            "Messages envoyés"
        );
        assert_eq!(
            decode_folder_name("Projet &- notes + été"),
            "Projet & notes + été"
        );
        assert_eq!(decode_folder_name("&ZeVnLIqe-"), "日本語");
        assert_eq!(
            decode_folder_name("A &bad?tail- &unfinished"),
            "A &bad?tail- &unfinished"
        );
    }

    #[test]
    fn body_charsets_and_transfer_encodings() {
        for (charset, encoding, body, expected) in [
            (
                "UTF-8",
                "quoted-printable",
                "=C3=89t=C3=A9 =C3=A0 3,14=E2=82=AC",
                "Été à 3,14€",
            ),
            (
                "ISO-8859-1",
                "quoted-printable",
                "=C9t=E9 d=E9j=E0",
                "Été déjà",
            ),
            (
                "windows-1252",
                "quoted-printable",
                "Jusqu=92=E0 30 =80",
                "Jusqu’à 30 €",
            ),
            ("UTF-8", "base64", "w4l0w6kgw6AgMywxNOKCrA==", "Été à 3,14€"),
        ] {
            let raw = format!("From: alice@example.test\r\nSubject: Charset\r\nContent-Type: text/plain; charset={charset}\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n{body}");
            let parsed = parse(raw.as_bytes()).unwrap();
            assert_eq!(parsed.body_text, expected, "{charset}");
            assert!(
                parsed.body_html.is_empty(),
                "plain text must not become synthetic HTML"
            );
        }
    }

    #[test]
    fn html_related_resources_keep_cid_and_disposition() {
        let raw = b"From: Alice <alice@example.test>\r\nSubject: Related\r\nContent-Type: multipart/related; boundary=x\r\n\r\n--x\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<p>Hello</p><img src=\"cid:logo@example.test\">\r\n--x\r\nContent-Type: image/png\r\nContent-ID: <logo@example.test>\r\nContent-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--x--\r\n";
        let parsed = parse(raw).unwrap();
        assert!(parsed.body_html.contains("cid:logo@example.test"));
        assert_eq!(parsed.attachments.len(), 1);
        let a = &parsed.attachments[0];
        assert_eq!(a.content_id, "logo@example.test");
        assert_eq!(a.disposition, "inline");
        assert_eq!(a.data, b"hello");
    }

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
        assert!(msg.body_html.is_empty());
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
    fn deeply_nested_html_is_bounded_and_preserves_following_content() {
        // Exercise the native stack budget, not only the amount of output:
        // empty spans synthesize an empty text alternative in mail-parser.
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                for (open, close) in [
                    ("<span>", "</span>"),
                    ("<blockquote>", "</blockquote>"),
                    ("<ul><li>", "</li></ul>"),
                ] {
                    let html = format!(
                        "{}{}<p>Still readable</p>",
                        open.repeat(10_000),
                        close.repeat(10_000)
                    );
                    let raw = format!("Content-Type: text/html; charset=utf-8\r\n\r\n{html}");
                    let message = parse(raw.as_bytes()).unwrap();
                    let text = render_html_subset(&message.body_html);
                    assert!(text.contains('…'));
                    assert!(text.ends_with("Still readable"));
                    assert!(text.len() < 20_000);
                }
                let raw = format!(
                    "Content-Type: text/html\r\n\r\n{}{}",
                    "<span>".repeat(10_000),
                    "</span>".repeat(10_000)
                );
                let message = parse(raw.as_bytes()).unwrap();
                assert!(message.body_text.is_empty());
                assert_eq!(render_html_subset(&message.body_html), "…");
                assert_eq!(render_html_subset("<p>Normal été</p>"), "Normal été");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn extract_preview_collapses_and_truncates() {
        let preview = extract_preview("Hello\n\nworld   foo", 11);
        assert_eq!(preview, "Hello world");
        // Limits count Unicode characters, not UTF-8 bytes.
        let preview = extract_preview("héllo wörld", 5);
        assert_eq!(preview, "héllo");
    }
}
