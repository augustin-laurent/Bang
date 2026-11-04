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
    if std::env::var_os("BANG_SNAPSHOT").is_none() {
        crate::gtk_host::init()?;
    }
    let ui = BangApp::new()?;
    if std::env::var_os("BANG_SNAPSHOT").is_none() {
        crate::gtk_host::attach(&ui, None, None);
    }
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
    let rows:Vec<_>=names.iter().enumerate().map(|(i,name)|MessageRow {id:i as i32+1,sender:(*name).into(),sender_address:format!("sender{i}@example.test").into(),initials:crate::avatars::initials(name, "").into(),avatar_tint:crate::avatars::tint(&format!("sender{i}@example.test")),subject:subjects[i].into(),date:if i==0 {"10:42".into()}else if i==1 {"09:18".into()}else {"31/10/2026".into()},preview:"Voici un récapitulatif des dernières pistes que nous avons évoquées, avec quelques ajustements…".into(),unread:i==1||i==2,..Default::default()}).collect();
    ui.set_messages(slint::ModelRc::new(VecModel::from(rows)));
    ui.set_message_sender(names[0].into());
    ui.set_message_subject(subjects[0].into());
    ui.set_message_recipients("augustin@example.fr".into());
    ui.set_message_initials("CM".into());
    ui.set_message_avatar_tint(crate::avatars::tint("sender0@example.test"));
    ui.set_categories_visible(true);
    ui.set_categories(slint::ModelRc::new(VecModel::from(vec![
        crate::CategoryRow {
            key: "".into(),
            name: crate::i18n::tr("All").into(),
            short_name: crate::i18n::tr("All").into(),
            unread: 2,
        },
        crate::CategoryRow {
            key: "primary".into(),
            name: crate::i18n::tr("Primary").into(),
            short_name: crate::i18n::tr("Primary").into(),
            unread: 1,
        },
        crate::CategoryRow {
            key: "transactions".into(),
            name: crate::i18n::tr("Transactions").into(),
            short_name: crate::i18n::tr("Orders").into(),
            unread: 0,
        },
        crate::CategoryRow {
            key: "updates".into(),
            name: crate::i18n::tr("Updates").into(),
            short_name: crate::i18n::tr("Updates").into(),
            unread: 1,
        },
        crate::CategoryRow {
            key: "promotions".into(),
            name: crate::i18n::tr("Promotions").into(),
            short_name: crate::i18n::tr("Offers").into(),
            unread: 0,
        },
    ])));
    ui.set_labels(slint::ModelRc::new(VecModel::from(vec![
        crate::LabelRow {
            id: 1,
            name: "Travail".into(),
            tint: crate::organization::color("#3977b8"),
            assigned: true,
            ..Default::default()
        },
        crate::LabelRow {
            id: 2,
            name: "Personnel".into(),
            tint: crate::organization::color("#30845b"),
            color_index: 1,
            ..Default::default()
        },
    ])));
    ui.set_message_date("10:42".into());
    ui.set_message_body("Bonjour Augustin,\n\nVoici un récapitulatif des dernières pistes que nous avons évoquées, avec quelques ajustements sur les proportions finales. L’ensemble reste volontairement sobre pour conserver une navigation lisible et agréable au quotidien.\n\nN’hésite pas à me dire ce que tu en penses et si tu vois d’autres points à affiner avant la prochaine itération.\n\nÀ bientôt,\nCamille".into());
    ui.set_settings_accounts(slint::ModelRc::new(VecModel::from(vec![
        AccountRow {
            reconnect: false,
            storage_locked: false,
            provider: "".into(),
            id: "1".into(),
            email: "augustin@example.fr".into(),
        },
        AccountRow {
            reconnect: false,
            storage_locked: false,
            provider: "".into(),
            id: "2".into(),
            email: "augustin@atelier.example".into(),
        },
    ])));
    let weak = ui.as_weak();
    ui.on_category_selected(move |key| {
        if let Some(ui) = weak.upgrade() {
            ui.set_selected_category(key);
        }
    });
    let weak = ui.as_weak();
    ui.on_organization_opened(move |manage| {
        if let Some(ui) = weak.upgrade() {
            ui.set_managing_labels(manage);
            ui.set_organization_effective(crate::i18n::tr("Primary").into());
            ui.set_organization_open(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_organization_close(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_organization_open(false);
        }
    });
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
    ui.on_wizard_provider_selected(move |provider| {
        if let Some(ui) = weak.upgrade() {
            crate::accounts::select_provider(&ui, &provider);
        }
    });
    let weak = ui.as_weak();
    ui.on_wizard_cancel(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_wizard_open(false);
        }
    });
    let weak = ui.as_weak();
    ui.on_settings_language_selected(move |mode| {
        if let Some(ui) = weak.upgrade() {
            crate::i18n::select(&mode);
            ui.set_settings_language(mode);
        }
    });
    let weak = ui.as_weak();
    ui.on_welcome_add_account(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_wizard_open(true);
        }
    });
    let weak = ui.as_weak();
    ui.on_welcome_finish(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_welcome_open(false);
            ui.invoke_focus_chrome();
        }
    });
    ui.set_settings_theme(
        std::env::var("BANG_THEME")
            .unwrap_or("system".into())
            .into(),
    );
    ui.set_settings_language(std::env::var("BANG_LANGUAGE").unwrap_or("fr".into()).into());
    ui.set_welcome_motion(std::env::var("BANG_REDUCED_MOTION").unwrap_or_default() != "1");
    if let Ok(surface) = std::env::var("BANG_SURFACE") {
        if surface.starts_with("welcome") {
            ui.set_account_count(if surface == "welcome-connected" { 1 } else { 0 });
            ui.set_selected_message(-1);
            ui.set_welcome_step(match surface.as_str() {
                "welcome-preferences" => 1,
                "welcome-connect" | "welcome-error" | "welcome-connected" => 2,
                _ => 0,
            });
            ui.set_welcome_open(true);
            if surface == "welcome-error" {
                ui.set_welcome_error(crate::i18n::tr("Could not save settings. Check access to your configuration folder and try again.").into());
            }
        }
        ui.set_settings_open(surface == "settings");
        ui.set_organization_open(surface == "organization" || surface == "labels");
        ui.set_managing_labels(surface == "labels");
        ui.set_organization_effective(crate::i18n::tr("Primary").into());
        ui.set_wizard_open(surface.starts_with("wizard"));
        ui.set_wizard_manual(surface == "wizard-manual");
        if surface == "wizard-google" {
            ui.set_wizard_provider("google".into());
        }
        if surface == "wizard-busy" {
            ui.set_wizard_provider("microsoft".into());
            ui.set_wizard_busy(true);
            ui.set_wizard_status(crate::i18n::tr("Waiting for browser sign-in…").into());
        }
        if surface == "wizard-icloud" {
            crate::accounts::select_provider(&ui, "icloud");
        }
        if surface == "wizard-manual" {
            crate::accounts::select_provider(&ui, "manual");
            ui.set_wizard_settings_ready(true);
            ui.set_wizard_advanced(true);
        }
        if surface == "wizard-storage" {
            crate::accounts::select_provider(&ui, "google");
            ui.set_wizard_storage_error(true);
            ui.set_wizard_status(
                crate::i18n::tr("Unlock your system keyring, or continue for this session.").into(),
            );
        }
        if surface == "wizard-error" {
            ui.set_wizard_provider("google".into());
            ui.set_wizard_status(crate::i18n::tr("Access was not granted. Check the selected account or ask your organization to allow mail access.").into());
        }
    }
    if std::env::var_os("BANG_HTML_PREVIEW").is_some() {
        let message: bang_core::MessageFull = serde_json::from_value(serde_json::json!({
            "summary": {"id":1,"account_id":"preview","folder_id":1,"uid":1,
                "message_id":"preview","from":{"name":"Atelier été","address":"atelier@example.test"},
                "to":[],"cc":[],"subject":"Les offres d’été — déjà disponibles","date":0,"flags":"","preview":"","has_attachments":false},
            "body_text":"Les offres d’été\nUne mise en page lisible, avec des accents corrects.\n3,14 €",
            "body_html":"<html><body style='background:#fff;color:#222'><table style='max-width:620px;margin:auto;border-collapse:collapse;width:100%'><tr><td style='padding:28px;background:#174f47;color:#fff'><h1>Les offres d’été</h1><p>Déjà disponibles, à découvrir aujourd’hui.</p></td></tr><tr><td style='padding:28px'><p>Bonjour Augustin,</p><p>Une mise en page lisible, avec des accents corrects.</p><table style='width:100%'><tr><td style='background:#f0e9d9;padding:20px'>Livraison incluse</td><td style='background:#e1eddf;padding:20px'>À partir de <b>3,14 €</b></td></tr></table><p><a href='https://example.test'>Voir les nouveautés</a></p></td></tr></table></body></html>","attachments":[]
        })).unwrap();
        ui.set_message_sender(message.summary.from.name.clone().into());
        ui.set_message_subject(message.summary.subject.clone().into());
        ui.set_message_body(message.body_text.clone().into());
        crate::reader::load(&ui, &message);
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
        slint::Timer::single_shot(
            std::time::Duration::from_millis(
                std::env::var("BANG_SNAPSHOT_DELAY_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1400),
            ),
            move || {
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
            },
        );
    }
    ui.run()
}
