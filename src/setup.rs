use crate::config::{ActivationMode, Config, OutputMethod};
use std::io::{self, BufRead, IsTerminal, Write};

mod console;

#[derive(Debug)]
pub(crate) struct Capabilities {
    microphones: Vec<(String, String)>,
    typing_backends: Vec<String>,
}

impl Capabilities {
    fn discover() -> anyhow::Result<Self> {
        let mut microphones = vec![(
            "default".to_string(),
            "System default microphone".to_string(),
        )];
        for device in crate::audio::list_devices()? {
            if !microphones.iter().any(|(value, _)| value == &device.name) {
                let detail = if device.is_default {
                    format!("{} (current default)", device.name)
                } else {
                    device.name.clone()
                };
                microphones.push((device.name, detail));
            }
        }
        Ok(Self {
            microphones,
            typing_backends: crate::output::list_available_backends(),
        })
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

pub fn run() -> anyhow::Result<()> {
    let capabilities = Capabilities::discover()?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    // Piped/non-interactive stdin or stdout (scripts, tests, CI) falls back to the
    // line-based flow below so `tonguetyped setup` stays scriptable with plain
    // newline-separated answers.
    if stdin.is_terminal() && stdout.is_terminal() {
        let outcome = console::run(capabilities)?;
        if matches!(outcome, SetupOutcome::Cancelled) {
            writeln!(stdout, "\nSetup cancelled. No changes were made.")?;
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
    let models = crate::model::ModelCatalog::model_names();
    let model_labels: Vec<String> = models.iter().map(|name| (*name).to_string()).collect();
    let model_default = models
        .iter()
        .position(|name| *name == config.model.selected)
        .unwrap_or(0);
    let Some(model) = choose(input, output, errors, &model_labels, model_default)? else {
        return Ok(SetupOutcome::Cancelled);
    };
    config.model.selected = models[model].to_string();

    ui.section(output, 2, "Microphone")?;
    if capabilities.microphones.len() == 1 {
        writeln!(output, "Only the system default microphone is available.")?;
    }
    let microphone_labels: Vec<String> = capabilities
        .microphones
        .iter()
        .map(|(_, label)| label.clone())
        .collect();
    let microphone_default = capabilities
        .microphones
        .iter()
        .position(|(value, _)| value == &config.audio.microphone)
        .unwrap_or(0);
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
    let Some(shortcut) =
        prompt_shortcut(input, output, errors, config.activation.keybind.as_str())?
    else {
        return Ok(SetupOutcome::Cancelled);
    };
    config.activation.keybind = shortcut;
    config.activation.keybind_status = "untested".to_string();

    ui.section(output, 5, "Transcript output")?;
    let mut output_labels = vec!["Keep transcripts in TongueTyped".to_string()];
    if !capabilities.typing_backends.is_empty() {
        output_labels.push("Type into the focused application".to_string());
    } else {
        writeln!(
            output,
            "No supported typing backend was detected; transcripts will stay in TongueTyped."
        )?;
    }
    let output_default =
        usize::from(config.output.method == OutputMethod::Type && output_labels.len() > 1);
    let Some(output_method) = choose(input, output, errors, &output_labels, output_default)? else {
        return Ok(SetupOutcome::Cancelled);
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
    writeln!(output, "  Model:       {}", config.model.selected)?;
    writeln!(output, "  Microphone:  {}", config.audio.microphone)?;
    writeln!(
        output,
        "  Activation:  {} with {}",
        config.activation.mode, config.activation.keybind
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

fn prompt_shortcut(
    input: &mut impl BufRead,
    output: &mut impl Write,
    errors: &mut impl Write,
    default: &str,
) -> anyhow::Result<Option<String>> {
    loop {
        write!(output, "Shortcut [{default}]: ")?;
        output.flush()?;
        let Some(answer) = read_answer(input)? else {
            return Ok(None);
        };
        if is_cancel(&answer) {
            return Ok(None);
        }
        let shortcut = if answer.is_empty() {
            default.to_string()
        } else {
            answer
        };
        match crate::activation::portal_trigger(&shortcut) {
            Ok(_) => return Ok(Some(shortcut)),
            Err(error) => writeln!(errors, "Invalid shortcut: {error}")?,
        }
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
}
