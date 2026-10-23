//! Batches of non-overlapping replacements, applied to a rope in one step.
//!
//! Every edit command, however many cursors it touches, produces a single
//! [`ChangeSet`]. Applying it yields the inverse set (for undo) and the list
//! of byte-level edits (for incremental reparsing).

use std::sync::Arc;

use ropey::Rope;

/// Replace the chars in `start..end` (coordinates of the document *before*
/// the change set is applied) with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub text: Arc<str>,
    text_chars: usize,
}

impl Change {
    pub fn new(start: usize, end: usize, text: impl Into<Arc<str>>) -> Self {
        debug_assert!(start <= end);
        let text = text.into();
        let text_chars = text.chars().count();
        Self { start, end, text, text_chars }
    }

    pub fn insert(at: usize, text: impl Into<Arc<str>>) -> Self {
        Self::new(at, at, text)
    }

    pub fn delete(start: usize, end: usize) -> Self {
        Self::new(start, end, "")
    }

    /// Number of chars the replacement text contains.
    pub fn text_chars(&self) -> usize {
        self.text_chars
    }

    fn delta(&self) -> isize {
        self.text_chars as isize - (self.end - self.start) as isize
    }
}

/// Which side of an insertion a mapped position should stick to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    Before,
    After,
}

/// A sorted list of non-overlapping changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    changes: Vec<Change>,
}

/// A byte-level description of one applied change, in the coordinates of the
/// document at the moment it was applied. Replaying a `Vec<ByteEdit>` in order
/// keeps an incremental parser (tree-sitter) in sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    /// (row, byte column)
    pub start_point: (usize, usize),
    pub old_end_point: (usize, usize),
    pub new_end_point: (usize, usize),
}

impl ChangeSet {
    /// Builds a change set. Changes are sorted; a change that overlaps the
    /// previous one is clipped so the set stays well formed.
    pub fn new(mut changes: Vec<Change>) -> Self {
        changes.sort_by_key(|c| (c.start, c.end));
        let mut out: Vec<Change> = Vec::with_capacity(changes.len());
        for mut change in changes {
            if let Some(prev) = out.last() {
                if change.start < prev.end {
                    change.start = prev.end;
                    change.end = change.end.max(change.start);
                }
                // Two pure insertions at the same point: concatenate.
                if change.start == change.end && prev.start == prev.end && prev.start == change.start
                {
                    let prev = out.last_mut().unwrap();
                    let text: String = format!("{}{}", prev.text, change.text);
                    *prev = Change::new(prev.start, prev.end, text);
                    continue;
                }
            }
            if change.start == change.end && change.text.is_empty() {
                continue;
            }
            out.push(change);
        }
        Self { changes: out }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// Applies the changes to `rope` and returns the inverse change set plus
    /// the byte edits (in application order) for incremental reparsing.
    pub fn apply(&self, rope: &mut Rope) -> (ChangeSet, Vec<ByteEdit>) {
        let mut removed: Vec<String> = Vec::with_capacity(self.changes.len());
        let mut edits = Vec::with_capacity(self.changes.len());

        // Back to front, so earlier offsets stay valid.
        for change in self.changes.iter().rev() {
            let start_byte = rope.char_to_byte(change.start);
            let old_end_byte = rope.char_to_byte(change.end);
            let start_point = point_at_byte(rope, start_byte);
            let old_end_point = point_at_byte(rope, old_end_byte);

            removed.push(rope.slice(change.start..change.end).to_string());
            if change.end > change.start {
                rope.remove(change.start..change.end);
            }
            if !change.text.is_empty() {
                rope.insert(change.start, &change.text);
            }

            let new_end_byte = start_byte + change.text.len();
            let new_end_point = point_at_byte(rope, new_end_byte);
            edits.push(ByteEdit {
                start_byte,
                old_end_byte,
                new_end_byte,
                start_point,
                old_end_point,
                new_end_point,
            });
        }
        removed.reverse();

        let mut inverse = Vec::with_capacity(self.changes.len());
        let mut delta: isize = 0;
        for (change, removed) in self.changes.iter().zip(removed) {
            let start = (change.start as isize + delta) as usize;
            inverse.push(Change::new(start, start + change.text_chars, removed));
            delta += change.delta();
        }
        (ChangeSet { changes: inverse }, edits)
    }

    /// Maps a position in the old document to the new one.
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        let mut delta: isize = 0;
        for change in &self.changes {
            if pos < change.start {
                break;
            }
            if change.start == change.end && pos == change.start {
                if assoc == Assoc::Before {
                    break;
                }
                delta += change.delta();
                continue;
            }
            if pos >= change.end {
                delta += change.delta();
                continue;
            }
            // Strictly inside (or at the start of) a replaced range.
            let new_start = (change.start as isize + delta) as usize;
            return if assoc == Assoc::Before || pos == change.start {
                new_start
            } else {
                new_start + change.text_chars
            };
        }
        (pos as isize + delta) as usize
    }

    /// How many chars the document grows (or shrinks) by.
    pub fn len_delta(&self) -> isize {
        self.changes.iter().map(Change::delta).sum()
    }
}

fn point_at_byte(rope: &Rope, byte: usize) -> (usize, usize) {
    let row = rope.byte_to_line(byte);
    (row, byte - rope.line_to_byte(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_invert() {
        let mut rope = Rope::from_str("hello world");
        let set = ChangeSet::new(vec![
            Change::new(0, 5, "goodbye"),
            Change::insert(11, "!"),
            Change::delete(5, 6),
        ]);
        let (inverse, edits) = set.apply(&mut rope);
        assert_eq!(rope.to_string(), "goodbyeworld!");
        assert_eq!(edits.len(), 3);
        inverse.apply(&mut rope);
        assert_eq!(rope.to_string(), "hello world");
    }

    #[test]
    fn map_positions() {
        let set = ChangeSet::new(vec![Change::insert(2, "xx"), Change::new(5, 7, "y")]);
        assert_eq!(set.map_pos(0, Assoc::After), 0);
        assert_eq!(set.map_pos(2, Assoc::Before), 2);
        assert_eq!(set.map_pos(2, Assoc::After), 4);
        assert_eq!(set.map_pos(4, Assoc::After), 6);
        assert_eq!(set.map_pos(5, Assoc::After), 7);
        assert_eq!(set.map_pos(6, Assoc::After), 8);
        assert_eq!(set.map_pos(6, Assoc::Before), 7);
        assert_eq!(set.map_pos(7, Assoc::After), 8);
        assert_eq!(set.map_pos(10, Assoc::After), 11);
    }

    #[test]
    fn overlapping_changes_are_clipped() {
        let set = ChangeSet::new(vec![Change::new(0, 4, "a"), Change::new(2, 6, "b")]);
        assert_eq!(set.changes()[1].start, 4);
        let mut rope = Rope::from_str("0123456789");
        set.apply(&mut rope);
        assert_eq!(rope.to_string(), "ab6789");
    }

    #[test]
    fn byte_edits_track_multibyte_text() {
        let mut rope = Rope::from_str("héllo\nwörld");
        let set = ChangeSet::new(vec![Change::insert(7, "ö")]);
        let (_, edits) = set.apply(&mut rope);
        assert_eq!(rope.to_string(), "héllo\nwöörld");
        let e = edits[0];
        assert_eq!(e.start_point, (1, 1));
        assert_eq!(e.old_end_byte, e.start_byte);
        assert_eq!(e.new_end_byte - e.start_byte, 2);
        assert_eq!(e.new_end_point, (1, 3));
    }
}
