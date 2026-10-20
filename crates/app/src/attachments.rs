//! Attachment preview and save.

use std::path::Path;
use std::sync::Arc;

use slint::{Image, Model};

use bang_core::Store;

use crate::i18n::tr;
use crate::BangApp;

/// Open the preview for the clicked attachment, or the save dialog when the
/// content type cannot be previewed (text and images are previewed inline).
pub fn open(ui: &BangApp, store: &Arc<Store>, index: i32) {
    let Some(row) = ui.get_message_attachments().row_data(index as usize) else {
        return;
    };
    if row.path.is_empty() {
        return;
    }
    let ct = row.content_type.to_lowercase();
    if ct.starts_with("text/") || ct == "application/json" || ct == "application/xml" {
        let Ok(data) = store.read_attachment(&row.path) else {
            return;
        };
        ui.set_preview_kind("text".into());
        ui.set_preview_text(String::from_utf8_lossy(&data).into_owned().into());
        ui.set_preview_image(slint::Image::default());
    } else if ct.starts_with("image/") {
        match Image::load_from_path(Path::new(&row.path)) {
            Ok(image) => {
                ui.set_preview_kind("image".into());
                ui.set_preview_image(image);
                ui.set_preview_text("".into());
            }
            Err(_) => {
                save_dialog(ui, &row);
                return;
            }
        }
    } else {
        save_dialog(ui, &row);
        return;
    }
    ui.set_preview_title(row.filename.clone());
    ui.set_preview_path(row.path.clone());
    ui.set_preview_open(true);
}

/// Write the current attachment to the chosen destination.
pub fn save(ui: &BangApp, store: &Arc<Store>, dest: slint::SharedString) {
    let src = ui.get_preview_path().to_string();
    if src.is_empty() {
        return;
    }
    match store.read_attachment(&src) {
        Ok(data) => match std::fs::write(&dest, &data) {
            Ok(()) => ui.set_status_message(format!("{} {dest}", tr("Saved")).into()),
            Err(e) => ui.set_status_message(format!("{} {e}", tr("Save failed")).into()),
        },
        Err(e) => ui.set_status_message(format!("{} {e}", tr("Save failed")).into()),
    }
}

fn save_dialog(ui: &BangApp, row: &crate::AttachmentRow) {
    ui.set_preview_path(row.path.clone());
    ui.set_save_filename(row.filename.clone());
    ui.set_save_path(default_destination(&row.filename));
    ui.set_save_open(true);
}

/// Default save location: the user's Downloads folder.
fn default_destination(filename: &str) -> slint::SharedString {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/Downloads/{filename}").into()
}
