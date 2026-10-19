use crate::config::Config;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

const AUTOSTART_ENTRY: &[u8] = include_bytes!("../data/tonguetyped-daemon.desktop");
const DESKTOP_FILE_NAME: &str = "tonguetyped.desktop";
const FLATPAK_INFO_PATH: &str = "/.flatpak-info";
const AUTOSTART_REASON: &str = "Start TongueTyped dictation when you sign in";
const AUTOSTART_COMMAND: [&str; 2] = ["tonguetyped", "daemon"];

pub fn update(enabled: bool) -> anyhow::Result<()> {
    let _lock = lock()?;
    let mut config = match Config::reload() {
        Ok(config) => config,
        Err(_) if !enabled => return apply(enabled),
        Err(error) => return Err(error),
    };
    let previous = config.startup.autostart;
    config.startup.autostart = enabled;
    save_configuration_locked(&config, Some(previous))
}

pub fn save_configuration(config: &Config) -> anyhow::Result<()> {
    let _lock = lock()?;
    let previous = Config::reload().ok().map(|config| config.startup.autostart);
    save_configuration_locked(config, previous)
}

fn save_configuration_locked(
    config: &Config,
    previous_enabled: Option<bool>,
) -> anyhow::Result<()> {
    apply(config.startup.autostart)?;
    if let Err(error) = config.save() {
        let rollback = previous_enabled.map_or(Ok(()), apply);
        if let Err(rollback_error) = rollback {
            anyhow::bail!(
                "failed to save configuration: {error}; failed to restore autostart: {rollback_error}"
            );
        }
        return Err(error);
    }
    Ok(())
}

fn apply(enabled: bool) -> anyhow::Result<()> {
    let path = if is_flatpak() { None } else { Some(path()?) };
    apply_dispatch(path.as_deref(), enabled, request_background_autostart)
}

fn apply_dispatch(
    path: Option<&Path>,
    enabled: bool,
    request_background: impl FnOnce(bool) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match path {
        Some(path) => set_enabled_at(path, enabled),
        None => request_background(enabled),
    }
}

fn request_background_autostart(enabled: bool) -> anyhow::Result<()> {
    std::thread::spawn(move || {
        crate::activation::portal_runtime().block_on(async {
            let request = ashpd::desktop::background::Background::request()
                .reason(AUTOSTART_REASON)
                .auto_start(enabled)
                .command(AUTOSTART_COMMAND)
                .dbus_activatable(false)
                .send()
                .await?;
            let response = request.response()?;
            portal_result(enabled, response.auto_start())
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("autostart portal request thread panicked"))?
}

fn portal_result(requested: bool, reported: bool) -> anyhow::Result<()> {
    if requested == reported {
        Ok(())
    } else if requested {
        anyhow::bail!("the desktop did not enable autostart")
    } else {
        anyhow::bail!("the desktop did not disable autostart")
    }
}

fn is_flatpak() -> bool {
    let flatpak_id = std::env::var_os("FLATPAK_ID");
    flatpak_detected(flatpak_id.as_deref(), Path::new(FLATPAK_INFO_PATH).exists())
}

fn flatpak_detected(flatpak_id: Option<&OsStr>, info_file_exists: bool) -> bool {
    info_file_exists || flatpak_id.is_some_and(|id| !id.is_empty())
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

fn set_enabled_at(path: &Path, enabled: bool) -> anyhow::Result<()> {
    if enabled {
        let directory = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("autostart path has no parent directory"))?;
        fs::create_dir_all(directory)?;
        crate::config::atomic_write(path, AUTOSTART_ENTRY)
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
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
        assert_eq!(fs::read(&path).unwrap(), AUTOSTART_ENTRY);

        fs::write(&path, "outdated entry").unwrap();
        set_enabled_at(&path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), AUTOSTART_ENTRY);

        set_enabled_at(&path, false).unwrap();
        set_enabled_at(&path, false).unwrap();
        assert!(!path.exists());

        set_enabled_at(&path, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), AUTOSTART_ENTRY);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desktop_file_backend_writes_entry_without_using_the_portal() {
        let root = test_root("desktop-file");
        let path = root.join("autostart").join(DESKTOP_FILE_NAME);

        apply_dispatch(Some(&path), true, |_| {
            panic!("the desktop-file backend must not request portal autostart")
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), AUTOSTART_ENTRY);

        apply_dispatch(Some(&path), false, |_| {
            panic!("the desktop-file backend must not request portal autostart")
        })
        .unwrap();
        assert!(!path.exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flatpak_backend_requests_portal_without_writing_desktop_entry() {
        let root = test_root("flatpak");
        let path = root.join("autostart").join(DESKTOP_FILE_NAME);
        let requested = std::cell::Cell::new(None);

        apply_dispatch(None, true, |enabled| {
            requested.set(Some(enabled));
            Ok(())
        })
        .unwrap();

        assert_eq!(requested.get(), Some(true));
        assert!(!path.exists());
        assert!(!root.exists());
    }

    #[test]
    fn portal_result_requires_the_requested_state() {
        assert!(portal_result(true, true).is_ok());
        assert!(portal_result(false, false).is_ok());
        assert!(portal_result(true, false).is_err());
        assert!(portal_result(false, true).is_err());
    }

    #[test]
    fn flatpak_detection_uses_the_app_id_or_the_info_file() {
        assert!(!flatpak_detected(None, false));
        assert!(flatpak_detected(
            Some(OsStr::new("io.github.cheerfulScumbag.tonguetyped")),
            false
        ));
        assert!(flatpak_detected(None, true));
        assert!(!flatpak_detected(Some(OsStr::new("")), false));
    }
}
