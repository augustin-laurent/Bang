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
use crate::message::AccountStatus;
use crate::store::Store;
use crate::sync::{start_syncer, SyncerCommand};

/// Owns the store and one syncer per account.
///
/// Commands are handled synchronously; long-running work (SMTP sends and
/// OAuth logins) is spawned on the ambient tokio runtime.
pub struct MailService {
    store: Arc<Store>,
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
            UiCommand::AddAccount(config) => {
                self.add_account(config);
            }
            UiCommand::OAuthLogin {
                provider,
                client_id,
            } => {
                let store = self.store.clone();
                let events_tx = self.events_tx.clone();
                let syncers = self.syncers.clone();
                tokio::spawn(async move {
                    let result = async {
                        let tokens = crate::oauth::start_flow(provider, &client_id).await?;
                        let email =
                            crate::oauth::fetch_email(provider, &tokens.access_token).await?;
                        Ok::<_, Error>((tokens, email))
                    }
                    .await;
                    match result {
                        Ok((tokens, email)) => {
                            let config = AccountConfig {
                                id: AccountId(email.clone()),
                                email: email.clone(),
                                imap: default_imap(provider),
                                smtp: default_smtp(provider),
                                auth: AuthMethod::OAuth2 {
                                    provider,
                                    access_token: tokens.access_token,
                                    refresh_token: tokens.refresh_token,
                                    expires_at: tokens.expires_at,
                                },
                            };
                            let tx = start_syncer(
                                config.clone(),
                                store.clone(),
                                events_tx.clone(),
                            );
                            syncers
                                .lock()
                                .unwrap()
                                .insert(config.id.0.clone(), tx);
                            let _ = store.save_account(&config);
                            let _ = events_tx.send(MailEvent::AccountStatus {
                                account: config.id,
                                status: AccountStatus::Syncing,
                            });
                        }
                        Err(e) => {
                            let _ = events_tx.send(MailEvent::Error {
                                account: None,
                                message: format!("Sign-in failed: {e}"),
                            });
                        }
                    }
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
                    Ok(Some(full)) if !full.body_text.is_empty() || !full.body_html.is_empty() => {
                        let _ = self
                            .events_tx
                            .send(MailEvent::MessageLoaded { message: full });
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
            host: "imap-mail.google.com".into(),
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
            host: "smtp-mail.google.com".into(),
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
