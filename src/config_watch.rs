//! Watches the daemon's own config file and reloads it live when it changes.
//!
//! Nothing else tells the daemon its config file changed, so a background task
//! polls the file's fingerprint - modification time and size - on a short
//! interval. Polling rather than an inotify watch keeps the mechanism
//! dependency-free and handles an editor's write-then-rename save without
//! having to follow the replaced inode: the path always resolves to whatever
//! file currently holds the config. A short debounce reads one settled file
//! instead of each intermediate write of a save that lands in pieces.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How often the config file's fingerprint is checked while the daemon runs.
/// One `stat` on a small file is negligible next to the daemon's other work.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How long the file must stop changing before a detected change is acted on,
/// so a save that writes and then renames (or a burst of writes) is read once,
/// after it settles, rather than while it is half-written.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// What identifies one version of the config file for change detection: its
/// modification time and its size. `None` means the file was absent or its
/// metadata unreadable, which is itself a change from a present file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    modified: Option<SystemTime>,
    len: u64,
}

impl Fingerprint {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}

/// Polls `path` and calls `reload` once after each change that settles.
///
/// The fingerprint read at the start is the baseline, so a file that is
/// already in place when the watch begins does not trigger a reload. After the
/// baseline, a change is any fingerprint that differs from the last one acted
/// on: the fingerprint is then re-read every `debounce` until it holds steady,
/// `reload` runs, and that settled fingerprint becomes the new baseline. An
/// error from `reload` is the caller's to report and never stops the watch, so
/// a persistently invalid file is reloaded-and-rejected once per change while
/// the daemon keeps running on its last good config.
pub async fn watch<F, Fut>(
    path: PathBuf,
    poll_interval: Duration,
    debounce: Duration,
    mut reload: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut baseline = Fingerprint::read(&path);
    loop {
        tokio::time::sleep(poll_interval).await;
        let observed = Fingerprint::read(&path);
        if observed == baseline {
            continue;
        }
        // Let the write settle before reading it, so a save split across
        // several writes (or a write followed by a rename) is reloaded once.
        let mut settled = observed;
        loop {
            tokio::time::sleep(debounce).await;
            let next = Fingerprint::read(&path);
            if next == settled {
                break;
            }
            settled = next;
        }
        baseline = settled;
        reload().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tonguetyped-config-watch-{}-{}-{name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    async fn wait_for_reloads(reloads: &AtomicUsize, expected: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while reloads.load(Ordering::SeqCst) < expected {
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected {expected} reloads, saw {}",
                reloads.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn spawn_watch(path: PathBuf, reloads: Arc<AtomicUsize>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            watch(
                path,
                Duration::from_millis(20),
                Duration::from_millis(40),
                move || {
                    let reloads = reloads.clone();
                    async move {
                        reloads.fetch_add(1, Ordering::SeqCst);
                    }
                },
            )
            .await
        })
    }

    #[tokio::test]
    async fn a_settled_change_triggers_one_reload() {
        let path = temp_path("settled");
        std::fs::write(&path, "before").unwrap();
        let reloads = Arc::new(AtomicUsize::new(0));
        let handle = spawn_watch(path.clone(), reloads.clone());

        // Let the task take its baseline before changing the file.
        tokio::time::sleep(Duration::from_millis(80)).await;
        std::fs::write(&path, "after-change").unwrap();
        wait_for_reloads(&reloads, 1).await;

        // The file is now stable, so no further reload may fire.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(reloads.load(Ordering::SeqCst), 1);

        handle.abort();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn rapid_writes_debounce_to_a_single_reload() {
        let path = temp_path("debounce");
        std::fs::write(&path, "v0").unwrap();
        let reloads = Arc::new(AtomicUsize::new(0));
        let handle = spawn_watch(path.clone(), reloads.clone());

        tokio::time::sleep(Duration::from_millis(80)).await;
        for value in ["v1", "v2", "v3", "v4"] {
            std::fs::write(&path, value).unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        wait_for_reloads(&reloads, 1).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(reloads.load(Ordering::SeqCst), 1);

        handle.abort();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn an_unchanged_file_never_reloads() {
        let path = temp_path("unchanged");
        std::fs::write(&path, "steady").unwrap();
        let reloads = Arc::new(AtomicUsize::new(0));
        let handle = spawn_watch(path.clone(), reloads.clone());

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(reloads.load(Ordering::SeqCst), 0);

        handle.abort();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
