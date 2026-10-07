//! UI state: selection and application of core events to the window.

use std::sync::atomic::{AtomicI64, Ordering};

use chrono::TimeZone;
use slint::{Model, VecModel};

use bang_core::events::MailEvent;
use bang_core::message::{EmailAddress, MessageSummary};
use bang_core::store::Store;

use crate::{BangApp, FolderRow, MessageRow};

/// Selection state shared between the UI callbacks and the event forwarder.
pub struct UiState {
    /// Id of the currently selected folder (-1 = none).
    pub selected_folder: AtomicI64,
    /// Id of the currently selected message (-1 = none).
    pub selected_message: AtomicI64,
}

impl UiState {
    /// Create an empty state.
    pub fn new() -> Self {
        Self {
            selected_folder: AtomicI64::new(-1),
            selected_message: AtomicI64::new(-1),
        }
    }

    /// Apply a core event to the window.
    ///
    /// Must be called on the Slint event loop thread.
    pub fn apply(&self, ui: &BangApp, store: &Store, event: MailEvent) {
        match event {
            MailEvent::AccountStatus { status, .. } => {
                ui.set_account_status(status.to_string().into());
            }
            MailEvent::FoldersUpdated { folders, .. } => {
                let rows: Vec<FolderRow> = folders
                    .iter()
                    .map(|f| FolderRow {
                        id: f.id as i32,
                        name: f.name.clone().into(),
                        unread: f.unread as i32,
                    })
                    .collect();
                ui.set_folders(slint::ModelRc::new(VecModel::from(rows)));
            }
            MailEvent::MessagesAppended {
                folder_id, messages, ..
            } => {
                if self.selected_folder.load(Ordering::SeqCst) != folder_id {
                    return;
                }
                // Newest first: insert at the front.
                let model = ui.get_messages();
                for msg in messages.iter().rev() {
                    let _ = model.insert_row(0, to_row(msg));
                }
            }
            MailEvent::MessagesUpdated { folder_id, .. } => {
                if self.selected_folder.load(Ordering::SeqCst) != folder_id {
                    return;
                }
                if let Ok(messages) = store.get_messages(folder_id, 200) {
                    let rows: Vec<MessageRow> = messages.iter().map(to_row).collect();
                    ui.set_messages(slint::ModelRc::new(VecModel::from(rows)));
                }
            }
            MailEvent::MessageLoaded { message } => {
                self.selected_message
                    .store(message.summary.id, Ordering::SeqCst);
                ui.set_selected_message(message.summary.id as i32);
                ui.set_message_sender(display_name(&message.summary.from).into());
                ui.set_message_subject(message.summary.subject.clone().into());
                ui.set_message_date(format_date(message.summary.date).into());
                ui.set_message_body(if message.body_text.is_empty() {
                    message.body_html.clone().into()
                } else {
                    message.body_text.clone().into()
                });
            }
            MailEvent::SearchResults { messages, .. } => {
                let rows: Vec<MessageRow> = messages.iter().map(to_row).collect();
                ui.set_messages(slint::ModelRc::new(VecModel::from(rows)));
            }
            MailEvent::SendResult { ok, error, .. } => {
                ui.set_status_message(if ok {
                    "Message sent".into()
                } else {
                    format!("Send failed: {}", error.unwrap_or_default()).into()
                });
            }
            MailEvent::Error { message, .. } => {
                ui.set_status_message(message.clone().into());
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

/// Display name of an address (name if set, else the address).
pub(crate) fn display_name(addr: &EmailAddress) -> String {
    if addr.name.is_empty() {
        addr.address.clone()
    } else {
        addr.name.clone()
    }
}

/// Format a Unix timestamp as a short local date.
pub(crate) fn format_date(ts: i64) -> String {
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%d %b %Y").to_string())
        .unwrap_or_default()
}
