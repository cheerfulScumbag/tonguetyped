use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Start,
    Stop,
    Toggle,
    Cancel,
    Status,
    Doctor,
    ReloadConfig,
    GetLastResult,
    HoldPress,
    HoldRelease,
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
        recording: bool,
        processing: bool,
        activation_mode: String,
    },
    DoctorResult {
        compositor: String,
        desktop: String,
        audio_available: bool,
        model_ready: bool,
        socket_health: String,
        helpers_found: Vec<String>,
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