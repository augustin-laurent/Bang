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
use crate::error::Error;
use crate::events::{MailEvent, UiCommand};
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
    fn add_account(&self, config: AccountConfig) {
        let _ = self.store.save_account(&config);
        let tx = start_syncer(config.clone(), self.store.clone(), self.events_tx.clone());
        self.syncers.lock().unwrap().insert(config.id.0.clone(), tx);
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
            UiCommand::SendEmail(req) => {
                let store = self.store.clone();
                let events_tx = self.events_tx.clone();
                tokio::spawn(async move {
                    let result = match store.get_account(&req.account.0) {
                        Ok(Some(acc)) => crate::smtp::send(
                            &acc.smtp,
                            &acc.auth,
                            &req.from,
                            &req.to,
                            &req.cc,
                            &req.bcc,
                            &req.subject,
                            &req.body_text,
                            req.in_reply_to.as_deref(),
                            req.references.as_deref(),
                        )
                        .await,
                        _ => Err(Error::Config(format!(
                            "unknown account {}",
                            req.account.0
                        ))),
                    };
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
