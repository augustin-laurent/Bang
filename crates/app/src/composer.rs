//! Own the isolated rich composer. Only the parent talks to the mail engine.
use crate::{bridge::Bridge, config::Config, i18n, BangApp, Theme};
use bang_composer::{Attachment, EditorMessage, HostMessage, Sender};
use slint::{ComponentHandle, Global};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};
static CLOSE_MAIN: AtomicBool = AtomicBool::new(false);
struct Session {
    child: Child,
    input: ChildStdin,
    attachments: Vec<(u64, bang_core::OutgoingAttachment)>,
    next_attachment: u64,
    pending: bool,
    dirty: bool,
}
fn slot() -> &'static Mutex<Option<Arc<Mutex<Session>>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<Mutex<Session>>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}
fn current() -> Option<Arc<Mutex<Session>>> {
    slot().lock().unwrap().clone()
}
fn write(session: &mut Session, msg: &HostMessage) -> std::io::Result<()> {
    serde_json::to_writer(&mut session.input, msg)?;
    session.input.write_all(b"\n")?;
    session.input.flush()
}
/// Whether a writer is open (including an in-flight send).
pub fn is_open() -> bool {
    current().is_some()
}
/// Ask the editor to confirm abandonment before closing its window.
pub fn request_close() {
    if let Some(s) = current() {
        let _ = write(&mut s.lock().unwrap(), &HostMessage::RequestClose);
    }
}
/// Close the main window only after the user agrees to close the writer.
pub fn request_application_close() {
    CLOSE_MAIN.store(true, Ordering::Relaxed);
    request_close();
}
/// Propagate language and appearance without changing the draft.
pub fn preferences(ui: &BangApp) {
    if let Some(s) = current() {
        let _ = write(
            &mut s.lock().unwrap(),
            &HostMessage::Preferences {
                language: i18n::resolve(&Config::load().language).into(),
                dark: Theme::get(ui).get_dark(),
            },
        );
    }
}
/// Deliver SMTP feedback. The draft stays alive until a successful result.
pub fn send_result(ok: bool, error: Option<String>) {
    if let Some(s) = current() {
        let mut s = s.lock().unwrap();
        if s.pending {
            s.pending = false;
            let _ = write(&mut s, &HostMessage::SendResult { ok, error });
        }
    }
}
/// Open a single writer, or bring the existing writer to the foreground.
pub fn open(ui: &BangApp, store: Arc<bang_core::Store>, bridge: Bridge) -> anyhow::Result<()> {
    if let Some(s) = current() {
        write(&mut s.lock().unwrap(), &HostMessage::Focus)?;
        return Ok(());
    }
    let accounts = store.get_accounts()?;
    anyhow::ensure!(
        !accounts.is_empty(),
        i18n::tr("Add an account before composing")
    );
    let executable = std::env::current_exe()?.with_file_name("bang-compose");
    anyhow::ensure!(
        executable.exists(),
        "Build the composer first: cargo build --workspace"
    );
    let mut child = Command::new(executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let session = Arc::new(Mutex::new(Session {
        child,
        input,
        attachments: vec![],
        next_attachment: 1,
        pending: false,
        dirty: false,
    }));
    *slot().lock().unwrap() = Some(session.clone());
    let init = HostMessage::Init {
        accounts: accounts
            .into_iter()
            .map(|a| Sender {
                id: a.id.0,
                email: a.email,
            })
            .collect(),
        language: i18n::resolve(&Config::load().language).into(),
        dark: Theme::get(ui).get_dark(),
        translations: i18n::catalogue().clone(),
    };
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else {
                break;
            };
            let Ok(msg) = serde_json::from_str::<EditorMessage>(&line) else {
                continue;
            };
            match msg {
                EditorMessage::Ready => {
                    let _ = write(&mut session.lock().unwrap(), &init);
                }
                EditorMessage::CloseCancelled => {
                    CLOSE_MAIN.store(false, Ordering::Relaxed);
                }
                EditorMessage::Dirty { dirty } => session.lock().unwrap().dirty = dirty,
                EditorMessage::AttachPaths { paths } => {
                    let mut s = session.lock().unwrap();
                    for path in paths {
                        match std::fs::read(&path) {
                            Ok(data) => {
                                let id = s.next_attachment;
                                s.next_attachment += 1;
                                let filename = std::path::Path::new(&path)
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned();
                                s.attachments.push((
                                    id,
                                    bang_core::OutgoingAttachment {
                                        filename,
                                        content_type: content_type(&path).into(),
                                        data,
                                    },
                                ));
                            }
                            Err(e) => {
                                let _ = write(
                                    &mut s,
                                    &HostMessage::Error {
                                        message: format!(
                                            "{}: {e}",
                                            i18n::tr("Could not attach file")
                                        ),
                                    },
                                );
                            }
                        }
                    }
                    attachments_changed(&mut s);
                }
                EditorMessage::RemoveAttachment { id } => {
                    let mut s = session.lock().unwrap();
                    if !s.pending {
                        s.attachments.retain(|(key, _)| *key != id);
                        attachments_changed(&mut s);
                    }
                }
                EditorMessage::Send {
                    account,
                    to,
                    cc,
                    bcc,
                    subject,
                    body_text,
                    body_html,
                } => {
                    let mut s = session.lock().unwrap();
                    if s.pending {
                        continue;
                    }
                    match store.get_account(&account) {
                        Ok(Some(acc)) => {
                            s.pending = true;
                            bridge.send(bang_core::UiCommand::SendEmail(bang_core::SendRequest {
                                account: acc.id,
                                from: acc.email,
                                to,
                                cc,
                                bcc,
                                subject,
                                body_text,
                                body_html: Some(body_html),
                                attachments: s.attachments.iter().map(|(_, a)| a.clone()).collect(),
                                in_reply_to: None,
                                references: None,
                            }));
                        }
                        _ => {
                            let _ = write(
                                &mut s,
                                &HostMessage::SendResult {
                                    ok: false,
                                    error: Some(i18n::tr("The account was removed.")),
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        let status = session.lock().unwrap().child.wait();
        let pending = session.lock().unwrap().pending;
        let mut slot = slot().lock().unwrap();
        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &session)) {
            *slot = None;
        }
        drop(slot);
        if CLOSE_MAIN.swap(false, Ordering::Relaxed) && status.as_ref().is_ok_and(|s| s.success()) {
            let weak = weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.hide().ok();
                }
            });
        }
        if pending || status.map(|s| !s.success()).unwrap_or(true) {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_status_message(i18n::tr("Compose unavailable:").into());
                }
            });
        }
    });
    Ok(())
}
fn attachments_changed(s: &mut Session) {
    let files = s
        .attachments
        .iter()
        .map(|(id, a)| Attachment {
            id: *id,
            name: a.filename.clone(),
            size: a.data.len() as u64,
        })
        .collect();
    let _ = write(s, &HostMessage::Attachments { files });
}
fn content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}
