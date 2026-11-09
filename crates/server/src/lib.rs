//! The collaborative editor server: rooms of clients editing one document
//! each, kept in sync over WebSockets.
//!
//! A client joins a room, gets the document as it stands, then sends ops.
//! The server transforms each op past any ops the client had not seen
//! yet, applies it, and broadcasts the transformed op to everyone else
//! in the room. The version is the number of ops applied so far, so a
//! client's `base_version` says exactly which history it has seen.

pub mod message;
pub mod room;

use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use message::{ClientMessage, ServerMessage};
use room::Room;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

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
}

impl RoomState {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            room: Room::new(),
            tx,
        }
    }
}

/// All rooms. One hub is shared by every connection.
pub struct Hub {
    rooms: HashMap<String, RoomState>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
        }
    }

    fn room(&mut self, id: &str) -> &mut RoomState {
        self.rooms
            .entry(id.to_string())
            .or_insert_with(RoomState::new)
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// Serves WebSocket connections on `listener` until it errors.
pub async fn serve(listener: TcpListener) -> std::io::Result<()> {
    let hub = Arc::new(Mutex::new(Hub::new()));
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
            Ok(ClientMessage::Join { room, client_id }) => {
                let mut hub = hub.lock().await;
                let state = hub.room(&room);
                let joined = ServerMessage::Joined {
                    room: room.clone(),
                    text: state.room.text().to_string(),
                    version: state.room.version(),
                };
                let rx = state.tx.subscribe();
                write
                    .send(Message::Text(serde_json::to_string(&joined)?.into()))
                    .await?;
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
    // it came from this client.
    loop {
        tokio::select! {
            incoming = read.next() => {
                let Some(msg) = incoming else { break };
                let msg = msg?;
                if msg.is_close() {
                    break;
                }
                let Message::Text(text) = msg else { continue };
                let reply = handle_message(&text, &room_id, client_id, &hub).await;
                if let Some(reply) = reply {
                    write
                        .send(Message::Text(serde_json::to_string(&reply)?.into()))
                        .await?;
                }
            }
            broadcast = rx.recv() => {
                match broadcast {
                    Ok(broadcast) => {
                        if broadcast.from == client_id {
                            continue;
                        }
                        write
                            .send(Message::Text(serde_json::to_string(&broadcast.message)?.into()))
                            .await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // This client fell too far behind to catch up from
                        // the buffer; it should rejoin and take a snapshot.
                        let reply = ServerMessage::Error {
                            message: "missed messages, please rejoin".into(),
                        };
                        write
                            .send(Message::Text(serde_json::to_string(&reply)?.into()))
                            .await?;
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
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
                    let _ = state.tx.send(Broadcast {
                        from: client_id,
                        message: ServerMessage::Op { version, op },
                    });
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
