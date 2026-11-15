import test from "node:test";
import assert from "node:assert/strict";
import {
  applyOp,
  applyOpWithAttrs,
  chars,
  diffToOps,
  EditorState,
  shiftPosition,
  spansOf,
} from "../editor.js";
import { deleteOp, formatOp, insertOp, plainAttrs } from "../protocol.js";

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

test("formatRange bolds a selection and returns the op to send", () => {
  const state = new EditorState(1);
  state.localEdit("hello world");
  const op = state.formatRange(0, 5, { bold: true });
  assert.deepEqual(op, formatOp(1, 0, 5, { bold: true }));
  assert.deepEqual(state.spans(), [
    { text: "hello", attrs: { bold: true, italic: false, link: null } },
    { text: " world", attrs: plainAttrs() },
  ]);
  // Toggling it off again restores plain text.
  state.formatRange(0, 5, { bold: false });
  assert.deepEqual(state.spans(), [{ text: "hello world", attrs: plainAttrs() }]);
});

test("formatRange of nothing returns null", () => {
  const state = new EditorState(1);
  state.localEdit("hello");
  assert.equal(state.formatRange(2, 0, { bold: true }), null);
  assert.equal(state.formatRange(0, 5, {}), null);
  assert.equal(state.text, "hello");
});

test("a link is set and removed by a format op", () => {
  const state = new EditorState(1);
  state.localEdit("click here");
  state.formatRange(0, 5, { link: "https://example.com" });
  assert.equal(state.attrs[0]?.link, "https://example.com");
  assert.equal(state.attrs[6]?.link, null);
  state.formatRange(0, 5, { link: null });
  assert.ok(state.attrs.every((a) => a.link === null));
});

test("typing inside a bold run inherits the bold", () => {
  const state = new EditorState(1);
  state.localEdit("hello");
  state.formatRange(0, 5, { bold: true });
  const ops = state.localEdit("hello!");
  assert.deepEqual(ops, [insertOp(1, 5, "!", { bold: true, italic: false, link: null })]);
  assert.ok(state.attrs[5]?.bold);
  // Typing at the start of plain text stays plain.
  const plain = new EditorState(1);
  plain.localEdit("hi");
  assert.deepEqual(plain.localEdit("!hi"), [insertOp(1, 0, "!")]);
});

test("applyOpWithAttrs formats without touching the text", () => {
  const bold = { bold: true, italic: false, link: null };
  const doc = applyOpWithAttrs("hello world", [...Array(11)].map(() => plainAttrs()), formatOp(2, 6, 5, { italic: true }));
  assert.equal(doc.text, "hello world");
  assert.deepEqual(doc.attrs.slice(0, 6), [...Array(6)].map(() => plainAttrs()));
  assert.ok(doc.attrs.slice(6).every((a) => a.italic && !a.bold));
  assert.equal(applyOp("hello", formatOp(2, 0, 5, { bold: true })), "hello");
  assert.throws(
    () => applyOpWithAttrs("hi", [plainAttrs(), plainAttrs()], formatOp(2, 1, 2, { bold: true })),
    /past end/,
  );
  assert.deepEqual(spansOf("aab", [bold, bold, plainAttrs()]), [
    { text: "aa", attrs: bold },
    { text: "b", attrs: plainAttrs() },
  ]);
});

test("replace takes the formatting with the text", () => {
  const state = new EditorState(1);
  state.localEdit("old");
  const bold = { bold: true, italic: false, link: null };
  state.replace("new text", 9, [bold, bold, bold, plainAttrs(), plainAttrs(), plainAttrs(), plainAttrs(), plainAttrs()]);
  assert.equal(state.text, "new text");
  assert.equal(state.attrs[0]?.bold, true);
  assert.equal(state.attrs[3]?.bold, false);
  // A join without formatting reads as plain text.
  state.replace("plain", 10);
  assert.ok(state.attrs.every((a) => !a.bold && !a.italic && a.link === null));
});

test("shiftPosition ignores a format op", () => {
  assert.equal(shiftPosition(4, formatOp(1, 0, 5, { bold: true })), 4);
});

test("a remote format lands on the local text", () => {
  const state = new EditorState(1);
  state.localEdit("hello");
  state.applyRemote(formatOp(2, 0, 5, { bold: true }), 2);
  assert.deepEqual(state.spans(), [
    { text: "hello", attrs: { bold: true, italic: false, link: null } },
  ]);
});
