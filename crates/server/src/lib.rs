//! The collaborative editor server: rooms of clients editing one document
//! each, kept in sync over WebSockets.
//!
//! A client joins a room, gets the document as it stands, then sends ops.
//! The server transforms each op past any ops the client had not seen
//! yet, applies it, and broadcasts the transformed op to everyone else
//! in the room. The version is the number of ops applied so far, so a
//! client's `base_version` says exactly which history it has seen.
//!
//! Rooms live on disk too: every op is appended to a per-room log and a
//! snapshot is written every fifty ops, so a restarted server reloads
//! each room as it was. A client that reconnects can ask for just the
//! ops it missed since its last version instead of the whole document.
//!
//! Presence rides alongside: each client reports where its caret is
//! and what it has selected, the server hands each client a colour,
//! and everyone in the room sees everyone else's cursor move. When an
//! op lands, the other clients' cursors shift past it so they keep
//! pointing at the same text. Presence is never saved to disk.

pub mod message;
pub mod presence;
pub mod room;
pub mod store;

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collab_ot::Op;
use futures_util::{SinkExt, StreamExt};
use message::{ClientMessage, ServerMessage};
use presence::PresenceMap;
use room::Room;
use store::Store;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

/// How many ops a room applies before it writes a fresh snapshot and
/// trims the log back to the ops after it.
const SNAPSHOT_EVERY: usize = 50;

/// What a room sends to its members. `from` is the client that sent the
/// op, so each connection can skip its own echo and wait for the ack
/// instead.
#[derive(Debug, Clone)]
struct Broadcast {
    from: u64,
    message: ServerMessage,
}

struct RoomState {
    room: Room,
    tx: broadcast::Sender<Broadcast>,
    store: Store,
    /// Who is in the room right now, where their carets are, and what
    /// colour each of them gets. Never saved to disk.
    presence: PresenceMap,
    /// Ops applied since the last snapshot; at SNAPSHOT_EVERY the room
    /// snapshots and trims its log.
    ops_since_snapshot: usize,
}

impl RoomState {
    fn new(data_dir: &Path, room_id: &str) -> Self {
        let (tx, _) = broadcast::channel(64);
        let store = Store::new(data_dir, room_id);
        let room = match store.load() {
            Ok(Some(restored)) => Room::from_parts(restored.doc, restored.base, restored.history),
            Ok(None) => Room::new(),
            Err(err) => {
                eprintln!("could not load room {room_id} from disk: {err}; starting empty");
                Room::new()
            }
        };
        // The restored history is exactly the ops since the snapshot.
        let ops_since_snapshot = room.version() - room.earliest_version();
        Self {
            room,
            tx,
            store,
            presence: PresenceMap::new(),
            ops_since_snapshot,
        }
    }

    /// Records an op the room just applied: append it to the log, and
    /// snapshot when enough ops have piled up since the last one.
    fn persist(&mut self, op: &Op, version: usize) {
        if let Err(err) = self.store.append(version, op) {
            eprintln!("could not append op {version} to the log: {err}");
            return;
        }
        self.ops_since_snapshot += 1;
        if self.ops_since_snapshot >= SNAPSHOT_EVERY {
            if let Err(err) = self.store.save_snapshot(version, self.room.text()) {
                eprintln!("could not save snapshot at version {version}: {err}");
                return;
            }
            self.ops_since_snapshot = 0;
        }
    }
}

/// All rooms. One hub is shared by every connection.
pub struct Hub {
    rooms: HashMap<String, RoomState>,
    data_dir: PathBuf,
}

impl Hub {
    pub fn new() -> Self {
        Self::with_data_dir("data")
    }

    pub fn with_data_dir(data_dir: impl AsRef<Path>) -> Self {
        Self {
            rooms: HashMap::new(),
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    fn room(&mut self, id: &str) -> &mut RoomState {
        let data_dir = self.data_dir.clone();
        self.rooms
            .entry(id.to_string())
            .or_insert_with(|| RoomState::new(&data_dir, id))
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// Serves WebSocket connections on `listener` until it errors.
/// Rooms are persisted under `./data`.
pub async fn serve(listener: TcpListener) -> std::io::Result<()> {
    serve_with_data_dir(listener, "data").await
}

/// Like `serve`, but rooms are persisted under `data_dir`.
pub async fn serve_with_data_dir(
    listener: TcpListener,
    data_dir: impl AsRef<Path>,
) -> std::io::Result<()> {
    let hub = Arc::new(Mutex::new(Hub::with_data_dir(data_dir)));
    loop {
        let (stream, addr) = listener.accept().await?;
        let hub = Arc::clone(&hub);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, hub).await {
                eprintln!("connection from {addr} ended: {err}");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    hub: Arc<Mutex<Hub>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let ws = accept_async(stream).await?;
    let (mut write, mut read) = ws.split();

    // The first message must be a join; without a room there is nothing
    // to do.
    let (room_id, client_id, mut rx) = match read.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Join {
                room,
                client_id,
                name,
            }) => {
                let mut hub = hub.lock().await;
                let state = hub.room(&room);
                let joined = ServerMessage::Joined {
                    room: room.clone(),
                    text: state.room.text().to_string(),
                    version: state.room.version(),
                    presences: state.presence.all(),
                };
                let rx = state.tx.subscribe();
                write
                    .send(Message::Text(serde_json::to_string(&joined)?.into()))
                    .await?;
                // Only once the client actually got the Joined is it
                // in the room: a failed send leaves no presence behind.
                let presence = state.presence.join(client_id, name);
                // Everyone already here learns about the new arrival.
                let _ = state.tx.send(Broadcast {
                    from: client_id,
                    message: ServerMessage::Presence { presence },
                });
                (room, client_id, rx)
            }
            _ => return Err("first message was not a join".into()),
        },
        Some(Ok(_)) => return Err("first message was not text".into()),
        Some(Err(err)) => return Err(err.into()),
        None => return Ok(()),
    };

    // From here on, incoming ops and room broadcasts run side by side:
    // an op is applied and fanned out, a broadcast is forwarded unless
    // it came from this client. Any failure of the socket ends the
    // session by breaking out, so the disconnect cleanup below always
    // runs — a dropped connection must drop the client's cursor too.
    loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(msg) = incoming else { break };
                let Ok(msg) = msg else { break };
                if msg.is_close() {
                    break;
                }
                let Message::Text(text) = msg else { continue };
                let reply = handle_message(&text, &room_id, client_id, &hub).await;
                if let Some(reply) = reply {
                    let Ok(text) = serde_json::to_string(&reply) else { break };
                    if write.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            }
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(broadcast) => {
                        if broadcast.from == client_id {
                            continue;
                        }
                        let Ok(text) = serde_json::to_string(&broadcast.message) else { break };
                        if write.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // This client fell too far behind to catch up from
                        // the buffer; it should rejoin and take a snapshot.
                        let reply = ServerMessage::Error {
                            message: "missed messages, please rejoin".into(),
                        };
                        if let Ok(text) = serde_json::to_string(&reply) {
                            let _ = write.send(Message::Text(text.into())).await;
                        }
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    // The client left: drop its cursor from the room and tell the rest.
    {
        let mut hub = hub.lock().await;
        if let Some(state) = hub.rooms.get_mut(&room_id) {
            if state.presence.remove(client_id).is_some() {
                let _ = state.tx.send(Broadcast {
                    from: client_id,
                    message: ServerMessage::PresenceLeft { client_id },
                });
            }
        }
    }

    Ok(())
}

/// Applies one client message. Returns a message to send back to that
/// client: an ack for an accepted op, or an error when the message was
/// not usable. Broadcasts go out through the room channel, not here.
async fn handle_message(
    text: &str,
    room_id: &str,
    client_id: u64,
    hub: &Arc<Mutex<Hub>>,
) -> Option<ServerMessage> {
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(msg) => msg,
        Err(err) => {
            return Some(ServerMessage::Error {
                message: format!("bad message: {err}"),
            });
        }
    };
    match msg {
        ClientMessage::Join { .. } => Some(ServerMessage::Error {
            message: "already joined a room".into(),
        }),
        ClientMessage::Sync { base_version } => {
            let hub = hub.lock().await;
            let Some(state) = hub.rooms.get(room_id) else {
                return Some(ServerMessage::Error {
                    message: "not in a room".into(),
                });
            };
            let room = &state.room;
            if base_version > room.version() {
                return Some(ServerMessage::Error {
                    message: format!(
                        "base version {base_version} is ahead of server version {}",
                        room.version()
                    ),
                });
            }
            match room.ops_since(base_version) {
                Some(ops) => Some(ServerMessage::Ops {
                    from_version: base_version,
                    ops: ops.to_vec(),
                }),
                // Too far behind to replay: send the whole document.
                None => Some(ServerMessage::Snapshot {
                    text: room.text().to_string(),
                    version: room.version(),
                }),
            }
        }
        ClientMessage::Presence { cursor, selection } => {
            let mut hub = hub.lock().await;
            let Some(state) = hub.rooms.get_mut(room_id) else {
                return Some(ServerMessage::Error {
                    message: "not in a room".into(),
                });
            };
            let doc_len = state.room.text().chars().count();
            match state.presence.update(client_id, cursor, selection, doc_len) {
                Some(presence) => {
                    let _ = state.tx.send(Broadcast {
                        from: client_id,
                        message: ServerMessage::Presence { presence },
                    });
                    None
                }
                None => Some(ServerMessage::Error {
                    message: "not in a room".into(),
                }),
            }
        }
        ClientMessage::Op { base_version, op } => {
            if op.site() != client_id {
                return Some(ServerMessage::Error {
                    message: "op site does not match client id".into(),
                });
            }
            let mut hub = hub.lock().await;
            let state = hub.room(room_id);
            match state.room.apply(&op, base_version) {
                Ok(Some((op, version))) => {
                    state.persist(&op, version);
                    let _ = state.tx.send(Broadcast {
                        from: client_id,
                        message: ServerMessage::Op {
                            version,
                            op: op.clone(),
                        },
                    });
                    // The edit moved everyone else's caret past it, so
                    // their cursors keep pointing at the same text.
                    // The op goes out first, then the moved cursors.
                    for presence in state.presence.shift_for_op(&op) {
                        let _ = state.tx.send(Broadcast {
                            from: client_id,
                            message: ServerMessage::Presence { presence },
                        });
                    }
                    Some(ServerMessage::Ack { version })
                }
                Ok(None) => Some(ServerMessage::Ack {
                    version: state.room.version(),
                }),
                Err(message) => Some(ServerMessage::Error { message }),
            }
        }
    }
}
