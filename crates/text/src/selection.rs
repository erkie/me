//! Selections and the multi-selection set.

use std::ops::Range;

use crate::change::{Assoc, ChangeSet};

/// One selection (or cursor, when empty). Positions are char offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Preferred visual column for vertical movement, kept across short lines.
    pub goal: Option<usize>,
}

impl Selection {
    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal: None,
        }
    }

    pub fn cursor(pos: usize) -> Self {
        Self::new(pos, pos)
    }

    pub fn from_range(range: Range<usize>) -> Self {
        Self::new(range.start, range.end)
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn is_reversed(&self) -> bool {
        self.head < self.anchor
    }

    /// Moves the head, keeping the anchor (shift-movement).
    pub fn extend_to(self, head: usize) -> Self {
        Self {
            anchor: self.anchor,
            head,
            goal: None,
        }
    }

    pub fn with_goal(mut self, goal: Option<usize>) -> Self {
        self.goal = goal;
        self
    }

    fn map(self, changes: &ChangeSet) -> Self {
        // A cursor sits after text inserted at its position (typing). A
        // selection hugs the text it encloses: text inserted right at one of
        // its edges stays outside.
        let (anchor_assoc, head_assoc) = if self.is_empty() {
            (Assoc::After, Assoc::After)
        } else if self.is_reversed() {
            (Assoc::Before, Assoc::After)
        } else {
            (Assoc::After, Assoc::Before)
        };
        Self {
            anchor: changes.map_pos(self.anchor, anchor_assoc),
            head: changes.map_pos(self.head, head_assoc),
            goal: None,
        }
    }
}

/// A non-empty set of selections, sorted by position and never overlapping.
/// One of them is the *primary* selection: the one most recently added, which
/// commands like "add next occurrence" continue from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::single(Selection::cursor(0))
    }
}

impl SelectionSet {
    pub fn single(selection: Selection) -> Self {
        Self {
            selections: vec![selection],
            primary: 0,
        }
    }

    /// Builds a set from arbitrary selections. `primary` indexes into
    /// `selections`. Overlapping selections are merged.
    pub fn new(selections: Vec<Selection>, primary: usize) -> Self {
        assert!(!selections.is_empty(), "a selection set is never empty");
        let mut set = Self {
            primary: primary.min(selections.len() - 1),
            selections,
        };
        set.normalize();
        set
    }

    pub fn len(&self) -> usize {
        self.selections.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Selection> + ExactSizeIterator {
        self.selections.iter()
    }

    pub fn as_slice(&self) -> &[Selection] {
        &self.selections
    }

    pub fn primary(&self) -> Selection {
        self.selections[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn first(&self) -> Selection {
        self.selections[0]
    }

    pub fn last(&self) -> Selection {
        *self.selections.last().unwrap()
    }

    /// Adds a selection and makes it primary.
    pub fn push(&mut self, selection: Selection) {
        self.selections.push(selection);
        self.primary = self.selections.len() - 1;
        self.normalize();
    }

    /// Removes the primary selection (if more than one), making the
    /// previously added neighbour primary.
    pub fn remove_primary(&mut self) -> Option<Selection> {
        if self.selections.len() < 2 {
            return None;
        }
        let removed = self.selections.remove(self.primary);
        self.primary = self.primary.saturating_sub(1);
        Some(removed)
    }

    /// Applies `f` to every selection, then re-normalizes.
    pub fn transform(&mut self, mut f: impl FnMut(Selection) -> Selection) {
        for sel in &mut self.selections {
            *sel = f(*sel);
        }
        self.normalize();
    }

    /// Replaces each selection with zero or more selections.
    pub fn flat_map(&mut self, mut f: impl FnMut(Selection) -> Vec<Selection>) {
        let mut out = Vec::with_capacity(self.selections.len());
        let mut primary = 0;
        for (ix, sel) in self.selections.iter().enumerate() {
            let produced = f(*sel);
            if ix == self.primary {
                primary = out.len() + produced.len().saturating_sub(1);
            }
            out.extend(produced);
        }
        if out.is_empty() {
            return;
        }
        self.selections = out;
        self.primary = primary.min(self.selections.len() - 1);
        self.normalize();
    }

    /// Maps every selection through an applied change set.
    pub fn map(&mut self, changes: &ChangeSet) {
        for sel in &mut self.selections {
            *sel = sel.map(changes);
        }
        self.normalize();
    }

    /// Clamps every selection to a document of `len` chars.
    pub fn clamp(&mut self, len: usize) {
        for sel in &mut self.selections {
            sel.anchor = sel.anchor.min(len);
            sel.head = sel.head.min(len);
        }
        self.normalize();
    }

    pub fn into_single(self) -> Self {
        Self::single(self.primary())
    }

    fn normalize(&mut self) {
        if self.selections.len() == 1 {
            self.primary = 0;
            return;
        }
        let primary_sel = self.selections[self.primary];
        let mut indexed: Vec<(usize, Selection)> = self.selections.drain(..).enumerate().collect();
        indexed.sort_by_key(|(_, s)| (s.start(), s.end()));

        let old_primary = self.primary;
        let mut merged: Vec<Selection> = Vec::with_capacity(indexed.len());
        let mut new_primary = 0;
        for (ix, sel) in indexed {
            if let Some(last) = merged.last_mut() {
                let touches = sel.start() == last.end() && (sel.is_empty() || last.is_empty());
                if sel.start() < last.end() || touches {
                    let start = last.start();
                    let end = last.end().max(sel.end());
                    *last = if last.is_reversed() {
                        Selection::new(end, start)
                    } else {
                        Selection::new(start, end)
                    };
                    if ix == old_primary {
                        new_primary = merged.len() - 1;
                    }
                    continue;
                }
            }
            if ix == old_primary {
                new_primary = merged.len();
            }
            merged.push(sel);
        }
        self.selections = merged;
        self.primary = new_primary;
        // Keep the primary's goal column if it survived unmerged.
        if self.selections[self.primary].range() == primary_sel.range() {
            self.selections[self.primary].goal = primary_sel.goal;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::Change;

    #[test]
    fn merges_overlaps_and_duplicate_cursors() {
        let set = SelectionSet::new(
            vec![
                Selection::new(10, 15),
                Selection::cursor(3),
                Selection::new(12, 20),
                Selection::cursor(3),
                Selection::cursor(25),
            ],
            4,
        );
        let ranges: Vec<_> = set.iter().map(|s| s.range()).collect();
        assert_eq!(ranges, vec![3..3, 10..20, 25..25]);
        assert_eq!(set.primary().range(), 25..25);
    }

    #[test]
    fn adjacent_non_empty_selections_stay_separate() {
        let set = SelectionSet::new(vec![Selection::new(0, 5), Selection::new(5, 9)], 0);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn primary_tracks_after_sort() {
        let set = SelectionSet::new(vec![Selection::cursor(50), Selection::cursor(1)], 0);
        assert_eq!(set.primary().head, 50);
        assert_eq!(set.primary_index(), 1);
    }

    #[test]
    fn cursors_move_past_inserted_text() {
        let mut set = SelectionSet::new(vec![Selection::cursor(1), Selection::cursor(4)], 1);
        let changes = ChangeSet::new(vec![Change::insert(1, "ab"), Change::insert(4, "ab")]);
        set.map(&changes);
        let heads: Vec<_> = set.iter().map(|s| s.head).collect();
        assert_eq!(heads, vec![3, 8]);
    }
}
