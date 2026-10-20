use clap::Parser;
use tonguetyped::cli::{
    AutostartCommand, Cli, Commands, DaemonCommand, ModelCommand, StatusFormat,
};
use tonguetyped::{
    activation, autostart, catalog, commands, config, daemon, doctor, ipc, model, setup, status,
    transcribe, tui,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // The dashboard (no subcommand) puts the terminal in raw mode on the
    // alternate screen and owns every byte written to it via ratatui's own
    // cursor-positioned redraws; a `tracing::warn!` line (e.g. inference
    // trying the next backend while loading a model from the Doctor or
    // Model screen) written straight to the same stderr would land askew of
    // whatever ratatui just drew and visibly corrupt the display. Every
    // other subcommand logs to stderr, keeping stdout for its own data - the
    // `transcribe` transcript, `status --format json`, `model list` rows -
    // since the ONNX runtime behind VAD is chatty at `info` level.
    if cli.command.is_none() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_filter())
            .with_writer(std::io::sink)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_filter())
            .with_writer(std::io::stderr)
            .init();
    }

    match cli.command {
        None => tui::run().await?,
        Some(Commands::Setup) => setup::run()?,
        Some(Commands::Daemon { command: None }) => {
            let config = config::Config::load()?;
            if let Err(error) = daemon::run_daemon(config).await {
                // "Another daemon owns the lock" is not a crash to retry: the
                // systemd units skip this exit status, so the unit fails once
                // instead of restarting forever behind a manual daemon. Every
                // other failure keeps anyhow's ordinary exit-1 reporting.
                match daemon::exit_code_for_error(&error) {
                    daemon::EXIT_DAEMON_ALREADY_RUNNING => {
                        eprintln!("Error: {error}");
                        std::process::exit(daemon::EXIT_DAEMON_ALREADY_RUNNING);
                    }
                    _ => return Err(error),
                }
            }
        }
        Some(Commands::Daemon {
            command: Some(DaemonCommand::Stop),
        }) => {
            commands::stop_daemon().await?;
            println!("daemon stopped");
        }
        Some(Commands::Daemon {
            command: Some(DaemonCommand::Restart),
        }) => {
            commands::restart_daemon().await?;
            println!("daemon restarted");
        }
        Some(Commands::Start) => {
            send_command(ipc::Request::Start).await?;
        }
        Some(Commands::Stop) => {
            send_command(ipc::Request::Stop).await?;
        }
        Some(Commands::Toggle) => {
            send_command(ipc::Request::Toggle).await?;
        }
        Some(Commands::Cancel) => {
            send_command(ipc::Request::Cancel).await?;
        }
        Some(Commands::Status {
            format: StatusFormat::Text,
            follow,
            extended,
        }) => {
            if follow || extended {
                anyhow::bail!("--follow and --extended require --format json");
            }
            send_command(ipc::Request::Status).await?;
        }
        Some(Commands::Status {
            format: StatusFormat::Json,
            follow: false,
            extended,
        }) => status::print_json(extended).await?,
        Some(Commands::Status {
            format: StatusFormat::Json,
            follow: true,
            extended,
        }) => status::follow_json(extended).await?,
        Some(Commands::Doctor { test_type }) => {
            if test_type {
                let config = config::Config::load()?;
                println!("{}", doctor::typing_test(&config)?);
            } else {
                let config = config::Config::load()?;
                let report = doctor::run_doctor(&config).await?;
                for output in commands::format_doctor_lines(&report) {
                    println!("{}", output.text);
                }
            }
        }
        Some(Commands::ShortcutTest) => {
            println!("Press your shortcut now...");
            match commands::test_shortcut().await {
                Ok(activation::ShortcutTestOutcome::Pressed) => {
                    println!("Shortcut press detected - the shortcut listener received it.");
                }
                Ok(activation::ShortcutTestOutcome::TimedOut) => {
                    anyhow::bail!(
                        "no shortcut press detected within 15s; set a shortcut in your \
                         desktop's own shortcut dialog (or the dashboard's Shortcut screen) \
                         and try again"
                    );
                }
                Err(error) => {
                    anyhow::bail!("shortcut binding test failed: {error}");
                }
            }
        }
        Some(Commands::Autostart { command }) => match command {
            AutostartCommand::Enable => {
                autostart::update(true)?;
                println!("autostart enabled");
            }
            AutostartCommand::Disable => {
                autostart::update(false)?;
                println!("autostart disabled");
            }
        },
        Some(Commands::Reload) => send_command(ipc::Request::ReloadConfig).await?,
        Some(Commands::LastResult) => send_command(ipc::Request::GetLastResult).await?,
        Some(Commands::Transcribe { file }) => {
            let config = config::Config::load()?;
            let transcription = transcribe::transcribe_file(&file, &config)?;
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            transcribe::write_transcript(&transcription, &mut stdout.lock(), &mut stderr.lock())?;
        }
        Some(Commands::Model { command }) => run_model_command(command).await?,
    }

    Ok(())
}

fn tracing_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
}

async fn run_model_command(command: ModelCommand) -> anyhow::Result<()> {
    match command {
        ModelCommand::List => {
            let config = config::Config::load()?;
            println!(
                "{:<32} {:<10} {:>10}  {:<11}  {:<10} {:<12} {:<14}  license",
                "MODEL ID", "QUANT", "SIZE", "STATUS", "STREAMING", "FAMILY", "LANGUAGES"
            );
            for entry in catalog::ENTRIES {
                let installed = catalog::is_installed(entry.id);
                let status = match (installed, entry.id == config.model.active_model) {
                    (true, true) => "active",
                    (true, false) => "installed",
                    (false, _) => "-",
                };
                let info = entry.info();
                // Only streaming models can produce live text while you speak
                // (shown when a style's live-text look is chosen). The tag
                // carries the language only for the English-only ports, so a
                // multilingual streaming model is not mislabelled.
                let streaming = match (info.supports_streaming(), info.is_english_only()) {
                    (false, _) => "-",
                    (true, true) => "live (en)",
                    (true, false) => "live",
                };
                println!(
                    "{:<32} {:<10} {:>10}  {:<11}  {:<10} {:<12} {:<14}  {}",
                    entry.id,
                    entry.quant,
                    commands::human_size(entry.size_bytes),
                    status,
                    streaming,
                    info.family_label(),
                    info.language_summary(),
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
            let (path, outcome) = manager.install_catalog_model(&id, None).await?;
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
                commands::select_model(&id)?;
                println!("selected {id} as the active inference model");
            }
        }
        ModelCommand::Remove { id } => {
            if catalog::find(&id).is_none() {
                anyhow::bail!("unknown catalog model: {id} (see `tonguetyped model list`)");
            }
            let config = config::Config::load()?;
            if config.model.active_model == id {
                anyhow::bail!(
                    "{id} is the active model; run `tonguetyped model use <other-id>` first"
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
            commands::select_model(&id)?;
            println!("selected {id} as the active inference model");
        }
    }
    Ok(())
}

async fn send_command(request: ipc::Request) -> anyhow::Result<()> {
    let response = commands::send_ipc(request).await?;
    if let ipc::Response::Error { message } = &response {
        anyhow::bail!(message.clone());
    }
    for output in commands::format_response_lines(&response) {
        println!("{}", output.text);
    }
    Ok(())
}
