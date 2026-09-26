use crate::config::Config;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

const DESKTOP_ENTRY: &[u8] = include_bytes!("../data/tonguetyped.desktop");
const DESKTOP_FILE_NAME: &str = "tonguetyped.desktop";

pub fn update(enabled: bool) -> anyhow::Result<()> {
    let _lock = lock()?;
    let mut config = Config::reload()?;
    config.startup.autostart = enabled;
    save_configuration_locked(&config)
}

pub fn save_configuration(config: &Config) -> anyhow::Result<()> {
    let _lock = lock()?;
    save_configuration_locked(config)
}

fn path() -> anyhow::Result<PathBuf> {
    let config_path = Config::config_path()?;
    let config_root = config_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("configuration path has no XDG config directory"))?;
    Ok(config_root.join("autostart").join(DESKTOP_FILE_NAME))
}

fn lock_path() -> anyhow::Result<PathBuf> {
    let config_path = Config::config_path()?;
    let config_root = config_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!("configuration path has no parent directory"))?;
    Ok(config_root.join(".tonguetyped-autostart.lock"))
}

fn lock() -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path()?)?;
    File::lock(&file)?;
    Ok(file)
}

fn save_configuration_locked(config: &Config) -> anyhow::Result<()> {
    let path = path()?;
    let previous = match fs::read(&path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };

    set_enabled_at(&path, config.startup.autostart)?;
    if let Err(error) = config.save() {
        if let Err(rollback_error) = restore(&path, previous.as_deref()) {
            anyhow::bail!(
                "failed to save configuration: {error}; failed to restore autostart entry: {rollback_error}"
            );
        }
        return Err(error);
    }
    Ok(())
}

fn set_enabled_at(path: &Path, enabled: bool) -> anyhow::Result<()> {
    if enabled {
        let directory = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("autostart path has no parent directory"))?;
        fs::create_dir_all(directory)?;
        crate::config::atomic_write(path, DESKTOP_ENTRY)
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn restore(path: &Path, previous: Option<&[u8]>) -> anyhow::Result<()> {
    match previous {
        Some(content) => {
            let directory = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("autostart path has no parent directory"))?;
            fs::create_dir_all(directory)?;
            crate::config::atomic_write(path, content)
        }
        None => set_enabled_at(path, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tonguetyped-autostart-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn enable_upgrade_disable_and_reenable_are_idempotent() {
        let root = test_root("lifecycle");
        let path = root.join("autostart").join(DESKTOP_FILE_NAME);

        set_enabled_at(&path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), DESKTOP_ENTRY);

        fs::write(&path, "outdated entry").unwrap();
        set_enabled_at(&path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), DESKTOP_ENTRY);

        set_enabled_at(&path, false).unwrap();
        set_enabled_at(&path, false).unwrap();
        assert!(!path.exists());

        set_enabled_at(&path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), DESKTOP_ENTRY);

        fs::remove_dir_all(root).unwrap();
    }
}
