use super::*;
use crate::organization::{
    classify, sender_key, ClassificationSignals, Label, MailCategory, MessageOrganization,
};

pub(super) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE messages ADD COLUMN sender_key TEXT NOT NULL DEFAULT '';
        ALTER TABLE messages ADD COLUMN automatic_category TEXT NOT NULL DEFAULT 'primary';
        ALTER TABLE messages ADD COLUMN category_override TEXT;
        ALTER TABLE messages ADD COLUMN classification_signals TEXT NOT NULL DEFAULT '{}';
        CREATE INDEX messages_sender ON messages(sender_key);
        CREATE TABLE sender_category_rules(sender TEXT PRIMARY KEY, category TEXT NOT NULL);
        CREATE TABLE labels(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL COLLATE NOCASE UNIQUE, color TEXT NOT NULL);
        CREATE TABLE message_labels(message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, label_id INTEGER NOT NULL REFERENCES labels(id) ON DELETE CASCADE, PRIMARY KEY(message_id,label_id));
        CREATE INDEX label_messages ON message_labels(label_id, message_id);")?;
    let rows = {
        let mut stmt = conn.prepare("SELECT id, from_email, subject FROM messages")?;
        let result = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result
    };
    for (id, sender, subject) in rows {
        conn.execute(
            "UPDATE messages SET sender_key=?2, automatic_category=?3 WHERE id=?1",
            params![
                id,
                sender_key(&sender),
                classify(&sender, &subject, &Default::default()).key()
            ],
        )?;
    }
    Ok(())
}

pub(super) fn refresh(
    conn: &Connection,
    id: i64,
    hints: Option<&ClassificationSignals>,
) -> Result<()> {
    let (sender, subject, cached): (String, String, String) = conn
        .query_row(
            "SELECT from_email, subject, classification_signals FROM messages WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(db)?;
    let signals = hints
        .cloned()
        .unwrap_or_else(|| serde_json::from_str(&cached).unwrap_or_default());
    conn.execute("UPDATE messages SET sender_key=?2, automatic_category=?3, classification_signals=?4 WHERE id=?1",
        params![id,sender_key(&sender),classify(&sender,&subject,&signals).key(),serde_json::to_string(&signals).unwrap()]).map_err(db)?;
    Ok(())
}
fn db(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}
const EFFECTIVE: &str = "COALESCE(m.category_override,(SELECT category FROM sender_category_rules WHERE sender=m.sender_key),m.automatic_category)";
const COLUMNS: &str = "m.id,m.account_id,m.folder_id,m.uid,m.message_id,m.from_name,m.from_email,m.\"to\",m.cc,m.subject,m.date,m.flags,m.preview,m.has_attachments";
const SCOPE: &str = "((?1 IS NOT NULL AND m.folder_id=?1) OR (?1 IS NULL AND (?2 IS NOT NULL OR UPPER(f.path)='INBOX'))) AND (?2 IS NULL OR EXISTS(SELECT 1 FROM message_labels ml WHERE ml.message_id=m.id AND ml.label_id=?2)) AND (m.snoozed_until IS NULL OR m.snoozed_until<=?3)";

impl Store {
    /// Refresh header-only classification, preserving manual choices.
    pub fn update_classification(&self, id: i64, hints: &ClassificationSignals) -> Result<()> {
        refresh(&*self.lock()?, id, Some(hints))
    }
    /// Query a folder (None = unified inbox), or a global label, filtering before LIMIT.
    pub fn organized_messages(
        &self,
        folder: Option<i64>,
        label: Option<i64>,
        category: Option<MailCategory>,
        limit: usize,
    ) -> Result<Vec<MessageSummary>> {
        let conn = self.lock()?;
        let sql = format!("SELECT {COLUMNS} FROM messages m JOIN folders f ON f.id=m.folder_id WHERE {SCOPE} AND (?4 IS NULL OR {EFFECTIVE}=?4) ORDER BY m.date DESC,m.id DESC LIMIT ?5");
        let mut stmt = conn.prepare(&sql).map_err(db)?;
        let rows = stmt
            .query_map(
                params![
                    folder,
                    label,
                    chrono::Utc::now().timestamp(),
                    category.map(MailCategory::key),
                    limit as i64
                ],
                Self::row_to_summary,
            )
            .map_err(db)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db)?;
        Ok(rows)
    }
    /// Unread counts for every category in the entire cached view, before LIMIT.
    pub fn category_counts(&self, folder: Option<i64>) -> Result<[i32; 4]> {
        let conn = self.lock()?;
        let sql = format!("SELECT {EFFECTIVE}, COUNT(*) FROM messages m JOIN folders f ON f.id=m.folder_id WHERE {SCOPE} AND (m.flags & 1)=0 GROUP BY {EFFECTIVE}");
        let mut stmt = conn.prepare(&sql).map_err(db)?;
        let rows = stmt
            .query_map(
                params![folder, Option::<i64>::None, chrono::Utc::now().timestamp()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)),
            )
            .map_err(db)?;
        let mut counts = [0; 4];
        for row in rows {
            let (key, count) = row.map_err(db)?;
            let i = match key.as_str() {
                "transactions" => 1,
                "updates" => 2,
                "promotions" => 3,
                _ => 0,
            };
            counts[i] = count;
        }
        Ok(counts)
    }
    /// Read category state and assigned labels for a selected message.
    pub fn message_organization(&self, id: i64) -> Result<MessageOrganization> {
        let conn = self.lock()?;
        let sql=format!("SELECT {EFFECTIVE},m.category_override IS NOT NULL,EXISTS(SELECT 1 FROM sender_category_rules WHERE sender=m.sender_key) FROM messages m WHERE m.id=?1");
        let mut result = conn
            .query_row(&sql, [id], |r| {
                let key: String = r.get(0)?;
                Ok(MessageOrganization {
                    category: MailCategory::parse(&key).unwrap_or_default(),
                    message_override: r.get(1)?,
                    sender_override: r.get(2)?,
                    labels: Vec::new(),
                })
            })
            .map_err(db)?;
        let mut stmt = conn
            .prepare("SELECT label_id FROM message_labels WHERE message_id=?1")
            .map_err(db)?;
        result.labels = stmt
            .query_map([id], |r| r.get(0))
            .map_err(db)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db)?;
        Ok(result)
    }
    /// Set or remove a message correction or a sender rule across all accounts.
    pub fn set_category(
        &self,
        id: i64,
        category: Option<MailCategory>,
        sender: bool,
    ) -> Result<()> {
        let conn = self.lock()?;
        if sender {
            let key: String = conn
                .query_row("SELECT sender_key FROM messages WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .map_err(db)?;
            if key.is_empty() {
                return Err(Error::Store("Sender address is missing".into()));
            }
            if let Some(category) = category {
                conn.execute("INSERT INTO sender_category_rules(sender,category) VALUES(?1,?2) ON CONFLICT(sender) DO UPDATE SET category=excluded.category",params![key,category.key()]).map_err(db)?;
            } else {
                conn.execute("DELETE FROM sender_category_rules WHERE sender=?1", [key])
                    .map_err(db)?;
            }
        } else {
            conn.execute(
                "UPDATE messages SET category_override=?2 WHERE id=?1",
                params![id, category.map(MailCategory::key)],
            )
            .map_err(db)?;
        }
        Ok(())
    }
    /// List local labels shared by all accounts.
    pub fn labels(&self) -> Result<Vec<Label>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id,name,color FROM labels ORDER BY name COLLATE NOCASE,id")
            .map_err(db)?;
        let result = stmt
            .query_map([], |r| {
                Ok(Label {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    color: r.get(2)?,
                })
            })
            .map_err(db)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db)?;
        Ok(result)
    }
    /// Create or edit a label. Names are nonempty and unique ignoring ASCII case.
    pub fn save_label(&self, id: Option<i64>, name: &str, color: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > 80
            || color.len() != 7
            || !color.starts_with('#')
            || !color[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Store("Invalid label name or color".into()));
        }
        let conn = self.lock()?;
        if let Some(id) = id {
            conn.execute(
                "UPDATE labels SET name=?2,color=?3 WHERE id=?1",
                params![id, name, color],
            )
            .map_err(db)?;
        } else {
            conn.execute(
                "INSERT INTO labels(name,color) VALUES(?1,?2)",
                params![name, color],
            )
            .map_err(db)?;
        }
        Ok(())
    }
    /// Delete a label and its assignments, never its messages.
    pub fn delete_label(&self, id: i64) -> Result<()> {
        self.lock()?
            .execute("DELETE FROM labels WHERE id=?1", [id])
            .map_err(db)?;
        Ok(())
    }
    /// Assign or remove one of a message's independent labels.
    pub fn set_message_label(&self, id: i64, label: i64, assigned: bool) -> Result<()> {
        let conn = self.lock()?;
        if assigned {
            conn.execute(
                "INSERT OR IGNORE INTO message_labels(message_id,label_id) VALUES(?1,?2)",
                params![id, label],
            )
            .map_err(db)?;
        } else {
            conn.execute(
                "DELETE FROM message_labels WHERE message_id=?1 AND label_id=?2",
                params![id, label],
            )
            .map_err(db)?;
        }
        Ok(())
    }
}
