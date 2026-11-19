//! The JSON messages clients and the server send each other over the
//! WebSocket. Every message is one text frame carrying one JSON object.

use crate::docs::DocInfo;
use crate::presence::{Presence, Selection};
use collab_ot::{Attrs, Op};
use serde::{Deserialize, Serialize};

/// A message a client sends to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Join a room. `client_id` is the site id this client's ops carry,
    /// used to break ties when two clients insert at the same spot.
    /// `name` is the display name shown next to the client's cursor;
    /// older clients that do not send one just show up unnamed.
    /// `read_only` joins as a viewer: the client can see the document
    /// and everyone in it, but its edits are refused.
    Join {
        room: String,
        client_id: u64,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        read_only: bool,
    },
    /// Ask for the list of documents on the server. Sent before
    /// joining, so a client can pick a room first; the server answers
    /// with `Docs` and the connection stays open for the join.
    List,
    /// An edit made against the document as it was at `base_version`.
    /// `seq` counts this client's ops from 1 up, so a resend after a
    /// lost ack carries the same number and the server can tell it
    /// apart from a new edit.
    Op {
        base_version: usize,
        seq: u64,
        op: Op,
    },
    /// A reconnecting client asking for the ops it missed since
    /// `base_version`, so it can catch up without taking a full copy.
    Sync { base_version: usize },
    /// Where this client's caret is, and what it has highlighted.
    /// Sent whenever the caret moves or the selection changes; the
    /// server forwards it to everyone else in the room.
    Presence {
        cursor: usize,
        selection: Option<Selection>,
    },
}

/// A message the server sends to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent right after joining: the room's current text and version,
    /// plus who else is already here. `attrs` is the formatting of
    /// each character, in order; without it a joining client would
    /// see the text but lose the bold. `read_only` echoes the join:
    /// a viewer joined read-only, an editor did not.
    Joined {
        room: String,
        text: String,
        #[serde(default)]
        attrs: Vec<Attrs>,
        version: usize,
        presences: Vec<Presence>,
        #[serde(default)]
        read_only: bool,
    },
    /// The documents list, in answer to a `List`: one row per room,
    /// newest first.
    Docs { docs: Vec<DocInfo> },
    /// An op from another client, already transformed and applied.
    Op { version: usize, op: Op },
    /// The server applied this client's op; `version` is the new version.
    /// `seq` echoes the op's number, so the client knows which of its
    /// queued ops this ack is for. `op` is the op as applied (after
    /// transforming it past anything the client had not seen): the
    /// client applies it when its own echo was wiped by a rejoin, and
    /// ignores it when the op is already in its text. `None` means the
    /// op transformed into a no-op and changed nothing.
    Ack {
        version: usize,
        seq: u64,
        op: Option<Op>,
    },
    /// The ops a syncing client missed, starting at `from_version`.
    /// Applying them in order brings it up to date.
    Ops { from_version: usize, ops: Vec<Op> },
    /// The answer to a sync when the client's version is older than the
    /// ops the server still keeps: take the whole document instead.
    /// `attrs` carries the formatting, for the same reason as in Joined.
    Snapshot {
        text: String,
        #[serde(default)]
        attrs: Vec<Attrs>,
        version: usize,
    },
    /// One client's presence changed: a join, a moved caret, or a new
    /// selection. The sender never gets its own presence back.
    Presence { presence: Presence },
    /// A client left the room; drop its cursor from the screen.
    PresenceLeft { client_id: u64 },
    /// Something about the client's last message was wrong; the
    /// connection stays open.
    Error { message: String },
}
