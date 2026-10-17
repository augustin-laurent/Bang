//! Offline visual fixture, deliberately isolated from accounts and network services.
use crate::{AccountRow, BangApp, FolderRow, MessageRow, Theme};
use slint::{ComponentHandle, Global, VecModel};
/// Run the deterministic visual fixture. BANG_SNAPSHOT saves a screenshot and exits.
pub fn run() -> Result<(), slint::PlatformError> {
    #[cfg(feature = "preview-renderer")]
    if std::env::var_os("BANG_SNAPSHOT").is_some() {
        slint::platform::set_platform(Box::new(i_slint_backend_testing::TestingBackend::new(
            i_slint_backend_testing::TestingBackendOptions {
                mock_time: false,
                threading: true,
                renderer_name: Some("software".into()),
            },
        )))
        .map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    }
    let ui = BangApp::new()?;
    crate::i18n::select(&std::env::var("BANG_LANGUAGE").unwrap_or("fr".into()));
    Theme::get(&ui).set_dark(std::env::var("BANG_THEME").unwrap_or_default() == "dark");
    Theme::get(&ui).invoke_sync_palette();
    ui.set_account_count(2);
    ui.set_selected_folder(0);
    ui.set_selected_message(1);
    ui.set_folder_name(crate::i18n::tr("All Inboxes").into());
    let mut folders = vec![FolderRow {
        id: 0,
        name: crate::i18n::tr("All Inboxes").into(),
        unread: 2,
        kind: "inbox".into(),
        ..Default::default()
    }];
    for (i, email) in ["augustin@example.fr", "augustin@atelier.example"]
        .iter()
        .enumerate()
    {
        folders.push(FolderRow {
            is_header: true,
            name: (*email).into(),
            id: -1,
            account_id: (*email).into(),
            ..Default::default()
        });
        for (j, name, kind) in [
            (1, "Boîte de réception", "inbox"),
            (2, "Brouillons", "draft"),
            (3, "Envoyés", "sent"),
            (4, "Corbeille", "trash"),
        ] {
            folders.push(FolderRow {
                id: (i * 10 + j) as i32,
                name: name.into(),
                kind: kind.into(),
                unread: if j == 1 { 1 } else { 0 },
                ..Default::default()
            });
        }
    }
    ui.set_folders(slint::ModelRc::new(VecModel::from(folders)));
    let names = [
        "Camille Moreau",
        "Atelier Nord",
        "Léa Bernard",
        "Thomas Petit",
        "Julie Martin",
        "Antoine Lefèvre",
        "Claire Dubois",
        "Hugo Renault",
    ];
    let subjects = [
        "Les dernières pistes pour Bang",
        "Point de vendredi",
        "Quelques nouvelles",
        "Revue de la maquette",
        "Planning du projet",
        "Ressources graphiques",
        "Retour utilisateur",
        "Prochaines étapes",
    ];
    let rows:Vec<_>=names.iter().enumerate().map(|(i,name)|MessageRow {id:i as i32+1,sender:(*name).into(),subject:subjects[i].into(),date:if i==0 {"10:42".into()}else if i==1 {"09:18".into()}else {"31/10/2026".into()},preview:"Voici un récapitulatif des dernières pistes que nous avons évoquées, avec quelques ajustements…".into(),unread:i==1||i==2,..Default::default()}).collect();
    ui.set_messages(slint::ModelRc::new(VecModel::from(rows)));
    ui.set_message_sender(names[0].into());
    ui.set_message_subject(subjects[0].into());
    ui.set_message_recipients("augustin@example.fr".into());
    ui.set_message_initials("CM".into());
    ui.set_message_date("10:42".into());
    ui.set_message_body("Bonjour Augustin,\n\nVoici un récapitulatif des dernières pistes que nous avons évoquées, avec quelques ajustements sur les proportions finales. L’ensemble reste volontairement sobre pour conserver une navigation lisible et agréable au quotidien.\n\nN’hésite pas à me dire ce que tu en penses et si tu vois d’autres points à affiner avant la prochaine itération.\n\nÀ bientôt,\nCamille".into());
    ui.set_settings_accounts(slint::ModelRc::new(VecModel::from(vec![
        AccountRow {
            id: "1".into(),
            email: "augustin@example.fr".into(),
        },
        AccountRow {
            id: "2".into(),
            email: "augustin@atelier.example".into(),
        },
    ])));
    let weak = ui.as_weak();
    ui.on_settings_opened(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_settings_open(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_settings_close(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_settings_open(false);
        }
    });
    let weak = ui.as_weak();
    ui.on_settings_theme_selected(move |m| {
        if let Some(ui) = weak.upgrade() {
            Theme::get(&ui).set_dark(m == "dark");
            Theme::get(&ui).invoke_sync_palette();
            ui.set_settings_theme(m);
        }
    });
    let weak = ui.as_weak();
    ui.on_message_selected(move |id| {
        if let Some(ui) = weak.upgrade() {
            ui.set_selected_message(id);
        }
    });
    let weak = ui.as_weak();
    ui.on_wizard_cancel(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_wizard_open(false);
        }
    });
    if let Ok(surface) = std::env::var("BANG_SURFACE") {
        ui.set_settings_open(surface == "settings");
        ui.set_wizard_open(surface.starts_with("wizard"));
        ui.set_wizard_manual(surface == "wizard-manual");
    }
    if let Ok(width) = std::env::var("BANG_WIDTH") {
        ui.window().set_size(slint::LogicalSize::new(
            width.parse().unwrap_or(1440.),
            std::env::var("BANG_HEIGHT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(900.),
        ));
    }
    if let Ok(path) = std::env::var("BANG_SNAPSHOT") {
        if std::env::var_os("BANG_WIDTH").is_none() {
            ui.window().set_size(slint::LogicalSize::new(1440., 900.));
        }
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(800), move || {
            if let Some(ui) = weak.upgrade() {
                match ui.window().take_snapshot() {
                    Ok(pixels) => {
                        image::save_buffer(
                            &path,
                            pixels.as_bytes(),
                            pixels.width(),
                            pixels.height(),
                            image::ColorType::Rgba8,
                        )
                        .expect("save visual fixture");
                    }
                    Err(e) => eprintln!("snapshot: {e}"),
                }
                ui.hide().ok();
                slint::quit_event_loop().ok();
            }
        });
    }
    ui.run()
}
