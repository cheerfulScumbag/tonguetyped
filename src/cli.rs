//! Clap argument definitions, kept as a library module (rather than private to
//! `main.rs`) so `tui::home` can introspect the exact same subcommand names
//! and `about` text that `--help` renders (`Cli::command()`), instead of
//! maintaining a second, driftable copy of each command's name/description
//! for the dashboard's first page.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tonguetyped", version, about = "Linux dictation application")]
pub struct Cli {
    /// Running with no subcommand opens the interactive dashboard
    /// (`crate::tui::run`) instead of erroring - every subcommand below
    /// keeps its exact existing CLI behavior.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Configure TongueTyped interactively
    Setup,
    /// Start the daemon process
    Daemon,
    /// Start a new recording
    Start,
    /// Stop the current recording
    Stop,
    /// Toggle recording on/off
    Toggle,
    /// Cancel current recording or processing
    Cancel,
    /// Get daemon status
    Status,
    /// Reload daemon configuration
    Reload,
    /// Return the most recent transcription
    LastResult,
    /// Run system diagnostics
    Doctor {
        /// Run explicit typing test
        #[arg(long)]
        test_type: bool,
    },
    /// Interactively validate desktop shortcut authorization and binding
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
pub enum AutostartCommand {
    /// Start TongueTyped automatically when the desktop session starts
    Enable,
    /// Stop starting TongueTyped automatically
    Disable,
}
