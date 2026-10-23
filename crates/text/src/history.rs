//! Undo/redo history.
//!
//! Each undo step is a *group* of change sets together with the selections
//! before and after. Consecutive typing (or deleting) within a short window is
//! coalesced into one group, as in every native editor.

use std::time::{Duration, Instant};

use crate::change::ChangeSet;
use crate::selection::SelectionSet;

/// What kind of edit produced a change, for coalescing undo steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Typing characters. Consecutive inserts coalesce.
    Insert,
    /// Backspace/delete. Consecutive deletes coalesce.
    Delete,
    /// Everything else (paste, indent, replace, ...). Never coalesces.
    Other,
}

const COALESCE_WINDOW: Duration = Duration::from_millis(1000);

#[derive(Clone, Debug)]
struct Group {
    /// Forward change sets, in application order.
    forward: Vec<ChangeSet>,
    /// Inverse change sets, in application order (undo applies them reversed).
    inverse: Vec<ChangeSet>,
    selections_before: SelectionSet,
    selections_after: SelectionSet,
    kind: EditKind,
    at: Instant,
    revision_before: u64,
    revision_after: u64,
}

/// One step returned by [`History::undo`] / [`History::redo`]: the change
/// sets to apply in order, and the selections to restore afterwards.
#[derive(Debug)]
pub struct Step {
    pub changes: Vec<ChangeSet>,
    pub selections: SelectionSet,
    pub revision: u64,
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Group>,
    redo: Vec<Group>,
    last_revision: u64,
    sealed: bool,
}

impl History {
    /// Records an applied change and returns the new document revision.
    pub fn record(
        &mut self,
        forward: ChangeSet,
        inverse: ChangeSet,
        selections_before: SelectionSet,
        selections_after: SelectionSet,
        kind: EditKind,
        current_revision: u64,
    ) -> u64 {
        self.redo.clear();
        self.last_revision += 1;
        let revision = self.last_revision;
        let now = Instant::now();

        if !self.sealed
            && kind != EditKind::Other
            && let Some(last) = self.undo.last_mut()
            && last.kind == kind
            && last.revision_after == current_revision
            && now.duration_since(last.at) < COALESCE_WINDOW
            && last.selections_after == selections_before
        {
            last.forward.push(forward);
            last.inverse.push(inverse);
            last.selections_after = selections_after;
            last.at = now;
            last.revision_after = revision;
            return revision;
        }

        self.sealed = false;
        self.undo.push(Group {
            forward: vec![forward],
            inverse: vec![inverse],
            selections_before,
            selections_after,
            kind,
            at: now,
            revision_before: current_revision,
            revision_after: revision,
        });
        revision
    }

    /// Stops the next edit from coalescing with the previous one (e.g. after
    /// the user clicked somewhere or saved).
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> Option<Step> {
        let group = self.undo.pop()?;
        let step = Step {
            changes: group.inverse.iter().rev().cloned().collect(),
            selections: group.selections_before.clone(),
            revision: group.revision_before,
        };
        self.redo.push(group);
        self.sealed = true;
        Some(step)
    }

    pub fn redo(&mut self) -> Option<Step> {
        let group = self.redo.pop()?;
        let step = Step {
            changes: group.forward.clone(),
            selections: group.selections_after.clone(),
            revision: group.revision_after,
        };
        self.undo.push(group);
        self.sealed = true;
        Some(step)
    }
}
