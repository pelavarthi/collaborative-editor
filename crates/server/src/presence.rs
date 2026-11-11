//! Live presence: where each client in a room is looking, and what
//! colour their cursor is.
//!
//! Presence is deliberately not part of the document. It is never
//! stored on disk, never written to the op log, and a restart clears
//! it: it only says who is here right now. When a client disconnects
//! its presence leaves with it.
//!
//! Positions count characters, like ops do, so a cursor never lands
//! in the middle of a multi-byte character. When someone else's op
//! lands, every other client's cursor and selection shift past it, so
//! a cursor keeps pointing at the same text it pointed at before the
//! edit.

use collab_ot::Op;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One colour per client, picked from the palette by client id, so the
/// same client always gets the same colour in every room.
const PALETTE: [&str; 8] = [
    "#e5484d", // red
    "#f76b15", // orange
    "#ffb224", // yellow
    "#46a758", // green
    "#0090ff", // blue
    "#8e4ec6", // purple
    "#d6409f", // pink
    "#12a594", // teal
];

pub fn color_for(client_id: u64) -> &'static str {
    PALETTE[(client_id as usize) % PALETTE.len()]
}

/// A highlighted range. `anchor` is where the selection started,
/// `head` is where the caret end is now; dragging backwards gives an
/// anchor after the head. A selection with anchor equal to head is
/// just a cursor with nothing highlighted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head }
    }

    /// The left edge of the highlight, whichever end that is.
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The right edge of the highlight, whichever end that is.
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn is_cursor(&self) -> bool {
        self.anchor == self.head
    }
}

/// One client's presence in a room: who they are, what colour they
/// get, where their caret is, and what they have highlighted, if
/// anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub client_id: u64,
    pub name: Option<String>,
    pub color: String,
    pub cursor: usize,
    pub selection: Option<Selection>,
}

impl Presence {
    fn new(client_id: u64, name: Option<String>) -> Self {
        Self {
            client_id,
            name,
            color: color_for(client_id).to_string(),
            cursor: 0,
            selection: None,
        }
    }
}

/// Everyone currently in one room, keyed by client id.
#[derive(Debug, Default)]
pub struct PresenceMap {
    presences: HashMap<u64, Presence>,
}

impl PresenceMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a client that just joined, at the start of the
    /// document with nothing selected.
    pub fn join(&mut self, client_id: u64, name: Option<String>) -> Presence {
        let presence = Presence::new(client_id, name);
        self.presences.insert(client_id, presence.clone());
        presence
    }

    /// Records a client's new cursor and selection, clamped to the
    /// end of the document so a stale position past the end cannot
    /// point at text that is not there. Returns `None` when the
    /// client is not in the room.
    pub fn update(
        &mut self,
        client_id: u64,
        cursor: usize,
        selection: Option<Selection>,
        doc_len: usize,
    ) -> Option<Presence> {
        let presence = self.presences.get_mut(&client_id)?;
        presence.cursor = cursor.min(doc_len);
        presence.selection =
            selection.map(|s| Selection::new(s.anchor.min(doc_len), s.head.min(doc_len)));
        Some(presence.clone())
    }

    /// Removes a client that left, returning its presence if it had one.
    pub fn remove(&mut self, client_id: u64) -> Option<Presence> {
        self.presences.remove(&client_id)
    }

    /// Everyone in the room, in client id order so the list is stable.
    pub fn all(&self) -> Vec<Presence> {
        let mut all: Vec<Presence> = self.presences.values().cloned().collect();
        all.sort_by_key(|p| p.client_id);
        all
    }

    /// Moves every client's cursor and selection past `op`, except the
    /// client that made the op: its own client already moves its caret
    /// as it types. Returns the presences that actually moved, so the
    /// server can tell the room about them.
    pub fn shift_for_op(&mut self, op: &Op) -> Vec<Presence> {
        let mut moved = Vec::new();
        for presence in self.presences.values_mut() {
            if presence.client_id == op.site() {
                continue;
            }
            let cursor = shift_pos(presence.cursor, op);
            let selection = presence
                .selection
                .map(|s| Selection::new(shift_pos(s.anchor, op), shift_pos(s.head, op)));
            if cursor != presence.cursor || selection != presence.selection {
                presence.cursor = cursor;
                presence.selection = selection;
                moved.push(presence.clone());
            }
        }
        moved.sort_by_key(|p| p.client_id);
        moved
    }
}

/// Where a position ends up after `op` is applied to the document.
/// An insert at or before the position pushes it right by the
/// inserted length; a delete before it pulls it left, and a position
/// inside the deleted range lands at the start of that range.
fn shift_pos(pos: usize, op: &Op) -> usize {
    match op {
        Op::Insert { pos: at, text, .. } => {
            if *at <= pos {
                pos + text.chars().count()
            } else {
                pos
            }
        }
        Op::Delete { pos: at, len, .. } => {
            if pos <= *at {
                pos
            } else if pos <= at + len {
                *at
            } else {
                pos - len
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_stable_and_wrap_around_the_palette() {
        assert_eq!(color_for(1), "#f76b15");
        assert_eq!(color_for(1), color_for(1));
        assert_ne!(color_for(1), color_for(2));
        assert_eq!(color_for(9), color_for(1));
    }

    #[test]
    fn join_starts_at_zero_with_a_color() {
        let mut map = PresenceMap::new();
        let presence = map.join(3, Some("pranav".to_string()));
        assert_eq!(presence.client_id, 3);
        assert_eq!(presence.name.as_deref(), Some("pranav"));
        assert_eq!(presence.color, color_for(3));
        assert_eq!(presence.cursor, 0);
        assert_eq!(presence.selection, None);
    }

    #[test]
    fn update_clamps_positions_to_the_end_of_the_document() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        let presence = map.update(1, 99, Some(Selection::new(2, 99)), 5).unwrap();
        assert_eq!(presence.cursor, 5);
        assert_eq!(presence.selection, Some(Selection::new(2, 5)));
    }

    #[test]
    fn update_of_an_unknown_client_is_nothing() {
        let mut map = PresenceMap::new();
        assert_eq!(map.update(7, 0, None, 0), None);
    }

    #[test]
    fn all_is_sorted_by_client_id() {
        let mut map = PresenceMap::new();
        map.join(3, None);
        map.join(1, None);
        map.join(2, None);
        let ids: Vec<u64> = map.all().iter().map(|p| p.client_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn remove_drops_the_client() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        map.join(2, None);
        assert!(map.remove(1).is_some());
        assert!(map.remove(1).is_none());
        assert_eq!(map.all().len(), 1);
    }

    #[test]
    fn insert_before_the_cursor_pushes_it_right() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        map.join(2, None);
        map.update(2, 5, Some(Selection::new(3, 5)), 10);
        let moved = map.shift_for_op(&Op::insert(1, 0, ">>"));
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].cursor, 7);
        assert_eq!(moved[0].selection, Some(Selection::new(5, 7)));
    }

    #[test]
    fn insert_after_the_cursor_leaves_it_alone() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        map.join(2, None);
        map.update(2, 2, None, 10);
        assert!(map.shift_for_op(&Op::insert(1, 5, "xx")).is_empty());
        assert_eq!(map.all()[1].cursor, 2);
    }

    #[test]
    fn delete_before_the_cursor_pulls_it_left() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        map.join(2, None);
        map.update(2, 8, None, 10);
        let moved = map.shift_for_op(&Op::delete(1, 0, 3));
        assert_eq!(moved[0].cursor, 5);
    }

    #[test]
    fn delete_covering_the_cursor_clamps_it_to_the_delete_start() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        map.join(2, None);
        map.update(2, 4, Some(Selection::new(2, 6)), 10);
        let moved = map.shift_for_op(&Op::delete(1, 3, 4));
        assert_eq!(moved[0].cursor, 3);
        assert_eq!(moved[0].selection, Some(Selection::new(2, 3)));
    }

    #[test]
    fn the_op_authors_own_cursor_does_not_move() {
        let mut map = PresenceMap::new();
        map.join(1, None);
        map.update(1, 2, None, 10);
        assert!(map.shift_for_op(&Op::insert(1, 0, "xx")).is_empty());
        assert_eq!(map.all()[0].cursor, 2);
    }

    #[test]
    fn selection_edges_and_cursor_check() {
        let forward = Selection::new(2, 5);
        assert_eq!((forward.start(), forward.end()), (2, 5));
        assert!(!forward.is_cursor());
        let backward = Selection::new(5, 2);
        assert_eq!((backward.start(), backward.end()), (2, 5));
        assert!(Selection::new(4, 4).is_cursor());
    }
}
