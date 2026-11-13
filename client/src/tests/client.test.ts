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

function connect(site = 1, room = "notes"): { client: CollabClient; socket: FakeSocket } {
  let socket: FakeSocket | undefined;
  const client = new CollabClient(site, () => {
    socket = new FakeSocket();
    return socket;
  });
  client.connect("ws://localhost:8080", room, "pranav");
  assert.ok(socket);
  socket.open();
  return { client, socket };
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

test("local edit echoes at once and sends the op with the base version", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  assert.equal(client.editor.text, "hello");
  assert.equal(socket.sent.length, 2);
  assert.equal(
    socket.sent[1],
    encodeClient({ type: "op", base_version: 0, op: insertOp(1, 0, "hello") }),
  );
});

test("remote op applies and a stale ack does not rewind the version", () => {
  const { client, socket } = connect();
  socket.receive('{"type":"joined","room":"notes","text":"","version":0,"presences":[]}');
  client.localEdit("hello");
  socket.receive('{"type":"op","version":2,"op":{"Insert":{"site":2,"pos":0,"text":">> "}}}');
  assert.equal(client.editor.text, ">> hello");
  assert.equal(client.editor.version, 2);
  socket.receive('{"type":"ack","version":1}');
  assert.equal(client.editor.version, 2);
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

test("closing the socket reports disconnected", () => {
  const { client, socket } = connect();
  const kinds: string[] = [];
  client.onChange = (change) => kinds.push(change.kind);
  socket.close();
  assert.equal(client.status, "closed");
  assert.ok(kinds.includes("status"));
});
