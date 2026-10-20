//! UI state: selection and application of core events to the window.

use crate::i18n::tr;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;

use chrono::TimeZone;
use slint::{Model, VecModel};

use bang_core::events::MailEvent;
use bang_core::message::{EmailAddress, MessageSummary};
use bang_core::store::{Store, UNIFIED_INBOX_ID};

use crate::{AttachmentRow, BangApp, DraftRow, FolderRow, MessageRow, TemplateRow};

/// Selection state shared between the UI callbacks and the event forwarder.
pub struct UiState {
    /// Id of the currently selected folder (-1 = none).
    pub selected_folder: AtomicI64,
    /// Id of the currently selected message (-1 = none).
    pub selected_message: AtomicI64,
    /// Whether the message list currently shows search results.
    pub search_active: AtomicBool,
    /// Account sections collapsed by the user.
    pub collapsed_accounts: Mutex<HashSet<String>>,
}

impl UiState {
    /// Create an empty state.
    pub fn new() -> Self {
        Self {
            selected_folder: AtomicI64::new(-1),
            selected_message: AtomicI64::new(-1),
            search_active: AtomicBool::new(false),
            collapsed_accounts: Mutex::new(HashSet::new()),
        }
    }

    /// Rebuild the message list for the currently selected folder.
    ///
    /// Local-cache read: the unified inbox is a query, not a stored folder.
    pub fn reload_messages(&self, ui: &BangApp, store: &Store) {
        let folder_id = self.selected_folder.load(Ordering::SeqCst);
        let messages = if folder_id == UNIFIED_INBOX_ID {
            store.get_unified_inbox(200)
        } else {
            store.get_messages(folder_id, 200)
        };
        if let Ok(messages) = messages {
            let selected = self.selected_message.load(Ordering::SeqCst);
            if selected >= 0 && !messages.iter().any(|m| m.id == selected) {
                self.selected_message.store(-1, Ordering::SeqCst);
                ui.set_selected_message(-1);
                ui.set_message_body("".into());
                ui.set_message_attachments(slint::ModelRc::new(VecModel::from(Vec::new())));
                ui.set_message_loading(false);
            } else if let Some(summary) = messages.iter().find(|m| m.id == selected) {
                set_message_header(ui, summary);
            }
            let rows: Vec<MessageRow> = messages.iter().map(to_row).collect();
            ui.set_messages(slint::ModelRc::new(VecModel::from(rows)));
        }
    }

    /// Rebuild the folder list from the store (all accounts, unified inbox on top).
    pub fn refresh_folders(&self, ui: &BangApp, store: &Store) {
        let accounts = store.get_accounts().unwrap_or_default();
        ui.set_account_count(accounts.len() as i32);
        let mut rows = Vec::new();
        if !accounts.is_empty() {
            rows.push(FolderRow {
                id: UNIFIED_INBOX_ID as i32,
                name: tr("All Inboxes").into(),
                unread: store.unified_inbox_unread().unwrap_or(0) as i32,
                kind: "inbox".into(),
                ..Default::default()
            });
        }
        let collapsed = self.collapsed_accounts.lock().unwrap();
        for account in accounts {
            let hidden = collapsed.contains(&account.id.0);
            rows.push(FolderRow {
                id: -1,
                name: account.email.clone().into(),
                account_id: account.id.0.clone().into(),
                is_header: true,
                collapsed: hidden,
                ..Default::default()
            });
            if !hidden {
                for folder in store.get_folders(&account.id.0).unwrap_or_default() {
                    rows.push(FolderRow {
                        id: folder.id as i32,
                        name: folder.name.clone().into(),
                        unread: folder.unread as i32,
                        account_id: account.id.0.clone().into(),
                        kind: folder_kind(&folder.path).into(),
                        ..Default::default()
                    });
                }
            }
        }
        ui.set_folders(slint::ModelRc::new(VecModel::from(rows)));
    }

    /// Apply a core event to the window.
    ///
    /// Must be called on the Slint event loop thread.
    pub fn apply(&self, ui: &BangApp, store: &Store, event: MailEvent) {
        match event {
            MailEvent::AccountStatus { status, .. } => {
                ui.set_account_status(
                    tr(match status {
                        bang_core::AccountStatus::Disconnected => "Disconnected",
                        bang_core::AccountStatus::Connected => "Connected",
                        bang_core::AccountStatus::Syncing => "Syncing…",
                        bang_core::AccountStatus::Reconnecting => "Reconnecting…",
                    })
                    .into(),
                );
                self.refresh_folders(ui, store);
                if ui.get_wizard_open() && matches!(status, bang_core::AccountStatus::Connected) {
                    ui.set_wizard_open(false);
                }
            }
            MailEvent::FoldersUpdated { .. } => {
                self.refresh_folders(ui, store);
            }
            MailEvent::MessagesAppended {
                folder_id,
                messages,
                ..
            } => {
                // Keep the unread badges fresh.
                self.refresh_folders(ui, store);
                let selected = self.selected_folder.load(Ordering::SeqCst);
                // Notify about new mail unless the user is already viewing it.
                if selected != folder_id && selected != UNIFIED_INBOX_ID {
                    if let Some(latest) = messages.first() {
                        notify_new_message(&display_name(&latest.from), &latest.subject);
                    }
                }
                // Search results are a snapshot; leave them alone.
                if self.search_active.load(Ordering::SeqCst) {
                    return;
                }
                if selected == UNIFIED_INBOX_ID {
                    // It spans every account's INBOX, so append is not enough.
                    self.reload_messages(ui, store);
                    return;
                }
                if selected != folder_id {
                    return;
                }
                // Newest first: insert at the front.
                let model = ui.get_messages();
                for msg in messages.iter().rev() {
                    let _ = model.insert_row(0, to_row(msg));
                }
            }
            MailEvent::MessagesUpdated { folder_id, .. } => {
                // Keep the unread badges fresh.
                self.refresh_folders(ui, store);
                // Search results are a snapshot; leave them alone.
                if self.search_active.load(Ordering::SeqCst) {
                    return;
                }
                let selected = self.selected_folder.load(Ordering::SeqCst);
                if selected == UNIFIED_INBOX_ID {
                    self.reload_messages(ui, store);
                    return;
                }
                if selected != folder_id {
                    return;
                }
                self.reload_messages(ui, store);
            }
            MailEvent::MessageLoaded { message } => {
                if self.selected_message.load(Ordering::SeqCst) != message.summary.id
                    || i64::from(ui.get_selected_message()) != message.summary.id
                {
                    return;
                }
                ui.set_message_loading(false);
                set_message_header(ui, &message.summary);
                self.selected_message
                    .store(message.summary.id, Ordering::SeqCst);
                ui.set_selected_message(message.summary.id as i32);
                ui.set_message_sender(display_name(&message.summary.from).into());
                ui.set_message_subject(message.summary.subject.clone().into());
                ui.set_message_date(format_date(message.summary.date).into());
                // HTML-only messages are rendered as structured text.
                ui.set_message_body(if message.body_text.is_empty() {
                    bang_core::mime::render_html_subset(&message.body_html).into()
                } else {
                    message.body_text.clone().into()
                });
                let atts: Vec<AttachmentRow> = message
                    .attachments
                    .iter()
                    .map(|a| AttachmentRow {
                        filename: a.filename.clone().into(),
                        size: format_size(a.size).into(),
                        path: a.path.clone().into(),
                        content_type: a.content_type.clone().into(),
                    })
                    .collect();
                ui.set_message_attachments(slint::ModelRc::new(VecModel::from(atts)));
            }
            MailEvent::FollowUpDue { subject, .. } => {
                notify(&tr("Follow-up due"), &subject);
            }
            MailEvent::SendScheduled { .. } => {
                ui.set_status_message(tr("Message scheduled").into());
            }
            MailEvent::DraftsUpdated => {
                load_drafts(ui, store);
            }
            MailEvent::TemplatesUpdated => {
                load_templates(ui, store);
            }
            MailEvent::SearchResults { messages, .. } => {
                let rows: Vec<MessageRow> = messages.iter().map(to_row).collect();
                ui.set_messages(slint::ModelRc::new(VecModel::from(rows)));
            }
            MailEvent::SendResult { ok, error, .. } => {
                crate::composer::send_result(ok, error.clone());
                ui.set_status_message(if ok {
                    tr("Message sent").into()
                } else {
                    format!("{} {}", tr("Send failed:"), error.unwrap_or_default()).into()
                });
            }
            MailEvent::Error { message, .. } => {
                ui.set_message_loading(false);
                ui.set_status_message(format!("{} {}", tr("Error:"), message).into());
                // The wizard stays open during OAuth; show failures there too.
                ui.set_wizard_status(message.into());
            }
        }
    }
}

/// Convert a message summary to a list row.
pub(crate) fn to_row(m: &MessageSummary) -> MessageRow {
    MessageRow {
        id: m.id as i32,
        sender: display_name(&m.from).into(),
        subject: m.subject.clone().into(),
        date: format_date(m.date).into(),
        preview: m.preview.clone().into(),
        unread: m.flags.is_unread(),
        starred: m.flags.is_flagged(),
        has_attachments: m.has_attachments,
    }
}

/// Human-readable file size (B, KB, MB).
pub(crate) fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Display name of an address (name if set, else the address).
pub(crate) fn display_name(addr: &EmailAddress) -> String {
    if addr.name.is_empty() {
        addr.address.clone()
    } else {
        addr.name.clone()
    }
}

/// Show a desktop notification (best effort).
///
/// Uses `notify-send` (libnotify), present on most Linux desktops. Fails
/// silently when it is missing (e.g. headless).
pub(crate) fn notify(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .arg(title)
        .arg(body)
        .spawn();
}

/// Show a desktop notification for a new message.
pub(crate) fn notify_new_message(sender: &str, subject: &str) {
    notify(&format!("{} {sender}", tr("New message from")), subject);
}

/// Format a Unix timestamp as a short local date.
pub(crate) fn format_date(ts: i64) -> String {
    let Some(date) = chrono::Local.timestamp_opt(ts, 0).single() else {
        return String::new();
    };
    let today = chrono::Local::now().date_naive();
    if date.date_naive() == today {
        date.format("%H:%M").to_string()
    } else if Some(date.date_naive()) == today.pred_opt() {
        tr("Yesterday")
    } else {
        date.format(if crate::i18n::french() {
            "%d/%m/%Y"
        } else {
            "%Y-%m-%d"
        })
        .to_string()
    }
}

/// Populate the compact reader using the real message metadata.
pub fn set_message_header(ui: &BangApp, summary: &MessageSummary) {
    ui.set_message_sender(display_name(&summary.from).into());
    ui.set_message_sender_address(summary.from.address.clone().into());
    ui.set_message_recipients(
        summary
            .to
            .iter()
            .map(display_name)
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
    );
    ui.set_message_initials(
        display_name(&summary.from)
            .split_whitespace()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect::<String>()
            .to_uppercase()
            .into(),
    );
    ui.set_message_subject(summary.subject.clone().into());
    ui.set_message_date(format_date(summary.date).into());
    ui.set_message_starred(summary.flags.is_flagged());
}

/// Reload the drafts list into the dialog's model.
pub(crate) fn load_drafts(ui: &BangApp, store: &Store) {
    let rows: Vec<DraftRow> = store
        .get_drafts()
        .unwrap_or_default()
        .into_iter()
        .map(|d| DraftRow {
            id: d.id as i32,
            title: d.subject.clone().into(),
            subtitle: d.to.clone().into(),
        })
        .collect();
    ui.set_drafts(slint::ModelRc::new(VecModel::from(rows)));
}

/// Reload the templates list into the dialog's model.
pub(crate) fn load_templates(ui: &BangApp, store: &Store) {
    let rows: Vec<TemplateRow> = store
        .get_templates()
        .unwrap_or_default()
        .into_iter()
        .map(|t| TemplateRow {
            id: t.id as i32,
            title: t.name.clone().into(),
            subtitle: t.subject.clone().into(),
        })
        .collect();
    ui.set_templates(slint::ModelRc::new(VecModel::from(rows)));
}

fn folder_kind(path: &str) -> &'static str {
    let last = path
        .rsplit(['/', '.'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    match last.as_str() {
        "inbox" => "inbox",
        "sent" | "sent messages" => "sent",
        "drafts" => "draft",
        "trash" | "deleted messages" => "trash",
        _ => "folder",
    }
}

#[cfg(test)]
mod presentation_tests {
    #[test]
    fn folder_icons_do_not_change_names() {
        assert_eq!(super::folder_kind("INBOX"), "inbox");
        assert_eq!(super::folder_kind("[Gmail]/Sent"), "sent");
        assert_eq!(super::folder_kind("My projects"), "folder");
    }
}
