//! Regression checks for lazy message bodies and metadata refreshes.

use std::sync::Arc;

use bang_core::account::{AccountConfig, AccountId, AuthMethod, ImapConfig, SmtpConfig, TlsMode};
use bang_core::events::{MailEvent, UiCommand};
use bang_core::message::{EmailAddress, Folder, MessageFlags, MessageSummary};
use bang_core::service::MailService;
use bang_core::store::Store;
use tokio::sync::mpsc::unbounded_channel;

fn cached_summary() -> (Arc<Store>, i64, i64) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store
        .save_account(&AccountConfig {
            id: AccountId("alice".into()),
            email: "alice@example.test".into(),
            imap: ImapConfig {
                host: "localhost".into(),
                port: 143,
                tls: TlsMode::Plaintext,
            },
            smtp: SmtpConfig {
                host: "localhost".into(),
                port: 25,
                tls: TlsMode::Plaintext,
            },
            auth: AuthMethod::Password {
                username: "alice".into(),
                password: "test".into(),
            },
        })
        .unwrap();
    store
        .upsert_folder(&Folder {
            id: 0,
            account_id: "alice".into(),
            name: "INBOX".into(),
            path: "INBOX".into(),
            unread: 0,
        })
        .unwrap();
    let folder = store.get_folders("alice").unwrap().remove(0).id;
    let id = store
        .upsert_message(&MessageSummary {
            id: 0,
            account_id: "alice".into(),
            folder_id: folder,
            uid: 7,
            message_id: "<test@example.test>".into(),
            from: EmailAddress {
                name: "Bob".into(),
                address: "bob@example.test".into(),
            },
            to: Vec::new(),
            cc: Vec::new(),
            subject: "Cached mail".into(),
            date: 123,
            flags: MessageFlags::empty(),
            preview: String::new(),
            has_attachments: false,
        })
        .unwrap();
    (store, folder, id)
}

#[test]
fn unfetched_and_fetched_empty_bodies_are_distinct() {
    let (store, _, id) = cached_summary();
    assert!(store.get_message_full(id).unwrap().is_none());
    assert!(store.get_message(id).unwrap().is_some());
    assert!(store.get_message_full(i64::MAX).unwrap().is_none());

    let (tx, mut events) = unbounded_channel();
    let service = MailService::new(store.clone(), tx);
    for (text, html) in [("", ""), ("", "<p>HTML only</p>"), ("Text only", "")] {
        store.save_message_bodies(id, text, html, &[]).unwrap();
        for _ in 0..2 {
            service.handle_command(UiCommand::OpenMessage {
                account: AccountId("alice".into()),
                message_id: id,
            });
            match events.try_recv().unwrap() {
                MailEvent::MessageLoaded { message } => {
                    assert_eq!(message.body_text, text);
                    assert_eq!(message.body_html, html);
                }
                _ => panic!("expected cached message without a network connection"),
            }
        }
    }
}

#[test]
fn flag_refresh_skips_unchanged_rows_and_preserves_cached_content() {
    let (store, folder, id) = cached_summary();
    store
        .save_message_bodies(id, "Searchable body", "<p>Searchable body</p>", &[])
        .unwrap();
    store.snooze_messages(&[id], i64::MAX).unwrap();
    assert_eq!(store.recent_message_uids(folder, 200).unwrap(), vec![7]);
    assert_eq!(
        store
            .refresh_message_flags(folder, 7, MessageFlags::empty())
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .refresh_message_flags(folder, 99, MessageFlags::SEEN)
            .unwrap(),
        None
    );
    let flags = MessageFlags::SEEN | MessageFlags::FLAGGED;
    assert_eq!(
        store.refresh_message_flags(folder, 7, flags).unwrap(),
        Some(id)
    );
    assert_eq!(store.refresh_message_flags(folder, 7, flags).unwrap(), None);
    assert_eq!(
        store
            .refresh_message_flags(folder, 7, MessageFlags::SEEN)
            .unwrap(),
        Some(id)
    );
    let message = store.get_message_full(id).unwrap().unwrap();
    assert_eq!(message.summary.subject, "Cached mail");
    assert_eq!(message.body_text, "Searchable body");
    assert_eq!(message.body_html, "<p>Searchable body</p>");
    assert_eq!(message.summary.flags, MessageFlags::SEEN);
    assert_eq!(store.search_all("Searchable", 20).unwrap().len(), 1);
}
