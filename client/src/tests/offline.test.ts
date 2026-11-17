import test from "node:test";
import assert from "node:assert/strict";
import {
  OfflineOutbox,
  memoryStore,
  type OfflineStore,
  type SavedState,
} from "../offline.js";
import { insertOp } from "../protocol.js";

function state(room = "notes"): SavedState {
  return {
    room,
    text: "hello world",
    attrs: [],
    version: 3,
    nextSeq: 4,
    pending: [
      { seq: 2, op: insertOp(1, 5, " world"), baseVersion: 1, echoed: true },
      { seq: 3, op: insertOp(1, 11, "!"), baseVersion: 2, echoed: false },
    ],
  };
}

test("save and load round-trips the text, queue, and next sequence number", () => {
  const store = memoryStore();
  const outbox = new OfflineOutbox(store);
  outbox.save(state());
  const loaded = outbox.load("notes");
  assert.deepEqual(loaded, state());
});

test("load returns null when nothing was saved", () => {
  const outbox = new OfflineOutbox(memoryStore());
  assert.equal(outbox.load("notes"), null);
});

test("the box is per room: another room loads nothing", () => {
  const outbox = new OfflineOutbox(memoryStore());
  outbox.save(state("notes"));
  assert.equal(outbox.load("other"), null);
  assert.notEqual(outbox.load("notes"), null);
});

test("clear removes the saved state", () => {
  const store = memoryStore();
  const outbox = new OfflineOutbox(store);
  outbox.save(state());
  outbox.clear("notes");
  assert.equal(outbox.load("notes"), null);
  assert.equal(store.get(OfflineOutbox.keyFor("notes")), null);
});

test("a corrupt box loads as nothing and is cleared", () => {
  const store = memoryStore();
  store.set(OfflineOutbox.keyFor("notes"), "{not json");
  const outbox = new OfflineOutbox(store);
  assert.equal(outbox.load("notes"), null);
  assert.equal(store.get(OfflineOutbox.keyFor("notes")), null);
});

test("a box saved for another room loads as nothing", () => {
  const store = memoryStore();
  const saved = state("other");
  store.set(OfflineOutbox.keyFor("notes"), JSON.stringify(saved));
  const outbox = new OfflineOutbox(store);
  assert.equal(outbox.load("notes"), null);
});

test("a box with a malformed op loads as nothing", () => {
  const store = memoryStore();
  const bad = { ...state(), pending: [{ seq: 1, op: { Insert: { site: 1 } }, baseVersion: 0, echoed: true }] };
  store.set(OfflineOutbox.keyFor("notes"), JSON.stringify(bad));
  const outbox = new OfflineOutbox(store);
  assert.equal(outbox.load("notes"), null);
});

test("a store that throws never breaks save, load, or clear", () => {
  const throwing: OfflineStore = {
    get: () => {
      throw new Error("boom");
    },
    set: () => {
      throw new Error("boom");
    },
    remove: () => {
      throw new Error("boom");
    },
  };
  const outbox = new OfflineOutbox(throwing);
  outbox.save(state());
  assert.equal(outbox.load("notes"), null);
  outbox.clear("notes");
});

test("the key encodes the room id instead of trusting it", () => {
  assert.equal(OfflineOutbox.keyFor("a/b"), "collab-offline:a%2Fb");
  assert.notEqual(OfflineOutbox.keyFor("a/b"), OfflineOutbox.keyFor("a_b"));
});
