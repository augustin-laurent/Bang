//! Attachment preview and save.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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

/// Stage and sync all bytes before publishing a new file or replacing an approved inode.
fn write_export(path: &Path, data: &[u8], replace: impl FnOnce() -> bool) -> io::Result<bool> {
    write_export_with(path, replace, |file| file.write_all(data))
}

fn write_export_with(
    path: &Path,
    replace: impl FnOnce() -> bool,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<bool> {
    use std::ffi::CString;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid export destination");
    let filename = path.file_name().ok_or_else(invalid)?;
    let target_name = CString::new(filename.as_bytes()).map_err(|_| invalid())?;
    let directory = std::fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    // Anchor every lookup to the same open directory even if a parent path moves.
    let anchor = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let target = anchor.join(filename);
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    // Retain the approved descriptor so its inode cannot be recycled while
    // confirmation and staging are in progress.
    let existing_file = match options.open(&target) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let existing = existing_file
        .as_ref()
        .map(|file| file.metadata())
        .transpose()?;
    if let Some(metadata) = &existing {
        if !metadata.is_file() {
            return Err(invalid());
        }
        if !replace() {
            return Ok(false);
        }
    }
    let (stage_name, mut stage) = loop {
        let name = format!(
            ".bang-export-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        match options
            .clone()
            .create_new(true)
            .mode(0o600)
            .open(anchor.join(&name))
        {
            Ok(file) => break (name, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    struct Staged {
        path: PathBuf,
        cleanup: bool,
    }
    impl Drop for Staged {
        fn drop(&mut self) {
            if self.cleanup {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
    let mut staged = Staged {
        path: anchor.join(&stage_name),
        cleanup: true,
    };
    write(&mut stage)?;
    if let Some(metadata) = &existing {
        stage.set_permissions(metadata.permissions())?;
    }
    stage.sync_all()?;
    let created = stage.metadata()?;
    let same =
        |a: &std::fs::Metadata, b: &std::fs::Metadata| a.dev() == b.dev() && a.ino() == b.ino();
    let stage_name_c = CString::new(stage_name.as_bytes()).map_err(|_| invalid())?;
    let exchange = |flags| {
        // Both names are NUL-terminated basenames relative to a live directory fd.
        let result = unsafe {
            libc::renameat2(
                directory.as_raw_fd(),
                stage_name_c.as_ptr(),
                directory.as_raw_fd(),
                target_name.as_ptr(),
                flags,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    if let Some(approved) = existing {
        if !std::fs::symlink_metadata(&target).is_ok_and(|m| same(&m, &approved)) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination changed during confirmation",
            ));
        }
        exchange(libc::RENAME_EXCHANGE)?;
        // The staging name now holds the previous destination. Never remove it
        // unless its identity matches the file the user actually approved.
        staged.cleanup = false;
        if !std::fs::symlink_metadata(&staged.path).is_ok_and(|m| same(&m, &approved)) {
            if std::fs::symlink_metadata(&target).is_ok_and(|m| same(&m, &created)) {
                exchange(libc::RENAME_EXCHANGE)?;
                staged.cleanup =
                    std::fs::symlink_metadata(&staged.path).is_ok_and(|m| same(&m, &created));
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("destination changed; any recovery copy is named {stage_name}"),
            ));
        }
        directory.sync_all()?;
        staged.cleanup = true;
    } else {
        // A concurrent creator must never be overwritten without confirmation.
        exchange(libc::RENAME_NOREPLACE)?;
        directory.sync_all()?;
    }
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
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        assert_eq!(std::fs::read(root.join("approved-file")).unwrap(), b"new");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_staged_export_preserves_original_and_removes_partial_file() {
        let root = std::env::temp_dir().join(format!("bang-export-failure-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file");
        std::fs::write(&path, b"original").unwrap();
        let error = write_export_with(
            &path,
            || true,
            |file| {
                file.write_all(b"partial")?;
                Err(io::Error::from_raw_os_error(libc::ENOSPC))
            },
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
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
