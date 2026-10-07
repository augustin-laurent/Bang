//! Bang desktop mail client — entry point.

use std::sync::Arc;

use slint::{include_modules, ComponentHandle};

include_modules!();

mod bridge;
mod config;
mod state;

fn main() -> Result<(), slint::PlatformError> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let dirs = directories::ProjectDirs::from("", "", "bang")
        .ok_or_else(|| slint::PlatformError::Other("no data directory".into()))?;
    std::fs::create_dir_all(dirs.data_dir()).ok();
    let store = Arc::new(
        bang_core::Store::open(&dirs.data_dir().join("bang.db"))
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?,
    );

    let ui = BangApp::new()?;
    let state = Arc::new(state::UiState::new());
    let bridge = bridge::Bridge::start(&ui, store.clone(), state.clone());

    // Resume syncing any saved accounts.
    if let Ok(accounts) = store.get_accounts() {
        for account in accounts {
            bridge.send(bang_core::UiCommand::AddAccount(account));
        }
    }

    wire_wizard(&ui, bridge.clone(), config::Config::load());
    wire_selection(&ui, &store, &state, bridge);

    // When the window closes, `run` returns and the process exits, which
    // terminates the syncer threads (IMAP servers notice the TCP drop).
    // The command channel stays open because the UI closures hold senders.
    ui.run()
}

/// Connect folder and message selection to the store and the mail core.
fn wire_selection(
    ui: &BangApp,
    store: &Arc<bang_core::Store>,
    state: &Arc<state::UiState>,
    bridge: bridge::Bridge,
) {
    ui.on_folder_selected({
        let store = store.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |id: i32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = i64::from(id);
            state.selected_folder.store(id, std::sync::atomic::Ordering::SeqCst);
            ui.set_selected_folder(id as i32);
            ui.set_selected_message(-1);
            // Clear the reading pane.
            ui.set_message_sender("".into());
            ui.set_message_subject("".into());
            ui.set_message_date("".into());
            ui.set_message_body("".into());
            match store.get_folder_by_id(id) {
                Ok(Some(folder)) => ui.set_folder_name(folder.name.into()),
                _ => ui.set_folder_name("".into()),
            }
            // Folder contents come straight from the local store.
            match store.get_messages(id, 200) {
                Ok(messages) => {
                    let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
                    ui.set_messages(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
                Err(e) => {
                    ui.set_status_message(format!("Folder load failed: {e}").into());
                }
            }
        }
    });

    ui.on_message_selected({
        let store = store.clone();
        let state = state.clone();
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        move |id: i32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = i64::from(id);
            state.selected_message.store(id, std::sync::atomic::Ordering::SeqCst);
            ui.set_selected_message(id as i32);
            // Show the header now; the body arrives via MessageLoaded.
            if let Ok(Some(summary)) = store.get_message(id) {
                ui.set_message_sender(state::display_name(&summary.from).into());
                ui.set_message_subject(summary.subject.clone().into());
                ui.set_message_date(state::format_date(summary.date).into());
                ui.set_message_body("".into());
                bridge.send(bang_core::UiCommand::OpenMessage {
                    account: bang_core::AccountId(summary.account_id),
                    message_id: id,
                });
            }
        }
    });
}

/// Connect the account wizard's callbacks to the mail core.
fn wire_wizard(ui: &BangApp, bridge: bridge::Bridge, config: config::Config) {
    ui.on_wizard_oauth_clicked({
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        move |provider: slint::SharedString| {
            let provider = match provider.as_str() {
                "google" => bang_core::OAuthProvider::Google,
                "microsoft" => bang_core::OAuthProvider::Microsoft,
                _ => return,
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let client_id = match provider {
                bang_core::OAuthProvider::Google => config.oauth.google.client_id.clone(),
                bang_core::OAuthProvider::Microsoft => config.oauth.microsoft.client_id.clone(),
            };
            if client_id.is_empty() {
                ui.set_wizard_status(
                    format!("No OAuth client id for {provider:?} in config.toml").into(),
                );
                return;
            }
            ui.set_wizard_status("Waiting for browser sign-in…".into());
            bridge.send(bang_core::UiCommand::OAuthLogin {
                provider,
                client_id,
            });
        }
    });

    ui.on_wizard_add_generic({
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let email = ui.get_email().to_string();
            if email.is_empty() {
                ui.set_wizard_status("Email address is required".into());
                return;
            }
            let starttls = ui.get_starttls();
            let tls = |starttls: bool| {
                if starttls {
                    bang_core::TlsMode::Starttls
                } else {
                    bang_core::TlsMode::Tls
                }
            };
            let config = bang_core::AccountConfig {
                id: bang_core::AccountId(email.clone()),
                email: email.clone(),
                imap: bang_core::ImapConfig {
                    host: ui.get_imap_host().to_string(),
                    port: ui.get_imap_port().parse().unwrap_or(993),
                    tls: tls(starttls),
                },
                smtp: bang_core::SmtpConfig {
                    host: ui.get_smtp_host().to_string(),
                    port: ui.get_smtp_port().parse().unwrap_or(465),
                    tls: tls(starttls),
                },
                auth: bang_core::AuthMethod::Password {
                    username: ui.get_username().to_string(),
                    password: ui.get_password().to_string(),
                },
            };
            bridge.send(bang_core::UiCommand::AddAccount(config));
            ui.set_wizard_open(false);
            ui.set_wizard_status("".into());
        }
    });

    ui.on_wizard_cancel({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_wizard_open(false);
                ui.set_wizard_status("".into());
            }
        }
    });
}
