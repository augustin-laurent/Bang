//! SQLite-backed store for accounts, folders, messages, and attachments.
//!
//! The store is a single connection guarded by a mutex. SQLite operations are
//! fast and local, so a connection pool is not needed at this scale.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::account::AccountConfig;
use crate::error::{Error, Result};
use crate::events::SendRequest;
use crate::message::{
    Attachment, EmailAddress, Folder, MessageFlags, MessageFull, MessageSummary, PendingSend,
};

/// Current schema version.
const SCHEMA_VERSION: i64 = 3;

/// Special folder id for the unified inbox (all accounts' INBOXes).
///
/// It is negative so it can never collide with a real store id.
pub const UNIFIED_INBOX_ID: i64 = -1;

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

/// V2 schema: snooze and follow-up scheduling.
///
/// Both columns are local (never synced from the server) and nullable:
/// `NULL` means "not snoozed" / "no follow-up scheduled".
const MIGRATION_V2: &str = r#"
ALTER TABLE messages ADD COLUMN snoozed_until INTEGER;
ALTER TABLE messages ADD COLUMN follow_up_at INTEGER;
"#;

/// V3 schema: send-later.
///
/// A pending send is a full `SendRequest` serialized as JSON, held until
/// `send_at` passes. It is removed once the SMTP send succeeds.
const MIGRATION_V3: &str = r#"
CREATE TABLE pending_sends (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    request TEXT NOT NULL,
    send_at INTEGER NOT NULL
);
"#;

/// Persistent store for all local mail data.
///
/// All methods take `&self`; the connection is guarded by an internal mutex,
/// so a single `Store` can be shared across threads.
pub struct Store {
    conn: Mutex<Connection>,
    /// Where attachment files live (`<dir>/<message_id>/<filename>`), for
    /// file-backed stores. `None` for in-memory stores.
    attachments_dir: Option<PathBuf>,
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
            attachments_dir: path.parent().map(|p| p.join("attachments")),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory store (for tests).
    ///
    /// # Errors
    /// Returns `Error::Store` on failure.
    #[cfg(any(test, feature = "integration"))]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(|e| Error::Store(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
            attachments_dir: None,
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
        if version < 1 {
            conn.execute_batch(MIGRATION_V1)
                .map_err(|e| Error::Store(e.to_string()))?;
        }
        if version < 2 {
            conn.execute_batch(MIGRATION_V2)
                .map_err(|e| Error::Store(e.to_string()))?;
        }
        if version < 3 {
            conn.execute_batch(MIGRATION_V3)
                .map_err(|e| Error::Store(e.to_string()))?;
        }
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
        // Remove the account's attachment files before the rows cascade.
        let mut stmt = conn
            .prepare("SELECT id FROM messages WHERE account_id = ?1")
            .map_err(|e| Error::Store(e.to_string()))?;
        let ids = stmt
            .query_map(params![id], |r| r.get::<_, i64>(0))
            .map_err(|e| Error::Store(e.to_string()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Store(e.to_string()))?;
        drop(stmt);
        conn.execute("DELETE FROM accounts WHERE id = ?1", params![id])
            .map_err(|e| Error::Store(e.to_string()))?;
        for id in ids {
            self.delete_attachment_files(id)?;
        }
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
        let now = chrono::Utc::now().timestamp();
        let mut stmt = conn
            .prepare(
                "SELECT id, account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                        subject, date, flags, preview, has_attachments
                 FROM messages
                 WHERE folder_id = ?1 AND (snoozed_until IS NULL OR snoozed_until <= ?2)
                 ORDER BY date DESC, id DESC LIMIT ?3",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![folder_id, now, limit as i64], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// List the most recent messages of every account's INBOX (newest first).
    ///
    /// The unified inbox is a smart folder: it is not stored, only queried.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_unified_inbox(&self, limit: usize) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let now = chrono::Utc::now().timestamp();
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.account_id, m.folder_id, m.uid, m.message_id, m.from_name,
                        m.from_email, m.\"to\", m.cc, m.subject, m.date, m.flags, m.preview,
                        m.has_attachments
                 FROM messages m
                 JOIN folders f ON f.id = m.folder_id
                 WHERE UPPER(f.path) = 'INBOX'
                   AND (m.snoozed_until IS NULL OR m.snoozed_until <= ?1)
                 ORDER BY m.date DESC, m.id DESC LIMIT ?2",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![now, limit as i64], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// Count unread messages across every account's INBOX.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn unified_inbox_unread(&self) -> Result<u32> {
        let conn = self.lock()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages m
                 JOIN folders f ON f.id = m.folder_id
                 WHERE UPPER(f.path) = 'INBOX' AND (m.flags & 1) = 0",
                [],
                |r| r.get(0),
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        Ok(count as u32)
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

    /// Delete messages from the local store (attachments are removed too).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn delete_messages(&self, ids: &[i64]) -> Result<()> {
        let conn = self.lock()?;
        for id in ids {
            conn.execute("DELETE FROM messages WHERE id = ?1", params![id])
                .map_err(|e| Error::Store(e.to_string()))?;
            self.delete_attachment_files(*id)?;
        }
        Ok(())
    }

    /// Snooze messages until the given Unix timestamp (seconds).
    ///
    /// Snoozed messages are hidden from folder listings until the time passes;
    /// they reappear on their own once `snoozed_until` is in the past.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn snooze_messages(&self, ids: &[i64], until: i64) -> Result<()> {
        let conn = self.lock()?;
        for id in ids {
            conn.execute(
                "UPDATE messages SET snoozed_until = ?2 WHERE id = ?1",
                params![id, until],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    /// Clear the snooze on messages so they reappear immediately.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn unsnooze_messages(&self, ids: &[i64]) -> Result<()> {
        let conn = self.lock()?;
        for id in ids {
            conn.execute(
                "UPDATE messages SET snoozed_until = NULL WHERE id = ?1",
                params![id],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    /// Schedule a follow-up reminder on messages at the given Unix timestamp.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn set_follow_up(&self, ids: &[i64], at: i64) -> Result<()> {
        let conn = self.lock()?;
        for id in ids {
            conn.execute(
                "UPDATE messages SET follow_up_at = ?2 WHERE id = ?1",
                params![id, at],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    /// Clear the follow-up reminder on messages.
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn clear_follow_up(&self, ids: &[i64]) -> Result<()> {
        let conn = self.lock()?;
        for id in ids {
            conn.execute(
                "UPDATE messages SET follow_up_at = NULL WHERE id = ?1",
                params![id],
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        }
        Ok(())
    }

    /// List messages whose follow-up reminder is due (`follow_up_at <= now`).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn due_follow_ups(&self, now: i64) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                        subject, date, flags, preview, has_attachments
                 FROM messages WHERE follow_up_at IS NOT NULL AND follow_up_at <= ?1",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![now], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// List all currently snoozed messages (hidden until their snooze passes).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn get_snoozed(&self) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let now = chrono::Utc::now().timestamp();
        let mut stmt = conn
            .prepare(
                "SELECT id, account_id, folder_id, uid, message_id, from_name, from_email, \"to\", cc,
                        subject, date, flags, preview, has_attachments
                 FROM messages WHERE snoozed_until IS NOT NULL AND snoozed_until > ?1
                 ORDER BY snoozed_until ASC",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![now], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }

    /// Schedule an email to be sent at the given Unix timestamp (seconds).
    ///
    /// The full request is stored as JSON and dispatched by the service
    /// once `send_at` passes.
    ///
    /// # Errors
    /// Returns `Error::Store` on database or serialization failure.
    pub fn schedule_send(&self, request: &SendRequest, send_at: i64) -> Result<i64> {
        let conn = self.lock()?;
        let json = serde_json::to_vec(request).map_err(|e| Error::Store(e.to_string()))?;
        conn.execute(
            "INSERT INTO pending_sends (account_id, request, send_at) VALUES (?1, ?2, ?3)",
            params![request.account.0, json, send_at],
        )
        .map_err(|e| Error::Store(e.to_string()))?;
        Ok(conn.last_insert_rowid())
    }

    /// List pending sends whose time has come (`send_at <= now`), oldest first.
    ///
    /// # Errors
    /// Returns `Error::Store` on database or deserialization failure.
    pub fn due_sends(&self, now: i64) -> Result<Vec<(i64, SendRequest)>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id, request FROM pending_sends WHERE send_at <= ?1 ORDER BY send_at")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![now], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, json) = row.map_err(|e| Error::Store(e.to_string()))?;
            let request = serde_json::from_slice(&json).map_err(|e| Error::Store(e.to_string()))?;
            out.push((id, request));
        }
        Ok(out)
    }

    /// List all pending sends, soonest first (for the UI).
    ///
    /// # Errors
    /// Returns `Error::Store` on database or deserialization failure.
    pub fn get_pending_sends(&self) -> Result<Vec<PendingSend>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id, account_id, request, send_at FROM pending_sends ORDER BY send_at")
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, account_id, json, send_at) = row.map_err(|e| Error::Store(e.to_string()))?;
            let request: SendRequest =
                serde_json::from_slice(&json).map_err(|e| Error::Store(e.to_string()))?;
            out.push(PendingSend {
                id,
                account_id,
                send_at,
                from: request.from,
                subject: request.subject,
            });
        }
        Ok(out)
    }

    /// Remove a pending send (after a successful send, or on cancel).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn remove_send(&self, id: i64) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM pending_sends WHERE id = ?1", params![id])
            .map_err(|e| Error::Store(e.to_string()))?;
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

    /// Save an attachment's data to disk and return the stored path.
    ///
    /// Layout: `<attachments_dir>/<message_id>/<filename>`. File names come
    /// from untrusted mail content, so only the final path component is kept
    /// and collisions get a numeric suffix.
    ///
    /// # Errors
    /// Returns `Error::Store` on I/O failure or when the store is in-memory.
    pub fn save_attachment(&self, message_id: i64, filename: &str, data: &[u8]) -> Result<String> {
        let dir = self
            .attachments_dir
            .as_ref()
            .ok_or_else(|| Error::Store("in-memory store: no attachment dir".into()))?;
        let msg_dir = dir.join(message_id.to_string());
        std::fs::create_dir_all(&msg_dir).map_err(|e| Error::Store(e.to_string()))?;
        let name = sanitize_filename(filename);
        let mut path = msg_dir.join(&name);
        let mut n = 1;
        while path.exists() {
            let (stem, ext) = split_name(&name);
            path = msg_dir.join(format!("{stem}-{n}{ext}"));
            n += 1;
        }
        std::fs::write(&path, data).map_err(|e| Error::Store(e.to_string()))?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Read an attachment's data from disk.
    ///
    /// The path must be inside this store's attachments directory.
    ///
    /// # Errors
    /// Returns `Error::Store` on I/O failure or if the path escapes the
    /// attachments directory.
    pub fn read_attachment(&self, path: &str) -> Result<Vec<u8>> {
        let dir = self
            .attachments_dir
            .as_ref()
            .ok_or_else(|| Error::Store("in-memory store: no attachment dir".into()))?;
        let p = Path::new(path);
        if !p.starts_with(dir) {
            return Err(Error::Store("path outside attachments dir".into()));
        }
        std::fs::read(p).map_err(|e| Error::Store(e.to_string()))
    }

    /// Delete all attachment files of a message.
    ///
    /// # Errors
    /// Returns `Error::Store` on I/O failure.
    pub fn delete_attachment_files(&self, message_id: i64) -> Result<()> {
        if let Some(dir) = &self.attachments_dir {
            let _ = std::fs::remove_dir_all(dir.join(message_id.to_string()));
        }
        Ok(())
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

    /// Full-text search over every account's messages.
    ///
    /// The query is an FTS5 MATCH expression (e.g. `"invoice" OR "tax"`).
    ///
    /// # Errors
    /// Returns `Error::Store` on database failure.
    pub fn search_all(&self, query: &str, limit: usize) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT m.id, m.account_id, m.folder_id, m.uid, m.message_id, m.from_name,
                        m.from_email, m.\"to\", m.cc, m.subject, m.date, m.flags, m.preview,
                        m.has_attachments
                 FROM messages_fts f
                 JOIN messages m ON m.id = f.rowid
                 WHERE messages_fts MATCH ?1
                 ORDER BY rank LIMIT ?2",
            )
            .map_err(|e| Error::Store(e.to_string()))?;
        let rows = stmt
            .query_map(params![query, limit as i64], Self::row_to_summary)
            .map_err(|e| Error::Store(e.to_string()))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Store(e.to_string()))?);
        }
        Ok(out)
    }
}

/// Reduce an untrusted attachment file name to a safe single component.
fn sanitize_filename(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment")
        .to_string();
    if base.is_empty() || base == "." || base == ".." {
        "attachment".into()
    } else {
        base
    }
}

/// Split a file name into (stem, extension including the dot).
fn split_name(name: &str) -> (String, String) {
    let p = Path::new(name);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    (
        stem.to_string(),
        if ext.is_empty() {
            String::new()
        } else {
            format!(".{ext}")
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{AccountId, AuthMethod, ImapConfig, SmtpConfig, TlsMode};
    use crate::events::{OutgoingAttachment, SendRequest};

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
    fn unified_inbox_spans_accounts_and_excludes_other_folders() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        store.save_account(&test_account("bob")).unwrap();

        let inbox_a = test_folder("alice", "INBOX");
        let inbox_b = test_folder("bob", "INBOX");
        let sent_a = test_folder("alice", "Sent");
        for f in [&inbox_a, &inbox_b, &sent_a] {
            store.upsert_folder(f).unwrap();
        }
        let folders = store.get_folders("alice").unwrap();
        let inbox_a_id = folders.iter().find(|f| f.path == "INBOX").unwrap().id;
        let sent_a_id = folders.iter().find(|f| f.path == "Sent").unwrap().id;
        let inbox_b_id = store.get_folders("bob").unwrap()[0].id;

        // Older message in alice's INBOX, newer in bob's INBOX, and one in Sent.
        let m1 = test_summary("alice", inbox_a_id, 1, "Alice inbox");
        let m2 = test_summary("bob", inbox_b_id, 1, "Bob inbox");
        let m3 = test_summary("alice", sent_a_id, 1, "Alice sent");
        let id1 = store.upsert_message(&m1).unwrap();
        let m2_newer = {
            let mut m = m2;
            m.date += 1000;
            m
        };
        let id2 = store.upsert_message(&m2_newer).unwrap();
        let id3 = store.upsert_message(&m3).unwrap();

        let unified = store.get_unified_inbox(10).unwrap();
        assert_eq!(unified.len(), 2);
        // Newest first: bob's message, then alice's. Sent is excluded.
        assert_eq!(unified[0].id, id2);
        assert_eq!(unified[1].id, id1);
        assert!(!unified.iter().any(|m| m.id == id3));

        // Both inbox messages are unread; marking one read drops the count.
        assert_eq!(store.unified_inbox_unread().unwrap(), 2);
        store
            .set_message_flags(&[id1], MessageFlags::SEEN, true)
            .unwrap();
        assert_eq!(store.unified_inbox_unread().unwrap(), 1);
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

        // A second account with a matching message: search_all spans both.
        store.save_account(&test_account("bob")).unwrap();
        let bob_folder = test_folder("bob", "INBOX");
        store.upsert_folder(&bob_folder).unwrap();
        let bob_folder_id = store.get_folders("bob").unwrap()[0].id;
        let bob_msg = test_summary("bob", bob_folder_id, 1, "Another invoice");
        store.upsert_message(&bob_msg).unwrap();

        assert_eq!(store.search("bob", "invoice", 10).unwrap().len(), 1);
        assert_eq!(store.search_all("invoice", 10).unwrap().len(), 2);
        assert_eq!(store.search_all("nonexistent", 10).unwrap().len(), 0);
    }

    #[test]
    fn snooze_hides_message_until_time_passes() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();
        let folder_id = store.get_folders("alice").unwrap()[0].id;

        let msg = test_summary("alice", folder_id, 1, "Snoozed");
        let id = store.upsert_message(&msg).unwrap();
        let now = chrono::Utc::now().timestamp();

        // Snoozed into the future: hidden from the folder listing and the
        // unified inbox, but listed in get_snoozed.
        store.snooze_messages(&[id], now + 3600).unwrap();
        assert!(store.get_messages(folder_id, 10).unwrap().is_empty());
        assert!(store.get_unified_inbox(10).unwrap().is_empty());
        let snoozed = store.get_snoozed().unwrap();
        assert_eq!(snoozed.len(), 1);
        assert_eq!(snoozed[0].id, id);

        // Unsnoozing brings it back immediately.
        store.unsnooze_messages(&[id]).unwrap();
        assert_eq!(store.get_messages(folder_id, 10).unwrap().len(), 1);
        assert!(store.get_snoozed().unwrap().is_empty());

        // A snooze in the past never hides the message.
        store.snooze_messages(&[id], now - 1).unwrap();
        assert_eq!(store.get_messages(folder_id, 10).unwrap().len(), 1);
    }

    #[test]
    fn snooze_and_follow_up_survive_reupsert() {
        // A sync re-upsert must not clear the local scheduling columns.
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();
        let folder_id = store.get_folders("alice").unwrap()[0].id;

        let msg = test_summary("alice", folder_id, 1, "Keep me");
        let id = store.upsert_message(&msg).unwrap();
        let now = chrono::Utc::now().timestamp();
        store.snooze_messages(&[id], now + 3600).unwrap();
        store.set_follow_up(&[id], now + 7200).unwrap();

        store.upsert_message(&msg).unwrap();

        assert!(store.get_messages(folder_id, 10).unwrap().is_empty());
        assert_eq!(store.due_follow_ups(now + 7200).unwrap().len(), 1);
    }

    #[test]
    fn follow_up_due_and_clear() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();
        let folder_id = store.get_folders("alice").unwrap()[0].id;

        let msg = test_summary("alice", folder_id, 1, "Follow up");
        let id = store.upsert_message(&msg).unwrap();
        let now = chrono::Utc::now().timestamp();

        // Not due yet.
        store.set_follow_up(&[id], now + 3600).unwrap();
        assert!(store.due_follow_ups(now).unwrap().is_empty());

        // Due once the time passes.
        store.set_follow_up(&[id], now - 1).unwrap();
        let due = store.due_follow_ups(now).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);

        // Cleared reminders never fire again.
        store.clear_follow_up(&[id]).unwrap();
        assert!(store.due_follow_ups(now).unwrap().is_empty());
    }

    #[test]
    fn migration_v1_to_v2_adds_snooze_columns() {
        let dir = std::env::temp_dir().join(format!("bang-mig-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v1.db");
        let _ = std::fs::remove_file(&path);
        {
            // Build a V1 database: schema + version row, no V2 columns.
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute("INSERT INTO schema_version (version) VALUES (1)", [])
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let folder = test_folder("alice", "INBOX");
        store.upsert_folder(&folder).unwrap();
        let folder_id = store.get_folders("alice").unwrap()[0].id;
        let msg = test_summary("alice", folder_id, 1, "Old");
        let id = store.upsert_message(&msg).unwrap();

        // The V2 columns work on the migrated database.
        store
            .snooze_messages(&[id], chrono::Utc::now().timestamp() + 3600)
            .unwrap();
        assert!(store.get_messages(folder_id, 10).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_send_request(account: &str) -> SendRequest {
        SendRequest {
            account: AccountId(account.into()),
            from: format!("{account}@example.com"),
            to: vec!["bob@example.com".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Later".into(),
            body_text: "Hello later".into(),
            body_html: None,
            attachments: vec![OutgoingAttachment {
                filename: "note.txt".into(),
                content_type: "text/plain".into(),
                data: b"hello".to_vec(),
            }],
            in_reply_to: None,
            references: None,
        }
    }

    #[test]
    fn send_later_due_and_remove() {
        let store = Store::open_in_memory().unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let now = chrono::Utc::now().timestamp();

        let id = store
            .schedule_send(&test_send_request("alice"), now + 3600)
            .unwrap();
        assert!(id > 0);

        // Not due yet.
        assert!(store.due_sends(now).unwrap().is_empty());

        // Listed for the UI.
        let pending = store.get_pending_sends().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].subject, "Later");
        assert_eq!(pending[0].from, "alice@example.com");

        // Due once the time passes; the request round-trips, attachments
        // included.
        let due = store.due_sends(now + 3600).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, id);
        assert_eq!(due[0].1.subject, "Later");
        assert_eq!(due[0].1.attachments[0].data, b"hello");

        // Removed after a successful send.
        store.remove_send(id).unwrap();
        assert!(store.due_sends(now + 3600).unwrap().is_empty());
        assert!(store.get_pending_sends().unwrap().is_empty());
    }

    #[test]
    fn attachment_files_roundtrip_and_sanitize() {
        let dir = std::env::temp_dir().join(format!("bang-att-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.db");
        let _ = std::fs::remove_file(&db);
        let store = Store::open(&db).unwrap();

        // Round-trip.
        let path = store.save_attachment(1, "note.txt", b"hello").unwrap();
        assert!(path.ends_with("note.txt"));
        assert_eq!(store.read_attachment(&path).unwrap(), b"hello");

        // Path traversal in the file name is reduced to the final component.
        let evil = store.save_attachment(1, "../../evil.txt", b"x").unwrap();
        assert!(evil.starts_with(dir.join("attachments").to_str().unwrap()));
        assert!(evil.ends_with("evil.txt"));

        // Collisions get a numeric suffix.
        let dup = store.save_attachment(1, "note.txt", b"other").unwrap();
        assert_ne!(dup, path);
        assert_eq!(store.read_attachment(&dup).unwrap(), b"other");

        // Paths outside the attachments directory are rejected.
        assert!(store.read_attachment("/etc/passwd").is_err());

        // Deleting a message's files makes them unreadable.
        store.delete_attachment_files(1).unwrap();
        assert!(store.read_attachment(&path).is_err());

        // In-memory stores have no attachment dir.
        let mem = Store::open_in_memory().unwrap();
        assert!(mem.save_attachment(1, "a.txt", b"x").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_v2_to_v3_adds_pending_sends() {
        let dir = std::env::temp_dir().join(format!("bang-mig3-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v2.db");
        let _ = std::fs::remove_file(&path);
        {
            // Build a V2 database: V1 + V2 schema, no pending_sends table.
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE schema_version (version INTEGER NOT NULL);")
                .unwrap();
            conn.execute_batch(MIGRATION_V1).unwrap();
            conn.execute_batch(MIGRATION_V2).unwrap();
            conn.execute("INSERT INTO schema_version (version) VALUES (2)", [])
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        store.save_account(&test_account("alice")).unwrap();
        let id = store
            .schedule_send(&test_send_request("alice"), chrono::Utc::now().timestamp() + 3600)
            .unwrap();
        assert_eq!(store.get_pending_sends().unwrap().len(), 1);
        store.remove_send(id).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}

