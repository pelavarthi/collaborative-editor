// One room is one shared document plus the history of ops that built it.
// The version is just the number of ops applied so far, so a client's
// `base_version` says exactly which history it has seen.
//
// A client that is behind does not need to catch up itself: the server
// transforms its op past every op it missed, applies the result, and
// broadcasts the transformed op, so every client ends up applying the
// same ops in the same order.

use collab_ot::{transform, Document, Op};

#[derive(Debug, Default)]
pub struct Room {
    doc: Document,
    history: Vec<Op>,
}

impl Room {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        self.doc.text()
    }

    pub fn version(&self) -> usize {
        self.history.len()
    }

    /// Applies an op a client made against `base_version` of the history.
    /// Returns the op as applied (after transforming it past the ops the
    /// client had not seen) and the new version, or `None` when the op
    /// transformed into a no-op and changed nothing.
    pub fn apply(&mut self, op: &Op, base_version: usize) -> Result<Option<(Op, usize)>, String> {
        if base_version > self.history.len() {
            return Err(format!(
                "base version {base_version} is ahead of server version {}",
                self.history.len()
            ));
        }
        let mut transformed = op.clone();
        for seen in &self.history[base_version..] {
            transformed = transform(&transformed, seen);
        }
        if transformed.is_noop() {
            return Ok(None);
        }
        self.doc
            .apply(&transformed)
            .map_err(|err| err.to_string())?;
        self.history.push(transformed.clone());
        Ok(Some((transformed, self.history.len())))
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
}
