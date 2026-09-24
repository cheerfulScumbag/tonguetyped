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

pub fn history_db_path() -> anyhow::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.data_dir().join("tonguetyped"))
        .map(|dir| dir.join("history.db"))
        .ok_or_else(|| anyhow::anyhow!("cannot determine the user data directory"))
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
}
