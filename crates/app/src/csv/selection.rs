//! Which cells of the table are selected: a rectangle between the cell
//! where the selection started and the one it was dragged or extended to.

use std::ops::Range;

/// What a click selected, and so what a drag or Shift extends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Cells,
    /// Whole rows, from the row numbers.
    Rows,
    /// Whole columns, from the header.
    Columns,
    All,
}

/// `anchor` is the active cell (the one edited, and moved from); `head`
/// is the corner Shift and dragging move. Both are (data row, column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (usize, usize),
    pub head: (usize, usize),
    pub span: Span,
}

impl Selection {
    pub fn cell(row: usize, col: usize) -> Self {
        Self {
            anchor: (row, col),
            head: (row, col),
            span: Span::Cells,
        }
    }

    pub fn all(rows: usize, cols: usize) -> Self {
        Self {
            anchor: (0, 0),
            head: (rows.saturating_sub(1), cols.saturating_sub(1)),
            span: Span::All,
        }
    }

    /// One cell, nothing more.
    pub fn is_single(&self) -> bool {
        self.span == Span::Cells && self.anchor == self.head
    }

    /// The selected rows, out of `total`.
    pub fn rows(&self, total: usize) -> Range<usize> {
        match self.span {
            Span::Columns | Span::All => 0..total,
            Span::Cells | Span::Rows => span(self.anchor.0, self.head.0, total),
        }
    }

    /// The selected columns, out of `total`.
    pub fn cols(&self, total: usize) -> Range<usize> {
        match self.span {
            Span::Rows | Span::All => 0..total,
            Span::Cells | Span::Columns => span(self.anchor.1, self.head.1, total),
        }
    }

    pub fn contains(&self, row: usize, col: usize, rows: usize, cols: usize) -> bool {
        self.rows(rows).contains(&row) && self.cols(cols).contains(&col)
    }

    /// Keeps both ends inside a table of this size (after an edit or a new
    /// separator); `None` when it's empty.
    pub fn clamped(mut self, rows: usize, cols: usize) -> Option<Self> {
        if rows == 0 || cols == 0 {
            return None;
        }
        for (row, col) in [&mut self.anchor, &mut self.head] {
            *row = (*row).min(rows - 1);
            *col = (*col).min(cols - 1);
        }
        Some(self)
    }
}

fn span(a: usize, b: usize, total: usize) -> Range<usize> {
    a.min(b).min(total)..(a.max(b) + 1).min(total)
}

/// A field for tab-separated text, quoted the way spreadsheets read it
/// back when it holds a tab, a line break or a quote.
pub fn tsv_field(value: &str) -> std::borrow::Cow<'_, str> {
    if value.contains(['\t', '\n', '\r', '"']) {
        format!("\"{}\"", value.replace('"', "\"\"")).into()
    } else {
        value.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_the_span() {
        let mut s = Selection::cell(5, 2);
        s.head = (3, 4);
        assert_eq!(s.rows(100), 3..6);
        assert_eq!(s.cols(10), 2..5);
        assert!(s.contains(4, 3, 100, 10) && !s.contains(6, 3, 100, 10));
        s.span = Span::Rows;
        assert_eq!(s.cols(10), 0..10);
        s.span = Span::Columns;
        assert_eq!(s.rows(100), 0..100);
        assert_eq!(Selection::all(7, 3).rows(7), 0..7);
        // Ends past a shrunken table are pulled in.
        assert_eq!(
            Selection::cell(9, 9).clamped(4, 2),
            Some(Selection::cell(3, 1))
        );
        assert_eq!(Selection::cell(0, 0).clamped(0, 2), None);
    }

    #[test]
    fn quotes_tsv_fields() {
        assert_eq!(tsv_field("plain, text"), "plain, text");
        assert_eq!(tsv_field("a\tb"), "\"a\tb\"");
        assert_eq!(tsv_field("say \"hi\"\n"), "\"say \"\"hi\"\"\n\"");
    }
}
