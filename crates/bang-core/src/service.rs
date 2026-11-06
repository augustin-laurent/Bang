//! The mail service: owns the store and the per-account syncers, and
//! dispatches UI commands to them.
//!
//! The service is driven from a tokio runtime (see the app's bridge); it is
//! `Send + Sync` and can be shared across tasks.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc as std_mpsc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use tokio::sync::mpsc::UnboundedSender;

use crate::account::{AccountConfig, AccountId, AuthMethod, ImapConfig, OAuthProvider, SmtpConfig};
use crate::credentials::{CredentialManager, LinuxSecretStore};
use crate::error::{Error, Result};
use crate::events::{MailEvent, SendRequest, UiCommand};
use crate::events::{SetupFailure, SetupStage};
use crate::store::Store;
use crate::sync::{start_managed_syncer, SyncerCommand};
#[cfg(test)]
use crate::TlsMode;
use crate::{AccountStatus, OAuthRegistration, StoredAccount};

#[derive(Clone)]
struct PreparedAccount {
    config: AccountConfig,
    registration: Option<OAuthRegistration>,
}

struct InFlightSend {
    id: i64,
    sends: Arc<Mutex<HashSet<i64>>>,
}
impl Drop for InFlightSend {
    fn drop(&mut self) {
        self.sends.lock().unwrap().remove(&self.id);
    }
}

type Syncers = Arc<Mutex<HashMap<String, std_mpsc::Sender<SyncerCommand>>>>;

fn start_account_workers(
    record: StoredAccount,
    credentials: &Arc<CredentialManager>,
    store: &Arc<Store>,
    events: &UnboundedSender<MailEvent>,
    syncers: &Syncers,
) {
    if !credentials.is_active(&record.id.0) {
        return;
    }
    let Ok(source) = credentials.handle(&record.id.0) else {
        return;
    };
    let mut workers = syncers.lock().unwrap();
    if let Some(old) = workers.remove(&record.id.0) {
        let _ = old.send(SyncerCommand::Quit);
    }
    let tx = start_managed_syncer(
        record.connection_template(),
        store.clone(),
        events.clone(),
        source,
    );
    workers.insert(record.id.0, tx);
}

fn finish_restoration(
    record: StoredAccount,
    result: Result<()>,
    credentials: &Arc<CredentialManager>,
    store: &Arc<Store>,
    events: &UnboundedSender<MailEvent>,
    syncers: &Syncers,
) {
    match result {
        // A temporary outage must leave workers retrying with backoff; only
        // missing/revoked grants should ask the user to sign in again.
        Ok(()) | Err(Error::Network(_) | Error::Timeout) => {
            if let Ok(Some(current)) = store.get_account(&record.id.0) {
                start_account_workers(current, credentials, store, events, syncers);
            }
        }
        Err(Error::Cancelled) => {}
        Err(error) => {
            let _ = events.send(MailEvent::AccountStatus {
                account: record.id,
                status: if matches!(error, Error::SecureStorage) {
                    AccountStatus::StorageLocked
                } else {
                    AccountStatus::ReconnectRequired
                },
            });
        }
    }
}

/// Owns the store and one syncer per account.
///
/// Commands are handled synchronously; long-running work (SMTP sends and
/// OAuth logins) is spawned on the ambient tokio runtime.
pub struct MailService {
    store: Arc<Store>,
    account_setup: Mutex<Option<tokio::task::JoinHandle<()>>>,
    setup_active: Mutex<Arc<AtomicBool>>,
    pending_setup: Arc<Mutex<Option<(i32, PreparedAccount)>>>,
    credentials: Arc<CredentialManager>,
    avatars: Arc<crate::avatars::AvatarCache>,
    events_tx: UnboundedSender<MailEvent>,
    syncers: Arc<Mutex<HashMap<String, std_mpsc::Sender<SyncerCommand>>>>,
    /// Pending-send ids currently being sent, so a slow SMTP server cannot
    /// make the periodic dispatch deliver the same email twice.
    in_flight_sends: Arc<Mutex<HashSet<i64>>>,
    send_tasks: Mutex<HashMap<String, Vec<tokio::task::AbortHandle>>>,
    removing_accounts: Arc<Mutex<HashSet<String>>>,
}

impl MailService {
    /// Create a service around a store.
    ///
    /// # Arguments
    /// * `store` — the shared local store.
    /// * `events_tx` — where core events are sent.
    pub fn new(store: Arc<Store>, events_tx: UnboundedSender<MailEvent>) -> Self {
        let credentials = Arc::new(CredentialManager::new(
            store.clone(),
            Arc::new(LinuxSecretStore),
        ));
        Self {
            credentials,
            setup_active: Mutex::new(Arc::new(AtomicBool::new(true))),
            pending_setup: Arc::new(Mutex::new(None)),
            store,
            account_setup: Mutex::new(None),
            avatars: crate::avatars::AvatarCache::new(),
            events_tx,
            syncers: Arc::new(Mutex::new(HashMap::new())),
            in_flight_sends: Arc::new(Mutex::new(HashSet::new())),
            send_tasks: Mutex::new(HashMap::new()),
            removing_accounts: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn track_send(&self, id: String, handle: tokio::task::AbortHandle) {
        let mut tasks = self.send_tasks.lock().unwrap();
        let account = tasks.entry(id).or_default();
        account.retain(|task| !task.is_finished());
        account.push(handle);
    }

    fn add_account(&self, config: AccountConfig) {
        let credentials = self.credentials.clone();
        let store = self.store.clone();
        let events = self.events_tx.clone();
        let syncers = self.syncers.clone();
        tokio::spawn(async move {
            let id = config.id.clone();
            match credentials.install(config, None, true).await {
                Ok(record) => {
                    start_account_workers(record, &credentials, &store, &events, &syncers)
                }
                Err(_) => {
                    let _ = events.send(MailEvent::Error {
                        account: Some(id),
                        message: "Could not add account for this session.".into(),
                    });
                }
            }
        });
    }

    fn cancel_account_setup(&self) {
        self.setup_active
            .lock()
            .unwrap()
            .store(false, Ordering::SeqCst);
        *self.pending_setup.lock().unwrap() = None;
        if let Some(task) = self.account_setup.lock().unwrap().take() {
            task.abort();
        }
    }

    fn start_account_setup(
        &self,
        attempt: i32,
        account: impl std::future::Future<Output = Result<AccountConfig>> + Send + 'static,
    ) {
        self.start_registered_setup(attempt, account, None);
    }

    fn start_registered_setup(
        &self,
        attempt: i32,
        account: impl std::future::Future<Output = Result<AccountConfig>> + Send + 'static,
        registration: Option<OAuthRegistration>,
    ) {
        self.cancel_account_setup();
        let active = Arc::new(AtomicBool::new(true));
        *self.setup_active.lock().unwrap() = active.clone();
        let events = self.events_tx.clone();
        let credentials = self.credentials.clone();
        let pending = self.pending_setup.clone();
        let store = self.store.clone();
        let syncers = self.syncers.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let config = account.await?;
                let _ = events.send(MailEvent::AccountSetupChecking { attempt });
                let check = config.clone();
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::task::spawn_blocking(move || {
                        crate::imap::ImapConn::connect(&check.imap, &check.auth, &check.email)
                            .map(|_| ())
                    })
                    .await
                    .map_err(|_| Error::Network("Could not check incoming mail.".into()))??;
                    crate::smtp::test_connection(&config.smtp, &config.auth, &config.email).await
                })
                .await
                .map_err(|_| Error::Timeout)??;
                if !active.load(Ordering::SeqCst) {
                    return Err(Error::Cancelled);
                }
                let prepared = PreparedAccount {
                    config,
                    registration,
                };
                *pending.lock().unwrap() = Some((attempt, prepared.clone()));
                let _ = events.send(MailEvent::AccountSetupProgress {
                    attempt,
                    stage: SetupStage::SavingCredentials,
                });
                // Commit runs independently so cancellation can invalidate it,
                // rather than dropping a keyring write after it reached D-Bus.
                let commit_active = active.clone();
                let commit_credentials = credentials.clone();
                let record = tokio::spawn(async move {
                    commit_credentials
                        .install_checked(
                            prepared.config,
                            prepared.registration,
                            false,
                            commit_active,
                        )
                        .await
                })
                .await
                .map_err(|_| Error::Cancelled)??;
                if !active.load(Ordering::SeqCst) {
                    return Err(Error::Cancelled);
                }
                start_account_workers(record, &credentials, &store, &events, &syncers);
                *pending.lock().unwrap() = None;
                Ok(())
            }
            .await;
            if active.load(Ordering::SeqCst) {
                let _ = events.send(MailEvent::AccountSetupFinished {
                    attempt,
                    error: result.as_ref().err().map(SetupFailure::from_error),
                });
            }
        });
        *self.account_setup.lock().unwrap() = Some(task);
    }

    /// Apply a local-only store mutation to a set of messages and notify the UI.
    ///
    /// On success, emits `MessagesUpdated` for each affected (account, folder)
    /// pair so the UI refreshes the visible list.
    fn update_local(&self, message_ids: &[i64], f: impl FnOnce(&Store) -> Result<()>) {
        // Capture (account, folder) before mutating so the UI can refresh
        // the right lists.
        let mut targets: Vec<(String, i64)> = Vec::new();
        for id in message_ids {
            if let Ok(Some(m)) = self.store.get_message(*id) {
                targets.push((m.account_id, m.folder_id));
            }
        }
        if let Err(e) = f(&self.store) {
            let _ = self.events_tx.send(MailEvent::Error {
                account: None,
                message: e.to_string(),
            });
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for (account, folder_id) in targets {
            if seen.insert((account.clone(), folder_id)) {
                let _ = self.events_tx.send(MailEvent::MessagesUpdated {
                    account: AccountId(account),
                    folder_id,
                    message_ids: message_ids.to_vec(),
                });
            }
        }
    }

    /// Emit `FollowUpDue` for every message whose follow-up reminder is due,
    /// then clear the reminder so it fires only once.
    ///
    /// Called periodically by the app's bridge; also runs once at startup,
    /// so reminders that came due while the app was closed fire on launch.
    pub fn check_due_follow_ups(&self) {
        let now = chrono::Utc::now().timestamp();
        let due = match self.store.due_follow_ups(now) {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!("due_follow_ups failed: {e}");
                return;
            }
        };
        for msg in due {
            let _ = self.events_tx.send(MailEvent::FollowUpDue {
                account: AccountId(msg.account_id),
                message_id: msg.id,
                subject: msg.subject,
            });
            if let Err(e) = self.store.clear_follow_up(&[msg.id]) {
                tracing::warn!("clear_follow_up failed for {}: {e}", msg.id);
            }
        }
    }

    fn organization_result(&self, result: Result<()>) {
        let event = match result {
            Ok(()) => MailEvent::OrganizationChanged,
            Err(e) => {
                let message = match e {
                    Error::Store(message)
                        if message.contains("UNIQUE constraint failed: labels.name") =>
                    {
                        "A label with this name already exists.".into()
                    }
                    Error::Store(message) => message,
                    other => other.to_string(),
                };
                MailEvent::OrganizationError { message }
            }
        };
        let _ = self.events_tx.send(event);
    }

    /// Dispatch every pending send whose time has come.
    ///
    /// Called periodically by the app's bridge. A successful send is removed
    /// from the pending list; a failed one is kept and retried on the next
    /// tick (no event, so a dead network does not spam the UI). A send that
    /// is already in flight is not dispatched again, so a slow SMTP server
    /// cannot produce duplicate deliveries. Scheduled sends report through
    /// `SendsUpdated` only; `SendResult` is reserved for the interactive
    /// send the composer is waiting on.
    pub fn check_due_sends(&self) {
        let now = chrono::Utc::now().timestamp();
        let due = match self.store.due_sends(now) {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!("due_sends failed: {e}");
                return;
            }
        };
        for (id, request) in due {
            if !self.in_flight_sends.lock().unwrap().insert(id) {
                continue;
            }
            let store = self.store.clone();
            let events_tx = self.events_tx.clone();
            let credentials = self.credentials.clone();
            let in_flight = InFlightSend {
                id,
                sends: self.in_flight_sends.clone(),
            };
            let account_id = request.account.0.clone();
            let task = tokio::spawn(async move {
                let _in_flight = in_flight;
                let result = smtp_send(&credentials, &request).await;
                report_credential_error(&events_tx, &request.account, &result);
                match result {
                    Ok(()) => {
                        if let Err(e) = store.remove_send(id) {
                            tracing::warn!("remove_send failed for {}: {e}", id);
                        }
                        let _ = events_tx.send(MailEvent::SendsUpdated);
                    }
                    Err(e) => tracing::warn!("scheduled send {} failed: {e}", id),
                }
            });
            self.track_send(account_id, task.abort_handle());
        }
    }

    /// Handle a command from the UI.
    ///
    /// Must be called from within a tokio runtime (send commands spawn tasks).
    pub fn handle_command(&self, cmd: UiCommand) {
        match cmd {
            UiCommand::UnlockAccount(id) => {
                let credentials = self.credentials.clone();
                let store = self.store.clone();
                let events = self.events_tx.clone();
                let syncers = self.syncers.clone();
                tokio::spawn(async move {
                    let Some(record) = store.get_account(&id.0).ok().flatten() else {
                        return;
                    };
                    let result = credentials.resume(record.clone()).await;
                    finish_restoration(record, result, &credentials, &store, &events, &syncers);
                });
            }
            UiCommand::RestoreAccounts => {
                let records = match self.store.account_records() {
                    Ok((records, malformed)) => {
                        if malformed > 0 {
                            let _ = self.events_tx.send(MailEvent::Error {
                                account: None,
                                message: "Some cached account settings could not be read.".into(),
                            });
                        }
                        records
                    }
                    Err(_) => {
                        let _ = self.events_tx.send(MailEvent::Error {
                            account: None,
                            message: "Cached accounts could not be restored.".into(),
                        });
                        return;
                    }
                };
                for record in records {
                    let credentials = self.credentials.clone();
                    let store = self.store.clone();
                    let events = self.events_tx.clone();
                    let syncers = self.syncers.clone();
                    tokio::spawn(async move {
                        let result = credentials.restore(record.clone()).await;
                        finish_restoration(record, result, &credentials, &store, &events, &syncers);
                    });
                }
            }
            UiCommand::DiscoverSettings { email, attempt } => {
                self.cancel_account_setup();
                let active = Arc::new(AtomicBool::new(true));
                *self.setup_active.lock().unwrap() = active.clone();
                let events = self.events_tx.clone();
                let task = tokio::spawn(async move {
                    let _ = events.send(MailEvent::AccountSetupProgress {
                        attempt,
                        stage: SetupStage::FindingSettings,
                    });
                    let result = crate::discovery::discover(&email)
                        .await
                        .unwrap_or(crate::discovery::DiscoveryResult::NotFound);
                    if active.load(Ordering::SeqCst) {
                        let _ =
                            events.send(MailEvent::AccountSettingsDiscovered { attempt, result });
                    }
                });
                *self.account_setup.lock().unwrap() = Some(task);
            }
            UiCommand::RetryAccountStorage {
                attempt,
                session_only,
            } => {
                let pending = self
                    .pending_setup
                    .lock()
                    .unwrap()
                    .clone()
                    .filter(|(id, _)| *id == attempt);
                if let Some((_, prepared)) = pending {
                    let active = self.setup_active.lock().unwrap().clone();
                    let credentials = self.credentials.clone();
                    let store = self.store.clone();
                    let events = self.events_tx.clone();
                    let syncers = self.syncers.clone();
                    let pending = self.pending_setup.clone();
                    let task = tokio::spawn(async move {
                        let worker_credentials = credentials.clone();
                        let worker_active = active.clone();
                        let result = tokio::spawn(async move {
                            worker_credentials
                                .install_checked(
                                    prepared.config,
                                    prepared.registration,
                                    session_only,
                                    worker_active,
                                )
                                .await
                        })
                        .await;
                        if !active.load(Ordering::SeqCst) {
                            return;
                        }
                        let error = match result {
                            Ok(Ok(record)) => {
                                start_account_workers(
                                    record,
                                    &credentials,
                                    &store,
                                    &events,
                                    &syncers,
                                );
                                *pending.lock().unwrap() = None;
                                None
                            }
                            Ok(Err(error)) => Some(SetupFailure::from_error(&error)),
                            Err(_) => Some(SetupFailure::Network),
                        };
                        let _ = events.send(MailEvent::AccountSetupFinished { attempt, error });
                    });
                    *self.account_setup.lock().unwrap() = Some(task);
                }
            }
            UiCommand::SetCategory {
                message_id,
                category,
                sender,
            } => self.organization_result(self.store.set_category(message_id, category, sender)),
            UiCommand::SaveLabel { id, name, color } => {
                self.organization_result(self.store.save_label(id, &name, &color))
            }
            UiCommand::DeleteLabel { id } => self.organization_result(self.store.delete_label(id)),
            UiCommand::SetMessageLabel {
                message_id,
                label_id,
                assigned,
            } => self
                .organization_result(self.store.set_message_label(message_id, label_id, assigned)),
            UiCommand::RequestAvatar(address) => {
                self.avatars.request(address, self.events_tx.clone())
            }
            UiCommand::SetOnlineAvatars(enabled) => self.avatars.set_enabled(enabled),
            UiCommand::AddAccount(config) => {
                self.add_account(config);
            }
            UiCommand::CancelAccountSetup => self.cancel_account_setup(),
            UiCommand::ConnectAccount { attempt, config } => {
                self.start_account_setup(attempt, async move { Ok(config) });
            }
            UiCommand::OAuthLogin {
                provider,
                client_id,
                client_secret,
                email,
                attempt,
                replace,
            } => {
                let registration = OAuthRegistration {
                    client_id: client_id.clone(),
                    desktop_parameter: if provider == OAuthProvider::Google {
                        client_secret.clone()
                    } else {
                        None
                    },
                };
                let expected = replace.and_then(|id| self.store.get_account(&id.0).ok().flatten());
                let events = self.events_tx.clone();
                let _ = events.send(MailEvent::AccountSetupProgress {
                    attempt,
                    stage: SetupStage::OpeningBrowser,
                });
                self.start_registered_setup(
                    attempt,
                    async move {
                        let _ = events.send(MailEvent::AccountSetupProgress {
                            attempt,
                            stage: SetupStage::WaitingForAuthorization,
                        });
                        let tokens = crate::oauth::start_flow_with_options(
                            provider,
                            &client_id,
                            client_secret.as_deref(),
                            &email,
                        )
                        .await?;
                        // Exchange access tokens cannot call Microsoft Graph /me.
                        // The supplied mailbox is authenticated by IMAP/SMTP below.
                        let email = match provider {
                            OAuthProvider::Google => {
                                crate::oauth::fetch_email(provider, &tokens.access_token).await?
                            }
                            OAuthProvider::Microsoft if email.contains('@') => email,
                            OAuthProvider::Microsoft => {
                                return Err(Error::Config("Enter a valid email address.".into()))
                            }
                        };
                        if expected
                            .as_ref()
                            .is_some_and(|old| !old.email.eq_ignore_ascii_case(&email))
                        {
                            return Err(Error::Auth(
                                "Choose the account you are reconnecting.".into(),
                            ));
                        }
                        Ok(AccountConfig {
                            id: expected
                                .map(|old| old.id)
                                .unwrap_or_else(|| AccountId(email.clone())),
                            email,
                            imap: default_imap(provider),
                            smtp: default_smtp(provider),
                            auth: AuthMethod::OAuth2 {
                                provider,
                                access_token: tokens.access_token,
                                refresh_token: tokens.refresh_token,
                                expires_at: tokens.expires_at,
                            },
                        })
                    },
                    Some(registration),
                );
            }
            UiCommand::RemoveAccount(id) => {
                if !self.removing_accounts.lock().unwrap().insert(id.0.clone()) {
                    return;
                }
                self.cancel_account_setup();
                self.credentials.invalidate(&id.0);
                if let Some(tasks) = self.send_tasks.lock().unwrap().remove(&id.0) {
                    for task in tasks {
                        task.abort();
                    }
                }
                if let Some(tx) = self.syncers.lock().unwrap().remove(&id.0) {
                    let _ = tx.send(SyncerCommand::Quit);
                }
                let credentials = self.credentials.clone();
                let events = self.events_tx.clone();
                let removing = self.removing_accounts.clone();
                tokio::spawn(async move {
                    let error = credentials
                        .remove(&id.0)
                        .await
                        .err()
                        .map(|e| SetupFailure::from_error(&e));
                    removing.lock().unwrap().remove(&id.0);
                    let _ = events.send(MailEvent::AccountRemoved { account: id, error });
                });
            }
            // The UI reads the local store directly for folder contents.
            UiCommand::SelectFolder { .. } => {}
            UiCommand::OpenMessage { message_id, .. } => {
                match self.store.get_message_full(message_id) {
                    Ok(Some(full)) => {
                        // Older caches did not retain CID metadata. Keep the
                        // offline body visible while refreshing those resources.
                        let refresh = full.body_html.to_ascii_lowercase().contains("cid:")
                            && full.attachments.iter().all(|a| a.content_id.is_empty());
                        let account = full.summary.account_id.clone();
                        let _ = self
                            .events_tx
                            .send(MailEvent::MessageLoaded { message: full });
                        if refresh {
                            if let Some(tx) = self.syncers.lock().unwrap().get(&account) {
                                let _ = tx.send(SyncerCommand::FetchBody { message_id });
                            }
                        }
                    }
                    Ok(_) => {
                        // Body not cached: ask the account's syncer to fetch it.
                        let account = self
                            .store
                            .get_message(message_id)
                            .ok()
                            .flatten()
                            .map(|m| m.account_id);
                        if let Some(account) = account {
                            if let Some(tx) = self.syncers.lock().unwrap().get(&account) {
                                let _ = tx.send(SyncerCommand::FetchBody { message_id });
                            }
                        }
                    }
                    Err(e) => {
                        let _ = self.events_tx.send(MailEvent::Error {
                            account: None,
                            message: e.to_string(),
                        });
                    }
                }
            }
            UiCommand::SetFlags {
                account,
                message_ids,
                flags,
                add,
            } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::SetFlags {
                        message_ids,
                        flags,
                        add,
                    });
                }
            }
            UiCommand::Delete {
                account,
                message_ids,
            } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::Delete { message_ids });
                }
            }
            UiCommand::Move {
                account,
                message_ids,
                dest_folder_id,
            } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::Move {
                        message_ids,
                        dest_folder_id,
                    });
                }
            }
            UiCommand::MarkFolderRead { account, folder_id } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::MarkFolderRead { folder_id });
                }
            }
            UiCommand::EmptyFolder { account, folder_id } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::EmptyFolder { folder_id });
                }
            }
            UiCommand::CreateFolder { account, name } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::CreateFolder { name });
                }
            }
            UiCommand::RenameFolder {
                account,
                folder_id,
                name,
            } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::RenameFolder { folder_id, name });
                }
            }
            UiCommand::DeleteFolder { account, folder_id } => {
                if let Some(tx) = self.syncers.lock().unwrap().get(&account.0) {
                    let _ = tx.send(SyncerCommand::DeleteFolder { folder_id });
                }
            }
            // Snooze and follow-up are local operations: they only touch the
            // store, so they are handled here rather than routed to a syncer.
            UiCommand::Snooze { message_ids, until } => {
                self.update_local(&message_ids, |store| {
                    store.snooze_messages(&message_ids, until)
                });
            }
            UiCommand::Unsnooze { message_ids } => {
                self.update_local(&message_ids, |store| store.unsnooze_messages(&message_ids));
            }
            UiCommand::SetFollowUp { message_ids, at } => {
                self.update_local(&message_ids, |store| store.set_follow_up(&message_ids, at));
            }
            UiCommand::ClearFollowUp { message_ids } => {
                self.update_local(&message_ids, |store| store.clear_follow_up(&message_ids));
            }
            UiCommand::ScheduleSend { send_at, request } => {
                // Reject unknown accounts now: the send would otherwise
                // retry forever against a dead account.
                match self.store.get_account(&request.account.0) {
                    Ok(Some(_)) => match self.store.schedule_send(&request, send_at) {
                        Ok(_) => {
                            let _ = self.events_tx.send(MailEvent::SendScheduled {
                                account: request.account,
                                send_at,
                            });
                        }
                        Err(e) => {
                            let _ = self.events_tx.send(MailEvent::Error {
                                account: Some(request.account),
                                message: e.to_string(),
                            });
                        }
                    },
                    _ => {
                        let message = format!("unknown account {}", request.account.0);
                        let _ = self.events_tx.send(MailEvent::Error {
                            account: Some(request.account),
                            message,
                        });
                    }
                }
                let _ = self.events_tx.send(MailEvent::SendsUpdated);
            }
            UiCommand::CancelSend { id } => {
                if let Err(e) = self.store.remove_send(id) {
                    let _ = self.events_tx.send(MailEvent::Error {
                        account: None,
                        message: e.to_string(),
                    });
                } else {
                    let _ = self.events_tx.send(MailEvent::SendsUpdated);
                }
            }
            UiCommand::SaveDraft(request) => match self.store.save_draft(&request) {
                Ok(_) => {
                    let _ = self.events_tx.send(MailEvent::DraftsUpdated);
                }
                Err(e) => {
                    let _ = self.events_tx.send(MailEvent::Error {
                        account: Some(request.account),
                        message: e.to_string(),
                    });
                }
            },
            UiCommand::DeleteDraft { id } => {
                if let Err(e) = self.store.delete_draft(id) {
                    let _ = self.events_tx.send(MailEvent::Error {
                        account: None,
                        message: e.to_string(),
                    });
                } else {
                    let _ = self.events_tx.send(MailEvent::DraftsUpdated);
                }
            }
            UiCommand::SaveTemplate { name, request } => {
                match self.store.save_template(&name, &request) {
                    Ok(_) => {
                        let _ = self.events_tx.send(MailEvent::TemplatesUpdated);
                    }
                    Err(e) => {
                        let _ = self.events_tx.send(MailEvent::Error {
                            account: Some(request.account),
                            message: e.to_string(),
                        });
                    }
                }
            }
            UiCommand::DeleteTemplate { id } => {
                if let Err(e) = self.store.delete_template(id) {
                    let _ = self.events_tx.send(MailEvent::Error {
                        account: None,
                        message: e.to_string(),
                    });
                } else {
                    let _ = self.events_tx.send(MailEvent::TemplatesUpdated);
                }
            }
            UiCommand::SendEmail(req) => {
                let events_tx = self.events_tx.clone();
                let credentials = self.credentials.clone();
                let account_id = req.account.0.clone();
                let task = tokio::spawn(async move {
                    let result = smtp_send(&credentials, &req).await;
                    report_credential_error(&events_tx, &req.account, &result);
                    let _ = events_tx.send(MailEvent::SendResult {
                        account: req.account,
                        ok: result.is_ok(),
                        error: result.err().map(|e| e.to_string()),
                    });
                });
                self.track_send(account_id, task.abort_handle());
            }
            UiCommand::Search { account, query } => {
                match self.store.search(&account.0, &query, 100) {
                    Ok(messages) => {
                        let _ = self
                            .events_tx
                            .send(MailEvent::SearchResults { account, messages });
                    }
                    Err(e) => {
                        let _ = self.events_tx.send(MailEvent::Error {
                            account: Some(account),
                            message: e.to_string(),
                        });
                    }
                }
            }
            UiCommand::Quit => {
                self.cancel_account_setup();
                for task in self
                    .send_tasks
                    .lock()
                    .unwrap()
                    .drain()
                    .flat_map(|(_, tasks)| tasks)
                {
                    task.abort();
                }
                let records = match self.store.account_records() {
                    Ok((records, malformed)) => {
                        if malformed > 0 {
                            let _ = self.events_tx.send(MailEvent::Error {
                                account: None,
                                message: "Some cached account settings could not be read.".into(),
                            });
                        }
                        records
                    }
                    Err(_) => {
                        let _ = self.events_tx.send(MailEvent::Error {
                            account: None,
                            message: "Cached accounts could not be restored.".into(),
                        });
                        return;
                    }
                };
                for record in records {
                    self.credentials.invalidate(&record.id.0);
                }
                for tx in self.syncers.lock().unwrap().drain().map(|(_, tx)| tx) {
                    let _ = tx.send(SyncerCommand::Quit);
                }
            }
        }
    }
}

/// Send a request over the account's SMTP endpoint.
async fn smtp_send(credentials: &CredentialManager, request: &SendRequest) -> Result<()> {
    let lease = credentials.configuration(&request.account.0).await?;
    let account = &lease.config;
    let result = crate::smtp::send(
        &account.smtp,
        &account.auth,
        &account.email,
        &request.from,
        &request.to,
        &request.cc,
        &request.bcc,
        &request.subject,
        &request.body_text,
        request.body_html.as_deref(),
        &request.attachments,
        request.in_reply_to.as_deref(),
        request.references.as_deref(),
    )
    .await;
    if matches!(&result, Err(Error::Smtp(e)) if crate::smtp::authentication_rejected(e)) {
        return Err(lease.reject().await);
    }
    result
}

fn report_credential_error(
    events: &UnboundedSender<MailEvent>,
    account: &AccountId,
    result: &Result<()>,
) {
    let status = match result {
        Err(Error::ReconnectRequired) => Some(AccountStatus::ReconnectRequired),
        Err(Error::SecureStorage) => Some(AccountStatus::StorageLocked),
        _ => None,
    };
    if let Some(status) = status {
        let _ = events.send(MailEvent::AccountStatus {
            account: account.clone(),
            status,
        });
    }
}

/// Default IMAP endpoint for an OAuth provider.
fn default_imap(provider: OAuthProvider) -> ImapConfig {
    crate::providers::endpoints(provider).0
}
fn default_smtp(provider: OAuthProvider) -> SmtpConfig {
    crate::providers::endpoints(provider).1
}

#[cfg(test)]
mod account_setup_tests {
    use super::*;
    #[tokio::test]
    async fn overlapping_removals_emit_one_completion_and_allow_retry() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        let service = MailService::new(store, tx);
        let command = UiCommand::RemoveAccount(crate::AccountId("missing-test-account".into()));
        service.handle_command(command.clone());
        service.handle_command(command.clone());
        assert!(matches!(
            events.recv().await,
            Some(MailEvent::AccountRemoved { .. })
        ));
        assert!(events.try_recv().is_err());
        assert!(service.removing_accounts.lock().unwrap().is_empty());
        service.handle_command(command);
        assert!(matches!(
            events.recv().await,
            Some(MailEvent::AccountRemoved { .. })
        ));
    }

    #[tokio::test]
    async fn failed_account_setup_is_reported_without_saving_credentials() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"* OK local IMAP\r\n").await.unwrap();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let tag = line.split_whitespace().next().unwrap();
            write
                .write_all(format!("{tag} NO Authentication failed\r\n").as_bytes())
                .await
                .unwrap();
        });
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let service = MailService::new(store.clone(), tx);
        service.handle_command(UiCommand::ConnectAccount {
            attempt: 42,
            config: AccountConfig {
                id: AccountId("new@example.test".into()),
                email: "new@example.test".into(),
                imap: ImapConfig {
                    host: "127.0.0.1".into(),
                    port,
                    tls: TlsMode::Plaintext,
                },
                smtp: SmtpConfig {
                    host: "127.0.0.1".into(),
                    port,
                    tls: TlsMode::Plaintext,
                },
                auth: AuthMethod::Password {
                    smtp_username: String::new(),
                    username: "local".into(),
                    password: "rejected".into(),
                },
            },
        });
        let checking = rx.recv().await.unwrap();
        assert!(matches!(
            checking,
            MailEvent::AccountSetupChecking { attempt: 42 }
        ));
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            MailEvent::AccountSetupFinished {
                attempt: 42,
                error: Some(_)
            }
        ));
        assert!(store.get_accounts().unwrap().is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancel_aborts_setup_and_only_a_new_attempt_reports_a_result() {
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let service = MailService::new(store.clone(), tx);
        service.start_account_setup(1, std::future::pending());
        service.handle_command(UiCommand::CancelAccountSetup);
        service.start_account_setup(2, async { Err(Error::Auth("declined".into())) });
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            event,
            MailEvent::AccountSetupFinished {
                attempt: 2,
                error: Some(_)
            }
        ));
        assert!(rx.try_recv().is_err());
        assert!(store.get_accounts().unwrap().is_empty());
    }

    #[test]
    fn gmail_uses_documented_endpoints() {
        assert_eq!(default_imap(OAuthProvider::Google).host, "imap.gmail.com");
        assert_eq!(default_smtp(OAuthProvider::Google).host, "smtp.gmail.com");
    }
}

#[cfg(test)]
mod scheduled_send_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A send that is still in flight must not be dispatched again by the
    /// next tick, or the recipient would get the email twice.
    #[tokio::test]
    async fn in_flight_scheduled_send_is_not_dispatched_twice() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_server = accepted.clone();
        // Accepts connections and holds them open without a greeting, so the
        // send stays in flight.
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                accepted_server.fetch_add(1, Ordering::SeqCst);
                held.push(stream);
            }
        });
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let account = AccountConfig {
            id: AccountId("send@example.test".into()),
            email: "send@example.test".into(),
            imap: ImapConfig {
                host: "127.0.0.1".into(),
                port: 1,
                tls: TlsMode::Plaintext,
            },
            smtp: SmtpConfig {
                host: "127.0.0.1".into(),
                port,
                tls: TlsMode::Plaintext,
            },
            auth: AuthMethod::Password {
                smtp_username: String::new(),
                username: "local".into(),
                password: "pw".into(),
            },
        };
        store.save_account(&account).unwrap();
        store
            .schedule_send(
                &SendRequest {
                    account: AccountId("send@example.test".into()),
                    from: "send@example.test".into(),
                    to: vec!["rcpt@example.test".into()],
                    cc: vec![],
                    bcc: vec![],
                    subject: "due".into(),
                    body_text: "body".into(),
                    body_html: None,
                    attachments: vec![],
                    in_reply_to: None,
                    references: None,
                },
                chrono::Utc::now().timestamp() - 1,
            )
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let service = MailService::new(store.clone(), tx);
        service
            .credentials
            .install(account, None, true)
            .await
            .unwrap();
        service.check_due_sends();
        for _ in 0..100 {
            if accepted.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        service.check_due_sends();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        service.handle_command(UiCommand::RemoveAccount(AccountId(
            "send@example.test".into(),
        )));
        for _ in 0..100 {
            if store.get_accounts().unwrap().is_empty()
                && service.in_flight_sends.lock().unwrap().is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(store.get_accounts().unwrap().is_empty());
        assert!(
            service.in_flight_sends.lock().unwrap().is_empty(),
            "cancelled sends must release the dispatch reservation"
        );
        server.abort();
    }
}
