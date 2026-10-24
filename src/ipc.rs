use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutStatus {
    /// The listener is connecting or rebinding; `activation_error` says why if
    /// it is recovering from a failure.
    Initializing,
    /// The listener holds a session and the desktop reports a key bound for
    /// the activation action.
    Available,
    /// The listener holds a session but the desktop reports no key bound for
    /// the activation action yet. Not a failure: the user has not set a
    /// shortcut.
    Unbound,
    /// The listener lost its session or could not bind the shortcut.
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
    /// Ask the running daemon's own activation listener to report the next
    /// press of the activation shortcut. Only the daemon may own a
    /// GlobalShortcuts session while it runs (see `activation`), so shortcut
    /// testing goes through it instead of opening a competing session.
    ShortcutTest,
    /// Ask the daemon to open the desktop's shortcut editor on its own
    /// session, so reconfiguring cannot deactivate the shared desktop action.
    ConfigureShortcut,
    /// Stop waiting for a `ConfigureShortcut` dialog (the user kept the
    /// current binding). Sent on a separate connection so the original
    /// request can settle on the unchanged trigger.
    CancelShortcutConfigure,
}

// The status reply is deliberately the one large variant: it is built,
// serialized and dropped per request, so boxing it would only add an
// allocation without keeping any value alive.
#[allow(clippy::large_enum_variant)]
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
        /// The key the desktop reports is currently bound for the activation
        /// action, in TongueTyped's own wording (`Super+O`). `None` when no
        /// key is bound, or from a daemon that predates reporting it.
        #[serde(default)]
        shortcut_trigger: Option<String>,
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
        /// The configured `output.typing_backend`. `None` from an older daemon.
        #[serde(default)]
        output_configured: Option<String>,
        /// The typing backend delivery will use after hidden capability
        /// probing, `None` while nothing has resolved. `None` from an older
        /// daemon.
        #[serde(default)]
        output_backend: Option<String>,
        /// A substitution or dead end worth showing the user, for example a
        /// pinned libei replaced by wtype. `None` when the configured backend
        /// works, or from an older daemon.
        #[serde(default)]
        output_warning: Option<String>,
        /// Clipboard fallbacks since the daemon started. Zero from an older
        /// daemon.
        #[serde(default)]
        output_fallbacks: u64,
        /// Why the most recent fallback happened. `None` when none has.
        #[serde(default)]
        last_output_failure: Option<String>,
    },
    LastResult {
        text: String,
        timestamp: u64,
    },
    RecordingStarted,
    RecordingStopped,
    Cancelled,
    /// The daemon's own listener saw (`pressed: true`) or timed out waiting
    /// for (`pressed: false`) a press of the activation shortcut.
    ShortcutTest {
        pressed: bool,
    },
    /// The desktop's shortcut editor closed on the daemon's own session.
    /// `trigger_description` is whatever the desktop reports afterwards, in
    /// the desktop's own wording.
    ShortcutConfigured {
        changed: bool,
        trigger_description: String,
    },
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
