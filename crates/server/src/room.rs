// One room is one shared document plus the history of ops that built it.
// The version is just the number of ops applied so far, so a client's
// `base_version` says exactly which history it has seen.
//
// A client that is behind does not need to catch up itself: the server
// transforms its op past every op it missed, applies the result, and
// broadcasts the transformed op, so every client ends up applying the
// same ops in the same order.

use collab_ot::{transform, Attrs, Document, Op};

#[derive(Debug, Default)]
pub struct Room {
    doc: Document,
    /// The ops applied since the last snapshot. `history[i]` is the op
    /// that moved the document from version `base + i` to `base + i + 1`,
    /// so after a restore from a snapshot at version N the history only
    /// holds ops after N.
    history: Vec<Op>,
    base: usize,
}

impl Room {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a room from a snapshot plus the ops applied since it:
    /// `doc` is the text at `base + history.len()`.
    pub fn from_parts(doc: Document, base: usize, history: Vec<Op>) -> Self {
        Self { doc, history, base }
    }

    pub fn text(&self) -> &str {
        self.doc.text()
    }

    /// Length of the document in characters. O(1).
    pub fn len(&self) -> usize {
        self.doc.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The formatting of each character, in order. A client that joins
    /// needs this with the text, or its copy would lose the bold.
    pub fn attrs(&self) -> &[Attrs] {
        self.doc.attrs()
    }

    pub fn version(&self) -> usize {
        self.base + self.history.len()
    }

    /// The earliest version a client can still catch up from: ops
    /// before this one were folded into the snapshot and are gone.
    pub fn earliest_version(&self) -> usize {
        self.base
    }

    /// The ops a client at `version` has not seen yet, or `None` when
    /// that version is older than the snapshot (it needs the full text)
    /// or ahead of the server (it claims to have seen ops that do not
    /// exist yet).
    pub fn ops_since(&self, version: usize) -> Option<&[Op]> {
        if version < self.base || version > self.version() {
            return None;
        }
        Some(&self.history[version - self.base..])
    }

    /// Applies an op a client made against `base_version` of the history.
    /// Returns the op as applied (after transforming it past the ops the
    /// client had not seen) and the new version, or `None` when the op
    /// transformed into a no-op and changed nothing. `base_version` is a
    /// version in the room's numbering, not an index into the history
    /// kept since the last snapshot: after a restore the history starts
    /// at `base`, so the ops to transform past begin at
    /// `base_version - base`.
    pub fn apply(&mut self, op: &Op, base_version: usize) -> Result<Option<(Op, usize)>, String> {
        if base_version < self.base {
            return Err(format!(
                "base version {base_version} is older than the snapshot at version {}",
                self.base
            ));
        }
        if base_version > self.version() {
            return Err(format!(
                "base version {base_version} is ahead of server version {}",
                self.version()
            ));
        }
        let mut transformed = op.clone();
        for seen in &self.history[base_version - self.base..] {
            transformed = transform(&transformed, seen);
        }
        if transformed.is_noop() {
            return Ok(None);
        }
        self.doc
            .apply(&transformed)
            .map_err(|err| err.to_string())?;
        self.history.push(transformed.clone());
        Ok(Some((transformed, self.version())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_at_current_version_bumps_version() {
        let mut room = Room::new();
        let (op, version) = room.apply(&Op::insert(1, 0, "hi"), 0).unwrap().unwrap();
        assert_eq!(op, Op::insert(1, 0, "hi"));
        assert_eq!(version, 1);
        assert_eq!(room.text(), "hi");
        assert_eq!(room.version(), 1);
    }

    #[test]
    fn apply_with_stale_base_transforms_past_unseen_ops() {
        let mut room = Room::new();
        room.apply(&Op::insert(1, 0, "abc"), 0).unwrap();
        room.apply(&Op::insert(1, 0, "X"), 1).unwrap();
        assert_eq!(room.text(), "Xabc");
        // Client 2 typed "!" at the end of "abc" without seeing the "X"
        // client 1 added at the start.
        let (op, version) = room.apply(&Op::insert(2, 3, "!"), 1).unwrap().unwrap();
        assert_eq!(op, Op::insert(2, 4, "!"));
        assert_eq!(version, 3);
        assert_eq!(room.text(), "Xabc!");
    }

    #[test]
    fn op_that_transforms_to_noop_is_not_recorded() {
        let mut room = Room::new();
        room.apply(&Op::insert(1, 0, "abcdef"), 0).unwrap();
        room.apply(&Op::delete(1, 1, 3), 1).unwrap();
        assert_eq!(room.text(), "aef");
        // Client 2 inserted inside the range client 1 deleted, without
        // seeing the delete. The insert has nowhere to go.
        let applied = room.apply(&Op::insert(2, 2, "X"), 1).unwrap();
        assert_eq!(applied, None);
        assert_eq!(room.version(), 2);
        assert_eq!(room.text(), "aef");
    }

    #[test]
    fn base_version_ahead_of_server_is_an_error() {
        let mut room = Room::new();
        let err = room.apply(&Op::insert(1, 0, "x"), 1).unwrap_err();
        assert!(err.contains("ahead of server version 0"));
    }

    #[test]
    fn ops_since_returns_only_the_missed_ops() {
        let mut room = Room::new();
        room.apply(&Op::insert(1, 0, "a"), 0).unwrap();
        room.apply(&Op::insert(1, 1, "b"), 1).unwrap();
        room.apply(&Op::insert(1, 2, "c"), 2).unwrap();
        assert_eq!(
            room.ops_since(1),
            Some([Op::insert(1, 1, "b"), Op::insert(1, 2, "c")].as_slice())
        );
        assert_eq!(room.ops_since(3), Some([].as_slice()));
        assert_eq!(room.ops_since(4), None);
    }

    #[test]
    fn restored_room_replays_only_ops_after_its_snapshot() {
        let doc = Document::from_text("ab");
        let history = vec![Op::insert(1, 2, "c")];
        let room = Room::from_parts(doc, 2, history);
        assert_eq!(room.text(), "ab");
        assert_eq!(room.version(), 3);
        assert_eq!(room.earliest_version(), 2);
        assert_eq!(room.ops_since(1), None);
        assert_eq!(room.ops_since(2), Some([Op::insert(1, 2, "c")].as_slice()));
    }

    #[test]
    fn apply_on_restored_room_counts_from_the_snapshot_base() {
        let doc = Document::from_text("ab");
        let history = vec![Op::insert(1, 2, "c")];
        let mut room = Room::from_parts(doc, 2, history);
        // An op made against the snapshot (version 2) transforms past
        // the one op since it and lands at version 4, not 2.
        let (op, version) = room.apply(&Op::insert(2, 0, ">> "), 2).unwrap().unwrap();
        assert_eq!(op, Op::insert(2, 0, ">> "));
        assert_eq!(version, 4);
        assert_eq!(room.text(), ">> ab");
        // An op made against the latest version applies as-is.
        let (op, version) = room.apply(&Op::insert(2, 5, "!"), 4).unwrap().unwrap();
        assert_eq!(op, Op::insert(2, 5, "!"));
        assert_eq!(version, 5);
        assert_eq!(room.text(), ">> ab!");
    }

    #[test]
    fn apply_with_base_before_the_snapshot_is_an_error() {
        let doc = Document::from_text("ab");
        let history = vec![Op::insert(1, 2, "c")];
        let mut room = Room::from_parts(doc, 2, history);
        let err = room.apply(&Op::insert(2, 0, "x"), 1).unwrap_err();
        assert!(err.contains("older than the snapshot at version 2"));
        let err = room.apply(&Op::insert(2, 0, "x"), 4).unwrap_err();
        assert!(err.contains("ahead of server version 3"));
    }
}
