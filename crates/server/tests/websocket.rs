//! Two real WebSocket clients talking to a real server on a local port.

use collab_ot::Op;
use collab_server::message::{ClientMessage, ServerMessage};
use collab_server::presence::{color_for, Presence, Selection};
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

/// The presence a client that just joined has: caret at the start,
/// nothing selected, its colour from its id.
fn presence(client_id: u64, name: Option<&str>) -> Presence {
    Presence {
        client_id,
        name: name.map(str::to_string),
        color: color_for(client_id).to_string(),
        cursor: 0,
        selection: None,
    }
}

struct Client {
    write: futures_util::stream::SplitSink<WsStream, Message>,
    read: futures_util::stream::SplitStream<WsStream>,
}

impl Client {
    /// Connects and joins a room, returning the client and the Joined
    /// message the server answered with.
    async fn connect(addr: SocketAddr, room: &str, client_id: u64) -> (Self, ServerMessage) {
        Self::connect_named(addr, room, client_id, None).await
    }

    /// Like `connect`, but with a display name for the client's cursor.
    async fn connect_named(
        addr: SocketAddr,
        room: &str,
        client_id: u64,
        name: Option<&str>,
    ) -> (Self, ServerMessage) {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let (mut write, read) = ws.split();
        let join = ClientMessage::Join {
            room: room.to_string(),
            client_id,
            name: name.map(str::to_string),
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

    async fn send_presence(&mut self, cursor: usize, selection: Option<Selection>) {
        self.send(&ClientMessage::Presence { cursor, selection })
            .await;
    }

    /// The next message that is not a presence update: joins, presence
    /// changes and leaves interleave with ops and acks, and most tests
    /// only care about the editing traffic.
    async fn next_server_message(&mut self) -> ServerMessage {
        loop {
            let msg = self.next_raw_message().await;
            match msg {
                ServerMessage::Presence { .. } | ServerMessage::PresenceLeft { .. } => continue,
                _ => return msg,
            }
        }
    }

    /// The next message as it arrives, presence updates included.
    async fn next_raw_message(&mut self) -> ServerMessage {
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
            // Client 1 was already here, caret still at the start.
            presences: vec![presence(1, None)],
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
            // Client 2 kept typing while A was away.
            presences: vec![presence(2, None)],
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
            // Presence is not saved to disk: a restart clears the room.
            presences: vec![],
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
            presences: vec![],
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

#[tokio::test]
async fn presence_update_reaches_the_other_client() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut b, _) = Client::connect(addr, "room1", 2).await;

    // Some text to point at first: a presence past the end of the
    // document gets clamped, and the document starts out empty.
    a.send_op(0, Op::insert(1, 0, "hello world")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack { version: 1 }
    );
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Op {
            version: 1,
            op: Op::insert(1, 0, "hello world"),
        }
    );
    // A's insert pushed B's caret to the end of the new text.
    let mut b_moved = presence(2, None);
    b_moved.cursor = 11;
    assert_eq!(
        b.next_raw_message().await,
        ServerMessage::Presence { presence: b_moved }
    );

    a.send_presence(3, Some(Selection::new(1, 3))).await;
    let mut expected = presence(1, None);
    expected.cursor = 3;
    expected.selection = Some(Selection::new(1, 3));
    assert_eq!(
        b.next_raw_message().await,
        ServerMessage::Presence { presence: expected }
    );

    // The sender never gets its own presence back.
    let echo = tokio::time::timeout(Duration::from_millis(200), a.next_raw_message()).await;
    assert!(echo.is_err(), "sender received its own presence: {echo:?}");
}

#[tokio::test]
async fn joiner_sees_who_is_here_and_the_room_sees_the_joiner() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect_named(addr, "room1", 1, Some("pranav")).await;
    let (mut b, joined) = Client::connect_named(addr, "room1", 2, Some("hriday")).await;

    // B's Joined lists A, who was already here with a name.
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "room1".to_string(),
            text: String::new(),
            version: 0,
            presences: vec![presence(1, Some("pranav"))],
        }
    );
    // And A learns about B through a presence broadcast.
    assert_eq!(
        a.next_raw_message().await,
        ServerMessage::Presence {
            presence: presence(2, Some("hriday")),
        }
    );

    // B does not get its own presence back.
    let echo = tokio::time::timeout(Duration::from_millis(200), b.next_raw_message()).await;
    assert!(echo.is_err(), "joiner received its own presence: {echo:?}");
}

#[tokio::test]
async fn leaving_client_drops_its_cursor() {
    let (addr, _server, _dir) = start_server().await;
    let (a, _) = Client::connect(addr, "room1", 1).await;
    let (mut b, _) = Client::connect(addr, "room1", 2).await;

    drop(a);
    assert_eq!(
        b.next_raw_message().await,
        ServerMessage::PresenceLeft { client_id: 1 }
    );
}

#[tokio::test]
async fn cursor_shifts_when_another_client_inserts_before_it() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut b, _) = Client::connect(addr, "room1", 2).await;

    // A types "hello", then parks its caret at the end.
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
    // A's insert landed at the start, pushing B's caret (at 0) to the
    // end of "hello" before B ever reports a position.
    let mut b_moved = presence(2, None);
    b_moved.cursor = 5;
    assert_eq!(
        b.next_raw_message().await,
        ServerMessage::Presence { presence: b_moved }
    );
    a.send_presence(5, None).await;
    let mut at_end = presence(1, None);
    at_end.cursor = 5;
    assert_eq!(
        b.next_raw_message().await,
        ServerMessage::Presence {
            presence: at_end.clone(),
        }
    );

    // B inserts ">> " at the start; A's caret rides right by three,
    // so it still points at the end of "hello".
    b.send_op(1, Op::insert(2, 0, ">> ")).await;
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Ack { version: 2 }
    );
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Op {
            version: 2,
            op: Op::insert(2, 0, ">> "),
        }
    );
    at_end.cursor = 8;
    assert_eq!(
        a.next_raw_message().await,
        ServerMessage::Presence { presence: at_end }
    );
}

#[tokio::test]
async fn presence_past_the_end_of_the_document_is_clamped() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut b, _) = Client::connect(addr, "room1", 2).await;

    // The document is empty, so a caret at 99 has nowhere to sit.
    a.send_presence(99, Some(Selection::new(0, 99))).await;
    let mut clamped = presence(1, None);
    clamped.cursor = 0;
    clamped.selection = Some(Selection::new(0, 0));
    assert_eq!(
        b.next_raw_message().await,
        ServerMessage::Presence { presence: clamped }
    );
}
