//! Machine-readable daemon status for status bars: `tonguetyped status
//! --format json` prints one JSON object, and `--follow` keeps printing one
//! per line each time it changes. Each object is in the shape Waybar's custom
//! module reads with `"return-type": "json"`:
//!
//! ```json
//! {"text":"recording","alt":"recording","class":"recording","tooltip":"TongueTyped: recording"}
//! ```
//!
//! `text`, `alt` and `class` all carry the state (`idle`, `recording`,
//! `processing`, or `stopped` when no daemon is running), so a bar can show
//! it directly, map it to an icon through `alt`, or style it through `class`.
//! `--extended` adds `model`, `backend`, `device` and `build` fields (each
//! `null` when the daemon does not report it); the tooltip lists `model`,
//! `backend` and `device`, while `build` appears only in the JSON.
//!
//! Following polls the daemon's ordinary `Status` request over one kept-open
//! connection, so it works against any daemon build. A daemon that stops or
//! restarts is reported as `stopped` and picked up again once it is back,
//! rather than ending the stream the bar is reading.

use crate::ipc::{self, Request, Response};
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

/// How often `--follow` asks the daemon for its state: fast enough that a bar
/// shows recording as it starts, and a `Status` round trip costs the daemon
/// one mutex lock.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long one `Status` request may take before the daemon counts as not
/// answering. The daemon answers from memory, so this only trips when it is
/// wedged, and then the bar shows `stopped` instead of freezing on a stale
/// state.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// The state reported when no daemon answers on the control socket.
pub const STOPPED: &str = "stopped";

/// One status line. Field order is the serialized order.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct BarStatus {
    pub text: String,
    pub alt: String,
    pub class: String,
    pub tooltip: String,
    #[serde(flatten)]
    pub details: Option<Details>,
}

/// The `--extended` fields.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Details {
    pub model: Option<String>,
    pub backend: Option<String>,
    pub device: Option<String>,
    pub build: Option<String>,
}

/// Builds the status line for a daemon `Status` response, or for a stopped
/// daemon when `response` is `None` (or is not a status at all).
pub fn bar_status(response: Option<&Response>, extended: bool) -> BarStatus {
    let Some(Response::Status {
        state,
        operation_error,
        build,
        model,
        backend,
        device,
        ..
    }) = response
    else {
        return BarStatus {
            text: STOPPED.to_string(),
            alt: STOPPED.to_string(),
            class: STOPPED.to_string(),
            tooltip: "TongueTyped: daemon not running".to_string(),
            details: extended.then_some(Details {
                model: None,
                backend: None,
                device: None,
                build: None,
            }),
        };
    };
    let mut tooltip = format!("TongueTyped: {state}");
    if extended {
        let unknown = |value: &Option<String>| value.clone().unwrap_or_else(|| "unknown".into());
        tooltip.push_str(&format!(
            "\nModel: {}\nBackend: {} on {}",
            unknown(model),
            unknown(backend),
            unknown(device)
        ));
    }
    if let Some(error) = operation_error {
        tooltip.push_str(&format!("\nLast error: {error}"));
    }
    BarStatus {
        text: state.clone(),
        alt: state.clone(),
        class: state.clone(),
        tooltip,
        details: extended.then(|| Details {
            model: model.clone(),
            backend: backend.clone(),
            device: device.clone(),
            build: build.clone(),
        }),
    }
}

/// `tonguetyped status --format json`: prints the current status once. A
/// stopped daemon is a normal `stopped` line, not an error, so a bar polling
/// this on an interval keeps showing something useful.
pub async fn print_json(extended: bool) -> anyhow::Result<()> {
    let sock_path = crate::daemon::socket_path()?;
    let response = DaemonConnection::default().status(&sock_path).await;
    let line = serde_json::to_string(&bar_status(response.as_ref(), extended))?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()?;
    Ok(())
}

/// `tonguetyped status --format json --follow`: prints the status, then a new
/// line every time it changes, until stdout closes (the bar went away).
pub async fn follow_json(extended: bool) -> anyhow::Result<()> {
    let sock_path = crate::daemon::socket_path()?;
    follow_json_at(&sock_path, extended, std::io::stdout()).await
}

/// `follow_json` against an explicit socket and writer, so it can be tested
/// without a real daemon. Only returns on a write failure.
pub async fn follow_json_at(
    sock_path: &Path,
    extended: bool,
    mut out: impl Write,
) -> anyhow::Result<()> {
    let mut connection = DaemonConnection::default();
    let mut last_line = None;
    loop {
        let response = connection.status(sock_path).await;
        let line = serde_json::to_string(&bar_status(response.as_ref(), extended))?;
        if last_line.as_ref() != Some(&line) {
            writeln!(out, "{line}")?;
            out.flush()?;
            last_line = Some(line);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A control-socket connection kept open across polls and reopened after the
/// daemon goes away.
#[derive(Default)]
struct DaemonConnection {
    stream: Option<(BufReader<OwnedReadHalf>, OwnedWriteHalf)>,
}

impl DaemonConnection {
    /// The daemon's `Status` response, or `None` when no daemon answers.
    async fn status(&mut self, sock_path: &Path) -> Option<Response> {
        if self.stream.is_none() {
            let stream = UnixStream::connect(sock_path).await.ok()?;
            let (reader, writer) = stream.into_split();
            self.stream = Some((BufReader::new(reader), writer));
        }
        let response = tokio::time::timeout(REQUEST_TIMEOUT, self.request_status())
            .await
            .ok()
            .flatten();
        if !matches!(response, Some(Response::Status { .. })) {
            self.stream = None;
            return None;
        }
        response
    }

    async fn request_status(&mut self) -> Option<Response> {
        let (reader, writer) = self.stream.as_mut()?;
        let frame = ipc::encode_frame(&Request::Status).ok()?;
        writer.write_all(frame.as_bytes()).await.ok()?;
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        ipc::decode_frame(&line).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn status(state: &str, operation_error: Option<&str>) -> Response {
        Response::Status {
            state: state.to_string(),
            activation_mode: "hold".to_string(),
            operation_error: operation_error.map(str::to_string),
            shortcut_status: ipc::ShortcutStatus::Available,
            activation_error: None,
            build: Some("0.2.2 (a1b2c3d)".to_string()),
            model: Some("whisper-small-q5_k_m".to_string()),
            backend: Some("transcribe.cpp/vulkan".to_string()),
            device: Some("AMD Radeon RX 7900".to_string()),
        }
    }

    fn json(response: Option<&Response>, extended: bool) -> serde_json::Value {
        serde_json::to_value(bar_status(response, extended)).unwrap()
    }

    #[test]
    fn json_status_is_a_waybar_object_carrying_the_state() {
        assert_eq!(
            json(Some(&status("recording", None)), false),
            serde_json::json!({
                "text": "recording",
                "alt": "recording",
                "class": "recording",
                "tooltip": "TongueTyped: recording",
            })
        );
    }

    #[test]
    fn json_status_serializes_on_one_line_in_waybar_field_order() {
        assert_eq!(
            serde_json::to_string(&bar_status(Some(&status("idle", None)), false)).unwrap(),
            r#"{"text":"idle","alt":"idle","class":"idle","tooltip":"TongueTyped: idle"}"#
        );
    }

    #[test]
    fn extended_json_status_adds_model_backend_device_and_build() {
        assert_eq!(
            json(Some(&status("processing", Some("output failed"))), true),
            serde_json::json!({
                "text": "processing",
                "alt": "processing",
                "class": "processing",
                "tooltip": "TongueTyped: processing\nModel: whisper-small-q5_k_m\n\
                            Backend: transcribe.cpp/vulkan on AMD Radeon RX 7900\n\
                            Last error: output failed",
                "model": "whisper-small-q5_k_m",
                "backend": "transcribe.cpp/vulkan",
                "device": "AMD Radeon RX 7900",
                "build": "0.2.2 (a1b2c3d)",
            })
        );
    }

    #[test]
    fn stopped_daemon_is_reported_as_a_stopped_state() {
        assert_eq!(
            json(None, false),
            serde_json::json!({
                "text": "stopped",
                "alt": "stopped",
                "class": "stopped",
                "tooltip": "TongueTyped: daemon not running",
            })
        );
        assert_eq!(
            json(None, true),
            serde_json::json!({
                "text": "stopped",
                "alt": "stopped",
                "class": "stopped",
                "tooltip": "TongueTyped: daemon not running",
                "model": null,
                "backend": null,
                "device": null,
                "build": null,
            })
        );
    }

    #[test]
    fn extended_fields_are_null_for_a_daemon_that_predates_them() {
        let frame = r#"{"type":"status","state":"idle","activation_mode":"hold","operation_error":null,"shortcut_status":"available","activation_error":null}"#;
        let response: Response = ipc::decode_frame(frame).unwrap();
        let value = json(Some(&response), true);
        assert_eq!(value["text"], "idle");
        assert_eq!(value["model"], serde_json::Value::Null);
        assert_eq!(value["backend"], serde_json::Value::Null);
        assert_eq!(
            value["tooltip"],
            "TongueTyped: idle\nModel: unknown\nBackend: unknown on unknown"
        );
    }

    /// Collects what `follow_json_at` writes, readable while it runs.
    #[derive(Clone, Default)]
    struct SharedOutput(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SharedOutput {
        fn states(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| {
                    let value: serde_json::Value = serde_json::from_str(line).unwrap();
                    value["text"].as_str().unwrap().to_string()
                })
                .collect()
        }

        async fn wait_for_states(&self, expected: &[&str]) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while self.states() != expected {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "expected states {expected:?}, got {:?}",
                    self.states()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    /// A fake daemon on one connection: answers every `Status` request with
    /// the state currently in `state`, and hangs up when `state` is `None`.
    async fn serve_one_connection(
        listener: &tokio::net::UnixListener,
        state: Arc<Mutex<Option<&'static str>>>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            assert!(matches!(ipc::decode_frame(&line).unwrap(), Request::Status));
            let Some(current) = *state.lock().unwrap() else {
                return;
            };
            let frame = ipc::encode_frame(&status(current, None)).unwrap();
            writer.write_all(frame.as_bytes()).await.unwrap();
        }
    }

    #[tokio::test]
    async fn follow_prints_each_change_once_and_survives_a_daemon_restart() {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-status-follow-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let sock_path = root.join("control.sock");

        let output = SharedOutput::default();
        let follow = {
            let (sock_path, output) = (sock_path.clone(), output.clone());
            tokio::spawn(async move { follow_json_at(&sock_path, false, output).await })
        };
        output.wait_for_states(&["stopped"]).await;

        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let state = Arc::new(Mutex::new(Some("idle")));
        let daemon = {
            let state = state.clone();
            tokio::spawn(async move {
                serve_one_connection(&listener, state.clone()).await;
                listener
            })
        };
        output.wait_for_states(&["stopped", "idle"]).await;
        *state.lock().unwrap() = Some("recording");
        output
            .wait_for_states(&["stopped", "idle", "recording"])
            .await;

        // The daemon goes away, then comes back on a fresh connection.
        *state.lock().unwrap() = None;
        let listener = daemon.await.unwrap();
        output
            .wait_for_states(&["stopped", "idle", "recording", "stopped"])
            .await;
        *state.lock().unwrap() = Some("idle");
        let daemon = tokio::spawn(async move { serve_one_connection(&listener, state).await });
        output
            .wait_for_states(&["stopped", "idle", "recording", "stopped", "idle"])
            .await;

        follow.abort();
        daemon.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}
