// The JSON messages the TypeScript client and the Rust server send each
// other over the WebSocket. These shapes must match crates/server/src/
// message.rs exactly: ops are serde's externally-tagged enums
// ({"Insert": {...}}), messages are internally tagged by "type".

export interface InsertOp {
  Insert: { site: number; pos: number; text: string };
}

export interface DeleteOp {
  Delete: { site: number; pos: number; len: number };
}

export type Op = InsertOp | DeleteOp;

export function insertOp(site: number, pos: number, text: string): Op {
  return { Insert: { site, pos, text } };
}

export function deleteOp(site: number, pos: number, len: number): Op {
  return { Delete: { site, pos, len } };
}

export function opSite(op: Op): number {
  return "Insert" in op ? op.Insert.site : op.Delete.site;
}

/** An op that changes nothing: an empty insert or a zero-length delete. */
export function isNoop(op: Op): boolean {
  return "Insert" in op ? op.Insert.text === "" : op.Delete.len === 0;
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

export type ClientMessage =
  | { type: "join"; room: string; client_id: number; name?: string }
  | { type: "op"; base_version: number; op: Op }
  | { type: "sync"; base_version: number }
  | { type: "presence"; cursor: number; selection: Selection | null };

export type ServerMessage =
  | { type: "joined"; room: string; text: string; version: number; presences: Presence[] }
  | { type: "op"; version: number; op: Op }
  | { type: "ack"; version: number }
  | { type: "ops"; from_version: number; ops: Op[] }
  | { type: "snapshot"; text: string; version: number }
  | { type: "presence"; presence: Presence }
  | { type: "presence_left"; client_id: number }
  | { type: "error"; message: string };

export function encodeClient(msg: ClientMessage): string {
  return JSON.stringify(msg);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isOp(value: unknown): value is Op {
  if (!isObject(value)) return false;
  if ("Insert" in value && isObject(value.Insert)) {
    const ins = value.Insert;
    return (
      typeof ins.site === "number" &&
      typeof ins.pos === "number" &&
      typeof ins.text === "string"
    );
  }
  if ("Delete" in value && isObject(value.Delete)) {
    const del = value.Delete;
    return (
      typeof del.site === "number" && typeof del.pos === "number" && typeof del.len === "number"
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
        typeof value.version === "number" &&
        Array.isArray(value.presences) &&
        value.presences.every(isPresence)
      ) {
        return value as ServerMessage;
      }
      break;
    case "op":
      if (typeof value.version === "number" && isOp(value.op)) {
        return value as ServerMessage;
      }
      break;
    case "ack":
      if (typeof value.version === "number") return value as ServerMessage;
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
      if (typeof value.text === "string" && typeof value.version === "number") {
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
