use clap::{Parser, Subcommand};
use tokio::io::AsyncWriteExt;
use tonguetyped::{activation, autostart, catalog, config, daemon, doctor, ipc, model, setup};

#[derive(Parser)]
#[command(name = "tonguetyped", version, about = "Linux dictation application")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    /// Manage the GGUF speech model catalog used by the GPU inference backend
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

#[derive(Subcommand)]
enum ModelCommand {
    /// List catalog models and their local installation status
    List,
    /// Download a catalog model (resumable; verified against a pinned SHA-256)
    Install {
        /// Catalog model id, e.g. "whisper-small-q5_k_m" (see `model list`)
        id: String,
        /// Also select the downloaded model as the active GPU inference model
        #[arg(long = "use")]
        use_after_install: bool,
    },
    /// Delete a locally installed catalog model
    Remove { id: String },
    /// Select which installed catalog model the GPU backend loads
    Use { id: String },
}

#[derive(Subcommand)]
enum AutostartCommand {
    /// Start TongueTyped automatically when the desktop session starts
    Enable,
    /// Stop starting TongueTyped automatically
    Disable,
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
        Commands::Setup => setup::run()?,
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
                    "overlay:        {}",
                    if report.layer_shell_overlay_available {
                        "layer-shell available"
                    } else {
                        "layer-shell unavailable (falls back to OSD/notification)"
                    }
                );
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
                println!("model id:       {}", report.model_id);
                println!("model path:     {}", report.model_path);
                println!(
                    "gpu model:      {} ({})",
                    report.gpu_model_id,
                    if report.gpu_model_installed {
                        "installed"
                    } else {
                        "not installed"
                    }
                );
                println!("backend:        {}", report.inference_backend);
                println!("device:         {}", report.inference_device);
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
                    match report.shortcut_status {
                        None => "not tested (run `tonguetyped shortcut-test`)",
                        Some(ipc::ShortcutStatus::Initializing) => "initializing",
                        Some(ipc::ShortcutStatus::Available) => "available",
                        Some(ipc::ShortcutStatus::Failed) => "unavailable",
                    }
                );
                if let Some(error) = report.shortcut_portal_error {
                    println!("shortcut error:  {error}");
                }
            }
        }
        Commands::ShortcutTest => {
            let config = config::Config::load()?;
            if let Some(error) = activation::test_shortcut_binding(&config.activation.keybind).await
            {
                anyhow::bail!("shortcut binding test failed: {error}");
            }
            println!("shortcut binding available");
        }
        Commands::Autostart { command } => match command {
            AutostartCommand::Enable => {
                autostart::update(true)?;
                println!("autostart enabled");
            }
            AutostartCommand::Disable => {
                autostart::update(false)?;
                println!("autostart disabled");
            }
        },
        Commands::Reload => send_command(ipc::Request::ReloadConfig).await?,
        Commands::LastResult => send_command(ipc::Request::GetLastResult).await?,
        Commands::Model { command } => run_model_command(command).await?,
    }

    Ok(())
}

async fn run_model_command(command: ModelCommand) -> anyhow::Result<()> {
    match command {
        ModelCommand::List => {
            let config = config::Config::load()?;
            let cpu_id = &config.model.selected;
            let cpu_path = model::ModelCatalog::model_path(cpu_id)?;
            println!(
                "CPU model (fixed default): {cpu_id} [{}]",
                if cpu_path.exists() {
                    "installed"
                } else {
                    "not installed"
                }
            );
            println!();
            println!(
                "{:<32} {:<10} {:>10}  {:<11}  license",
                "GPU CATALOG MODEL ID", "QUANT", "SIZE", "STATUS"
            );
            for entry in catalog::ENTRIES {
                let installed = catalog::is_installed(entry.id);
                let status = match (installed, entry.id == config.model.gpu_model) {
                    (true, true) => "active",
                    (true, false) => "installed",
                    (false, _) => "-",
                };
                println!(
                    "{:<32} {:<10} {:>10}  {:<11}  {}",
                    entry.id,
                    entry.quant,
                    human_size(entry.size_bytes),
                    status,
                    entry.license_spdx,
                );
            }
        }
        ModelCommand::Install {
            id,
            use_after_install,
        } => {
            if catalog::find(&id).is_none() {
                anyhow::bail!("unknown catalog model: {id} (see `tonguetyped model list`)");
            }
            let manager = model::DownloadManager::new()?;
            let (path, outcome) = manager.install_catalog_model(&id).await?;
            match outcome {
                model::DownloadOutcome::AlreadyInstalled => {
                    println!("{id} is already installed at {}", path.display())
                }
                model::DownloadOutcome::Resumed => {
                    println!("resumed and verified {id} at {}", path.display())
                }
                model::DownloadOutcome::Fresh => {
                    println!("installed and verified {id} at {}", path.display())
                }
            }
            if use_after_install {
                select_gpu_model(&id)?;
            }
        }
        ModelCommand::Remove { id } => {
            if catalog::find(&id).is_none() {
                anyhow::bail!("unknown catalog model: {id} (see `tonguetyped model list`)");
            }
            let config = config::Config::load()?;
            if config.model.gpu_model == id {
                anyhow::bail!(
                    "{id} is the active GPU model; run `tonguetyped model use <other-id>` first"
                );
            }
            if model::DownloadManager::remove_catalog_model(&id)? {
                println!("removed {id}");
            } else {
                println!("{id} is not installed");
            }
        }
        ModelCommand::Use { id } => {
            if catalog::find(&id).is_none() {
                anyhow::bail!("unknown catalog model: {id} (see `tonguetyped model list`)");
            }
            if !catalog::is_installed(&id) {
                anyhow::bail!("{id} is not installed; run `tonguetyped model install {id}` first");
            }
            select_gpu_model(&id)?;
        }
    }
    Ok(())
}

fn select_gpu_model(id: &str) -> anyhow::Result<()> {
    let mut config = config::Config::load()?;
    config.model.gpu_model = id.to_string();
    config.save()?;
    println!("selected {id} as the active GPU inference model");
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
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
            shortcut_status,
            activation_error,
        } => {
            println!("state:            {}", state);
            println!("activation mode:  {}", activation_mode);
            println!(
                "shortcut status:  {}",
                match shortcut_status {
                    ipc::ShortcutStatus::Initializing => "initializing",
                    ipc::ShortcutStatus::Available => "available",
                    ipc::ShortcutStatus::Failed => "failed",
                }
            );
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
