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
//!
//! Before joining, a client can ask for the list of documents on the
//! server, newest first. And a client can join read-only, as a viewer:
//! it sees the document and everyone in it, but the server refuses
//! its edits, so a read-only share link cannot write no matter what
//! client opens it.

pub mod docs;
pub mod message;
pub mod presence;
pub mod room;
pub mod store;

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collab_ot::Op;
use docs::DocInfo;
use futures_util::{SinkExt, StreamExt};
use message::{ClientMessage, ServerMessage};
use presence::PresenceMap;
use room::Room;
use store::{safe_name, Store};
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
    /// The ops already applied, by (client, seq): a client that resends
    /// an op because its ack was lost gets the stored answer back
    /// instead of the op being applied twice. Kept in memory only.
    acked: HashMap<(u64, u64), (usize, Op)>,
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
            acked: HashMap::new(),
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
            if let Err(err) = self
                .store
                .save_snapshot(version, self.room.text(), self.room.attrs())
            {
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

    /// Every document the server knows about, newest first: the rooms
    /// on disk, overlaid with the rooms in memory, which are fresher.
    /// A room that was joined but never edited has no files yet, so it
    /// shows up with no save time and sorts last.
    pub fn list_docs(&self) -> Vec<DocInfo> {
        let mut docs = docs::scan(&self.data_dir);
        for (id, state) in &self.rooms {
            let info = docs::from_room(
                id,
                state.room.text(),
                state.room.version(),
                state.store.updated_at(),
            );
            match docs.iter_mut().find(|doc| doc.id == safe_name(id)) {
                Some(existing) => *existing = info,
                None => docs.push(info),
            }
        }
        docs::sort_docs(&mut docs);
        docs
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

/// The half of a WebSocket a connection writes to.
type WsWrite = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    Message,
>;

/// Forwards one broadcast to this client, unless it came from the
/// client itself. Returns false when the connection should end.
async fn forward_broadcast(write: &mut WsWrite, broadcast: Broadcast, client_id: u64) -> bool {
    if broadcast.from == client_id {
        return true;
    }
    let Ok(text) = serde_json::to_string(&broadcast.message) else {
        return false;
    };
    write.send(Message::Text(text.into())).await.is_ok()
}

/// Tells a client it fell too far behind the broadcast buffer to catch
/// up: it should rejoin and take a snapshot.
async fn send_missed_messages(write: &mut WsWrite) {
    let reply = ServerMessage::Error {
        message: "missed messages, please rejoin".into(),
    };
    if let Ok(text) = serde_json::to_string(&reply) {
        let _ = write.send(Message::Text(text.into())).await;
    }
}

async fn handle_connection(
    stream: TcpStream,
    hub: Arc<Mutex<Hub>>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let ws = accept_async(stream).await?;
    let (mut write, mut read) = ws.split();

    // The first message is a join, or a list request first and a join
    // after it: a client that has not picked a room yet asks what
    // documents exist, then joins one on the same connection.
    let (room_id, client_id, read_only, mut rx) = loop {
        let msg = match read.next().await {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(&text) {
                Ok(msg) => msg,
                Err(err) => return Err(format!("bad message: {err}").into()),
            },
            Some(Ok(_)) => return Err("first message was not text".into()),
            Some(Err(err)) => return Err(err.into()),
            None => return Ok(()),
        };
        match msg {
            ClientMessage::List => {
                let docs = hub.lock().await.list_docs();
                let reply = ServerMessage::Docs { docs };
                write
                    .send(Message::Text(serde_json::to_string(&reply)?.into()))
                    .await?;
            }
            ClientMessage::Join {
                room,
                client_id,
                name,
                read_only,
            } => {
                let mut hub = hub.lock().await;
                let state = hub.room(&room);
                let joined = ServerMessage::Joined {
                    room: room.clone(),
                    text: state.room.text().to_string(),
                    attrs: state.room.attrs().to_vec(),
                    version: state.room.version(),
                    presences: state.presence.all(),
                    read_only,
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
                break (room, client_id, read_only, rx);
            }
            _ => return Err("first message was not a join or a list".into()),
        }
    };

    // From here on, incoming ops and room broadcasts run side by side:
    // an op is applied and fanned out, a broadcast is forwarded unless
    // it came from this client. Any failure of the socket ends the
    // session by breaking out, so the disconnect cleanup below always
    // runs — a dropped connection must drop the client's cursor too.
    //
    // One ordering rule matters here: a reply must never jump ahead of
    // broadcasts the server already sent this client. An op applied
    // before this one could still be queued in the broadcast channel
    // when this client's op is answered, and a client that applies
    // messages in arrival order would then apply them out of order and
    // diverge. So before any reply goes out, the queued broadcasts are
    // forwarded first, in order. Anything sent after that drain is for
    // an op applied after this one, so it cannot overtake the reply.
    'session: loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(msg) = incoming else { break };
                let Ok(msg) = msg else { break };
                if msg.is_close() {
                    break;
                }
                let Message::Text(text) = msg else { continue };
                let reply = handle_message(&text, &room_id, client_id, read_only, &hub).await;
                if let Some(reply) = reply {
                    loop {
                        match rx.try_recv() {
                            Ok(broadcast) => {
                                if !forward_broadcast(&mut write, broadcast, client_id).await {
                                    break 'session;
                                }
                            }
                            Err(broadcast::error::TryRecvError::Empty) => break,
                            Err(broadcast::error::TryRecvError::Closed) => break 'session,
                            Err(broadcast::error::TryRecvError::Lagged(_)) => {
                                send_missed_messages(&mut write).await;
                                break 'session;
                            }
                        }
                    }
                    let Ok(text) = serde_json::to_string(&reply) else { break 'session };
                    if write.send(Message::Text(text.into())).await.is_err() {
                        break 'session;
                    }
                }
            }
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(broadcast) => {
                        if !forward_broadcast(&mut write, broadcast, client_id).await {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        send_missed_messages(&mut write).await;
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
    read_only: bool,
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
        ClientMessage::List => {
            let hub = hub.lock().await;
            Some(ServerMessage::Docs {
                docs: hub.list_docs(),
            })
        }
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
                    attrs: room.attrs().to_vec(),
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
            let doc_len = state.room.len();
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
        ClientMessage::Op {
            base_version,
            seq,
            op,
        } => {
            // A viewer can watch, but its edits are refused before
            // anything is applied or broadcast: read-only is enforced
            // here, not just in the UI, so a hand-made client cannot
            // write through a read-only link.
            if read_only {
                return Some(ServerMessage::Error {
                    message: "this document is read-only for you".into(),
                });
            }
            if op.site() != client_id {
                return Some(ServerMessage::Error {
                    message: "op site does not match client id".into(),
                });
            }
            let mut hub = hub.lock().await;
            let state = hub.room(room_id);
            // A resend of an op whose ack was lost: answer from memory
            // instead of applying it a second time.
            if let Some((version, applied)) = state.acked.get(&(client_id, seq)) {
                return Some(ServerMessage::Ack {
                    version: *version,
                    seq,
                    op: Some(applied.clone()),
                });
            }
            match state.room.apply(&op, base_version) {
                Ok(Some((op, version))) => {
                    state.persist(&op, version);
                    state.acked.insert((client_id, seq), (version, op.clone()));
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
                    Some(ServerMessage::Ack {
                        version,
                        seq,
                        op: Some(op),
                    })
                }
                Ok(None) => Some(ServerMessage::Ack {
                    version: state.room.version(),
                    seq,
                    op: None,
                }),
                Err(message) => Some(ServerMessage::Error { message }),
            }
        }
    }
}
