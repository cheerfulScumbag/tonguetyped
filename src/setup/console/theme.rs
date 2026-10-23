//! The setup console's palette: a Catppuccin Mocha-inspired dark theme that
//! paints its own background, so it reads the same on a light or dark
//! terminal. With `NO_COLOR` set every style drops its colours and keeps only
//! modifiers; meaning never depends on colour alone (radio dots, checkboxes,
//! focus markers, and worded states carry it), so the console stays usable.

use ratatui::style::{Color, Modifier, Style};

const BASE: Color = Color::Rgb(0x18, 0x18, 0x25);
const SURFACE: Color = Color::Rgb(0x31, 0x32, 0x44);
/// Unfocused pane borders: a step above `SURFACE` so the frames stay visible.
const OUTLINE: Color = Color::Rgb(0x58, 0x5b, 0x70);
const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
const SUBTEXT: Color = Color::Rgb(0x93, 0x99, 0xb2);
const CYAN: Color = Color::Rgb(0x89, 0xdc, 0xeb);
const VIOLET: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
const GREEN: Color = Color::Rgb(0xa6, 0xe3, 0xa1);

#[derive(Debug, Clone, Copy)]
pub(super) struct Theme {
    color: bool,
}

impl Theme {
    /// Honours the `NO_COLOR` convention: any non-empty value turns colour off.
    pub(super) fn from_env() -> Self {
        Self::new(std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty()))
    }

    pub(super) const fn new(color: bool) -> Self {
        Self { color }
    }

    fn paint(&self, fg: Color, modifier: Modifier) -> Style {
        let style = Style::default().add_modifier(modifier);
        if self.color {
            style.fg(fg)
        } else {
            style
        }
    }

    /// The whole frame: soft white text on the slate background.
    pub(super) fn base(&self) -> Style {
        if self.color {
            Style::default().fg(TEXT).bg(BASE)
        } else {
            Style::default()
        }
    }

    pub(super) fn text(&self) -> Style {
        self.paint(TEXT, Modifier::empty())
    }

    pub(super) fn dim(&self) -> Style {
        if self.color {
            Style::default().fg(SUBTEXT)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        }
    }

    pub(super) fn brand(&self) -> Style {
        self.paint(CYAN, Modifier::BOLD)
    }

    pub(super) fn accent(&self) -> Style {
        self.paint(CYAN, Modifier::empty())
    }

    pub(super) fn heading(&self) -> Style {
        self.paint(VIOLET, Modifier::BOLD)
    }

    pub(super) fn key(&self) -> Style {
        self.paint(VIOLET, Modifier::BOLD)
    }

    /// The focused row: a lifted surface plus bold, and reversed without
    /// colour so the focus stays visible on a monochrome terminal.
    pub(super) fn focus(&self) -> Style {
        if self.color {
            Style::default()
                .fg(CYAN)
                .bg(SURFACE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    /// The active tab: an inverted cyan pill.
    pub(super) fn active_tab(&self) -> Style {
        if self.color {
            Style::default()
                .fg(BASE)
                .bg(CYAN)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        }
    }

    pub(super) fn border(&self, focused: bool) -> Style {
        match (self.color, focused) {
            (true, true) => Style::default().fg(CYAN),
            (true, false) => Style::default().fg(OUTLINE),
            (false, true) => Style::default().add_modifier(Modifier::BOLD),
            (false, false) => Style::default(),
        }
    }

    pub(super) fn ok(&self) -> Style {
        self.paint(GREEN, Modifier::BOLD)
    }

    pub(super) fn warn(&self) -> Style {
        self.paint(YELLOW, Modifier::BOLD)
    }

    pub(super) fn error(&self) -> Style {
        self.paint(RED, Modifier::BOLD)
    }

    /// The recording badge: red, and bold either way.
    pub(super) fn recording(&self) -> Style {
        self.paint(RED, Modifier::BOLD)
    }

    pub(super) fn gauge(&self) -> Style {
        self.paint(GREEN, Modifier::empty())
    }
}
