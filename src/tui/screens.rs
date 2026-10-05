//! State and rendering for the dashboard's two interactive sub-screens that
//! aren't just "run one action and show the result": the model catalog
//! (navigable list + activation) and the autostart toggle.

use crate::commands;
use crate::config::Config;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

pub(super) struct ModelScreen {
    pub rows: Vec<commands::ModelRow>,
    pub selected: usize,
}

impl ModelScreen {
    pub(super) fn new(config: &Config) -> Self {
        let rows = commands::model_rows(config);
        let selected = rows.iter().position(|row| row.active).unwrap_or(0);
        Self { rows, selected }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let len = self.rows.len() as i32;
        if len == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    pub(super) fn selected_id(&self) -> &'static str {
        self.rows[self.selected].id
    }

    pub(super) fn list_widget(&self, height: u16) -> Paragraph<'static> {
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let marker = if index == self.selected { "> " } else { "  " };
                let status = if row.active {
                    "active"
                } else if row.installed {
                    "installed"
                } else {
                    "-"
                };
                let text = format!(
                    "{marker}{:<32} {:<8} {:>10}  {:<9}",
                    row.id,
                    row.quant,
                    commands::human_size(row.size_bytes),
                    status
                );
                let style = if index == self.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(text, style))
            })
            .collect();
        let visible_rows = usize::from(height.saturating_sub(2)).max(1);
        let scroll = self.selected.saturating_sub(visible_rows.saturating_sub(1)) as u16;
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Model catalog (MODEL ID / QUANT / SIZE / STATUS)"),
            )
            .scroll((scroll, 0))
    }
}

pub(super) struct AutostartScreen {
    pub selected: usize,
    pub result: Option<Result<(), String>>,
}

pub(super) const AUTOSTART_LABELS: [&str; 2] =
    ["Start manually", "Start TongueTyped when you sign in"];

impl AutostartScreen {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            selected: usize::from(config.startup.autostart),
            result: None,
        }
    }

    pub(super) fn move_selection(&mut self, delta: i32) {
        let next = (self.selected as i32 + delta).clamp(0, 1) as usize;
        self.selected = next;
    }

    pub(super) fn list_widget(&self) -> Paragraph<'static> {
        let lines: Vec<Line> = AUTOSTART_LABELS
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let marker = if index == self.selected { "> " } else { "  " };
                let style = if index == self.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!("{marker}{label}"), style))
            })
            .collect();
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Autostart"))
    }

    pub(super) fn result_line(&self) -> Line<'static> {
        match &self.result {
            None => Line::from(""),
            Some(Ok(())) => Line::from(Span::styled(
                "Autostart setting saved.",
                Style::default().fg(Color::Green),
            )),
            Some(Err(message)) => Line::from(Span::styled(
                message.clone(),
                Style::default().fg(Color::Red),
            )),
        }
    }
}
