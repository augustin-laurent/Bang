//! Bang desktop mail client — entry point.

use std::sync::Arc;

use slint::{include_modules, ComponentHandle, Global};

include_modules!();

mod attachments;
mod bridge;
mod composer;
mod config;
mod i18n;
mod preview;
mod state;
#[cfg(all(test, feature = "preview-renderer"))]
mod ui_tests;
use i18n::tr;

fn main() -> Result<(), slint::PlatformError> {
    if std::env::args().any(|a| a == "--preview") {
        return preview::run();
    }
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

    // Apply the saved theme (light / dark / system).
    let config = config::Config::load();
    apply_theme(&ui, &config.theme);
    i18n::select(&config.language);
    state.refresh_folders(&ui, &store);
    state::load_drafts(&ui, &store);
    state::load_templates(&ui, &store);
    if ui.get_account_count() > 0 {
        state.selected_folder.store(
            bang_core::UNIFIED_INBOX_ID,
            std::sync::atomic::Ordering::SeqCst,
        );
        load_folder_view(&ui, &store, bang_core::UNIFIED_INBOX_ID);
    }

    // Resume syncing any saved accounts.
    if let Ok(accounts) = store.get_accounts() {
        for account in accounts {
            bridge.send(bang_core::UiCommand::AddAccount(account));
        }
    }

    wire_wizard(&ui, bridge.clone(), config);
    wire_selection(&ui, &store, &state, bridge.clone());
    wire_compose(&ui, &store, bridge.clone());
    wire_attachments(&ui, &store);
    wire_drafts(&ui, &store, bridge.clone());
    wire_settings(&ui, &store, &state, bridge);
    let weak = ui.as_weak();
    ui.window().on_close_requested({
        let weak = weak.clone();
        move || close_application(&weak)
    });
    // Ctrl+Q quits through the same close path as the window button.
    ui.on_quit({
        let weak = weak.clone();
        move || {
            let _ = close_application(&weak);
        }
    });

    // Follow changes to the desktop appearance while System is selected.
    let theme_watch = slint::Timer::default();
    let weak_theme = ui.as_weak();
    theme_watch.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_secs(3),
        move || {
            if let Some(ui) = weak_theme.upgrade() {
                if config::Config::load().theme == "system" {
                    let dark = system_dark_mode();
                    if dark != Theme::get(&ui).get_dark() {
                        apply_theme(&ui, "system");
                        composer::preferences(&ui);
                    }
                }
            }
        },
    );
    // When the window closes, `run` returns and the process exits, which
    // terminates the syncer threads (IMAP servers notice the TCP drop).
    // The command channel stays open because the UI closures hold senders.
    ui.run()
}

/// Close the application, asking the composer to close first if it is open.
///
/// Returns [`slint::CloseRequestResponse::KeepWindowShown`] while the composer
/// is still closing, otherwise hides the window (which ends the process).
fn close_application(weak: &slint::Weak<BangApp>) -> slint::CloseRequestResponse {
    if composer::is_open() {
        composer::request_application_close();
        slint::CloseRequestResponse::KeepWindowShown
    } else {
        if let Some(ui) = weak.upgrade() {
            ui.hide().ok();
        }
        slint::CloseRequestResponse::HideWindow
    }
}

/// Connect folder and message selection to the store and the mail core.
fn wire_selection(
    ui: &BangApp,
    store: &Arc<bang_core::Store>,
    state: &Arc<state::UiState>,
    bridge: bridge::Bridge,
) {
    ui.on_account_toggled({
        let store = store.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |id| {
            let id = id.to_string();
            {
                let mut collapsed = state.collapsed_accounts.lock().unwrap();
                if !collapsed.remove(&id) {
                    collapsed.insert(id);
                }
            }
            if let Some(ui) = weak.upgrade() {
                state.refresh_folders(&ui, &store);
            }
        }
    });
    ui.on_navigate_message({
        let weak = ui.as_weak();
        move |delta| {
            if let Some(ui) = weak.upgrade() {
                use slint::Model;
                let model = ui.get_messages();
                let pos = (0..model.row_count()).find(|&i| {
                    model
                        .row_data(i)
                        .map(|r| r.id == ui.get_selected_message())
                        .unwrap_or(false)
                });
                let next = match pos {
                    Some(i) => (i as i32 + delta)
                        .clamp(0, model.row_count().saturating_sub(1) as i32)
                        as usize,
                    None => 0,
                };
                if let Some(row) = model.row_data(next) {
                    ui.invoke_message_selected(row.id);
                }
            }
        }
    });
    ui.on_folder_selected({
        let store = store.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |id: i32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = i64::from(id);
            state
                .selected_folder
                .store(id, std::sync::atomic::Ordering::SeqCst);
            // Selecting a folder leaves search mode.
            ui.set_search_text("".into());
            state
                .search_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            ui.set_search_active(false);
            load_folder_view(&ui, &store, id);
        }
    });

    ui.on_search_edited({
        let store = store.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |text: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let query = text.to_string();
            if query.trim().is_empty() {
                // Leaving search mode: restore the folder view.
                state
                    .search_active
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                ui.set_search_active(false);
                let id = state
                    .selected_folder
                    .load(std::sync::atomic::Ordering::SeqCst);
                if id != -1 {
                    load_folder_view(&ui, &store, id);
                }
                return;
            }
            // Live results straight from the local store (FTS5).
            state
                .search_active
                .store(true, std::sync::atomic::Ordering::SeqCst);
            ui.set_search_active(true);
            ui.set_selected_message(-1);
            ui.set_message_sender("".into());
            ui.set_message_subject("".into());
            ui.set_message_date("".into());
            ui.set_message_body("".into());
            match store.search_all(&fts_query(&query), 100) {
                Ok(messages) => {
                    ui.set_folder_name(format!("{} {query}", tr("Search:")).into());
                    let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
                    ui.set_messages(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
                Err(e) => {
                    ui.set_status_message(format!("{} {e}", tr("Search failed:")).into());
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
            state
                .selected_message
                .store(id, std::sync::atomic::Ordering::SeqCst);
            ui.set_selected_message(id as i32);
            // Show the header now; the body arrives via MessageLoaded.
            if let Ok(Some(summary)) = store.get_message(id) {
                ui.set_message_sender(state::display_name(&summary.from).into());
                ui.set_message_subject(summary.subject.clone().into());
                ui.set_message_date(state::format_date(summary.date).into());
                // The body arrives via MessageLoaded (store cache or IMAP fetch).
                state::set_message_header(&ui, &summary);
                ui.set_message_loading(true);
                ui.set_message_body("".into());
                bridge.send(bang_core::UiCommand::OpenMessage {
                    account: bang_core::AccountId(summary.account_id),
                    message_id: id,
                });
            }
        }
    });

    ui.on_message_action({
        let store = store.clone();
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        move |id: i32, action: slint::SharedString| {
            let id = i64::from(id);
            // Reply / forward open the composer prefilled with a quote.
            if action == "reply" || action == "forward" {
                if let Some(ui) = weak.upgrade() {
                    if let Err(e) =
                        composer::reply_or_forward(&ui, store.clone(), bridge.clone(), id, action == "forward")
                    {
                        ui.set_status_message(format!("{} {e}", tr("Reply unavailable:")).into());
                    }
                }
                return;
            }
            // The row does not carry the account; look it up locally.
            let Ok(Some(summary)) = store.get_message(id) else {
                return;
            };
            let account = bang_core::AccountId(summary.account_id);
            let command = match action.as_str() {
                "star" => bang_core::UiCommand::SetFlags {
                    account,
                    message_ids: vec![id],
                    flags: bang_core::MessageFlags::FLAGGED,
                    add: !summary.flags.is_flagged(),
                },
                "read" => bang_core::UiCommand::SetFlags {
                    account,
                    message_ids: vec![id],
                    flags: bang_core::MessageFlags::SEEN,
                    add: summary.flags.is_unread(),
                },
                "delete" => bang_core::UiCommand::Delete {
                    account,
                    message_ids: vec![id],
                },
                _ => return,
            };
            bridge.send(command);
        }
    });
}

/// Load a folder's contents into the message list (local-cache read).
fn load_folder_view(ui: &BangApp, store: &Arc<bang_core::Store>, id: i64) {
    ui.set_selected_folder(id as i32);
    ui.set_selected_message(-1);
    // Clear the reading pane.
    ui.set_message_sender("".into());
    ui.set_message_subject("".into());
    ui.set_message_date("".into());
    ui.set_message_body("".into());
    ui.set_message_attachments(slint::ModelRc::new(slint::VecModel::from(Vec::new())));
    let messages = if id == bang_core::UNIFIED_INBOX_ID {
        ui.set_folder_name(tr("All Inboxes").into());
        store.get_unified_inbox(200)
    } else {
        match store.get_folder_by_id(id) {
            Ok(Some(folder)) => ui.set_folder_name(folder.name.into()),
            _ => ui.set_folder_name("".into()),
        }
        store.get_messages(id, 200)
    };
    match messages {
        Ok(messages) => {
            let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
            ui.set_messages(slint::ModelRc::new(slint::VecModel::from(rows)));
        }
        Err(e) => {
            ui.set_status_message(format!("{} {e}", tr("Folder load failed:")).into());
        }
    }
}

/// Turn free text into a safe FTS5 MATCH expression: each word becomes a
/// quoted token, joined as an implicit AND.
fn fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the desktop environment is in dark mode.
///
/// Queries GNOME (`gsettings`) then KDE (`kreadconfig5`) in turn and uses the
/// first tool that reports a value; this also covers compositors such as
/// Hyprland where `gsettings` is present. Falls back to `false` (light) when
/// no tool reports a value.
fn system_dark_mode() -> bool {
    if let Ok(out) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        let value = String::from_utf8_lossy(&out.stdout);
        if !value.trim().is_empty() {
            return value.contains("dark");
        }
    }
    if let Ok(out) = std::process::Command::new("kreadconfig5")
        .args(["--hierarchical", "false", "General", "ColorScheme"])
        .output()
    {
        let value = String::from_utf8_lossy(&out.stdout);
        if !value.trim().is_empty() {
            return value.to_ascii_lowercase().contains("dark");
        }
    }
    false
}

/// Apply a theme mode (`"light"`, `"dark"`, or `"system"`) to the UI.
fn apply_theme(ui: &BangApp, mode: &str) {
    let dark = match mode {
        "dark" => true,
        "light" => false,
        _ => system_dark_mode(),
    };
    Theme::get(ui).set_dark(dark);
    Theme::get(ui).invoke_sync_palette();
}

/// Build the settings dialog's account list from the store.
fn account_rows(store: &Arc<bang_core::Store>) -> Vec<AccountRow> {
    store
        .get_accounts()
        .unwrap_or_default()
        .into_iter()
        .map(|a| AccountRow {
            id: a.id.0.clone().into(),
            email: a.email.clone().into(),
        })
        .collect()
}

/// Connect the settings dialog: theme selection and account management.
fn wire_settings(
    ui: &BangApp,
    store: &Arc<bang_core::Store>,
    state: &Arc<state::UiState>,
    bridge: bridge::Bridge,
) {
    ui.on_settings_opened({
        let store = store.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let rows = account_rows(&store);
            ui.set_settings_accounts(slint::ModelRc::new(slint::VecModel::from(rows)));
            ui.set_settings_theme(config::Config::load().theme.into());
            ui.set_settings_language(config::Config::load().language.into());
            ui.set_settings_open(true);
        }
    });

    ui.on_settings_theme_selected({
        let weak = ui.as_weak();
        move |mode: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mode = mode.to_string();
            let mut config = config::Config::load();
            config.theme = mode.clone();
            if config.save().is_err() {
                ui.set_status_message(tr("Could not save settings").into());
                return;
            }
            apply_theme(&ui, &mode);
            ui.set_settings_theme(mode.into());
            composer::preferences(&ui);
        }
    });

    ui.on_settings_language_selected({
        let weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        move |mode| {
            if let Some(ui) = weak.upgrade() {
                let mut config = config::Config::load();
                config.language = mode.to_string();
                if config.save().is_err() {
                    ui.set_status_message(tr("Could not save settings").into());
                    return;
                }
                i18n::select(&config.language);
                ui.set_settings_language(mode);
                state.refresh_folders(&ui, &store);
                if state
                    .search_active
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    let query = ui.get_search_text();
                    if let Ok(messages) = store.search_all(&fts_query(query.as_str()), 100) {
                        ui.set_folder_name(format!("{} {query}", tr("Search:")).into());
                        let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
                        ui.set_messages(slint::ModelRc::new(slint::VecModel::from(rows)));
                    }
                } else {
                    state.reload_messages(&ui, &store);
                    if state
                        .selected_folder
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == bang_core::UNIFIED_INBOX_ID
                    {
                        ui.set_folder_name(tr("All Inboxes").into());
                    }
                }
                if let Ok(Some(summary)) = store.get_message(i64::from(ui.get_selected_message())) {
                    state::set_message_header(&ui, &summary);
                }
                composer::preferences(&ui);
            }
        }
    });
    ui.on_settings_account_remove({
        let store = store.clone();
        let state = state.clone();
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        move |id: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = id.to_string();
            // Delete from the store synchronously so the list refresh is
            // correct; the command below stops the account's syncer.
            let _ = store.delete_account(&id);
            bridge.send(bang_core::UiCommand::RemoveAccount(bang_core::AccountId(
                id,
            )));
            let rows = account_rows(&store);
            ui.set_settings_accounts(slint::ModelRc::new(slint::VecModel::from(rows)));
            state.refresh_folders(&ui, &store);
            state.selected_folder.store(
                bang_core::UNIFIED_INBOX_ID,
                std::sync::atomic::Ordering::SeqCst,
            );
            load_folder_view(&ui, &store, bang_core::UNIFIED_INBOX_ID);
        }
    });

    ui.on_settings_close({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_settings_open(false);
            }
        }
    });
}

/// Open the isolated rich composer, keeping an existing draft alive.
fn wire_compose(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: bridge::Bridge) {
    let weak = ui.as_weak();
    let store = store.clone();
    ui.on_compose_clicked(move || {
        if let Some(ui) = weak.upgrade() {
            if let Err(error) = composer::open(&ui, store.clone(), bridge.clone(), None) {
                ui.set_status_message(format!("{} {error}", tr("Compose unavailable:")).into());
            }
        }
    });
}

/// Connect attachment chips to preview and save.
fn wire_attachments(ui: &BangApp, store: &Arc<bang_core::Store>) {
    ui.on_attachment_clicked({
        let weak = ui.as_weak();
        let store = store.clone();
        move |index| {
            if let Some(ui) = weak.upgrade() {
                attachments::open(&ui, &store, index);
            }
        }
    });
    ui.on_attachment_save({
        let weak = ui.as_weak();
        let store = store.clone();
        move |dest| {
            if let Some(ui) = weak.upgrade() {
                attachments::save(&ui, &store, dest);
            }
        }
    });
}

/// Connect the drafts and templates dialogs to the store and the composer.
fn wire_drafts(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: bridge::Bridge) {
    ui.on_drafts_opened({
        let weak = ui.as_weak();
        let store = store.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                state::load_drafts(&ui, &store);
                ui.set_drafts_open(true);
            }
        }
    });
    ui.on_templates_opened({
        let weak = ui.as_weak();
        let store = store.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                state::load_templates(&ui, &store);
                ui.set_templates_open(true);
            }
        }
    });
    ui.on_draft_row_clicked({
        let weak = ui.as_weak();
        let store = store.clone();
        let bridge = bridge.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                ui.set_drafts_open(false);
                resume_draft(&ui, &store, &bridge, i64::from(id));
            }
        }
    });
    ui.on_draft_row_delete({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                bridge.send(bang_core::UiCommand::DeleteDraft { id: i64::from(id) });
                ui.set_status_message(tr("Draft deleted").into());
            }
        }
    });
    ui.on_template_row_clicked({
        let weak = ui.as_weak();
        let store = store.clone();
        let bridge = bridge.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                ui.set_templates_open(false);
                resume_template(&ui, &store, &bridge, i64::from(id));
            }
        }
    });
    ui.on_template_row_delete({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                bridge.send(bang_core::UiCommand::DeleteTemplate { id: i64::from(id) });
                ui.set_status_message(tr("Template deleted").into());
            }
        }
    });
    ui.on_drafts_dialog_close({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_drafts_open(false);
                ui.set_templates_open(false);
            }
        }
    });
}

/// Open a saved draft in the composer, then remove it from the list.
fn resume_draft(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: &bridge::Bridge, id: i64) {
    let Ok(Some(request)) = store.get_draft(id) else {
        return;
    };
    let prefill = composer::Prefill {
        from: Some(request.account.0.clone()),
        to: request.to.clone(),
        subject: request.subject.clone(),
        body_html: request.body_html.clone().unwrap_or_default(),
        in_reply_to: request.in_reply_to.clone(),
        references: request.references.clone(),
    };
    if let Err(e) = composer::open(ui, store.clone(), bridge.clone(), Some(prefill)) {
        ui.set_status_message(format!("{} {e}", tr("Compose unavailable:")).into());
        return;
    }
    bridge.send(bang_core::UiCommand::DeleteDraft { id });
}

/// Open a saved template in the composer (the template stays in the list).
fn resume_template(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: &bridge::Bridge, id: i64) {
    let Ok(Some(request)) = store.get_template(id) else {
        return;
    };
    let prefill = composer::Prefill {
        from: Some(request.account.0.clone()),
        to: request.to.clone(),
        subject: request.subject.clone(),
        body_html: request.body_html.clone().unwrap_or_default(),
        in_reply_to: None,
        references: None,
    };
    if let Err(e) = composer::open(ui, store.clone(), bridge.clone(), Some(prefill)) {
        ui.set_status_message(format!("{} {e}", tr("Compose unavailable:")).into());
    }
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
                ui.set_wizard_status(tr("OAuth client ID missing in config.toml").into());
                return;
            }
            ui.set_wizard_status(tr("Waiting for browser sign-in…").into());
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
                ui.set_wizard_status(tr("Email address is required").into());
                return;
            }
            if ui.get_imap_host().trim().is_empty()
                || ui.get_smtp_host().trim().is_empty()
                || ui.get_password().is_empty()
                || ui
                    .get_imap_port()
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .is_none()
                || ui
                    .get_smtp_port()
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .is_none()
            {
                ui.set_wizard_status(tr("Check the servers, ports and password.").into());
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
                    username: if ui.get_username().trim().is_empty() {
                        email.clone()
                    } else {
                        ui.get_username().to_string()
                    },
                    password: ui.get_password().to_string(),
                },
            };
            bridge.send(bang_core::UiCommand::AddAccount(config));
            ui.set_wizard_open(false);
            ui.set_wizard_status("".into());
            ui.set_password("".into());
        }
    });

    ui.on_wizard_cancel({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_wizard_open(false);
                ui.set_wizard_status("".into());
                ui.set_password("".into());
            }
        }
    });
}
