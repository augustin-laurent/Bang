//! Local message categories and user labels, independent of IMAP folders.
use serde::{Deserialize, Serialize};

/// The four mutually exclusive automatic categories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailCategory {
    /// Personal mail or insufficient classification signals.
    #[default]
    Primary,
    /// Receipts, orders, security confirmations and deliveries.
    Transactions,
    /// Newsletters and recurring notifications.
    Updates,
    /// Commercial offers.
    Promotions,
}
impl MailCategory {
    /// Stable database/UI identifier.
    pub fn key(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Transactions => "transactions",
            Self::Updates => "updates",
            Self::Promotions => "promotions",
        }
    }
    /// Parse an identifier; unknown values are rejected.
    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "primary" => Self::Primary,
            "transactions" => Self::Transactions,
            "updates" => Self::Updates,
            "promotions" => Self::Promotions,
            _ => return None,
        })
    }
}

/// Small header-only hints. Never contains a message body or a remote URL.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClassificationSignals {
    /// A mailing-list identifier is present.
    pub mailing_list: bool,
    /// A list unsubscribe header is present.
    pub unsubscribe: bool,
    /// A bulk/list precedence or automated sender header is present.
    pub automated: bool,
}
impl ClassificationSignals {
    /// Read only the header section, including folded fields.
    pub fn from_headers(raw: &[u8]) -> Self {
        let mut result = Self::default();
        let mut fields: Vec<(String, String)> = Vec::new();
        let end = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .or_else(|| raw.windows(2).position(|w| w == b"\n\n"))
            .unwrap_or(raw.len());
        for line in String::from_utf8_lossy(&raw[..end]).lines() {
            if line.is_empty() {
                break;
            }
            if line.starts_with([' ', '\t']) {
                if let Some((_, value)) = fields.last_mut() {
                    value.push(' ');
                    value.push_str(line.trim());
                }
            } else if let Some((name, value)) = line.split_once(':') {
                fields.push((name.to_ascii_lowercase(), value.trim().to_ascii_lowercase()));
            }
        }
        for (name, value) in fields {
            match name.as_str() {
                "list-id" => result.mailing_list = !value.is_empty(),
                "list-unsubscribe" => result.unsubscribe = !value.is_empty(),
                "precedence" if ["bulk", "list", "junk"].contains(&value.as_str()) => {
                    result.automated = true
                }
                "auto-submitted" if !value.is_empty() && value != "no" => result.automated = true,
                _ => (),
            }
        }
        result
    }
}

/// Shared identity for avatars and sender-level category rules.
pub fn sender_key(address: &str) -> String {
    address.trim().to_lowercase()
}

fn words(text: &str) -> String {
    let normalized: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c if c.is_alphanumeric() => c,
            _ => ' ',
        })
        .collect();
    format!(
        " {} ",
        normalized.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// Conservative deterministic FR/EN rules, ordered by specificity.
pub fn classify(sender: &str, subject: &str, hints: &ClassificationSignals) -> MailCategory {
    let text = words(&format!("{sender} {subject}"));
    let has = |terms: &[&str]| terms.iter().any(|t| text.contains(&format!(" {t} ")));
    if has(&[
        "invoice",
        "receipt",
        "payment confirmation",
        "order confirmation",
        "order shipped",
        "delivery",
        "tracking",
        "verification code",
        "security code",
        "password reset",
        "facture",
        "recu",
        "confirmation de commande",
        "commande confirmee",
        "livraison",
        "colis",
        "paiement",
        "code de verification",
        "code de securite",
        "billet",
        "reservation",
    ]) {
        MailCategory::Transactions
    } else if has(&[
        "sale",
        "discount",
        "special offer",
        "exclusive offer",
        "deal",
        "promotion",
        "promotions",
        "promo",
        "soldes",
        "reduction",
        "offre speciale",
        "offre exclusive",
        "offres",
        "black friday",
        "coupon",
        "economisez",
        "save now",
    ]) {
        MailCategory::Promotions
    } else if hints.mailing_list
        || hints.unsubscribe
        || hints.automated
        || has(&[
            "newsletter",
            "digest",
            "weekly",
            "notification",
            "notifications",
            "actualites",
            "lettre d information",
            "news",
            "noreply",
            "no reply",
        ])
    {
        MailCategory::Updates
    } else {
        MailCategory::Primary
    }
}

/// A local label shared by all accounts in this installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    /// Local identifier.
    pub id: i64,
    /// User-defined display name.
    pub name: String,
    /// Opaque RGB color, in #RRGGBB format.
    pub color: String,
}

/// Current category and labels for a message.
#[derive(Debug, Clone)]
pub struct MessageOrganization {
    /// Effective category, including user rules.
    pub category: MailCategory,
    /// Whether a message-specific correction exists.
    pub message_override: bool,
    /// Whether a sender rule exists.
    pub sender_override: bool,
    /// Labels assigned to the message.
    pub labels: Vec<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bilingual_rules_and_precedence() {
        let list = ClassificationSignals {
            mailing_list: true,
            ..Default::default()
        };
        for subject in [
            "Votre facture",
            "Order confirmation — special offer",
            "Code de vérification",
            "Votre colis est arrivé",
        ] {
            assert_eq!(
                classify("shop@example.com", subject, &list),
                MailCategory::Transactions
            );
        }
        assert_eq!(
            classify("shop@example.com", "Offre spéciale", &list),
            MailCategory::Promotions
        );
        assert_eq!(
            classify("alice@example.com", "Compte rendu", &list),
            MailCategory::Updates
        );
        assert_eq!(
            classify(
                "alice@example.com",
                "On se voit demain ?",
                &Default::default()
            ),
            MailCategory::Primary
        );
        assert_eq!(
            classify(
                "alice@example.com",
                "Wholesale meeting",
                &Default::default()
            ),
            MailCategory::Primary
        );
    }
    #[test]
    fn folded_headers_stop_before_body() {
        let hints = ClassificationSignals::from_headers(b"List-Id:\r\n <weekly.example.com>\r\nAuto-Submitted: no\r\n\r\nList-Unsubscribe: fake");
        assert!(hints.mailing_list);
        assert!(!hints.unsubscribe);
        assert!(!hints.automated);
    }
}
