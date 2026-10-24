//! Soft wrap: splitting a long line into display rows.
//!
//! Wrapping works in visual columns (the editor uses a monospace font), so it
//! needs no text shaping and is cheap enough to run per visible line on every
//! frame. Tabs expand relative to the start of the row they are on.

use ropey::RopeSlice;

use crate::movement::char_width;

/// Lines at least this long are wrapped at exactly `max_cols` without
/// looking for word boundaries (minified files, data dumps).
const WORD_BREAK_LIMIT: usize = 100_000;

/// The char column where each display row of `line` starts. The first entry
/// is always 0; a line that fits returns `[0]`.
///
/// Rows break after whitespace when possible, else mid-word. Whitespace is
/// allowed to hang past `max_cols` so no row starts with a space.
pub fn wrap_line(line: RopeSlice, max_cols: usize, tab_width: usize) -> Vec<usize> {
    let max_cols = max_cols.max(1);
    let mut rows = vec![0];
    if line.len_chars() <= max_cols && !line.chars().any(|c| c == '\t' || c.len_utf8() > 2) {
        // Fast path: every char is one cell wide.
        return rows;
    }
    let word_breaks = line.len_chars() < WORD_BREAK_LIMIT;
    let mut row_start = 0;
    // Visual column within the current row.
    let mut col = 0;
    // Char column just after the last whitespace in this row, and the visual
    // column there.
    let mut break_at: Option<(usize, usize)> = None;
    for (i, ch) in line.chars().enumerate() {
        let w = char_width(ch, col, tab_width);
        let is_space = ch == ' ' || ch == '\t';
        if col + w > max_cols && i > row_start && !is_space {
            match break_at.filter(|_| word_breaks) {
                Some((at, at_col)) if at > row_start => {
                    rows.push(at);
                    row_start = at;
                    col -= at_col;
                }
                _ => {
                    rows.push(i);
                    row_start = i;
                    col = 0;
                }
            }
            break_at = None;
            // Re-measure this char at its new position (tabs depend on it).
            let w = char_width(ch, col, tab_width);
            col += w;
            continue;
        }
        col += w;
        if is_space {
            break_at = Some((i + 1, col));
        }
    }
    rows
}

/// The row of `rows` that displays char column `col`. A column on a row
/// boundary belongs to the row it starts.
pub fn row_of(rows: &[usize], col: usize) -> usize {
    rows.partition_point(|&start| start <= col)
        .saturating_sub(1)
}

/// Char range `[start, end)` of row `row`, for a line of `line_len` chars.
pub fn row_range(rows: &[usize], row: usize, line_len: usize) -> (usize, usize) {
    let start = rows[row];
    let end = rows.get(row + 1).copied().unwrap_or(line_len);
    (start, end)
}

/// Visual column of char column `col` within its row starting at `row_start`.
pub fn row_visual_col(line: RopeSlice, row_start: usize, col: usize, tab_width: usize) -> usize {
    let mut v = 0;
    for ch in line.slice(row_start..col.max(row_start)).chars() {
        v += char_width(ch, v, tab_width);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn wrap(text: &str, cols: usize) -> Vec<String> {
        let rope = Rope::from_str(text);
        let rows = wrap_line(rope.slice(..), cols, 4);
        (0..rows.len())
            .map(|r| {
                let (s, e) = row_range(&rows, r, rope.len_chars());
                rope.slice(s..e).to_string()
            })
            .collect()
    }

    #[test]
    fn short_lines_are_one_row() {
        assert_eq!(wrap("hello", 10), vec!["hello"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("exactly 10", 10), vec!["exactly 10"]);
    }

    #[test]
    fn breaks_after_spaces() {
        assert_eq!(
            wrap("the quick brown fox jumps", 10),
            vec!["the quick ", "brown fox ", "jumps"]
        );
    }

    #[test]
    fn spaces_hang_past_the_edge() {
        assert_eq!(wrap("aaaa     bbbb", 6), vec!["aaaa     ", "bbbb"]);
    }

    #[test]
    fn long_words_break_anywhere() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab abcdefghij", 4), vec!["ab ", "abcd", "efgh", "ij"]);
    }

    #[test]
    fn wide_chars_take_two_cells() {
        assert_eq!(wrap("日本語テキスト", 6), vec!["日本語", "テキス", "ト"]);
    }

    #[test]
    fn tabs_expand_from_the_row_start() {
        // The third tab ends exactly at column 12 and hangs there; tab stops
        // restart on the next row.
        assert_eq!(wrap("\tx \tx \tx ", 12), vec!["\tx \tx \t", "x "]);
        assert_eq!(wrap("ab\tcd\tef", 6), vec!["ab\tcd\t", "ef"]);
    }

    #[test]
    fn rows_and_boundaries() {
        let rows = vec![0, 10, 20];
        assert_eq!(row_of(&rows, 0), 0);
        assert_eq!(row_of(&rows, 9), 0);
        assert_eq!(row_of(&rows, 10), 1);
        assert_eq!(row_of(&rows, 25), 2);
        assert_eq!(row_range(&rows, 2, 25), (20, 25));
    }
}
