//! Editing cells. The new value is written into the file's text in the
//! editor, quoted as the file needs, so undo, save and the source view all
//! work as for typing.

use std::time::Instant;

use gpui::{AppContext, Context, Entity, SharedString, Subscription, Window};
use gpui_component::input::{InputEvent, InputState};

use super::CsvPreview;
use super::parse::{self, MAX_FIELD_BYTES};

/// The cell being edited and its text box.
pub(super) struct Editing {
    /// The record, header row included.
    pub record: usize,
    pub col: usize,
    pub original: String,
    pub input: Entity<InputState>,
    _subscription: Subscription,
}

/// Cells cleared by one Delete, at most.
const MAX_CLEARED_CELLS: usize = 100_000;

impl CsvPreview {
    /// Whether cells can be changed: the table shows the editor's own text
    /// and the file isn't read-only.
    pub(super) fn editable(&self, cx: &gpui::App) -> bool {
        self.source.as_ref().is_some_and(|s| s.from_editor)
            && !self.editor.read(cx).is_read_only()
            && self.index.is_some()
    }

    /// Edits the active cell. `typed` replaces its value (typing on a
    /// selected cell); otherwise the value is kept with all of it selected.
    pub(super) fn edit_active(
        &mut self,
        typed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.selection else {
            return;
        };
        let (row, col) = selection.anchor;
        let record = row + self.has_header() as usize;
        self.edit_record(record, col, typed, window, cx);
    }

    pub(super) fn edit_record(
        &mut self,
        record: usize,
        col: usize,
        typed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable(cx) {
            self.hint("This file is read-only here", cx);
            return;
        }
        let value = self
            .records_range(record, 1)
            .first()
            .and_then(|r| r.get(col).cloned())
            .unwrap_or_default();
        if value.len() >= MAX_FIELD_BYTES || value.contains('\n') {
            self.hint("Long and multi-line cells are edited in source (⇧⌘V)", cx);
            return;
        }
        let original = value.to_string();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            match &typed {
                Some(text) => state.set_value(text.clone(), window, cx),
                None => {
                    state.set_value(original.clone(), window, cx);
                    state.set_selected_range(0..original.len(), cx);
                }
            }
            state
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { shift, .. } => {
                    this.commit_edit(if *shift { -1 } else { 1 }, 0, window, cx)
                }
                // Clicking elsewhere keeps what was typed.
                InputEvent::Blur => this.commit_edit(0, 0, window, cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.editing = Some(Editing {
            record,
            col,
            original,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Writes the edited value (if it changed) and moves the selection by
    /// `rows` and `cols`, like Enter and Tab in a spreadsheet.
    pub(super) fn commit_edit(
        &mut self,
        rows: isize,
        cols: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let value = editing.input.read(cx).value().to_string();
        if value != editing.original {
            self.write_cells(vec![(editing.record, editing.col, value)], cx);
        }
        window.focus(&self.focus_handle, cx);
        if rows != 0 || cols != 0 {
            self.move_by(rows, cols, false, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Empties the selected cells (Delete), as one undoable change.
    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.selection else {
            return;
        };
        if !self.editable(cx) {
            self.hint("This file is read-only here", cx);
            return;
        }
        let (rows, cols) = (
            selection.rows(self.rows()),
            selection.cols(self.column_count()),
        );
        if rows.len() * cols.len() > MAX_CLEARED_CELLS {
            self.hint("Too many cells to clear at once", cx);
            return;
        }
        let header = self.has_header() as usize;
        let cells = rows
            .flat_map(|row| {
                cols.clone()
                    .map(move |col| (row + header, col, String::new()))
            })
            .collect();
        self.write_cells(cells, cx);
    }

    /// Puts each `(record, column, value)` into the text in one undoable
    /// step. Columns past the end of a record are added, except when
    /// clearing (an empty value there changes nothing).
    fn write_cells(&mut self, cells: Vec<(usize, usize, String)>, cx: &mut Context<Self>) {
        let (Some(index), Some(source)) = (self.index.clone(), &self.source) else {
            return;
        };
        let dialect = self.dialect();
        let rope = &source.rope;
        let mut edits = Vec::new();
        let mut current: Option<(usize, parse::RawRecord)> = None;
        for (record, col, value) in cells {
            if current.as_ref().is_none_or(|(r, _)| *r != record) {
                let raw = index
                    .record_start(rope, dialect, record)
                    .and_then(|start| parse::raw_record(rope, dialect, start));
                let Some(raw) = raw else { continue };
                current = Some((record, raw));
            }
            let (_, raw) = current.as_ref().unwrap();
            if value.is_empty() && col >= raw.fields.len() {
                continue;
            }
            let (range, text) = parse::field_edit(raw, col, &value, dialect);
            let chars = rope.byte_to_char(range.start)..rope.byte_to_char(range.end);
            edits.push((chars, text));
        }
        if edits.is_empty() {
            return;
        }
        self.editor.update(cx, |e, cx| e.apply_edits(edits, cx));
        // Show the new text now rather than after the usual delay.
        self.refresh = None;
        self.sync_with_editor(cx);
    }

    /// A short note in the status bar, for an edit that can't be made.
    pub(super) fn hint(&mut self, text: &str, cx: &mut Context<Self>) {
        self.hint = Some((SharedString::from(text.to_string()), Instant::now()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(super::HINT_FOR).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }
}
