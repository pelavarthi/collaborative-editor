// The local copy of the document and the edits made to it.
//
// Positions count characters, like the Rust side does: one emoji is one
// position. JavaScript strings count UTF-16 code units instead, so every
// position here is measured on the spread [...text], not on .length.
//
// Day 9 adds formatting: the state keeps one set of attributes per
// character next to the text, so a bold word stays bold when the text
// around it changes. A format op rewrites the attributes of a range
// without touching the text, and typing inside a formatted run inherits
// its formatting, the way typing at the end of a bold word stays bold.
//
// Local echo: a keystroke is applied to this state the moment it happens,
// so typing never waits on the network, and the op it produces is what
// gets sent to the server. A remote op arrives already transformed by the
// server past anything this client sent but has not had acked yet, so it
// can be applied to the local text as-is.

import {
  applyChange,
  deleteOp,
  formatOp,
  insertOp,
  isEmptyChange,
  isNoop,
  isPlainAttrs,
  plainAttrs,
  type Attrs,
  type FormatChange,
  type Op,
} from "./protocol.js";

/** Splits a string into its characters (code points). */
export function chars(text: string): string[] {
  return [...text];
}

/** A document with its per-character formatting. */
export interface RichDoc {
  text: string;
  attrs: Attrs[];
}

/**
 * Applies one op to a text and its attributes, returning both. The
 * attributes stay lined up with the characters: an insert splices its
 * own attributes in, a delete takes both out, and a format rewrites
 * the attributes of its range. A short attribute list is padded with
 * plain attributes, so a text-only caller can pass an empty list.
 */
export function applyOpWithAttrs(text: string, attrs: Attrs[], op: Op): RichDoc {
  const cs = chars(text);
  const nextAttrs = attrs.slice();
  while (nextAttrs.length < cs.length) nextAttrs.push(plainAttrs());
  if ("Insert" in op) {
    const { pos, text: inserted } = op.Insert;
    if (pos < 0 || pos > cs.length) throw new Error(`insert at ${pos} past end (${cs.length})`);
    const insertedAttrs = op.Insert.attrs ?? plainAttrs();
    cs.splice(pos, 0, ...chars(inserted));
    nextAttrs.splice(
      pos,
      0,
      ...chars(inserted).map(() => ({ ...insertedAttrs })),
    );
  } else if ("Delete" in op) {
    const { pos, len } = op.Delete;
    if (pos < 0 || pos + len > cs.length) {
      throw new Error(`delete of ${len} at ${pos} past end (${cs.length})`);
    }
    cs.splice(pos, len);
    nextAttrs.splice(pos, len);
  } else {
    const { pos, len, change } = op.Format;
    if (pos < 0 || pos + len > cs.length) {
      throw new Error(`format of ${len} at ${pos} past end (${cs.length})`);
    }
    for (let i = pos; i < pos + len; i++) {
      nextAttrs[i] = applyChange(nextAttrs[i] ?? plainAttrs(), change);
    }
  }
  return { text: cs.join(""), attrs: nextAttrs };
}

/** Applies one op to a string, returning the new string. Formatting
 * does not change the text, so a format op leaves it alone. */
export function applyOp(text: string, op: Op): string {
  return applyOpWithAttrs(text, chars(text).map(() => plainAttrs()), op).text;
}

/** The text as runs of equally formatted characters, for rendering. */
export function spansOf(text: string, attrs: Attrs[]): { text: string; attrs: Attrs }[] {
  const spans: { text: string; attrs: Attrs }[] = [];
  chars(text).forEach((ch, i) => {
    const at = attrs[i] ?? plainAttrs();
    const last = spans[spans.length - 1];
    if (
      last !== undefined &&
      last.attrs.bold === at.bold &&
      last.attrs.italic === at.italic &&
      last.attrs.link === at.link
    ) {
      last.text += ch;
    } else {
      spans.push({ text: ch, attrs: { ...at } });
    }
  });
  return spans;
}

/**
 * Where a position ends up after `op` is applied. An insert at or before
 * the position pushes it right; a delete before it pulls it left, and a
 * position inside the deleted range lands at the start of that range.
 * A format moves no text, so it moves no position either.
 * Mirrors shift_pos in the server's presence.rs.
 */
export function shiftPosition(pos: number, op: Op): number {
  if ("Insert" in op) {
    return op.Insert.pos <= pos ? pos + chars(op.Insert.text).length : pos;
  }
  if ("Delete" in op) {
    const { pos: at, len } = op.Delete;
    if (pos <= at) return pos;
    if (pos <= at + len) return at;
    return pos - len;
  }
  return pos;
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
  /** The formatting of each character, in order. */
  attrs: Attrs[] = [];
  /** The server version this state has seen. */
  version = 0;

  constructor(site: number) {
    this.site = site;
  }

  /** The text as runs of equally formatted characters, for rendering. */
  spans(): { text: string; attrs: Attrs }[] {
    return spansOf(this.text, this.attrs);
  }

  /** Applies a local edit, echoing it at once, and returns the ops to send. */
  localEdit(newText: string): Op[] {
    const ops = diffToOps(this.text, newText, this.site);
    for (const op of ops) {
      if ("Insert" in op) {
        // Typing inherits the formatting at the insertion point: the
        // character before it, or the first character when typing at
        // the start. Plain text stays plain, so no attrs field is sent.
        const inherited = this.attrsAt(op.Insert.pos);
        if (!isPlainAttrs(inherited)) op.Insert.attrs = { ...inherited };
      }
      const next = applyOpWithAttrs(this.text, this.attrs, op);
      this.text = next.text;
      this.attrs = next.attrs;
    }
    return ops.filter((op) => !isNoop(op));
  }

  /**
   * Formats a range locally at once, and returns the op to send, or
   * null when there is nothing to format. The change is applied to
   * the range as it stands now; the server transforms it past
   * anything this client had not seen yet.
   */
  formatRange(pos: number, len: number, change: FormatChange): Op | null {
    if (len <= 0 || isEmptyChange(change)) return null;
    const op = formatOp(this.site, pos, len, change);
    const next = applyOpWithAttrs(this.text, this.attrs, op);
    this.text = next.text;
    this.attrs = next.attrs;
    return op;
  }

  /**
   * Applies an op that came from the server. The version only ever moves
   * forward: an ack for an op sent earlier can arrive after a newer
   * broadcast already moved it on.
   */
  applyRemote(op: Op, version: number): void {
    const next = applyOpWithAttrs(this.text, this.attrs, op);
    this.text = next.text;
    this.attrs = next.attrs;
    this.version = Math.max(this.version, version);
  }

  /** Replaces the whole text, e.g. on joining or on a snapshot. */
  replace(text: string, version: number, attrs?: Attrs[]): void {
    this.text = text;
    this.attrs =
      attrs !== undefined && attrs.length === chars(text).length
        ? attrs.map((a) => ({ ...a }))
        : chars(text).map(() => plainAttrs());
    this.version = Math.max(this.version, version);
  }

  /** The formatting at an insertion point: the character before it,
   * or the first character when inserting at the start. */
  private attrsAt(pos: number): Attrs {
    if (pos > 0) return this.attrs[pos - 1] ?? plainAttrs();
    return this.attrs[0] ?? plainAttrs();
  }
}
