//! Two real WebSocket clients talking to a real server on a local port.

use collab_ot::Op;
use collab_server::message::{ClientMessage, ServerMessage};
use collab_server::serve_with_data_dir;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_data_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("collab-server-test-{}-{n}", std::process::id()))
}

struct Client {
    write: futures_util::stream::SplitSink<WsStream, Message>,
    read: futures_util::stream::SplitStream<WsStream>,
}

impl Client {
    /// Connects and joins a room, returning the client and the Joined
    /// message the server answered with.
    async fn connect(addr: SocketAddr, room: &str, client_id: u64) -> (Self, ServerMessage) {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let (mut write, read) = ws.split();
        let join = ClientMessage::Join {
            room: room.to_string(),
            client_id,
        };
        write
            .send(Message::Text(serde_json::to_string(&join).unwrap().into()))
            .await
            .unwrap();
        let mut client = Self { write, read };
        let joined = client.next_server_message().await;
        assert!(matches!(joined, ServerMessage::Joined { .. }));
        (client, joined)
    }

    async fn send(&mut self, msg: &ClientMessage) {
        self.write
            .send(Message::Text(serde_json::to_string(msg).unwrap().into()))
            .await
            .unwrap();
    }

    async fn send_op(&mut self, base_version: usize, op: Op) {
        self.send(&ClientMessage::Op { base_version, op }).await;
    }

    async fn send_sync(&mut self, base_version: usize) {
        self.send(&ClientMessage::Sync { base_version }).await;
    }

    async fn next_server_message(&mut self) -> ServerMessage {
        let msg = self.read.next().await.unwrap().unwrap();
        let Message::Text(text) = msg else {
            panic!("expected a text message, got {msg:?}");
        };
        serde_json::from_str(&text).unwrap()
    }
}

/// Starts a server on a free port, returning its address, a handle to
/// stop it, and the data directory its rooms are saved under.
async fn start_server() -> (SocketAddr, JoinHandle<std::io::Result<()>>, PathBuf) {
    let dir = temp_data_dir();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve_dir = dir.clone();
    let handle = tokio::spawn(async move { serve_with_data_dir(listener, serve_dir).await });
    (addr, handle, dir)
}

#[tokio::test]
async fn op_from_one_client_reaches_the_other() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut b, _) = Client::connect(addr, "room1", 2).await;

    a.send_op(0, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack { version: 1 }
    );
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Op {
            version: 1,
            op: Op::insert(1, 0, "hello"),
        }
    );
}

#[tokio::test]
async fn rooms_are_isolated() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut c, _) = Client::connect(addr, "room2", 3).await;

    a.send_op(0, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack { version: 1 }
    );
    // Nothing from room1 may leak into room2.
    let leaked = tokio::time::timeout(Duration::from_millis(200), c.next_server_message()).await;
    assert!(
        leaked.is_err(),
        "room2 received an op from room1: {leaked:?}"
    );
}

#[tokio::test]
async fn late_joiner_gets_current_text() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "notes", 1).await;
    a.send_op(0, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack { version: 1 }
    );

    let (_, joined) = Client::connect(addr, "notes", 2).await;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "notes".to_string(),
            text: "hello".to_string(),
            version: 1,
        }
    );
}

#[tokio::test]
async fn reconnecting_client_replays_missed_ops() {
    let (addr, _server, _dir) = start_server().await;
    let (a, _) = Client::connect(addr, "notes", 1).await;
    let (mut b, _) = Client::connect(addr, "notes", 2).await;

    // Client A drops off at version 0 while B keeps typing.
    drop(a);
    b.send_op(0, Op::insert(2, 0, "hello")).await;
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Ack { version: 1 }
    );
    b.send_op(1, Op::insert(2, 5, " world")).await;
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Ack { version: 2 }
    );

    // A reconnects, joins (full text, version 2), then asks for just
    // the ops it missed since version 0.
    let (mut a, joined) = Client::connect(addr, "notes", 1).await;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "notes".to_string(),
            text: "hello world".to_string(),
            version: 2,
        }
    );
    a.send_sync(0).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ops {
            from_version: 0,
            ops: vec![Op::insert(2, 0, "hello"), Op::insert(2, 5, " world"),],
        }
    );

    // Already up to date: an empty replay.
    a.send_sync(2).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ops {
            from_version: 2,
            ops: vec![],
        }
    );
}

#[tokio::test]
async fn room_survives_a_server_restart() {
    let (addr, server, dir) = start_server().await;
    {
        let (mut a, _) = Client::connect(addr, "notes", 1).await;
        a.send_op(0, Op::insert(1, 0, "hello")).await;
        assert_eq!(
            a.next_server_message().await,
            ServerMessage::Ack { version: 1 }
        );
    }
    server.abort();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { serve_with_data_dir(listener, dir).await });

    let (_, joined) = Client::connect(addr, "notes", 2).await;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "notes".to_string(),
            text: "hello".to_string(),
            version: 1,
        }
    );
}

#[tokio::test]
async fn sync_from_before_the_snapshot_gets_the_whole_document() {
    let (addr, server, dir) = start_server().await;
    {
        let (mut a, _) = Client::connect(addr, "notes", 1).await;
        // Fifty-one ops force a snapshot at version 50 and trim the log,
        // so version 0 is no longer replayable after a restart.
        for i in 0..51 {
            a.send_op(i, Op::insert(1, i, "x")).await;
            assert_eq!(
                a.next_server_message().await,
                ServerMessage::Ack { version: i + 1 }
            );
        }
    }
    server.abort();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { serve_with_data_dir(listener, dir).await });

    let (mut a, joined) = Client::connect(addr, "notes", 1).await;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "notes".to_string(),
            text: "x".repeat(51),
            version: 51,
        }
    );
    a.send_sync(0).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Snapshot {
            text: "x".repeat(51),
            version: 51,
        }
    );
    // But a version after the snapshot still replays from the log.
    a.send_sync(50).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ops {
            from_version: 50,
            ops: vec![Op::insert(1, 50, "x")],
        }
    );
}
