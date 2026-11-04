//! Publisher registrations and known mail endpoints; no confidential keys.
use crate::{ImapConfig, OAuthProvider, OAuthRegistration, SmtpConfig, TlsMode};

/// Public registration bundled with Google desktop builds.
pub const GOOGLE_CLIENT_ID: &str =
    "724511987587-e9iftfae1c70qmvjhgl8dkn95ui0rg1n.apps.googleusercontent.com";

/// Public Microsoft registration for the native desktop client.
pub const MICROSOFT_CLIENT_ID: &str = "17d82cc7-8458-43c3-a8c9-19f737d366fe";

/// Publisher-provided native registration, optionally set by the release builder.
pub fn registration(provider: OAuthProvider) -> Option<OAuthRegistration> {
    let (id, desktop_parameter) = match provider {
        OAuthProvider::Google => {
            let id = option_env!("BANG_GOOGLE_CLIENT_ID").unwrap_or(GOOGLE_CLIENT_ID);
            let parameter = option_env!("BANG_GOOGLE_DESKTOP_PARAMETER").filter(|v| !v.is_empty());
            // Keep an incomplete publisher registration unavailable.
            parameter?;
            (id, parameter)
        }
        OAuthProvider::Microsoft => (
            option_env!("BANG_MICROSOFT_CLIENT_ID").unwrap_or(MICROSOFT_CLIENT_ID),
            None,
        ),
    };
    (!id.trim().is_empty()).then(|| OAuthRegistration {
        client_id: id.trim().into(),
        desktop_parameter: desktop_parameter
            .filter(|v| !v.is_empty())
            .map(str::to_owned),
    })
}

/// Human-readable provider name.
pub fn label(provider: OAuthProvider) -> &'static str {
    match provider {
        OAuthProvider::Google => "Google",
        OAuthProvider::Microsoft => "Microsoft",
    }
}

/// Incoming and outgoing endpoints for a browser-authorized account.
pub fn endpoints(provider: OAuthProvider) -> (ImapConfig, SmtpConfig) {
    match provider {
        OAuthProvider::Google => (
            ImapConfig {
                host: "imap.gmail.com".into(),
                port: 993,
                tls: TlsMode::Tls,
            },
            SmtpConfig {
                host: "smtp.gmail.com".into(),
                port: 465,
                tls: TlsMode::Tls,
            },
        ),
        OAuthProvider::Microsoft => (
            ImapConfig {
                host: "outlook.office365.com".into(),
                port: 993,
                tls: TlsMode::Tls,
            },
            SmtpConfig {
                host: "smtp.office365.com".into(),
                port: 587,
                tls: TlsMode::Starttls,
            },
        ),
    }
}

/// Apple's fixed IMAP and SMTP settings for an app-password account.
pub fn icloud_endpoints() -> (ImapConfig, SmtpConfig) {
    (
        ImapConfig {
            host: "imap.mail.me.com".into(),
            port: 993,
            tls: TlsMode::Tls,
        },
        SmtpConfig {
            host: "smtp.mail.me.com".into(),
            port: 587,
            tls: TlsMode::Starttls,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn microsoft_registration_uses_a_public_client_without_a_secret() {
        let registration = registration(OAuthProvider::Microsoft)
            .expect("Microsoft desktop registration must be bundled");
        assert_eq!(
            registration.client_id,
            option_env!("BANG_MICROSOFT_CLIENT_ID").unwrap_or(MICROSOFT_CLIENT_ID)
        );
        assert!(registration.desktop_parameter.is_none());
        assert_eq!(MICROSOFT_CLIENT_ID, "17d82cc7-8458-43c3-a8c9-19f737d366fe");
    }

    #[test]
    fn clean_install_has_google_and_encrypted_presets() {
        let google = registration(OAuthProvider::Google);
        assert_eq!(
            google.is_some(),
            option_env!("BANG_GOOGLE_DESKTOP_PARAMETER").is_some_and(|v| !v.is_empty())
        );
        if let Some(registration) = google {
            assert!(!registration.client_id.is_empty());
            assert!(registration.desktop_parameter.is_some());
        }
        for p in [OAuthProvider::Google, OAuthProvider::Microsoft] {
            let (imap, smtp) = endpoints(p);
            assert!(!matches!(imap.tls, TlsMode::Plaintext));
            assert!(!matches!(smtp.tls, TlsMode::Plaintext));
        }
    }
}
