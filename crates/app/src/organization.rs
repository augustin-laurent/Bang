//! Organization controls and view queries. Local mutations go through the core.
use crate::{bridge::Bridge, i18n::tr, state::UiState, BangApp, CategoryRow, LabelRow};
use bang_core::{organization::MailCategory, MessageSummary, Store};
use slint::{ComponentHandle, Model};
use std::sync::{atomic::Ordering, Arc};

pub(crate) const COLORS: [&str; 6] = [
    "#3977b8", "#30845b", "#b96c17", "#ba4655", "#8555a6", "#6b7280",
];
pub(crate) fn color(hex: &str) -> slint::Color {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x3977b8);
    slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}
fn category_name(category: MailCategory) -> String {
    tr(match category {
        MailCategory::Primary => "Primary",
        MailCategory::Transactions => "Transactions",
        MailCategory::Updates => "Updates",
        MailCategory::Promotions => "Promotions",
    })
}
fn inbox(store: &Store, folder: i64) -> bool {
    folder == bang_core::UNIFIED_INBOX_ID
        || store
            .get_folder_by_id(folder)
            .ok()
            .flatten()
            .is_some_and(|f| f.path.eq_ignore_ascii_case("INBOX"))
}

pub(crate) fn refresh(ui: &BangApp, store: &Store, folder: i64) {
    let labels = store.labels().unwrap_or_default();
    if ui.get_selected_label() >= 0
        && !labels
            .iter()
            .any(|l| l.id == i64::from(ui.get_selected_label()))
    {
        ui.set_selected_label(-1);
    }
    let organization = if ui.get_organization_message() >= 0 {
        store
            .message_organization(i64::from(ui.get_organization_message()))
            .ok()
    } else {
        None
    };
    let rows = labels
        .iter()
        .map(|l| LabelRow {
            id: l.id as i32,
            name: l.name.clone().into(),
            tint: color(&l.color),
            color_index: COLORS.iter().position(|c| *c == l.color).unwrap_or(0) as i32,
            assigned: organization
                .as_ref()
                .is_some_and(|o| o.labels.contains(&l.id)),
        })
        .collect::<Vec<_>>();
    let previous = ui.get_labels();
    let model = crate::state::reconcile_rows(&previous, rows, |r| r.id);
    if model != previous {
        ui.set_labels(model);
    }
    if let Some(o) = organization {
        ui.set_organization_effective(category_name(o.category).into());
        ui.set_organization_message_override(o.message_override);
        ui.set_organization_sender_override(o.sender_override);
        let index = match o.category {
            MailCategory::Primary => 1,
            MailCategory::Transactions => 2,
            MailCategory::Updates => 3,
            MailCategory::Promotions => 4,
        };
        ui.set_organization_category(if o.message_override || o.sender_override {
            index
        } else {
            0
        });
    }
    let is_inbox = inbox(store, folder) && ui.get_selected_label() < 0;
    ui.set_categories_visible(is_inbox);
    let counts = store
        .category_counts((folder != bang_core::UNIFIED_INBOX_ID).then_some(folder))
        .unwrap_or_default();
    let mut categories = vec![CategoryRow {
        key: "".into(),
        name: tr("All").into(),
        short_name: tr("All").into(),
        unread: counts.iter().sum(),
    }];
    for (i, category) in [
        MailCategory::Primary,
        MailCategory::Transactions,
        MailCategory::Updates,
        MailCategory::Promotions,
    ]
    .into_iter()
    .enumerate()
    {
        categories.push(CategoryRow {
            key: category.key().into(),
            name: category_name(category).into(),
            short_name: tr(["Primary", "Orders", "Updates", "Offers"][i]).into(),
            unread: counts[i],
        });
    }
    let previous = ui.get_categories();
    let model = crate::state::reconcile_rows(&previous, categories, |r| r.key.clone());
    if model != previous {
        ui.set_categories(model);
    }
    if !ui.get_search_active() {
        let title = if let Some(label) = labels
            .iter()
            .find(|l| l.id == i64::from(ui.get_selected_label()))
        {
            label.name.clone()
        } else if folder == bang_core::UNIFIED_INBOX_ID {
            tr("All Inboxes")
        } else {
            store
                .get_folder_by_id(folder)
                .ok()
                .flatten()
                .map(|f| f.name)
                .unwrap_or_default()
        };
        ui.set_folder_name(title.into());
    }
}

pub(crate) fn messages(
    ui: &BangApp,
    store: &Store,
    folder: i64,
) -> bang_core::Result<Vec<MessageSummary>> {
    refresh(ui, store, folder);
    let label = (ui.get_selected_label() >= 0).then_some(i64::from(ui.get_selected_label()));
    let category = if label.is_none() && inbox(store, folder) {
        MailCategory::parse(ui.get_selected_category().as_str())
    } else {
        None
    };
    store.organized_messages(
        if label.is_some() || folder == bang_core::UNIFIED_INBOX_ID {
            None
        } else {
            Some(folder)
        },
        label,
        category,
        200,
    )
}

pub(crate) fn wire(ui: &BangApp, store: &Arc<Store>, state: &Arc<UiState>, bridge: Bridge) {
    ui.on_category_selected({
        let weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        move |key| {
            if let Some(ui) = weak.upgrade() {
                state.selected_messages.lock().unwrap().clear();
                state.selection_anchor.store(-1, Ordering::SeqCst);
                ui.set_selection_count(0);
                ui.set_selected_category(key);
                state.reload_messages(&ui, &store);
                ui.invoke_reset_message_scroll();
            }
        }
    });
    ui.on_label_selected({
        let weak = ui.as_weak();
        let state = state.clone();
        let store = store.clone();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                state.selected_messages.lock().unwrap().clear();
                state.selection_anchor.store(-1, Ordering::SeqCst);
                ui.set_selection_count(0);
                ui.set_selected_label(id);
                ui.set_search_text("".into());
                ui.set_search_active(false);
                state.search_active.store(false, Ordering::SeqCst);
                state.reload_messages(&ui, &store);
                ui.invoke_reset_message_scroll();
            }
        }
    });
    ui.on_label_context({
        let weak = ui.as_weak();
        move |id, x, y| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let labels = ui.get_labels();
            let Some(label) = (0..labels.row_count())
                .filter_map(|index| labels.row_data(index))
                .find(|label| label.id == id)
            else {
                return;
            };
            ui.set_menu_label(id);
            ui.set_organization_edit_id(id);
            ui.set_organization_edit_name(label.name);
            ui.set_organization_edit_color(label.color_index);
            ui.set_menu_x(x);
            ui.set_menu_y(y);
            ui.invoke_show_label_menu();
        }
    });
    ui.on_organization_opened({
        let weak = ui.as_weak();
        let store = store.clone();
        let state = state.clone();
        move |manage| {
            if let Some(ui) = weak.upgrade() {
                if !manage && ui.get_selected_message() < 0 {
                    return;
                }
                ui.set_managing_labels(manage);
                ui.set_organization_message(if manage {
                    -1
                } else {
                    ui.get_selected_message()
                });
                ui.set_organization_error("".into());
                refresh(&ui, &store, state.selected_folder.load(Ordering::SeqCst));
                ui.set_organization_open(true);
            }
        }
    });
    ui.on_organization_close({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_organization_open(false);
                ui.invoke_focus_chrome();
            }
        }
    });
    ui.on_category_saved({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |index, sender| {
            if let Some(ui) = weak.upgrade() {
                ui.set_organization_error("".into());
                let category = match index {
                    1 => Some(MailCategory::Primary),
                    2 => Some(MailCategory::Transactions),
                    3 => Some(MailCategory::Updates),
                    4 => Some(MailCategory::Promotions),
                    _ => None,
                };
                bridge.send(bang_core::UiCommand::SetCategory {
                    message_id: i64::from(ui.get_organization_message()),
                    category,
                    sender,
                });
            }
        }
    });
    ui.on_label_assigned({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |id, assigned| {
            if let Some(ui) = weak.upgrade() {
                ui.set_organization_error("".into());
                bridge.send(bang_core::UiCommand::SetMessageLabel {
                    message_id: i64::from(ui.get_organization_message()),
                    label_id: i64::from(id),
                    assigned,
                });
            }
        }
    });
    ui.on_label_saved({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |id, name: slint::SharedString, color: slint::SharedString| {
            if let Some(ui) = weak.upgrade() {
                ui.set_organization_error("".into());
                bridge.send(bang_core::UiCommand::SaveLabel {
                    id: (id >= 0).then_some(i64::from(id)),
                    name: name.to_string(),
                    color: color.to_string(),
                });
            }
        }
    });
    ui.on_label_deleted({
        let weak = ui.as_weak();
        move |id| {
            if let Some(ui) = weak.upgrade() {
                ui.set_organization_error("".into());
                bridge.send(bang_core::UiCommand::DeleteLabel { id: i64::from(id) });
            }
        }
    });
}
