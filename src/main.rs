mod audio;
mod config;
mod coordinator;
mod daemon;
mod doctor;
mod history;
mod inference;
mod ipc;
mod model;
mod output;
mod vad;

use clap::{Parser, Subcommand};
use tokio::io::AsyncWriteExt;

#[derive(Parser)]
#[command(name = "tonguetyped", version, about = "Linux dictation application")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    /// Run system diagnostics
    Doctor {
        /// Run explicit typing test
        #[arg(long)]
        test_type: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => {
            let config = config::Config::load()?;
            daemon::run_daemon(config).await?;
        }
        Commands::Start => {
            send_command(ipc::Request::Start).await?;
        }
        Commands::Stop => {
            send_command(ipc::Request::Stop).await?;
        }
        Commands::Toggle => {
            send_command(ipc::Request::Toggle).await?;
        }
        Commands::Cancel => {
            send_command(ipc::Request::Cancel).await?;
        }
        Commands::Status => {
            send_command(ipc::Request::Status).await?;
        }
        Commands::Doctor { test_type } => {
            if test_type {
                doctor::typing_test();
            } else {
                let config = config::Config::load().unwrap_or_default();
                let report = doctor::run_doctor(&config.model.selected);
                println!("compositor:     {}", report.compositor);
                println!("desktop:        {}", report.desktop);
                println!(
                    "audio:          {}",
                    if report.audio_available {
                        "available"
                    } else {
                        "unavailable"
                    }
                );
                println!("audio devices:  {}", report.audio_devices.join(", "));
                println!(
                    "model:          {}",
                    if report.model_ready {
                        "ready"
                    } else {
                        "not found"
                    }
                );
                println!("model path:     {}", report.model_path);
                println!("socket:         {}", report.socket_health);
                println!(
                    "helpers:        {}",
                    if report.helpers_found.is_empty() {
                        "none".to_string()
                    } else {
                        report.helpers_found.join(", ")
                    }
                );
                println!(
                    "output method:  {}",
                    if report.output_method_available {
                        "available"
                    } else {
                        "unavailable (none mode only)"
                    }
                );
            }
        }
    }

    Ok(())
}

async fn send_command(request: ipc::Request) -> anyhow::Result<()> {
    let sock_path = daemon::socket_path();

    if !sock_path.exists() {
        anyhow::bail!(
            "daemon is not running (no socket at {})",
            sock_path.display()
        );
    }

    let stream = tokio::net::UnixStream::connect(&sock_path).await?;
    let (reader, mut writer) = stream.into_split();

    let frame = ipc::encode_frame(&request)?;
    writer.write_all(frame.as_bytes()).await?;

    let mut reader = tokio::io::BufReader::new(reader);
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await?;

    let response: ipc::Response = ipc::decode_frame(&line)?;

    match response {
        ipc::Response::Ok => println!("ok"),
        ipc::Response::Busy => println!("busy"),
        ipc::Response::RecordingStarted => println!("recording started"),
        ipc::Response::RecordingStopped => println!("recording stopped"),
        ipc::Response::Cancelled => println!("cancelled"),
        ipc::Response::Error { message } => {
            eprintln!("error: {}", message);
        }
        ipc::Response::Status {
            state,
            recording,
            processing,
            activation_mode,
        } => {
            println!("state:            {}", state);
            println!("recording:        {}", recording);
            println!("processing:       {}", processing);
            println!("activation mode:  {}", activation_mode);
        }
        ipc::Response::DoctorResult {
            compositor,
            desktop,
            audio_available,
            model_ready,
            socket_health,
            helpers_found,
        } => {
            println!("compositor:     {}", compositor);
            println!("desktop:        {}", desktop);
            println!(
                "audio:          {}",
                if audio_available {
                    "available"
                } else {
                    "unavailable"
                }
            );
            println!(
                "model:          {}",
                if model_ready {
                    "ready"
                } else {
                    "not found"
                }
            );
            println!("socket:         {}", socket_health);
            println!(
                "helpers:        {}",
                if helpers_found.is_empty() {
                    "none".to_string()
                } else {
                    helpers_found.join(", ")
                }
            );
        }
        ipc::Response::LastResult { text, timestamp } => {
            println!("result:   {}", text);
            println!("time:     {}", timestamp);
        }
    }

    Ok(())
}