import test from "node:test";
import assert from "node:assert/strict";
import { CollabClient, type Socket } from "../client.js";
import { memoryStore, OfflineOutbox, type OfflineStore } from "../offline.js";
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

/** Offline-mode tests: a client with a shared outbox store and a
 * switchable network, so a drop can mean "the browser is offline". */
function connectOffline(
  store: OfflineStore,
  online: { value: boolean },
  room = "notes",
): { client: CollabClient; sockets: FakeSocket[]; socket: FakeSocket } {
  const sockets: FakeSocket[] = [];
  const client = new CollabClient(
    1,
    () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
    {
      reconnectDelay: () => 0,
      isOnline: () => online.value,
      outbox: new OfflineOutbox(store),
    },
  );
  client.connect("ws://localhost:8080", room, "pranav");
  const first = sockets[0];
  assert.ok(first);
  first.open();
  return { client, sockets, socket: first };
}

test("dropping the socket while the browser is offline reports offline, not reconnecting", async () => {
  const online = { value: false };
  const { client, sockets } = connectOffline(memoryStore(), online);
  sockets[0]?.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  assert.equal(client.status, "connected");
  client.dropConnection();
  assert.equal(client.status, "offline");
  // No reconnect is scheduled while offline: the browser has to say
  // the network is back first.
  await tick();
  assert.equal(sockets.length, 1);
  // The network comes back: the client reconnects right away instead
  // of waiting out a backoff timer.
  online.value = true;
  client.networkChanged();
  await tick();
  assert.equal(sockets.length, 2);
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

test("offline edits survive a reload in the same room", async () => {
  const store = memoryStore();
  const online = { value: true };
  const first = connectOffline(store, online);
  first.socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  first.client.localEdit("hello");
  first.socket.receive(
    '{"type":"ack","version":1,"seq":1,"op":{"Insert":{"site":1,"pos":0,"text":"hello"}}}',
  );
  assert.equal(first.client.pendingCount, 0);

  // The socket drops; the client keeps typing while it is down.
  first.client.dropConnection();
  await tick();
  first.client.localEdit("hello world");
  assert.equal(first.client.pendingCount, 1);
  // The outbox holds the text with the offline edit and the queue.
  assert.notEqual(store.get(OfflineOutbox.keyFor("notes")), null);

  // A fresh client — a reload — on the same store picks it back up
  // before it even opens the socket.
  const second = connectOffline(store, online);
  assert.equal(second.client.editor.text, "hello world");
  assert.equal(second.client.pendingCount, 1);
  assert.equal(second.client.editor.version, 1);

  // The rejoin hands back the server text, without the offline edit.
  second.socket.receive('{"type":"joined","room":"notes","text":"hello","version":1,"presences":[]}');
  assert.equal(second.client.editor.text, "hello");
  // The queued edit goes out again with its original number.
  assert.equal(second.socket.sent.length, 2);
  assert.equal(
    second.socket.sent[1],
    encodeClient({ type: "op", base_version: 1, seq: 2, op: insertOp(1, 5, " world") }),
  );
  second.socket.receive(
    '{"type":"ack","version":2,"seq":2,"op":{"Insert":{"site":1,"pos":5,"text":" world"}}}',
  );
  assert.equal(second.client.editor.text, "hello world");
  assert.equal(second.client.pendingCount, 0);
  // Everything is acked, so the outbox is cleared.
  assert.equal(store.get(OfflineOutbox.keyFor("notes")), null);
});

test("a reload in another room does not restore the saved edits", () => {
  const store = memoryStore();
  const online = { value: true };
  const first = connectOffline(store, online, "notes");
  first.socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  first.client.localEdit("hello");
  assert.equal(first.client.pendingCount, 1);

  const second = connectOffline(store, online, "other");
  assert.equal(second.client.editor.text, "");
  assert.equal(second.client.pendingCount, 0);
});

test("an ack that nulls an echoed edit resyncs instead of leaving a phantom edit", async () => {
  const online = { value: true };
  const { client, sockets } = connectOffline(memoryStore(), online);
  const first = sockets[0];
  assert.ok(first);
  first.receive('{"type":"joined","room":"notes","text":"abcdef","version":1,"presences":[]}');
  // Typing inside a range another client deletes at the same time:
  // the server drops the edit, and the echo is still in the text.
  client.localEdit("abcXdef");
  assert.equal(client.editor.text, "abcXdef");
  const errors: string[] = [];
  client.onError = (message) => errors.push(message);
  first.receive('{"type":"ack","version":1,"seq":1,"op":null}');
  assert.equal(client.pendingCount, 0);
  assert.deepEqual(errors, ["an edit conflicted and was dropped; resyncing"]);
  // The client drops and rejoins on its own for a fresh snapshot.
  assert.equal(first.closed, true);
  await tick();
  assert.equal(sockets.length, 2);
  const second = sockets[1];
  assert.ok(second);
  second.open();
  second.receive('{"type":"joined","room":"notes","text":"abcdef","version":1,"presences":[]}');
  assert.equal(client.status, "connected");
  assert.equal(client.editor.text, "abcdef");
});

test("a stale base version error drops the offline edits and rejoins", async () => {
  const online = { value: true };
  const { client, sockets } = connectOffline(memoryStore(), online);
  const first = sockets[0];
  assert.ok(first);
  first.receive('{"type":"joined","room":"notes","text":"hello","version":1,"presences":[]}');
  client.localEdit("hello!");
  first.receive(
    '{"type":"ack","version":2,"seq":1,"op":{"Insert":{"site":1,"pos":5,"text":"!"}}}',
  );
  assert.equal(client.pendingCount, 0);

  // The socket drops; the client types while down, then rejoins to a
  // server that has compacted past the version its edit was made on.
  client.dropConnection();
  await tick();
  const second = sockets[1];
  assert.ok(second);
  client.localEdit("hello!!");
  second.open();
  second.receive(
    '{"type":"joined","room":"notes","text":"hello world","version":5,"presences":[]}',
  );
  assert.equal(second.sent.length, 2);
  const errors: string[] = [];
  client.onError = (message) => errors.push(message);
  second.receive('{"type":"error","message":"base version 2 is older than the snapshot at version 5"}');
  // The unreplayable edit is dropped, not resent, and the client
  // rejoins for a fresh copy.
  assert.equal(client.pendingCount, 0);
  assert.deepEqual(errors, [
    "edits made while offline could not be replayed; resynced from the server",
  ]);
  assert.equal(second.closed, true);
  await tick();
  assert.equal(sockets.length, 3);
  const third = sockets[2];
  assert.ok(third);
  third.open();
  third.receive(
    '{"type":"joined","room":"notes","text":"hello world","version":5,"presences":[]}',
  );
  assert.equal(client.status, "connected");
  assert.equal(client.editor.text, "hello world");
  assert.equal(client.pendingCount, 0);
});

test("a snapshot replaces the text and resends the queued edits", () => {
  const online = { value: true };
  const { client, socket } = connectOffline(memoryStore(), online);
  socket.receive('{"type":"joined","room":"notes","text":"hello","version":1,"presences":[]}');
  client.localEdit("hello!");
  assert.equal(client.pendingCount, 1);
  // A sync reply hands back the whole document instead of the missed
  // ops: the echo is gone with the old text, so the edit is resent
  // and the ack puts it back as the server applied it.
  socket.receive('{"type":"snapshot","text":"hello world","version":5}');
  assert.equal(client.editor.text, "hello world");
  assert.equal(socket.sent.length, 3);
  assert.equal(
    socket.sent[2],
    encodeClient({ type: "op", base_version: 1, seq: 1, op: insertOp(1, 5, "!") }),
  );
  socket.receive(
    '{"type":"ack","version":6,"seq":1,"op":{"Insert":{"site":1,"pos":11,"text":"!"}}}',
  );
  assert.equal(client.editor.text, "hello world!");
  assert.equal(client.pendingCount, 0);
});
