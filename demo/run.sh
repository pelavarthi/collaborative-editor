#!/bin/sh
# Runs the scripted two-client demo against a throwaway server:
# builds the server, starts it on a scratch data directory, runs
# demo/demo.mjs, then stops the server and deletes the data.
# Usage: ./demo/run.sh
set -e
cd "$(dirname "$0")/.."

export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/workspace/tools/rust/rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/workspace/tools/rust/cargo}"
export PATH="$CARGO_HOME/bin:$PATH"

PORT="${COLLAB_DEMO_PORT:-18080}"
DATA_DIR="$(mktemp -d)"
echo "building server..."
cargo build --release -p collab-server -q
echo "starting server on 127.0.0.1:$PORT (data in $DATA_DIR)..."
./target/release/collab-server "127.0.0.1:$PORT" "$DATA_DIR" &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null; rm -rf "$DATA_DIR"' EXIT

# Wait until the server accepts a connection.
for _ in 1 2 3 4 5 6 7 8 9 10; do
  if node -e "const s=new WebSocket('ws://127.0.0.1:$PORT');s.onopen=()=>{s.close();process.exit(0)};s.onerror=()=>process.exit(1);" 2>/dev/null; then
    break
  fi
  sleep 0.5
done

echo "running demo..."
node demo/demo.mjs "ws://127.0.0.1:$PORT"
