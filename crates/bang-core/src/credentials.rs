//! One credential lifecycle for incoming mail, outgoing mail and secure storage.
use crate::{
    AccountConfig, AuthMethod, Error, OAuthRegistration, Result, Store, StoredAccount, StoredAuth,
};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

type StorageFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Secret storage seam; implementations must never put credentials in the mail DB.
pub trait CredentialStore: Send + Sync {
    /// Read a credential payload by its opaque reference.
    fn load<'a>(&'a self, key: &'a str) -> StorageFuture<'a, Option<AuthMethod>>;
    /// Save or replace a credential payload.
    fn save<'a>(&'a self, key: &'a str, auth: &'a AuthMethod) -> StorageFuture<'a, ()>;
    /// Delete a saved credential.
    fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()>;
}

/// Linux desktop keyring, accessed over the user's session D-Bus.
pub struct LinuxSecretStore;

impl CredentialStore for LinuxSecretStore {
    fn load<'a>(&'a self, key: &'a str) -> StorageFuture<'a, Option<AuthMethod>> {
        Box::pin(async move {
            let service =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh)
                    .await
                    .map_err(|_| Error::SecureStorage)?;
            let found = service
                .search_items(HashMap::from([
                    ("application", "bang"),
                    ("credential", key),
                ]))
                .await
                .map_err(|_| Error::SecureStorage)?;
            let item = found.unlocked.into_iter().chain(found.locked).next();
            match item {
                Some(item) => {
                    if item.is_locked().await.map_err(|_| Error::SecureStorage)? {
                        item.unlock().await.map_err(|_| Error::SecureStorage)?;
                    }
                    let bytes = item.get_secret().await.map_err(|_| Error::SecureStorage)?;
                    serde_json::from_slice(&bytes)
                        .map(Some)
                        .map_err(|_| Error::SecureStorage)
                }
                None => Ok(None),
            }
        })
    }
    fn save<'a>(&'a self, key: &'a str, auth: &'a AuthMethod) -> StorageFuture<'a, ()> {
        Box::pin(async move {
            let service =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh)
                    .await
                    .map_err(|_| Error::SecureStorage)?;
            let collection = service
                .get_default_collection()
                .await
                .map_err(|_| Error::SecureStorage)?;
            if collection
                .is_locked()
                .await
                .map_err(|_| Error::SecureStorage)?
            {
                collection
                    .unlock()
                    .await
                    .map_err(|_| Error::SecureStorage)?;
            }
            let bytes = serde_json::to_vec(auth).map_err(|_| Error::SecureStorage)?;
            collection
                .create_item(
                    "Bang mail account",
                    HashMap::from([("application", "bang"), ("credential", key)]),
                    &bytes,
                    true,
                    "application/json",
                )
                .await
                .map_err(|_| Error::SecureStorage)?;
            Ok(())
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()> {
        Box::pin(async move {
            let service =
                secret_service::SecretService::connect(secret_service::EncryptionType::Dh)
                    .await
                    .map_err(|_| Error::SecureStorage)?;
            let found = service
                .search_items(HashMap::from([
                    ("application", "bang"),
                    ("credential", key),
                ]))
                .await
                .map_err(|_| Error::SecureStorage)?;
            for item in found.unlocked.into_iter().chain(found.locked) {
                item.delete().await.map_err(|_| Error::SecureStorage)?;
            }
            Ok(())
        })
    }
}

/// Memory-only backend used by tests; session-only accounts bypass any backend.
#[derive(Default)]
pub struct MemorySecretStore(Mutex<HashMap<String, AuthMethod>>);
impl CredentialStore for MemorySecretStore {
    fn load<'a>(&'a self, key: &'a str) -> StorageFuture<'a, Option<AuthMethod>> {
        Box::pin(async move { Ok(self.0.lock().unwrap().get(key).cloned()) })
    }
    fn save<'a>(&'a self, key: &'a str, auth: &'a AuthMethod) -> StorageFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().insert(key.into(), auth.clone());
            Ok(())
        })
    }
    fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().remove(key);
            Ok(())
        })
    }
}

struct State {
    record: StoredAccount,
    auth: Option<AuthMethod>,
    revoked: bool,
    dirty: bool,
    retry_after: i64,
}
struct Entry {
    live: AtomicBool,
    state: AsyncMutex<State>,
}

/// Runtime configuration bound to exactly one account generation.
pub struct CredentialLease {
    /// Coherent endpoints and credentials; never send this to UI or SQLite.
    pub config: AccountConfig,
    entry: Arc<Entry>,
}
impl CredentialLease {
    /// Reject only the generation that actually attempted authentication.
    pub async fn reject(&self) -> Error {
        let mut state = self.entry.state.lock().await;
        if !self.entry.live.load(Ordering::SeqCst) {
            return Error::Cancelled;
        }
        state.revoked = true;
        Error::ReconnectRequired
    }
}

/// Shared, serialized refresh and persistence for every account's credentials.
pub struct CredentialManager {
    store: Arc<Store>,
    backend: Arc<dyn CredentialStore>,
    accounts: Mutex<HashMap<String, Arc<Entry>>>,
    retired: Mutex<HashSet<String>>,
    refresh_url: Option<String>,
}

impl CredentialManager {
    /// Construct without contacting D-Bus or a provider.
    pub fn new(store: Arc<Store>, backend: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            backend,
            accounts: Mutex::new(HashMap::new()),
            retired: Mutex::new(HashSet::new()),
            refresh_url: None,
        }
    }

    fn entry(&self, id: &str) -> Result<Arc<Entry>> {
        self.accounts
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(Error::ReconnectRequired)
    }

    /// Install a validated account. Persist secure storage before mail metadata.
    pub async fn install(
        &self,
        config: AccountConfig,
        registration: Option<OAuthRegistration>,
        session_only: bool,
    ) -> Result<StoredAccount> {
        self.install_checked(
            config,
            registration,
            session_only,
            Arc::new(AtomicBool::new(true)),
        )
        .await
    }

    /// Commit only while the originating setup attempt remains active.
    pub async fn install_checked(
        &self,
        config: AccountConfig,
        registration: Option<OAuthRegistration>,
        session_only: bool,
        active: Arc<AtomicBool>,
    ) -> Result<StoredAccount> {
        self.install_inner(config, registration, session_only, active, false)
            .await
    }

    async fn install_inner(
        &self,
        config: AccountConfig,
        registration: Option<OAuthRegistration>,
        session_only: bool,
        active: Arc<AtomicBool>,
        restoring: bool,
    ) -> Result<StoredAccount> {
        let persisted = self.store.get_account(&config.id.0)?;
        let reference =
            (!session_only).then(|| oauth2::CsrfToken::new_random().secret().to_owned());
        let mut record = config.metadata(registration, reference.clone());
        if let Some(previous) = &persisted {
            record.retired_credential_refs = previous.retired_credential_refs.clone();
            if let Some(key) = previous.auth.credential_ref() {
                if !record.retired_credential_refs.iter().any(|old| old == key) {
                    record.retired_credential_refs.push(key.to_owned());
                }
            }
        }
        let entry = Arc::new(Entry {
            live: AtomicBool::new(true),
            state: AsyncMutex::new(State {
                record: record.clone(),
                auth: Some(config.auth.clone()),
                revoked: false,
                dirty: false,
                retry_after: 0,
            }),
        });
        let old = {
            let mut retired = self.retired.lock().unwrap();
            if restoring && retired.contains(&record.id.0) {
                return Err(Error::Cancelled);
            }
            if !restoring {
                retired.remove(&record.id.0);
            }
            self.accounts
                .lock()
                .unwrap()
                .insert(record.id.0.clone(), entry.clone())
        };
        if let Some(old) = &old {
            old.live.store(false, Ordering::SeqCst);
        }
        let install_guard = entry.state.lock().await;
        let result = async {
            if let Some(key) = &reference {
                self.backend.save(key, &config.auth).await?;
            }
            if !entry.live.load(Ordering::SeqCst) || !active.load(Ordering::SeqCst) {
                return Err(Error::Cancelled);
            }
            self.store.save_account_record(&record)
        }
        .await;
        drop(install_guard);
        if let Err(error) = result {
            entry.live.store(false, Ordering::SeqCst);
            {
                let mut accounts = self.accounts.lock().unwrap();
                if accounts
                    .get(&record.id.0)
                    .is_some_and(|current| Arc::ptr_eq(current, &entry))
                {
                    accounts.remove(&record.id.0);
                    if let Some(old) = &old {
                        old.live.store(true, Ordering::SeqCst);
                        accounts.insert(record.id.0.clone(), old.clone());
                    }
                }
            }
            if let Some(key) = reference {
                let _ = self.backend.delete(&key).await;
            }
            return Err(error);
        }
        if let Some(old) = old {
            let previous = old.state.lock().await;
            if !session_only {
                if let Some(key) = previous.record.auth.credential_ref() {
                    let _ = self.backend.delete(key).await;
                }
            }
        }
        if !session_only {
            for key in &record.retired_credential_refs {
                let _ = self.backend.delete(key).await;
            }
        }
        Ok(record)
    }

    /// Restore metadata or safely migrate a legacy password row.
    pub async fn restore(&self, record: StoredAccount) -> Result<()> {
        if self.retired.lock().unwrap().contains(&record.id.0) {
            return Err(Error::Cancelled);
        }
        if let Some(legacy) = self.store.legacy_account(&record.id.0)? {
            if matches!(legacy.auth, AuthMethod::Password { .. }) {
                self.install_inner(legacy, None, false, Arc::new(AtomicBool::new(true)), true)
                    .await?;
                return Ok(());
            }
            // Old OAuth rows have no reliable issuing registration. Preserve the
            // legacy row until explicit reconnect, but never use its tokens.
            return Err(Error::ReconnectRequired);
        }
        let entry = Arc::new(Entry {
            live: AtomicBool::new(true),
            state: AsyncMutex::new(State {
                record: record.clone(),
                auth: None,
                revoked: false,
                dirty: false,
                retry_after: 0,
            }),
        });
        {
            let retired = self.retired.lock().unwrap();
            if retired.contains(&record.id.0) {
                return Err(Error::Cancelled);
            }
            self.accounts
                .lock()
                .unwrap()
                .insert(record.id.0.clone(), entry);
        }
        self.current(&record.id.0).await.map(|_| ())
    }

    /// Retry a locked keyring without losing a rotated token already in memory.
    pub async fn resume(&self, record: StoredAccount) -> Result<()> {
        if self.accounts.lock().unwrap().contains_key(&record.id.0) {
            self.current(&record.id.0).await.map(|_| ())
        } else {
            self.restore(record).await
        }
    }

    /// Whether a worker may still attach to this account generation.
    pub(crate) fn is_active(&self, id: &str) -> bool {
        self.accounts
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|entry| entry.live.load(Ordering::SeqCst))
    }

    /// Resolve current credentials, refreshing once per account before expiry.
    pub async fn current(&self, id: &str) -> Result<AuthMethod> {
        let entry = self.entry(id)?;
        self.current_entry(entry).await
    }

    /// Resolve credentials and endpoints from the same account generation.
    pub async fn configuration(&self, id: &str) -> Result<CredentialLease> {
        let entry = self.entry(id)?;
        self.configuration_entry(entry).await
    }

    async fn configuration_entry(&self, entry: Arc<Entry>) -> Result<CredentialLease> {
        let auth = self.current_entry(entry.clone()).await?;
        let state = entry.state.lock().await;
        if !entry.live.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        let config = state.record.with_auth(auth);
        drop(state);
        Ok(CredentialLease { config, entry })
    }

    async fn current_entry(&self, entry: Arc<Entry>) -> Result<AuthMethod> {
        let mut state = entry.state.lock().await;
        if !entry.live.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        if state.revoked {
            return Err(Error::ReconnectRequired);
        }
        if state.retry_after > chrono::Utc::now().timestamp() {
            return Err(Error::Network(
                "Token renewal is temporarily unavailable.".into(),
            ));
        }
        if state.auth.is_none() {
            let key = state
                .record
                .auth
                .credential_ref()
                .ok_or(Error::ReconnectRequired)?;
            state.auth = self.backend.load(key).await?;
        }
        let mut auth = state.auth.clone().ok_or(Error::ReconnectRequired)?;
        if state.dirty {
            if let Some(key) = state.record.auth.credential_ref() {
                self.backend.save(key, &auth).await?;
            }
            state.dirty = false;
        }
        if let AuthMethod::OAuth2 {
            provider,
            access_token,
            refresh_token,
            expires_at,
        } = &mut auth
        {
            if expires_at.is_none_or(|deadline| deadline <= chrono::Utc::now().timestamp() + 60) {
                let registration = match &state.record.auth {
                    StoredAuth::OAuth2 {
                        registration: Some(r),
                        ..
                    } => r,
                    _ => {
                        state.revoked = true;
                        return Err(Error::ReconnectRequired);
                    }
                };
                let Some(refresh) = refresh_token.as_deref() else {
                    state.revoked = true;
                    return Err(Error::ReconnectRequired);
                };
                let renewed = crate::oauth::refresh(
                    *provider,
                    registration,
                    refresh,
                    self.refresh_url.as_deref(),
                )
                .await;
                let renewed = match renewed {
                    Err(Error::ReconnectRequired) => {
                        state.revoked = true;
                        return Err(Error::ReconnectRequired);
                    }
                    Err(error) => {
                        state.retry_after = chrono::Utc::now().timestamp() + 30;
                        return Err(error);
                    }
                    Ok(tokens) => tokens,
                };
                *access_token = renewed.access_token;
                if renewed.refresh_token.is_some() {
                    *refresh_token = renewed.refresh_token;
                }
                *expires_at = renewed.expires_at;
                if !entry.live.load(Ordering::SeqCst) {
                    return Err(Error::Cancelled);
                }
                // Keep a rotated refresh token in memory even if its keyring
                // write fails; never redeem an obsolete refresh token again.
                state.auth = Some(auth.clone());
                state.dirty = true;
                if let Some(key) = state.record.auth.credential_ref() {
                    self.backend.save(key, &auth).await?;
                }
                state.dirty = false;
            }
        }
        if !entry.live.load(Ordering::SeqCst) {
            return Err(Error::Cancelled);
        }
        Ok(auth)
    }

    /// Synchronously invalidate ongoing requests before asynchronous cleanup.
    pub fn invalidate(&self, id: &str) {
        self.retired.lock().unwrap().insert(id.into());
        if let Some(entry) = self.accounts.lock().unwrap().get(id) {
            entry.live.store(false, Ordering::SeqCst);
        }
    }

    /// Remove credentials, then metadata. Failed keyring deletion stays retryable.
    pub async fn remove(&self, id: &str) -> Result<()> {
        self.invalidate(id);
        let persisted = self.store.get_account(id)?;
        let entry = self.accounts.lock().unwrap().remove(id);
        let record = if let Some(entry) = &entry {
            Some(entry.state.lock().await.record.clone())
        } else {
            self.store.get_account(id)?
        };
        let mut keys = Vec::new();
        for r in [persisted.as_ref(), record.as_ref()].into_iter().flatten() {
            if let Some(key) = r.auth.credential_ref() {
                if !keys.contains(&key.to_owned()) {
                    keys.push(key.to_owned());
                }
            }
            for key in &r.retired_credential_refs {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
        }
        for key in keys {
            if let Err(error) = self.backend.delete(&key).await {
                let retired = self.retired.lock().unwrap();
                if retired.contains(id) {
                    if let Some(entry) = entry {
                        self.accounts
                            .lock()
                            .unwrap()
                            .entry(id.into())
                            .or_insert(entry);
                    }
                }
                return Err(error);
            }
        }
        let retired = self.retired.lock().unwrap();
        if !retired.contains(id) {
            return Err(Error::Cancelled);
        }
        self.store.delete_account(id)
    }

    /// Start the channel used by blocking IMAP threads to resolve credentials.
    pub fn handle(self: &Arc<Self>, id: &str) -> Result<CredentialHandle> {
        let entry = self.entry(id)?;
        let (tx, mut rx) = mpsc::unbounded_channel::<CredentialRequest>();
        let manager = self.clone();
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                let manager = manager.clone();
                tokio::spawn(async move {
                    let result = if request.reject {
                        let mut state = request.entry.state.lock().await;
                        if request.entry.live.load(Ordering::SeqCst) {
                            state.revoked = true;
                            Err(Error::ReconnectRequired)
                        } else {
                            Err(Error::Cancelled)
                        }
                    } else {
                        manager
                            .configuration_entry(request.entry)
                            .await
                            .map(|lease| lease.config)
                    };
                    let _ = request.reply.send(result);
                });
            }
        });
        Ok(CredentialHandle {
            tx,
            entry,
            account: id.to_owned(),
        })
    }
}

struct CredentialRequest {
    entry: Arc<Entry>,
    reject: bool,
    reply: oneshot::Sender<Result<AccountConfig>>,
}

/// Blocking-worker interface; only call from dedicated OS threads.
#[derive(Clone)]
pub struct CredentialHandle {
    tx: mpsc::UnboundedSender<CredentialRequest>,
    entry: Arc<Entry>,
    account: String,
}
impl CredentialHandle {
    /// Ask the async credential manager for the latest credentials.
    pub fn current(&self, id: &str) -> Result<AccountConfig> {
        if id != self.account {
            return Err(Error::Cancelled);
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(CredentialRequest {
                entry: self.entry.clone(),
                reject: false,
                reply: tx,
            })
            .map_err(|_| Error::Cancelled)?;
        rx.blocking_recv().map_err(|_| Error::Cancelled)?
    }

    /// Stop further authentication retries after a server rejects these secrets.
    pub fn reject(&self, id: &str) -> Result<()> {
        if id != self.account {
            return Err(Error::Cancelled);
        }
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(CredentialRequest {
                entry: self.entry.clone(),
                reject: true,
                reply: tx,
            })
            .is_ok()
        {
            return match rx.blocking_recv() {
                Ok(Err(Error::ReconnectRequired)) => Ok(()),
                _ => Err(Error::Cancelled),
            };
        }
        Err(Error::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountId, ImapConfig, SmtpConfig, TlsMode};
    fn password() -> AccountConfig {
        AccountConfig {
            id: AccountId("alice@example.test".into()),
            email: "alice@example.test".into(),
            imap: ImapConfig {
                host: "imap.example.test".into(),
                port: 993,
                tls: TlsMode::Tls,
            },
            smtp: SmtpConfig {
                host: "smtp.example.test".into(),
                port: 465,
                tls: TlsMode::Tls,
            },
            auth: AuthMethod::Password {
                username: "alice".into(),
                smtp_username: "outgoing".into(),
                password: "private-password".into(),
            },
        }
    }

    #[tokio::test]
    #[ignore = "requires scripts/test-keyring.sh and its isolated session bus/keyring"]
    async fn linux_secret_service_roundtrip() {
        assert_eq!(
            std::env::var("BANG_ISOLATED_KEYRING_TEST").as_deref(),
            Ok("1")
        );
        let key = oauth2::CsrfToken::new_random().secret().to_owned();
        let backend = LinuxSecretStore;
        backend.save(&key, &password().auth).await.unwrap();
        assert!(
            matches!(backend.load(&key).await.unwrap(), Some(AuthMethod::Password { password, .. }) if password == "private-password")
        );
        backend.delete(&key).await.unwrap();
        assert!(backend.load(&key).await.unwrap().is_none());
    }

    fn oauth() -> (AccountConfig, OAuthRegistration) {
        let mut account = password();
        account.auth = AuthMethod::OAuth2 {
            provider: crate::OAuthProvider::Google,
            access_token: "expired-access".into(),
            refresh_token: Some("original-refresh".into()),
            expires_at: Some(1),
        };
        (
            account,
            OAuthRegistration {
                client_id: "issuing-client".into(),
                desktop_parameter: Some("desktop-parameter".into()),
            },
        )
    }

    async fn token_server(
        body: &'static str,
        status: u16,
    ) -> (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut read = BufReader::new(read);
                let mut size = 0;
                loop {
                    let mut line = String::new();
                    read.read_line(&mut line).await.unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(n) = line.to_lowercase().strip_prefix("content-length:") {
                        size = n.trim().parse().unwrap();
                    }
                }
                let mut form = vec![0; size];
                read.read_exact(&mut form).await.unwrap();
                let params: HashMap<_, _> =
                    url::form_urlencoded::parse(&form).into_owned().collect();
                assert_eq!(params.get("client_id").unwrap(), "issuing-client");
                assert_eq!(params.get("grant_type").unwrap(), "refresh_token");
                count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = write.write_all(response.as_bytes()).await;
            }
        });
        (url, requests, server)
    }

    #[tokio::test]
    async fn concurrent_refresh_rotates_once_and_survives_restart() {
        let (url, count, server) = token_server(r#"{"access_token":"fresh-access","refresh_token":"rotated-refresh","expires_in":3600}"#, 200).await;
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let backend = Arc::new(MemorySecretStore::default());
        let mut manager = CredentialManager::new(store.clone(), backend.clone());
        manager.refresh_url = Some(url);
        let (account, registration) = oauth();
        let record = manager
            .install(account, Some(registration), false)
            .await
            .unwrap();
        let (imap, smtp) =
            tokio::join!(manager.current(&record.id.0), manager.current(&record.id.0));
        for auth in [imap.unwrap(), smtp.unwrap()] {
            assert!(
                matches!(auth, AuthMethod::OAuth2 { access_token, refresh_token: Some(refresh), .. } if access_token == "fresh-access" && refresh == "rotated-refresh")
            );
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let restarted = CredentialManager::new(store.clone(), backend);
        restarted.restore(record).await.unwrap();
        assert!(restarted.current("alice@example.test").await.is_ok());
        assert!(!serde_json::to_string(&store.get_accounts().unwrap())
            .unwrap()
            .contains("fresh-access"));
        server.abort();
    }

    #[tokio::test]
    async fn refresh_preserves_omitted_token_and_revocation_stops_retrying() {
        let (url, _, server) =
            token_server(r#"{"access_token":"fresh","expires_in":3600}"#, 200).await;
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let mut manager =
            CredentialManager::new(store.clone(), Arc::new(MemorySecretStore::default()));
        manager.refresh_url = Some(url);
        let (account, registration) = oauth();
        manager
            .install(account, Some(registration), true)
            .await
            .unwrap();
        assert!(
            matches!(manager.current("alice@example.test").await.unwrap(), AuthMethod::OAuth2 { refresh_token: Some(token), .. } if token == "original-refresh")
        );
        server.abort();
        let (url, count, server) = token_server(r#"{"error":"invalid_grant"}"#, 400).await;
        manager.refresh_url = Some(url);
        let (account, registration) = oauth();
        manager
            .install(account, Some(registration), true)
            .await
            .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                manager.current("alice@example.test").await,
                Err(Error::ReconnectRequired)
            ));
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn temporary_refresh_failure_retains_grant_and_backs_off() {
        let (url, count, server) =
            token_server(r#"{"error":"temporarily_unavailable"}"#, 503).await;
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let mut manager = CredentialManager::new(store, Arc::new(MemorySecretStore::default()));
        manager.refresh_url = Some(url);
        let (account, registration) = oauth();
        manager
            .install(account, Some(registration), true)
            .await
            .unwrap();
        for _ in 0..2 {
            assert!(matches!(
                manager.current("alice@example.test").await,
                Err(Error::Network(_))
            ));
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let entry = manager.entry("alice@example.test").unwrap();
        let mut state = entry.state.lock().await;
        assert!(!state.revoked);
        state.retry_after = 0;
        assert!(
            matches!(&state.auth, Some(AuthMethod::OAuth2 { refresh_token: Some(token), .. }) if token == "original-refresh")
        );
        drop(state);
        server.abort();
    }

    #[tokio::test]
    async fn removal_during_refresh_cannot_resurrect_credentials_or_account() {
        let (url, count, server) = token_server(
            r#"{"access_token":"fresh","refresh_token":"new","expires_in":3600}"#,
            200,
        )
        .await;
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let backend = Arc::new(MemorySecretStore::default());
        let mut manager = CredentialManager::new(store.clone(), backend.clone());
        manager.refresh_url = Some(url);
        let (account, registration) = oauth();
        manager
            .install(account, Some(registration), false)
            .await
            .unwrap();
        let manager = Arc::new(manager);
        let refreshing = manager.clone();
        let task = tokio::spawn(async move { refreshing.current("alice@example.test").await });
        while count.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        manager.remove("alice@example.test").await.unwrap();
        assert!(matches!(task.await.unwrap(), Err(Error::Cancelled)));
        assert!(store.get_accounts().unwrap().is_empty());
        assert!(backend.0.lock().unwrap().is_empty());
        server.abort();
    }

    struct FlakyStore {
        memory: MemorySecretStore,
        fail: AtomicBool,
    }
    impl CredentialStore for FlakyStore {
        fn load<'a>(&'a self, key: &'a str) -> StorageFuture<'a, Option<AuthMethod>> {
            self.memory.load(key)
        }
        fn save<'a>(&'a self, key: &'a str, auth: &'a AuthMethod) -> StorageFuture<'a, ()> {
            Box::pin(async move {
                if self.fail.load(Ordering::SeqCst) {
                    Err(Error::SecureStorage)
                } else {
                    self.memory.save(key, auth).await
                }
            })
        }
        fn delete<'a>(&'a self, key: &'a str) -> StorageFuture<'a, ()> {
            self.memory.delete(key)
        }
    }

    #[tokio::test]
    async fn missing_keyring_has_no_plaintext_fallback_and_session_choice_works() {
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let backend = Arc::new(FlakyStore {
            memory: MemorySecretStore::default(),
            fail: AtomicBool::new(true),
        });
        let manager = CredentialManager::new(store.clone(), backend);
        assert!(matches!(
            manager.install(password(), None, false).await,
            Err(Error::SecureStorage)
        ));
        assert!(store.get_accounts().unwrap().is_empty());
        assert!(manager.install(password(), None, true).await.is_ok());
        assert!(manager.current("alice@example.test").await.is_ok());
    }

    #[tokio::test]
    async fn rotated_token_is_retained_if_keyring_write_temporarily_fails() {
        let (url, count, server) = token_server(
            r#"{"access_token":"fresh","refresh_token":"rotated","expires_in":3600}"#,
            200,
        )
        .await;
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let backend = Arc::new(FlakyStore {
            memory: MemorySecretStore::default(),
            fail: AtomicBool::new(false),
        });
        let mut manager = CredentialManager::new(store, backend.clone());
        manager.refresh_url = Some(url);
        let (account, registration) = oauth();
        manager
            .install(account, Some(registration), false)
            .await
            .unwrap();
        backend.fail.store(true, Ordering::SeqCst);
        assert!(matches!(
            manager.current("alice@example.test").await,
            Err(Error::SecureStorage)
        ));
        backend.fail.store(false, Ordering::SeqCst);
        assert!(
            matches!(manager.current("alice@example.test").await.unwrap(), AuthMethod::OAuth2 { refresh_token: Some(token), .. } if token == "rotated")
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn cancelled_setup_does_not_persist_or_replace_an_account() {
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let backend = Arc::new(MemorySecretStore::default());
        let manager = CredentialManager::new(store.clone(), backend.clone());
        assert!(matches!(
            manager
                .install_checked(password(), None, false, Arc::new(AtomicBool::new(false)))
                .await,
            Err(Error::Cancelled)
        ));
        assert!(store.get_accounts().unwrap().is_empty());
        assert!(backend.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn reconnect_never_gives_old_workers_new_secrets_or_rejects_the_new_generation() {
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let manager = Arc::new(CredentialManager::new(
            store,
            Arc::new(MemorySecretStore::default()),
        ));
        let original = password();
        let id = original.id.0.clone();
        manager.install(original.clone(), None, true).await.unwrap();
        let old_worker = manager.handle(&id).unwrap();
        let old_send = manager.configuration(&id).await.unwrap();
        let mut changed = original;
        changed.imap.host = "new-imap.example.test".into();
        changed.smtp.host = "new-smtp.example.test".into();
        changed.auth = AuthMethod::Password {
            username: "new-user".into(),
            smtp_username: "new-smtp-user".into(),
            password: "new-password".into(),
        };
        manager.install(changed, None, true).await.unwrap();
        let worker_id = id.clone();
        tokio::task::spawn_blocking(move || {
            assert!(matches!(
                old_worker.current(&worker_id),
                Err(Error::Cancelled)
            ));
            assert!(matches!(
                old_worker.reject(&worker_id),
                Err(Error::Cancelled)
            ));
        })
        .await
        .unwrap();
        assert!(matches!(old_send.reject().await, Error::Cancelled));
        let new_send = manager.configuration(&id).await.unwrap();
        assert_eq!(new_send.config.smtp.host, "new-smtp.example.test");
        assert!(
            matches!(&new_send.config.auth, AuthMethod::Password { password, .. } if password == "new-password")
        );
        assert!(manager.current(&id).await.is_ok());
    }
    #[tokio::test]
    async fn persisted_metadata_has_no_password_and_session_only_needs_login_after_restart() {
        let store = Arc::new(Store::open(std::path::Path::new(":memory:")).unwrap());
        let backend = Arc::new(MemorySecretStore::default());
        let manager = CredentialManager::new(store.clone(), backend.clone());
        let record = manager.install(password(), None, false).await.unwrap();
        let serialized = serde_json::to_string(&store.get_accounts().unwrap()).unwrap();
        assert!(!serialized.contains("private-password"));
        assert!(store.legacy_account(&record.id.0).unwrap().is_none());
        let restarted = CredentialManager::new(store.clone(), backend.clone());
        restarted.restore(record).await.unwrap();
        assert!(
            matches!(restarted.current("alice@example.test").await.unwrap(), AuthMethod::Password { password, .. } if password == "private-password")
        );
        let session = manager.install(password(), None, true).await.unwrap();
        assert!(manager.current(&session.id.0).await.is_ok());
        let restarted = CredentialManager::new(store.clone(), backend);
        assert!(matches!(
            restarted.restore(session).await,
            Err(Error::ReconnectRequired)
        ));
        manager.remove("alice@example.test").await.unwrap();
        assert!(matches!(
            manager.current("alice@example.test").await,
            Err(Error::ReconnectRequired)
        ));
        assert!(store.get_accounts().unwrap().is_empty());
    }
}
