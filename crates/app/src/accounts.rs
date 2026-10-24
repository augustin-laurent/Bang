//! Account setup, provider presets and input validation.
use crate::{bridge::Bridge, config::Config, i18n::tr, BangApp};
use slint::ComponentHandle;

pub(crate) fn select_provider(ui: &BangApp, provider: &str) {
    ui.set_wizard_status("".into());
    ui.set_password("".into());
    ui.set_wizard_provider(provider.into());
    ui.set_wizard_manual(matches!(provider, "manual" | "icloud"));
    ui.set_username("".into());
    ui.set_starttls(false);
    ui.set_smtp_starttls(provider == "icloud");
    ui.set_imap_port("993".into());
    ui.set_smtp_port(if provider == "icloud" { "587" } else { "465" }.into());
    ui.set_imap_host(
        if provider == "icloud" {
            "imap.mail.me.com"
        } else {
            ""
        }
        .into(),
    );
    ui.set_smtp_host(
        if provider == "icloud" {
            "smtp.mail.me.com"
        } else {
            ""
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
        id: bang_core::AccountId(email.clone()),
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
    let weak = ui.as_weak();
    ui.on_wizard_provider_selected(move |provider| {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_wizard_busy() {
                select_provider(&ui, &provider);
            }
        }
    });
    ui.on_wizard_oauth_clicked({
        let weak = ui.as_weak(); let bridge = bridge.clone();
        move |provider: slint::SharedString| {
            let Some(ui) = weak.upgrade() else { return; };
            if ui.get_wizard_busy() { return; }
            let cfg = Config::load();
            let (provider, settings, missing) = match provider.as_str() {
                "google" => (bang_core::OAuthProvider::Google, cfg.oauth.google, "Google sign-in is not configured. Follow docs/ACCOUNTS.md to add your desktop OAuth client."),
                "microsoft" => (bang_core::OAuthProvider::Microsoft, cfg.oauth.microsoft, "Microsoft sign-in is not configured. Follow docs/ACCOUNTS.md to add your desktop OAuth client."),
                _ => return,
            };
            let email = ui.get_email().trim().to_owned();
            if (provider == bang_core::OAuthProvider::Microsoft || !email.is_empty()) && !valid_email(&email) {
                ui.set_wizard_status(tr("Enter a valid email address.").into()); return;
            }
            if settings.client_id.trim().is_empty() { ui.set_wizard_status(tr(missing).into()); return; }
            let attempt = begin(&ui, "Waiting for browser sign-in…");
            bridge.send(bang_core::UiCommand::OAuthLogin {
                provider, client_id: settings.client_id.trim().into(),
                client_secret: (!settings.client_secret.is_empty()).then_some(settings.client_secret), email, attempt,
            });
        }
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
