//! Exercise real Slint controls against an isolated software backend.
use crate::{BangApp, FolderRow, MessageRow};
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, Global, LogicalSize, Model, VecModel};

#[test]
fn verification_and_autostart_controls_are_independent() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    i_slint_backend_testing::init_no_event_loop();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    let otp = Arc::new(AtomicBool::new(false));
    let startup = Arc::new(AtomicBool::new(false));
    let otp_result = otp.clone();
    ui.on_settings_otp_toggled(move |enabled| otp_result.store(enabled, Ordering::SeqCst));
    let startup_result = startup.clone();
    ui.on_settings_autostart_toggled(move |enabled| {
        startup_result.store(enabled, Ordering::SeqCst)
    });
    ui.set_settings_open(true);
    ui.show().unwrap();
    ElementHandle::find_by_accessible_label(&ui, "Suggest verification codes")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert!(otp.load(Ordering::SeqCst));
    assert!(!startup.load(Ordering::SeqCst));
    ElementHandle::find_by_accessible_label(&ui, "Start service when signing in")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert!(startup.load(Ordering::SeqCst));
    assert!(otp.load(Ordering::SeqCst));
    ui.hide().unwrap();
}
#[test]
fn native_navigation_resize_and_grouping() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_account_count(1);
    ui.set_folders(slint::ModelRc::new(VecModel::from(vec![FolderRow {
        id: 1,
        name: "Inbox".into(),
        kind: "inbox".into(),
        ..Default::default()
    }])));
    ui.set_messages(slint::ModelRc::new(VecModel::from(vec![MessageRow {
        id: 1,
        sender: "Camille".into(),
        subject: "Hello".into(),
        ..Default::default()
    }])));
    let weak = ui.as_weak();
    ui.on_message_selected(move |id| {
        weak.upgrade().unwrap().set_selected_message(id);
    });
    ui.show().unwrap();
    let compose = || {
        ElementHandle::find_by_accessible_label(&ui, "New message")
            .next()
            .expect("compose button")
    };
    let before = compose().absolute_position().x;
    let splitter = ElementHandle::find_by_element_id(&ui, "BangApp::nav-splitter")
        .next()
        .expect("sidebar separator");
    let pos = splitter.absolute_position();
    splitter.mock_drag(
        slint::LogicalPosition::new(pos.x + 40., pos.y + 30.),
        PointerEventButton::Left,
    );
    assert!(
        compose().absolute_position().x > before + 10.,
        "drag must resize the sidebar"
    );
    ui.window().set_size(LogicalSize::new(720., 480.));
    ElementHandle::find_by_accessible_label(&ui, "Camille, Hello")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(ui.get_selected_message(), 1);
    assert!(ElementHandle::find_by_accessible_label(&ui, "Back")
        .next()
        .is_some());
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Escape.into(),
    });
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Back")
            .next()
            .is_none(),
        "Escape returns to the list"
    );
    ui.set_messages(slint::ModelRc::new(VecModel::<MessageRow>::default()));
    let search = ElementHandle::find_by_accessible_label(&ui, "Search")
        .find(|element| element.absolute_position().y > 50.)
        .expect("visible narrow search above the empty state");
    search.mock_single_click(PointerEventButton::Left);
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: "café".into(),
    });
    assert_eq!(ui.get_search_text(), "café");
    ui.hide().unwrap();

    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    let state = crate::state::UiState::new();
    state.refresh_folders(&ui, &store);
    assert_eq!(ui.get_account_count(), 0);
    for id in ["personal@example.test", "work@example.test"] {
        store
            .save_account(&bang_core::AccountConfig {
                id: bang_core::AccountId(id.into()),
                email: id.into(),
                imap: bang_core::ImapConfig {
                    host: "localhost".into(),
                    port: 993,
                    tls: bang_core::TlsMode::Tls,
                },
                smtp: bang_core::SmtpConfig {
                    host: "localhost".into(),
                    port: 465,
                    tls: bang_core::TlsMode::Tls,
                },
                auth: bang_core::AuthMethod::Password {
                    smtp_username: String::new(),
                    username: id.into(),
                    password: "test".into(),
                },
            })
            .unwrap();
        store
            .upsert_folder(&bang_core::Folder {
                id: 0,
                account_id: id.into(),
                name: "Projet été".into(),
                path: "Custom".into(),
                unread: 0,
            })
            .unwrap();
    }
    state.refresh_folders(&ui, &store);
    assert_eq!(ui.get_account_count(), 2);
    // Two account headers, two real folders, and the unified inbox.
    assert_eq!(ui.get_folders().row_count(), 5);
    state
        .collapsed_accounts
        .lock()
        .unwrap()
        .insert("personal@example.test".into());
    state.refresh_folders(&ui, &store);
    assert_eq!(ui.get_folders().row_count(), 4);
    assert_eq!(ui.get_folders().iter().filter(|r| r.is_header).count(), 2);
    assert!(ui.get_folders().iter().any(|r| r.name == "Projet été"));

    // A late fetch from the previously open folder must not reopen its reader.
    ui.set_selected_message(-1);
    state
        .selected_message
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let message: bang_core::MessageFull = serde_json::from_value(serde_json::json!({
        "summary": {"id":1,"account_id":"personal@example.test","folder_id":1,
            "uid":1,"message_id":"test","from":{"name":"Camille","address":"camille@example.test"},
            "to":[],"cc":[],"subject":"Previous folder","date":0,"flags":"",
            "preview":"","has_attachments":false},
        "body_text":"Late response","body_html":"","attachments":[]
    }))
    .unwrap();
    state.apply(&ui, &store, bang_core::MailEvent::MessageLoaded { message });
    assert_eq!(ui.get_selected_message(), -1);
}

#[test]
fn rapid_message_selection_keeps_ui_stable() {
    use std::sync::atomic::Ordering;
    let backend = i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    );
    let logs = backend.debug_logs.clone();
    slint::platform::set_platform(Box::new(backend)).unwrap();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_account_count(1);
    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    let state = std::rc::Rc::new(crate::state::UiState::new());
    let messages: Vec<bang_core::MessageFull> = (1..=200).map(|id| {
        serde_json::from_value(serde_json::json!({
            "summary": {"id":id,"account_id":"test","folder_id":1,
                "uid":id,"message_id":"test","from":{"name":"Camille","address":"camille@example.test"},
                "to":[],"cc":[],"subject":format!("Message {id}"),"date":0,"flags":"",
                "preview":"Preview","has_attachments":false},
            "body_text":"A long body\n".repeat(id as usize * 5),
            "body_html":if id % 2 == 0 { "<p>HTML mail</p>" } else { "" },
            "attachments":if id % 3 == 0 { serde_json::json!([{
                "id":1,"message_id":id,"filename":"report.pdf","content_type":"application/pdf",
                "size":42,"path":"","content_id":"","disposition":"attachment"
            }]) } else { serde_json::json!([]) }
        })).unwrap()
    }).collect();
    ui.set_messages(slint::ModelRc::new(VecModel::from(
        messages
            .iter()
            .map(|m| crate::state::to_row(&m.summary))
            .collect::<Vec<_>>(),
    )));
    // GTK can instantiate the populated list before assigning its real size.
    ui.window().dispatch_event(WindowEvent::Resized {
        size: LogicalSize::new(1., 1.),
    });
    ui.window().dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(0., 0.),
    });
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.on_message_selected({
        let weak = ui.as_weak();
        let state = state.clone();
        move |id| {
            let ui = weak.upgrade().unwrap();
            state
                .selected_message
                .store(i64::from(id), Ordering::SeqCst);
            ui.set_selected_message(id);
            ui.set_message_attachments(slint::ModelRc::default());
            ui.set_message_loading(true);
            ui.set_message_body("".into());
        }
    });
    ui.show().unwrap();
    for width in [1440., 1024., 720.] {
        ui.window().set_size(LogicalSize::new(width, 900.));
        for id in [1, 2, 3, 4, 1, 3, 2, 4] {
            // Escape returns to the list when using the narrow layout.
            ui.window().dispatch_event(WindowEvent::KeyPressed {
                text: Key::Escape.into(),
            });
            ElementHandle::find_by_accessible_label(&ui, &format!("Camille, Message {id}"))
                .next()
                .unwrap()
                .mock_single_click(PointerEventButton::Left);
            assert_eq!(ui.get_selected_message(), id);
            ui.window().take_snapshot().unwrap();
            state.apply(
                &ui,
                &store,
                bang_core::MailEvent::MessageLoaded {
                    message: messages[id as usize - 1].clone(),
                },
            );
            // Read/unread events rebuild the list model while keeping its viewport.
            ui.set_messages(slint::ModelRc::new(VecModel::from(
                messages
                    .iter()
                    .map(|m| crate::state::to_row(&m.summary))
                    .collect::<Vec<_>>(),
            )));
            ui.window().take_snapshot().unwrap();
        }
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Escape.into(),
        });
        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(if width >= 1120. { 350. } else { 100. }, 200.),
            delta_x: 0.,
            delta_y: -1800.,
        });
        ui.window().take_snapshot().unwrap();
        for _ in 0..8 {
            ui.set_messages(slint::ModelRc::new(VecModel::from(
                messages
                    .iter()
                    .map(|m| crate::state::to_row(&m.summary))
                    .collect::<Vec<_>>(),
            )));
            ui.window().take_snapshot().unwrap();
        }
        ui.window().dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(if width >= 1120. { 350. } else { 100. }, 200.),
            delta_x: 0.,
            delta_y: 20000.,
        });
    }
    assert!(
        !logs
            .borrow()
            .iter()
            .any(|log| log.contains("long callback/instantiation chain")),
        "{:?}",
        logs.borrow()
    );
    ui.hide().unwrap();
}

#[test]
fn ten_thousand_messages_materialize_only_visible_rows() {
    use std::{cell::Cell, rc::Rc};
    struct LargeMailbox {
        reads: Cell<usize>,
        notify: slint::ModelNotify,
    }
    impl Model for LargeMailbox {
        type Data = MessageRow;
        fn row_count(&self) -> usize {
            10_000
        }
        fn row_data(&self, row: usize) -> Option<MessageRow> {
            self.reads.set(self.reads.get() + 1);
            (row < self.row_count()).then(|| MessageRow {
                id: row as i32 + 1,
                sender: "Sender".into(),
                subject: format!("Message {}", row + 1).into(),
                ..Default::default()
            })
        }
        fn model_tracker(&self) -> &dyn slint::ModelTracker {
            &self.notify
        }
    }
    let backend = i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    );
    let logs = backend.debug_logs.clone();
    slint::platform::set_platform(Box::new(backend)).unwrap();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_account_count(1);
    let mailbox = Rc::new(LargeMailbox {
        reads: Cell::new(0),
        notify: Default::default(),
    });
    ui.set_messages(mailbox.clone().into());
    ui.show().unwrap();
    ui.window().take_snapshot().unwrap();
    let initial_reads = mailbox.reads.replace(0);
    assert!(
        initial_reads < 100,
        "initial render read {initial_reads} rows"
    );
    ui.invoke_reveal_message(9999);
    ui.window().take_snapshot().unwrap();
    let seek_reads = mailbox.reads.get();
    assert!(seek_reads < 100, "direct seek read {seek_reads} rows");
    let last = ElementHandle::find_by_accessible_label(&ui, "Sender, Message 10000")
        .next()
        .unwrap();
    assert!(last.absolute_position().y >= 53.);
    assert!(
        last.absolute_position().y + last.size().height <= 900.,
        "last row position {:?}, size {:?}",
        last.absolute_position(),
        last.size()
    );
    let live_rows = ElementHandle::find_by_element_type_name(&ui, "Rectangle")
        .filter(|e| {
            e.accessible_label()
                .is_some_and(|s| s.starts_with("Sender, Message "))
        })
        .count();
    assert!(
        live_rows <= 12,
        "instantiated {live_rows} rows for one viewport"
    );
    eprintln!("10000 messages: {initial_reads} initial row reads, {seek_reads} seek reads, {live_rows} live delegates");
    ui.invoke_reset_message_scroll();
    ui.window().take_snapshot().unwrap();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Sender, Message 1")
            .next()
            .is_some()
    );
    assert!(logs.borrow().is_empty(), "{:?}", logs.borrow());
    ui.hide().unwrap();
}

#[test]
fn list_refresh_preserves_visible_delegates_and_scroll() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_account_count(1);
    let mut rows: Vec<_> = (1..=10_000)
        .map(|id| MessageRow {
            id,
            sender: "Sender".into(),
            subject: format!("Message {id}").into(),
            ..Default::default()
        })
        .collect();
    crate::state::set_message_rows(&ui, rows.clone());
    ui.show().unwrap();
    ui.invoke_reveal_message(4999);
    let visible = ElementHandle::find_by_accessible_label(&ui, "Sender, Message 5000")
        .next()
        .unwrap();
    let y = visible.absolute_position().y;
    let model = ui.get_messages();
    rows[4999].starred = true;
    crate::state::set_message_rows(&ui, rows.clone());
    assert!(ui.get_messages() == model);
    assert!(visible.is_valid());
    assert_eq!(visible.absolute_position().y, y);
    // A new head row and eviction of the oldest row preserve the viewport.
    rows.insert(
        0,
        MessageRow {
            id: 10_001,
            sender: "New sender".into(),
            ..Default::default()
        },
    );
    rows.pop();
    crate::state::set_message_rows(&ui, rows.clone());
    ElementHandle::find_by_accessible_label(&ui, "Sender, Message 5000")
        .next()
        .unwrap();
    assert!(visible.is_valid());
    assert_eq!(visible.absolute_position().y, y);
    rows.remove(0);
    crate::state::set_message_rows(&ui, rows);
    ElementHandle::find_by_accessible_label(&ui, "Sender, Message 5000")
        .next()
        .unwrap();
    assert!(visible.is_valid());
    assert_eq!(visible.absolute_position().y, y);
    crate::state::set_message_rows(&ui, Vec::new());
    assert_eq!(model.row_count(), 0);
    ui.hide().unwrap();
}

#[test]
fn sync_events_keep_a_sorted_bounded_model_and_clear_removed_selection() {
    use bang_core::{
        AccountConfig, AccountId, AuthMethod, EmailAddress, Folder, ImapConfig, MailEvent,
        MessageFlags, MessageSummary, SmtpConfig, TlsMode,
    };
    use std::sync::atomic::Ordering;
    i_slint_backend_testing::init_no_event_loop();
    let ui = BangApp::new().unwrap();
    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    store
        .save_account(&AccountConfig {
            id: AccountId("test".into()),
            email: "test@example.test".into(),
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
                smtp_username: String::new(),
                username: "test".into(),
                password: "test".into(),
            },
        })
        .unwrap();
    store
        .upsert_folder(&Folder {
            id: 0,
            account_id: "test".into(),
            path: "INBOX".into(),
            name: "INBOX".into(),
            unread: 0,
        })
        .unwrap();
    let folder_id = store.get_folders("test").unwrap()[0].id;
    let mut summary = MessageSummary {
        id: 0,
        account_id: "test".into(),
        folder_id,
        uid: 0,
        message_id: String::new(),
        from: EmailAddress {
            name: "Sender".into(),
            address: "sender@example.test".into(),
        },
        to: vec![],
        cc: vec![],
        subject: "Message".into(),
        date: 0,
        flags: MessageFlags::empty(),
        preview: String::new(),
        has_attachments: false,
    };
    for uid in 1..=225 {
        summary.uid = uid;
        summary.date = uid as i64;
        summary.id = store.upsert_message(&summary).unwrap();
    }
    let state = crate::state::UiState::new();
    state.selected_folder.store(folder_id, Ordering::SeqCst);
    state.reload_messages(&ui, &store);
    let model = ui.get_messages();
    let head = model.row_data(0).unwrap().id;
    assert_eq!(model.row_count(), 200);
    // Repeat an append event: the store is authoritative, not the event order.
    for _ in 0..2 {
        state.apply(
            &ui,
            &store,
            MailEvent::MessagesAppended {
                account: AccountId("test".into()),
                folder_id,
                messages: vec![summary.clone()],
            },
        );
        assert!(ui.get_messages() == model);
        assert_eq!(model.row_count(), 200);
    }
    let folders = ui.get_folders();
    summary.uid = 226;
    summary.date = 0;
    summary.id = store.upsert_message(&summary).unwrap();
    state.apply(
        &ui,
        &store,
        MailEvent::MessagesAppended {
            account: AccountId("test".into()),
            folder_id,
            messages: vec![summary],
        },
    );
    assert_eq!(model.row_data(0).unwrap().id, head);
    assert!(ui.get_folders() == folders);
    store
        .set_message_flags(&[i64::from(head)], MessageFlags::SEEN, true)
        .unwrap();
    state.apply(
        &ui,
        &store,
        MailEvent::MessagesUpdated {
            account: AccountId("test".into()),
            folder_id,
            message_ids: vec![i64::from(head)],
        },
    );
    assert!(ui.get_messages() == model);
    assert!(!model.row_data(0).unwrap().unread);
    state
        .selected_message
        .store(i64::from(head), Ordering::SeqCst);
    ui.set_selected_message(head);
    store.snooze_messages(&[i64::from(head)], i64::MAX).unwrap();
    state.reload_messages(&ui, &store);
    assert_eq!(ui.get_selected_message(), -1);
    assert_eq!(state.selected_message.load(Ordering::SeqCst), -1);
    assert_eq!(model.row_count(), 200);
    assert!(!model.iter().any(|row| row.id == head));
}

#[test]
fn categories_labels_dialog_and_late_avatars() {
    use std::{cell::Cell, rc::Rc};
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_account_count(1);
    ui.set_categories_visible(true);
    ui.set_categories(slint::ModelRc::new(VecModel::from(vec![
        crate::CategoryRow {
            key: "".into(),
            name: "All".into(),
            short_name: "All".into(),
            unread: 2,
        },
        crate::CategoryRow {
            key: "transactions".into(),
            name: "Transactions".into(),
            short_name: "Orders".into(),
            unread: 1,
        },
    ])));
    let weak = ui.as_weak();
    ui.on_category_selected(move |key| weak.upgrade().unwrap().set_selected_category(key));
    ui.set_messages(slint::ModelRc::new(VecModel::from(vec![
        MessageRow {
            id: 1,
            sender: "Alice".into(),
            sender_address: "alice@example.test".into(),
            initials: "A".into(),
            avatar_tint: crate::avatars::tint("alice@example.test"),
            ..Default::default()
        },
        MessageRow {
            id: 2,
            sender: "Bob".into(),
            sender_address: "bob@example.test".into(),
            ..Default::default()
        },
    ])));
    ui.set_labels(slint::ModelRc::new(VecModel::from(vec![crate::LabelRow {
        id: 1,
        name: "Work".into(),
        tint: crate::organization::color("#3977b8"),
        ..Default::default()
    }])));
    ui.set_selected_message(2);
    ui.set_message_sender_address("bob@example.test".into());
    ui.set_native_reader_available(true);
    ui.set_message_html("<p>Reader</p>".into());
    let weak = ui.as_weak();
    ui.on_organization_opened(move |manage| {
        let ui = weak.upgrade().unwrap();
        ui.set_managing_labels(manage);
        ui.set_organization_open(true);
    });
    let weak = ui.as_weak();
    ui.on_organization_close(move || weak.upgrade().unwrap().set_organization_open(false));
    ui.show().unwrap();
    ElementHandle::find_by_accessible_label(&ui, "Transactions, 1")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(ui.get_selected_category(), "transactions");
    ElementHandle::find_by_accessible_label(&ui, "Classify message")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert!(ui.get_organization_open());
    assert!(!ui.get_reader_shown());
    let assigned = Rc::new(Cell::new(false));
    let assigned_cb = assigned.clone();
    ui.on_label_assigned(move |id, value| {
        assert_eq!(id, 1);
        assigned_cb.set(value);
    });
    ElementHandle::find_by_accessible_label(&ui, "Work")
        .find(|e| e.type_name().is_some_and(|name| name.contains("CheckBox")))
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert!(assigned.get());
    ui.window().set_size(LogicalSize::new(720., 480.));
    ui.window().take_snapshot().unwrap();
    ui.invoke_organization_close();
    assert!(!ui.get_organization_open());
    let path = std::env::temp_dir().join(format!("bang-avatar-ui-{}.png", std::process::id()));
    image::RgbaImage::from_pixel(80, 80, image::Rgba([10, 30, 60, 255]))
        .save(&path)
        .unwrap();
    crate::avatars::ready(&ui, "alice@example.test", path.to_str().unwrap());
    assert!(ui.get_messages().row_data(0).unwrap().has_avatar);
    let mut unchanged = ui.get_messages().row_data(1).unwrap();
    crate::avatars::decorate(&ui, &mut unchanged);
    assert_eq!(
        unchanged,
        unchanged.clone(),
        "empty avatar rows must compare equal to prevent needless model updates"
    );
    assert!(!ui.get_messages().row_data(1).unwrap().has_avatar);
    assert!(
        !ui.get_message_avatar_loaded(),
        "a late Alice result must not replace Bob's avatar"
    );
    ui.set_online_avatars(false);
    crate::avatars::ready(&ui, "bob@example.test", path.to_str().unwrap());
    assert!(!ui.get_message_avatar_loaded());
    std::fs::remove_file(path).unwrap();
    ui.hide().unwrap();
}

#[test]
fn organization_views_suspend_filters_for_search_and_clear_removed_selection() {
    use bang_core::{
        organization::MailCategory, AccountConfig, AccountId, AuthMethod, EmailAddress, Folder,
        ImapConfig, MessageFlags, MessageSummary, SmtpConfig, TlsMode,
    };
    use std::sync::atomic::Ordering;
    i_slint_backend_testing::init_no_event_loop();
    let ui = BangApp::new().unwrap();
    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    let account = AccountConfig {
        id: AccountId("local".into()),
        email: "me@example.test".into(),
        imap: ImapConfig {
            host: "localhost".into(),
            port: 993,
            tls: TlsMode::Tls,
        },
        smtp: SmtpConfig {
            host: "localhost".into(),
            port: 465,
            tls: TlsMode::Tls,
        },
        auth: AuthMethod::Password {
            smtp_username: String::new(),
            username: "local".into(),
            password: "test".into(),
        },
    };
    store.save_account(&account).unwrap();
    store
        .upsert_folder(&Folder {
            id: 0,
            account_id: "local".into(),
            name: "Inbox".into(),
            path: "INBOX".into(),
            unread: 0,
        })
        .unwrap();
    let folder = store.get_folders("local").unwrap()[0].id;
    let mut ids = Vec::new();
    for (uid, subject) in [(1, "Votre facture"), (2, "Bonjour")] {
        ids.push(
            store
                .upsert_message(&MessageSummary {
                    id: 0,
                    account_id: "local".into(),
                    folder_id: folder,
                    uid,
                    message_id: format!("id-{uid}"),
                    from: EmailAddress {
                        name: "Alice".into(),
                        address: "alice@example.test".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    subject: subject.into(),
                    date: i64::from(uid),
                    flags: MessageFlags::empty(),
                    preview: "".into(),
                    has_attachments: false,
                })
                .unwrap(),
        );
    }
    let state = crate::state::UiState::new();
    state.selected_folder.store(-1, Ordering::SeqCst);
    ui.set_selected_category("transactions".into());
    state.reload_messages(&ui, &store);
    assert_eq!(ui.get_messages().row_count(), 1);
    state.selected_message.store(ids[0], Ordering::SeqCst);
    ui.set_selected_message(ids[0] as i32);
    store
        .set_category(ids[0], Some(MailCategory::Primary), false)
        .unwrap();
    state.apply(&ui, &store, bang_core::MailEvent::OrganizationChanged);
    assert_eq!(ui.get_messages().row_count(), 0);
    assert_eq!(ui.get_selected_message(), -1);
    store.save_label(None, "Work", "#3977b8").unwrap();
    let label = store.labels().unwrap()[0].id;
    store.set_message_label(ids[0], label, true).unwrap();
    ui.set_selected_label(label as i32);
    state.reload_messages(&ui, &store);
    assert_eq!(ui.get_messages().row_data(0).unwrap().id, ids[0] as i32);
    // Search uses global store results while the chosen label/category stay saved.
    state.search_active.store(true, Ordering::SeqCst);
    ui.set_search_active(true);
    crate::state::set_message_rows(
        &ui,
        store
            .search_all("Bonjour", 100)
            .unwrap()
            .iter()
            .map(crate::state::to_row)
            .collect(),
    );
    state.apply(&ui, &store, bang_core::MailEvent::OrganizationChanged);
    assert_eq!(ui.get_messages().row_data(0).unwrap().id, ids[1] as i32);
    state.search_active.store(false, Ordering::SeqCst);
    ui.set_search_active(false);
    state.reload_messages(&ui, &store);
    assert_eq!(ui.get_selected_label(), label as i32);
    assert_eq!(ui.get_selected_category(), "transactions");
    assert_eq!(ui.get_messages().row_data(0).unwrap().id, ids[0] as i32);
}

#[test]
fn sender_photos_fit_the_avatar_at_any_source_resolution() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_account_count(1);
    ui.set_selected_message(1);
    ui.show().unwrap();
    for resolution in [80, 512] {
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(resolution, resolution);
        pixels.make_mut_slice().fill(slint::Rgba8Pixel {
            r: 250,
            g: 0,
            b: 250,
            a: 255,
        });
        let photo = slint::Image::from_rgba8(pixels);
        ui.set_messages(slint::ModelRc::new(VecModel::from(vec![MessageRow {
            id: 1,
            sender: "Google".into(),
            subject: "Message".into(),
            avatar: photo.clone(),
            has_avatar: true,
            ..Default::default()
        }])));
        ui.set_message_avatar(photo);
        ui.set_message_avatar_loaded(true);
        let snapshot = ui.window().take_snapshot().unwrap();
        let photos =
            ElementHandle::find_by_element_id(&ui, "SenderAvatar::photo-image").collect::<Vec<_>>();
        let mut sizes = photos
            .iter()
            .map(|p| {
                (
                    p.size().width.round() as u32,
                    p.size().height.round() as u32,
                )
            })
            .collect::<Vec<_>>();
        sizes.sort_unstable();
        assert_eq!(
            sizes,
            vec![(32, 32), (40, 40)],
            "source resolution {resolution} must not control displayed size"
        );
        for (index, pixel) in snapshot.as_slice().iter().enumerate() {
            if pixel.r > 240 && pixel.g < 10 && pixel.b > 240 {
                let x = (index as u32 % snapshot.width()) as f32;
                let y = (index as u32 / snapshot.width()) as f32;
                assert!(
                    photos.iter().any(|photo| {
                        let pos = photo.absolute_position();
                        let size = photo.size();
                        x >= pos.x
                            && x < pos.x + size.width
                            && y >= pos.y
                            && y < pos.y + size.height
                    }),
                    "avatar painted outside its bounds at {x},{y}"
                );
            }
        }
    }
    ui.hide().unwrap();
}

#[test]
fn account_wizard_provider_presets_theme_and_correlated_results() {
    use slint::Global;
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    let weak = ui.as_weak();
    ui.on_wizard_provider_selected(move |provider| {
        crate::accounts::select_provider(&weak.upgrade().unwrap(), &provider)
    });
    let state = crate::state::UiState::new();
    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    ui.window().set_size(LogicalSize::new(1440., 900.));
    ui.set_wizard_open(true);
    ui.show().unwrap();
    ui.set_google_available(false);
    ui.window().take_snapshot().unwrap();
    ElementHandle::find_by_accessible_label(&ui, "Google")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert_eq!(
        ui.get_wizard_provider(),
        "",
        "Unconfigured Google must remain unavailable"
    );
    ui.set_google_available(true);
    for dark in [true, false, true, false] {
        crate::Theme::get(&ui).set_dark(dark);
        ui.window().take_snapshot().unwrap();
        assert_eq!(
            crate::Palette::get(&ui).get_foreground().color().red() > 128,
            dark,
            "standard controls must follow the app theme"
        );
        let google = ElementHandle::find_by_accessible_label(&ui, "Google")
            .next()
            .unwrap();
        google.mock_single_click(PointerEventButton::Left);
        assert_eq!(ui.get_wizard_provider(), "google");
        assert!(!ui.get_wizard_manual());
        ElementHandle::find_by_accessible_label(&ui, "Back")
            .next()
            .unwrap()
            .mock_single_click(PointerEventButton::Left);
        assert_eq!(ui.get_wizard_provider(), "");
    }
    ElementHandle::find_by_accessible_label(&ui, "iCloud")
        .next()
        .unwrap()
        .mock_single_click(PointerEventButton::Left);
    assert!(ui.get_wizard_manual());
    assert_eq!(ui.get_imap_host(), "imap.mail.me.com");
    assert_eq!(ui.get_imap_port(), "993");
    assert!(!ui.get_starttls());
    assert!(ui.get_smtp_starttls());
    assert_eq!(ui.get_smtp_port(), "587");
    ui.set_password("temporary".into());
    ui.window().take_snapshot().unwrap();
    let show = ElementHandle::find_by_accessible_label(&ui, "Show password")
        .next()
        .unwrap();
    show.mock_single_click(PointerEventButton::Left);
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Hide password")
            .next()
            .is_some()
    );
    ui.set_password("".into());
    slint::platform::update_timers_and_animations();
    ui.window().take_snapshot().unwrap();
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Show password")
            .next()
            .is_some(),
        "a new password must start masked"
    );
    ui.window().set_size(LogicalSize::new(720., 480.));
    ui.window().take_snapshot().unwrap();
    let connect = ElementHandle::find_by_accessible_label(&ui, "Connect account")
        .next()
        .unwrap();
    assert!(connect.absolute_position().y + connect.size().height <= 480.);
    let field = ElementHandle::find_by_accessible_label(&ui, "Email address")
        .find(|field| {
            field.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::TextInput)
        })
        .unwrap();
    field.mock_single_click(PointerEventButton::Left);
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: "augustin@example.test".into(),
    });
    assert_eq!(ui.get_email(), "augustin@example.test");
    ui.set_wizard_attempt(7);
    ui.set_wizard_busy(true);
    ui.set_password("do-not-persist".into());
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountStatus {
            account: bang_core::AccountId("old@example.test".into()),
            status: bang_core::AccountStatus::Connected,
        },
    );
    assert!(
        ui.get_wizard_open(),
        "background sync cannot close account setup"
    );
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 6,
            error: None,
        },
    );
    assert!(
        ui.get_wizard_busy(),
        "a stale attempt cannot change the current one"
    );
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 7,
            error: Some(bang_core::events::SetupFailure::Authentication),
        },
    );
    assert!(!ui.get_wizard_busy());
    assert!(ui.get_wizard_open());
    assert!(!ui.get_wizard_status().is_empty());
    assert_eq!(
        ui.get_password(),
        "do-not-persist",
        "retry retains the form"
    );
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 7,
            error: None,
        },
    );
    assert!(!ui.get_wizard_open());
    assert_eq!(ui.get_password(), "");
    assert_eq!(ui.get_wizard_provider(), "");
}

#[test]
fn account_discovery_storage_recovery_and_reconnect_preserve_metadata() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    let state = crate::state::UiState::new();
    ui.window().set_size(LogicalSize::new(720., 480.));
    ui.set_wizard_open(true);
    ui.show().unwrap();
    crate::accounts::select_provider(&ui, "manual");
    assert!(!ui.get_wizard_settings_ready());
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Find settings")
            .next()
            .is_some()
    );
    ui.set_email("alice@example.test".into());
    ui.set_wizard_attempt(3);
    ui.set_wizard_busy(true);
    let settings = bang_core::discovery::DiscoveredSettings {
        name: "Example".into(),
        imap: bang_core::ImapConfig {
            host: "imap.example.test".into(),
            port: 993,
            tls: bang_core::TlsMode::Tls,
        },
        smtp: bang_core::SmtpConfig {
            host: "smtp.example.test".into(),
            port: 587,
            tls: bang_core::TlsMode::Starttls,
        },
        username: "incoming-alice".into(),
        smtp_username: "outgoing-alice".into(),
    };
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSettingsDiscovered {
            attempt: 2,
            result: bang_core::discovery::DiscoveryResult::Found(settings.clone()),
        },
    );
    assert!(!ui.get_wizard_settings_ready());
    assert!(ui.get_wizard_busy());
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSettingsDiscovered {
            attempt: 3,
            result: bang_core::discovery::DiscoveryResult::Found(settings.clone()),
        },
    );
    assert!(ui.get_wizard_settings_ready());
    assert!(!ui.get_wizard_advanced());
    assert!(!ui.get_wizard_busy());
    assert_eq!(ui.get_username(), "incoming-alice");
    assert_eq!(ui.get_smtp_username(), "outgoing-alice");
    ui.window().take_snapshot().unwrap();
    ui.set_wizard_busy(true);
    ui.set_password("not-for-ui-after-validation".into());
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 3,
            error: Some(bang_core::events::SetupFailure::SecureStorage),
        },
    );
    assert!(ui.get_wizard_storage_error());
    assert_eq!(ui.get_password(), "");
    for label in ["Try secure storage again", "Continue for this session"] {
        assert!(ElementHandle::find_by_accessible_label(&ui, label)
            .next()
            .is_some());
    }
    ui.window().take_snapshot().unwrap();
    let config = bang_core::AccountConfig {
        id: bang_core::AccountId("stable-id".into()),
        email: "alice@example.test".into(),
        imap: settings.imap,
        smtp: settings.smtp,
        auth: bang_core::AuthMethod::Password {
            username: settings.username,
            smtp_username: settings.smtp_username,
            password: "private".into(),
        },
    };
    store.save_account(&config).unwrap();
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountStatus {
            account: config.id.clone(),
            status: bang_core::AccountStatus::ReconnectRequired,
        },
    );
    assert!(ui.get_settings_accounts().row_data(0).unwrap().reconnect);
    crate::accounts::reconnect(&ui, &store.get_account("stable-id").unwrap().unwrap());
    assert_eq!(ui.get_wizard_reconnect_id(), "stable-id");
    assert_eq!(ui.get_email(), "alice@example.test");
    assert_eq!(ui.get_username(), "incoming-alice");
    assert_eq!(ui.get_smtp_username(), "outgoing-alice");
    assert_eq!(ui.get_password(), "");
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountStatus {
            account: config.id,
            status: bang_core::AccountStatus::Connected,
        },
    );
    assert!(!ui.get_settings_accounts().row_data(0).unwrap().reconnect);
    ui.hide().unwrap();
}

#[test]
fn welcome_navigation_preferences_and_account_handoff() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    let dir = std::env::temp_dir().join(format!(
        "bang-welcome-ui-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let path = dir.join("config.toml");
    crate::welcome::wire(&ui, path.clone(), false);
    crate::i18n::select("en");
    ui.window().set_size(LogicalSize::new(720., 480.));
    ui.show().unwrap();
    let settle = || {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1500));
        ui.window().take_snapshot().unwrap();
    };
    let click = |label: &str| {
        ui.window().take_snapshot().unwrap();
        let element = ElementHandle::find_by_accessible_label(&ui, label)
            .next()
            .unwrap_or_else(|| panic!("missing {label}"));
        let position = element.absolute_position();
        assert!(position.x >= 0. && position.y >= 0.);
        assert!(
            position.x + element.size().width <= 720. && position.y + element.size().height <= 480.,
            "{label} clipped"
        );
        element.mock_single_click(PointerEventButton::Left);
    };
    settle();
    let composed = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = composed.clone();
    ui.on_compose_clicked(move || flag.set(true));
    let quit = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = quit.clone();
    ui.on_quit(move || flag.set(true));
    let shortcut = |key: &str| {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
    };
    shortcut("n");
    shortcut("f");
    assert!(!composed.get(), "welcome must block inbox shortcuts");
    shortcut("q");
    assert!(quit.get(), "Ctrl+Q must remain available during setup");
    assert!(ui.get_welcome_open());
    assert!(crate::Theme::get(&ui).get_blocked());
    assert_eq!(
        crate::config::Config::load_from(&path).welcome_completed,
        Some(false)
    );
    assert!(!ui.get_reader_shown());
    assert!(ElementHandle::find_by_accessible_label(&ui, "New message")
        .next()
        .is_none());
    click("Get started");
    settle();
    assert_eq!(ui.get_welcome_step(), 1);
    // Exercise the same callbacks that the welcome screen sends to Settings.
    let weak = ui.as_weak();
    let preference_path = path.clone();
    ui.on_settings_theme_selected(move |theme| {
        let ui = weak.upgrade().unwrap();
        let mut config = crate::config::Config::load_from(&preference_path);
        config.theme = theme.to_string();
        config.save_to(&preference_path).unwrap();
        crate::apply_theme(&ui, &theme);
        ui.set_settings_theme(theme);
    });
    let weak = ui.as_weak();
    let preference_path = path.clone();
    ui.on_settings_language_selected(move |language| {
        let ui = weak.upgrade().unwrap();
        let mut config = crate::config::Config::load_from(&preference_path);
        config.language = language.to_string();
        config.save_to(&preference_path).unwrap();
        crate::i18n::select(&language);
        ui.set_settings_language(language);
    });
    click("Dark");
    settle();
    assert!(crate::Theme::get(&ui).get_dark());
    click("Français");
    settle();
    assert_eq!(ui.get_settings_language(), "fr");
    let config = crate::config::Config::load_from(&path);
    assert_eq!(config.theme, "dark");
    assert_eq!(config.welcome_completed, Some(false));
    assert!(
        crate::welcome::eligible(&config, true, false),
        "preference saves must not complete setup"
    );
    click("Continuer");
    settle();
    assert_eq!(ui.get_welcome_step(), 2);
    click("Ajouter un compte");
    settle();
    assert!(ui.get_wizard_open());
    assert!(ui.get_welcome_open());
    // Return from the real account UI with Escape, without writing credentials.
    let weak = ui.as_weak();
    ui.on_wizard_cancel(move || {
        let ui = weak.upgrade().unwrap();
        ui.set_wizard_open(false);
        ui.invoke_welcome_focus();
    });
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Escape.into(),
    });
    settle();
    assert!(!ui.get_wizard_open());
    assert_eq!(ui.get_welcome_step(), 2);
    click("Retour");
    settle();
    assert_eq!(ui.get_welcome_step(), 1);
    // Primary focus follows the step, so Enter advances without a pointer.
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: "\n".into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: "\n".into() });
    settle();
    assert_eq!(ui.get_welcome_step(), 2);
    click("Configurer plus tard");
    settle();
    assert!(!ui.get_welcome_open());
    assert!(!crate::Theme::get(&ui).get_blocked());
    let config = crate::config::Config::load_from(&path);
    assert_eq!(config.welcome_completed, Some(true));
    assert_eq!(config.language, "fr");
    assert_eq!(config.theme, "dark");
    crate::welcome::wire(&ui, path, false);
    assert!(
        !ui.get_welcome_open(),
        "completed setup must not reappear with no accounts"
    );
    ui.hide().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn welcome_account_result_and_completion_failure_are_recoverable() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    crate::i18n::select("en");
    let dir = std::env::temp_dir().join(format!(
        "bang-welcome-result-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let blocker = dir.join("blocked");
    std::fs::write(&blocker, "not a directory").unwrap();
    let path = blocker.join("config.toml");
    crate::welcome::wire(&ui, path.clone(), false);
    assert!(ui.get_welcome_open());
    assert!(!ui.get_welcome_error().is_empty());
    ui.set_welcome_step(2);
    ui.set_welcome_motion(false);
    ui.window().set_size(LogicalSize::new(720., 480.));
    ui.show().unwrap();
    let state = crate::state::UiState::new();
    let store = bang_core::Store::open(&dir.join("mail.db")).unwrap();
    ui.set_wizard_open(true);
    ui.set_wizard_busy(true);
    ui.set_wizard_attempt(4);
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 3,
            error: None,
        },
    );
    assert!(
        ui.get_wizard_busy(),
        "stale success must not complete onboarding"
    );
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 4,
            error: Some(bang_core::events::SetupFailure::Network),
        },
    );
    assert!(ui.get_wizard_open());
    assert!(ui.get_welcome_open());
    assert!(!ui.get_wizard_status().is_empty());
    let account = bang_core::AccountConfig {
        id: bang_core::AccountId("welcome@example.test".into()),
        email: "welcome@example.test".into(),
        imap: bang_core::ImapConfig {
            host: "imap.example.test".into(),
            port: 993,
            tls: bang_core::TlsMode::Tls,
        },
        smtp: bang_core::SmtpConfig {
            host: "smtp.example.test".into(),
            port: 465,
            tls: bang_core::TlsMode::Tls,
        },
        auth: bang_core::AuthMethod::Password {
            username: "welcome@example.test".into(),
            password: "test".into(),
            smtp_username: String::new(),
        },
    };
    store.save_account(&account).unwrap();
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountSetupFinished {
            attempt: 4,
            error: None,
        },
    );
    assert!(!ui.get_wizard_open());
    assert!(ui.get_welcome_open());
    assert_eq!(ui.get_account_count(), 1);
    assert!(!ui.get_welcome_error().is_empty());
    assert_eq!(
        store.get_accounts().unwrap().len(),
        1,
        "save failure must retain the connected account"
    );
    ui.window().take_snapshot().unwrap();
    let retry = ElementHandle::find_by_accessible_label(&ui, "Open inbox")
        .next()
        .unwrap();
    assert!(retry.absolute_position().y + retry.size().height <= 480.);
    std::fs::remove_file(&blocker).unwrap();
    std::fs::create_dir(&blocker).unwrap();
    retry.mock_single_click(PointerEventButton::Left);
    assert!(!ui.get_welcome_open());
    assert_eq!(
        crate::config::Config::load_from(&path).welcome_completed,
        Some(true)
    );
    ui.hide().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn welcome_motion_settles_and_desktop_reduced_motion_stops_it() {
    slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
        i_slint_backend_testing::TestingBackendOptions {
            renderer_name: Some("software".into()),
            mock_time: true,
            ..Default::default()
        },
    )))
    .unwrap();
    let ui = BangApp::new().unwrap();
    crate::i18n::select("en");
    ui.window().set_size(LogicalSize::new(1024., 768.));
    ui.set_welcome_open(true);
    ui.show().unwrap();
    let first = ui.window().take_snapshot().unwrap();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(450));
    let middle = ui.window().take_snapshot().unwrap();
    assert_ne!(first.as_bytes(), middle.as_bytes(), "envelope must animate");
    ui.set_welcome_motion(false);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let reduced = ui.window().take_snapshot().unwrap();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
    assert_eq!(
        reduced.as_bytes(),
        ui.window().take_snapshot().unwrap().as_bytes()
    );
    assert!(!ui.window().has_active_animations());
    ui.set_welcome_motion(true);
    ui.set_welcome_step(1);
    ui.window().take_snapshot().unwrap();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let entering = ui.window().take_snapshot().unwrap();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(300));
    let settled = ui.window().take_snapshot().unwrap();
    assert_ne!(
        entering.as_bytes(),
        settled.as_bytes(),
        "step change must animate"
    );
    ui.set_welcome_step(2);
    ui.window().take_snapshot().unwrap();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(80));
    ui.set_welcome_open(false);
    // Remove the conditional tree, then refresh Slint's per-frame activity flag.
    // The flag can still reflect bindings evaluated before that tree was removed.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    ui.window().take_snapshot().unwrap();
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(16));
    ui.window().take_snapshot().unwrap();
    assert!(
        !ui.window().has_active_animations(),
        "closed welcome must not animate"
    );
    ui.hide().unwrap();
}

#[test]
fn account_removal_updates_sidebar_selection_and_clears_pending_state() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = BangApp::new().unwrap();
    let store = bang_core::Store::open(std::path::Path::new(":memory:")).unwrap();
    let state = crate::state::UiState::new();
    state
        .removing_accounts
        .lock()
        .unwrap()
        .insert("removed".into());
    ui.set_selected_folder(99);
    state.apply(
        &ui,
        &store,
        bang_core::MailEvent::AccountRemoved {
            account: bang_core::AccountId("removed".into()),
            error: None,
        },
    );
    assert_eq!(ui.get_selected_folder(), bang_core::UNIFIED_INBOX_ID as i32);
    assert!(state.removing_accounts.lock().unwrap().is_empty());
}
