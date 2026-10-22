//! Per-account sync engine.
//!
//! Each account runs its syncer on a dedicated OS thread, because the IMAP
//! client is synchronous and IDLE blocks. The thread connects, syncs folders
//! and messages, then idles for push updates while processing commands.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::account::AccountConfig;
use crate::error::{Error, Result};
use crate::events::MailEvent;
use crate::imap::{ImapConn, MetaMessage};
use crate::message::{AccountStatus, Folder, MessageFlags, MessageFull, MessageSummary};
use crate::store::Store;

/// Number of most-recent messages to fetch per folder on sync.
const SYNC_WINDOW: u32 = 200;

/// A command for a syncer thread.
pub enum SyncerCommand {
    /// Add or remove flags on messages (by local store id).
    SetFlags {
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
        /// Flags to add or remove.
        flags: MessageFlags,
        /// `true` to add, `false` to remove.
        add: bool,
    },
    /// Fetch the full body of a message (by local store id).
    FetchBody {
        /// Local store id of the message.
        message_id: i64,
    },
    /// Delete messages (expunge on the server, remove from the local store).
    Delete {
        /// Local store ids of the messages.
        message_ids: Vec<i64>,
    },
    /// Stop the syncer.
    Quit,
}

/// Start a syncer thread for an account.
///
/// # Arguments
/// * `config` — the account to sync.
/// * `store` — shared store.
/// * `events_tx` — where to send [`MailEvent`]s.
///
/// # Returns
/// A sender for issuing [`SyncerCommand`]s to the syncer.
pub fn start_syncer(
    config: AccountConfig,
    store: Arc<Store>,
    events_tx: UnboundedSender<MailEvent>,
) -> std::sync::mpsc::Sender<SyncerCommand> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<SyncerCommand>();
    std::thread::Builder::new()
        .name(format!("sync-{}", config.id.0))
        .spawn(move || run_syncer(config, store, events_tx, cmd_rx))
        .expect("spawn syncer thread");
    cmd_tx
}

/// The syncer main loop: connect, sync, idle, reconnect on failure.
fn run_syncer(
    config: AccountConfig,
    store: Arc<Store>,
    events_tx: UnboundedSender<MailEvent>,
    cmd_rx: std::sync::mpsc::Receiver<SyncerCommand>,
) {
    let account_id = config.id.clone();
    let mut backoff = Duration::from_secs(1);

    loop {
        // Honor a Quit before (re)connecting.
        if check_quit(&cmd_rx) {
            return;
        }

        let failed = match connect_and_sync(&config, &store, &events_tx) {
            Ok(conn) => {
                backoff = Duration::from_secs(1);
                emit(&events_tx, &account_id, AccountStatus::Connected);
                // `false` means the idle loop ended (dropped connection or a
                // condition like an empty folder list); fall through to the
                // backoff so we cannot reconnect in a tight loop.
                !idle_loop(conn, &config, &store, &events_tx, &cmd_rx)
            }
            Err(e) => {
                tracing::warn!("sync error for {account_id}: {e}");
                true
            }
        };
        if failed {
            emit(&events_tx, &account_id, AccountStatus::Reconnecting);
            if sleep_or_quit(&cmd_rx, backoff) {
                return;
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }
}

/// Connect and perform an initial/incremental sync of all folders.
fn connect_and_sync(
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
) -> Result<ImapConn> {
    emit(events_tx, &config.id, AccountStatus::Syncing);
    let mut conn = ImapConn::connect(&config.imap, &config.auth, &config.email)?;
    sync_folders(&mut conn, config, store, events_tx)?;
    Ok(conn)
}

/// List folders, reconcile with the store, and sync each folder's messages.
fn sync_folders(
    conn: &mut ImapConn,
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
) -> Result<()> {
    let paths = conn.list_folders()?;
    for path in &paths {
        let folder = Folder {
            id: 0,
            account_id: config.id.0.clone(),
            name: last_component(path),
            path: path.clone(),
            unread: 0,
        };
        store.upsert_folder(&folder)?;
    }
    store.delete_folders_not_in(&config.id.0, &paths)?;
    let folders = store.get_folders(&config.id.0)?;

    for folder in &folders {
        if let Err(e) = sync_folder(conn, config, store, events_tx, folder) {
            tracing::warn!("sync folder {} failed: {e}", folder.path);
        }
    }

    let folders = store.get_folders(&config.id.0)?;
    let _ = events_tx.send(MailEvent::FoldersUpdated {
        account: config.id.clone(),
        folders,
    });
    Ok(())
}

/// Sync a single folder's messages (incremental where possible).
fn sync_folder(
    conn: &mut ImapConn,
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
    folder: &Folder,
) -> Result<()> {
    let status = conn.select(&folder.path)?;
    if let Some(unseen) = status.unseen {
        store.set_folder_unread(folder.id, unseen)?;
    }
    let (last_uid, last_validity) = store
        .get_folder_sync_state(folder.id)?
        .unwrap_or((0, None));

    let full = match (last_validity, status.uid_validity) {
        (Some(a), Some(b)) => a != b,
        (None, Some(_)) => true,
        _ => last_uid == 0,
    };

    if full && last_validity.is_some() {
        store.reset_folder_sync(folder.id)?;
    }

    let mut updated = Vec::new();
    let metas: Vec<MetaMessage> = if full {
        conn.fetch_recent_meta(status.exists, SYNC_WINDOW)?
    } else {
        // Cached envelopes are immutable while UIDVALIDITY stays the same.
        // Refresh only flags, and avoid writes/events for unchanged messages.
        let uids = store.recent_message_uids(folder.id, SYNC_WINDOW)?;
        if !uids.is_empty() {
            let set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            for (uid, flags) in conn.fetch_flags(&set)? {
                if let Some(id) = store.refresh_message_flags(folder.id, uid, flags)? {
                    updated.push(id);
                }
            }
        }
        match last_uid.checked_add(1) {
            Some(first) if status.uid_next.is_none_or(|next| first < next) => {
                // A bounded range avoids IMAP's reversed n:* range returning
                // the last existing message when there is no new mail.
                let end = status
                    .uid_next
                    .map(|n| (n - 1).to_string())
                    .unwrap_or("*".into());
                conn.fetch_meta(&format!("{first}:{end}"))?
                    .into_iter()
                    .filter(|m| m.uid > last_uid)
                    .collect()
            }
            _ => Vec::new(),
        }
    };

    let new_last_uid = metas
        .iter()
        .map(|m| m.uid)
        .max()
        .unwrap_or(if full { 0 } else { last_uid });
    let mut appended = Vec::new();
    for meta in metas {
        let existing = store
            .get_message_by_uid(&config.id.0, folder.id, meta.uid)?
            .is_some();
        let mut summary = meta_to_summary(config, folder, &meta);
        let id = store.upsert_message(&summary)?;
        summary.id = id;
        if existing {
            updated.push(id);
        } else {
            appended.push(summary);
        }
    }

    store.set_folder_sync_state(folder.id, new_last_uid, status.uid_validity)?;

    if !appended.is_empty() {
        let _ = events_tx.send(MailEvent::MessagesAppended {
            account: config.id.clone(),
            folder_id: folder.id,
            messages: appended,
        });
    }
    if !updated.is_empty() || (full && last_validity.is_some()) {
        let _ = events_tx.send(MailEvent::MessagesUpdated {
            account: config.id.clone(),
            folder_id: folder.id,
            message_ids: updated,
        });
    }
    Ok(())
}

/// The IDLE loop: wait for changes, process commands, re-sync on change.
///
/// Returns `true` if the syncer should quit.
fn idle_loop(
    mut conn: ImapConn,
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
    cmd_rx: &std::sync::mpsc::Receiver<SyncerCommand>,
) -> bool {
    // Select the first folder (usually INBOX) for IDLE.
    let folders = store.get_folders(&config.id.0).unwrap_or_default();
    let idle_folder = folders
        .iter()
        .find(|f| f.path == "INBOX")
        .or_else(|| folders.first())
        .cloned();
    let idle_folder = match idle_folder {
        Some(f) => f,
        None => return false,
    };
    if conn.select(&idle_folder.path).is_err() {
        return false;
    }

    loop {
        // Process any pending commands.
        let mut handled = false;
        loop {
            match cmd_rx.try_recv() {
                Ok(SyncerCommand::Quit) => return true,
                Ok(SyncerCommand::SetFlags {
                    message_ids,
                    flags,
                    add,
                }) => {
                    handled = true;
                    if let Err(e) =
                        handle_set_flags(&mut conn, config, store, events_tx, &message_ids, flags, add)
                    {
                        tracing::warn!("set_flags failed: {e}");
                    }
                }
                Ok(SyncerCommand::FetchBody { message_id }) => {
                    handled = true;
                    if let Err(e) = handle_fetch_body(&mut conn, store, events_tx, message_id) {
                        tracing::warn!("fetch_body failed: {e}");
                    }
                }
                Ok(SyncerCommand::Delete { message_ids }) => {
                    handled = true;
                    if let Err(e) = handle_delete(&mut conn, config, store, events_tx, &message_ids)
                    {
                        tracing::warn!("delete failed: {e}");
                    }
                }
                Err(_) => break,
            }
        }
        if handled {
            // Commands may have changed the selected mailbox; restore it.
            if conn.select(&idle_folder.path).is_err() {
                return false;
            }
        }

        // Wait for mailbox changes (bounded so we keep checking commands).
        match conn.idle_wait(Duration::from_secs(10)) {
            Ok(true) => {
                // Mailbox changed: re-sync the idle folder.
                if let Err(e) = sync_folder(&mut conn, config, store, events_tx, &idle_folder) {
                    tracing::warn!("re-sync failed: {e}");
                    return false; // trigger reconnect
                }
            }
            Ok(false) => {
                // Timeout: loop to check commands.
            }
            Err(e) => {
                tracing::warn!("idle error: {e}");
                return false; // trigger reconnect
            }
        }
    }
}

/// Drain pending commands, returning `true` if a Quit was received.
///
/// Non-Quit commands arriving before a connection exists cannot be executed
/// and are dropped.
fn check_quit(cmd_rx: &std::sync::mpsc::Receiver<SyncerCommand>) -> bool {
    loop {
        match cmd_rx.try_recv() {
            Ok(SyncerCommand::Quit) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

/// Apply flag changes to messages and update the store.
fn handle_set_flags(
    conn: &mut ImapConn,
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
    message_ids: &[i64],
    flags: MessageFlags,
    add: bool,
) -> Result<()> {
    let by_folder = group_by_folder(store, message_ids)?;

    let flag_str = flags_to_imap(flags);
    let mut folder_ids = Vec::new();
    for (folder_id, (path, uids)) in &by_folder {
        if uids.is_empty() {
            continue;
        }
        conn.select(&path)?;
        let uid_set = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        conn.set_flags(&uid_set, &flag_str, add)?;
        folder_ids.push(*folder_id);
    }
    // Update the store before emitting: the UI reloads on the event.
    store.set_message_flags(message_ids, flags, add)?;
    for folder_id in folder_ids {
        let _ = events_tx.send(MailEvent::MessagesUpdated {
            account: config.id.clone(),
            folder_id,
            message_ids: message_ids.to_vec(),
        });
    }
    Ok(())
}

/// Group local message ids by their folder: `folder_id -> (path, uids)`.
fn group_by_folder(
    store: &Store,
    message_ids: &[i64],
) -> Result<std::collections::HashMap<i64, (String, Vec<u32>)>> {
    let mut by_folder: std::collections::HashMap<i64, (String, Vec<u32>)> =
        std::collections::HashMap::new();
    for id in message_ids {
        let Some(msg) = store.get_message(*id)? else {
            continue;
        };
        let entry = by_folder.entry(msg.folder_id).or_default();
        if entry.0.is_empty() {
            if let Some(folder) = store.get_folder_by_id(msg.folder_id)? {
                entry.0 = folder.path;
            }
        }
        entry.1.push(msg.uid);
    }
    Ok(by_folder)
}

/// Delete messages: expunge on the server, remove from the local store.
fn handle_delete(
    conn: &mut ImapConn,
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
    message_ids: &[i64],
) -> Result<()> {
    let by_folder = group_by_folder(store, message_ids)?;
    let mut folder_ids = Vec::new();
    for (folder_id, (path, uids)) in &by_folder {
        if uids.is_empty() {
            continue;
        }
        conn.select(&path)?;
        let uid_set = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        conn.expunge(&uid_set)?;
        folder_ids.push(*folder_id);
    }
    // Remove from the store before emitting: the UI reloads on the event.
    store.delete_messages(message_ids)?;
    for folder_id in folder_ids {
        let _ = events_tx.send(MailEvent::MessagesUpdated {
            account: config.id.clone(),
            folder_id,
            message_ids: message_ids.to_vec(),
        });
    }
    Ok(())
}

/// Fetch a message's full body, parse it, store it, and emit MessageLoaded.
fn handle_fetch_body(
    conn: &mut ImapConn,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
    message_id: i64,
) -> Result<()> {
    let summary = store
        .get_message(message_id)?
        .ok_or_else(|| Error::Store(format!("message {message_id} not found")))?;
    let folder = store
        .get_folder_by_id(summary.folder_id)?
        .ok_or_else(|| Error::Store("folder not found".into()))?;

    conn.select(&folder.path)?;
    let raw = conn.fetch_body(summary.uid)?;
    let parsed = crate::mime::parse(&raw)?;

    // Persist attachment data on disk; the record's path is set once stored.
    let mut attachments = crate::mime::to_attachment_records(message_id, &parsed.attachments);
    for (record, data) in attachments.iter_mut().zip(&parsed.attachments) {
        if let Ok(path) = store.save_attachment(message_id, &record.filename, &data.data) {
            record.path = path;
        }
    }
    store.save_message_bodies(
        message_id,
        &parsed.body_text,
        &parsed.body_html,
        &attachments,
    )?;

    let full = MessageFull {
        summary,
        body_text: parsed.body_text,
        body_html: parsed.body_html,
        attachments,
    };
    let _ = events_tx.send(MailEvent::MessageLoaded { message: full });
    Ok(())
}

/// Convert a `MetaMessage` to a `MessageSummary`.
fn meta_to_summary(config: &AccountConfig, folder: &Folder, meta: &MetaMessage) -> MessageSummary {
    MessageSummary {
        id: 0, // assigned by the store on upsert
        account_id: config.id.0.clone(),
        folder_id: folder.id,
        uid: meta.uid,
        message_id: meta.message_id.clone(),
        from: meta.from.clone(),
        to: meta.to.clone(),
        cc: meta.cc.clone(),
        subject: meta.subject.clone(),
        date: meta.date,
        flags: meta.flags,
        preview: String::new(),
        has_attachments: false,
    }
}

/// Convert `MessageFlags` to an IMAP flag string.
fn flags_to_imap(flags: MessageFlags) -> String {
    let mut parts = Vec::new();
    if flags.contains(MessageFlags::SEEN) {
        parts.push("\\Seen");
    }
    if flags.contains(MessageFlags::FLAGGED) {
        parts.push("\\Flagged");
    }
    if flags.contains(MessageFlags::ANSWERED) {
        parts.push("\\Answered");
    }
    if flags.contains(MessageFlags::DELETED) {
        parts.push("\\Deleted");
    }
    if flags.contains(MessageFlags::DRAFT) {
        parts.push("\\Draft");
    }
    parts.join(" ")
}

/// Sleep for `dur`, returning `true` if a Quit command arrives.
fn sleep_or_quit(cmd_rx: &std::sync::mpsc::Receiver<SyncerCommand>, dur: Duration) -> bool {
    let tick = dur / 10;
    let mut elapsed = Duration::ZERO;
    while elapsed < dur {
        if let Ok(SyncerCommand::Quit) = cmd_rx.try_recv() {
            return true;
        }
        std::thread::sleep(tick.max(Duration::from_millis(50)));
        elapsed += tick;
    }
    false
}

/// Emit an account-status event.
fn emit(events_tx: &UnboundedSender<MailEvent>, account_id: &crate::account::AccountId, status: AccountStatus) {
    let _ = events_tx.send(MailEvent::AccountStatus {
        account: account_id.clone(),
        status,
    });
}

/// Last path component of an IMAP folder path.
fn last_component(path: &str) -> String {
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}
