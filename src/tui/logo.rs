//! Centers the TongueTyped wordmark across the full terminal width. Pure and
//! terminal-independent (just takes a column count) so alignment at normal,
//! wide, and narrow sizes can be unit-tested without a real TTY.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Identical wordmark to `setup::console::LOGO`, reused here rather than
/// imported since the console draws it left-aligned (see
/// `.superdesign/design-system.md`) while the dashboard must center it - the
/// two call sites intentionally diverge only in alignment, not content.
const FULL_LOGO: &str = "████████╗ ██████╗ ███╗   ██╗ ██████╗ ██╗   ██╗███████╗████████╗██╗   ██╗██████╗ ███████╗██████╗\n╚══██╔══╝██╔═══██╗████╗  ██║██╔════╝ ██║   ██║██╔════╝╚══██╔══╝╚██╗ ██╔╝██╔══██╗██╔════╝██╔══██╗\n   ██║   ██║   ██║██╔██╗ ██║██║  ███╗██║   ██║█████╗     ██║    ╚████╔╝ ██████╔╝█████╗  ██║  ██║\n   ██║   ██║   ██║██║╚██╗██║██║   ██║██║   ██║██╔══╝     ██║     ╚██╔╝  ██╔═══╝ ██╔══╝  ██║  ██║\n   ██║   ╚██████╔╝██║ ╚████║╚██████╔╝╚██████╔╝███████╗   ██║      ██║   ██║     ███████╗██████╔╝\n   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝ ╚═════╝  ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚═╝     ╚══════╝╚═════╝\n\n                              ░▒▓  S P E A K .  T Y P E .  R E P E A T .  ▓▒░";

pub const FULL_LOGO_HEIGHT: u16 = 8;
pub const COMPACT_LOGO_HEIGHT: u16 = 2;

fn full_logo_width() -> u16 {
    FULL_LOGO
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as u16
}

fn centered_line(text: &str, width: u16, style: Style) -> Line<'static> {
    let text_width = text.chars().count() as u16;
    let margin = usize::from(width.saturating_sub(text_width) / 2);
    Line::from(Span::styled(format!("{}{text}", " ".repeat(margin)), style))
}

/// Centers the wordmark within `width` columns using the full block-art logo
/// when it fits, falling back to a compact text wordmark otherwise so a
/// narrow terminal never clips the logo into garbage - the existing setup
/// console has no such fallback (see design-system.md), which is a gap this
/// dashboard must close. Returns the rendered lines plus their total height
/// so callers can size their layout chunk.
pub fn centered_logo(width: u16) -> (Vec<Line<'static>>, u16) {
    let brand = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let full_width = full_logo_width();

    if width >= full_width {
        let margin = usize::from((width - full_width) / 2);
        let pad = " ".repeat(margin);
        let lines = FULL_LOGO
            .lines()
            .map(|line| Line::from(Span::styled(format!("{pad}{line}"), brand)))
            .collect();
        (lines, FULL_LOGO_HEIGHT)
    } else {
        let lines = vec![
            centered_line("TONGUETYPED", width, brand),
            centered_line("Speak. Type. Repeat.", width, brand),
        ];
        (lines, COMPACT_LOGO_HEIGHT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_terminal_uses_the_full_block_art_logo_centered() {
        let full_width = full_logo_width();
        let (lines, height) = centered_logo(full_width + 20);
        assert_eq!(height, FULL_LOGO_HEIGHT);
        assert_eq!(lines.len(), usize::from(FULL_LOGO_HEIGHT));
        let margin = usize::from(((full_width + 20) - full_width) / 2);
        for (rendered, original) in lines.iter().zip(FULL_LOGO.lines()) {
            let rendered_text: String = rendered
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(rendered_text, format!("{}{original}", " ".repeat(margin)));
        }
    }

    #[test]
    fn exact_width_match_uses_the_full_logo_with_no_margin() {
        let full_width = full_logo_width();
        let (lines, height) = centered_logo(full_width);
        assert_eq!(height, FULL_LOGO_HEIGHT);
        let first_line_text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(first_line_text, FULL_LOGO.lines().next().unwrap());
    }

    #[test]
    fn narrow_terminal_falls_back_to_the_compact_wordmark() {
        let full_width = full_logo_width();
        let (lines, height) = centered_logo(full_width - 1);
        assert_eq!(height, COMPACT_LOGO_HEIGHT);
        assert_eq!(lines.len(), 2);
        let first_line_text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(first_line_text.contains("TONGUETYPED"));
    }

    #[test]
    fn degenerate_zero_width_terminal_does_not_panic() {
        let (lines, _height) = centered_logo(0);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn compact_wordmark_is_centered_within_its_own_width() {
        let (lines, _height) = centered_logo(21); // "TONGUETYPED" is 11 chars
        let first_line_text: String = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(first_line_text, format!("{}TONGUETYPED", " ".repeat(5)));
    }
}

/// The compact speech-bubble mark used by the split-deck header. The art is
/// the 14x7 half-block version of the app icon: `▓` is the purple bubble fill,
/// `█` is the green outline and T. The text blocks align to the right of the
/// mark, so narrow terminals still see the wordmark.
pub const BUBBLE_HEIGHT: u16 = 7;

const BUBBLE_ART: [&str; 7] = [
    "   ▂▂▃▃▃▃▄▄▄▖",
    "  ▐▔▕▅  ▗▅ ▗▌",
    "  ▋▐▇█▆▛██▛▐▏",
    " ▕▍ ▟▋  █▍ ▟",
    " ▐▎ ▜▙▃▅█▏ ▌",
    " ▐▅▖▂▔▔▔  ▝",
    "  ▀▔▔",
];

const SPLASH_ART: [&str; 21] = [
    "",
    "                        ▁▁▁▁▂▂▂▃▃▃▃▄▄▄▃",
    "         ▃▅▅▅▆▆▆▆▇▇▇▇█▛▛▛▛▛▛▀▀▀▀▀▀▀▀▀██▙",
    "        ▟▀▔                          ▐█▉",
    "       ▕▉      ▁▃▅▆        ▐█▆▖      ▐█▌",
    "       ▐▋      ▐██▊        ▟██▍      ██▏",
    "       ▟▍      ▟██▌     ▁▁▁███▂▂▂▂▂  █▊",
    "       █▏ ▐▆▇▇▇███████████████████▌ ▗█▍",
    "      ▕█  ▀▀▀▀▜███▀▀▀▀▔▔▔▔▜██▊      ▐█▏",
    "      ▐▊      ▐██▊        ▟██▌      █▊",
    "      ▟▌      ▟██▌        ███▎      █▍",
    "     ▕█▎      ███▎       ▕███      ▕█",
    "     ▐█      ▕███        ▐██▊      ▐▊",
    "     █▊      ▕██▉       ▁▟██▌      ▟▌",
    "    ▕█▌      ▝███▆▄▃▃▄▄▅███▛       █▎",
    "    ▐█▎       ▝▜████████▛▀▘       ▗█",
    "    ██▁                      ▁▁▁▂▄▇▘",
    "    ▜██▅▅▅▆▎    ▂▄▅▆▇▇▇▀▀▀▀▀▀▀▀▀▀▔",
    "     ▔▔▔██▀▁▃▅▆▛▀▔▔",
    "      ▗███▇▀▀▔",
    "      ▝▀▀▔",
];

fn bubble_line(art: &str, text: Option<Line<'static>>) -> Line<'static> {
    let green = Style::default()
        .fg(Color::Rgb(0, 255, 0))
        .add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for ch in art.chars() {
        used += 1;
        if ch == ' ' {
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::styled(ch.to_string(), green));
        }
    }
    for _ in used..14 {
        spans.push(Span::raw(" "));
    }
    if let Some(text) = text {
        spans.push(Span::raw("   "));
        spans.extend(text.spans);
    }
    Line::from(spans)
}

/// The header for the split deck: the compact bubble on the left, the wordmark
/// and live status on the right. The art is always 14 columns so the text has a
/// stable column; terminals narrower than that simply clip the right side.
pub fn bubble_header(daemon_running: bool, model: &str) -> Vec<Line<'static>> {
    let brand = Style::default()
        .fg(Color::Rgb(0, 255, 0))
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(Color::DarkGray);
    let (daemon_text, daemon_style) = if daemon_running {
        ("running", Style::default().fg(Color::Green))
    } else {
        ("stopped", Style::default().fg(Color::Red))
    };
    vec![
        bubble_line(
            BUBBLE_ART[0],
            Some(Line::from(Span::styled("TONGUETYPED", brand))),
        ),
        bubble_line(
            BUBBLE_ART[1],
            Some(Line::from(Span::styled("speak · type · repeat", muted))),
        ),
        bubble_line(
            BUBBLE_ART[2],
            Some(Line::from(vec![
                Span::styled("● ", daemon_style),
                Span::raw("daemon "),
                Span::styled(daemon_text, daemon_style),
                Span::raw("   "),
                Span::styled("model ", muted),
                Span::styled(model.to_string(), Style::default().fg(Color::Cyan)),
            ])),
        ),
        bubble_line(BUBBLE_ART[3], None),
        bubble_line(BUBBLE_ART[4], None),
        bubble_line(
            BUBBLE_ART[5],
            Some(Line::from(Span::styled(
                "~/tonguetyped · local only",
                muted,
            ))),
        ),
        bubble_line(BUBBLE_ART[6], None),
    ]
}

/// The launch splash: the chafa line-art bubble, then the wordmark and the
/// skip hint. At narrow widths the art is dropped and the wordmark remains.
pub fn splash_lines(width: u16) -> Vec<Line<'static>> {
    let art_width = SPLASH_ART
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let green = Style::default()
        .fg(Color::Rgb(0, 255, 0))
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    if width >= art_width {
        for art in SPLASH_ART {
            let text_width = art.chars().count() as u16;
            let margin = usize::from(width.saturating_sub(text_width) / 2);
            let mut spans = vec![Span::raw(" ".repeat(margin))];
            for ch in art.chars() {
                if ch == ' ' {
                    spans.push(Span::raw(" "));
                } else {
                    spans.push(Span::styled(ch.to_string(), green));
                }
            }
            lines.push(Line::from(spans));
        }
    }
    let brand = Line::from(vec![Span::styled("TONGUETYPED", green)])
        .alignment(ratatui::layout::Alignment::Center);
    let tagline = Line::from(vec![Span::styled("speak · type · repeat", muted)])
        .alignment(ratatui::layout::Alignment::Center);
    let hint = Line::from(vec![Span::styled("press any key", muted)])
        .alignment(ratatui::layout::Alignment::Center);
    if lines.is_empty() {
        lines.push(Line::from(""));
        lines.push(brand);
        lines.push(tagline);
        lines.push(hint);
    } else {
        lines.push(brand);
        lines.push(tagline);
        lines.push(hint);
    }
    lines
}
