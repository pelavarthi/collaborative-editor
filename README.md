# collaborative-editor

A real-time collaborative text editor: several people type in the same
document at once, and everyone ends up with the same text. The server
is Rust, the client is TypeScript, and they talk over a WebSocket.

## What it does

- **Shared typing.** Every keystroke is an op (insert, delete, or
  format). The server orders the ops and transforms each one past the
  edits its author had not seen yet, so two people typing at the same
  spot converge instead of overwriting each other.
- **Local echo.** The client applies your keystroke at once and sends
  the op behind it, so typing never waits on the network. Each op
  stays queued until the server acks it, and the ack carries the op
  as the server applied it.
- **Presence.** Live cursors and selections, one color per person.
- **Undo and redo.** Undo sends the inverse of an op, transformed past
  whatever landed since, so it composes with other people's edits.
- **Rich text.** Bold, italic, and links, stored as attributes on each
  character, so formatting survives other people's typing.
- **Documents list and sharing.** Opening the client with no room shows
  every document on the server, newest first. "Copy edit link" shares
  a link others can edit with; "Copy read-only link" shares one that
  opens as a viewer. Read-only is enforced on the server, not just in
  the UI.
- **Persistence.** Rooms are saved to disk as a snapshot plus an op
  log, so a restart brings back every document. A reconnecting client
  asks for the ops it missed; if it is too far behind, it takes a
  fresh snapshot instead.
- **Offline mode.** Typing still echoes and queues while offline, and
  the text and the queue are saved on the device after each change,
  so a reload while offline brings the edits back. On reconnect the
  server transforms each resent edit past whatever landed meanwhile.
  Two conflicts cannot be transformed away and are handled by
  resyncing instead: an edit the server dropped as a no-op, and edits
  written against a version the server has compacted away.
- **Chaos-tested.** Several clients type at once with random lag
  between keystrokes, one drops off mid-session and comes back, and
  every client still ends up with the same text at the same version.

## Quickstart

Start the server first, then the client:

```sh
cargo run --release -p collab-server
```

```sh
cd client
npm install
npm run build
npm start       # serves the editor at http://localhost:3000
```

Open http://localhost:3000/?room=notes&name=pranav, then open the same
room in a second tab to see live cursors and shared typing. Opening
the client with no room shows the documents list instead.

The server takes an address and a data directory, both optional:

```sh
cargo run -p collab-server -- 127.0.0.1:8080 ./data
# or: COLLAB_DATA_DIR=./data cargo run -p collab-server
```

## Demo

`demo/run.sh` starts a throwaway server and runs a scripted session
with two clients, no browser needed. Alice and Bob join the same room
and type at position 0 at the same time; the script prints both
copies, bolds a word from one side, checks the other side sees it,
and checks a cursor move arrives as presence:

```sh
./demo/run.sh
```

It prints the final document and exits non-zero if the two clients
ever disagree. `demo/demo.mjs` on its own connects to the server at
`ws://127.0.0.1:8080` (or the URL in its first argument).

## How it fits together

```text
                WebSocket (JSON)
  ┌──────────────┐                  ┌──────────────────────────┐
  │  TypeScript  │  join / op /     │  Rust server             │
  │  client      │ ───────────────► │  room: one document +    │
  │  editor UI,  │  op / ack /      │  op log, in apply order  │
  │  local echo, │ ◄─────────────── │                          │
  │  pending     │  presence        │  store: snapshot + op    │
  │  queue,      │                  │  log on disk, fsync      │
  │  offline     │                  │  per op                  │
  │  outbox      │                  └──────────────────────────┘
  └──────────────┘
```

- `crates/ot` — the OT core: the document model, ops, transform,
  and undo history. No networking, no I/O.
- `crates/server` — the WebSocket server: rooms and broadcast,
  persistence, the documents list, and read-only enforcement.
- `client/` — the TypeScript editor UI: local echo, acks and the
  pending queue, reconnect catch-up, presence, rich text, the
  documents list, and the offline outbox.

One rule holds the whole thing together: the server applies ops in
one order and delivers them to clients in that same order. Acks go
out only after any queued broadcasts for that client, so a client
never sees an ack for an op it has not yet seen the ops before it
for. Break that ordering and clients diverge by a character — the
chaos test caught exactly that bug on Day 12.

## The protocol

One JSON object per WebSocket text frame. Client messages:

| Message | What it means |
| --- | --- |
| `join` | Join a room, with a client id, a display name, and whether this client is read-only. |
| `list` | Ask for the documents list before joining. |
| `op` | An edit made against the document at `base_version`. `seq` counts this client's ops from 1 up, so a resend after a lost ack carries the same number. |
| `sync` | Ask for the ops missed since `base_version`. |
| `presence` | Where this client's caret is, and what it has highlighted. |

Server messages: `joined` (the room's text, formatting, version, and
who else is here), `docs` (the documents list), `op` (an op from
another client, already transformed and applied), `ack` (your op as
applied, or null when it transformed into a no-op), `ops` (the missed
ops for a sync), `snapshot` (the whole document, when the client is
too far behind to catch up by ops), `presence`, `presence_left`, and
`error`.

Ops are serde's externally tagged enums: `{"Insert": {"site": 1,
"pos": 0, "text": "hi"}}`. A plain insert carries no `attrs` field;
only styled inserts do. See `crates/server/src/message.rs` and
`client/src/protocol.ts` — the two files must match exactly.

## Testing

```sh
cargo test --release   # 122 Rust tests: OT convergence, undo, server, chaos
cd client && npm test  # typecheck + 72 TypeScript tests, incl. a two-client integration test
cargo clippy --all-targets
cargo fmt --check
```

`crates/server/tests/chaos.rs` runs real WebSocket clients against
the real server: four editors typing at once, three hammering the same
spot, one dropping off and coming back, all with random lag. Every
client must end with the same text at the same version.

## Benchmarks

`crates/server/examples/bench.rs` times ten cases with no extra
dependencies:

```sh
cargo run --release -p collab-server --example bench
```

Results and what was tuned are in `BENCHMARKS.md`. Typing at the end
of a 10k-character document costs about 86 ns per keystroke after the
Day 13 tuning; mid-document edits still scan from the start, so a
rope is the next step if documents get large.

## Known limits

- No authentication. Anyone who can reach the server can open any
  room; read-only links are sharing, not security.
- Mid-document edits are O(document length): the document is one
  string, so inserting in the middle shifts the rest. A rope would
  fix that.
- The op log is compacted into snapshots every 50 ops, so a client
  more than 50 versions behind takes a full snapshot instead of
  catching up op by op.
- Overlapping conflicting formats (bold and italic applied to the
  same range in different orders) do not converge with single-range
  ops; that needs op splitting.
