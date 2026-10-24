//! The mail service: owns the store and the per-account syncers, and
//! dispatches UI commands to them.
//!
//! The service is driven from a tokio runtime (see the app's bridge); it is
//! `Send + Sync` and can be shared across tasks.

use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use crate::account::{AccountConfig, AccountId, AuthMethod, ImapConfig, OAuthProvider, SmtpConfig, TlsMode};
use crate::error::{Error, Result};
use crate::events::{MailEvent, SendRequest, UiCommand};
use crate::store::Store;
use crate::sync::{start_syncer, SyncerCommand};

/// Owns the store and one syncer per account.
///
/// Commands are handled synchronously; long-running work (SMTP sends and
/// OAuth logins) is spawned on the ambient tokio runtime.
pub struct MailService {
    store: Arc<Store>,
    account_setup: Mutex<Option<tokio::task::JoinHandle<()>>>,
    avatars: Arc<crate::avatars::AvatarCache>,
    events_tx: UnboundedSender<MailEvent>,
    syncers: Arc<Mutex<HashMap<String, std_mpsc::Sender<SyncerCommand>>>>,
}

impl MailService {
    /// Create a service around a store.
    ///
    /// # Arguments
    /// * `store` — the shared local store.
    /// * `events_tx` — where core events are sent.
    pub fn new(store: Arc<Store>, events_tx: UnboundedSender<MailEvent>) -> Self {
        Self {
            store,
            account_setup: Mutex::new(None),
            avatars: crate::avatars::AvatarCache::new(),
            events_tx,
            syncers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a new account and start its syncer.
    ///
    /// If a syncer is already running for this account (e.g. the user
    /// re-added it with new credentials), it is quit first so the old
    /// in-memory config cannot keep syncing alongside the new one.
    fn add_account(&self, config: AccountConfig) {
        let _ = self.store.save_account(&config);
        let mut syncers = self.syncers.lock().unwrap();
        if let Some(old) = syncers.get(&config.id.0) {
            let _ = old.send(SyncerCommand::Quit);
        }
        let tx = start_syncer(config.clone(), self.store.clone(), self.events_tx.clone());
        syncers.insert(config.id.0.clone(), tx);
    }

    fn cancel_account_setup(&self) {
        if let Some(task) = self.account_setup.lock().unwrap().take() {
            task.abort();
        }
    }

    fn start_account_setup(
        &self,
        attempt: i32,
        account: impl std::future::Future<Output = Result<AccountConfig>> + Send + 'static,
    ) {
        self.cancel_account_setup();
        let store = self.store.clone();
        let events = self.events_tx.clone();
        let syncers = self.syncers.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let config = account.await?;
                let _ = events.send(MailEvent::AccountSetupChecking { attempt });
                // Do not persist invalid credentials or send a test message.
                let check = config.clone();
                tokio::time::timeout(std::time::Duration::from_secs(30), async {
                    tokio::task::spawn_blocking(move || {
                        crate::imap::ImapConn::connect(&check.imap, &check.auth, &check.email)
                            .map(|_| ())
                    })
                    .await
                    .map_err(|e| Error::Network(e.to_string()))??;
                    crate::smtp::test_connection(&config.smtp, &config.auth, &config.email).await
                })
                .await
                .map_err(|_| Error::Network("Connection timed out".into()))??;
                store.save_account(&config)?;
                let mut syncers = syncers.lock().unwrap();
                if let Some(old) = syncers.remove(&config.id.0) {
                    let _ = old.send(SyncerCommand::Quit);
                }
                let tx = start_syncer(config.clone(), store, events.clone());
                syncers.insert(config.id.0, tx);
                Ok::<_, Error>(())
            }
            .await;
            if let Err(error) = &result {
                tracing::warn!("account setup failed: {error}");
            }
            let _ = events.send(MailEvent::AccountSetupFinished {
                attempt,
                error: result.err().map(|e| e.to_string()),
            });
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
                    Error::Store(message) if message.contains("UNIQUE constraint failed: labels.name") => "A label with this name already exists.".into(),
                    Error::Store(message) => message,
                    other => other.to_string(),
                };
                MailEvent::OrganizationError { message }
            },
        };
        let _ = self.events_tx.send(event);
    }

    /// Dispatch every pending send whose time has come.
    ///
    /// Called periodically by the app's bridge. A successful send is removed
    /// from the pending list; a failed one is kept and retried on the next
    /// tick (no event, so a dead network does not spam the UI).
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
            let store = self.store.clone();
            let events_tx = self.events_tx.clone();
            tokio::spawn(async move {
                match smtp_send(&store, &request).await {
                    Ok(()) => {
                        if let Err(e) = store.remove_send(id) {
                            tracing::warn!("remove_send failed for {}: {e}", id);
                        }
                        let _ = events_tx.send(MailEvent::SendsUpdated);
                        let _ = events_tx.send(MailEvent::SendResult {
                            account: request.account,
                            ok: true,
                            error: None,
                        });
                    }
                    Err(e) => tracing::warn!("scheduled send {} failed: {e}", id),
                }
            });
        }
    }

    /// Handle a command from the UI.
    ///
    /// Must be called from within a tokio runtime (send commands spawn tasks).
    pub fn handle_command(&self, cmd: UiCommand) {
        match cmd {
            UiCommand::SetCategory { message_id, category, sender } => self.organization_result(self.store.set_category(message_id, category, sender)),
            UiCommand::SaveLabel { id, name, color } => self.organization_result(self.store.save_label(id, &name, &color)),
            UiCommand::DeleteLabel { id } => self.organization_result(self.store.delete_label(id)),
            UiCommand::SetMessageLabel { message_id, label_id, assigned } => self.organization_result(self.store.set_message_label(message_id, label_id, assigned)),
            UiCommand::RequestAvatar(address) => self.avatars.request(address, self.events_tx.clone()),
            UiCommand::SetOnlineAvatars(enabled) => self.avatars.set_enabled(enabled),
            UiCommand::AddAccount(config) => {
                self.add_account(config);
            }
            UiCommand::CancelAccountSetup => self.cancel_account_setup(),
            UiCommand::ConnectAccount { attempt, config } => {
                self.start_account_setup(attempt, async move { Ok(config) });
            }
            UiCommand::OAuthLogin { provider, client_id, client_secret, email, attempt } => {
                self.start_account_setup(attempt, async move {
                    let tokens = crate::oauth::start_flow_with_options(
                        provider, &client_id, client_secret.as_deref(), &email,
                    ).await?;
                    // Exchange access tokens cannot call Microsoft Graph /me.
                    // The supplied mailbox is authenticated by IMAP/SMTP below.
                    let email = match provider {
                        OAuthProvider::Google => crate::oauth::fetch_email(provider, &tokens.access_token).await?,
                        OAuthProvider::Microsoft if email.contains('@') => email,
                        OAuthProvider::Microsoft => return Err(Error::Config("Enter a valid email address.".into())),
                    };
                    Ok(AccountConfig {
                        id: AccountId(email.clone()), email,
                        imap: default_imap(provider), smtp: default_smtp(provider),
                        auth: AuthMethod::OAuth2 {
                            provider, access_token: tokens.access_token,
                            refresh_token: tokens.refresh_token, expires_at: tokens.expires_at,
                        },
                    })
                });
            }
            UiCommand::RemoveAccount(id) => {
                if let Some(tx) = self.syncers.lock().unwrap().remove(&id.0) {
                    let _ = tx.send(SyncerCommand::Quit);
                }
                let _ = self.store.delete_account(&id.0);
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
                        let _ = self.events_tx.send(MailEvent::MessageLoaded { message: full });
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
            // Snooze and follow-up are local operations: they only touch the
            // store, so they are handled here rather than routed to a syncer.
            UiCommand::Snooze {
                message_ids,
                until,
            } => {
                self.update_local(&message_ids, |store| store.snooze_messages(&message_ids, until));
            }
            UiCommand::Unsnooze { message_ids } => {
                self.update_local(&message_ids, |store| store.unsnooze_messages(&message_ids));
            }
            UiCommand::SetFollowUp {
                message_ids,
                at,
            } => {
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
            UiCommand::SaveTemplate { name, request } => match self.store.save_template(&name, &request) {
                Ok(_) => {
                    let _ = self.events_tx.send(MailEvent::TemplatesUpdated);
                }
                Err(e) => {
                    let _ = self.events_tx.send(MailEvent::Error {
                        account: Some(request.account),
                        message: e.to_string(),
                    });
                }
            },
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
                let store = self.store.clone();
                let events_tx = self.events_tx.clone();
                tokio::spawn(async move {
                    let result = smtp_send(&store, &req).await;
                    let _ = events_tx.send(MailEvent::SendResult {
                        account: req.account,
                        ok: result.is_ok(),
                        error: result.err().map(|e| e.to_string()),
                    });
                });
            }
            UiCommand::Search { account, query } => {
                match self.store.search(&account.0, &query, 100) {
                    Ok(messages) => {
                        let _ = self.events_tx.send(MailEvent::SearchResults {
                            account,
                            messages,
                        });
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
                for tx in self.syncers.lock().unwrap().drain().map(|(_, tx)| tx) {
                    let _ = tx.send(SyncerCommand::Quit);
                }
            }
        }
    }
}

/// Send a request over the account's SMTP endpoint.
async fn smtp_send(store: &Store, request: &SendRequest) -> Result<()> {
    match store.get_account(&request.account.0) {
        Ok(Some(acc)) => crate::smtp::send(
            &acc.smtp,
            &acc.auth,
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
        .await,
        _ => Err(Error::Config(format!(
            "unknown account {}",
            request.account.0
        ))),
    }
}

/// Default IMAP endpoint for an OAuth provider.
fn default_imap(provider: OAuthProvider) -> ImapConfig {
    match provider {
        OAuthProvider::Google => ImapConfig {
            host: "imap.gmail.com".into(),
            port: 993,
            tls: TlsMode::Tls,
        },
        OAuthProvider::Microsoft => ImapConfig {
            host: "outlook.office365.com".into(),
            port: 993,
            tls: TlsMode::Tls,
        },
    }
}

/// Default SMTP endpoint for an OAuth provider.
fn default_smtp(provider: OAuthProvider) -> SmtpConfig {
    match provider {
        OAuthProvider::Google => SmtpConfig {
            host: "smtp.gmail.com".into(),
            port: 465,
            tls: TlsMode::Tls,
        },
        OAuthProvider::Microsoft => SmtpConfig {
            host: "smtp.office365.com".into(),
            port: 587,
            tls: TlsMode::Starttls,
        },
    }
}

#[cfg(test)]
mod account_setup_tests {
    use super::*;
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
