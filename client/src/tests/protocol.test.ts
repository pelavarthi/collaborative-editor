import test from "node:test";
import assert from "node:assert/strict";
import {
  decodeServer,
  deleteOp,
  encodeClient,
  insertOp,
  isNoop,
  opSite,
} from "../protocol.js";

test("join encodes with the snake_case type tag", () => {
  const json = encodeClient({ type: "join", room: "notes", client_id: 7, name: "pranav" });
  assert.equal(json, '{"type":"join","room":"notes","client_id":7,"name":"pranav"}');
});

test("join without a name omits the field", () => {
  const json = encodeClient({ type: "join", room: "notes", client_id: 7 });
  assert.equal(json, '{"type":"join","room":"notes","client_id":7}');
});

test("op encodes with the Rust externally-tagged shape", () => {
  // Serde serializes the Rust Op enum as {"Insert": {...}}, not with a
  // "type" tag, so the client must send exactly this.
  assert.equal(
    encodeClient({ type: "op", base_version: 3, op: insertOp(1, 0, "hi") }),
    '{"type":"op","base_version":3,"op":{"Insert":{"site":1,"pos":0,"text":"hi"}}}',
  );
  assert.equal(
    encodeClient({ type: "op", base_version: 3, op: deleteOp(1, 2, 4) }),
    '{"type":"op","base_version":3,"op":{"Delete":{"site":1,"pos":2,"len":4}}}',
  );
});

test("presence encodes with a null selection when there is none", () => {
  assert.equal(
    encodeClient({ type: "presence", cursor: 5, selection: null }),
    '{"type":"presence","cursor":5,"selection":null}',
  );
});

test("decode joined, ack, and op messages", () => {
  const joined = decodeServer(
    '{"type":"joined","room":"notes","text":"hello","version":1,"presences":[{"client_id":2,"name":"hriday","color":"#f76b15","cursor":5,"selection":null}]}',
  );
  assert.equal(joined.type, "joined");
  if (joined.type === "joined") {
    assert.equal(joined.text, "hello");
    assert.equal(joined.presences[0]?.name, "hriday");
  }

  assert.deepEqual(decodeServer('{"type":"ack","version":4}'), { type: "ack", version: 4 });
  assert.deepEqual(decodeServer('{"type":"op","version":2,"op":{"Insert":{"site":2,"pos":0,"text":"x"}}}'), {
    type: "op",
    version: 2,
    op: insertOp(2, 0, "x"),
  });
});

test("decode presence, presence_left, ops, snapshot, and error", () => {
  assert.deepEqual(
    decodeServer(
      '{"type":"presence","presence":{"client_id":2,"name":null,"color":"#46a758","cursor":3,"selection":{"anchor":1,"head":3}}}',
    ),
    {
      type: "presence",
      presence: {
        client_id: 2,
        name: null,
        color: "#46a758",
        cursor: 3,
        selection: { anchor: 1, head: 3 },
      },
    },
  );
  assert.deepEqual(decodeServer('{"type":"presence_left","client_id":2}'), {
    type: "presence_left",
    client_id: 2,
  });
  assert.deepEqual(decodeServer('{"type":"ops","from_version":1,"ops":[]}'), {
    type: "ops",
    from_version: 1,
    ops: [],
  });
  assert.deepEqual(decodeServer('{"type":"snapshot","text":"hi","version":9}'), {
    type: "snapshot",
    text: "hi",
    version: 9,
  });
  assert.deepEqual(decodeServer('{"type":"error","message":"bad message"}'), {
    type: "error",
    message: "bad message",
  });
});

test("decode rejects garbage and unknown types", () => {
  assert.throws(() => decodeServer("not json"), /not JSON/);
  assert.throws(() => decodeServer('{"type":"nope"}'), /not a server message/);
  assert.throws(() => decodeServer('{"type":"op","version":1}'), /not a server message/);
});

test("op helpers", () => {
  assert.equal(opSite(insertOp(3, 0, "x")), 3);
  assert.equal(opSite(deleteOp(5, 0, 1)), 5);
  assert.equal(isNoop(insertOp(1, 0, "")), true);
  assert.equal(isNoop(deleteOp(1, 0, 0)), true);
  assert.equal(isNoop(insertOp(1, 0, "x")), false);
});
