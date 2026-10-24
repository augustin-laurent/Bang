//! Per-account sync engine.
//!
//! Each account runs its syncer on a dedicated OS thread, because the IMAP
//! client is synchronous and IDLE blocks. The thread connects, syncs folders
//! and messages, then idles for push updates. Interactive commands use a
//! separate, lazily connected session so opening mail never waits for IDLE.

use std::collections::VecDeque;
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

/// Start background sync and an interactive command worker for an account.
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
    let (quit_tx, quit_rx) = std::sync::mpsc::channel::<()>();
    let sync_config = config.clone();
    let sync_store = store.clone();
    let sync_events = events_tx.clone();
    std::thread::Builder::new()
        .name(format!("sync-{}", config.id.0))
        .spawn(move || run_syncer(sync_config, sync_store, sync_events, quit_rx))
        .expect("spawn syncer thread");
    std::thread::Builder::new()
        .name(format!("mail-{}", config.id.0))
        .spawn(move || run_commands(config, store, events_tx, cmd_rx, quit_tx))
        .expect("spawn interactive mail thread");
    cmd_tx
}

/// The syncer main loop: connect, sync, idle, reconnect on failure.
fn run_syncer(
    config: AccountConfig,
    store: Arc<Store>,
    events_tx: UnboundedSender<MailEvent>,
    cmd_rx: std::sync::mpsc::Receiver<()>,
) {
    let account_id = config.id.clone();
    let mut backoff = Duration::from_secs(1);

    loop {
        // Honor a Quit before (re)connecting.
        if check_quit(&cmd_rx) {
            return;
        }

        match connect_and_sync(&config, &store, &events_tx) {
            Ok(conn) => {
                backoff = Duration::from_secs(1);
                emit(&events_tx, &account_id, AccountStatus::Connected);
                if idle_loop(conn, &config, &store, &events_tx, &cmd_rx) {
                    return;
                }
            }
            Err(e) => tracing::warn!("sync error for {account_id}: {e}"),
        }
        emit(&events_tx, &account_id, AccountStatus::Reconnecting);
        if sleep_or_quit(&cmd_rx, backoff) {
            return;
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
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
            name: crate::mime::decode_folder_name(&last_component(path)),
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
        if let Some(hints) = &meta.classification { store.update_classification(id, hints)?; }
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
    cmd_rx: &std::sync::mpsc::Receiver<()>,
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
        if check_quit(cmd_rx) {
            return true;
        }

        // Only the watcher waits in IDLE; the interactive worker stays available.
        match conn.idle_wait(Duration::from_secs(10)) {
            Ok(true) => {
                if check_quit(cmd_rx) {
                    return true;
                }
                // Mailbox changed: re-sync the idle folder.
                if let Err(e) = sync_folder(&mut conn, config, store, events_tx, &idle_folder) {
                    tracing::warn!("re-sync failed: {e}");
                    return false; // trigger reconnect
                }
            }
            Ok(false) => {
                // Timeout: check shutdown before renewing IDLE.
            }
            Err(e) => {
                tracing::warn!("idle error: {e}");
                return false; // trigger reconnect
            }
        }
    }
}

/// Stop when requested or when the interactive worker has exited.
fn check_quit(cmd_rx: &std::sync::mpsc::Receiver<()>) -> bool {
    !matches!(cmd_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
}

// Superseded reads have no side effects. Keep mutations in their original order.
fn queue_command(pending: &mut VecDeque<SyncerCommand>, command: SyncerCommand) -> bool {
    if matches!(command, SyncerCommand::Quit) {
        return true;
    }
    if matches!(command, SyncerCommand::FetchBody { .. }) {
        pending.retain(|c| !matches!(c, SyncerCommand::FetchBody { .. }));
    }
    pending.push_back(command);
    false
}

fn run_commands(
    config: AccountConfig,
    store: Arc<Store>,
    events_tx: UnboundedSender<MailEvent>,
    cmd_rx: std::sync::mpsc::Receiver<SyncerCommand>,
    quit_tx: std::sync::mpsc::Sender<()>,
) {
    let mut conn = None;
    let mut pending = VecDeque::new();
    'commands: loop {
        if pending.is_empty() {
            match cmd_rx.recv() {
                Ok(command) => {
                    if queue_command(&mut pending, command) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        loop {
            match cmd_rx.try_recv() {
                Ok(command) => {
                    if queue_command(&mut pending, command) {
                        break 'commands;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'commands,
            }
        }
        if conn.is_none() {
            match ImapConn::connect(&config.imap, &config.auth, &config.email) {
                Ok(connected) => {
                    conn = Some(connected);
                    // A newer selection may have arrived during login.
                    continue;
                }
                Err(e) => {
                    pending.pop_front();
                    let _ = events_tx.send(MailEvent::Error {
                        account: Some(config.id.clone()),
                        message: e.to_string(),
                    });
                    continue;
                }
            }
        }
        let command = pending.pop_front().unwrap();
        let started = std::time::Instant::now();
        let mut result = execute_command(
            conn.as_mut().unwrap(),
            &config,
            &store,
            &events_tx,
            &command,
        );
        // A quiet command session may have expired on the server. Reads can
        // safely reconnect once; mutations are never replayed automatically.
        if matches!(command, SyncerCommand::FetchBody { .. })
            && matches!(&result, Err(Error::Imap(_) | Error::Network(_)))
        {
            result = match ImapConn::connect(&config.imap, &config.auth, &config.email) {
                Ok(connected) => {
                    conn = Some(connected);
                    execute_command(
                        conn.as_mut().unwrap(),
                        &config,
                        &store,
                        &events_tx,
                        &command,
                    )
                }
                Err(e) => Err(e),
            };
        }
        if let SyncerCommand::FetchBody { message_id } = command {
            tracing::debug!(
                message_id,
                elapsed_ms = started.elapsed().as_millis(),
                "interactive body fetch"
            );
        }
        if let Err(e) = result {
            conn = None;
            let _ = events_tx.send(MailEvent::Error {
                account: Some(config.id.clone()),
                message: e.to_string(),
            });
        }
    }
    let _ = quit_tx.send(());
}

fn execute_command(
    conn: &mut ImapConn,
    config: &AccountConfig,
    store: &Store,
    events_tx: &UnboundedSender<MailEvent>,
    command: &SyncerCommand,
) -> Result<()> {
    match command {
        SyncerCommand::FetchBody { message_id } => {
            handle_fetch_body(conn, store, events_tx, *message_id)
        }
        SyncerCommand::SetFlags {
            message_ids,
            flags,
            add,
        } => handle_set_flags(conn, config, store, events_tx, message_ids, *flags, *add),
        SyncerCommand::Delete { message_ids } => {
            handle_delete(conn, config, store, events_tx, message_ids)
        }
        SyncerCommand::Quit => Ok(()),
    }
}

// An interactive connection must not act on UIDs from an obsolete generation.
fn select_cached_folder(
    conn: &mut ImapConn,
    store: &Store,
    folder_id: i64,
    path: &str,
) -> Result<()> {
    let status = conn.select(path)?;
    let expected = store
        .get_folder_sync_state(folder_id)?
        .and_then(|(_, validity)| validity);
    if let (Some(expected), Some(actual)) = (expected, status.uid_validity) {
        if expected != actual {
            return Err(Error::Store(
                "mailbox changed; wait for synchronization before retrying".into(),
            ));
        }
    }
    Ok(())
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
        select_cached_folder(conn, store, *folder_id, path)?;
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
        select_cached_folder(conn, store, *folder_id, path)?;
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
    let mut summary = store
        .get_message(message_id)?
        .ok_or_else(|| Error::Store(format!("message {message_id} not found")))?;
    let folder = store
        .get_folder_by_id(summary.folder_id)?
        .ok_or_else(|| Error::Store("folder not found".into()))?;

    select_cached_folder(conn, store, folder.id, &folder.path)?;
    let raw = conn.fetch_body(summary.uid)?;
    let parsed = crate::mime::parse(&raw)?;

    summary.from = parsed.from.clone();
    summary.to = parsed.to.clone();
    summary.cc = parsed.cc.clone();
    summary.subject = parsed.subject.clone();
    summary.message_id = parsed.message_id.clone();
    summary.preview = crate::mime::extract_preview(&parsed.body_text, 200);
    summary.has_attachments = parsed.attachments.iter().any(|a| a.disposition != "inline");
    store.upsert_message(&summary)?;
    store.update_classification(summary.id, &crate::organization::ClassificationSignals::from_headers(&raw))?;

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
fn sleep_or_quit(cmd_rx: &std::sync::mpsc::Receiver<()>, dur: Duration) -> bool {
    !matches!(
        cmd_rx.recv_timeout(dur),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_reads_are_replaced_without_reordering_mutations() {
        let mut pending = VecDeque::new();
        for command in [
            SyncerCommand::FetchBody { message_id: 1 },
            SyncerCommand::SetFlags {
                message_ids: vec![1],
                flags: MessageFlags::SEEN,
                add: true,
            },
            SyncerCommand::FetchBody { message_id: 2 },
            SyncerCommand::Delete {
                message_ids: vec![2],
            },
            SyncerCommand::FetchBody { message_id: 3 },
        ] {
            assert!(!queue_command(&mut pending, command));
        }
        assert!(matches!(
            pending.pop_front(),
            Some(SyncerCommand::SetFlags { .. })
        ));
        assert!(matches!(
            pending.pop_front(),
            Some(SyncerCommand::Delete { .. })
        ));
        assert!(matches!(
            pending.pop_front(),
            Some(SyncerCommand::FetchBody { message_id: 3 })
        ));
        assert!(pending.is_empty());
        assert!(queue_command(&mut pending, SyncerCommand::Quit));
    }

    #[test]
    fn shutdown_interrupts_reconnect_backoff() {
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(!check_quit(&rx));
        tx.send(()).unwrap();
        assert!(sleep_or_quit(&rx, Duration::from_secs(60)));
        drop(tx);
        assert!(check_quit(&rx));
    }
}
