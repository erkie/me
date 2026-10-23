//! Editor state: a document plus selections, and every editing command.
//!
//! All commands work on every selection at once, so multi-cursor editing is
//! the normal case rather than a special mode. Each command that changes the
//! text produces exactly one transaction (one undo step).

use std::ops::Range;

use crate::change::{Assoc, Change, ChangeSet};
use crate::document::Document;
use crate::history::EditKind;
use crate::movement::{
    char_col_at_visual, indentation, next_grapheme_boundary, next_word_end, prev_grapheme_boundary,
    prev_word_start, visual_col, word_range_at,
};
use crate::search::SearchQuery;
use crate::selection::{Selection, SelectionSet};

/// Text copied from the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardText {
    pub text: String,
    /// One entry per selection, so a paste into the same number of cursors
    /// can distribute them.
    pub per_selection: Vec<String>,
    /// Copied whole lines from empty selections (Sublime/VS Code behaviour):
    /// pasting inserts above the current line rather than at the cursor.
    pub whole_lines: bool,
}

#[derive(Debug)]
pub struct EditorState {
    pub doc: Document,
    selections: SelectionSet,
    pub tab_width: usize,
    /// Indent with tabs instead of spaces.
    pub hard_tabs: bool,
    /// The word Cmd+D expanded from a bare cursor. While the primary
    /// selection still holds that word, further Cmd+D presses only match
    /// whole words.
    occurrence_word: Option<String>,
}

impl EditorState {
    pub fn new(doc: Document) -> Self {
        let hard_tabs = detect_hard_tabs(&doc);
        Self {
            doc,
            selections: SelectionSet::default(),
            tab_width: 4,
            hard_tabs,
            occurrence_word: None,
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        Self::new(Document::from_str(text))
    }

    pub fn text(&self) -> String {
        self.doc.rope().to_string()
    }

    pub fn selections(&self) -> &SelectionSet {
        &self.selections
    }

    pub fn set_selections(&mut self, mut selections: SelectionSet) {
        selections.clamp(self.doc.len_chars());
        self.selections = selections;
        self.occurrence_word = None;
        self.doc.seal_undo();
    }

    pub fn set_cursor(&mut self, pos: usize) {
        self.set_selections(SelectionSet::single(Selection::cursor(pos)));
    }

    fn indent_unit(&self) -> String {
        if self.hard_tabs {
            "\t".into()
        } else {
            " ".repeat(self.tab_width)
        }
    }

    // ---------------------------------------------------------------------
    // Committing changes
    // ---------------------------------------------------------------------

    fn commit(&mut self, changes: ChangeSet, mut after: SelectionSet, kind: EditKind) -> bool {
        if changes.is_empty() {
            return false;
        }
        let before = self.selections.clone();
        self.doc.apply(changes, before, &mut after, kind);
        self.selections = after;
        true
    }

    /// Replaces each selection with the text produced by `f` and leaves a
    /// cursor after each inserted text.
    fn replace_each(&mut self, kind: EditKind, mut f: impl FnMut(&Self, Selection) -> String) {
        let changes: Vec<Change> = self
            .selections
            .iter()
            .map(|s| Change::new(s.start(), s.end(), f(self, *s)))
            .collect();
        let ends: Vec<usize> = self.selections.iter().map(|s| s.end()).collect();
        let changes = ChangeSet::new(changes);
        let after = cursors(
            ends.into_iter().map(|e| changes.map_pos(e, Assoc::After)),
            self.selections.primary_index(),
        );
        self.commit(changes, after, kind);
    }

    /// Deletes, for each selection, the range `f` returns (non-empty
    /// selections always delete themselves).
    fn delete_each(&mut self, kind: EditKind, mut f: impl FnMut(&Self, usize) -> Range<usize>) {
        let ranges: Vec<Range<usize>> = self
            .selections
            .iter()
            .map(|s| {
                if s.is_empty() {
                    f(self, s.head)
                } else {
                    s.range()
                }
            })
            .collect();
        let changes = ChangeSet::new(
            ranges
                .iter()
                .map(|r| Change::delete(r.start, r.end))
                .collect(),
        );
        let after = cursors(
            ranges
                .iter()
                .map(|r| changes.map_pos(r.start, Assoc::Before)),
            self.selections.primary_index(),
        );
        self.commit(changes, after, kind);
    }

    /// Replaces each of `ranges` (one per selection, e.g. IME marked text)
    /// with `text`, leaving a cursor after each. Returns the inserted ranges.
    pub fn replace_ranges(
        &mut self,
        ranges: &[Range<usize>],
        text: &str,
        kind: EditKind,
    ) -> Vec<Range<usize>> {
        let len = self.doc.len_chars();
        let changes = ChangeSet::new(
            ranges
                .iter()
                .map(|r| Change::new(r.start.min(len), r.end.min(len), text))
                .collect(),
        );
        let text_len = text.chars().count();
        let inserted: Vec<Range<usize>> = ranges
            .iter()
            .map(|r| {
                let end = changes.map_pos(r.end.min(len), Assoc::After);
                end.saturating_sub(text_len)..end
            })
            .collect();
        let after = cursors(
            inserted.iter().map(|r| r.end),
            self.selections.primary_index(),
        );
        self.occurrence_word = None;
        self.commit(changes, after, kind);
        inserted
    }

    // ---------------------------------------------------------------------
    // Typing and deleting
    // ---------------------------------------------------------------------

    /// Types `text` at every selection.
    pub fn insert(&mut self, text: &str) {
        let kind = if text.chars().count() == 1 && !text.contains('\n') {
            EditKind::Insert
        } else {
            EditKind::Other
        };
        self.occurrence_word = None;
        self.replace_each(kind, |_, _| text.to_string());
    }

    /// Inserts a line break, keeping the current line's indentation and
    /// indenting one more level after an opening bracket.
    pub fn newline(&mut self) {
        let eol = self.doc.line_ending().as_str();
        let unit = self.indent_unit();
        self.replace_each(EditKind::Other, |this, sel| {
            let line = this.doc.char_to_line(sel.start());
            let line_start = this.doc.line_start(line);
            let mut indent = indentation(this.doc.line(line));
            let col = sel.start() - line_start;
            if indent.chars().count() > col {
                indent = indent.chars().take(col).collect();
            }
            let before = this.doc.slice(line_start..sel.start()).to_string();
            if matches!(before.trim_end().chars().last(), Some('{' | '[' | '(')) {
                indent.push_str(&unit);
            }
            format!("{eol}{indent}")
        });
    }

    pub fn backspace(&mut self) {
        let tab_width = self.tab_width;
        let hard_tabs = self.hard_tabs;
        self.delete_each(EditKind::Delete, |this, pos| {
            if pos == 0 {
                return 0..0;
            }
            let (line, col) = this.doc.line_col(pos);
            if !hard_tabs && col > 0 {
                let before = this.doc.slice(this.doc.line_start(line)..pos);
                if before.chars().all(|c| c == ' ') {
                    let remove = match col % tab_width {
                        0 => tab_width,
                        n => n,
                    };
                    return pos - remove..pos;
                }
            }
            prev_grapheme_boundary(this.doc.rope().slice(..), pos)..pos
        });
    }

    pub fn delete_forward(&mut self) {
        self.delete_each(EditKind::Delete, |this, pos| {
            pos..next_grapheme_boundary(this.doc.rope().slice(..), pos)
        });
    }

    pub fn delete_word_left(&mut self) {
        self.delete_each(EditKind::Other, |this, pos| {
            prev_word_start(this.doc.rope().slice(..), pos)..pos
        });
    }

    pub fn delete_word_right(&mut self) {
        self.delete_each(EditKind::Other, |this, pos| {
            pos..next_word_end(this.doc.rope().slice(..), pos)
        });
    }

    /// Cmd+Backspace.
    pub fn delete_to_line_start(&mut self) {
        self.delete_each(EditKind::Other, |this, pos| {
            let line_start = this.doc.line_start(this.doc.char_to_line(pos));
            if pos == line_start {
                prev_grapheme_boundary(this.doc.rope().slice(..), pos)..pos
            } else {
                line_start..pos
            }
        });
    }

    /// Ctrl+K.
    pub fn delete_to_line_end(&mut self) {
        self.delete_each(EditKind::Other, |this, pos| {
            let line_end = this.doc.line_end(this.doc.char_to_line(pos));
            if pos == line_end {
                pos..next_grapheme_boundary(this.doc.rope().slice(..), pos)
            } else {
                pos..line_end
            }
        });
    }

    // ---------------------------------------------------------------------
    // Clipboard
    // ---------------------------------------------------------------------

    pub fn copy(&self) -> ClipboardText {
        let eol = self.doc.line_ending().as_str();
        if self.selections.iter().all(|s| s.is_empty()) {
            let mut per_selection = Vec::new();
            let mut last_line = None;
            for sel in self.selections.iter() {
                let line = self.doc.char_to_line(sel.head);
                if last_line == Some(line) {
                    continue;
                }
                last_line = Some(line);
                let mut text = self.doc.line(line).to_string();
                text.push_str(eol);
                per_selection.push(text);
            }
            return ClipboardText {
                text: per_selection.concat(),
                per_selection,
                whole_lines: true,
            };
        }
        let per_selection: Vec<String> = self
            .selections
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| self.doc.text(s.range()))
            .collect();
        ClipboardText {
            text: per_selection.join(eol),
            per_selection,
            whole_lines: false,
        }
    }

    pub fn cut(&mut self) -> ClipboardText {
        let copied = self.copy();
        if copied.whole_lines {
            self.delete_lines();
        } else {
            self.delete_each(EditKind::Other, |_, pos| pos..pos);
        }
        copied
    }

    /// Pastes `clip`. When the clipboard holds one entry per selection, each
    /// selection gets its own entry; when it holds lines copied from bare
    /// cursors, they go above the cursor's line.
    pub fn paste(&mut self, clip: &ClipboardText) {
        self.occurrence_word = None;
        let n = self.selections.len();
        let pieces: Option<Vec<String>> = if n > 1 && clip.per_selection.len() == n {
            Some(clip.per_selection.clone())
        } else if n > 1 && clip.per_selection.len() <= 1 {
            let lines: Vec<&str> = clip.text.lines().collect();
            (lines.len() == n).then(|| lines.into_iter().map(String::from).collect())
        } else {
            None
        };

        if clip.whole_lines && self.selections.iter().all(|s| s.is_empty()) {
            let sels: Vec<Selection> = self.selections.iter().copied().collect();
            let changes: Vec<Change> = sels
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let text = pieces
                        .as_ref()
                        .map(|p| p[i].clone())
                        .unwrap_or(clip.text.clone());
                    let line_start = self.doc.line_start(self.doc.char_to_line(s.head));
                    Change::insert(line_start, text)
                })
                .collect();
            let changes = ChangeSet::new(changes);
            let after = cursors(
                sels.iter().map(|s| changes.map_pos(s.head, Assoc::After)),
                self.selections.primary_index(),
            );
            self.commit(changes, after, EditKind::Other);
            return;
        }

        let mut i = 0;
        self.replace_each(EditKind::Other, |_, _| {
            let text = match &pieces {
                Some(p) => p[i].clone(),
                None => clip.text.clone(),
            };
            i += 1;
            text
        });
    }

    // ---------------------------------------------------------------------
    // Line operations
    // ---------------------------------------------------------------------

    /// Line range touched by a selection. A selection that ends at column 0
    /// of a later line does not touch that line.
    fn touched_lines(&self, sel: Selection) -> Range<usize> {
        let first = self.doc.char_to_line(sel.start());
        let mut last = self.doc.char_to_line(sel.end());
        if last > first && sel.end() == self.doc.line_start(last) {
            last -= 1;
        }
        first..last + 1
    }

    /// Unique touched lines across all selections, in order.
    fn all_touched_lines(&self) -> Vec<usize> {
        let mut lines: Vec<usize> = Vec::new();
        for sel in self.selections.iter() {
            for line in self.touched_lines(*sel) {
                if lines.last().is_none_or(|l| *l < line) {
                    lines.push(line);
                }
            }
        }
        lines
    }

    /// Merged blocks of touched lines.
    fn line_blocks(&self) -> Vec<Range<usize>> {
        let mut blocks: Vec<Range<usize>> = Vec::new();
        for sel in self.selections.iter() {
            let r = self.touched_lines(*sel);
            match blocks.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => blocks.push(r),
            }
        }
        blocks
    }

    /// Maps selections through `changes`, keeping edges that sit at a line
    /// start in place (so indenting a full-line selection keeps it whole).
    fn map_keeping_line_starts(&self, changes: &ChangeSet) -> SelectionSet {
        let map = |pos: usize| {
            let at_line_start = pos == self.doc.line_start(self.doc.char_to_line(pos));
            changes.map_pos(
                pos,
                if at_line_start {
                    Assoc::Before
                } else {
                    Assoc::After
                },
            )
        };
        SelectionSet::new(
            self.selections
                .iter()
                .map(|s| Selection::new(map(s.anchor), map(s.head)))
                .collect(),
            self.selections.primary_index(),
        )
    }

    /// Tab: indents touched lines when any selection spans lines, otherwise
    /// inserts indentation at each cursor.
    pub fn tab(&mut self) {
        let multi_line = self
            .selections
            .iter()
            .any(|s| self.touched_lines(*s).len() > 1);
        if multi_line {
            self.indent_lines();
            return;
        }
        if self.hard_tabs {
            self.replace_each(EditKind::Insert, |_, _| "\t".into());
        } else {
            let tab_width = self.tab_width;
            self.replace_each(EditKind::Insert, |this, sel| {
                let (line, col) = this.doc.line_col(sel.start());
                let vcol = visual_col(this.doc.line(line), col, tab_width);
                " ".repeat(tab_width - vcol % tab_width)
            });
        }
    }

    pub fn indent_lines(&mut self) {
        let unit = self.indent_unit();
        let changes: Vec<Change> = self
            .all_touched_lines()
            .into_iter()
            .filter(|&line| self.doc.line_len(line) > 0)
            .map(|line| Change::insert(self.doc.line_start(line), unit.clone()))
            .collect();
        let changes = ChangeSet::new(changes);
        let after = self.map_keeping_line_starts(&changes);
        self.commit(changes, after, EditKind::Other);
    }

    pub fn outdent_lines(&mut self) {
        let tab_width = self.tab_width;
        let changes: Vec<Change> = self
            .all_touched_lines()
            .into_iter()
            .filter_map(|line| {
                let start = self.doc.line_start(line);
                let mut width = 0;
                let mut n = 0;
                for ch in self.doc.line(line).chars() {
                    match ch {
                        '\t' if n == 0 => {
                            n = 1;
                            break;
                        }
                        ' ' if width < tab_width => {
                            width += 1;
                            n += 1;
                        }
                        _ => break,
                    }
                }
                (n > 0).then(|| Change::delete(start, start + n))
            })
            .collect();
        let changes = ChangeSet::new(changes);
        let after = self.map_keeping_line_starts(&changes);
        self.commit(changes, after, EditKind::Other);
    }

    /// Cmd+/ with the language's line comment token (e.g. "//", "#").
    pub fn toggle_line_comment(&mut self, token: &str) {
        let lines: Vec<usize> = self
            .all_touched_lines()
            .into_iter()
            .filter(|&l| !self.doc.line(l).chars().all(char::is_whitespace))
            .collect();
        if lines.is_empty() {
            return;
        }
        let all_commented = lines.iter().all(|&l| {
            let text = self.doc.line(l).to_string();
            text.trim_start().starts_with(token)
        });
        let changes: Vec<Change> = if all_commented {
            lines
                .iter()
                .map(|&l| {
                    let text = self.doc.line(l).to_string();
                    let indent = text.chars().take_while(|c| c.is_whitespace()).count();
                    let mut len = token.chars().count();
                    if text.chars().nth(indent + len) == Some(' ') {
                        len += 1;
                    }
                    let start = self.doc.line_start(l) + indent;
                    Change::delete(start, start + len)
                })
                .collect()
        } else {
            let min_indent = lines
                .iter()
                .map(|&l| {
                    self.doc
                        .line(l)
                        .chars()
                        .take_while(|c| c.is_whitespace())
                        .count()
                })
                .min()
                .unwrap_or(0);
            lines
                .iter()
                .map(|&l| Change::insert(self.doc.line_start(l) + min_indent, format!("{token} ")))
                .collect()
        };
        let changes = ChangeSet::new(changes);
        let after = self.map_keeping_line_starts(&changes);
        self.commit(changes, after, EditKind::Other);
    }

    /// Ctrl+Shift+K.
    pub fn delete_lines(&mut self) {
        let len_lines = self.doc.len_lines();
        let mut changes = Vec::new();
        let mut targets = Vec::new();
        for block in self.line_blocks() {
            let mut start = self.doc.line_start(block.start);
            let end = self.doc.line_start_after(block.end - 1);
            // Deleting the last line: take the line break before it instead.
            if block.end >= len_lines && block.start > 0 {
                start = self.doc.line_end(block.start - 1);
            }
            changes.push(Change::delete(start, end));
            targets.push(start);
        }
        let changes = ChangeSet::new(changes);
        let after = cursors(
            targets
                .into_iter()
                .map(|t| changes.map_pos(t, Assoc::Before)),
            0,
        );
        self.commit(changes, after, EditKind::Other);
        // Land on the first non-whitespace char of the line that moved up.
        let sels: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                let line = self.doc.char_to_line(s.head);
                Selection::cursor(self.doc.line_start(line))
            })
            .collect();
        self.selections = SelectionSet::new(sels, 0);
    }

    /// Cmd+Shift+D: duplicates touched lines (bare cursors) or the selected
    /// text (non-empty selections).
    pub fn duplicate(&mut self) {
        let eol = self.doc.line_ending().as_str().to_string();
        if self.selections.iter().all(|s| s.is_empty()) {
            let mut changes = Vec::new();
            for block in self.line_blocks() {
                let start = self.doc.line_start(block.start);
                let end = self.doc.line_end(block.end - 1);
                let text = self.doc.text(start..end);
                changes.push(Change::insert(end, format!("{eol}{text}")));
            }
            let changes = ChangeSet::new(changes);
            // Cursors move down onto the copy.
            let eol_len = eol.chars().count();
            let after = SelectionSet::new(
                self.selections
                    .iter()
                    .map(|s| {
                        let block = self.touched_lines(*s);
                        let block_len =
                            self.doc.line_end(block.end - 1) - self.doc.line_start(block.start);
                        let pos = changes.map_pos(s.head, Assoc::Before) + block_len + eol_len;
                        Selection::cursor(pos)
                    })
                    .collect(),
                self.selections.primary_index(),
            );
            self.commit(changes, after, EditKind::Other);
        } else {
            let sels: Vec<Selection> = self.selections.iter().copied().collect();
            let changes = ChangeSet::new(
                sels.iter()
                    .map(|s| Change::insert(s.end(), self.doc.text(s.range())))
                    .collect(),
            );
            let after = SelectionSet::new(
                sels.iter()
                    .map(|s| {
                        let start = changes.map_pos(s.end(), Assoc::Before);
                        Selection::new(start, start + s.end() - s.start())
                    })
                    .collect(),
                self.selections.primary_index(),
            );
            self.commit(changes, after, EditKind::Other);
        }
    }

    /// Ctrl+Cmd+Up / Ctrl+Cmd+Down.
    pub fn move_lines(&mut self, down: bool) {
        let blocks = self.line_blocks();
        let len_lines = self.doc.len_lines();
        if down && blocks.last().is_some_and(|b| b.end >= len_lines) {
            return;
        }
        if !down && blocks.first().is_some_and(|b| b.start == 0) {
            return;
        }
        let eol = self.doc.line_ending().as_str();
        let eol_len = eol.chars().count();
        let line_text = |l: usize| self.doc.line(l).to_string();
        let mut changes = Vec::new();
        // (block, shift) for moving selections.
        let mut shifts: Vec<(Range<usize>, isize)> = Vec::new();
        for block in &blocks {
            let block_text: Vec<String> = block.clone().map(line_text).collect();
            let (region_lines, other_line) = if down {
                (block.start..block.end + 1, block.end)
            } else {
                (block.start - 1..block.end, block.start - 1)
            };
            let other = line_text(other_line);
            let mut new_lines = block_text.clone();
            if down {
                new_lines.insert(0, other.clone());
            } else {
                new_lines.push(other.clone());
            }
            let start = self.doc.line_start(region_lines.start);
            let end = self.doc.line_end(region_lines.end - 1);
            changes.push(Change::new(start, end, new_lines.join(eol)));
            let shift = (other.chars().count() + eol_len) as isize;
            shifts.push((block.clone(), if down { shift } else { -shift }));
        }
        let changes = ChangeSet::new(changes);
        let after = SelectionSet::new(
            self.selections
                .iter()
                .map(|s| {
                    let line = self.doc.char_to_line(s.start());
                    let shift = shifts
                        .iter()
                        .find(|(b, _)| b.contains(&line))
                        .map(|(_, d)| *d)
                        .unwrap_or(0);
                    let m = |p: usize| (p as isize + shift) as usize;
                    Selection::new(m(s.anchor), m(s.head))
                })
                .collect(),
            self.selections.primary_index(),
        );
        self.commit(changes, after, EditKind::Other);
    }

    // ---------------------------------------------------------------------
    // Undo
    // ---------------------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        match self.doc.undo() {
            Some(sels) => {
                self.selections = sels;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.doc.redo() {
            Some(sels) => {
                self.selections = sels;
                true
            }
            None => false,
        }
    }

    // ---------------------------------------------------------------------
    // Movement
    // ---------------------------------------------------------------------

    fn move_each(&mut self, extend: bool, mut f: impl FnMut(&Self, Selection) -> Selection) {
        let moved: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                let target = f(self, *s);
                if extend {
                    Selection {
                        anchor: s.anchor,
                        head: target.head,
                        goal: target.goal,
                    }
                } else {
                    Selection::cursor(target.head).with_goal(target.goal)
                }
            })
            .collect();
        self.selections = SelectionSet::new(moved, self.selections.primary_index());
        self.doc.seal_undo();
    }

    pub fn move_left(&mut self, extend: bool) {
        self.move_each(extend, |this, s| {
            let head = if !extend && !s.is_empty() {
                s.start()
            } else {
                prev_grapheme_boundary(this.doc.rope().slice(..), s.head)
            };
            Selection::cursor(head)
        });
    }

    pub fn move_right(&mut self, extend: bool) {
        self.move_each(extend, |this, s| {
            let head = if !extend && !s.is_empty() {
                s.end()
            } else {
                next_grapheme_boundary(this.doc.rope().slice(..), s.head)
            };
            Selection::cursor(head)
        });
    }

    pub fn move_word_left(&mut self, extend: bool) {
        self.move_each(extend, |this, s| {
            Selection::cursor(prev_word_start(this.doc.rope().slice(..), s.head))
        });
    }

    pub fn move_word_right(&mut self, extend: bool) {
        self.move_each(extend, |this, s| {
            Selection::cursor(next_word_end(this.doc.rope().slice(..), s.head))
        });
    }

    /// Up/down by `lines` (negative is up), keeping the goal column.
    pub fn move_vertically(&mut self, lines: isize, extend: bool) {
        let tab_width = self.tab_width;
        self.move_each(extend, |this, s| {
            let (line, col) = this.doc.line_col(s.head);
            let goal = s
                .goal
                .unwrap_or_else(|| visual_col(this.doc.line(line), col, tab_width));
            let target = line as isize + lines;
            if target < 0 {
                return Selection::cursor(0).with_goal(Some(goal));
            }
            let target = target as usize;
            if target >= this.doc.len_lines() {
                return Selection::cursor(this.doc.len_chars()).with_goal(Some(goal));
            }
            let (c, _) = char_col_at_visual(this.doc.line(target), goal, tab_width);
            Selection::cursor(this.doc.line_start(target) + c).with_goal(Some(goal))
        });
    }

    pub fn move_up(&mut self, extend: bool) {
        self.move_vertically(-1, extend);
    }

    pub fn move_down(&mut self, extend: bool) {
        self.move_vertically(1, extend);
    }

    /// Cmd+Left: first non-whitespace char, then column 0.
    pub fn move_line_start(&mut self, extend: bool) {
        self.move_each(extend, |this, s| {
            let line = this.doc.char_to_line(s.head);
            let start = this.doc.line_start(line);
            let indent = indentation(this.doc.line(line)).chars().count();
            let smart = start + indent;
            Selection::cursor(if s.head == smart { start } else { smart })
        });
    }

    pub fn move_line_end(&mut self, extend: bool) {
        self.move_each(extend, |this, s| {
            Selection::cursor(this.doc.line_end(this.doc.char_to_line(s.head)))
        });
    }

    pub fn move_doc_start(&mut self, extend: bool) {
        self.move_each(extend, |_, _| Selection::cursor(0));
    }

    pub fn move_doc_end(&mut self, extend: bool) {
        self.move_each(extend, |this, _| Selection::cursor(this.doc.len_chars()));
    }

    // ---------------------------------------------------------------------
    // Selection commands
    // ---------------------------------------------------------------------

    pub fn select_all(&mut self) {
        self.set_selections(SelectionSet::single(Selection::new(
            0,
            self.doc.len_chars(),
        )));
    }

    /// Esc: back to one selection; then to a bare cursor.
    pub fn collapse_selections(&mut self) {
        if self.selections.len() > 1 {
            let primary = self.selections.primary();
            self.set_selections(SelectionSet::single(primary));
        } else {
            let head = self.selections.primary().head;
            self.set_selections(SelectionSet::single(Selection::cursor(head)));
        }
    }

    /// Cmd+L: expand every selection to whole lines; repeat to extend down.
    pub fn select_line(&mut self) {
        let sels: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                let start = self.doc.line_start(self.doc.char_to_line(s.start()));
                let end = self.doc.line_start_after(self.doc.char_to_line(s.end()));
                Selection::new(start, end)
            })
            .collect();
        self.selections = SelectionSet::new(sels, self.selections.primary_index());
    }

    /// Cmd+Shift+L: split every multi-line selection into one selection per
    /// line.
    pub fn split_into_lines(&mut self) {
        let doc = &self.doc;
        let mut out = Vec::new();
        for s in self.selections.iter() {
            if s.is_empty() {
                out.push(*s);
                continue;
            }
            for line in self.touched_lines(*s) {
                let start = s.start().max(doc.line_start(line));
                let end = s.end().min(doc.line_end(line));
                out.push(Selection::new(start, end.max(start)));
            }
        }
        let primary = out.len() - 1;
        self.selections = SelectionSet::new(out, primary);
        self.occurrence_word = None;
    }

    /// Expands every bare cursor to the word under it. Returns false if the
    /// primary cursor isn't on a word.
    fn expand_cursors_to_words(&mut self) -> bool {
        let rope = self.doc.rope().slice(..);
        let sels: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                if s.is_empty() {
                    let r = word_range_at(rope, s.head);
                    Selection::new(r.start, r.end)
                } else {
                    *s
                }
            })
            .collect();
        self.selections = SelectionSet::new(sels, self.selections.primary_index());
        let primary = self.selections.primary();
        if primary.is_empty() {
            return false;
        }
        self.occurrence_word = Some(self.doc.text(primary.range()));
        true
    }

    fn occurrence_query(&self) -> Option<SearchQuery> {
        let primary = self.selections.primary();
        if primary.is_empty() {
            return None;
        }
        let needle = self.doc.text(primary.range());
        let whole_word = self.occurrence_word.as_deref() == Some(needle.as_str());
        Some(SearchQuery::literal(needle).whole_word(whole_word))
    }

    /// Cmd+D: select the word under the cursor, then add the next occurrence
    /// of the primary selection's text.
    pub fn add_next_occurrence(&mut self) -> bool {
        if self.selections.primary().is_empty() {
            return self.expand_cursors_to_words();
        }
        let Some(query) = self.occurrence_query() else {
            return false;
        };
        let Ok(matcher) = query.compile() else {
            return false;
        };
        let rope = self.doc.rope();
        let mut from = self.selections.primary().end();
        // Skip matches that are already selected; give up after a full lap.
        for _ in 0..self.selections.len() + 1 {
            let Some(found) = matcher.find_next(rope, from) else {
                return false;
            };
            let taken = self.selections.iter().any(|s| s.range() == found);
            if !taken {
                let word = self.occurrence_word.take();
                self.selections.push(Selection::new(found.start, found.end));
                self.occurrence_word = word;
                return true;
            }
            from = found.end;
        }
        false
    }

    /// Cmd+K Cmd+D: drop the primary selection and select the next
    /// occurrence instead.
    pub fn skip_occurrence(&mut self) -> bool {
        let primary = self.selections.primary();
        if primary.is_empty() {
            return self.add_next_occurrence();
        }
        let Some(query) = self.occurrence_query() else {
            return false;
        };
        let Ok(matcher) = query.compile() else {
            return false;
        };
        let Some(found) = matcher.find_next(self.doc.rope(), primary.end()) else {
            return false;
        };
        if self.selections.iter().any(|s| s.range() == found) {
            return false;
        }
        let word = self.occurrence_word.take();
        if self.selections.len() > 1 {
            self.selections.remove_primary();
            self.selections.push(Selection::new(found.start, found.end));
        } else {
            self.selections = SelectionSet::single(Selection::new(found.start, found.end));
        }
        self.occurrence_word = word;
        true
    }

    /// Ctrl+Cmd+G: select every occurrence of the primary selection (or of the
    /// word under the cursor).
    pub fn select_all_occurrences(&mut self) -> bool {
        if self.selections.primary().is_empty() && !self.expand_cursors_to_words() {
            return false;
        }
        let Some(query) = self.occurrence_query() else {
            return false;
        };
        self.select_all_matches(&query)
    }

    /// Turns every match of `query` into a selection (Alt+Enter in the find
    /// bar). Keeps the match nearest the old primary as primary.
    pub fn select_all_matches(&mut self, query: &SearchQuery) -> bool {
        let Ok(matcher) = query.compile() else {
            return false;
        };
        let len = self.doc.len_chars();
        let matches = matcher.find_all(self.doc.rope(), 0..len, usize::MAX);
        if matches.is_empty() {
            return false;
        }
        let anchor = self.selections.primary().start();
        let primary = matches.iter().position(|m| m.start >= anchor).unwrap_or(0);
        let word = self.occurrence_word.take();
        self.selections = SelectionSet::new(
            matches
                .into_iter()
                .map(|m| Selection::new(m.start, m.end))
                .collect(),
            primary,
        );
        self.occurrence_word = word;
        true
    }

    /// Selects the next (or previous) match of `query` from the primary
    /// selection. Returns the match.
    pub fn find(&mut self, query: &SearchQuery, backwards: bool) -> Option<Range<usize>> {
        let matcher = query.compile().ok()?;
        let primary = self.selections.primary();
        let found = if backwards {
            matcher.find_prev(self.doc.rope(), primary.start())
        } else {
            matcher.find_next(self.doc.rope(), primary.end())
        }?;
        self.set_selections(SelectionSet::single(Selection::new(found.start, found.end)));
        Some(found)
    }

    /// Replaces every match of `query` in the document as one undo step.
    /// Returns the number of replacements.
    pub fn replace_all(&mut self, query: &SearchQuery, replacement: &str) -> usize {
        let Ok(matcher) = query.compile() else {
            return 0;
        };
        let len = self.doc.len_chars();
        let reps = matcher.replacements(self.doc.rope(), 0..len, replacement, query.regex);
        let count = reps.len();
        let changes = ChangeSet::new(
            reps.into_iter()
                .map(|(r, t)| Change::new(r.start, r.end, t))
                .collect(),
        );
        let mut after = self.selections.clone();
        after.map(&changes);
        self.commit(changes, after, EditKind::Other);
        count
    }

    /// Replaces the primary selection if it is a match of `query`, then
    /// selects the next match.
    pub fn replace_next(&mut self, query: &SearchQuery, replacement: &str) -> bool {
        let Ok(matcher) = query.compile() else {
            return false;
        };
        let primary = self.selections.primary();
        let reps = matcher.replacements(self.doc.rope(), primary.range(), replacement, query.regex);
        if let Some((range, text)) = reps.into_iter().find(|(r, _)| *r == primary.range()) {
            let changes = ChangeSet::new(vec![Change::new(range.start, range.end, text)]);
            let end = changes.map_pos(range.end, Assoc::After);
            self.commit(
                changes,
                SelectionSet::single(Selection::cursor(end)),
                EditKind::Other,
            );
        }
        self.find(query, false).is_some()
    }

    /// Ctrl+Shift+Up/Down, Cmd+Alt+Up/Down: add a cursor on the line above the
    /// first selection (or below the last one).
    pub fn add_cursor_vertically(&mut self, below: bool) -> bool {
        let tab_width = self.tab_width;
        let edge = if below {
            self.selections.last()
        } else {
            self.selections.first()
        };
        let (line, col) = self.doc.line_col(edge.head);
        let goal = edge
            .goal
            .unwrap_or_else(|| visual_col(self.doc.line(line), col, tab_width));
        let target = if below {
            if line + 1 >= self.doc.len_lines() {
                return false;
            }
            line + 1
        } else {
            if line == 0 {
                return false;
            }
            line - 1
        };
        let (c, _) = char_col_at_visual(self.doc.line(target), goal, tab_width);
        let pos = self.doc.line_start(target) + c;
        self.selections
            .push(Selection::cursor(pos).with_goal(Some(goal)));
        true
    }

    /// Cmd+click: add a cursor, or remove the selection under `pos` when
    /// there are several.
    pub fn toggle_cursor(&mut self, pos: usize) {
        let pos = pos.min(self.doc.len_chars());
        if self.selections.len() > 1 {
            let hit: Vec<Selection> = self
                .selections
                .iter()
                .copied()
                .filter(|s| !(s.start() <= pos && pos <= s.end()))
                .collect();
            if hit.len() < self.selections.len() && !hit.is_empty() {
                let primary = hit.len() - 1;
                self.selections = SelectionSet::new(hit, primary);
                return;
            }
        }
        self.selections.push(Selection::cursor(pos));
        self.doc.seal_undo();
    }

    /// Option+drag: a rectangular selection between two (line, visual column)
    /// corners. Lines shorter than the left edge are skipped, unless the
    /// rectangle has zero width (then every line gets a cursor).
    pub fn select_column(&mut self, anchor: (usize, usize), head: (usize, usize)) {
        let tab_width = self.tab_width;
        let last_line = self.doc.len_lines().saturating_sub(1);
        let (a_line, a_col) = (anchor.0.min(last_line), anchor.1);
        let (h_line, h_col) = (head.0.min(last_line), head.1);
        let (left, right) = (a_col.min(h_col), a_col.max(h_col));
        let reversed = h_col < a_col;
        let lines: Vec<usize> = if a_line <= h_line {
            (a_line..=h_line).collect()
        } else {
            (h_line..=a_line).rev().collect()
        };

        let mut sels = Vec::new();
        for &line in &lines {
            let text = self.doc.line(line);
            let (l, beyond) = char_col_at_visual(text, left, tab_width);
            if beyond && left != right {
                continue;
            }
            let (r, _) = char_col_at_visual(text, right, tab_width);
            let start = self.doc.line_start(line);
            sels.push(if reversed {
                Selection::new(start + r, start + l)
            } else {
                Selection::new(start + l, start + r)
            });
        }
        if sels.is_empty() {
            let (c, _) = char_col_at_visual(self.doc.line(h_line), h_col, tab_width);
            sels.push(Selection::cursor(self.doc.line_start(h_line) + c));
        }
        // The head line's selection is primary; it's the last one we pushed
        // in drag order.
        let primary = sels.len() - 1;
        self.selections = SelectionSet::new(sels, primary);
        self.occurrence_word = None;
        self.doc.seal_undo();
    }

    /// Ranges a double click (word) or triple click (line) selects.
    pub fn word_range_at(&self, pos: usize) -> Range<usize> {
        word_range_at(self.doc.rope().slice(..), pos)
    }

    pub fn line_range_at(&self, pos: usize) -> Range<usize> {
        let line = self.doc.char_to_line(pos);
        self.doc.line_start(line)..self.doc.line_start_after(line)
    }
}

/// Builds a set of cursors from positions.
fn cursors(positions: impl Iterator<Item = usize>, primary: usize) -> SelectionSet {
    let sels: Vec<Selection> = positions.map(Selection::cursor).collect();
    if sels.is_empty() {
        return SelectionSet::default();
    }
    let primary = primary.min(sels.len() - 1);
    SelectionSet::new(sels, primary)
}

/// Guess whether a file is indented with tabs by looking at its first lines.
fn detect_hard_tabs(doc: &Document) -> bool {
    let mut tabs = 0;
    let mut spaces = 0;
    for line in doc.rope().lines().take(1000) {
        match line.chars().next() {
            Some('\t') => tabs += 1,
            Some(' ') => spaces += 1,
            _ => {}
        }
    }
    tabs > spaces
}
