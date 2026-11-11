//! The JSON messages clients and the server send each other over the
//! WebSocket. Every message is one text frame carrying one JSON object.

use crate::presence::{Presence, Selection};
use collab_ot::Op;
use serde::{Deserialize, Serialize};

/// A message a client sends to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Join a room. `client_id` is the site id this client's ops carry,
    /// used to break ties when two clients insert at the same spot.
    /// `name` is the display name shown next to the client's cursor;
    /// older clients that do not send one just show up unnamed.
    Join {
        room: String,
        client_id: u64,
        #[serde(default)]
        name: Option<String>,
    },
    /// An edit made against the document as it was at `base_version`.
    Op { base_version: usize, op: Op },
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
    /// plus who else is already here.
    Joined {
        room: String,
        text: String,
        version: usize,
        presences: Vec<Presence>,
    },
    /// An op from another client, already transformed and applied.
    Op { version: usize, op: Op },
    /// The server applied this client's op; `version` is the new version.
    Ack { version: usize },
    /// The ops a syncing client missed, starting at `from_version`.
    /// Applying them in order brings it up to date.
    Ops { from_version: usize, ops: Vec<Op> },
    /// The answer to a sync when the client's version is older than the
    /// ops the server still keeps: take the whole document instead.
    Snapshot { text: String, version: usize },
    /// One client's presence changed: a join, a moved caret, or a new
    /// selection. The sender never gets its own presence back.
    Presence { presence: Presence },
    /// A client left the room; drop its cursor from the screen.
    PresenceLeft { client_id: u64 },
    /// Something about the client's last message was wrong; the
    /// connection stays open.
    Error { message: String },
}
