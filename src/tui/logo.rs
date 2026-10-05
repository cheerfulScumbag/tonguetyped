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
