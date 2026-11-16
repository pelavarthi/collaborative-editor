//! Two real WebSocket clients talking to a real server on a local port.

use collab_ot::{Attrs, Op};
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
    next_seq: u64,
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
        Self::connect_with_mode(addr, room, client_id, name, false).await
    }

    /// Joins as a viewer: the client can see the document, but the
    /// server refuses its edits.
    async fn connect_read_only(addr: SocketAddr, room: &str, client_id: u64) -> (Self, ServerMessage) {
        Self::connect_with_mode(addr, room, client_id, None, true).await
    }

    async fn connect_with_mode(
        addr: SocketAddr,
        room: &str,
        client_id: u64,
        name: Option<&str>,
        read_only: bool,
    ) -> (Self, ServerMessage) {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let (mut write, read) = ws.split();
        let join = ClientMessage::Join {
            room: room.to_string(),
            client_id,
            name: name.map(str::to_string),
            read_only,
        };
        write
            .send(Message::Text(serde_json::to_string(&join).unwrap().into()))
            .await
            .unwrap();
        let mut client = Self {
            write,
            read,
            next_seq: 1,
        };
        let joined = client.next_server_message().await;
        assert!(matches!(joined, ServerMessage::Joined { .. }));
        (client, joined)
    }

    /// Connects without joining anything: the client asks for the
    /// documents list first and joins after it.
    async fn raw_connect(addr: SocketAddr) -> Self {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let (write, read) = ws.split();
        Self {
            write,
            read,
            next_seq: 1,
        }
    }

    async fn send(&mut self, msg: &ClientMessage) {
        self.write
            .send(Message::Text(serde_json::to_string(msg).unwrap().into()))
            .await
            .unwrap();
    }

    /// Sends an op with the next sequence number for this client.
    async fn send_op(&mut self, base_version: usize, op: Op) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.send_op_with_seq(base_version, seq, op).await;
    }

    /// Sends an op with an explicit sequence number: a resend after a
    /// lost ack reuses the number of the op it repeats.
    async fn send_op_with_seq(&mut self, base_version: usize, seq: u64, op: Op) {
        self.send(&ClientMessage::Op {
            base_version,
            seq,
            op,
        })
        .await;
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
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
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
async fn resent_op_after_a_lost_ack_applies_once() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut b, _) = Client::connect(addr, "room1", 2).await;

    // A sends an edit and gets its ack.
    a.send_op_with_seq(0, 1, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
    );
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Op {
            version: 1,
            op: Op::insert(1, 0, "hello"),
        }
    );

    // The ack is lost on the way back, so A resends the same op with
    // the same number: the server answers from memory instead of
    // applying it again, and B sees nothing twice.
    a.send_op_with_seq(0, 1, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
    );
    let echo = tokio::time::timeout(Duration::from_millis(200), b.next_server_message()).await;
    assert!(echo.is_err(), "resent op was broadcast twice: {echo:?}");
}

#[tokio::test]
async fn rooms_are_isolated() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "room1", 1).await;
    let (mut c, _) = Client::connect(addr, "room2", 3).await;

    a.send_op(0, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
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
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
    );

    let (_, joined) = Client::connect(addr, "notes", 2).await;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "notes".to_string(),
            text: "hello".to_string(),
            attrs: vec![Attrs::plain(); 5],
            version: 1,
            // Client 1 was already here, caret still at the start.
            presences: vec![presence(1, None)],
            read_only: false,
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
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(2, 0, "hello")),
        }
    );
    b.send_op(1, Op::insert(2, 5, " world")).await;
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Ack {
            version: 2,
            seq: 2,
            op: Some(Op::insert(2, 5, " world")),
        }
    );

    // A reconnects, joins (full text, version 2), then asks for just
    // the ops it missed since version 0.
    let (mut a, joined) = Client::connect(addr, "notes", 1).await;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "notes".to_string(),
            text: "hello world".to_string(),
            attrs: vec![Attrs::plain(); 11],
            version: 2,
            // Client 2 kept typing while A was away.
            presences: vec![presence(2, None)],
            read_only: false,
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
            ServerMessage::Ack {
                version: 1,
                seq: 1,
                op: Some(Op::insert(1, 0, "hello")),
            }
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
            attrs: vec![Attrs::plain(); 5],
            version: 1,
            // Presence is not saved to disk: a restart clears the room.
            presences: vec![],
            read_only: false,
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
                ServerMessage::Ack {
                    version: i + 1,
                    seq: (i + 1) as u64,
                    op: Some(Op::insert(1, i, "x")),
                }
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
            attrs: vec![Attrs::plain(); 51],
            version: 51,
            presences: vec![],
            read_only: false,
        }
    );
    a.send_sync(0).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Snapshot {
            text: "x".repeat(51),
            attrs: vec![Attrs::plain(); 51],
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
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello world")),
        }
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
            attrs: vec![],
            version: 0,
            presences: vec![presence(1, Some("pranav"))],
            read_only: false,
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
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
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
        ServerMessage::Ack {
            version: 2,
            seq: 1,
            op: Some(Op::insert(2, 0, ">> ")),
        }
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

#[tokio::test]
async fn formatting_reaches_the_other_client_and_a_late_joiner() {
    use collab_ot::FormatChange;

    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "rich", 1).await;
    let (mut b, _) = Client::connect(addr, "rich", 2).await;

    a.send_op(0, Op::insert(1, 0, "hello world")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello world")),
        }
    );
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Op {
            version: 1,
            op: Op::insert(1, 0, "hello world"),
        }
    );

    // A bolds "hello": B gets the format op, and the text is unchanged.
    let bold = Op::format(1, 0, 5, FormatChange::bold(true));
    a.send_op(1, bold.clone()).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack {
            version: 2,
            seq: 2,
            op: Some(bold.clone()),
        }
    );
    assert_eq!(
        b.next_server_message().await,
        ServerMessage::Op {
            version: 2,
            op: bold,
        }
    );

    // A client that joins now gets the bold with the text.
    let (_, joined) = Client::connect(addr, "rich", 3).await;
    let mut attrs = vec![Attrs::plain(); 11];
    for attr in attrs.iter_mut().take(5) {
        *attr = Attrs::bold();
    }
    // B's cursor moved to the end of the text when A's insert landed.
    let mut b_presence = presence(2, None);
    b_presence.cursor = 11;
    assert_eq!(
        joined,
        ServerMessage::Joined {
            room: "rich".to_string(),
            text: "hello world".to_string(),
            attrs,
            version: 2,
            presences: vec![presence(1, None), b_presence],
            read_only: false,
        }
    );
}

#[tokio::test]
async fn list_before_join_then_join_on_the_same_connection() {
    let (addr, _server, _dir) = start_server().await;
    let mut client = Client::raw_connect(addr).await;

    // Nothing saved yet: the list is empty, and the connection stays
    // open for the join that follows.
    client.send(&ClientMessage::List).await;
    assert_eq!(
        client.next_server_message().await,
        ServerMessage::Docs { docs: vec![] }
    );

    client
        .send(&ClientMessage::Join {
            room: "notes".to_string(),
            client_id: 1,
            name: None,
            read_only: false,
        })
        .await;
    assert!(matches!(
        client.next_server_message().await,
        ServerMessage::Joined { .. }
    ));
}

#[tokio::test]
async fn list_shows_documents_newest_first() {
    let (addr, _server, _dir) = start_server().await;
    let (mut a, _) = Client::connect(addr, "notes", 1).await;
    a.send_op(0, Op::insert(1, 0, "Shopping list\nmilk")).await;
    assert_eq!(
        a.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "Shopping list\nmilk")),
        }
    );
    // "ideas" is created second. Its name also sorts first, so the
    // newest-first order holds whether the two saves land in the same
    // second or not.
    let (mut c, _) = Client::connect(addr, "ideas", 3).await;
    c.send_op(0, Op::insert(3, 0, "Big idea")).await;
    assert_eq!(
        c.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(3, 0, "Big idea")),
        }
    );

    let mut lister = Client::raw_connect(addr).await;
    lister.send(&ClientMessage::List).await;
    let ServerMessage::Docs { docs } = lister.next_server_message().await else {
        panic!("expected a Docs reply to a List");
    };
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].id, "ideas");
    assert_eq!(docs[0].title, "Big idea");
    assert_eq!(docs[0].preview, "Big idea");
    assert_eq!(docs[0].version, 1);
    assert!(docs[0].updated_at.is_some());
    assert_eq!(docs[1].id, "notes");
    assert_eq!(docs[1].title, "Shopping list");
    assert_eq!(docs[1].preview, "Shopping list milk");
    assert_eq!(docs[1].version, 1);
}

#[tokio::test]
async fn read_only_join_reports_read_only_and_refuses_edits() {
    let (addr, _server, _dir) = start_server().await;
    let (mut editor, _) = Client::connect(addr, "notes", 1).await;
    let (mut viewer, joined) = Client::connect_read_only(addr, "notes", 2).await;
    assert!(
        matches!(joined, ServerMessage::Joined { read_only: true, .. }),
        "read-only join must say so: {joined:?}"
    );

    // The viewer's edit is refused, and nothing is broadcast.
    viewer.send_op(0, Op::insert(2, 0, "nope")).await;
    assert_eq!(
        viewer.next_server_message().await,
        ServerMessage::Error {
            message: "this document is read-only for you".to_string(),
        }
    );
    let leaked = tokio::time::timeout(Duration::from_millis(200), editor.next_server_message()).await;
    assert!(leaked.is_err(), "refused op reached the editor: {leaked:?}");

    // The editor can still write, and the viewer still sees it: the
    // refused op never moved the version.
    editor.send_op(0, Op::insert(1, 0, "hello")).await;
    assert_eq!(
        editor.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello")),
        }
    );
    assert_eq!(
        viewer.next_server_message().await,
        ServerMessage::Op {
            version: 1,
            op: Op::insert(1, 0, "hello"),
        }
    );
}

#[tokio::test]
async fn read_only_viewer_can_still_move_its_cursor() {
    let (addr, _server, _dir) = start_server().await;
    let (mut editor, _) = Client::connect(addr, "notes", 1).await;
    let (mut viewer, _) = Client::connect_read_only(addr, "notes", 2).await;

    editor.send_op(0, Op::insert(1, 0, "hello world")).await;
    assert_eq!(
        editor.next_server_message().await,
        ServerMessage::Ack {
            version: 1,
            seq: 1,
            op: Some(Op::insert(1, 0, "hello world")),
        }
    );
    assert_eq!(
        viewer.next_server_message().await,
        ServerMessage::Op {
            version: 1,
            op: Op::insert(1, 0, "hello world"),
        }
    );

    // The viewer reports its caret; the editor sees it move, even
    // though the viewer cannot type.
    viewer.send_presence(5, None).await;
    let mut seen = false;
    for _ in 0..3 {
        match editor.next_raw_message().await {
            ServerMessage::Presence { presence } if presence.client_id == 2 => {
                if presence.cursor == 5 {
                    seen = true;
                    break;
                }
            }
            _ => continue,
        }
    }
    assert!(seen, "editor never saw the viewer's cursor at 5");
}
