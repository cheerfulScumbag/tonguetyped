use clap::{Parser, Subcommand};
use tokio::io::AsyncWriteExt;
use tonguetyped::{config, daemon, doctor, ipc};

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
                let config = config::Config::load()?;
                doctor::typing_test(&config)?;
            } else {
                let config = config::Config::load()?;
                let report = doctor::run_doctor(&config).await?;
                println!("compositor:     {}", report.compositor);
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
                    } else if report.model_error.is_some() {
                        "invalid"
                    } else {
                        "not found"
                    }
                );
                println!("model path:     {}", report.model_path);
                if let Some(error) = report.model_error {
                    println!("model error:    {error}");
                }
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
                println!(
                    "shortcut portal: {}",
                    report
                        .shortcut_portal_error
                        .as_deref()
                        .map_or("available", |_| "unavailable")
                );
                if let Some(error) = report.shortcut_portal_error {
                    println!("shortcut error:  {error}");
                }
            }
        }
        Commands::Reload => send_command(ipc::Request::ReloadConfig).await?,
        Commands::LastResult => send_command(ipc::Request::GetLastResult).await?,
    }

    Ok(())
}

async fn send_command(request: ipc::Request) -> anyhow::Result<()> {
    let sock_path = daemon::socket_path()?;

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
            anyhow::bail!(message);
        }
        ipc::Response::Status {
            state,
            activation_mode,
            operation_error,
            activation_error,
        } => {
            println!("state:            {}", state);
            println!("activation mode:  {}", activation_mode);
            if let Some(error) = operation_error {
                println!("last error:       {}", error);
            }
            if let Some(error) = activation_error {
                println!("shortcut error:   {}", error);
            }
        }
        ipc::Response::LastResult { text, timestamp } => {
            println!("result:   {}", text);
            println!("time:     {}", timestamp);
        }
    }

    Ok(())
}
