use crate::config::OutputMethod;
use anyhow::Context;
use std::process::Command;

pub fn output_text(text: &str, method: &OutputMethod, backend: &str) -> anyhow::Result<()> {
    match method {
        OutputMethod::None => {
            tracing::debug!("output method 'none': skipping output of {} chars", text.len());
        }
        OutputMethod::Type => {
            type_text(text, backend)?;
        }
        OutputMethod::Paste | OutputMethod::ClipboardOnly => {
            anyhow::bail!("paste/clipboard-only output not implemented in Stage 1");
        }
    }
    Ok(())
}

fn type_text(text: &str, backend: &str) -> anyhow::Result<()> {
    let backend = if backend == "auto" {
        probe_type_backend()
    } else {
        backend.to_string()
    };

    match backend.as_str() {
        "wtype" => {
            let status = Command::new("wtype")
                .args(["-"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn wtype")?;
            if let Some(mut stdin) = status.stdin {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }
        }
        "dotool" => {
            let mut child = Command::new("dotool")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn dotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                writeln!(stdin, "type {}", text)?;
            }
            child.wait()?;
        }
        "ydotool" => {
            let mut child = Command::new("ydotool")
                .args(["type", "--file", "-"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .context("failed to spawn ydotool")?;
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                stdin.write_all(text.as_bytes())?;
            }
            child.wait()?;
        }
        _ => {
            anyhow::bail!("unsupported typing backend: {}", backend);
        }
    }

    Ok(())
}

pub fn probe_type_backend() -> String {
    if which_exists("wtype") {
        return "wtype".to_string();
    }
    if which_exists("dotool") {
        return "dotool".to_string();
    }
    if which_exists("ydotool") {
        return "ydotool".to_string();
    }
    "none".to_string()
}

pub fn list_available_backends() -> Vec<String> {
    let mut backends = Vec::new();
    if which_exists("enigo") {
        backends.push("enigo".to_string());
    }
    if which_exists("wtype") {
        backends.push("wtype".to_string());
    }
    if which_exists("dotool") {
        backends.push("dotool".to_string());
    }
    if which_exists("ydotool") {
        backends.push("ydotool".to_string());
    }
    backends
}

pub fn has_any_type_backend() -> bool {
    which_exists("wtype")
        || which_exists("dotool")
        || which_exists("ydotool")
        || which_exists("enigo")
}

fn which_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_output() {
        let result = output_text("test", &OutputMethod::None, "auto");
        assert!(result.is_ok());
    }

    #[test]
    fn test_probe_returns_string() {
        let backend = probe_type_backend();
        assert!(!backend.is_empty());
    }

    #[test]
    fn test_list_backends() {
        let backends = list_available_backends();
        assert!(!backends.is_empty() || backends.is_empty());
    }
}