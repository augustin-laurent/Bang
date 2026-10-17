//! Exercise real Slint controls against an isolated software backend.
use crate::{BangApp, FolderRow, MessageRow};
use i_slint_backend_testing::ElementHandle;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalSize, Model, VecModel};
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
