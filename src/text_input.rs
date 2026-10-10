//! A minimal single-line text editor shared by the dashboard's settings
//! screens (`src/tui/screens.rs`) and the setup console's text steps
//! (`src/setup/console.rs`) - the two configuration UIs otherwise have no
//! free-text entry at all, and both need the same folder-path / numeric-field
//! editing behavior (character cursor, insert, backspace, home/end).
//!
//! The cursor is a character index (`0..=char_count`), not a byte offset, so
//! multi-byte input never splits a UTF-8 code point; the value and cursor are
//! exposed so a renderer can draw the cursor with a reversed cell.

#[derive(Debug, Clone)]
pub(crate) struct TextField {
    value: String,
    cursor: usize,
}

impl TextField {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    /// The cursor position as a character index from the start of the value.
    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// Splits the value at the cursor for rendering: the text before the
    /// cursor, the character under the cursor (if any), and the text after it.
    /// A renderer draws the cursor cell reversed when the middle value is
    /// `Some`, or a blank reversed cell when the cursor sits past the end.
    pub(crate) fn split_at_cursor(&self) -> (String, Option<char>, String) {
        let chars: Vec<char> = self.value.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let before = chars[..cursor].iter().collect();
        let cursor_char = chars.get(cursor).copied();
        let after = chars[(cursor + 1).min(chars.len())..].iter().collect();
        (before, cursor_char, after)
    }

    /// The byte offset of the cursor within the value.
    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }

    pub(crate) fn insert(&mut self, c: char) {
        let at = self.byte_index();
        self.value.insert(at, c);
        self.cursor += 1;
    }

    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.byte_index();
        self.cursor -= 1;
        let start = self.byte_index();
        self.value.replace_range(start..end, "");
    }

    pub(crate) fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor < self.value.chars().count() {
            self.cursor += 1;
        }
    }

    pub(crate) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_respects_char_boundaries_and_utf8() {
        let mut field = TextField::new("hällo");
        assert_eq!(field.cursor(), 5);
        field.backspace();
        assert_eq!(field.value(), "häll");
        field.home();
        field.insert('X');
        assert_eq!(field.value(), "Xhäll");
        assert_eq!(field.cursor(), 1);
        field.end();
        field.insert('!');
        assert_eq!(field.value(), "Xhäll!");
    }

    #[test]
    fn movement_clamps_to_the_value_bounds() {
        let mut field = TextField::new("ab");
        field.move_left();
        field.move_left();
        field.move_left();
        assert_eq!(field.cursor(), 0);
        field.move_right();
        field.move_right();
        field.move_right();
        assert_eq!(field.cursor(), 2);
        field.backspace();
        assert_eq!(field.value(), "a");
    }
}
