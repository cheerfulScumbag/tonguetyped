//! Live caption text for the `live-text` overlay styles: a font rasteriser, the
//! single-line "newest words win" layout the caption uses, and the multi-line
//! wrap the scrolling box uses.
//!
//! The overlay paints raw pixels, so text has to be rasterised by hand. The
//! bundled Inter (SIL OFL, see `assets/fonts/LICENSE-Inter.txt`) is parsed
//! once into an [`ab_glyph::FontVec`] and every glyph is blended as
//! anti-aliased coverage through [`super::Canvas::blend`] - no shaping stack, no
//! system font dependency, so the caption looks the same on any host and can be
//! rendered headlessly in tests.
//!
//! The default single-line layout keeps the whole committed + tentative string
//! on one line and never wraps; when it overruns the card the newest words stay
//! in frame and the oldest fade off the left edge as the line slides, so a
//! long dictation reads as a scrolling caption rather than a squished block.
//! The committed/tentative split is carried per character, so the still
//! revisable tail can be dimmed without re-measuring. The scrolling box reuses
//! a simple word wrap with the same "newest words win" window.

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::sync::OnceLock;

use super::{Canvas, LiveText};

const FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");

/// Caption type size in pixels.
pub(super) const CAPTION_FONT_SIZE: f32 = 23.0;
/// Baseline-to-baseline distance for the scrolling box's lines; a touch over
/// the type size so lines breathe without growing the box too fast.
pub(super) const CAPTION_LINE_HEIGHT: f32 = 30.0;
/// How many wrapped lines the scrolling box shows before its newest-words
/// window kicks in.
pub(super) const CAPTION_MAX_LINES: usize = 5;

/// How wide (in pixels) the single-line caption's left edge fades, so words
/// leaving the frame dissolve instead of cutting at a hard edge.
pub(super) const CAPTION_LEFT_FADE: f32 = 42.0;

static FONT: OnceLock<Option<FontVec>> = OnceLock::new();

/// The bundled font, parsed once. `None` only if the embedded bytes are not a
/// valid font, which would be a build-time mistake; callers fall back to the
/// waveform rather than panicking on the paint path.
fn font() -> Option<&'static FontVec> {
    FONT.get_or_init(|| FontVec::try_from_vec(FONT_BYTES.to_vec()).ok())
        .as_ref()
}

/// One laid-out character plus whether it is still tentative (revisable), so
/// the caller can colour the committed prefix and the tentative tail
/// differently without a second layout pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LaidChar {
    ch: char,
    tentative: bool,
}

/// One visual line of laid-out characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LaidLine {
    chars: Vec<LaidChar>,
}

impl LaidLine {
    #[allow(dead_code)]
    pub(super) fn text(&self) -> String {
        self.chars.iter().map(|c| c.ch).collect()
    }
}

/// Lay `committed + tentative` out on a single line - no wrapping, the whole
/// run in order with each character's committed/tentative flag carried through.
/// Newlines (which a partial transcript should never contain, but a batch
/// transcript might if it is ever shown) collapse to spaces so the line stays
/// one line.
pub(super) fn caption_line(committed: &str, tentative: &str) -> LaidLine {
    let committed_chars = committed.chars().count();
    let chars = committed
        .chars()
        .chain(tentative.chars())
        .enumerate()
        .map(|(index, ch)| LaidChar {
            ch: if ch == '\n' || ch == '\r' { ' ' } else { ch },
            tentative: index >= committed_chars,
        })
        .collect();
    LaidLine { chars }
}

/// The measured advance width of a laid-out line at the caption size.
pub(super) fn line_width(line: &LaidLine) -> f32 {
    let Some(font) = font() else { return 0.0 };
    let scale = PxScale::from(CAPTION_FONT_SIZE);
    let scaled = font.as_scaled(scale);
    line.chars
        .iter()
        .map(|c| scaled.h_advance(font.glyph_id(c.ch)))
        .sum()
}

/// Wrap `committed + tentative` to `max_width`, returning at most
/// `CAPTION_MAX_LINES` lines with the most recent speech kept. Used by the
/// scrolling box. When earlier lines are dropped the first visible line is
/// prefixed with an ellipsis marked tentative; the committed/tentative flag of
/// every character - including that ellipsis - is carried through, so the
/// caller colours each glyph by its own flag.
pub(super) fn caption_lines(committed: &str, tentative: &str, max_width: f32) -> Vec<LaidLine> {
    let Some(font) = font() else {
        return Vec::new();
    };
    let scale = PxScale::from(CAPTION_FONT_SIZE);
    let scaled = font.as_scaled(scale);
    let committed_chars = committed.chars().count();

    let mut lines: Vec<LaidLine> = vec![LaidLine::default()];
    let mut line_width = 0.0_f32;
    for (index, ch) in committed.chars().chain(tentative.chars()).enumerate() {
        let tentative = index >= committed_chars;
        if ch == '\n' {
            lines.push(LaidLine::default());
            line_width = 0.0;
            continue;
        }
        let advance = scaled.h_advance(font.glyph_id(ch));
        if line_width + advance > max_width && !lines.last().unwrap().chars.is_empty() {
            // Break at the last space on the line when there is one, so words
            // stay whole; otherwise hard-break a too-long word.
            let current = lines.last_mut().unwrap();
            let break_at = current.chars.iter().rposition(|c| c.ch == ' ');
            match break_at {
                Some(position) if position > 0 => {
                    let tail: Vec<LaidChar> = current.chars.split_off(position + 1);
                    current.chars.pop();
                    line_width = tail
                        .iter()
                        .map(|c| scaled.h_advance(font.glyph_id(c.ch)))
                        .sum();
                    lines.push(LaidLine { chars: tail });
                }
                _ => {
                    lines.push(LaidLine::default());
                    line_width = 0.0;
                }
            }
        }
        lines
            .last_mut()
            .unwrap()
            .chars
            .push(LaidChar { ch, tentative });
        line_width += advance;
    }

    // Trim trailing empty lines (a trailing newline, or no text at all).
    while lines.len() > 1 && lines.last().is_some_and(|line| line.chars.is_empty()) {
        lines.pop();
    }

    let dropped = lines.len().saturating_sub(CAPTION_MAX_LINES);
    let mut visible: Vec<LaidLine> = lines.split_off(dropped);
    if dropped > 0 {
        if let Some(first) = visible.first_mut() {
            first.chars.insert(
                0,
                LaidChar {
                    ch: '…',
                    tentative: true,
                },
            );
        }
    }

    visible
        .into_iter()
        .filter(|line| !line.chars.is_empty())
        .collect()
}

/// The colours of a caption line: the frozen prefix and the still-revisable
/// tail, plus one opacity for the whole caption.
pub(super) struct CaptionColors {
    pub committed: (u8, u8, u8),
    pub tentative: (u8, u8, u8),
    pub alpha: f32,
}

/// Draw one laid-out line with its left edge at `x` and the top of the line's
/// box at `y_top`. Output is left-aligned at `x`; the caller chooses `x` (for
/// the single-line caption it is the value that keeps the newest words in
/// frame). When `clip_left` is `Some(edge)`, nothing is drawn left of `edge`
/// and the glyphs fade to nothing across the `CAPTION_LEFT_FADE` pixels after
/// it, so text scrolling off the left dissolves rather than cutting off.
pub(super) fn draw_line(
    canvas: &mut Canvas,
    x: f32,
    y_top: f32,
    line: &LaidLine,
    colors: &CaptionColors,
    clip_left: Option<f32>,
) {
    let Some(font) = font() else { return };
    let scale = PxScale::from(CAPTION_FONT_SIZE);
    let scaled = font.as_scaled(scale);
    let baseline = y_top + scaled.ascent();
    let mut cursor = x;
    for laid in &line.chars {
        let ch = laid.ch;
        let glyph_id = font.glyph_id(ch);
        let color = if laid.tentative {
            colors.tentative
        } else {
            colors.committed
        };
        let glyph = glyph_id.with_scale_and_position(scale, point(cursor, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|dx, dy, coverage| {
                let fade = match clip_left {
                    Some(edge) => ((bounds.min.x + dx as f32 - edge) / CAPTION_LEFT_FADE)
                        .clamp(0.0, 1.0),
                    None => 1.0,
                };
                if fade <= 0.0 {
                    return;
                }
                canvas.blend(
                    bounds.min.x as i32 + dx as i32,
                    bounds.min.y as i32 + dy as i32,
                    color,
                    coverage * colors.alpha * fade,
                );
            });
        }
        cursor += scaled.h_advance(glyph_id);
    }
}

/// Draw a multi-line caption (the scrolling box): each line starts at the same
/// left `x`, one `CAPTION_LINE_HEIGHT` below the last.
pub(super) fn draw_caption(
    canvas: &mut Canvas,
    x: f32,
    y_top: f32,
    max_width: f32,
    text: &LiveText,
    colors: CaptionColors,
) {
    let lines = caption_lines(&text.committed, &text.tentative, max_width);
    let mut y = y_top;
    for line in &lines {
        draw_line(canvas, x, y, line, &colors, None);
        y += CAPTION_LINE_HEIGHT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_font_parses() {
        assert!(font().is_some(), "embedded Inter font must parse");
    }

    #[test]
    fn single_line_keeps_the_whole_run_and_its_split() {
        let line = caption_line("hello world", " again");
        assert_eq!(line.text(), "hello world again");
        let committed = "hello world".chars().count();
        assert!(line.chars[..committed].iter().all(|c| !c.tentative));
        assert!(line.chars[committed..].iter().all(|c| c.tentative));
        assert!(line_width(&line) > 0.0);
    }

    #[test]
    fn single_line_never_wraps() {
        let long = "one two three four five six seven eight nine ten eleven twelve";
        let line = caption_line(long, "");
        assert_eq!(line.text(), long);
    }

    #[test]
    fn box_wraps_at_word_boundaries() {
        let lines = caption_lines(
            "the quick brown fox jumps over the lazy dog and keeps running",
            "",
            220.0,
        );
        assert!(lines.len() >= 2);
        for line in &lines {
            assert!(
                !line.text().starts_with(' '),
                "a wrapped line kept a leading space"
            );
        }
    }

    #[test]
    fn box_newest_words_window_drops_leading_lines_and_ellipsizes() {
        let long = "one two three four five six seven eight nine ten eleven twelve \
                    thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
        let lines = caption_lines(long, "", 220.0);
        assert_eq!(lines.len(), CAPTION_MAX_LINES, "window keeps the cap");
        let first = &lines[0];
        assert!(
            first.text().starts_with('…'),
            "the first visible line should mark dropped text: {:?}",
            first.text()
        );
        assert!(first.chars[0].tentative, "the ellipsis must be tentative");
    }

    #[test]
    fn empty_input_lays_out_nothing() {
        assert!(caption_lines("", "", 720.0).is_empty());
    }
}
