//! Undo and redo, by inverse operations: each undo step keeps the [`Edit`] that undoes it, as
//! [`florastack_project::Project::apply`] returned it, and undoing applies that edit, which returns the edit that
//! redoes it. A retouch stroke is one step whose inverse removes that one stroke, so strokes undo one at a time
//! without a copy of the project per step.
//!
//! - One command is one step: the edits a command (or a job's commit) makes are merged into the step it started.
//! - Calls of the same command with the same coalescing key, one right after the other (no undo, redo or other change
//!   in between), merge too, so that a slider drag is one step. Of a run of edits that each set a value whole, only the
//!   oldest inverse is kept ([`Edit::overwrites`]).
//! - A new step clears the redo steps; the oldest steps go beyond [`LIMIT`].
//! - The history isn't saved with the project, and saving doesn't change it. The project is dirty when its state isn't
//!   the one last saved: undoing back to the saved state makes it clean again. Bookkeeping that isn't a step of its own
//!   (a system edit) makes it dirty until the next save.

use florastack_project::Edit;

/// Steps as their labels and the commands that made them.
pub type StepList = Vec<(String, &'static str)>;

/// The most undo steps kept.
pub const LIMIT: usize = 1000;

/// One undo or redo step.
#[derive(Clone, Debug)]
pub(crate) struct Step {
    /// What the user sees: the command's title ("Add frames").
    pub(crate) label: String,
    pub(crate) command: &'static str,
    /// The edit that undoes the step (or, on the redo stack, redoes it).
    pub(crate) edit: Edit,
    pub(crate) coalesce: Option<String>,
    /// The project's state before and after the step.
    pub(crate) before: u64,
    pub(crate) after: u64,
}

/// The undo and redo steps of the open project. See the module documentation.
#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    pub(crate) undo: Vec<Step>,
    pub(crate) redo: Vec<Step>,
    /// The project's current state, and the next state id to give out.
    current: u64,
    next: u64,
    saved: Option<u64>,
    unsaved_system: bool,
    /// Whether the last step was made just now, with nothing since that a coalesced call may not merge across.
    mergeable: bool,
}

impl History {
    /// The history of a project just opened (saved as it is).
    pub(crate) fn new() -> History {
        History { undo: Vec::new(), redo: Vec::new(), current: 0, next: 1, saved: Some(0), unsaved_system: false, mergeable: false }
    }

    fn fresh(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        id
    }

    /// Records a new step whose inverse is `inverse`.
    pub(crate) fn record(&mut self, label: &str, command: &'static str, inverse: Edit, coalesce: Option<String>) {
        let before = self.current;
        let after = self.fresh();
        self.undo.push(Step { label: label.to_owned(), command, edit: inverse, coalesce, before, after });
        self.current = after;
        self.redo.clear();
        self.mergeable = true;
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
    }

    /// Whether a call of `command` with this coalescing key merges into the last step.
    pub(crate) fn coalesces(&self, command: &str, key: Option<&str>) -> bool {
        key.is_some()
            && self.mergeable
            && self.undo.last().is_some_and(|s| s.after == self.current && s.command == command && s.coalesce.as_deref() == key)
    }

    /// Merges an edit made after the last step into it: undoing the step now undoes `inverse` first.
    pub(crate) fn merge(&mut self, inverse: Edit) {
        let after = self.fresh();
        let Some(step) = self.undo.last_mut() else { return };
        let older = std::mem::replace(&mut step.edit, Edit::Batch(Vec::new()));
        let mut parts = match older {
            Edit::Batch(parts) => parts,
            other => vec![other],
        };
        // When the older inverse sets the same value whole, it already restores what this one would.
        if !parts.first().is_some_and(|first| first.overwrites(&inverse)) {
            parts.insert(0, inverse);
        }
        step.edit = if parts.len() == 1 { parts.pop().unwrap_or(Edit::Batch(Vec::new())) } else { Edit::Batch(parts) };
        step.after = after;
        self.redo.clear();
        self.current = after;
        self.mergeable = true;
    }

    /// The step to undo, taken off the stack.
    pub(crate) fn take_undo(&mut self) -> Option<Step> {
        self.undo.pop()
    }

    /// Puts back a step whose undo failed.
    pub(crate) fn restore_undo(&mut self, step: Step) {
        self.undo.push(step);
    }

    /// Records an undone step: `redo` is the edit that redoes it.
    pub(crate) fn undone(&mut self, step: Step, redo: Edit) {
        self.current = step.before;
        self.mergeable = false;
        self.redo.push(Step { edit: redo, ..step });
    }

    pub(crate) fn take_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    pub(crate) fn restore_redo(&mut self, step: Step) {
        self.redo.push(step);
    }

    /// Records a redone step: `undo` is the edit that undoes it again.
    pub(crate) fn redone(&mut self, step: Step, undo: Edit) {
        self.current = step.after;
        self.mergeable = false;
        self.undo.push(Step { edit: undo, ..step });
    }

    /// A change that is no step (bookkeeping): the project needs saving.
    pub(crate) fn system_edit(&mut self) {
        self.unsaved_system = true;
        self.mergeable = false;
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved = Some(self.current);
        self.unsaved_system = false;
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.unsaved_system || self.saved != Some(self.current)
    }

    pub(crate) fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub(crate) fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// The undo steps (newest first) and the redo steps (next first), as their labels and the commands that made them.
    pub(crate) fn steps(&self) -> (StepList, StepList) {
        let list = |steps: &[Step]| steps.iter().rev().map(|s| (s.label.clone(), s.command)).collect();
        (list(&self.undo), list(&self.redo))
    }
}

#[cfg(test)]
mod tests {
    use florastack_project::{Group, Project, Table};
    use serde_json::{Value, json};

    use super::*;

    fn table(v: &Value) -> Table {
        v.as_object().cloned().unwrap_or_default()
    }

    fn set(project: &mut Project, history: &mut History, exposure: f64, key: Option<&str>) {
        let edit = Edit::SetPreferences { group: Group::Develop, table: table(&json!({ "exposure": exposure })) };
        let inverse = project.apply(edit).unwrap();
        if history.coalesces("develop.set", key) {
            history.merge(inverse);
        } else {
            history.record("Set develop settings", "develop.set", inverse, key.map(str::to_owned));
        }
    }

    fn undo(project: &mut Project, history: &mut History) {
        let step = history.take_undo().unwrap();
        let redo = project.apply(step.edit.clone()).unwrap();
        history.undone(step, redo);
    }

    fn redo(project: &mut Project, history: &mut History) {
        let step = history.take_redo().unwrap();
        let undo = project.apply(step.edit.clone()).unwrap();
        history.redone(step, undo);
    }

    #[test]
    fn a_drag_is_one_step_and_saving_tracks_the_state() {
        let mut p = Project::new(None).unwrap();
        let mut h = History::new();
        assert!(!h.is_dirty());
        set(&mut p, &mut h, 0.5, None);
        h.mark_saved();
        let saved = p.clone();
        for i in 0..100 {
            set(&mut p, &mut h, f64::from(i) / 10.0, Some("drag-1"));
        }
        assert_eq!(h.undo.len(), 2, "the drag is one step");
        assert!(matches!(&h.undo[1].edit, Edit::SetPreferences { .. }), "of a run of whole sets, one inverse is kept: {:?}", h.undo[1].edit);
        assert!(h.is_dirty());
        undo(&mut p, &mut h);
        assert_eq!(p, saved, "undoing the drag restores the state before it");
        assert!(!h.is_dirty(), "the saved state again");
        redo(&mut p, &mut h);
        assert!(h.is_dirty());
        // Another key, or anything in between, starts a new step.
        set(&mut p, &mut h, 1.0, Some("drag-2"));
        assert_eq!(h.undo.len(), 3);
        undo(&mut p, &mut h);
        set(&mut p, &mut h, 2.0, Some("drag-2"));
        assert_eq!(h.undo.len(), 3, "after an undo, the same key starts a new step");
        assert!(h.redo.is_empty());
        undo(&mut p, &mut h);
        redo(&mut p, &mut h);
        set(&mut p, &mut h, 3.0, Some("drag-2"));
        assert_eq!(h.undo.len(), 4, "nor does it merge across an undo and a redo");
        assert!(!h.coalesces("output.rename", Some("drag-2")), "only calls of the same command merge");
        assert!(h.coalesces("develop.set", Some("drag-2")));
        h.system_edit();
        h.mark_saved();
        assert!(!h.is_dirty());
    }

    #[test]
    fn the_history_is_bounded() {
        let mut p = Project::new(None).unwrap();
        let mut h = History::new();
        for i in 0..LIMIT + 10 {
            set(&mut p, &mut h, f64::from(u32::try_from(i % 20).unwrap()), None);
        }
        assert_eq!(h.undo.len(), LIMIT);
    }
}
