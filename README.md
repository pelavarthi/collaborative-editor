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
