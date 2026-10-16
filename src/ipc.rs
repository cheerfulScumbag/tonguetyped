use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutStatus {
    Initializing,
    Available,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Start,
    Stop,
    Toggle,
    Cancel,
    Status,
    ReloadConfig,
    GetLastResult,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error {
        message: String,
    },
    Busy,
    Status {
        state: String,
        activation_mode: String,
        operation_error: Option<String>,
        shortcut_status: ShortcutStatus,
        activation_error: Option<String>,
        /// Why the last attempt to load the config file failed. Set when a
        /// manual `reload` or the config-file watcher rejects the file on disk;
        /// the daemon keeps running on its last good config until a reload
        /// succeeds, at which point this clears. `None` when there is no such
        /// error, and from a daemon that predates the field.
        #[serde(default)]
        config_error: Option<String>,
        /// The daemon's `build_info::VERSION`. `None` when talking to a daemon
        /// built before it started reporting this, which by definition is an
        /// older build than any client that knows about the field.
        #[serde(default)]
        build: Option<String>,
        /// The daemon's configured model id. `None` from a daemon that
        /// predates reporting it, like `build`.
        #[serde(default)]
        model: Option<String>,
        /// The inference backend the daemon's configuration resolves to,
        /// e.g. `transcribe.cpp/vulkan`. `None` from an older daemon.
        #[serde(default)]
        backend: Option<String>,
        /// The device that backend runs on. `None` from an older daemon.
        #[serde(default)]
        device: Option<String>,
    },
    LastResult {
        text: String,
        timestamp: u64,
    },
    RecordingStarted,
    RecordingStopped,
    Cancelled,
}

pub fn encode_frame<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let mut json = serde_json::to_string(value)?;
    json.push('\n');
    Ok(json)
}

pub fn decode_frame<T: serde::de::DeserializeOwned>(data: &str) -> anyhow::Result<T> {
    let value: T = serde_json::from_str(data.trim())?;
    Ok(value)
}
