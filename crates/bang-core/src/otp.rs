//! Conservative extraction of numeric verification codes from message text.

use serde::{Deserialize, Serialize};

/// An ephemeral code and its source. Debug output deliberately excludes the code.
#[derive(Clone, Serialize, Deserialize)]
pub struct OtpCandidate {
    /// The account that received the message.
    pub account_id: String,
    /// Local message identifier.
    pub message_id: i64,
    /// Sender address as supplied by the message; it is not authenticated.
    pub sender: String,
    /// Account address that received the message.
    pub recipient: String,
    /// IMAP internal date, in seconds since the Unix epoch.
    pub received_at: i64,
    /// The code, which must only be retained in memory.
    pub code: String,
}

impl std::fmt::Debug for OtpCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtpCandidate")
            .field("account_id", &self.account_id)
            .field("message_id", &self.message_id)
            .field("received_at", &self.received_at)
            .field("code", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// Maximum permitted size of a message inspected for an OTP.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// Maximum age of a message inspected for an OTP.
pub const MAX_AGE_SECONDS: i64 = 180;

/// Check freshness against the server's internal delivery timestamp.
pub fn is_fresh(received_at: i64, now: i64) -> bool {
    received_at > 0 && received_at <= now && now - received_at < MAX_AGE_SECONDS
}

/// Extract a code from a bounded MIME message without persisting its body.
pub fn extract_from_message(raw: &[u8]) -> Option<String> {
    if raw.len() > MAX_MESSAGE_BYTES {
        return None;
    }
    let message = crate::mime::parse(raw).ok()?;
    // Alternative parts can differ (for example, a plain-text part may only
    // say to view the HTML version). Inspect both, while keeping the same
    // ambiguity check if they contain different numeric candidates.
    let body = format!(
        "{}\n{}",
        message.body_text,
        crate::mime::render_html_subset(&message.body_html)
    );
    extract_code(&message.subject, &body)
}

/// Return a code only when the message contains verification context and a
/// single unambiguous sequence of four to eight ASCII digits.
pub fn extract_code(subject: &str, body: &str) -> Option<String> {
    let text = format!("{subject}\n{body}");
    let lower = text.to_lowercase();
    let has_context = [
        "verification",
        "vérification",
        "verify",
        "vérifier",
        "security code",
        "code de sécurité",
        "one-time",
        "one time",
        "code de connexion",
        "sign-in code",
        "login code",
        "authentication",
        "authentification",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    if !has_context {
        return None;
    }
    let bytes = text.as_bytes();
    let mut candidate: Option<&str> = None;
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        let adjacent_letter = start
            .checked_sub(1)
            .is_some_and(|i| bytes[i].is_ascii_alphanumeric())
            || bytes.get(end).is_some_and(u8::is_ascii_alphanumeric);
        if (4..=8).contains(&(end - start)) && !adjacent_letter {
            let code = &text[start..end];
            if candidate.is_some_and(|previous| previous != code) {
                return None;
            }
            candidate = Some(code);
        }
        start = end;
    }
    candidate.map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{extract_code, extract_from_message, is_fresh, OtpCandidate};

    #[test]
    fn extracts_codes_only_with_context_and_without_ambiguity() {
        assert_eq!(
            extract_code("Code de vérification", "Votre code : 001234"),
            Some("001234".into())
        );
        assert_eq!(
            extract_code("Security code", "Use 123456 to sign in"),
            Some("123456".into())
        );
        assert_eq!(extract_code("Invoice", "Total 123456"), None);
        assert_eq!(
            extract_code("Verification", "Codes 123456 and 654321"),
            None
        );
        assert_eq!(extract_code("Verification", "Order A123456B"), None);
        assert_eq!(extract_code("Verification", "Code 123456789"), None);
    }

    #[test]
    fn html_only_and_freshness_are_supported() {
        let raw = b"Subject: Verification code\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Use <strong>001234</strong></p>";
        assert_eq!(extract_from_message(raw), Some("001234".into()));
        assert!(is_fresh(100, 279));
        assert!(!is_fresh(100, 280));
        assert!(!is_fresh(101, 100));
        assert!(!is_fresh(0, 100));
    }

    #[test]
    fn multipart_checks_both_alternatives_and_rejects_conflicting_codes() {
        let raw = b"Subject: Verification code\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nPlease view the HTML version.\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Use <b>001234</b></p>\r\n--b--\r\n";
        assert_eq!(extract_from_message(raw), Some("001234".into()));
        let conflict = b"Subject: Verification code\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nUse 654321\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Use 001234</p>\r\n--b--\r\n";
        assert_eq!(extract_from_message(conflict), None);
    }

    #[test]
    fn debug_never_displays_the_code() {
        let candidate = OtpCandidate {
            account_id: "a".into(),
            message_id: 1,
            sender: "sender@example.test".into(),
            recipient: "recipient@example.test".into(),
            received_at: 100,
            code: "123456".into(),
        };
        assert!(!format!("{candidate:?}").contains("123456"));
    }
}
