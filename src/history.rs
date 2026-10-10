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
        self.insert_at(transcript_text, recording_path, language, now_seconds())
    }

    /// Insert with an explicit timestamp - the shared implementation behind
    /// `insert`, and the seam age-based pruning is tested through.
    fn insert_at(
        &self,
        transcript_text: &str,
        recording_path: Option<&str>,
        language: &str,
        timestamp: i64,
    ) -> anyhow::Result<i64> {
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

    /// Applies both retention limits in one pass: drop rows older than
    /// `max_age_days`, then drop the oldest rows beyond `max_entries`. A `0`
    /// limit means "no limit" for that dimension, so `prune(0, 0)` is a no-op.
    pub fn prune(&self, max_entries: u64, max_age_days: u64) -> anyhow::Result<()> {
        self.prune_at(max_entries, max_age_days, now_seconds())
    }

    fn prune_at(&self, max_entries: u64, max_age_days: u64, now: i64) -> anyhow::Result<()> {
        if max_age_days > 0 {
            let age_seconds = (max_age_days as i64).saturating_mul(86_400);
            let cutoff = now.saturating_sub(age_seconds);
            self.conn
                .execute("DELETE FROM history WHERE timestamp < ?1", [cutoff])?;
        }
        if max_entries > 0 {
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
        }
        Ok(())
    }
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn history_db_path() -> anyhow::Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|b| b.data_dir().join("tonguetyped"))
        .map(|dir| dir.join("history.db"))
        .ok_or_else(|| anyhow::anyhow!("cannot determine the user data directory"))
}

/// Writes one finished transcript into `folder` as a plain-text file stamped
/// with the date and time, and returns the file it wrote. Called on every
/// finished dictation while a transcript folder is configured.
///
/// Write-once by contract: this only ever writes (creating `folder` if
/// needed, and picking a `-2`, `-3`, ... suffixed name rather than
/// overwriting an existing file), never reads back, and nothing ever prunes
/// these files - they outlive the database retention entirely. A leading
/// `~`/`~/` in `folder` expands to the user's home directory. The file is
/// written `0600` like the history database, since it holds transcript text.
pub fn export_transcript(folder: &str, text: &str, timestamp_secs: u64) -> anyhow::Result<PathBuf> {
    let folder = expand_home(folder.trim());
    if folder.as_os_str().is_empty() {
        anyhow::bail!("transcript folder is empty");
    }
    std::fs::create_dir_all(&folder)?;
    let (year, month, day, hour, minute, second) = local_datetime(timestamp_secs);
    let base = format!("{year:04}-{month:02}-{day:02}_{hour:02}-{minute:02}-{second:02}");
    let mut path = folder.join(format!("{base}.txt"));
    let mut suffix = 2;
    while path.exists() {
        path = folder.join(format!("{base}-{suffix}.txt"));
        suffix += 1;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    use std::io::Write;
    file.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(path)
}

/// Expands a leading `~` or `~/` to the user's home directory; any other path
/// (including a bare `~user`, which is left untouched) is returned unchanged.
pub(crate) fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            return home;
        }
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// The local-time calendar fields for a Unix timestamp, via `localtime_r` so
/// the exported filename matches the user's own clock. Falls back to UTC
/// (`gmtime_r`) if the local timezone can't be resolved.
fn local_datetime(timestamp_secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let time = timestamp_secs as libc::time_t;
    // SAFETY: `tm` is a zeroed `libc::tm` we own and hand to `localtime_r`,
    // which only writes into it; the returned pointer is to `tm`.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            libc::gmtime_r(&time, &mut tm);
        }
        tm
    };
    (
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tonguetyped-history-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn count(store: &HistoryStore) -> i64 {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn test_schema_creation() {
        let tmp = temp_path("schema.db");
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
    fn prune_caps_the_entry_count_keeping_the_newest() {
        let tmp = temp_path("count.db");
        let store = HistoryStore::new(&tmp).unwrap();
        for index in 0..10 {
            store
                .insert_at(&format!("entry {index}"), None, "auto", 1_000 + index)
                .unwrap();
        }
        store.prune_at(4, 0, 1_000 + 9).unwrap();
        assert_eq!(count(&store), 4);
        let oldest: i64 = store
            .conn
            .query_row("SELECT MIN(timestamp) FROM history", [], |row| row.get(0))
            .unwrap();
        assert_eq!(oldest, 1_006, "the newest four rows must survive");

        store.prune_at(0, 0, 1_000 + 9).unwrap();
        assert_eq!(count(&store), 4, "a zero entry limit means no limit");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn prune_drops_rows_older_than_the_age_limit() {
        let tmp = temp_path("age.db");
        let store = HistoryStore::new(&tmp).unwrap();
        let now = 1_000_000_000;
        store
            .insert_at("old", None, "auto", now - 40 * 86_400)
            .unwrap();
        store
            .insert_at("recent", None, "auto", now - 2 * 86_400)
            .unwrap();
        store.insert_at("now", None, "auto", now).unwrap();
        store.prune_at(0, 30, now).unwrap();
        assert_eq!(count(&store), 2, "only the 40-day-old row should go");

        store.prune_at(2, 30, now).unwrap();
        assert_eq!(count(&store), 2);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn export_writes_one_date_stamped_file_per_call() {
        let folder = temp_path("export");
        let first = export_transcript(folder.to_str().unwrap(), "hello", 1_700_000_000).unwrap();
        let second = export_transcript(folder.to_str().unwrap(), "world", 1_700_000_000).unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "hello\n");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "world\n");
        assert_ne!(first, second, "a same-second dictation must not overwrite");
        assert!(first
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(".txt"));
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
