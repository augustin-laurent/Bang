//! Attachment preview and save.

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slint::{Image, Model};

use bang_core::Store;
use gtk::prelude::*;

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
    // Preview Save and direct Save must start from the same safe suggestion.
    ui.set_save_filename(row.filename.clone());
    ui.set_save_path(default_destination(&row.filename));
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
        Ok(data) => {
            match write_export(Path::new(dest.as_str()), &data, || confirm_replace(&dest)) {
                Ok(false) => {}
                Ok(true) => ui.set_status_message(format!("{} {dest}", tr("Saved")).into()),
                Err(e) => ui.set_status_message(format!("{} {e}", tr("Save failed")).into()),
            }
        }
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
    destination_in(Path::new(&home), filename)
        .to_string_lossy()
        .into_owned()
        .into()
}

fn destination_in(home: &Path, filename: &str) -> PathBuf {
    home.join("Downloads")
        .join(bang_core::store::sanitize_filename(filename))
}

fn confirm_replace(path: &str) -> bool {
    let parent = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .find(|w| w.is_active());
    let dialog = gtk::MessageDialog::builder()
        .modal(true)
        .message_type(gtk::MessageType::Question)
        .buttons(gtk::ButtonsType::YesNo)
        .text(tr("Replace existing file?"))
        .secondary_text(path)
        .build();
    dialog.set_transient_for(parent.as_ref());
    dialog.set_default_response(gtk::ResponseType::No);
    let confirmed = dialog.run() == gtk::ResponseType::Yes;
    // GTK dialogs survive a response. No callback retains this local dialog,
    // and its state is never queried after destruction.
    unsafe {
        dialog.destroy();
    }
    confirmed
}

/// Create atomically, or ask before truncating an already-open regular file.
fn write_export(path: &Path, data: &[u8], replace: impl FnOnce() -> bool) -> io::Result<bool> {
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let (mut file, existing) = match options.clone().create_new(true).open(path) {
        Ok(file) => (file, false),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (options.open(path)?, true),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination is not a regular file",
        ));
    }
    if existing {
        if !replace() {
            return Ok(false);
        }
        // Keep the same descriptor across confirmation: a swapped path cannot
        // redirect the approved overwrite to a different inode.
        file.set_len(0)?;
    }
    file.write_all(data)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a native GTK display"]
    fn native_overwrite_confirmation_accepts_and_cancels() {
        gtk::init().unwrap();
        eprintln!("GTK initialized for overwrite confirmation");
        for response in [gtk::ResponseType::No, gtk::ResponseType::Yes] {
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                eprintln!("Responding to overwrite confirmation: {response:?}");
                let dialog = gtk::Window::list_toplevels()
                    .into_iter()
                    .filter(|w| w.is_visible())
                    .find_map(|w| w.downcast::<gtk::MessageDialog>().ok())
                    .unwrap();
                dialog.response(response);
            });
            assert_eq!(
                confirm_replace("/tmp/rapport été.txt"),
                response == gtk::ResponseType::Yes
            );
            eprintln!("Overwrite confirmation returned: {response:?}");
        }
    }

    #[test]
    fn export_suggestions_cannot_escape_downloads() {
        let home = Path::new("/home/test");
        for (name, expected) in [
            ("../.bashrc", ".bashrc"),
            ("../../config/file", "file"),
            ("/etc/passwd", "passwd"),
            (r"..\..\startup", "startup"),
            ("..", "attachment"),
            (".", "attachment"),
            ("", "attachment"),
            ("folder/", "attachment"),
            ("bad\0name", "attachment"),
            ("rapport été.pdf", "rapport été.pdf"),
            ("%2e%2e%2ffile", "%2e%2e%2ffile"),
        ] {
            assert_eq!(
                destination_in(home, name),
                home.join("Downloads").join(expected)
            );
        }
    }

    #[test]
    fn export_requires_confirmation_and_never_follows_symlinks() {
        let root = std::env::temp_dir().join(format!("bang-export-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file");
        std::fs::write(&path, b"original").unwrap();
        assert!(!write_export(&path, b"rejected", || false).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert!(write_export(&path, b"new", || true).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(write_export(&link, b"bad", || panic!(
            "symlink must fail before confirmation"
        ))
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(write_export(&root.join("new-file"), b"fresh", || panic!(
            "new file needs no overwrite"
        ))
        .unwrap());
        // A path changed during confirmation cannot redirect the write.
        assert!(write_export(&path, b"approved", || {
            std::fs::rename(&path, root.join("approved-file")).unwrap();
            std::fs::write(&path, b"replacement").unwrap();
            true
        })
        .unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        assert_eq!(
            std::fs::read(root.join("approved-file")).unwrap(),
            b"approved"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "preview-renderer")]
    #[test]
    fn preview_and_direct_save_share_a_fresh_safe_destination() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = BangApp::new().unwrap();
        let root = std::env::temp_dir().join(format!("bang-preview-save-{}", std::process::id()));
        let store = Arc::new(Store::open(&root.join("mail.db")).unwrap());
        let path = store.save_attachment(1, "cached", b"preview text").unwrap();
        for content_type in ["text/plain", "application/octet-stream"] {
            ui.set_save_path("/previous/destination".into());
            ui.set_message_attachments(slint::ModelRc::new(slint::VecModel::from(vec![
                crate::AttachmentRow {
                    filename: "../../report.txt".into(),
                    path: path.clone().into(),
                    content_type: content_type.into(),
                    ..Default::default()
                },
            ])));
            open(&ui, &store, 0);
            assert_eq!(ui.get_save_path(), default_destination("report.txt"));
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
