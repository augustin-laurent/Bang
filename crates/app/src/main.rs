//! Bang desktop mail client — entry point.

use std::sync::Arc;

use chrono::TimeZone;
use slint::{include_modules, ComponentHandle, Global};

include_modules!();

mod accounts;
mod attachments;
mod avatars;
mod bridge;
mod composer;
mod config;
mod gtk_host;
mod i18n;
mod organization;
mod preview;
mod reader;
mod state;
#[cfg(all(test, feature = "preview-renderer"))]
mod ui_tests;
mod welcome;
use i18n::tr;

fn main() -> Result<(), slint::PlatformError> {
    if std::env::args().any(|a| a == "--check-oauth-release") {
        match bang_core::providers::registration(bang_core::OAuthProvider::Google) {
            Some(google) if google.client_id.ends_with(".apps.googleusercontent.com") => {
                println!("Google Desktop registration bundled: {}", google.client_id);
            }
            _ => println!("Google: not configured; provider disabled"),
        }
        println!(
            "Microsoft: {}",
            if bang_core::providers::registration(bang_core::OAuthProvider::Microsoft).is_some() {
                "public registration bundled"
            } else {
                "deferred; provider disabled"
            }
        );
        println!("Publisher must verify production consent and perform a live browser login before release.");
        return Ok(());
    }
    if std::env::args().any(|a| a == "--preview") {
        return preview::run();
    }
    gtk::glib::set_prgname(Some("bang"));
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let dirs = directories::ProjectDirs::from("", "", "bang")
        .ok_or_else(|| slint::PlatformError::Other("no data directory".into()))?;
    let store = Arc::new(
        bang_core::Store::open(&dirs.data_dir().join("bang.db"))
            .map_err(|e| slint::PlatformError::Other(e.to_string()))?,
    );

    gtk_host::init()?;
    let ui = BangApp::new()?;
    let state = Arc::new(state::UiState::new());
    let bridge = bridge::Bridge::start(&ui, store.clone(), state.clone());

    // Apply the saved theme (light / dark / system).
    let config = config::Config::load();
    ui.set_online_avatars(config.online_avatars);
    bridge.send(bang_core::UiCommand::SetOnlineAvatars(
        config.online_avatars,
    ));
    avatars::wire(&ui, bridge.clone());
    organization::wire(&ui, &store, &state, bridge.clone());
    apply_theme(&ui, &config.theme);
    i18n::select(&config.language);
    state.refresh_folders(&ui, &store);
    state::load_drafts(&ui, &store);
    state::load_templates(&ui, &store);
    state::load_sends(&ui, &store);
    if ui.get_account_count() > 0 {
        state.selected_folder.store(
            bang_core::UNIFIED_INBOX_ID,
            std::sync::atomic::Ordering::SeqCst,
        );
        load_folder_view(&ui, &store, bang_core::UNIFIED_INBOX_ID);
    }

    // Only the core resolves credentials and migrates old account records.
    bridge.send(bang_core::UiCommand::RestoreAccounts);

    gtk_host::attach(&ui, Some(store.clone()), Some(bridge.clone()));
    accounts::wire(&ui, bridge.clone());
    wire_selection(&ui, &store, &state, bridge.clone());
    wire_compose(&ui, &store, bridge.clone());
    wire_attachments(&ui, &store);
    wire_drafts(&ui, &store, bridge.clone());
    wire_settings(&ui, &store, &state, bridge);
    welcome::wire(
        &ui,
        dirs.config_dir().join("config.toml"),
        ui.get_account_count() > 0,
    );
    gtk_host::watch_welcome_motion(&ui);
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
    use slint::Model;
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
                    ui.invoke_reveal_message(next as i32);
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
            state.selected_messages.lock().unwrap().clear();
            state
                .selection_anchor
                .store(-1, std::sync::atomic::Ordering::SeqCst);
            ui.set_selection_count(0);
            state
                .selected_folder
                .store(id, std::sync::atomic::Ordering::SeqCst);
            ui.set_selected_label(-1);
            ui.set_selected_category("".into());
            state
                .selected_message
                .store(-1, std::sync::atomic::Ordering::SeqCst);
            // Selecting a folder leaves search mode.
            ui.set_search_text("".into());
            state
                .search_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
            ui.set_search_active(false);
            load_folder_view(&ui, &store, id);
        }
    });

    ui.on_folder_context({
        let store = store.clone();
        let weak = ui.as_weak();
        move |id: i32, x: f32, y: f32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(Some(folder)) = store.get_folder_by_id(i64::from(id)) else {
                return;
            };
            // The virtual unified row and account headers are filtered in the
            // sidebar. INBOX can be marked read / used to create a folder,
            // while the UI hides its rename/delete actions.
            ui.set_menu_folder(id);
            ui.set_menu_folder_kind(bang_core::message::folder_kind(&folder.path).into());
            ui.set_menu_folder_name(folder.name.into());
            ui.set_menu_folder_inbox(folder.path.eq_ignore_ascii_case("INBOX"));
            ui.set_menu_x(x);
            ui.set_menu_y(y);
            ui.invoke_show_folder_menu();
        }
    });
    ui.on_selection_clicked({
        let state = state.clone();
        let weak = ui.as_weak();
        move |id: i32, control: bool, shift: bool, index: i32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let id = i64::from(id);
            let mut selection = state.selected_messages.lock().unwrap();
            if shift {
                let anchor = state
                    .selection_anchor
                    .load(std::sync::atomic::Ordering::SeqCst);
                let model = ui.get_messages();
                let anchor_index = (0..model.row_count())
                    .find(|&i| {
                        model
                            .row_data(i)
                            .map(|r| i64::from(r.id) == anchor)
                            .unwrap_or(false)
                    })
                    .unwrap_or(index as usize);
                let low = anchor_index.min(index.max(0) as usize);
                let high = anchor_index
                    .max(index.max(0) as usize)
                    .min(model.row_count().saturating_sub(1));
                for i in low..=high {
                    if let Some(row) = model.row_data(i) {
                        let row_id = i64::from(row.id);
                        if !selection.contains(&row_id) {
                            selection.push(row_id);
                        }
                    }
                }
            } else if control {
                if let Some(position) = selection.iter().position(|&selected| selected == id) {
                    selection.remove(position);
                } else {
                    selection.push(id);
                }
                state
                    .selection_anchor
                    .store(id, std::sync::atomic::Ordering::SeqCst);
            } else {
                selection.clear();
                state
                    .selection_anchor
                    .store(id, std::sync::atomic::Ordering::SeqCst);
            }
            ui.set_selection_count(if control || shift {
                selection.len() as i32
            } else {
                0
            });
            let batch = selection.len() > 1 || control || shift;
            drop(selection);
            if batch {
                state
                    .selected_message
                    .store(-1, std::sync::atomic::Ordering::SeqCst);
                ui.set_selected_message(-1);
                reader::clear(&ui);
                ui.set_message_body("".into());
                ui.set_message_loading(false);
            }
            state::update_message_selection(&ui, &state);
        }
    });
    ui.on_folder_action({
        let store = store.clone();
        let bridge = bridge.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |id: i32, action: slint::SharedString, name: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let folder_id = i64::from(id);
            let Ok(Some(folder)) = store.get_folder_by_id(folder_id) else {
                return;
            };
            let account = bang_core::AccountId(folder.account_id.clone());
            let command = match action.as_str() {
                "read" => bang_core::UiCommand::MarkFolderRead { account, folder_id },
                "empty" => bang_core::UiCommand::EmptyFolder { account, folder_id },
                "create" => bang_core::UiCommand::CreateFolder {
                    account,
                    name: name.to_string(),
                },
                "rename" => bang_core::UiCommand::RenameFolder {
                    account,
                    folder_id,
                    name: name.to_string(),
                },
                "delete" => bang_core::UiCommand::DeleteFolder { account, folder_id },
                _ => return,
            };
            if action == "delete"
                && state
                    .selected_folder
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == folder_id
            {
                state
                    .selected_folder
                    .store(-1, std::sync::atomic::Ordering::SeqCst);
                state
                    .selected_message
                    .store(-1, std::sync::atomic::Ordering::SeqCst);
                ui.set_selected_folder(-1);
                ui.set_selected_message(-1);
                ui.set_message_body("".into());
                reader::clear(&ui);
                state.selected_messages.lock().unwrap().clear();
                ui.set_selection_count(0);
                load_folder_view(&ui, &store, bang_core::UNIFIED_INBOX_ID);
            }
            bridge.send(command);
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
            state.selected_messages.lock().unwrap().clear();
            state
                .selection_anchor
                .store(-1, std::sync::atomic::Ordering::SeqCst);
            ui.set_selection_count(0);
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
                state
                    .selected_message
                    .store(-1, std::sync::atomic::Ordering::SeqCst);
                load_folder_view(&ui, &store, id);
                return;
            }
            // Live results straight from the local store (FTS5).
            state
                .search_active
                .store(true, std::sync::atomic::Ordering::SeqCst);
            ui.set_search_active(true);
            reader::clear(&ui);
            ui.set_selected_message(-1);
            ui.set_message_sender("".into());
            ui.set_message_subject("".into());
            ui.set_message_date("".into());
            ui.set_message_body("".into());
            match store.search_all(&fts_query(&query), 100) {
                Ok(messages) => {
                    ui.set_folder_name(format!("{} {query}", tr("Search:")).into());
                    let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
                    state::set_message_rows(&ui, rows);
                    ui.invoke_reset_message_scroll();
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
                reader::clear(&ui);
                ui.set_message_attachments(slint::ModelRc::default());
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
            if action == "reply" || action == "reply-all" || action == "forward" {
                let mode = match action.as_str() {
                    "forward" => composer::ComposerMode::Forward,
                    "reply-all" => composer::ComposerMode::ReplyAll,
                    _ => composer::ComposerMode::Reply,
                };
                if let Some(ui) = weak.upgrade() {
                    if let Err(e) =
                        composer::reply_or_forward(&ui, store.clone(), bridge.clone(), id, mode)
                    {
                        ui.set_status_message(format!("{} {e}", tr("Reply unavailable:")).into());
                    }
                }
                return;
            }
            // Snooze and follow-up are local: no account needed.
            if action == "snooze" || action == "follow-up" {
                let at = tomorrow_at(9);
                let command = if action == "snooze" {
                    bang_core::UiCommand::Snooze {
                        message_ids: vec![id],
                        until: at,
                    }
                } else {
                    bang_core::UiCommand::SetFollowUp {
                        message_ids: vec![id],
                        at,
                    }
                };
                bridge.send(command);
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

    // Right click on a row: resolve the message, prepare menu state, and show
    // the popup at the cursor.
    ui.on_message_context({
        let store = store.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |id: i32, x: f32, y: f32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(Some(summary)) = store.get_message(i64::from(id)) else {
                return;
            };
            ui.set_menu_message(id);
            ui.set_menu_msg_unread(summary.flags.is_unread());
            ui.set_menu_msg_starred(summary.flags.is_flagged());
            ui.set_menu_x(x);
            ui.set_menu_y(y);
            let mut selected = state.selected_messages.lock().unwrap();
            if !selected.contains(&i64::from(id)) {
                selected.clear();
                ui.set_selection_count(0);
                state
                    .selected_message
                    .store(-1, std::sync::atomic::Ordering::SeqCst);
                ui.set_selected_message(-1);
            }
            ui.set_menu_batch(!selected.is_empty());
            drop(selected);
            // Deletion inside Trash/Junk is permanent; elsewhere it moves.
            let kind = store
                .get_folder_by_id(summary.folder_id)
                .ok()
                .flatten()
                .map(|f| bang_core::message::folder_kind(&f.path))
                .unwrap_or("folder");
            ui.set_menu_msg_permanent(kind == "trash" || kind == "junk");
            // "Move to..." lists the message's account folders except its own.
            let mut rows: Vec<FolderRow> = Vec::new();
            if let Ok(folders) = store.get_folders(&summary.account_id) {
                for folder in folders {
                    if folder.id == summary.folder_id {
                        continue;
                    }
                    rows.push(FolderRow {
                        id: folder.id as i32,
                        name: folder.name.into(),
                        kind: bang_core::message::folder_kind(&folder.path).into(),
                        is_header: false,
                        ..Default::default()
                    });
                }
            }
            ui.set_move_folders(slint::ModelRc::new(slint::VecModel::from(rows)));
            ui.invoke_show_menu();
        }
    });

    // Folder picked in the "Move to..." popup: move the menu's message.
    ui.on_move_selected({
        let store = store.clone();
        let bridge = bridge.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |folder_id: i32| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let ids = {
                let selected = state.selected_messages.lock().unwrap();
                if selected.len() > 1 {
                    selected.clone()
                } else {
                    vec![i64::from(ui.get_menu_message())]
                }
            };
            let Ok(Some(destination)) = store.get_folder_by_id(i64::from(folder_id)) else {
                return;
            };
            let mut message_ids = Vec::new();
            for id in ids {
                if let Ok(Some(summary)) = store.get_message(id) {
                    if summary.account_id == destination.account_id {
                        message_ids.push(id);
                    }
                }
            }
            if !message_ids.is_empty() {
                bridge.send(bang_core::UiCommand::Move {
                    account: bang_core::AccountId(destination.account_id),
                    message_ids,
                    dest_folder_id: i64::from(folder_id),
                });
            }
            state.selected_messages.lock().unwrap().clear();
            ui.set_selection_count(0);
            state.reload_messages(&ui, &store);
        }
    });
    ui.on_batch_action({
        let store = store.clone();
        let bridge = bridge.clone();
        let state = state.clone();
        let weak = ui.as_weak();
        move |action: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let ids = state.selected_messages.lock().unwrap().clone();
            if ids.is_empty() {
                return;
            }
            if action == "move" {
                let Some(first) = ids.first().copied() else {
                    return;
                };
                let Ok(Some(summary)) = store.get_message(first) else {
                    return;
                };
                let Ok(folders) = store.get_folders(&summary.account_id) else {
                    return;
                };
                let rows = folders
                    .into_iter()
                    .filter(|f| f.id != summary.folder_id)
                    .map(|f| FolderRow {
                        id: f.id as i32,
                        name: f.name.into(),
                        kind: bang_core::message::folder_kind(&f.path).into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                ui.set_menu_message(first as i32);
                ui.set_move_folders(slint::ModelRc::new(slint::VecModel::from(rows)));
                ui.set_menu_x(220.0);
                ui.set_menu_y(80.0);
                ui.set_menu_batch(true);
                ui.invoke_show_move_menu();
                return;
            }
            let mut groups: std::collections::HashMap<String, Vec<i64>> =
                std::collections::HashMap::new();
            for id in ids {
                if let Ok(Some(summary)) = store.get_message(id) {
                    groups.entry(summary.account_id).or_default().push(id);
                }
            }
            for (account_id, message_ids) in groups {
                let account = bang_core::AccountId(account_id);
                let command = match action.as_str() {
                    "delete" => bang_core::UiCommand::Delete {
                        account,
                        message_ids,
                    },
                    "read" => bang_core::UiCommand::SetFlags {
                        account,
                        message_ids,
                        flags: bang_core::MessageFlags::SEEN,
                        add: true,
                    },
                    "star" => bang_core::UiCommand::SetFlags {
                        account,
                        message_ids,
                        flags: bang_core::MessageFlags::FLAGGED,
                        add: true,
                    },
                    _ => return,
                };
                bridge.send(command);
            }
            state.selected_messages.lock().unwrap().clear();
            ui.set_selection_count(0);
            state.reload_messages(&ui, &store);
        }
    });
}

/// Load a folder's contents into the message list (local-cache read).
fn load_folder_view(ui: &BangApp, store: &Arc<bang_core::Store>, id: i64) {
    ui.set_selected_folder(id as i32);
    reader::clear(ui);
    ui.set_selected_message(-1);
    // Clear the reading pane.
    ui.set_message_sender("".into());
    ui.set_message_subject("".into());
    ui.set_message_date("".into());
    ui.set_message_body("".into());
    ui.set_message_attachments(slint::ModelRc::new(slint::VecModel::from(Vec::new())));
    let messages = organization::messages(ui, store, id);
    match messages {
        Ok(messages) => {
            let rows: Vec<MessageRow> = messages.iter().map(state::to_row).collect();
            state::set_message_rows(ui, rows);
            ui.invoke_reset_message_scroll();
        }
        Err(e) => {
            ui.set_status_message(format!("{} {e}", tr("Folder load failed:")).into());
        }
    }
}

/// Tomorrow at the given local hour (snooze / follow-up default).
fn tomorrow_at(hour: u32) -> i64 {
    let now = chrono::Local::now();
    let target = (now.date_naive() + chrono::Duration::days(1)).and_hms_opt(hour, 0, 0);
    target
        .and_then(|t| chrono::Local.from_local_datetime(&t).single())
        .map(|t| t.timestamp())
        .unwrap_or_else(|| now.timestamp() + 86_400)
}

/// Turn free text into a safe FTS5 MATCH expression: each word becomes a
/// quoted token, joined as an implicit AND.
fn fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod fts_tests {
    use super::*;

    /// FTS5 operator injection must stay impossible: every token is quoted
    /// and quotes are stripped, so search input can never become operators.
    #[test]
    fn fts_query_quotes_every_token_and_strips_quotes() {
        assert_eq!(fts_query("foo bar"), "\"foo\" \"bar\"");
        assert_eq!(fts_query("foo\" OR \"bar"), "\"foo\" \"OR\" \"bar\"");
        assert_eq!(fts_query("x AND y"), "\"x\" \"AND\" \"y\"");
        assert_eq!(fts_query("a^2"), "\"a^2\"");
        assert_eq!(fts_query("NOT (a)"), "\"NOT\" \"(a)\"");
        assert_eq!(fts_query(""), "");
    }
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
fn account_rows(store: &bang_core::Store, state: &state::UiState) -> Vec<AccountRow> {
    store
        .get_accounts()
        .unwrap_or_default()
        .into_iter()
        .map(|a| AccountRow {
            removing: state.removing_accounts.lock().unwrap().contains(&a.id.0),
            reconnect: state.auth_attention.lock().unwrap().contains(&a.id.0),
            storage_locked: state.storage_locked.lock().unwrap().contains(&a.id.0),
            provider: match a.auth {
                bang_core::StoredAuth::OAuth2 {
                    provider: bang_core::OAuthProvider::Google,
                    ..
                } => "google",
                bang_core::StoredAuth::OAuth2 { .. } => "microsoft",
                _ => "",
            }
            .into(),
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
        let state = state.clone();
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let rows = account_rows(&store, &state);
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
            if welcome::save_preferences(&ui, &mut config).is_err() {
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
                if welcome::save_preferences(&ui, &mut config).is_err() {
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
                        state::set_message_rows(&ui, rows);
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
    ui.on_settings_account_reconnect({
        let weak = ui.as_weak();
        let store = store.clone();
        move |id| {
            if let (Some(ui), Ok(Some(record))) = (weak.upgrade(), store.get_account(&id)) {
                accounts::reconnect(&ui, &record);
            }
        }
    });
    ui.on_settings_account_unlock({
        let bridge = bridge.clone();
        move |id| {
            bridge.send(bang_core::UiCommand::UnlockAccount(bang_core::AccountId(
                id.into(),
            )));
        }
    });
    ui.on_settings_account_remove({
        let bridge = bridge.clone();
        let weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        move |id: slint::SharedString| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !state
                .removing_accounts
                .lock()
                .unwrap()
                .insert(id.to_string())
            {
                return;
            }
            ui.set_settings_accounts(slint::ModelRc::new(slint::VecModel::from(account_rows(
                &store, &state,
            ))));
            ui.set_status_message(i18n::tr("Removing account…").into());
            bridge.send(bang_core::UiCommand::RemoveAccount(bang_core::AccountId(
                id.into(),
            )));
        }
    });
}

/// Open the isolated rich composer, keeping an existing draft alive.
fn wire_compose(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: bridge::Bridge) {
    let weak = ui.as_weak();
    let store = store.clone();
    ui.on_compose_clicked(move || {
        if let Some(ui) = weak.upgrade() {
            if let Err(error) = composer::open(&ui, store.clone(), bridge.clone(), None, None) {
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
    ui.on_sends_opened({
        let weak = ui.as_weak();
        let store = store.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                state::load_sends(&ui, &store);
                ui.set_sends_open(true);
            }
        }
    });
    ui.on_send_row_delete({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                bridge.send(bang_core::UiCommand::CancelSend { id: i64::from(id) });
                ui.set_status_message(tr("Send cancelled").into());
            }
        }
    });
    ui.on_drafts_dialog_close({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_drafts_open(false);
                ui.set_templates_open(false);
                ui.set_sends_open(false);
            }
        }
    });
}

/// Open a saved draft in the composer; the composer removes it from the
/// list once the editor has received its content.
fn resume_draft(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: &bridge::Bridge, id: i64) {
    let Ok(Some(request)) = store.get_draft(id) else {
        return;
    };
    let prefill = composer::Prefill {
        from: Some(request.account.0.clone()),
        to: request.to.clone(),
        cc: Vec::new(),
        subject: request.subject.clone(),
        body_html: request.body_html.clone().unwrap_or_default(),
        in_reply_to: request.in_reply_to.clone(),
        references: request.references.clone(),
    };
    if let Err(e) = composer::open(ui, store.clone(), bridge.clone(), Some(prefill), Some(id)) {
        ui.set_status_message(format!("{} {e}", tr("Compose unavailable:")).into());
    }
}

/// Open a saved template in the composer (the template stays in the list).
fn resume_template(ui: &BangApp, store: &Arc<bang_core::Store>, bridge: &bridge::Bridge, id: i64) {
    let Ok(Some(request)) = store.get_template(id) else {
        return;
    };
    let prefill = composer::Prefill {
        from: Some(request.account.0.clone()),
        to: request.to.clone(),
        cc: Vec::new(),
        subject: request.subject.clone(),
        body_html: request.body_html.clone().unwrap_or_default(),
        in_reply_to: None,
        references: None,
    };
    if let Err(e) = composer::open(ui, store.clone(), bridge.clone(), Some(prefill), None) {
        ui.set_status_message(format!("{} {e}", tr("Compose unavailable:")).into());
    }
}
