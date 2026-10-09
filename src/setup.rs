use crate::config::{ActivationMode, Config, OutputMethod};
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::{Arc, Mutex};

mod console;

/// The speech model the daemon needs a file for before dictation will work -
/// the catalog entry `setup`'s "Speech model" step selects, requested on
/// whichever backend (CPU, or an accelerator) ends up active. Missing it
/// forces `InferenceEngine::load` to fail outright (no separate CPU-only
/// fallback file exists anymore - see `src/inference.rs`), so the console
/// step below always fetches it when absent.
pub(crate) struct ModelRequirement {
    pub label: String,
    pub already_present: bool,
    id: String,
}

pub(crate) fn model_requirements(config: &Config) -> Vec<ModelRequirement> {
    vec![ModelRequirement {
        label: config.model.active_model.clone(),
        already_present: crate::catalog::is_installed(&config.model.active_model),
        id: config.model.active_model.clone(),
    }]
}

/// Shared state a background provisioning thread reports into, polled by the
/// setup console's render loop (same shape as `console::ConsoleState`'s
/// microphone level meter, which polls an `Arc<Mutex<f32>>` the same way).
pub(crate) struct ProvisionHandle {
    pub progress: Arc<Mutex<(u64, u64)>>,
    pub result: Arc<Mutex<Option<Result<crate::model::DownloadOutcome, String>>>>,
}

/// Spawns a background thread that fetches `requirement` and reports its
/// progress through the returned handle. Runs its own single-threaded tokio
/// runtime (matching `feedback.rs`'s D-Bus calls) rather than borrowing the
/// caller's, since the setup console's render loop is synchronous and must
/// keep polling `ProvisionHandle` while this download is in flight.
pub(crate) fn provision_model_async(requirement: ModelRequirement) -> ProvisionHandle {
    let progress = Arc::new(Mutex::new((0u64, 0u64)));
    let result = Arc::new(Mutex::new(None));
    let progress_for_thread = progress.clone();
    let result_for_thread = result.clone();
    std::thread::spawn(move || {
        let outcome = fetch_requirement(requirement.id, progress_for_thread);
        if let Ok(mut guard) = result_for_thread.lock() {
            *guard = Some(outcome.map_err(|error| error.to_string()));
        }
    });
    ProvisionHandle { progress, result }
}

fn fetch_requirement(
    id: String,
    progress: Arc<Mutex<(u64, u64)>>,
) -> anyhow::Result<crate::model::DownloadOutcome> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let manager = crate::model::DownloadManager::new()?;
        let on_progress: crate::model::ProgressCallback = Arc::new(move |downloaded, total| {
            if let Ok(mut guard) = progress.lock() {
                *guard = (downloaded, total);
            }
        });
        let (_, outcome) = manager
            .install_catalog_model(&id, Some(on_progress))
            .await?;
        Ok(outcome)
    })
}

/// Shared state a background shortcut-test thread reports into, polled by
/// the dashboard's Shortcut screen - same shape as `ReconfigureHandle` above.
pub(crate) struct ShortcutTestHandle {
    pub result: Arc<Mutex<Option<Result<crate::activation::ShortcutTestOutcome, String>>>>,
}

/// Spawns a background thread that drives `activation::test_shortcut_binding`
/// to completion and reports its outcome through the returned handle. Runs
/// its own single-threaded tokio runtime, same as `reconfigure_shortcut_async`
/// below.
pub(crate) fn shortcut_test_async() -> ShortcutTestHandle {
    let result = Arc::new(Mutex::new(None));
    let result_for_thread = result.clone();
    std::thread::spawn(move || {
        let outcome = run_shortcut_test();
        if let Ok(mut guard) = result_for_thread.lock() {
            *guard = Some(outcome);
        }
    });
    ShortcutTestHandle { result }
}

fn run_shortcut_test() -> Result<crate::activation::ShortcutTestOutcome, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(crate::activation::test_shortcut_binding())
}

/// Shared state a background shortcut-reconfigure thread reports into,
/// polled by the setup console's render loop - same shape as
/// `ProvisionHandle` above and `console::ConsoleState`'s microphone level
/// meter, needed for the same reason: the console's render loop is
/// synchronous and must keep drawing while the portal's native "press your
/// new shortcut" dialog (`activation::reconfigure_shortcut`) is open.
pub(crate) struct ReconfigureHandle {
    pub result: Arc<Mutex<Option<Result<String, String>>>>,
}

/// Spawns a background thread that drives `activation::reconfigure_shortcut`
/// to completion and reports its outcome through the returned handle. Runs
/// its own single-threaded tokio runtime, same as `fetch_requirement` below,
/// rather than borrowing the caller's (the console has none to borrow).
pub(crate) fn reconfigure_shortcut_async() -> ReconfigureHandle {
    let result = Arc::new(Mutex::new(None));
    let result_for_thread = result.clone();
    std::thread::spawn(move || {
        let outcome = run_reconfigure();
        if let Ok(mut guard) = result_for_thread.lock() {
            *guard = Some(outcome);
        }
    });
    ReconfigureHandle { result }
}

fn run_reconfigure() -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(crate::activation::reconfigure_shortcut())
        .map(|outcome| outcome.trigger_description)
}

pub(crate) fn outcome_label(outcome: crate::model::DownloadOutcome) -> &'static str {
    match outcome {
        crate::model::DownloadOutcome::AlreadyInstalled => "already installed",
        crate::model::DownloadOutcome::Resumed => "resumed and verified",
        crate::model::DownloadOutcome::Fresh => "downloaded and verified",
    }
}

#[derive(Debug)]
pub(crate) struct Capabilities {
    pub(crate) microphones: Vec<(String, String)>,
    pub(crate) typing_backends: Vec<String>,
    pub(crate) inference_backends: Vec<crate::inference::BackendChoice>,
}

impl Capabilities {
    pub(crate) fn discover() -> anyhow::Result<Self> {
        let mut microphones = vec![(
            "default".to_string(),
            "System default microphone".to_string(),
        )];
        for device in crate::audio::list_devices()? {
            if !microphones.iter().any(|(value, _)| value == &device.id) {
                let detail = if device.is_default {
                    format!("{} (current default)", device.name)
                } else {
                    device.name.clone()
                };
                microphones.push((device.id, detail));
            }
        }
        Ok(Self {
            microphones,
            typing_backends: crate::output::list_available_backends(),
            inference_backends: crate::inference::backend_choices(),
        })
    }

    /// Discovery for surfaces (the dashboard) that must still open when audio
    /// enumeration fails: falls back to just the system-default microphone,
    /// whose own preview error is then surfaced in the microphone screen.
    pub(crate) fn discover_or_default() -> Self {
        Self::discover().unwrap_or_else(|error| {
            tracing::warn!("audio device discovery failed: {error}");
            Self {
                microphones: vec![(
                    "default".to_string(),
                    "System default microphone".to_string(),
                )],
                typing_backends: crate::output::list_available_backends(),
                inference_backends: crate::inference::backend_choices(),
            }
        })
    }

    pub(crate) fn microphone_index(&self, selected: &str) -> usize {
        self.microphone_position(selected).unwrap_or(0)
    }

    /// The display label for `selected`, resolving both stable device ids and
    /// legacy stored names the same way `microphone_index` does, and falling
    /// back to the stored value for a device that is no longer present.
    pub(crate) fn microphone_label(&self, selected: &str) -> String {
        self.microphone_position(selected)
            .map(|index| self.microphones[index].1.clone())
            .unwrap_or_else(|| selected.to_string())
    }

    fn microphone_position(&self, selected: &str) -> Option<usize> {
        self.microphones.iter().position(|(value, label)| {
            value == selected
                || label == selected
                || label
                    .strip_suffix(" (current default)")
                    .is_some_and(|name| name == selected)
        })
    }

    /// Whether any typing helper actually works here. The configuration UIs
    /// use this to keep "type into the focused application" from ever being
    /// selected on a machine where it would fail (see
    /// `crate::output::list_available_backends`, which populates
    /// `typing_backends`).
    pub(crate) fn has_type_backend(&self) -> bool {
        !self.typing_backends.is_empty()
    }

    /// The install warning both configuration UIs show when
    /// `has_type_backend` is false, or `None` otherwise.
    pub(crate) fn typing_helper_warning(&self) -> Option<&'static str> {
        (!self.has_type_backend()).then(crate::output::typing_helper_warning)
    }

    /// The transcript-output choices, in the order both configuration UIs
    /// offer them: keep-in-app first, then type-into-the-focused-application.
    /// The type choice is always listed - the screens explain its
    /// unavailability via `typing_helper_warning` when no helper works,
    /// rather than silently dropping it - but it can only be applied when
    /// `has_type_backend` is true.
    pub(crate) fn output_labels(&self) -> Vec<String> {
        vec![
            "Keep transcripts in TongueTyped".to_string(),
            "Type into the focused application".to_string(),
        ]
    }

    /// The typing-backend choices: explicit automatic selection first, then
    /// every backend detected on this machine.
    pub(crate) fn typing_backend_values(&self) -> Vec<String> {
        let mut values = vec!["auto".to_string()];
        values.extend(self.typing_backends.iter().cloned());
        values
    }
}

struct Ui {
    color: bool,
}

impl Ui {
    fn title(&self, output: &mut impl Write, text: &str) -> io::Result<()> {
        if self.color {
            writeln!(output, "\x1b[1;36m{text}\x1b[0m")
        } else {
            writeln!(output, "{text}")
        }
    }

    fn section(&self, output: &mut impl Write, step: usize, text: &str) -> io::Result<()> {
        if self.color {
            writeln!(output, "\n\x1b[1m[{step}/6] {text}\x1b[0m")
        } else {
            writeln!(output, "\n[{step}/6] {text}")
        }
    }

    fn success(&self, output: &mut impl Write, text: &str) -> io::Result<()> {
        if self.color {
            writeln!(output, "\n\x1b[1;32m{text}\x1b[0m")
        } else {
            writeln!(output, "\n{text}")
        }
    }
}

pub(crate) enum SetupOutcome {
    Saved,
    Cancelled,
}

/// Runs the interactive Ratatui setup console directly (`src/setup/console.rs`),
/// bypassing the `stdin`/`stdout`-is-a-terminal branch in `run()` below since
/// the dashboard (`crate::tui`) already knows it is attached to a real
/// terminal when it offers "setup" as an action - this is the same console
/// code `tonguetyped setup` uses, reused rather than reimplemented.
pub(crate) fn run_console() -> anyhow::Result<SetupOutcome> {
    let capabilities = Capabilities::discover()?;
    console::run(capabilities)
}

pub fn run() -> anyhow::Result<()> {
    let capabilities = Capabilities::discover()?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Piped/non-interactive stdin or stdout (scripts, tests, CI) falls back to the
    // line-based flow below so `tonguetyped setup` stays scriptable with plain
    // newline-separated answers.
    if stdin.is_terminal() && stdout.is_terminal() {
        let outcome = console::run(capabilities)?;
        match outcome {
            SetupOutcome::Cancelled => {
                writeln!(stdout, "\nSetup cancelled. No changes were made.")?;
            }
            SetupOutcome::Saved => {
                writeln!(stdout, "\nConfiguration saved.")?;
                writeln!(
                    stdout,
                    "Run `tonguetyped doctor` to check the installation."
                )?;
            }
        }
        return Ok(());
    }

    let mut stderr = io::stderr();
    let ui = Ui {
        color: stdout.is_terminal() && std::env::var_os("NO_COLOR").is_none(),
    };
    let outcome = configure(
        &mut stdin.lock(),
        &mut stdout,
        &mut stderr,
        capabilities,
        ui,
    )?;
    if matches!(outcome, SetupOutcome::Cancelled) {
        writeln!(stdout, "\nSetup cancelled. No changes were made.")?;
    }
    Ok(())
}

fn configure(
    input: &mut impl BufRead,
    output: &mut impl Write,
    errors: &mut impl Write,
    capabilities: Capabilities,
    ui: Ui,
) -> anyhow::Result<SetupOutcome> {
    let mut config = Config::reload()?;
    ui.title(output, "TongueTyped setup")?;
    writeln!(
        output,
        "Choose a number, press Enter for the default, or enter q to cancel."
    )?;

    ui.section(output, 1, "Speech model")?;
    let model_labels: Vec<String> = crate::catalog::ENTRIES
        .iter()
        .map(|entry| entry.id.to_string())
        .collect();
    let model_default = crate::catalog::ENTRIES
        .iter()
        .position(|entry| entry.id == config.model.active_model)
        .unwrap_or(0);
    let Some(model) = choose(input, output, errors, &model_labels, model_default)? else {
        return Ok(SetupOutcome::Cancelled);
    };
    config.model.active_model = crate::catalog::ENTRIES[model].id.to_string();

    ui.section(output, 2, "Microphone")?;
    if capabilities.microphones.len() == 1 {
        writeln!(output, "Only the system default microphone is available.")?;
    }
    let microphone_labels: Vec<String> = capabilities
        .microphones
        .iter()
        .map(|(_, label)| label.clone())
        .collect();
    let microphone_default = capabilities.microphone_index(&config.audio.microphone);
    let Some(microphone) = choose(
        input,
        output,
        errors,
        &microphone_labels,
        microphone_default,
    )?
    else {
        return Ok(SetupOutcome::Cancelled);
    };
    config.audio.microphone = capabilities.microphones[microphone].0.clone();

    ui.section(output, 3, "Activation")?;
    let activation_labels = [
        "Hold the shortcut while speaking".to_string(),
        "Press once to start and again to stop".to_string(),
    ];
    let activation_default = usize::from(config.activation.mode == ActivationMode::Toggle);
    let Some(activation) = choose(
        input,
        output,
        errors,
        &activation_labels,
        activation_default,
    )?
    else {
        return Ok(SetupOutcome::Cancelled);
    };
    config.activation.mode = if activation == 0 {
        ActivationMode::Hold
    } else {
        ActivationMode::Toggle
    };

    ui.section(output, 4, "Shortcut")?;
    writeln!(
        output,
        "TongueTyped no longer picks a shortcut for you. After setup, run `tonguetyped` and \
         open the Shortcut screen to choose one in your desktop's own shortcut dialog, or set \
         it from your desktop's keyboard settings."
    )?;

    ui.section(output, 5, "Transcript output")?;
    let output_labels = capabilities.output_labels();
    if let Some(warning) = capabilities.typing_helper_warning() {
        writeln!(output, "{warning}")?;
    }
    let output_default =
        usize::from(config.output.method == OutputMethod::Type && capabilities.has_type_backend());
    // Choosing "type" with no working helper is refused with the same warning
    // rather than silently saved into a configuration that can never type.
    let output_method = loop {
        let Some(choice) = choose(input, output, errors, &output_labels, output_default)? else {
            return Ok(SetupOutcome::Cancelled);
        };
        if choice == 1 && !capabilities.has_type_backend() {
            writeln!(
                errors,
                "{}",
                capabilities
                    .typing_helper_warning()
                    .expect("warning exists when no helper does")
            )?;
            continue;
        }
        break choice;
    };
    if output_method == 0 {
        config.output.method = OutputMethod::None;
        config.output.typing_backend = "auto".to_string();
    } else {
        config.output.method = OutputMethod::Type;
        let mut backend_values = vec!["auto".to_string()];
        backend_values.extend(capabilities.typing_backends);
        let mut backend_labels = vec!["Automatic (recommended)".to_string()];
        backend_labels.extend(
            backend_values
                .iter()
                .skip(1)
                .map(|backend| backend.to_string()),
        );
        writeln!(output, "\nTyping backend")?;
        let backend_default = backend_values
            .iter()
            .position(|backend| backend == &config.output.typing_backend)
            .unwrap_or(0);
        let Some(backend) = choose(input, output, errors, &backend_labels, backend_default)? else {
            return Ok(SetupOutcome::Cancelled);
        };
        config.output.typing_backend = backend_values[backend].clone();
    }

    ui.section(output, 6, "Startup")?;
    let startup_labels = [
        "Start manually".to_string(),
        "Start TongueTyped when you sign in".to_string(),
    ];
    let Some(startup) = choose(
        input,
        output,
        errors,
        &startup_labels,
        usize::from(config.startup.autostart),
    )?
    else {
        return Ok(SetupOutcome::Cancelled);
    };
    config.startup.autostart = startup == 1;

    config.validate()?;
    writeln!(output, "\nConfiguration ready:")?;
    writeln!(output, "  Model:       {}", config.model.active_model)?;
    writeln!(output, "  Microphone:  {}", config.audio.microphone)?;
    writeln!(
        output,
        "  Activation:  {} with {}",
        config.activation.mode,
        if config.activation.keybind.is_empty() {
            "no shortcut set yet".to_string()
        } else {
            crate::activation::keybind_label(&config.activation.keybind)
        }
    )?;
    writeln!(output, "  Output:      {}", config.output.method)?;
    writeln!(
        output,
        "  Autostart:   {}",
        if config.startup.autostart {
            "yes"
        } else {
            "no"
        }
    )?;
    let Some(confirm) = confirm(input, output, errors, "Write this configuration?", true)? else {
        return Ok(SetupOutcome::Cancelled);
    };
    if !confirm {
        return Ok(SetupOutcome::Cancelled);
    }

    crate::autostart::save_configuration(&config)?;
    ui.success(output, "Configuration saved.")?;
    writeln!(
        output,
        "Run `tonguetyped doctor` to check the installation."
    )?;
    Ok(SetupOutcome::Saved)
}

fn choose(
    input: &mut impl BufRead,
    output: &mut impl Write,
    errors: &mut impl Write,
    labels: &[String],
    default: usize,
) -> anyhow::Result<Option<usize>> {
    for (index, label) in labels.iter().enumerate() {
        let marker = if index == default { " (default)" } else { "" };
        writeln!(output, "  {}. {}{}", index + 1, label, marker)?;
    }
    loop {
        write!(output, "> ")?;
        output.flush()?;
        let Some(answer) = read_answer(input)? else {
            return Ok(None);
        };
        if is_cancel(&answer) {
            return Ok(None);
        }
        if answer.is_empty() {
            return Ok(Some(default));
        }
        if let Ok(index) = answer.parse::<usize>() {
            if (1..=labels.len()).contains(&index) {
                return Ok(Some(index - 1));
            }
        }
        writeln!(
            errors,
            "Invalid choice. Enter a number from 1 to {}.",
            labels.len()
        )?;
    }
}

fn confirm(
    input: &mut impl BufRead,
    output: &mut impl Write,
    errors: &mut impl Write,
    prompt: &str,
    default: bool,
) -> anyhow::Result<Option<bool>> {
    loop {
        write!(
            output,
            "{prompt} {} ",
            if default { "[Y/n]" } else { "[y/N]" }
        )?;
        output.flush()?;
        let Some(answer) = read_answer(input)? else {
            return Ok(None);
        };
        if is_cancel(&answer) {
            return Ok(None);
        }
        match answer.to_ascii_lowercase().as_str() {
            "" => return Ok(Some(default)),
            "y" | "yes" => return Ok(Some(true)),
            "n" | "no" => return Ok(Some(false)),
            _ => writeln!(errors, "Invalid answer. Enter y or n.")?,
        }
    }
}

fn read_answer(input: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn is_cancel(answer: &str) -> bool {
    matches!(
        answer.to_ascii_lowercase().as_str(),
        "q" | "quit" | "cancel"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `model_requirements` resolves paths through `directories::BaseDirs`,
    // which reads `XDG_DATA_HOME` - a process-wide env var `cargo test`'s
    // default parallel test threads would otherwise race on (same shape as
    // `overlay.rs`'s `WAYLAND_DISPLAY_LOCK` guarding `WAYLAND_DISPLAY`).
    static XDG_DATA_HOME_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn model_requirements_flips_to_present_once_the_file_exists() {
        let _guard = XDG_DATA_HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "tonguetyped-model-requirements-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous = std::env::var("XDG_DATA_HOME").ok();
        std::env::set_var("XDG_DATA_HOME", &root);

        let config = Config::default();
        let requirements = model_requirements(&config);
        assert_eq!(requirements[0].label, config.model.active_model);
        assert!(!requirements[0].already_present);

        let model_path = crate::catalog::model_path(&config.model.active_model).unwrap();
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"stub").unwrap();
        assert!(model_requirements(&config)[0].already_present);

        match previous {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn menu_retries_invalid_input_and_accepts_cancellation() {
        let mut input = "9\n2\n".as_bytes();
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let selected = choose(
            &mut input,
            &mut output,
            &mut errors,
            &["one".into(), "two".into()],
            0,
        )
        .unwrap();
        assert_eq!(selected, Some(1));
        assert!(String::from_utf8(errors)
            .unwrap()
            .contains("Invalid choice"));

        let mut input = "cancel\n".as_bytes();
        assert_eq!(
            choose(
                &mut input,
                &mut Vec::new(),
                &mut Vec::new(),
                &["one".into()],
                0,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn microphone_selection_accepts_stable_ids_and_legacy_names() {
        let capabilities = Capabilities {
            microphones: vec![
                ("default".into(), "System default microphone".into()),
                ("pipewire:usb-source".into(), "USB Microphone".into()),
                (
                    "pipewire:default-source".into(),
                    "Built-in Audio (current default)".into(),
                ),
            ],
            typing_backends: Vec::new(),
            inference_backends: Vec::new(),
        };

        assert_eq!(capabilities.microphone_index("pipewire:usb-source"), 1);
        assert_eq!(capabilities.microphone_index("USB Microphone"), 1);
        assert_eq!(capabilities.microphone_index("Built-in Audio"), 2);
        assert_eq!(capabilities.microphone_index("missing microphone"), 0);
    }
}
