//! `tonguetyped::media` against real MPRIS players on a private session bus:
//! a throwaway `dbus-daemon` with fake players that implement the parts of
//! `org.mpris.MediaPlayer2.Player` the pause/resume code uses.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tonguetyped::media;
use zbus::Connection;

/// A private bus that lives as long as this value.
struct PrivateBus {
    daemon: Child,
    address: String,
    root: std::path::PathBuf,
}

impl PrivateBus {
    fn start() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-media-bus-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                root.join("bus").display()
            ),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is needed for the media pause tests");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            daemon,
            address: address.trim().to_string(),
            root,
        }
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A fake player whose playback status the test can read and set, and which
/// logs every Pause/Play call it receives.
#[derive(Clone)]
struct FakePlayer {
    status: Arc<Mutex<String>>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    fn pause(&self) {
        self.calls.lock().unwrap().push("Pause");
        *self.status.lock().unwrap() = "Paused".to_string();
    }

    fn play(&self) {
        self.calls.lock().unwrap().push("Play");
        *self.status.lock().unwrap() = "Playing".to_string();
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.status.lock().unwrap().clone()
    }
}

impl FakePlayer {
    /// Serves a player under `name` on the bus, returning its handle and the
    /// connection that keeps it registered.
    async fn serve(bus: &PrivateBus, name: &str, status: &str) -> (Self, Connection) {
        let player = FakePlayer {
            status: Arc::new(Mutex::new(status.to_string())),
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let connection = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(name.to_string())
            .unwrap()
            .serve_at("/org/mpris/MediaPlayer2", player.clone())
            .unwrap()
            .build()
            .await
            .unwrap();
        (player, connection)
    }

    fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, status: &str) {
        *self.status.lock().unwrap() = status.to_string();
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn only_playing_players_are_paused_and_exactly_those_are_resumed() {
    let bus = PrivateBus::start();
    let (music, _music_bus) =
        FakePlayer::serve(&bus, "org.mpris.MediaPlayer2.music", "Playing").await;
    let (video, _video_bus) =
        FakePlayer::serve(&bus, "org.mpris.MediaPlayer2.video", "Paused").await;
    let (podcast, _podcast_bus) =
        FakePlayer::serve(&bus, "org.mpris.MediaPlayer2.podcast", "Stopped").await;
    let client = bus.connect().await;

    let paused = media::pause_playing(&client).await;

    assert_eq!(paused, ["org.mpris.MediaPlayer2.music"]);
    assert_eq!(music.status(), "Paused");
    assert_eq!(music.calls(), ["Pause"]);
    assert!(video.calls().is_empty());
    assert!(podcast.calls().is_empty());

    media::resume(&client, &paused).await;

    assert_eq!(music.status(), "Playing");
    assert_eq!(music.calls(), ["Pause", "Play"]);
    assert_eq!(video.status(), "Paused");
    assert!(video.calls().is_empty());
    assert!(podcast.calls().is_empty());
}

#[tokio::test]
async fn a_player_stopped_during_the_recording_is_not_restarted() {
    let bus = PrivateBus::start();
    let (music, _music_bus) =
        FakePlayer::serve(&bus, "org.mpris.MediaPlayer2.music", "Playing").await;
    let client = bus.connect().await;

    let paused = media::pause_playing(&client).await;
    music.set_status("Stopped");
    media::resume(&client, &paused).await;

    assert_eq!(music.status(), "Stopped");
    assert_eq!(music.calls(), ["Pause"]);
}

#[tokio::test]
async fn playerctld_is_skipped_so_the_real_player_is_paused_once() {
    let bus = PrivateBus::start();
    let (music, _music_bus) =
        FakePlayer::serve(&bus, "org.mpris.MediaPlayer2.music", "Playing").await;
    let (playerctld, _playerctld_bus) =
        FakePlayer::serve(&bus, "org.mpris.MediaPlayer2.playerctld", "Playing").await;
    let client = bus.connect().await;

    let paused = media::pause_playing(&client).await;

    assert_eq!(paused, ["org.mpris.MediaPlayer2.music"]);
    assert_eq!(music.calls(), ["Pause"]);
    assert!(playerctld.calls().is_empty());
}
