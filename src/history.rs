use rusqlite::Connection;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct HistoryStore {
    conn: Connection,
}

impl HistoryStore {
    pub fn new(db_path: &PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            let parent_existed = parent.exists();
            std::fs::create_dir_all(parent)?;
            if !parent_existed {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(db_path)?;
        std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600))?;
        let conn = Connection::open(db_path)?;
        let store = HistoryStore { conn };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                transcript_text TEXT NOT NULL,
                recording_path TEXT,
                language TEXT NOT NULL DEFAULT 'auto'
            );

            CREATE INDEX IF NOT EXISTS idx_history_timestamp ON history(timestamp);",
        )?;
        Ok(())
    }

    pub fn insert(
        &self,
        transcript_text: &str,
        recording_path: Option<&str>,
        language: &str,
    ) -> anyhow::Result<i64> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.conn.execute(
            "INSERT INTO history (timestamp, transcript_text, recording_path, language)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![timestamp, transcript_text, recording_path, language],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_last(&self) -> anyhow::Result<Option<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, transcript_text, language FROM history ORDER BY timestamp DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        match rows.next() {
            Some(Ok(row)) => Ok(Some(row)),
            _ => Ok(None),
        }
    }

    pub fn get_last_result(&self) -> anyhow::Result<Option<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT transcript_text, timestamp FROM history ORDER BY timestamp DESC, id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn delete_recording_path(&self, id: i64) -> anyhow::Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT recording_path FROM history WHERE id = ?1")?;
        let path: Option<String> = stmt
            .query_map([id], |row| row.get(0))?
            .next()
            .and_then(|r| r.ok())
            .flatten();

        if let Some(path) = path {
            if !path.is_empty() {
                let p = PathBuf::from(&path);
                if p.exists() {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }

        self.conn.execute(
            "UPDATE history SET recording_path = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    pub fn prune(&self, max_entries: u64) -> anyhow::Result<()> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))?;
        if count as u64 > max_entries {
            let to_delete = count as u64 - max_entries;
            self.conn.execute(
                "DELETE FROM history WHERE id IN (
                    SELECT id FROM history ORDER BY timestamp ASC LIMIT ?1
                )",
                [to_delete],
            )?;
        }
        Ok(())
    }
}

pub fn history_db_path() -> PathBuf {
    let dir = directories::BaseDirs::new()
        .map(|b| b.data_dir().join("tonguetyped"))
        .unwrap_or_else(|| PathBuf::from(".local/share/tonguetyped"));
    std::fs::create_dir_all(&dir).ok();
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir.join("history.db")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_schema_creation() {
        let tmp = std::env::temp_dir().join("tonguetyped_test_history.db");
        let store = HistoryStore::new(&tmp).unwrap();
        let result = store.insert("test transcript", None, "auto");
        assert!(result.is_ok());
        assert_eq!(
            std::fs::metadata(&tmp).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_get_last_empty() {
        let tmp = std::env::temp_dir().join("tonguetyped_test_history_empty.db");
        let store = HistoryStore::new(&tmp).unwrap();
        let result = store.get_last().unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_insert_and_retrieve() {
        let tmp = std::env::temp_dir().join("tonguetyped_test_history_retrieve.db");
        let store = HistoryStore::new(&tmp).unwrap();
        store.insert("hello world", None, "en").unwrap();
        let last = store.get_last().unwrap();
        assert!(last.is_some());
        let (_, text, lang) = last.unwrap();
        assert_eq!(text, "hello world");
        assert_eq!(lang, "en");
        let _ = std::fs::remove_file(&tmp);
    }
}
