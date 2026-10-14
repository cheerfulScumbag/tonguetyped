//! Pauses media players while a recording runs, so music or a video does not
//! talk over the dictation. Players are reached through MPRIS on the session
//! bus: every `org.mpris.MediaPlayer2.*` name whose `PlaybackStatus` is
//! `Playing` is paused when recording starts, and exactly those players are
//! resumed when it ends. Gated by `audio.pause_media`, which defaults to off.

use std::time::Duration;
use zbus::proxy::CacheProperties;
use zbus::Connection;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld re-exports whichever real player was active last under its own
/// MPRIS name. Pausing through it as well would act on that player twice, so
/// only real players are paused.
const PLAYERCTLD: &str = "org.mpris.MediaPlayer2.playerctld";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// Bounds every D-Bus call, so a hung player can delay the end of a recording
/// by at most this much per call instead of the bus default of 25 seconds.
const CALL_TIMEOUT: Duration = Duration::from_millis(500);

/// Connects to the session bus with `CALL_TIMEOUT` applied to every call.
pub async fn session_connection() -> zbus::Result<Connection> {
    zbus::connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()
        .await
}

/// Pauses every MPRIS player that is currently playing and returns the bus
/// names of the ones it paused, for `resume`. A player that fails to answer is
/// logged and skipped rather than stopping the others from being paused.
pub async fn pause_playing(connection: &Connection) -> Vec<String> {
    let names = match zbus::fdo::DBusProxy::new(connection).await {
        Ok(dbus) => match dbus.list_names().await {
            Ok(names) => names,
            Err(error) => {
                tracing::warn!("could not list media players: {error}");
                return Vec::new();
            }
        },
        Err(error) => {
            tracing::warn!("could not list media players: {error}");
            return Vec::new();
        }
    };
    let mut paused = Vec::new();
    for name in names {
        let name = name.as_str();
        if !name.starts_with(MPRIS_PREFIX) || name == PLAYERCTLD {
            continue;
        }
        match pause_if_playing(connection, name).await {
            Ok(true) => paused.push(name.to_string()),
            Ok(false) => {}
            Err(error) => tracing::warn!("could not pause media player {name}: {error}"),
        }
    }
    paused
}

/// Resumes the players `pause_playing` paused. A player that is no longer
/// paused (the user stopped it or started it again meanwhile) is left alone.
pub async fn resume(connection: &Connection, players: &[String]) {
    for name in players {
        if let Err(error) = resume_if_paused(connection, name).await {
            tracing::warn!("could not resume media player {name}: {error}");
        }
    }
}

async fn player(connection: &Connection, name: &str) -> zbus::Result<zbus::Proxy<'static>> {
    zbus::proxy::Builder::new(connection)
        .destination(name.to_string())?
        .path(PLAYER_PATH)?
        .interface(PLAYER_INTERFACE)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

async fn pause_if_playing(connection: &Connection, name: &str) -> zbus::Result<bool> {
    let player = player(connection, name).await?;
    if player.get_property::<String>("PlaybackStatus").await? != "Playing" {
        return Ok(false);
    }
    player.call_method("Pause", &()).await?;
    Ok(true)
}

async fn resume_if_paused(connection: &Connection, name: &str) -> zbus::Result<()> {
    let player = player(connection, name).await?;
    if player.get_property::<String>("PlaybackStatus").await? == "Paused" {
        player.call_method("Play", &()).await?;
    }
    Ok(())
}

/// Media paused for the length of one recording. `start` begins pausing in
/// the background, so the microphone never waits on the bus; dropping the
/// guard waits for that pause to finish and resumes what it paused. Because
/// the resume happens on drop, it runs however the recording ends: stop,
/// cancel, error, or a panic in the recording worker.
pub struct MediaPause {
    runtime: tokio::runtime::Handle,
    pausing: Option<tokio::task::JoinHandle<Option<Paused>>>,
}

/// The players one `MediaPause` paused, and the connection to resume them on.
struct Paused {
    connection: Connection,
    players: Vec<String>,
}

impl MediaPause {
    /// Starts pausing playing media on the current tokio runtime. Call it from
    /// a thread inside the runtime but outside async code (the recording
    /// worker's `spawn_blocking` thread), because dropping the guard blocks.
    pub fn start(runtime: tokio::runtime::Handle) -> Self {
        let pausing = runtime.spawn(async {
            let connection = match session_connection().await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!("could not reach the session bus to pause media: {error}");
                    return None;
                }
            };
            let players = pause_playing(&connection).await;
            Some(Paused {
                connection,
                players,
            })
        });
        Self {
            runtime,
            pausing: Some(pausing),
        }
    }
}

impl Drop for MediaPause {
    fn drop(&mut self) {
        let Some(pausing) = self.pausing.take() else {
            return;
        };
        self.runtime.block_on(async {
            if let Ok(Some(paused)) = pausing.await {
                resume(&paused.connection, &paused.players).await;
            }
        });
    }
}
