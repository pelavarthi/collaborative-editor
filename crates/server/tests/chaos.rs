//! Chaos test: lag, dropped messages, and concurrent editors against
//! the real server over real WebSockets.
//!
//! Unit tests prove the transform is right for one pair of ops. This
//! file proves the whole system stays right when the network is not:
//! clients type at the same time with random delays between keystrokes,
//! one client drops off in the middle and comes back, and every client
//! still ends up with the same text at the same version.
//!
//! The checks are the ones that matter for a collaborative editor:
//! convergence (every client and the server agree on the text), and
//! nothing lost (every insert that the server accepted is in the final
//! text exactly once). Deletes may transform into no-ops when another
//! client deleted the same text first, so they are counted only when
//! the server acks them as applied.
//!
//! Two limits of this test, stated plainly: it only sends inserts and
//! deletes, never format ops, because overlapping conflicting formats
//! are already known not to converge with single-range ops (Day 9).
//! And each room stays under 64 ops in total, because the server's
//! broadcast buffer holds 64: past that, a client that stops reading
//! is told to rejoin instead of being kept up to date.

use collab_ot::{Document, Op};
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
    std::env::temp_dir().join(format!("collab-chaos-test-{}-{n}", std::process::id()))
}

/// A tiny deterministic RNG: the tests must play the same chaos every
/// run, so a failure can be replayed from its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A number in 0..n.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One client in a chaos run: a socket plus the local copy of the
/// document, kept up to date from acks and broadcasts.
struct ChaosClient {
    write: futures_util::stream::SplitSink<WsStream, Message>,
    read: futures_util::stream::SplitStream<WsStream>,
    id: u64,
    seq: u64,
    version: usize,
    doc: Document,
    /// Ops the server acked as applied (not the ones it dropped as
    /// no-ops). The final version should equal this count plus the
    /// ops the other clients got applied.
    applied: usize,
}

impl ChaosClient {
    async fn connect(addr: SocketAddr, room: &str, client_id: u64) -> Self {
        let (ws, _) = connect_async(format!("ws://{addr}")).await.unwrap();
        let (mut write, mut read) = ws.split();
        write
            .send(Message::Text(
                serde_json::to_string(&ClientMessage::Join {
                    room: room.to_string(),
                    client_id,
                    name: None,
                    read_only: false,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let joined = loop {
            let msg = read.next().await.unwrap().unwrap();
            let Message::Text(text) = msg else { continue };
            let parsed: ServerMessage = serde_json::from_str(&text).unwrap();
            if let ServerMessage::Joined { text, version, .. } = parsed {
                break (text, version);
            }
        };
        Self {
            write,
            read,
            id: client_id,
            seq: 0,
            version: joined.1,
            doc: Document::from_text(&joined.0),
            applied: 0,
        }
    }

    async fn send_raw(&mut self, msg: &ClientMessage) {
        self.write
            .send(Message::Text(serde_json::to_string(msg).unwrap().into()))
            .await
            .unwrap();
    }

    /// The next message that is not a presence update, or an error when
    /// nothing arrives in time: a stuck test should fail, not hang.
    async fn next_edit_message(&mut self) -> ServerMessage {
        let read = async {
            loop {
                let msg = self.read.next().await.unwrap().unwrap();
                let Message::Text(text) = msg else { continue };
                let parsed: ServerMessage = serde_json::from_str(&text).unwrap();
                match parsed {
                    ServerMessage::Presence { .. } | ServerMessage::PresenceLeft { .. } => {
                        continue
                    }
                    _ => return parsed,
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), read)
            .await
            .expect("timed out waiting for a server message")
    }

    /// Sends an op made against the document as this client knows it,
    /// then reads until its ack, applying every broadcast that arrives
    /// on the way. Returns true when the server applied the op.
    async fn send_op(&mut self, op: Op) -> bool {
        self.seq += 1;
        let seq = self.seq;
        self.send_raw(&ClientMessage::Op {
            base_version: self.version,
            seq,
            op,
        })
        .await;
        loop {
            match self.next_edit_message().await {
                ServerMessage::Ack {
                    version,
                    seq: ack_seq,
                    op,
                } => {
                    assert_eq!(ack_seq, seq, "ack for the wrong op");
                    self.version = version;
                    if let Some(applied) = op {
                        self.doc.apply(&applied).unwrap();
                        self.applied += 1;
                        return true;
                    }
                    return false;
                }
                ServerMessage::Op { version, op } => {
                    self.doc.apply(&op).unwrap();
                    self.version = version;
                }
                other => panic!("unexpected message while waiting for ack: {other:?}"),
            }
        }
    }

    /// Asks for the ops missed since this client's version and applies
    /// them, so its copy catches up to the server's. Broadcasts that
    /// were already on their way arrive before the reply and are
    /// applied too; a snapshot reply replaces the copy outright.
    async fn drain(&mut self) {
        self.send_raw(&ClientMessage::Sync {
            base_version: self.version,
        })
        .await;
        loop {
            match self.next_edit_message().await {
                ServerMessage::Op { version, op } => {
                    self.doc.apply(&op).unwrap();
                    self.version = version;
                }
                ServerMessage::Ops {
                    from_version, ops, ..
                } => {
                    // The reply covers every op since the Sync was
                    // sent, including any that already arrived as a
                    // broadcast above: skip those, or they would be
                    // applied twice.
                    let already = self.version.saturating_sub(from_version);
                    for op in ops.into_iter().skip(already) {
                        self.doc.apply(&op).unwrap();
                        self.version += 1;
                    }
                    return;
                }
                ServerMessage::Snapshot {
                    text, version, ..
                } => {
                    self.doc = Document::from_text(&text);
                    self.version = version;
                    return;
                }
                other => panic!("unexpected reply to Sync: {other:?}"),
            }
        }
    }
}

/// Starts a server on a free port, returning its address and a handle
/// to stop it. Each test gets its own server and data directory.
async fn start_server() -> (SocketAddr, JoinHandle<std::io::Result<()>>) {
    let dir = temp_data_dir();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { serve_with_data_dir(listener, dir).await });
    (addr, handle)
}

/// One random edit against the client's local copy: mostly a one-letter
/// insert at a random spot, sometimes a one-character delete.
fn random_op(rng: &mut Rng, client_id: u64, doc_len: usize) -> Op {
    if doc_len > 0 && rng.below(10) < 3 {
        Op::delete(client_id, rng.below(doc_len), 1)
    } else {
        let letter = (b'a' + rng.below(26) as u8) as char;
        Op::insert(client_id, rng.below(doc_len + 1), letter.to_string())
    }
}

/// Types `ops` random edits with a random pause of up to `max_delay_ms`
/// between them, then drains once. The client is returned so the test
/// can drain it again after every other client has finished: a drain
/// only catches the client up to the server's version at that moment.
async fn type_randomly(
    mut client: ChaosClient,
    ops: usize,
    max_delay_ms: u64,
    seed: u64,
) -> ChaosClient {
    let mut rng = Rng::new(seed);
    for _ in 0..ops {
        let op = random_op(&mut rng, client.id, client.doc.text().chars().count());
        client.send_op(op).await;
        let delay = rng.below(max_delay_ms as usize + 1) as u64;
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    client.drain().await;
    client
}

/// The text a client that joins now sees: the server's ground truth.
async fn server_text(addr: SocketAddr, room: &str) -> (String, usize) {
    let client = ChaosClient::connect(addr, room, 999).await;
    (client.doc.text().to_string(), client.version)
}

#[tokio::test]
async fn four_editors_type_at_once_and_converge() {
    let (addr, _server) = start_server().await;
    let room = "chaos-room";

    let mut tasks = Vec::new();
    for (i, seed) in [11, 23, 37, 41].into_iter().enumerate() {
        let client = ChaosClient::connect(addr, room, (i + 1) as u64).await;
        tasks.push(tokio::spawn(type_randomly(client, 15, 20, seed)));
    }
    let mut clients = Vec::new();
    for task in tasks {
        clients.push(task.await.unwrap());
    }
    // Drain again now that nobody is typing: a client that finished
    // early only caught up to the version at its own drain.
    for client in &mut clients {
        client.drain().await;
    }

    let (text, version) = server_text(addr, room).await;
    // Every client agrees with the server, on the text and the
    // version, and the version is exactly the ops the server applied.
    let applied: usize = clients.iter().map(|client| client.applied).sum();
    assert_eq!(version, applied, "version {version} != applied ops {applied}");
    for client in &clients {
        assert_eq!(
            client.doc.text(),
            text,
            "client {} diverged from the server",
            client.id
        );
        assert_eq!(
            client.version, version,
            "client {} is at version {}, server at {version}",
            client.id, client.version
        );
    }
}

#[tokio::test]
async fn everyone_types_at_the_same_spot_and_nothing_is_lost() {
    let (addr, _server) = start_server().await;
    let room = "same-spot";

    // Each client inserts only its own letter, always at position 0,
    // with up to 80ms of lag between keystrokes: the most transform
    // pressure the server can get, since every pair of concurrent
    // inserts lands on the same spot.
    let mut tasks = Vec::new();
    for (i, letter) in ['a', 'b', 'c'].into_iter().enumerate() {
        let client_id = (i + 1) as u64;
        let client = ChaosClient::connect(addr, room, client_id).await;
        tasks.push(tokio::spawn(async move {
            let mut client = client;
            let mut rng = Rng::new(100 + client_id);
            for _ in 0..15 {
                client.send_op(Op::insert(client_id, 0, letter.to_string())).await;
                tokio::time::sleep(Duration::from_millis(rng.below(81) as u64)).await;
            }
            client.drain().await;
            client
        }));
    }
    let mut clients = Vec::new();
    for task in tasks {
        clients.push(task.await.unwrap());
    }
    // Drain again now that nobody is typing: a client that finished
    // early only caught up to the version at its own drain.
    for client in &mut clients {
        client.drain().await;
    }

    let (text, version) = server_text(addr, room).await;
    assert_eq!(version, 45, "15 inserts x 3 clients should be 45 ops");
    // No insert is lost and none is doubled: the final text is 15 of
    // each letter, in whatever order the server happened to apply.
    let mut chars: Vec<char> = text.chars().collect();
    chars.sort_unstable();
    let expected = "a".repeat(15) + &"b".repeat(15) + &"c".repeat(15);
    assert_eq!(chars.into_iter().collect::<String>(), expected);
    for client in &clients {
        assert_eq!(
            client.doc.text(),
            text,
            "client {} diverged from the server",
            client.id
        );
        assert_eq!(client.version, version);
    }
}

#[tokio::test]
async fn client_that_goes_dark_and_reconnects_catches_up() {
    let (addr, _server) = start_server().await;
    let room = "dark-room";

    // A types a little, then its connection drops mid-session.
    let mut a = ChaosClient::connect(addr, room, 1).await;
    a.send_op(Op::insert(1, 0, "hello")).await;
    let old_version = a.version;
    let old_doc = a.doc.clone();
    drop(a);

    // While A is dark, B and C keep typing: 10 ops each.
    let mut b = ChaosClient::connect(addr, room, 2).await;
    let mut c = ChaosClient::connect(addr, room, 3).await;
    for i in 0..10 {
        b.send_op(Op::insert(2, b.doc.text().chars().count(), "b")).await;
        let _ = i;
        c.send_op(Op::insert(3, 0, "c")).await;
    }
    b.drain().await;
    c.drain().await;

    // A comes back. Its fresh join already has the current text, but
    // the replay path is what this test is about: the ops A missed,
    // applied to the copy A had when it went dark, must rebuild the
    // same text — otherwise the Sync answer is wrong.
    let mut rejoined = ChaosClient::connect(addr, room, 1).await;
    let current = rejoined.doc.text().to_string();
    let current_version = rejoined.version;
    assert_eq!(current_version, 21, "1 + 10 + 10 ops should be version 21");

    rejoined
        .send_raw(&ClientMessage::Sync {
            base_version: old_version,
        })
        .await;
    let mut replayed = old_doc;
    match rejoined.next_edit_message().await {
        ServerMessage::Ops { ops, .. } => {
            assert_eq!(ops.len(), 20, "A missed 20 ops while dark");
            for op in ops {
                replayed.apply(&op).unwrap();
            }
        }
        other => panic!("expected an Ops replay, got {other:?}"),
    }
    assert_eq!(replayed.text(), current, "replay did not rebuild the text");
    assert_eq!(b.doc.text(), current);
    assert_eq!(c.doc.text(), current);
}
