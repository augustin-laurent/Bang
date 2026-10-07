//! SQLite-backed store for accounts, folders, messages, and attachments.
//!
//! The store is a single connection guarded by a mutex. SQLite operations are
//! fast and local, so a connection pool is not needed at this scale.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::account::AccountConfig;
use crate::error::{Error, Result};
use crate::message::{
    Attachment, EmailAddress, Folder, MessageFlags, MessageFull, MessageSummary,
};

/// Current schema version.
const SCHEMA_VERSION: i64 = 1;

/// V1 schema: initial tables.
const MIGRATION_V1: &str = r#"
CREATE TABLE accounts (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    config TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE folders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    unread INTEGER NOT NULL DEFAULT 0,
    last_uid INTEGER NOT NULL DEFAULT 0,
    uidvalidity INTEGER,
    UNIQUE(account_id, path)
);

CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    message_id TEXT NOT NULL DEFAULT '',
    from_name TEXT NOT NULL DEFAULT '',
    from_email TEXT NOT NULL DEFAULT '',
    "to" TEXT NOT NULL DEFAULT '[]',
    cc TEXT NOT NULL DEFAULT '[]',
    subject TEXT NOT NULL DEFAULT '',
    date INTEGER NOT NULL DEFAULT 0,
    flags INTEGER NOT NULL DEFAULT 0,
    preview TEXT NOT NULL DEFAULT '',
    has_attachments INTEGER NOT NULL DEFAULT 0,
    body_text TEXT,
    body_html TEXT,
    UNIQUE(account_id, folder_id, uid)
);

CREATE TABLE attachments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL DEFAULT 0,
    path TEXT NOT NULL DEFAULT ''
);

CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject, from_name, from_email, preview, body_text,
    content='messages', content_rowid='id'
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, subject, from_name, from_email, preview, body_text)
    VALUES (new.id, new.subject, new.from_name, new.from_email, new.preview, coalesce(new.body_text, ''));
END;

CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, from_name, from_email, preview, body_text)
    VALUES ('delete', old.id, old.subject, old.from_name, old.from_email, old.preview, coalesce(old.body_text, ''));
END;

CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, from_name, from_email, preview, body_text)
    VALUES ('delete', old.id, old.subject, old.from_name, old.from_email, old.preview, coalesce(old.body_text, ''));
    INSERT INTO messages_fts(rowid, subject, from_name, from_email, preview, body_text)
    VALUES (new.id, new.subject, new.from_name, new.from_email, new.preview, coalesce(new.body_text, ''));
END;
"#;

/// Persistent store for all local mail data.
///
/// All methods take `&self`; the connection is guarded by an internal mutex,
/// so a single `Store` can be shared across threads.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (or create) the store at the given path.
    ///
    /// Enables WAL mode and runs any pending migrations.
    ///
    /// # Errors
    /// Returns `Error::Store` if the database cannot be opened or migrated.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Store(format!("create data dir: {e}")))?;
        }
        let conn = Connection::open(path).map_err(|e| Error::Store(e.to_string()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Store(e.to_string()))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(|e| Error::Store(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    ///
    /// # Errors
    /// Returns `Error::Store` on failure.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| Error::Store(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap_or(0);
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        conn.execute_batch(MIGRATION_V1)
            .map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| Error::Store(format!("store poisoned: {e}")))
    }

    // ------------------------------------------------------------------
    // Accounts
    // ------------------------------------------------------------------

    /// Insert or update an account.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn save_account(&self, config: &AccountConfig) -> Result<()> {
        let conn = self.lock()?;
        let json = serde_json::to_string(config).map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT INTO accounts (id, email, config, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET email = excluded.email, config = excluded.config",
            params![
                config.id.0,
                config.email,
                json,
                chrono::Utc::now().timestamp()
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// List all accounts.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_accounts(&self) -> Result<Vec<AccountConfig>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT config FROM accounts ORDER BY created_at")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row.map_err(|e| Error::Store(e.to_string()))?;
            let config: AccountConfig =
                serde_json::from_str(&json).map_err(|e| Error::Store(e.to_string()))?;
            out.push(config);
        }
        Ok(out)
    }

    /// Get a single account by id.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_account(&self, id: &str) -> Result<Option<AccountConfig>> {
        let conn = self.lock()?;
        let json: Option<String> = conn
            .query_row("SELECT config FROM accounts WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| Error::Store(e.to_string()))?;
        match json {
            Some(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| Error::Store(e.to_string())),
            None => Ok(None),
        }
    }

    /// Delete an account and all its data (folders, messages, attachments).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn delete_account(&self, id: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Folders
    // ------------------------------------------------------------------

    /// Insert or update a folder (matched on account + IMAP path).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn upsert_folder(&self, folder: &Folder) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO folders (account_id, path, name, unread)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, path) DO UPDATE SET name = excluded.name",
            params![folder.account_id, folder.path, folder.name, folder.unread],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// Delete folders of an account whose path is not in the given list.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn delete_folders_not_in(&self, account_id: &str, paths: &[String]) -> Result<()> {
        let conn = self.lock()?;
        if paths.is_empty() {
            conn.execute(
                "DELETE FROM folders WHERE account_id = ?1",
                params![account_id],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
            return Ok(());
        }
        let mut sql = String::from("DELETE FROM folders WHERE account_id = ?1 AND path NOT IN (");
        for (i, _) in paths.iter().enumerate() {
            sql.push_str(&format!("?{}", i + 2));
            if i + 1 < paths.len() {
                sql.push(',');
            }
        }
        sql.push(')');
        let mut stmt = conn.prepare(&sql).map_err(|e| Error::Store(e.to_string()))?;
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(account_id.to_string())];
        for p in paths {
            params_vec.push(Box::new(p.clone()));
        }
        let refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        stmt.execute(refs.as_slice()).map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// List folders of an account, ordered by name.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_folders(&self, account_id: &str) -> Result<Vec<Folder>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id, account_id, name, path, unread FROM folders WHERE account_id = ?1 ORDER BY name")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok(Folder {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    name: r.get(2)?,
                    path: r.get(3)?,
                    unread: r.get(4)?,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// Update the unread count of a folder.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn set_folder_unread(&self, folder_id: i64, unread: u32) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE folders SET unread = ?2 WHERE id = ?1",
            params![folder_id, unread],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// Persist per-folder sync state (last seen UID and UIDVALIDITY).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn set_folder_sync_state(
        &self,
        folder_id: i64,
        last_uid: u32,
        uidvalidity: Option<u32>,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE folders SET last_uid = ?2, uidvalidity = ?3 WHERE id = ?1",
            params![folder_id, last_uid, uidvalidity],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(())
    }

    /// Read per-folder sync state.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_folder_sync_state(&self, folder_id: i64) -> Result<Option<(u32, Option<u32>)>> {
        let conn = self.lock()?;
        let row: Option<(i64, Option<i64>)> = conn
            .query_row(
                "SELECT last_uid, uidvalidity FROM folders WHERE id = ?1",
                params![folder_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(row.map(|(l, u)| (l as u32, u.map(|v| v as u32))))
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    /// Insert or update a message summary (matched on account + folder + UID).
    ///
    /// # Returns
    /// The local store id of the message.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn upsert_message(&self, msg: &MessageSummary) -> Result<i64> {
        let conn = self.lock()?;
        let to = serde_json::to_string(&msg.to).map_err(|e| Error::Store(e.to_string()))?;
        let cc = serde_json::to_string(&msg.cc).map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT INTO messages
                (account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                 subject, date, flags, preview, has_attachments)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(account_id, folder_id, uid) DO UPDATE SET
                message_id = excluded.message_id,
                from_name = excluded.from_name,
                from_email = excluded.from_email,
                \"to\" = excluded.\"to\",
                cc = excluded.cc,
                subject = excluded.subject,
                date = excluded.date,
                flags = excluded.flags,
                preview = excluded.preview,
                has_attachments = excluded.has_attachments",
            params![
                msg.account_id,
                msg.folder_id,
                msg.uid,
                msg.message_id,
                msg.from.name,
                msg.from.address,
                to,
                cc,
                msg.subject,
                msg.date,
                msg.flags.bits(),
                msg.preview,
                msg.has_attachments as i64
            ],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        let id: i64 = conn
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND folder_id = ?2 AND uid = ?3",
                params![msg.account_id, msg.folder_id, msg.uid],
                |r| r.get(0),
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(id)
    }

    /// Get a message summary by account, folder, and IMAP UID.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_message_by_uid(
        &self,
        account_id: &str,
        folder_id: i64,
        uid: u32,
    ) -> Result<Option<MessageSummary>> {
        let conn = self.lock()?;
        let row: Option<MessageSummary> = conn
            .query_row(
                "SELECT id, account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                        subject, date, flags, preview, has_attachments
                 FROM messages WHERE account_id = ?1 AND folder_id = ?2 AND uid = ?3",
                params![account_id, folder_id, uid],
                Self::row_to_summary,
            )
            .optional()
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(row)
    }

    /// Get a folder by its local store id.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_folder_by_id(&self, id: i64) -> Result<Option<Folder>> {
        let conn = self.lock()?;
        let row: Option<Folder> = conn
            .query_row(
                "SELECT id, account_id, name, path, unread FROM folders WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Folder {
                        id: r.get(0)?,
                        account_id: r.get(1)?,
                        name: r.get(2)?,
                        path: r.get(3)?,
                        unread: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(row)
    }

    /// List the most recent messages of a folder (newest first).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_messages(&self, folder_id: i64, limit: usize) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                        subject, date, flags, preview, has_attachments
                 FROM messages WHERE folder_id = ?1 ORDER BY date DESC, id DESC LIMIT ?2",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![folder_id, limit as i64], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// Get a single message summary by local id.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_message(&self, id: i64) -> Result<Option<MessageSummary>> {
        let conn = self.lock()?;
        let row: Option<MessageSummary> = conn
            .query_row(
                "SELECT id, account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                        subject, date, flags, preview, has_attachments
                 FROM messages WHERE id = ?1",
                params![id],
                Self::row_to_summary,
            )
            .optional()
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(row)
    }

    fn row_to_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageSummary> {
        let to_json: String = row.get(7)?;
        let cc_json: String = row.get(8)?;
        let to: Vec<EmailAddress> = serde_json::from_str(&to_json).unwrap_or_default();
        let cc: Vec<EmailAddress> = serde_json::from_str(&cc_json).unwrap_or_default();
        Ok(MessageSummary {
            id: row.get(0)?,
            account_id: row.get(1)?,
            folder_id: row.get(2)?,
            uid: row.get(3)?,
            message_id: row.get(4)?,
            from: EmailAddress {
                name: row.get(5)?,
                address: row.get(6)?,
            },
            to,
            cc,
            subject: row.get(9)?,
            date: row.get(10)?,
            flags: MessageFlags::from_bits_truncate(row.get::<_, i64>(11)? as u32),
            preview: row.get(12)?,
            has_attachments: row.get::<_, i64>(13)? != 0,
        })
    }

    /// Add or remove flags on messages.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn set_message_flags(&self, ids: &[i64], flags: MessageFlags, add: bool) -> Result<()> {
        let conn = self.lock()?;
        for id in ids {
            let current: i64 = conn
                .query_row("SELECT flags FROM messages WHERE id = ?1", params![id], |r| {
                    r.get(0)
                })
                .map_err(|e| Error::Store(e.to_string()))?;
            let mut current = MessageFlags::from_bits_truncate(current as u32);
            if add {
                current.insert(flags);
            } else {
                current.remove(flags);
            }
            conn.execute(
                "UPDATE messages SET flags = ?2 WHERE id = ?1",
                params![id, current.bits()],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    /// Save the full body of a message (text and HTML).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn save_message_bodies(
        &self,
        id: i64,
        body_text: &str,
        body_html: &str,
        attachments: &[Attachment],
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE messages SET body_text = ?2, body_html = ?3 WHERE id = ?1",
            params![id, body_text, body_html],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        conn.execute("DELETE FROM attachments WHERE message_id = ?1", params![id])
            .map_err(|e| Error::Store(e.to_string()))?;
        for a in attachments {
            conn.execute(
                "INSERT INTO attachments (message_id, filename, content_type, size, path)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![a.message_id, a.filename, a.content_type, a.size as i64, a.path],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    /// Get a fully loaded message (summary + bodies + attachments).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_message_full(&self, id: i64) -> Result<Option<MessageFull>> {
        // Fetch the summary before locking: calling `self.get_message` while
        // holding the mutex would deadlock (the mutex is not reentrant).
        let summary = match self.get_message(id)? {
            Some(s) => s,
            None => return Ok(None),
        };
        let conn = self.lock()?;
        let Some((body_text, body_html)): Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT body_text, body_html FROM messages WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| Error::Store(e.to_string()))?
        else {
            // Deleted between the two reads.
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT id, message_id, filename, content_type, size, path
                 FROM attachments WHERE message_id = ?1",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let attachments = stmt
            .query_map(params![id], |r| {
                Ok(Attachment {
                    id: r.get(0)?,
                    message_id: r.get(1)?,
                    filename: r.get(2)?,
                    content_type: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    path: r.get(5)?,
                })
            })
            .map_err(|e| Error::Store(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(Some(MessageFull {
            body_text: body_text.unwrap_or_default(),
            body_html: body_html.unwrap_or_default(),
            attachments,
            summary,
        }))
    }

    // ------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------

    /// Full-text search over an account's messages.
    ///
    /// The query is an FTS5 MATCH expression (e.g. `"invoice" OR "tax"`).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn search(&self, account_id: &str, query: &str, limit: usize) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.account_id, m.folder_id, m.uid, m.message_id, m.from_name,
                        m.from_email, m.\"to\", m.cc, m.subject, m.date, m.flags, m.preview,
                        m.has_attachments
                 FROM messages_fts f
                 JOIN messages m ON m.id = f.rowid
                 WHERE m.account_id = ?1 AND messages_fts MATCH ?2
                 ORDER BY rank LIMIT ?3",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![account_id, query, limit as i64], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AccountId, AuthMethod, ImapConfig, SmtpConfig, TlsMode};

    fn test_account(id: &str) -> AccountConfig {
        AccountConfig {
            id: AccountId(id.into()),
            email: format!("{id}@example.com"),
            imap: ImapConfig {
                host: "imap.example.com".into(),
                port: 993,
                tls: TlsMode::Tls,
            },
            smtp: SmtpConfig {
                host: "smtp.example.com".into(),
                port: 465,
                tls: TlsMode::Tls,
            },
            auth: AuthMethod::Password {
                username: id.into(),
                password: "secret".into(),
            },
        }
    }

    fn test_folder(account: &str, path: &str) -> Folder {
        Folder {
            id: 0,
            account_id: account.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            path: path.into(),
            unread: 0,
        }
    }

    fn test_summary(account: &str, folder_id: i64, uid: u32, subject: &str) -> MessageSummary {
        MessageSummary {
            id: 0,
            account_id: account.into(),
            folder_id,
            uid,
            message_id: format!("<{uid}@example.com>"),
            from: EmailAddress {
                name: "Alice".into(),
                address: "alice@example.com".into(),
            },
            to: Vec::new(),
            cc: Vec::new(),
            subject: subject.into(),
            date: 1_700_000_000,
            flags: MessageFlags::empty(),
            preview: String::new(),
            has_attachments: false,
        }
    }

    #[test]
    fn account_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let account = test_account("alice");
        store.save_account(&account).unwrap();

        let accounts = store.get_accounts().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id.0, "alice");
        assert_eq!(accounts[0].email, "alice@example.com");

        let got = store.get_account("alice").unwrap().unwrap();
        assert_eq!(got.imap.host, "imap.example.com");

        store.delete_account("alice").unwrap();
        assert!(store.get_account("alice").unwrap().is_none());
    }

    #[test]
    fn folder_upsert_and_sync_state() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();

        let folders = store.get_folders("alice").unwrap();
        assert_eq!(folders.len(), 1);
        let id = folders[0].id;
        assert!(id > 0);
        assert_eq!(folders[0].path, "INBOX");

        // Sync state: (last uid, uid validity). A fresh folder starts at
        // (0, None) — the syncer treats last_uid == 0 as "full sync".
        assert_eq!(
            store.get_folder_sync_state(id).unwrap(),
            Some((0, None))
        );
        store
            .set_folder_sync_state(id, 42, Some(7))
            .unwrap();
        assert_eq!(
            store.get_folder_sync_state(id).unwrap(),
            Some((42, Some(7)))
        );

        store.set_folder_unread(id, 5).unwrap();
        let folders = store.get_folders("alice").unwrap();
        assert_eq!(folders[0].unread, 5);
    }

    #[test]
    fn message_upsert_flags_and_bodies() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();
        let folder_id = store.get_folders("alice").unwrap()[0].id;

        let msg = test_summary("alice", folder_id, 1, "Hello");
        let id = store.upsert_message(&msg).unwrap();
        assert!(id > 0);

        // Upserting the same (account, folder, uid) keeps the same id.
        let id2 = store.upsert_message(&msg).unwrap();
        assert_eq!(id, id2);

        let got = store.get_message_by_uid("alice", folder_id, 1).unwrap().unwrap();
        assert_eq!(got.subject, "Hello");
        assert_eq!(got.from.address, "alice@example.com");

        // Flags: add then remove.
        store
            .set_message_flags(&[id], MessageFlags::SEEN, true)
            .unwrap();
        let got = store.get_message(id).unwrap().unwrap();
        assert!(got.flags.contains(MessageFlags::SEEN));
        store
            .set_message_flags(&[id], MessageFlags::SEEN, false)
            .unwrap();
        let got = store.get_message(id).unwrap().unwrap();
        assert!(!got.flags.contains(MessageFlags::SEEN));

        // Bodies round-trip through get_message_full.
        store
            .save_message_bodies(id, "plain body", "<b>html</b>", &[])
            .unwrap();
        let full = store.get_message_full(id).unwrap().unwrap();
        assert_eq!(full.body_text, "plain body");
        assert_eq!(full.body_html, "<b>html</b>");
    }

    #[test]
    fn search_matches_subject_and_body() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();
        let folder_id = store.get_folders("alice").unwrap()[0].id;

        let msg = test_summary("alice", folder_id, 1, "Quarterly invoice");
        let id = store.upsert_message(&msg).unwrap();
        store
            .save_message_bodies(id, "The total is 42 euros.", "", &[])
            .unwrap();

        assert_eq!(store.search("alice", "invoice", 10).unwrap().len(), 1);
        assert_eq!(store.search("alice", "euros", 10).unwrap().len(), 1);
        assert_eq!(store.search("alice", "nonexistent", 10).unwrap().len(), 0);
    }
}

