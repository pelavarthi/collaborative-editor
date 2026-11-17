# collaborative-editor
Real-time collaborative text editor in Rust + TypeScript: OT core, WebSocket sync, presence, undo, rich text.

## TypeScript client

`client/` is a small TypeScript editor UI that talks to the Rust server.
Typing echoes locally at once — the keystroke is applied to the local
copy and sent as an op behind it — so there is no waiting on the network.
Remote ops arrive already transformed by the server, so the client
applies them as-is and moves the caret past them.

```sh
cd client
npm install
npm test        # typecheck + unit tests + two-client integration test
npm run build
npm start       # serves the editor at http://localhost:3000
```

Start the server first (`cargo run -p collab-server`), then open
http://localhost:3000/?room=notes&name=pranav. Open the same room in a
second tab to see live cursors and shared typing.

Edits are numbered, and each one stays queued until the server acks it.
If the connection drops, the client reconnects on its own (backing off
from half a second up to eight) and resends whatever was not acked; the
server answers a resend from memory instead of applying the edit twice,
and the ack carries the edit as the server applied it, so the client's
copy ends up matching even when other people typed in the meantime.
The client keeps working offline: typing still echoes and queues, the
status line says offline and how many edits are unsent, and the text
and the queue are saved on the device after each change, so a reload
while offline brings the edits back. When the network returns, the
client reconnects at once and the server transforms each resent edit
past whatever landed while it was away, so both sides converge. Two
conflicts cannot be transformed away and are handled by resyncing
instead: an edit the server dropped as a no-op is still in the local
text, so the client rejoins for a fresh copy, and edits written
against a version the server has compacted away are dropped with a
warning rather than replayed onto the wrong document.

Opening the client with no room shows the documents list instead:
every document on the server, newest first, with its title (the first
line of the text), a preview, its version, and when it was last saved,
plus a form to create a new one by name. Inside a document, "Copy edit
link" copies a link others can edit with, and "Copy read-only link"
copies one with `?mode=read`: it opens the same document as a viewer,
with a locked box and no format buttons, and the server refuses any
edit a viewer tries to send — read-only is enforced server-side, not
just in the UI.

The server is also tested under chaos: several clients type at once
with random lag between keystrokes, one drops off mid-session and
comes back, and every client still ends up with the same text at the
same version. See `crates/server/tests/chaos.rs`.
