import test from "node:test";
import assert from "node:assert/strict";
import { CollabClient, type Socket } from "../client.js";
import { encodeClient, insertOp } from "../protocol.js";

class FakeSocket implements Socket {
  sent: string[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((data: string) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: ((error: unknown) => void) | null = null;

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
    this.onclose?.();
  }

  /** Pretends the server sent `text` down the wire. */
  receive(text: string): void {
    this.onmessage?.(text);
  }

  open(): void {
    this.onopen?.();
  }
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 10));

function connect(site = 1, room = "notes"): {
  client: CollabClient;
  sockets: FakeSocket[];
  socket: FakeSocket;
} {
  const sockets: FakeSocket[] = [];
  const client = new CollabClient(
    site,
    () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
    // Reconnect at once in tests; the real client backs off.
    { reconnectDelay: () => 0 },
  );
  client.connect("ws://localhost:8080", room, "pranav");
  const first = sockets[0];
  assert.ok(first);
  first.open();
  return { client, sockets, socket: first };
}

test("connect sends a join as its first message", () => {
  const { socket } = connect();
  assert.deepEqual(JSON.parse(socket.sent[0] ?? ""), {
    type: "join",
    room: "notes",
    client_id: 1,
    name: "pranav",
  });
});

test("joined sets the text, version, and who is already here", () => {
  const { client, socket } = connect();
  socket.receive(
    '{"type":"joined","room":"notes","text":"hello","version":1,"presences":[{"client_id":2,"name":"hriday","color":"#f76b15","cursor":5,"selection":null}]}',
  );
  assert.equal(client.editor.text, "hello");
  assert.equal(client.editor.version, 1);
  assert.equal(client.status, "connected");
  assert.equal(client.presences.get(2)?.name, "hriday");
});

test("local edit echoes at once and sends the op with its sequence number", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  assert.equal(client.editor.text, "hello");
  assert.equal(client.pendingCount, 1);
  assert.equal(socket.sent.length, 2);
  assert.equal(
    socket.sent[1],
    encodeClient({ type: "op", base_version: 0, seq: 1, op: insertOp(1, 0, "hello") }),
  );
});

test("an ack clears the op from the pending queue", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  assert.equal(client.pendingCount, 1);
  socket.receive(
    '{"type":"ack","version":1,"seq":1,"op":{"Insert":{"site":1,"pos":0,"text":"hello"}}}',
  );
  assert.equal(client.pendingCount, 0);
  assert.equal(client.editor.version, 1);
});

test("remote op applies and a stale ack does not rewind the version", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  socket.receive('{"type":"op","version":2,"op":{"Insert":{"site":2,"pos":0,"text":">> "}}}');
  assert.equal(client.editor.text, ">> hello");
  assert.equal(client.editor.version, 2);
  socket.receive(
    '{"type":"ack","version":1,"seq":1,"op":{"Insert":{"site":1,"pos":0,"text":"hello"}}}',
  );
  assert.equal(client.editor.version, 2);
  assert.equal(client.pendingCount, 0);
});

test("an ack for an unknown seq takes the version and leaves the queue alone", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  socket.receive(
    '{"type":"ack","version":5,"seq":99,"op":{"Insert":{"site":1,"pos":0,"text":"hello"}}}',
  );
  assert.equal(client.pendingCount, 1);
  assert.equal(client.editor.version, 5);
});

test("presence updates and leaves track the room", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  socket.receive(
    '{"type":"presence","presence":{"client_id":2,"name":null,"color":"#46a758","cursor":3,"selection":null}}',
  );
  assert.equal(client.presences.get(2)?.cursor, 3);
  socket.receive('{"type":"presence_left","client_id":2}');
  assert.equal(client.presences.has(2), false);
});

test("server error surfaces through onError", () => {
  const { client, socket } = connect();
  const errors: string[] = [];
  client.onError = (message) => errors.push(message);
  socket.receive('{"type":"error","message":"bad message: oops"}');
  assert.deepEqual(errors, ["bad message: oops"]);
  assert.equal(client.lastError, "bad message: oops");
});

test("a missed-messages error asks for a sync instead of failing", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"hello","version":3,"presences":[]}');
  const errors: string[] = [];
  client.onError = (message) => errors.push(message);
  socket.receive('{"type":"error","message":"missed messages, please rejoin"}');
  assert.deepEqual(errors, []);
  assert.equal(socket.sent.length, 2);
  assert.deepEqual(JSON.parse(socket.sent[1] ?? ""), { type: "sync", base_version: 3 });
});

test("garbage from the server surfaces as an error, not a crash", () => {
  const { client, socket } = connect();
  const errors: string[] = [];
  client.onError = (message) => errors.push(message);
  socket.receive("this is not json");
  assert.equal(errors.length, 1);
});

test("sendPresence and sendSync send the right shapes", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.sendPresence(4, { anchor: 1, head: 4 });
  client.sendSync(0);
  assert.deepEqual(JSON.parse(socket.sent[1] ?? ""), {
    type: "presence",
    cursor: 4,
    selection: { anchor: 1, head: 4 },
  });
  assert.deepEqual(JSON.parse(socket.sent[2] ?? ""), { type: "sync", base_version: 0 });
});

test("snapshot replaces the text wholesale", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"old","version":1,"presences":[]}');
  socket.receive('{"type":"snapshot","text":"restored text","version":7}');
  assert.equal(client.editor.text, "restored text");
  assert.equal(client.editor.version, 7);
});

test("a dropped socket reconnects instead of closing", async () => {
  const { client, sockets } = connect();
  sockets[0]?.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  sockets[0]?.close();
  assert.equal(client.status, "reconnecting");
  await tick();
  assert.equal(sockets.length, 2);
  // The new socket joins the same room again.
  const second = sockets[1];
  assert.ok(second);
  second.open();
  assert.deepEqual(JSON.parse(second.sent[0] ?? ""), {
    type: "join",
    room: "notes",
    client_id: 1,
    name: "pranav",
  });
});

test("edits typed while down are queued and resent on reconnect", async () => {
  const { client, sockets } = connect();
  const first = sockets[0];
  assert.ok(first);
  first.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  first.receive(
    '{"type":"ack","version":1,"seq":1,"op":{"Insert":{"site":1,"pos":0,"text":"hello"}}}',
  );
  assert.equal(client.pendingCount, 0);

  // The socket drops; the client reconnects on its own.
  client.dropConnection();
  assert.equal(client.status, "reconnecting");
  await tick();
  assert.equal(sockets.length, 2);
  const second = sockets[1];
  assert.ok(second);

  // Typing while down: echoed locally, queued, nothing sent yet.
  client.localEdit("hello!");
  assert.equal(client.editor.text, "hello!");
  assert.equal(client.pendingCount, 1);
  assert.equal(second.sent.length, 0);

  second.open();
  assert.deepEqual(JSON.parse(second.sent[0] ?? ""), {
    type: "join",
    room: "notes",
    client_id: 1,
    name: "pranav",
  });
  // The rejoin hands back the server text, without the offline edit.
  second.receive('{"type":"joined","room":"notes","text":"hello","version":1,"presences":[]}');
  assert.equal(client.status, "connected");
  assert.equal(client.editor.text, "hello");
  // The queued edit goes out again with its original number.
  assert.equal(second.sent.length, 2);
  assert.equal(
    second.sent[1],
    encodeClient({ type: "op", base_version: 1, seq: 2, op: insertOp(1, 5, "!") }),
  );
  // The ack carries the op as applied; the rejoin wiped the echo, so
  // the ack puts the edit back into the text.
  second.receive(
    '{"type":"ack","version":2,"seq":2,"op":{"Insert":{"site":1,"pos":5,"text":"!"}}}',
  );
  assert.equal(client.editor.text, "hello!");
  assert.equal(client.pendingCount, 0);
});

test("an ack with a null op drops the queued edit without changing the text", async () => {
  const { client, sockets } = connect();
  const first = sockets[0];
  assert.ok(first);
  first.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  client.dropConnection();
  await tick();
  const second = sockets[1];
  assert.ok(second);
  second.open();
  second.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  assert.equal(second.sent.length, 2);
  // The server says the edit transformed into a no-op: nothing to put
  // back, but the queue entry is done.
  second.receive('{"type":"ack","version":0,"seq":1,"op":null}');
  assert.equal(client.pendingCount, 0);
  assert.equal(client.editor.text, "");
});

test("closing the client reports disconnected", () => {
  const { client, socket } = connect();
  const kinds: string[] = [];
  client.onChange = (change) => kinds.push(change.kind);
  client.close();
  assert.equal(client.status, "closed");
  assert.ok(kinds.includes("status"));
  assert.equal(socket.closed, true);
});

test("close stops reconnecting after a drop", async () => {
  const { client, sockets } = connect();
  sockets[0]?.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.dropConnection();
  assert.equal(client.status, "reconnecting");
  client.close();
  assert.equal(client.status, "closed");
  await tick();
  await tick();
  assert.equal(sockets.length, 1);
});

test("read-only connect sends read_only and locks local edits", () => {
  const sockets: FakeSocket[] = [];
  const client = new CollabClient(
    1,
    () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
    { reconnectDelay: () => 0 },
  );
  client.connect("ws://localhost:8080", "notes", "pranav", true);
  const socket = sockets[0];
  assert.ok(socket);
  socket.open();
  assert.deepEqual(JSON.parse(socket.sent[0] ?? ""), {
    type: "join",
    room: "notes",
    client_id: 1,
    name: "pranav",
    read_only: true,
  });
  socket.receive(
    '{"type":"joined","room":"notes","text":"hello","read_only":true,"version":1,"presences":[]}',
  );
  assert.equal(client.readOnly, true);
  // A viewer cannot type or format: nothing is echoed or queued.
  client.localEdit("hello world");
  client.formatRange(0, 5, { bold: true });
  assert.equal(client.editor.text, "hello");
  assert.equal(client.pendingCount, 0);
  assert.equal(socket.sent.length, 1);
});

test("list before join: the list goes out, then the join on the same socket", () => {
  const sockets: FakeSocket[] = [];
  const client = new CollabClient(
    1,
    () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
    { reconnectDelay: () => 0 },
  );
  const kinds: string[] = [];
  client.onChange = (change) => kinds.push(change.kind);
  client.connectForList("ws://localhost:8080");
  const socket = sockets[0];
  assert.ok(socket);
  socket.open();
  assert.equal(socket.sent[0], '{"type":"list"}');
  socket.receive(
    '{"type":"docs","docs":[{"id":"notes","title":"Shopping list","preview":"milk","version":2,"updated_at":1793000000}]}',
  );
  assert.equal(client.docs.length, 1);
  assert.equal(client.docs[0]?.title, "Shopping list");
  assert.deepEqual(kinds, ["docs"]);
  // Picking a document joins it on the same socket.
  client.join("notes", "pranav");
  assert.equal(
    socket.sent[1],
    encodeClient({ type: "join", room: "notes", client_id: 1, name: "pranav" }),
  );
  socket.receive('{"type":"joined","room":"notes","text":"milk","version":2,"presences":[]}');
  assert.equal(client.status, "connected");
  assert.equal(client.readOnly, false);
});
