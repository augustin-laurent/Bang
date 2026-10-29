//! UI-thread image reuse. Every completion is matched by sender identity.
use crate::{
    organization::{color, COLORS},
    BangApp, MessageRow,
};
use slint::{ComponentHandle, Model};
use std::{cell::RefCell, collections::HashMap};
// Slint 1.18 compares two empty Images as unequal. Reuse a transparent pixel
// for absent row photos so unchanged rows still reconcile without notifications.
thread_local! {static PLACEHOLDER:slint::Image=slint::Image::from_rgba8(slint::SharedPixelBuffer::new(1,1));}
pub(crate) fn placeholder() -> slint::Image {
    PLACEHOLDER.with(Clone::clone)
}
thread_local! {static IMAGES:RefCell<HashMap<String,slint::Image>>=RefCell::new(HashMap::new());}

pub(crate) fn initials(name: &str, address: &str) -> String {
    let text = if name.trim().is_empty() {
        address.split('@').next().unwrap_or("?")
    } else {
        name
    };
    let words: Vec<_> = text
        .split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .collect();
    let mut chars = words
        .iter()
        .take(2)
        .filter_map(|w| w.chars().next())
        .collect::<String>()
        .to_uppercase();
    if chars.is_empty() {
        chars.push('?');
    }
    chars
}
pub(crate) fn tint(address: &str) -> slint::Color {
    let hash = bang_core::organization::sender_key(address)
        .bytes()
        .fold(2166136261u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(16777619)
        });
    color(COLORS[hash as usize % COLORS.len()])
}
pub(crate) fn image(address: &str) -> slint::Image {
    IMAGES.with(|images| {
        images
            .borrow()
            .get(&bang_core::organization::sender_key(address))
            .cloned()
            .unwrap_or_default()
    })
}
pub(crate) fn decorate(ui: &BangApp, row: &mut MessageRow) {
    row.has_avatar = ui.get_online_avatars() && image(&row.sender_address).size().width > 0;
    row.avatar = if ui.get_online_avatars() {
        image(&row.sender_address)
    } else {
        placeholder()
    };
    if !row.has_avatar {
        row.avatar = placeholder();
    }
}
pub(crate) fn ready(ui: &BangApp, address: &str, path: &str) {
    if !ui.get_online_avatars() {
        return;
    }
    let Ok(photo) = slint::Image::load_from_path(std::path::Path::new(path)) else {
        return;
    };
    IMAGES.with(|images| {
        let mut images = images.borrow_mut();
        // Bounded by design: one decoded image per unique sender, cleared
        // rather than grown without limit in long sessions.
        if images.len() >= 512 {
            images.clear();
        }
        images.insert(address.into(), photo.clone());
    });
    let model = ui.get_messages();
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if bang_core::organization::sender_key(&row.sender_address) == address {
                row.avatar = photo.clone();
                row.has_avatar = true;
                model.set_row_data(i, row);
            }
        }
    }
    if ui.get_selected_message() >= 0
        && bang_core::organization::sender_key(&ui.get_message_sender_address()) == address
    {
        ui.set_message_avatar(photo);
        ui.set_message_avatar_loaded(true);
    }
}
pub(crate) fn wire(ui: &BangApp, bridge: crate::bridge::Bridge) {
    ui.on_avatar_requested({
        let weak = ui.as_weak();
        let bridge = bridge.clone();
        move |address: slint::SharedString| {
            if weak.upgrade().is_some_and(|ui| ui.get_online_avatars()) {
                bridge.send(bang_core::UiCommand::RequestAvatar(address.to_string()));
            }
        }
    });
    ui.on_avatars_toggled({
        let weak = ui.as_weak();
        move |enabled| {
            if let Some(ui) = weak.upgrade() {
                let mut config = crate::config::Config::load();
                config.online_avatars = enabled;
                if config.save().is_err() {
                    ui.set_status_message(crate::i18n::tr("Could not save settings").into());
                    return;
                }
                ui.set_online_avatars(enabled);
                bridge.send(bang_core::UiCommand::SetOnlineAvatars(enabled));
                let model = ui.get_messages();
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        decorate(&ui, &mut row);
                        if enabled {
                            ui.invoke_avatar_requested(row.sender_address.clone());
                        }
                        model.set_row_data(i, row);
                    }
                }
                ui.set_message_avatar_loaded(
                    enabled && image(&ui.get_message_sender_address()).size().width > 0,
                );
                ui.set_message_avatar(if enabled {
                    image(&ui.get_message_sender_address())
                } else {
                    Default::default()
                });
                if enabled && ui.get_selected_message() >= 0 {
                    ui.invoke_avatar_requested(ui.get_message_sender_address());
                }
            }
        }
    });
}
