// The JSON messages the TypeScript client and the Rust server send each
// other over the WebSocket. These shapes must match crates/server/src/
// message.rs exactly: ops are serde's externally-tagged enums
// ({"Insert": {...}}), messages are internally tagged by "type".
//
// Day 9 adds formatting: an insert carries the attributes of the text
// it adds (omitted when the text is plain, so ordinary typing keeps
// the old JSON shape), and a format op changes the attributes of a
// range that is already there.

/** The formatting of one character: bold, italic, an optional link. */
export interface Attrs {
  bold: boolean;
  italic: boolean;
  link: string | null;
}

export function plainAttrs(): Attrs {
  return { bold: false, italic: false, link: null };
}

export function isPlainAttrs(attrs: Attrs): boolean {
  return !attrs.bold && !attrs.italic && attrs.link === null;
}

/**
 * A change to apply to the formatting of a range. A missing field
 * leaves that attribute alone; `link: null` removes the link, a
 * string sets it. That is why `link` is checked with `"link" in
 * change` instead of a plain null check.
 */
export interface FormatChange {
  bold?: boolean;
  italic?: boolean;
  link?: string | null;
}

/** The attributes a character ends up with after `change`. */
export function applyChange(attrs: Attrs, change: FormatChange): Attrs {
  return {
    bold: change.bold ?? attrs.bold,
    italic: change.italic ?? attrs.italic,
    link: "link" in change ? (change.link ?? null) : attrs.link,
  };
}

/** True when the change touches no attribute at all. */
export function isEmptyChange(change: FormatChange): boolean {
  return change.bold === undefined && change.italic === undefined && !("link" in change);
}

export interface InsertOp {
  Insert: { site: number; pos: number; text: string; attrs?: Attrs };
}

export interface DeleteOp {
  Delete: { site: number; pos: number; len: number };
}

export interface FormatOp {
  Format: { site: number; pos: number; len: number; change: FormatChange };
}

export type Op = InsertOp | DeleteOp | FormatOp;

export function insertOp(site: number, pos: number, text: string, attrs?: Attrs): Op {
  // Plain text keeps the old shape with no attrs field; only styled
  // inserts carry one, so old clients and logs still read.
  if (attrs === undefined || isPlainAttrs(attrs)) return { Insert: { site, pos, text } };
  return { Insert: { site, pos, text, attrs } };
}

export function deleteOp(site: number, pos: number, len: number): Op {
  return { Delete: { site, pos, len } };
}

export function formatOp(site: number, pos: number, len: number, change: FormatChange): Op {
  return { Format: { site, pos, len, change } };
}

export function opSite(op: Op): number {
  if ("Insert" in op) return op.Insert.site;
  if ("Delete" in op) return op.Delete.site;
  return op.Format.site;
}

/** An op that changes nothing: an empty insert, a zero-length delete
 * or format. */
export function isNoop(op: Op): boolean {
  if ("Insert" in op) return op.Insert.text === "";
  if ("Delete" in op) return op.Delete.len === 0;
  return op.Format.len === 0;
}

export interface Selection {
  anchor: number;
  head: number;
}

export interface Presence {
  client_id: number;
  name: string | null;
  color: string;
  cursor: number;
  selection: Selection | null;
}

/** One row of the documents list. `updated_at` is seconds since the
 * Unix epoch when the room was last saved, or null when it never was. */
export interface DocInfo {
  id: string;
  title: string;
  preview: string;
  version: number;
  updated_at: number | null;
}

export type ClientMessage =
  | { type: "join"; room: string; client_id: number; name?: string; read_only?: boolean }
  | { type: "list" }
  | { type: "op"; base_version: number; seq: number; op: Op }
  | { type: "sync"; base_version: number }
  | { type: "presence"; cursor: number; selection: Selection | null };

export type ServerMessage =
  | {
      type: "joined";
      room: string;
      text: string;
      /** The formatting of each character, in order; absent on old messages. */
      attrs?: Attrs[];
      /** True when this client joined as a viewer. Absent on old messages. */
      read_only?: boolean;
      version: number;
      presences: Presence[];
    }
  | { type: "docs"; docs: DocInfo[] }
  | { type: "op"; version: number; op: Op }
  // `seq` echoes the op's number, so the client knows which queued op
  // this ack is for. `op` is the op as the server applied it, or null
  // when it transformed into a no-op: the client applies it when its
  // own echo was wiped by a rejoin, and ignores it when the op is
  // already in its text.
  | { type: "ack"; version: number; seq: number; op: Op | null }
  | { type: "ops"; from_version: number; ops: Op[] }
  | { type: "snapshot"; text: string; attrs?: Attrs[]; version: number }
  | { type: "presence"; presence: Presence }
  | { type: "presence_left"; client_id: number }
  | { type: "error"; message: string };

export function encodeClient(msg: ClientMessage): string {
  return JSON.stringify(msg);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isAttrs(value: unknown): value is Attrs {
  return (
    isObject(value) &&
    typeof value.bold === "boolean" &&
    typeof value.italic === "boolean" &&
    (typeof value.link === "string" || value.link === null)
  );
}

function isFormatChange(value: unknown): value is FormatChange {
  // Rust serializes a format change as {"bold":true,"italic":null}:
  // null means that attribute is left alone, just like a missing key.
  if (!isObject(value)) return false;
  if ("bold" in value && !(typeof value.bold === "boolean" || value.bold === null)) return false;
  if ("italic" in value && !(typeof value.italic === "boolean" || value.italic === null)) {
    return false;
  }
  if ("link" in value && !(typeof value.link === "string" || value.link === null)) {
    return false;
  }
  return true;
}

function isOp(value: unknown): value is Op {
  if (!isObject(value)) return false;
  if ("Insert" in value && isObject(value.Insert)) {
    const ins = value.Insert;
    return (
      typeof ins.site === "number" &&
      typeof ins.pos === "number" &&
      typeof ins.text === "string" &&
      (ins.attrs === undefined || isAttrs(ins.attrs))
    );
  }
  if ("Delete" in value && isObject(value.Delete)) {
    const del = value.Delete;
    return (
      typeof del.site === "number" && typeof del.pos === "number" && typeof del.len === "number"
    );
  }
  if ("Format" in value && isObject(value.Format)) {
    const fmt = value.Format;
    return (
      typeof fmt.site === "number" &&
      typeof fmt.pos === "number" &&
      typeof fmt.len === "number" &&
      isFormatChange(fmt.change)
    );
  }
  return false;
}

function isPresence(value: unknown): value is Presence {
  if (!isObject(value)) return false;
  return (
    typeof value.client_id === "number" &&
    (typeof value.name === "string" || value.name === null) &&
    typeof value.color === "string" &&
    typeof value.cursor === "number" &&
    (value.selection === null || isSelection(value.selection))
  );
}

function isDocInfo(value: unknown): value is DocInfo {
  return (
    isObject(value) &&
    typeof value.id === "string" &&
    typeof value.title === "string" &&
    typeof value.preview === "string" &&
    typeof value.version === "number" &&
    (typeof value.updated_at === "number" || value.updated_at === null)
  );
}

function isSelection(value: unknown): value is Selection {
  return (
    isObject(value) && typeof value.anchor === "number" && typeof value.head === "number"
  );
}

/** Parses one server message, throwing on anything that is not one. */
export function decodeServer(text: string): ServerMessage {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error(`not JSON: ${text.slice(0, 80)}`);
  }
  if (!isObject(value) || typeof value.type !== "string") {
    throw new Error(`not a server message: ${text.slice(0, 80)}`);
  }
  switch (value.type) {
    case "joined":
      if (
        typeof value.room === "string" &&
        typeof value.text === "string" &&
        (value.attrs === undefined ||
          (Array.isArray(value.attrs) && value.attrs.every(isAttrs))) &&
        (value.read_only === undefined || typeof value.read_only === "boolean") &&
        typeof value.version === "number" &&
        Array.isArray(value.presences) &&
        value.presences.every(isPresence)
      ) {
        return value as ServerMessage;
      }
      break;
    case "docs":
      if (Array.isArray(value.docs) && value.docs.every(isDocInfo)) {
        return value as ServerMessage;
      }
      break;
    case "op":
      if (typeof value.version === "number" && isOp(value.op)) {
        return value as ServerMessage;
      }
      break;
    case "ack":
      if (
        typeof value.version === "number" &&
        typeof value.seq === "number" &&
        (value.op === null || isOp(value.op))
      ) {
        return value as ServerMessage;
      }
      break;
    case "ops":
      if (
        typeof value.from_version === "number" &&
        Array.isArray(value.ops) &&
        value.ops.every(isOp)
      ) {
        return value as ServerMessage;
      }
      break;
    case "snapshot":
      if (
        typeof value.text === "string" &&
        (value.attrs === undefined ||
          (Array.isArray(value.attrs) && value.attrs.every(isAttrs))) &&
        typeof value.version === "number"
      ) {
        return value as ServerMessage;
      }
      break;
    case "presence":
      if (isPresence(value.presence)) return value as ServerMessage;
      break;
    case "presence_left":
      if (typeof value.client_id === "number") return value as ServerMessage;
      break;
    case "error":
      if (typeof value.message === "string") return value as ServerMessage;
      break;
  }
  throw new Error(`not a server message: ${text.slice(0, 80)}`);
}
