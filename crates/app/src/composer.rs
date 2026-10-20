//! Own the isolated rich composer. Only the parent talks to the mail engine.
use crate::{bridge::Bridge, config::Config, i18n, BangApp, Theme};
use bang_composer::{Attachment, DraftContent, EditorMessage, HostMessage, Sender};
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
    /// Prefill to deliver once the editor is ready (new session only).
    pending_prefill: Option<Prefill>,
    /// Reply threading headers for the current draft (set by a prefill).
    in_reply_to: Option<String>,
    references: Option<String>,
}

/// Draft fields to prefill the composer with (reply / forward).
pub struct Prefill {
    /// Recipients for the To field.
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Body as HTML (quoted original).
    pub body_html: String,
    /// Message-ID being replied to, if any.
    pub in_reply_to: Option<String>,
    /// References header value, if any.
    pub references: Option<String>,
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
///
/// With a `prefill`, the draft fields are replaced (reply / forward).
pub fn open(
    ui: &BangApp,
    store: Arc<bang_core::Store>,
    bridge: Bridge,
    prefill: Option<Prefill>,
) -> anyhow::Result<()> {
    if let Some(s) = current() {
        match prefill {
            Some(p) => {
                let mut s = s.lock().unwrap();
                s.in_reply_to = p.in_reply_to.clone();
                s.references = p.references.clone();
                write(
                    &mut s,
                    &HostMessage::Prefill {
                        to: p.to,
                        subject: p.subject,
                        body_html: p.body_html,
                        in_reply_to: p.in_reply_to,
                        references: p.references,
                    },
                )?;
            }
            None => write(&mut s.lock().unwrap(), &HostMessage::Focus)?,
        }
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
    let in_reply_to = prefill.as_ref().and_then(|p| p.in_reply_to.clone());
    let references = prefill.as_ref().and_then(|p| p.references.clone());
    let session = Arc::new(Mutex::new(Session {
        child,
        input,
        attachments: vec![],
        next_attachment: 1,
        pending: false,
        dirty: false,
        pending_prefill: prefill,
        in_reply_to,
        references,
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
                    let mut s = session.lock().unwrap();
                    let _ = write(&mut s, &init);
                    if let Some(p) = s.pending_prefill.take() {
                        let _ = write(
                            &mut s,
                            &HostMessage::Prefill {
                                to: p.to,
                                subject: p.subject,
                                body_html: p.body_html,
                                in_reply_to: p.in_reply_to,
                                references: p.references,
                            },
                        );
                    }
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
                                in_reply_to: s.in_reply_to.clone(),
                                references: s.references.clone(),
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
                EditorMessage::SaveDraft(content) => {
                    let mut s = session.lock().unwrap();
                    match build_request(&store, &s, &content) {
                        Some(request) => bridge.send(bang_core::UiCommand::SaveDraft(request)),
                        None => {
                            let _ = write(
                                &mut s,
                                &HostMessage::Error {
                                    message: i18n::tr("The account was removed.").into(),
                                },
                            );
                        }
                    }
                }
                EditorMessage::SaveTemplate { name, content } => {
                    let mut s = session.lock().unwrap();
                    match build_request(&store, &s, &content) {
                        Some(request) => bridge.send(bang_core::UiCommand::SaveTemplate {
                            name,
                            request,
                        }),
                        None => {
                            let _ = write(
                                &mut s,
                                &HostMessage::Error {
                                    message: i18n::tr("The account was removed.").into(),
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
/// Build a send request from the editor's draft content and session state.
///
/// Returns `None` when the selected account no longer exists.
fn build_request(
    store: &bang_core::Store,
    s: &Session,
    c: &DraftContent,
) -> Option<bang_core::SendRequest> {
    let acc = store.get_account(&c.account).ok()??;
    Some(bang_core::SendRequest {
        account: acc.id,
        from: acc.email,
        to: c.to.clone(),
        cc: c.cc.clone(),
        bcc: c.bcc.clone(),
        subject: c.subject.clone(),
        body_text: c.body_text.clone(),
        body_html: Some(c.body_html.clone()),
        attachments: s.attachments.iter().map(|(_, a)| a.clone()).collect(),
        in_reply_to: s.in_reply_to.clone(),
        references: s.references.clone(),
    })
}

/// Open the composer prefilled to reply to or forward a message.
pub fn reply_or_forward(
    ui: &BangApp,
    store: Arc<bang_core::Store>,
    bridge: Bridge,
    message_id: i64,
    forward: bool,
) -> anyhow::Result<()> {
    let full = store
        .get_message_full(message_id)?
        .ok_or_else(|| anyhow::anyhow!("message not found"))?;
    let body = if full.body_text.is_empty() {
        bang_core::mime::render_html_subset(&full.body_html)
    } else {
        full.body_text.clone()
    };
    let mut body_html = quoted_body_html(&body);
    if forward {
        // Prepend the original headers above the quote.
        let to = full
            .summary
            .to
            .iter()
            .map(|a| display(&a))
            .collect::<Vec<_>>()
            .join(", ");
        let headers = format!(
            "From: {}\nTo: {}\nDate: {}\nSubject: {}",
            display(&full.summary.from),
            to,
            crate::state::format_date(full.summary.date),
            full.summary.subject
        );
        body_html = format!(
            "<p>{}</p>{body_html}",
            html_escape(&headers).replace('\n', "<br>")
        );
    }
    let subject = full.summary.subject.clone();
    let prefix = if forward { "Fwd: " } else { "Re: " };
    let subject = if subject.to_ascii_lowercase().starts_with(&prefix.to_ascii_lowercase()) {
        subject
    } else {
        format!("{prefix}{subject}")
    };
    let to = if forward {
        Vec::new()
    } else {
        vec![full.summary.from.address.clone()]
    };
    let in_reply_to = if full.summary.message_id.is_empty() {
        None
    } else {
        Some(full.summary.message_id.clone())
    };
    open(
        ui,
        store,
        bridge,
        Some(Prefill {
            to,
            subject,
            body_html,
            in_reply_to,
            references: None,
        }),
    )
}

/// Display name of an address (name if set, else the address).
fn display(addr: &bang_core::EmailAddress) -> String {
    if addr.name.is_empty() {
        addr.address.clone()
    } else {
        addr.name.clone()
    }
}

/// Build the quoted body as HTML: an empty paragraph for the user to type in,
/// then the original body with each line prefixed by "> ".
fn quoted_body_html(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "<p></p>".into();
    }
    let lines: Vec<String> = trimmed
        .lines()
        .map(|l| format!("&gt; {}", html_escape(l.trim_end())))
        .collect();
    format!("<p></p><p>{}</p>", lines.join("<br>"))
}

/// Minimal HTML escaping for text embedded in generated markup.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_escapes_specials() {
        assert_eq!(html_escape("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(html_escape("plain"), "plain");
    }

    #[test]
    fn quoted_body_empty_is_blank_paragraph() {
        assert_eq!(quoted_body_html(""), "<p></p>");
        assert_eq!(quoted_body_html("   \n  "), "<p></p>");
    }

    #[test]
    fn quoted_body_prefixes_each_line() {
        let html = quoted_body_html("line one\nline two");
        assert_eq!(html, "<p></p><p>&gt; line one<br>&gt; line two</p>");
    }

    #[test]
    fn quoted_body_escapes_markup() {
        let html = quoted_body_html("<b>bold</b> & more");
        assert_eq!(html, "<p></p><p>&gt; &lt;b&gt;bold&lt;/b&gt; &amp; more</p>");
    }
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
