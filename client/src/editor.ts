// The local copy of the document and the edits made to it.
//
// Positions count characters, like the Rust side does: one emoji is one
// position. JavaScript strings count UTF-16 code units instead, so every
// position here is measured on the spread [...text], not on .length.
//
// Local echo: a keystroke is applied to this state the moment it happens,
// so typing never waits on the network, and the op it produces is what
// gets sent to the server. A remote op arrives already transformed by the
// server past anything this client sent but has not had acked yet, so it
// can be applied to the local text as-is.

import { deleteOp, insertOp, isNoop, type Op } from "./protocol.js";

/** Splits a string into its characters (code points). */
export function chars(text: string): string[] {
  return [...text];
}

/** Applies one op to a string, returning the new string. */
export function applyOp(text: string, op: Op): string {
  const cs = chars(text);
  if ("Insert" in op) {
    const { pos, text: inserted } = op.Insert;
    if (pos < 0 || pos > cs.length) throw new Error(`insert at ${pos} past end (${cs.length})`);
    cs.splice(pos, 0, ...chars(inserted));
  } else {
    const { pos, len } = op.Delete;
    if (pos < 0 || pos + len > cs.length) {
      throw new Error(`delete of ${len} at ${pos} past end (${cs.length})`);
    }
    cs.splice(pos, len);
  }
  return cs.join("");
}

/**
 * Where a position ends up after `op` is applied. An insert at or before
 * the position pushes it right; a delete before it pulls it left, and a
 * position inside the deleted range lands at the start of that range.
 * Mirrors shift_pos in the server's presence.rs.
 */
export function shiftPosition(pos: number, op: Op): number {
  if ("Insert" in op) {
    return op.Insert.pos <= pos ? pos + chars(op.Insert.text).length : pos;
  }
  const { pos: at, len } = op.Delete;
  if (pos <= at) return pos;
  if (pos <= at + len) return at;
  return pos - len;
}

/**
 * Turns a change of the whole text into ops: the longest common prefix
 * and suffix stay, the middle is deleted and the new middle inserted.
 * A pure replacement comes back as a delete followed by an insert.
 */
export function diffToOps(oldText: string, newText: string, site: number): Op[] {
  const oldCs = chars(oldText);
  const newCs = chars(newText);
  let prefix = 0;
  while (
    prefix < oldCs.length &&
    prefix < newCs.length &&
    oldCs[prefix] === newCs[prefix]
  ) {
    prefix++;
  }
  let suffix = 0;
  while (
    suffix < oldCs.length - prefix &&
    suffix < newCs.length - prefix &&
    oldCs[oldCs.length - 1 - suffix] === newCs[newCs.length - 1 - suffix]
  ) {
    suffix++;
  }
  const ops: Op[] = [];
  const deleted = oldCs.length - prefix - suffix;
  if (deleted > 0) ops.push(deleteOp(site, prefix, deleted));
  const inserted = newCs.slice(prefix, newCs.length - suffix).join("");
  if (inserted !== "") ops.push(insertOp(site, prefix, inserted));
  return ops;
}

export class EditorState {
  readonly site: number;
  text = "";
  /** The server version this state has seen. */
  version = 0;

  constructor(site: number) {
    this.site = site;
  }

  /** Applies a local edit, echoing it at once, and returns the ops to send. */
  localEdit(newText: string): Op[] {
    const ops = diffToOps(this.text, newText, this.site);
    for (const op of ops) this.text = applyOp(this.text, op);
    return ops.filter((op) => !isNoop(op));
  }

  /**
   * Applies an op that came from the server. The version only ever moves
   * forward: an ack for an op sent earlier can arrive after a newer
   * broadcast already moved it on.
   */
  applyRemote(op: Op, version: number): void {
    this.text = applyOp(this.text, op);
    this.version = Math.max(this.version, version);
  }

  /** Replaces the whole text, e.g. on joining or on a snapshot. */
  replace(text: string, version: number): void {
    this.text = text;
    this.version = Math.max(this.version, version);
  }
}
