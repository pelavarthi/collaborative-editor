//! The JSON messages clients and the server send each other over the
//! WebSocket. Every message is one text frame carrying one JSON object.

use collab_ot::Op;
use serde::{Deserialize, Serialize};

/// A message a client sends to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Join a room. `client_id` is the site id this client's ops carry,
    /// used to break ties when two clients insert at the same spot.
    Join { room: String, client_id: u64 },
    /// An edit made against the document as it was at `base_version`.
    Op { base_version: usize, op: Op },
    /// A reconnecting client asking for the ops it missed since
    /// `base_version`, so it can catch up without taking a full copy.
    Sync { base_version: usize },
}

/// A message the server sends to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Sent right after joining: the room's current text and version.
    Joined {
        room: String,
        text: String,
        version: usize,
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
    /// Something about the client's last message was wrong; the
    /// connection stays open.
    Error { message: String },
}
