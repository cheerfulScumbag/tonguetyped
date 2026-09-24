use crate::config::OutputMethod;
use anyhow::Context;
use std::process::Command;

pub fn output_text(
    text: &str,
    method: &OutputMethod,
    backend: &str,
    auto_submit: bool,
) -> anyhow::Result<()> {
    match method {
        OutputMethod::None => {
            tracing::debug!(
                "output method 'none': skipping output of {} chars",
                text.len()
            );
        }
        OutputMethod::Type => {
            type_text(text, backend, auto_submit)?;
        }
    }
    Ok(())
}

fn type_text(text: &str, backend: &str, auto_submit: bool) -> anyhow::Result<()> {
    let backend = if backend == "auto" {
        probe_type_backend()
    } else {
        backend.to_string()
    };

    match backend.as_str() {
        "wtype" => {
            let mut child = Command::new("wtype")
                .args(["-"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn wtype")?;
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }
            require_success("wtype", child.wait()?)?;
            if auto_submit {
                require_success(
                    "wtype",
                    Command::new("wtype").args(["-k", "Return"]).status()?,
                )?;
            }
        }
        "enigo" => {
            use enigo::{Enigo, Keyboard, Settings};
            let mut enigo = Enigo::new(&Settings::default())?;
            enigo.text(text)?;
            if auto_submit {
                enigo.key(enigo::Key::Return, enigo::Direction::Click)?;
            }
        }
        "dotool" => {
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn dotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                write_dotool_commands(&mut stdin, text, auto_submit)?;
            }
            require_success("dotool", child.wait()?)?;
        }
        _ => {
            anyhow::bail!("unsupported typing backend: {}", backend);
        }
    }

    Ok(())
}

pub fn probe_type_backend() -> String {
    if helper_self_test("wtype") {
        return "wtype".to_string();
    }
    if enigo_available() {
        return "enigo".to_string();
    }
    if helper_self_test("dotool") {
        return "dotool".to_string();
    }
    "none".to_string()
}

pub fn list_available_backends() -> Vec<String> {
    let mut backends = Vec::new();
    if helper_self_test("wtype") {
        backends.push("wtype".to_string());
    }
    if enigo_available() {
        backends.push("enigo".to_string());
    }
    if helper_self_test("dotool") {
        backends.push("dotool".to_string());
    }
    backends
}

pub fn has_any_type_backend() -> bool {
    helper_self_test("wtype") || enigo_available() || helper_self_test("dotool")
}

pub fn type_backend_available(backend: &str) -> bool {
    match backend {
        "auto" => has_any_type_backend(),
        "wtype" | "dotool" => helper_self_test(backend),
        "enigo" => enigo_available(),
        _ => false,
    }
}

fn enigo_available() -> bool {
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session == "wayland") {
        return false;
    }
    use enigo::{Enigo, Settings};
    Enigo::new(&Settings::default()).is_ok()
}

fn write_dotool_commands(
    writer: &mut impl std::io::Write,
    text: &str,
    auto_submit: bool,
) -> std::io::Result<()> {
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            writeln!(writer, "key enter")?;
        }
        writeln!(writer, "type {line}")?;
    }
    if auto_submit {
        writeln!(writer, "key enter")?;
    }
    Ok(())
}

fn require_success(backend: &str, status: std::process::ExitStatus) -> anyhow::Result<()> {
    if !status.success() {
        anyhow::bail!("{} exited with status {}", backend, status);
    }
    Ok(())
}

fn helper_self_test(command: &str) -> bool {
    let mut command = Command::new(command);
    if command.get_program() == "wtype" {
        command.arg("-");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_output() {
        let result = output_text("test", &OutputMethod::None, "auto", false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_probe_returns_string() {
        let backend = probe_type_backend();
        assert!(!backend.is_empty());
    }

    #[test]
    fn dotool_encodes_newlines_as_keys() {
        let mut commands = Vec::new();
        write_dotool_commands(&mut commands, "notes\nkey ctrl+a", true).unwrap();
        assert_eq!(
            String::from_utf8(commands).unwrap(),
            "type notes\nkey enter\ntype key ctrl+a\nkey enter\n"
        );
    }
}
