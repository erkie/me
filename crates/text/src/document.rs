//! The text document: a rope plus line-ending style and undo history.

use std::io;
use std::ops::Range;

use ropey::{Rope, RopeSlice};

use crate::change::{ByteEdit, ChangeSet};
use crate::history::{EditKind, History};
use crate::selection::SelectionSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }

    /// Detects the dominant line ending from the start of the text.
    pub fn detect(rope: &Rope) -> Self {
        let mut lf = 0usize;
        let mut crlf = 0usize;
        let mut prev = '\0';
        for ch in rope.chars().take(64 * 1024) {
            if ch == '\n' {
                if prev == '\r' {
                    crlf += 1;
                } else {
                    lf += 1;
                }
            }
            prev = ch;
        }
        if crlf > lf {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

#[derive(Debug, Default)]
pub struct Document {
    rope: Rope,
    line_ending: LineEnding,
    history: History,
    revision: u64,
    saved_revision: u64,
    pending_edits: Vec<ByteEdit>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rope(rope: Rope) -> Self {
        let line_ending = LineEnding::detect(&rope);
        Self {
            rope,
            line_ending,
            ..Default::default()
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self::from_rope(Rope::from_str(text))
    }

    pub fn from_reader(reader: impl io::Read) -> io::Result<Self> {
        Ok(Self::from_rope(Rope::from_reader(reader)?))
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn slice(&self, range: Range<usize>) -> RopeSlice<'_> {
        self.rope.slice(range)
    }

    pub fn text(&self, range: Range<usize>) -> String {
        self.rope.slice(range).to_string()
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn set_line_ending(&mut self, line_ending: LineEnding) {
        self.line_ending = line_ending;
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn char_to_line(&self, pos: usize) -> usize {
        self.rope.char_to_line(pos.min(self.rope.len_chars()))
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.rope.len_lines()))
    }

    /// Char offset of the end of `line`, before its line ending.
    pub fn line_end(&self, line: usize) -> usize {
        if line + 1 >= self.rope.len_lines() {
            return self.rope.len_chars();
        }
        let next = self.rope.line_to_char(line + 1);
        let mut end = next - 1; // the '\n'
        if end > 0 && self.rope.char(end - 1) == '\r' {
            end -= 1;
        }
        end.max(self.line_start(line))
    }

    /// Start of the line after `line` (or the end of the document).
    pub fn line_start_after(&self, line: usize) -> usize {
        if line + 1 >= self.rope.len_lines() {
            self.rope.len_chars()
        } else {
            self.rope.line_to_char(line + 1)
        }
    }

    /// Length of `line` in chars, without its line ending.
    pub fn line_len(&self, line: usize) -> usize {
        self.line_end(line) - self.line_start(line)
    }

    /// The text of `line` without its line ending.
    pub fn line(&self, line: usize) -> RopeSlice<'_> {
        self.rope.slice(self.line_start(line)..self.line_end(line))
    }

    /// (line, char column) of a char offset.
    pub fn line_col(&self, pos: usize) -> (usize, usize) {
        let line = self.char_to_line(pos);
        (line, pos - self.line_start(line))
    }

    pub fn pos_at(&self, line: usize, col: usize) -> usize {
        if line >= self.len_lines() {
            return self.len_chars();
        }
        self.line_start(line) + col.min(self.line_len(line))
    }

    pub fn char_at(&self, pos: usize) -> Option<char> {
        (pos < self.rope.len_chars()).then(|| self.rope.char(pos))
    }

    /// Current revision. Changes on every edit, undo and redo; undoing back
    /// to a saved state restores the saved revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
        self.history.seal();
    }

    /// Replaces the whole content (e.g. reload from disk) as one undoable step.
    pub fn replace_all(&mut self, text: &str, selections: &mut SelectionSet) {
        let changes = ChangeSet::new(vec![crate::change::Change::new(0, self.len_chars(), text)]);
        let before = selections.clone();
        selections.map(&changes);
        selections.clamp(self.len_chars() + text.chars().count());
        self.apply(changes, before, selections, EditKind::Other);
    }

    /// Applies a change set. `selections_after` must already be expressed in
    /// the coordinates of the document *after* the change; it is clamped to
    /// the new length.
    pub fn apply(
        &mut self,
        changes: ChangeSet,
        selections_before: SelectionSet,
        selections_after: &mut SelectionSet,
        kind: EditKind,
    ) {
        if changes.is_empty() {
            return;
        }
        let (inverse, edits) = changes.apply(&mut self.rope);
        self.pending_edits.extend(edits);
        selections_after.clamp(self.rope.len_chars());
        self.revision = self.history.record(
            changes,
            inverse,
            selections_before,
            selections_after.clone(),
            kind,
            self.revision,
        );
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Undoes one step; returns the selections to restore.
    pub fn undo(&mut self) -> Option<SelectionSet> {
        let step = self.history.undo()?;
        for changes in &step.changes {
            let (_, edits) = changes.apply(&mut self.rope);
            self.pending_edits.extend(edits);
        }
        self.revision = step.revision;
        let mut selections = step.selections;
        selections.clamp(self.rope.len_chars());
        Some(selections)
    }

    /// Redoes one step; returns the selections to restore.
    pub fn redo(&mut self) -> Option<SelectionSet> {
        let step = self.history.redo()?;
        for changes in &step.changes {
            let (_, edits) = changes.apply(&mut self.rope);
            self.pending_edits.extend(edits);
        }
        self.revision = step.revision;
        let mut selections = step.selections;
        selections.clamp(self.rope.len_chars());
        Some(selections)
    }

    /// Stops the next edit from merging into the current undo step.
    pub fn seal_undo(&mut self) {
        self.history.seal();
    }

    /// Byte edits applied since the last call, in order. A syntax tree that
    /// replays these stays in sync with the document.
    pub fn take_edits(&mut self) -> Vec<ByteEdit> {
        std::mem::take(&mut self.pending_edits)
    }

    pub fn write_to(&self, writer: impl io::Write) -> io::Result<()> {
        self.rope.write_to(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_helpers_handle_crlf() {
        let doc = Document::from_str("ab\r\ncd\r\n\r\nlast");
        assert_eq!(doc.line_ending(), LineEnding::CrLf);
        assert_eq!(doc.len_lines(), 4);
        assert_eq!(doc.line(0).to_string(), "ab");
        assert_eq!(doc.line(1).to_string(), "cd");
        assert_eq!(doc.line(2).to_string(), "");
        assert_eq!(doc.line(3).to_string(), "last");
        assert_eq!(doc.line_end(0), 2);
        assert_eq!(doc.line_start(1), 4);
    }

    #[test]
    fn trailing_newline_makes_empty_last_line() {
        let doc = Document::from_str("a\nb\n");
        assert_eq!(doc.len_lines(), 3);
        assert_eq!(doc.line_len(2), 0);
        assert_eq!(doc.line_end(1), 3);
    }

    #[test]
    fn dirty_tracking_survives_undo() {
        let mut doc = Document::from_str("abc");
        assert!(!doc.is_dirty());
        let mut sels = SelectionSet::single(crate::Selection::cursor(3));
        let changes = ChangeSet::new(vec![crate::change::Change::insert(3, "d")]);
        let before = sels.clone();
        sels.map(&changes);
        doc.apply(changes, before, &mut sels, EditKind::Other);
        assert!(doc.is_dirty());
        doc.undo();
        assert!(!doc.is_dirty());
        doc.redo();
        assert!(doc.is_dirty());
        doc.mark_saved();
        assert!(!doc.is_dirty());
    }
}
