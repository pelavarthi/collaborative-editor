// Undo and redo for one editor, built to compose with OT.
//
// The hard part of undo in a collaborative editor is that the document
// keeps moving after you type: by the time you hit undo, other people's
// edits have landed on top of yours. Undoing the raw inverse of your op
// would undo the wrong text. So an undo is not the inverse itself — it
// is the inverse transformed past every op applied since the original
// edit, which is exactly what `transform` already does for concurrent
// edits.
//
// Redo has one more twist: it is the inverse of the undo op, not the
// original op transformed past everything since. The undo re-created
// the very text the original op removed, and transforming the original
// op past its own undo would treat that re-created text as someone
// else's text to preserve — a redo of a delete would slide past the
// text it should delete and remove nothing. Taking the inverse of the
// undo op instead keeps redo pointed at the right text.
//
// A History keeps two stacks and the list of ops applied to the
// document, in order. Each entry remembers where its op sits in that
// list, so undo and redo only ever transform past the ops that came
// after it. The undo op is just an op, so it needs no server change:
// the client sends it like any other edit, and it is logged,
// broadcast, and persisted as usual.

use crate::document::Document;
use crate::op::Op;
use crate::transform::transform;

/// How many edits back undo can reach. Older edits fall off the stack,
// and the op list is trimmed with them so memory stays bounded.
const MAX_UNDO: usize = 100;

#[derive(Debug, Clone)]
struct Entry {
    /// The op as last applied: the original edit, or its redone form.
    /// For a redo entry, the op that re-applies the edit.
    op: Op,
    /// The inverse of `op`. For a redo entry, the undo op it undoes.
    inverse: Op,
    /// Position of `op` in `History::applied`.
    at: usize,
}

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    /// Every op applied to the document, local or remote, in order.
    applied: Vec<Op>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a local edit, with the document as it was just before
    /// the edit, so the inverse of a delete can carry the deleted text.
    /// A new edit clears the redo stack, and no-ops are not recorded.
    pub fn record(&mut self, op: &Op, before: &Document) {
        if op.is_noop() {
            return;
        }
        let Some(inverse) = inverse(op, before) else {
            return;
        };
        self.applied.push(op.clone());
        self.undo.push(Entry {
            op: op.clone(),
            inverse,
            at: self.applied.len() - 1,
        });
        self.redo.clear();
        self.trim();
    }

    /// Notes an op from someone else (or the server) that was just
    /// applied to the document, so later undos transform past it.
    pub fn note_remote(&mut self, op: &Op) {
        if !op.is_noop() {
            self.applied.push(op.clone());
        }
    }

    /// The op that undoes the most recent local edit, transformed past
    /// everything applied since. `doc` is the document before the undo
    /// is applied: the redo op is the inverse of the undo op, and the
    /// inverse of a delete has to read the text it is about to remove.
    /// Applying the returned op is the undo; the entry moves to the
    /// redo stack so it can be redone.
    pub fn undo(&mut self, doc: &Document) -> Option<Op> {
        let entry = self.undo.pop()?;
        let undone = self.transform_past(&entry.inverse, entry.at);
        let Some(redo_op) = inverse(&undone, doc) else {
            self.undo.push(entry);
            return None;
        };
        let at = self.applied.len();
        self.redo.push(Entry {
            op: redo_op,
            inverse: undone.clone(),
            at,
        });
        self.applied.push(undone.clone());
        Some(undone)
    }

    /// Re-applies the most recently undone edit, transformed past
    /// everything applied since the undo. `doc` is the document before
    /// the redo is applied, for the same reason as in `undo`.
    pub fn redo(&mut self, doc: &Document) -> Option<Op> {
        let entry = self.redo.pop()?;
        let redone = self.transform_past(&entry.op, entry.at);
        let Some(inverse) = inverse(&redone, doc) else {
            self.redo.push(entry);
            return None;
        };
        let at = self.applied.len();
        self.undo.push(Entry {
            op: redone.clone(),
            inverse,
            at,
        });
        self.applied.push(redone.clone());
        Some(redone)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.applied.clear();
    }

    /// Transforms `op` past every op applied after position `at`.
    fn transform_past(&self, op: &Op, at: usize) -> Op {
        self.applied[at + 1..]
            .iter()
            .fold(op.clone(), |op, against| transform(&op, against))
    }

    /// Drops the oldest entries past the cap, and the applied ops
    /// before the oldest entry still kept, shifting positions with them.
    fn trim(&mut self) {
        if self.undo.len() <= MAX_UNDO {
            return;
        }
        self.undo.remove(0);
        let cut = self
            .undo
            .first()
            .map(|e| e.at)
            .unwrap_or(self.applied.len());
        self.applied.drain(..cut);
        for entry in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            entry.at -= cut;
        }
    }
}

/// The op that exactly reverses `op`: an insert becomes a delete of the
/// same text, a delete becomes an insert of the text it removed, which
/// has to be read out of the document before the delete runs.
fn inverse(op: &Op, before: &Document) -> Option<Op> {
    match op {
        Op::Insert { site, pos, text } => Some(Op::delete(*site, *pos, text.chars().count())),
        Op::Delete { site, pos, len } => {
            let text: String = before.text().chars().skip(*pos).take(*len).collect();
            if text.chars().count() != *len {
                return None;
            }
            Some(Op::insert(*site, *pos, text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_of_insert_is_a_delete_of_the_same_text() {
        let before = Document::from_text("ab");
        let op = Op::insert(1, 1, "XY");
        assert_eq!(inverse(&op, &before), Some(Op::delete(1, 1, 2)));
    }

    #[test]
    fn inverse_of_delete_carries_the_deleted_text() {
        let before = Document::from_text("hello");
        let op = Op::delete(1, 1, 3);
        assert_eq!(inverse(&op, &before), Some(Op::insert(1, 1, "ell")));
    }

    #[test]
    fn inverse_of_an_out_of_bounds_delete_is_none() {
        let before = Document::from_text("hi");
        let op = Op::delete(1, 1, 5);
        assert_eq!(inverse(&op, &before), None);
    }

    #[test]
    fn recording_a_noop_does_not_enable_undo() {
        let mut history = History::new();
        let doc = Document::from_text("abc");
        history.record(&Op::insert(1, 0, ""), &doc);
        assert!(!history.can_undo());
    }
}
