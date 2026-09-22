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
