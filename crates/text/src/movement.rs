//! Grapheme, word and visual-column helpers on a rope.

use ropey::RopeSlice;
use ropey::str_utils::byte_to_char_idx;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
use unicode_width::UnicodeWidthChar;

/// Char offset of the next grapheme boundary after `char_idx`.
pub fn next_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> usize {
    let len = slice.len_chars();
    if char_idx >= len {
        return len;
    }
    let byte_idx = slice.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = slice.chunk_at_byte(byte_idx);
    let mut cursor = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match cursor.next_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return len,
            Ok(Some(n)) => {
                return chunk_char_idx + byte_to_char_idx(chunk, n - chunk_byte_idx);
            }
            Err(GraphemeIncomplete::NextChunk) => {
                chunk_byte_idx += chunk.len();
                let (c, _, ci, _) = slice.chunk_at_byte(chunk_byte_idx);
                chunk = c;
                chunk_char_idx = ci;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = slice.chunk_at_byte(n - 1).0;
                cursor.provide_context(ctx, n - ctx.len());
            }
            Err(_) => return (char_idx + 1).min(len),
        }
    }
}

/// Char offset of the previous grapheme boundary before `char_idx`.
pub fn prev_grapheme_boundary(slice: RopeSlice, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }
    let char_idx = char_idx.min(slice.len_chars());
    let byte_idx = slice.char_to_byte(char_idx);
    let (mut chunk, mut chunk_byte_idx, mut chunk_char_idx, _) = slice.chunk_at_byte(byte_idx);
    let mut cursor = GraphemeCursor::new(byte_idx, slice.len_bytes(), true);
    loop {
        match cursor.prev_boundary(chunk, chunk_byte_idx) {
            Ok(None) => return 0,
            Ok(Some(n)) => {
                return chunk_char_idx + byte_to_char_idx(chunk, n - chunk_byte_idx);
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                let (c, bi, ci, _) = slice.chunk_at_byte(chunk_byte_idx - 1);
                chunk = c;
                chunk_byte_idx = bi;
                chunk_char_idx = ci;
            }
            Err(GraphemeIncomplete::PreContext(n)) => {
                let ctx = slice.chunk_at_byte(n - 1).0;
                cursor.provide_context(ctx, n - ctx.len());
            }
            Err(_) => return char_idx - 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharClass {
    Word,
    Punctuation,
    Whitespace,
    Newline,
}

pub fn char_class(ch: char) -> CharClass {
    if ch == '\n' || ch == '\r' {
        CharClass::Newline
    } else if ch.is_whitespace() {
        CharClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Alt+Right: skip whitespace, then the following run of word or
/// punctuation characters. Stops at line ends.
pub fn next_word_end(slice: RopeSlice, pos: usize) -> usize {
    let len = slice.len_chars();
    let mut i = pos;
    let mut chars = slice.chars_at(pos.min(len));
    // Crossing a line break counts as one step on its own.
    match chars.next() {
        None => return len,
        Some(c) if char_class(c) == CharClass::Newline => {
            i += 1;
            if c == '\r' && chars.next() == Some('\n') {
                i += 1;
            }
            return i;
        }
        Some(_) => {
            chars.prev();
        }
    }
    let mut class = None;
    for ch in chars {
        let c = char_class(ch);
        match (class, c) {
            (_, CharClass::Newline) => break,
            (None, CharClass::Whitespace) => {}
            (None, c) => class = Some(c),
            (Some(k), c) if k != c => break,
            _ => {}
        }
        i += 1;
    }
    i
}

/// Alt+Left: mirror of [`next_word_end`].
pub fn prev_word_start(slice: RopeSlice, pos: usize) -> usize {
    let mut i = pos.min(slice.len_chars());
    let mut chars = slice.chars_at(i);
    match chars.prev() {
        None => return 0,
        Some(c) if char_class(c) == CharClass::Newline => {
            i -= 1;
            if c == '\n' && chars.prev() == Some('\r') {
                i -= 1;
            }
            return i;
        }
        Some(_) => {
            chars.next();
        }
    }
    let mut class = None;
    while let Some(ch) = chars.prev() {
        let c = char_class(ch);
        match (class, c) {
            (_, CharClass::Newline) => break,
            (None, CharClass::Whitespace) => {}
            (None, c) => class = Some(c),
            (Some(k), c) if k != c => break,
            _ => {}
        }
        i -= 1;
    }
    i
}

/// The word (or punctuation run) touching `pos`, preferring word characters.
/// Returns an empty range at `pos` when there is nothing to select.
pub fn word_range_at(slice: RopeSlice, pos: usize) -> std::ops::Range<usize> {
    let len = slice.len_chars();
    let pos = pos.min(len);
    let after = (pos < len).then(|| char_class(slice.char(pos)));
    let before = (pos > 0).then(|| char_class(slice.char(pos - 1)));
    let class = match (before, after) {
        (_, Some(CharClass::Word)) => CharClass::Word,
        (Some(CharClass::Word), _) => CharClass::Word,
        (_, Some(CharClass::Punctuation)) => CharClass::Punctuation,
        (Some(CharClass::Punctuation), _) => CharClass::Punctuation,
        (_, Some(CharClass::Whitespace)) => CharClass::Whitespace,
        (Some(CharClass::Whitespace), _) => CharClass::Whitespace,
        _ => return pos..pos,
    };
    let mut start = pos;
    let mut chars = slice.chars_at(pos);
    while let Some(ch) = chars.prev() {
        if char_class(ch) != class {
            break;
        }
        start -= 1;
    }
    let mut end = pos;
    for ch in slice.chars_at(pos) {
        if char_class(ch) != class {
            break;
        }
        end += 1;
    }
    start..end
}

/// Display width of a char at visual column `col` (tabs expand to the next
/// tab stop, wide CJK characters take two cells).
pub fn char_width(ch: char, col: usize, tab_width: usize) -> usize {
    if ch == '\t' {
        tab_width - (col % tab_width)
    } else {
        ch.width().unwrap_or(0).max(if ch.is_control() { 0 } else { 1 })
    }
}

/// Visual column of the char at `char_col` within `line`.
pub fn visual_col(line: RopeSlice, char_col: usize, tab_width: usize) -> usize {
    let mut col = 0;
    for ch in line.chars().take(char_col) {
        col += char_width(ch, col, tab_width);
    }
    col
}

/// Visual width of the whole line.
pub fn visual_len(line: RopeSlice, tab_width: usize) -> usize {
    visual_col(line, line.len_chars(), tab_width)
}

/// Char column within `line` closest to visual column `target`.
/// Returns (char column, whether target lies beyond the line's end).
pub fn char_col_at_visual(line: RopeSlice, target: usize, tab_width: usize) -> (usize, bool) {
    let mut col = 0;
    for (i, ch) in line.chars().enumerate() {
        let w = char_width(ch, col, tab_width);
        if col + w > target {
            // Snap to the nearer edge of the char.
            return if target - col > w / 2 && w > 1 { (i + 1, false) } else { (i, false) };
        }
        col += w;
    }
    (line.len_chars(), target > col)
}

/// Leading whitespace of a line, as a string.
pub fn indentation(line: RopeSlice) -> String {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn graphemes() {
        let rope = Rope::from_str("ae\u{301}x\r\ny👍🏽z");
        let s = rope.slice(..);
        assert_eq!(next_grapheme_boundary(s, 0), 1);
        assert_eq!(next_grapheme_boundary(s, 1), 3); // e + combining acute
        assert_eq!(next_grapheme_boundary(s, 4), 6); // CRLF is one grapheme
        assert_eq!(prev_grapheme_boundary(s, 6), 4);
        assert_eq!(prev_grapheme_boundary(s, 3), 1);
        let thumbs = 7;
        assert_eq!(next_grapheme_boundary(s, thumbs), thumbs + 2);
        assert_eq!(prev_grapheme_boundary(s, thumbs + 2), thumbs);
    }

    #[test]
    fn word_motion() {
        let rope = Rope::from_str("foo_bar  (baz)\nnext");
        let s = rope.slice(..);
        assert_eq!(next_word_end(s, 0), 7);
        assert_eq!(next_word_end(s, 7), 10);
        assert_eq!(next_word_end(s, 10), 13);
        assert_eq!(next_word_end(s, 14), 15);
        assert_eq!(prev_word_start(s, 13), 10);
        assert_eq!(prev_word_start(s, 10), 9);
        assert_eq!(prev_word_start(s, 9), 0);
        assert_eq!(prev_word_start(s, 15), 14);
    }

    #[test]
    fn word_at_cursor() {
        let rope = Rope::from_str("let foo = bar;");
        let s = rope.slice(..);
        assert_eq!(word_range_at(s, 5), 4..7);
        assert_eq!(word_range_at(s, 7), 4..7);
        assert_eq!(word_range_at(s, 13), 10..13);
        assert_eq!(word_range_at(s, 8), 8..9);
    }

    #[test]
    fn visual_columns_with_tabs_and_wide_chars() {
        let rope = Rope::from_str("\tab日本x");
        let line = rope.slice(..);
        assert_eq!(visual_col(line, 1, 4), 4);
        assert_eq!(visual_col(line, 3, 4), 6);
        assert_eq!(visual_col(line, 4, 4), 8);
        assert_eq!(char_col_at_visual(line, 2, 4), (0, false));
        assert_eq!(char_col_at_visual(line, 5, 4), (2, false));
        assert_eq!(char_col_at_visual(line, 9, 4), (4, false));
        assert_eq!(char_col_at_visual(line, 10, 4), (5, false));
        assert_eq!(char_col_at_visual(line, 20, 4), (6, true));
    }
}
