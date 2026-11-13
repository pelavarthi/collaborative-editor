import test from "node:test";
import assert from "node:assert/strict";
import {
  applyOp,
  chars,
  diffToOps,
  EditorState,
  shiftPosition,
} from "../editor.js";
import { deleteOp, insertOp } from "../protocol.js";

test("diff of typing at the end is one insert", () => {
  assert.deepEqual(diffToOps("", "hello", 1), [insertOp(1, 0, "hello")]);
  assert.deepEqual(diffToOps("hello", "hello world", 1), [insertOp(1, 5, " world")]);
});

test("diff of deleting the middle is one delete", () => {
  assert.deepEqual(diffToOps("hello world", "helloworld", 1), [deleteOp(1, 5, 1)]);
});

test("diff of a replacement is a delete then an insert", () => {
  assert.deepEqual(diffToOps("hello", "hallo", 1), [
    deleteOp(1, 1, 1),
    insertOp(1, 1, "a"),
  ]);
});

test("diff of no change is empty", () => {
  assert.deepEqual(diffToOps("same", "same", 1), []);
});

test("positions count characters, not bytes or UTF-16 units", () => {
  // One emoji is one position, even though it is two UTF-16 units.
  assert.equal(chars("a\u{1F600}b").length, 3);
  assert.deepEqual(diffToOps("a\u{1F600}b", "a\u{1F600}Xb", 1), [insertOp(1, 2, "X")]);
  assert.deepEqual(diffToOps("a\u{1F600}b", "ab", 1), [deleteOp(1, 1, 1)]);
});

test("applyOp inserts and deletes by character position", () => {
  assert.equal(applyOp("bcd", insertOp(1, 0, "a")), "abcd");
  assert.equal(applyOp("hello world", deleteOp(1, 5, 1)), "helloworld");
  assert.equal(applyOp("a\u{1F600}b", insertOp(1, 2, "X")), "a\u{1F600}Xb");
  assert.throws(() => applyOp("hi", insertOp(1, 3, "x")), /past end/);
  assert.throws(() => applyOp("hi", deleteOp(1, 1, 2)), /past end/);
});

test("shiftPosition mirrors the server's cursor shifting", () => {
  assert.equal(shiftPosition(5, insertOp(1, 0, ">>")), 7);
  assert.equal(shiftPosition(2, insertOp(1, 5, "xx")), 2);
  assert.equal(shiftPosition(8, deleteOp(1, 0, 3)), 5);
  assert.equal(shiftPosition(4, deleteOp(1, 3, 4)), 3);
});

test("local echo: typing applies at once and returns the op to send", () => {
  const state = new EditorState(1);
  const ops = state.localEdit("hello");
  assert.deepEqual(ops, [insertOp(1, 0, "hello")]);
  assert.equal(state.text, "hello");
  // Typing more keeps echoing without waiting for an ack.
  assert.deepEqual(state.localEdit("hello!"), [insertOp(1, 5, "!")]);
  assert.equal(state.text, "hello!");
});

test("remote op applies on top of unacked local edits", () => {
  // The server transforms a remote op past our unacked op, so we apply
  // it as-is: our "hello" is already here, their ">> " lands at 0.
  const state = new EditorState(1);
  state.localEdit("hello");
  state.applyRemote(insertOp(2, 0, ">> "), 2);
  assert.equal(state.text, ">> hello");
  assert.equal(state.version, 2);
});

test("version never moves backwards on a stale ack", () => {
  const state = new EditorState(1);
  state.applyRemote(insertOp(2, 0, "x"), 2);
  state.replace(state.text, 1);
  assert.equal(state.version, 2);
});

test("replace takes a snapshot wholesale", () => {
  const state = new EditorState(1);
  state.localEdit("old");
  state.replace("brand new text", 9);
  assert.equal(state.text, "brand new text");
  assert.equal(state.version, 9);
});
