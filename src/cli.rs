//! Clap argument definitions, kept as a library module (rather than private to
//! `main.rs`) so `build.rs` can render the man pages and shell completions
//! from the exact same definition `--help` uses.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "tonguetyped",
    version = crate::build_info::VERSION,
    about = "Linux dictation application"
)]
pub struct Cli {
    /// Running with no subcommand opens the interactive setup console, the
    /// same one `setup` opens - every subcommand below keeps its exact
    /// existing CLI behavior.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Configure TongueTyped interactively
    Setup,
    /// Start the daemon process
    Daemon {
        #[command(subcommand)]
        command: Option<DaemonCommand>,
    },
    /// Start a new recording
    Start,
    /// Stop the current recording
    Stop,
    /// Toggle recording on/off
    Toggle,
    /// Cancel current recording or processing
    Cancel,
    /// Get daemon status
    Status {
        /// Output format; `json` prints one object per line for status bars
        /// such as Waybar, and reports a stopped daemon instead of failing
        #[arg(long, value_enum, default_value_t = StatusFormat::Text)]
        format: StatusFormat,
        /// Keep running and print a new line each time the status changes
        /// (requires `--format json`)
        #[arg(long)]
        follow: bool,
        /// Also report the model, inference backend, device, and daemon build
        /// (requires `--format json`)
        #[arg(long)]
        extended: bool,
    },
    /// Reload daemon configuration
    Reload,
    /// Return the most recent transcription
    LastResult,
    /// Transcribe an audio file (WAV) and print the transcript
    Transcribe {
        /// Path to the audio file
        file: PathBuf,
    },
    /// Run system diagnostics
    Doctor {
        /// Send a test line through the configured output method
        #[arg(long)]
        test_type: bool,
    },
    /// Bind the desktop shortcut and wait for you to actually press it
    ///
    /// While a daemon is running it asks the daemon's own listener instead of
    /// opening a second shortcut session; with no daemon it uses its own.
    ShortcutTest,
    /// Manage desktop-session autostart
    Autostart {
        #[command(subcommand)]
        command: AutostartCommand,
    },
    /// Manage the GGUF speech model catalog used for inference
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

#[derive(Subcommand, Clone)]
pub enum ModelCommand {
    /// List catalog models and their local installation status
    List,
    /// Download a catalog model (resumable; verified against a pinned SHA-256)
    Install {
        /// Catalog model id, e.g. "whisper-small-q5_k_m" (see `model list`)
        id: String,
        /// Also select the downloaded model as the active inference model
        #[arg(long = "use")]
        use_after_install: bool,
    },
    /// Delete a locally installed catalog model
    Remove { id: String },
    /// Select which installed catalog model the inference engine loads
    Use { id: String },
}

#[derive(Subcommand, Clone, Copy)]
pub enum DaemonCommand {
    /// Gracefully stop the running daemon
    Stop,
    /// Stop the running daemon, then start a fresh one
    Restart,
}

#[derive(Subcommand, Clone, Copy)]
pub enum AutostartCommand {
    /// Start TongueTyped automatically when the desktop session starts
    Enable,
    /// Stop starting TongueTyped automatically
    Disable,
}

#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusFormat {
    /// The human-readable status report
    Text,
    /// One JSON object per line, in Waybar's custom-module format
    Json,
}
