//! Two real WebSocket clients talking to a real server on a local port.

use collab_ot::Op;
use collab_server::message::{ClientMessage, ServerMessage};
use collab_server::serve;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

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

    async fn send_op(&mut self, base_version: usize, op: Op) {
        let msg = ClientMessage::Op { base_version, op };
        self.write
            .send(Message::Text(serde_json::to_string(&msg).unwrap().into()))
            .await
            .unwrap();
    }

    async fn next_server_message(&mut self) -> ServerMessage {
        let msg = self.read.next().await.unwrap().unwrap();
        let Message::Text(text) = msg else {
            panic!("expected a text message, got {msg:?}");
        };
        serde_json::from_str(&text).unwrap()
    }
}

async fn start_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        serve(listener).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn op_from_one_client_reaches_the_other() {
    let addr = start_server().await;
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
    let addr = start_server().await;
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
    let addr = start_server().await;
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
