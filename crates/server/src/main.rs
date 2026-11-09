//! `collab-server`: one binary, one address. Clients connect over
//! WebSocket and join a room by name; every room is one shared document.

use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let listener = TcpListener::bind(&addr).await?;
    println!("collab-server listening on {}", listener.local_addr()?);
    collab_server::serve(listener).await
}
