//! Live caption text for the `live-text` overlay style: a font rasteriser, a
//! single-pass word wrap, and the "newest words win" window.
//!
//! The overlay paints raw pixels, so text has to be rasterised by hand. The
//! bundled Inter (SIL OFL, see `assets/fonts/LICENSE-Inter.txt`) is parsed
//! once into an [`ab_glyph::FontVec`] and every glyph is blended as
//! anti-aliased coverage through [`super::Canvas::blend`] - no shaping stack, no
//! system font dependency, so the caption looks the same on any host and can be
//! rendered headlessly in tests.
//!
//! Layout is deliberately simple: the whole committed + tentative string is
//! wrapped to the caption width, then all but the last [`CAPTION_MAX_LINES`]
//! lines are dropped and the first surviving line is prefixed with an ellipsis,
//! so the reader always sees the most recent speech even when a sentence runs
//! long. The committed/tentative split is carried per character, so the still
//! revisable tail can be dimmed without re-measuring.

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::sync::OnceLock;

use super::{Canvas, LiveText};

const FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");

/// Caption type size in pixels, matching the reviewed mockup.
pub(super) const CAPTION_FONT_SIZE: f32 = 22.0;
/// Baseline-to-baseline distance; a touch over the type size so the two lines
/// breathe without growing the pill.
pub(super) const CAPTION_LINE_HEIGHT: f32 = 28.0;
/// How many caption lines fit the pill before the newest-words window kicks in.
pub(super) const CAPTION_MAX_LINES: usize = 2;

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
    pub(super) fn text(&self) -> String {
        self.chars.iter().map(|c| c.ch).collect()
    }
}

/// Wrap `committed + tentative` to `max_width`, returning at most
/// `max_lines` lines with the most recent speech kept. When earlier lines are
/// dropped the first visible line is prefixed with an ellipsis marked
/// tentative; the committed/tentative flag of every character - including that
/// ellipsis - is carried through, so the caller colours each glyph by its own
/// flag instead of assuming a committed-prefix/tentative-suffix layout. Layout
/// uses the bundled font, so the same input always yields the same lines.
pub(super) fn caption_lines(
    committed: &str,
    tentative: &str,
    max_width: f32,
) -> Vec<LaidLine> {
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

/// Draw the live caption text. `x` is the left edge, `y_top` the top of the
/// first line's box. `colors` supplies the committed/tentative hues and the
/// overall opacity. Carries the newest-words window from [`caption_lines`]; a
/// no-op when the bundled font failed to parse or there is no text yet.
pub(super) fn draw_caption(
    canvas: &mut Canvas,
    x: f32,
    y_top: f32,
    max_width: f32,
    text: &LiveText,
    colors: CaptionColors,
) {
    let Some(font) = font() else {
        return;
    };
    let lines = caption_lines(&text.committed, &text.tentative, max_width);
    if lines.is_empty() {
        return;
    }
    let scale = PxScale::from(CAPTION_FONT_SIZE);
    let scaled = font.as_scaled(scale);
    let ascent = scaled.ascent();
    let mut y = y_top;
    for line in lines {
        let baseline = y + ascent;
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
                    canvas.blend(
                        bounds.min.x as i32 + dx as i32,
                        bounds.min.y as i32 + dy as i32,
                        color,
                        coverage * colors.alpha,
                    );
                });
            }
            cursor += scaled.h_advance(glyph_id);
        }
        y += CAPTION_LINE_HEIGHT;
    }
}

/// The measured advance width of `text` at the caption size, used by the pill
/// to place the trailing caret after the newest tentative word.
pub(super) fn width_of(text: &str) -> f32 {
    let Some(font) = font() else { return 0.0 };
    let scale = PxScale::from(CAPTION_FONT_SIZE);
    let scaled = font.as_scaled(scale);
    text.chars()
        .map(|ch| scaled.h_advance(font.glyph_id(ch)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_font_parses() {
        assert!(font().is_some(), "embedded Inter font must parse");
    }

    #[test]
    fn short_text_stays_on_one_line_and_keeps_its_split() {
        let lines = caption_lines("hello world", " again", 720.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "hello world again");
        let committed = "hello world".chars().count();
        assert!(lines[0].chars[..committed].iter().all(|c| !c.tentative));
        assert!(lines[0].chars[committed..].iter().all(|c| c.tentative));
    }

    #[test]
    fn wrapping_splits_at_word_boundaries() {
        let lines = caption_lines(
            "the quick brown fox jumps over the lazy dog and keeps running",
            "",
            220.0,
        );
        assert!(lines.len() >= CAPTION_MAX_LINES);
        for line in &lines {
            assert!(
                !line.text().starts_with(' '),
                "a wrapped line kept a leading space"
            );
        }
    }

    #[test]
    fn newest_words_window_drops_leading_lines_and_ellipsizes() {
        let long = "one two three four five six seven eight nine ten eleven twelve \
                    thirteen fourteen fifteen sixteen seventeen eighteen";
        let lines = caption_lines(long, "", 220.0);
        assert_eq!(lines.len(), CAPTION_MAX_LINES, "window keeps two lines");
        let first = &lines[0];
        assert!(
            first.text().starts_with('…'),
            "the first visible line should mark dropped text: {:?}",
            first.text()
        );
        // The dropped-text marker is a tentative glyph even though it sits at
        // the start of the line, and the committed words after it stay
        // committed - colouring must follow each glyph's own flag.
        assert!(first.chars[0].tentative, "the ellipsis must be tentative");
        assert!(
            !first.chars.last().unwrap().tentative,
            "committed text after the ellipsis must stay committed"
        );
    }

    #[test]
    fn tentative_tail_is_tracked_per_character() {
        let lines = caption_lines("hello", " world", 720.0);
        let chars = &lines[0].chars;
        assert_eq!(chars.len(), 11);
        assert!(chars[..5].iter().all(|c| !c.tentative));
        assert!(chars[5..].iter().all(|c| c.tentative));
    }

    #[test]
    fn empty_input_lays_out_nothing() {
        assert!(caption_lines("", "", 720.0).is_empty());
    }
}
