import test from "node:test";
import assert from "node:assert/strict";
import {
  decodeServer,
  deleteOp,
  encodeClient,
  formatOp,
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
  // "type" tag, so the client must send exactly this. `seq` counts the
  // client's ops from 1 up, so a resend after a lost ack is recognizable.
  assert.equal(
    encodeClient({ type: "op", base_version: 3, seq: 2, op: insertOp(1, 0, "hi") }),
    '{"type":"op","base_version":3,"seq":2,"op":{"Insert":{"site":1,"pos":0,"text":"hi"}}}',
  );
  assert.equal(
    encodeClient({ type: "op", base_version: 3, seq: 2, op: deleteOp(1, 2, 4) }),
    '{"type":"op","base_version":3,"seq":2,"op":{"Delete":{"site":1,"pos":2,"len":4}}}',
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

  assert.deepEqual(
    decodeServer(
      '{"type":"ack","version":4,"seq":2,"op":{"Insert":{"site":1,"pos":0,"text":"hi"}}}',
    ),
    { type: "ack", version: 4, seq: 2, op: insertOp(1, 0, "hi") },
  );
  // An ack for an op that transformed into a no-op carries a null op.
  assert.deepEqual(decodeServer('{"type":"ack","version":4,"seq":2,"op":null}'), {
    type: "ack",
    version: 4,
    seq: 2,
    op: null,
  });
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
  assert.throws(() => decodeServer('{"type":"ack","version":1}'), /not a server message/);
});

test("op helpers", () => {
  assert.equal(opSite(insertOp(3, 0, "x")), 3);
  assert.equal(opSite(deleteOp(5, 0, 1)), 5);
  assert.equal(isNoop(insertOp(1, 0, "")), true);
  assert.equal(isNoop(deleteOp(1, 0, 0)), true);
  assert.equal(isNoop(insertOp(1, 0, "x")), false);
});

test("format op encodes with the Rust externally-tagged shape", () => {
  assert.equal(
    encodeClient({ type: "op", base_version: 3, seq: 2, op: formatOp(1, 0, 5, { bold: true }) }),
    '{"type":"op","base_version":3,"seq":2,"op":{"Format":{"site":1,"pos":0,"len":5,"change":{"bold":true}}}}',
  );
  // Removing a link sends an explicit null; leaving it alone omits it.
  assert.equal(
    encodeClient({ type: "op", base_version: 3, seq: 2, op: formatOp(1, 0, 5, { link: null }) }),
    '{"type":"op","base_version":3,"seq":2,"op":{"Format":{"site":1,"pos":0,"len":5,"change":{"link":null}}}}',
  );
  assert.equal(
    encodeClient({ type: "op", base_version: 3, seq: 2, op: formatOp(1, 0, 5, { italic: true }) }),
    '{"type":"op","base_version":3,"seq":2,"op":{"Format":{"site":1,"pos":0,"len":5,"change":{"italic":true}}}}',
  );
});

test("a styled insert carries its attributes, a plain one does not", () => {
  assert.equal(
    encodeClient({
      type: "op",
      base_version: 0,
      seq: 1,
      op: insertOp(1, 0, "hi", { bold: true, italic: false, link: null }),
    }),
    '{"type":"op","base_version":0,"seq":1,"op":{"Insert":{"site":1,"pos":0,"text":"hi","attrs":{"bold":true,"italic":false,"link":null}}}}',
  );
  assert.deepEqual(
    decodeServer(
      '{"type":"op","version":2,"op":{"Format":{"site":2,"pos":0,"len":5,"change":{"bold":true}}}}',
    ),
    { type: "op", version: 2, op: formatOp(2, 0, 5, { bold: true }) },
  );
  // An insert without attrs reads as plain text.
  assert.deepEqual(
    decodeServer('{"type":"op","version":2,"op":{"Insert":{"site":2,"pos":0,"text":"x"}}}'),
    { type: "op", version: 2, op: insertOp(2, 0, "x") },
  );
});

test("decode joined and snapshot carry the formatting", () => {
  const joined = decodeServer(
    '{"type":"joined","room":"notes","text":"hi","attrs":[{"bold":true,"italic":false,"link":null},{"bold":false,"italic":false,"link":null}],"version":3,"presences":[]}',
  );
  assert.equal(joined.type, "joined");
  if (joined.type === "joined") {
    assert.deepEqual(joined.attrs, [
      { bold: true, italic: false, link: null },
      { bold: false, italic: false, link: null },
    ]);
  }
  const snapshot = decodeServer(
    '{"type":"snapshot","text":"hi","attrs":[{"bold":false,"italic":true,"link":null},{"bold":false,"italic":false,"link":null}],"version":9}',
  );
  assert.equal(snapshot.type, "snapshot");
  if (snapshot.type === "snapshot") {
    assert.equal(snapshot.attrs?.[0]?.italic, true);
  }
  // Messages without attrs still decode: an old server sends none.
  assert.deepEqual(decodeServer('{"type":"snapshot","text":"hi","version":9}'), {
    type: "snapshot",
    text: "hi",
    version: 9,
  });
});

test("decode rejects a malformed format op", () => {
  assert.throws(
    () => decodeServer('{"type":"op","version":1,"op":{"Format":{"site":1,"pos":0,"len":5,"change":{"bold":"yes"}}}}'),
    /not a server message/,
  );
  assert.throws(
    () => decodeServer('{"type":"op","version":1,"op":{"Format":{"site":1,"pos":0,"len":5}}}'),
    /not a server message/,
  );
});

test("op helpers cover formats", () => {
  assert.equal(opSite(formatOp(4, 0, 2, { bold: true })), 4);
  assert.equal(isNoop(formatOp(1, 0, 0, { bold: true })), true);
  assert.equal(isNoop(formatOp(1, 0, 2, { bold: true })), false);
});

test("decode docs list and joined with read_only", () => {
  assert.deepEqual(
    decodeServer(
      '{"type":"docs","docs":[{"id":"notes","title":"Shopping list","preview":"Shopping list milk","version":3,"updated_at":1793000000},{"id":"empty","title":"empty","preview":"","version":0,"updated_at":null}]}',
    ),
    {
      type: "docs",
      docs: [
        {
          id: "notes",
          title: "Shopping list",
          preview: "Shopping list milk",
          version: 3,
          updated_at: 1793000000,
        },
        { id: "empty", title: "empty", preview: "", version: 0, updated_at: null },
      ],
    },
  );

  // A read-only join says so; a server that omits the flag still
  // decodes, as an editor.
  const joined = decodeServer(
    '{"type":"joined","room":"notes","text":"hi","read_only":true,"version":1,"presences":[]}',
  );
  assert.equal(joined.type, "joined");
  if (joined.type === "joined") assert.equal(joined.read_only, true);
  const old = decodeServer(
    '{"type":"joined","room":"notes","text":"hi","version":1,"presences":[]}',
  );
  assert.equal(old.type, "joined");
  if (old.type === "joined") assert.equal(old.read_only, undefined);
});

test("decode rejects a docs row with a bad shape", () => {
  assert.throws(
    () => decodeServer('{"type":"docs","docs":[{"id":"notes","title":"x"}]}'),
    /not a server message/,
  );
});
