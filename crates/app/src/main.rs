//! Bang desktop mail client — entry point.

use std::sync::Arc;

use slint::{include_modules, ComponentHandle, Global};

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

    // Apply the saved theme (light / dark / system).
    let config = config::Config::load();
    apply_theme(&ui, &config.theme);
    ui.set_theme_label(theme_label(&config.theme).into());

    // Resume syncing any saved accounts.
    if let Ok(accounts) = store.get_accounts() {
        for account in accounts {
            bridge.send(bang_core::UiCommand::AddAccount(account));
        }
    }

    wire_wizard(&ui, bridge.clone(), config);
    wire_selection(&ui, &store, &state, bridge.clone());
    wire_compose(&ui, &store, bridge.clone());
    wire_theme(&ui);

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
            // Selecting a folder leaves search mode.
            ui.set_search_text("".into());
            state.search_active.store(false, std::sync::atomic::Ordering::SeqCst);
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
                state.search_active.store(false, std::sync::atomic::Ordering::SeqCst);
                let id = state.selected_folder.load(std::sync::atomic::Ordering::SeqCst);
                if id >= 0 {
                    load_folder_view(&ui, &store, id);
                }
                return;
            }
            // Live results straight from the local store (FTS5).
            state.search_active.store(true, std::sync::atomic::Ordering::SeqCst);
            ui.set_selected_message(-1);
            ui.set_message_sender("".into());
            ui.set_message_subject("".into());
            ui.set_message_date("".into());
            ui.set_message_body("".into());
            match store.search_all(&fts_query(&query), 100) {
                Ok(messages) => {
                    ui.set_folder_name(format!("Search: {query}").into());
                    let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
                    ui.set_messages(slint::ModelRc::new(slint::VecModel::from(rows)));
                }
                Err(e) => {
                    ui.set_status_message(format!("Search failed: {e}").into());
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
                // The body arrives via MessageLoaded (store cache or IMAP fetch).
                ui.set_message_body("Loading…".into());
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
        move |id: i32, action: slint::SharedString| {
            let id = i64::from(id);
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
    let messages = if id == bang_core::UNIFIED_INBOX_ID {
        ui.set_folder_name("Unified Inbox".into());
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
            ui.set_status_message(format!("Folder load failed: {e}").into());
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

/// Whether the desktop environment is in dark mode (GNOME or KDE).
///
/// Falls back to `false` (light) when the desktop is unknown or the query
/// tool is missing.
fn system_dark_mode() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if desktop.contains("GNOME") {
        if let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
        {
            return String::from_utf8_lossy(&out.stdout).contains("dark");
        }
    } else if desktop.contains("KDE") {
        if let Ok(out) = std::process::Command::new("kreadconfig5")
            .args(["--hierarchical", "false", "General", "ColorScheme"])
            .output()
        {
            return String::from_utf8_lossy(&out.stdout)
                .to_ascii_lowercase()
                .contains("dark");
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
}

/// Short label for the theme toggle button.
fn theme_label(mode: &str) -> &'static str {
    match mode {
        "light" => "☀",
        "dark" => "☾",
        _ => "⚙",
    }
}

// The open compose window, kept alive until it is replaced.
// Thread-local because Slint components are not `Sync`; every access
// happens on the Slint event loop thread.
std::thread_local! {
    static COMPOSE: std::cell::RefCell<Option<ComposeWindow>> =
        const { std::cell::RefCell::new(None) };
    /// Attachments staged for the open compose window.
    static COMPOSE_ATTACHMENTS: std::cell::RefCell<Vec<bang_core::OutgoingAttachment>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Connect the theme toggle button: cycle light → dark → system, persist, apply.
fn wire_theme(ui: &BangApp) {
    ui.on_theme_clicked({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let config = config::Config::load();
            let next = match config.theme.as_str() {
                "light" => "dark",
                "dark" => "system",
                _ => "light",
            };
            let mut config = config;
            config.theme = next.into();
            config.save();
            apply_theme(&ui, next);
            ui.set_theme_label(theme_label(next).into());
        }
    });
}

/// Guess a MIME content type from a file extension.
fn content_type_for(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Connect the compose button to a compose window and the mail core.
fn wire_compose(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: bridge::Bridge) {
    ui.on_compose_clicked({
        let store = store.clone();
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        move || {
            // The first saved account sends the message.
            let Ok(Some(account)) = store.get_accounts().map(|a| a.into_iter().next()) else {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_message("Add an account before composing".into());
                }
                return;
            };
            let Ok(compose) = ComposeWindow::new() else {
                return;
            };
            compose.set_from(account.email.clone().into());
            let weak_compose = compose.as_weak();

            compose.on_send({
                let bridge = bridge.clone();
                let account_id = account.id.clone();
                let from = account.email.clone();
                let weak_compose = weak_compose.clone();
                move || {
                    let Some(compose) = weak_compose.upgrade() else {
                        return;
                    };
                    let to = compose.get_to().to_string();
                    if to.trim().is_empty() {
                        return;
                    }
                    // Comma-separated address lists.
                    let addrs = |s: &str| {
                        s.split(',')
                            .map(|a| a.trim().to_string())
                            .filter(|a| !a.is_empty())
                            .collect()
                    };
                    let request = bang_core::SendRequest {
                        account: account_id.clone(),
                        from: from.clone(),
                        to: addrs(&to),
                        cc: addrs(&compose.get_cc().to_string()),
                        bcc: addrs(&compose.get_bcc().to_string()),
                        subject: compose.get_subject().to_string(),
                        body_text: compose.get_body().to_string(),
                        attachments: COMPOSE_ATTACHMENTS.with(|a| std::mem::take(&mut *a.borrow_mut())),
                        in_reply_to: None,
                        references: None,
                    };
                    bridge.send(bang_core::UiCommand::SendEmail(request));
                    // Dropping the handle closes the window.
                    COMPOSE.with(|c| {
                        c.take();
                    });
                }
            });

            compose.on_cancel({
                move || {
                    // Dropping the handle closes the window.
                    COMPOSE.with(|c| {
                        c.take();
                    });
                }
            });

            compose.on_attach_clicked({
                let weak_compose = weak_compose.clone();
                move || {
                    let Some(compose) = weak_compose.upgrade() else {
                        return;
                    };
                    let path = compose.get_attach_path().to_string();
                    if path.trim().is_empty() {
                        return;
                    }
                    let Ok(data) = std::fs::read(&path) else {
                        return;
                    };
                    let filename = path
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&path)
                        .to_string();
                    let attachment = bang_core::OutgoingAttachment {
                        filename,
                        content_type: content_type_for(&path),
                        data,
                    };
                    let names = COMPOSE_ATTACHMENTS.with(|a| {
                        a.borrow_mut().push(attachment);
                        a.borrow()
                            .iter()
                            .map(|x| x.filename.clone())
                            .collect::<Vec<_>>()
                            .join(", ")
                    });
                    compose.set_attachments(names.into());
                    compose.set_attach_path("".into());
                }
            });

            // Keep the window alive: the handle owns it. Send feedback
            // arrives in the main window via SendResult.
            COMPOSE.with(|c| *c.borrow_mut() = Some(compose));
            COMPOSE_ATTACHMENTS.with(|a| a.borrow_mut().clear());
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
