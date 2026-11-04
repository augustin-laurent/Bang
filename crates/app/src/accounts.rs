//! Account setup, provider presets and input validation.
use crate::{bridge::Bridge, config::Config, i18n::tr, BangApp};
use slint::ComponentHandle;

pub(crate) fn select_provider(ui: &BangApp, provider: &str) {
    ui.set_wizard_status("".into());
    ui.set_password("".into());
    ui.set_wizard_provider(provider.into());
    ui.set_wizard_manual(matches!(provider, "manual" | "icloud"));
    ui.set_wizard_advanced(false);
    ui.set_wizard_settings_ready(provider == "icloud");
    ui.set_wizard_storage_error(false);
    ui.set_wizard_reconnect_id("".into());
    ui.set_username("".into());
    ui.set_smtp_username("".into());
    ui.set_starttls(false);
    ui.set_smtp_starttls(provider == "icloud");
    let (imap, smtp) = bang_core::providers::icloud_endpoints();
    ui.set_imap_port("993".into());
    ui.set_smtp_port(
        if provider == "icloud" {
            smtp.port.to_string()
        } else {
            "465".into()
        }
        .into(),
    );
    ui.set_imap_host(
        if provider == "icloud" {
            imap.host
        } else {
            String::new()
        }
        .into(),
    );
    ui.set_smtp_host(
        if provider == "icloud" {
            smtp.host
        } else {
            String::new()
        }
        .into(),
    );
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && !local.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(char::is_whitespace)
}

/// Open the existing account's sign-in flow without deleting its cache.
pub(crate) fn reconnect(ui: &BangApp, record: &bang_core::StoredAccount) {
    let provider = match &record.auth {
        bang_core::StoredAuth::OAuth2 {
            provider: bang_core::OAuthProvider::Google,
            ..
        } => "google",
        bang_core::StoredAuth::OAuth2 { .. } => "microsoft",
        _ if record.imap.host == "imap.mail.me.com" => "icloud",
        _ => "manual",
    };
    select_provider(ui, provider);
    ui.set_email(record.email.clone().into());
    ui.set_wizard_reconnect_id(record.id.0.clone().into());
    ui.set_imap_host(record.imap.host.clone().into());
    ui.set_imap_port(record.imap.port.to_string().into());
    ui.set_smtp_host(record.smtp.host.clone().into());
    ui.set_smtp_port(record.smtp.port.to_string().into());
    ui.set_starttls(record.imap.tls == bang_core::TlsMode::Starttls);
    ui.set_smtp_starttls(record.smtp.tls == bang_core::TlsMode::Starttls);
    if let bang_core::StoredAuth::Password {
        username,
        smtp_username,
        ..
    } = &record.auth
    {
        ui.set_username(username.clone().into());
        ui.set_smtp_username(smtp_username.clone().into());
        ui.set_wizard_settings_ready(true);
    }
    ui.set_settings_open(false);
    ui.set_wizard_open(true);
}

fn manual_config(ui: &BangApp) -> Result<bang_core::AccountConfig, &'static str> {
    let email = ui.get_email().trim().to_owned();
    if !valid_email(&email) {
        return Err("Enter a valid email address.");
    }
    let port = |value: slint::SharedString| value.trim().parse::<u16>().ok().filter(|p| *p > 0);
    let imap_port = port(ui.get_imap_port());
    let smtp_port = port(ui.get_smtp_port());
    if ui.get_imap_host().trim().is_empty()
        || ui.get_smtp_host().trim().is_empty()
        || ui.get_password().is_empty()
        || imap_port.is_none()
        || smtp_port.is_none()
    {
        return Err("Check the servers, ports and password.");
    }
    let tls = |starttls| {
        if starttls {
            bang_core::TlsMode::Starttls
        } else {
            bang_core::TlsMode::Tls
        }
    };
    Ok(bang_core::AccountConfig {
        id: bang_core::AccountId(if ui.get_wizard_reconnect_id().is_empty() {
            email.clone()
        } else {
            ui.get_wizard_reconnect_id().into()
        }),
        email: email.clone(),
        imap: bang_core::ImapConfig {
            host: ui.get_imap_host().trim().into(),
            port: imap_port.unwrap(),
            tls: tls(ui.get_starttls()),
        },
        smtp: bang_core::SmtpConfig {
            host: ui.get_smtp_host().trim().into(),
            port: smtp_port.unwrap(),
            tls: tls(ui.get_smtp_starttls()),
        },
        auth: bang_core::AuthMethod::Password {
            smtp_username: if ui.get_smtp_username().trim().is_empty() {
                email.clone()
            } else {
                ui.get_smtp_username().trim().into()
            },
            username: if ui.get_username().trim().is_empty() {
                email
            } else {
                ui.get_username().trim().into()
            },
            password: ui.get_password().into(),
        },
    })
}

fn begin(ui: &BangApp, status: &str) -> i32 {
    let attempt = ui.get_wizard_attempt().wrapping_add(1);
    ui.set_wizard_attempt(attempt);
    ui.set_wizard_busy(true);
    ui.set_wizard_status(tr(status).into());
    attempt
}

pub(crate) fn wire(ui: &BangApp, bridge: Bridge) {
    ui.set_google_available(
        Config::load()
            .oauth
            .registration(bang_core::OAuthProvider::Google)
            .is_some(),
    );
    ui.set_microsoft_available(
        Config::load()
            .oauth
            .registration(bang_core::OAuthProvider::Microsoft)
            .is_some(),
    );
    ui.on_wizard_email_edited({
        let weak = ui.as_weak();
        move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.get_wizard_provider() == "manual" && !ui.get_wizard_busy() {
                    ui.set_wizard_settings_ready(false);
                    ui.set_wizard_advanced(false);
                    ui.set_password("".into());
                    ui.set_username("".into());
                    ui.set_smtp_username("".into());
                    ui.set_imap_host("".into());
                    ui.set_smtp_host("".into());
                    ui.set_wizard_status("".into());
                }
            }
        }
    });
    let weak = ui.as_weak();
    ui.on_wizard_provider_selected(move |provider| {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_wizard_busy() {
                select_provider(&ui, &provider);
            }
        }
    });
    ui.on_wizard_oauth_clicked({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |provider: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.get_wizard_busy() {
                return;
            }
            let cfg = Config::load();
            let provider = match provider.as_str() {
                "google" => bang_core::OAuthProvider::Google,
                "microsoft" => bang_core::OAuthProvider::Microsoft,
                _ => return,
            };
            let Some(registration) = cfg.oauth.registration(provider) else {
                ui.set_wizard_status(tr("This provider is unavailable in this build.").into());
                return;
            };
            let email = ui.get_email().trim().to_owned();
            if (provider == bang_core::OAuthProvider::Microsoft || !email.is_empty())
                && !valid_email(&email)
            {
                ui.set_wizard_status(tr("Enter a valid email address.").into());
                return;
            }
            let attempt = begin(&ui, "Waiting for browser sign-in…");
            bridge.send(bang_core::UiCommand::OAuthLogin {
                replace: (!ui.get_wizard_reconnect_id().is_empty())
                    .then(|| bang_core::AccountId(ui.get_wizard_reconnect_id().into())),
                provider,
                client_id: registration.client_id,
                client_secret: registration.desktop_parameter,
                email,
                attempt,
            });
        }
    });
    ui.on_wizard_discover({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.get_wizard_busy() {
                return;
            }
            let email = ui.get_email().trim().to_owned();
            if !valid_email(&email) {
                ui.set_wizard_status(tr("Enter a valid email address.").into());
                return;
            }
            let attempt = begin(&ui, "Finding mail settings…");
            bridge.send(bang_core::UiCommand::DiscoverSettings { email, attempt });
        }
    });
    ui.on_wizard_storage_choice({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |session_only| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.get_wizard_busy() {
                return;
            }
            ui.set_wizard_busy(true);
            ui.set_wizard_status(tr("Saving account…").into());
            bridge.send(bang_core::UiCommand::RetryAccountStorage {
                attempt: ui.get_wizard_attempt(),
                session_only,
            });
        }
    });
    ui.on_wizard_app_password(|| {
        let _ = webbrowser::open("https://support.apple.com/102654");
    });
    ui.on_wizard_add_generic({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.get_wizard_busy() {
                return;
            }
            match manual_config(&ui) {
                Ok(config) => {
                    let attempt = begin(&ui, "Checking incoming and outgoing servers…");
                    bridge.send(bang_core::UiCommand::ConnectAccount { attempt, config });
                }
                Err(error) => ui.set_wizard_status(tr(error).into()),
            }
        }
    });
    let weak = ui.as_weak();
    ui.on_wizard_cancel(move || {
        bridge.send(bang_core::UiCommand::CancelAccountSetup);
        if let Some(ui) = weak.upgrade() {
            ui.set_wizard_attempt(ui.get_wizard_attempt().wrapping_add(1));
            ui.set_wizard_busy(false);
            ui.set_wizard_open(false);
            ui.set_wizard_manual(false);
            ui.set_wizard_provider("".into());
            ui.set_wizard_status("".into());
            ui.set_password("".into());
            ui.set_wizard_storage_error(false);
            ui.set_wizard_reconnect_id("".into());
            if ui.get_welcome_open() {
                ui.invoke_welcome_focus();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "preview-renderer")]
    #[test]
    fn manual_configuration_keeps_imap_and_smtp_encryption_independent() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = crate::BangApp::new().unwrap();
        super::select_provider(&ui, "icloud");
        ui.set_email(" augustin@example.test ".into());
        ui.set_password(" app-password ".into());
        let cfg = super::manual_config(&ui).unwrap();
        assert_eq!(cfg.email, "augustin@example.test");
        assert_eq!(cfg.imap.tls, bang_core::TlsMode::Tls);
        assert_eq!(cfg.smtp.tls, bang_core::TlsMode::Starttls);
        assert_eq!(cfg.smtp.port, 587);
        if let bang_core::AuthMethod::Password { password, .. } = cfg.auth {
            assert_eq!(password, " app-password ");
        }
        ui.set_imap_port("0".into());
        assert!(super::manual_config(&ui).is_err());
        super::select_provider(&ui, "manual");
        assert_eq!(ui.get_password(), "");
        assert_eq!(ui.get_imap_host(), "");
        assert!(!ui.get_smtp_starttls());
    }

    #[test]
    fn email_validation_rejects_incomplete_or_multiple_addresses() {
        for email in [
            "",
            "a@",
            "@example.com",
            "a@example",
            "a@.com",
            "a@b.",
            "a b@example.com",
            "a@b@example.com",
        ] {
            assert!(!super::valid_email(email), "{email}");
        }
        assert!(super::valid_email("augustin+mail@example.com"));
    }
}
