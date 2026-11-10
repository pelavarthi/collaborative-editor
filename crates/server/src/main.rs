//! `collab-server`: one binary, one address. Clients connect over
//! WebSocket and join a room by name; every room is one shared document.
//!
//! Rooms are saved to disk under the data directory (second argument,
//! `COLLAB_DATA_DIR`, or `./data`), so a restart brings back every
//! room's text and history.

use std::path::PathBuf;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let data_dir = args
        .next()
        .map(PathBuf::from)
        .or_else(|| std::env::var("COLLAB_DATA_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("data"));
    let listener = TcpListener::bind(&addr).await?;
    println!("collab-server listening on {}", listener.local_addr()?);
    collab_server::serve_with_data_dir(listener, data_dir).await
}
