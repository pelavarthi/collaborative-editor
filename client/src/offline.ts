// The offline outbox: the edits the server has not confirmed yet,
// saved on this device so a reload while offline does not lose them.
//
// The client saves the text as it stands plus every queued op after
// each change, and clears the box once the queue is empty — then the
// server holds everything and there is nothing to restore. On
// connecting to the same room, the saved text and queue come back,
// and the normal rejoin replays the queued ops against the server.
//
// Storage is injected so tests can pass a memory store: in the
// browser it is localStorage, and anything without it (private mode,
// a quota error, Node) falls back to memory. A save that throws is
// skipped — the in-memory queue still works, the reload just loses it.

import { isAttrs, isOp, type Attrs, type Op } from "./protocol.js";

/** The smallest storage shape the outbox needs. */
export interface OfflineStore {
  get(key: string): string | null;
  set(key: string, value: string): void;
  remove(key: string): void;
}

/** An in-memory store: the fallback, and what tests use. */
export function memoryStore(): OfflineStore {
  const data = new Map<string, string>();
  return {
    get: (key) => data.get(key) ?? null,
    set: (key, value) => void data.set(key, value),
    remove: (key) => void data.delete(key),
  };
}

/** localStorage when it is there and works, memory otherwise. */
export function browserStore(): OfflineStore {
  try {
    const storage = globalThis.localStorage;
    if (storage !== undefined) {
      const probe = "collab-offline-probe";
      storage.setItem(probe, "1");
      storage.removeItem(probe);
      return {
        get: (key) => storage.getItem(key),
        set: (key, value) => storage.setItem(key, value),
        remove: (key) => storage.removeItem(key),
      };
    }
  } catch {
    // Private mode, a quota error, or no localStorage at all.
  }
  return memoryStore();
}

/** One queued edit, as saved: enough to resend it after a reload. */
export interface SavedPendingOp {
  seq: number;
  op: Op;
  baseVersion: number;
  /** True when the edit is already in the saved text (local echo). */
  echoed: boolean;
}

/** Everything a reload needs to pick up where the client left off. */
export interface SavedState {
  room: string;
  text: string;
  attrs: Attrs[];
  version: number;
  /** The next sequence number, so a reloaded client never reuses one
   * the server already answered. */
  nextSeq: number;
  pending: SavedPendingOp[];
}

export class OfflineOutbox {
  private readonly store: OfflineStore;

  constructor(store: OfflineStore = browserStore()) {
    this.store = store;
  }

  /** The storage key for a room: the room id is user input, so it is
   * encoded rather than trusted as part of the key. */
  static keyFor(room: string): string {
    return `collab-offline:${encodeURIComponent(room)}`;
  }

  save(state: SavedState): void {
    try {
      this.store.set(OfflineOutbox.keyFor(state.room), JSON.stringify(state));
    } catch {
      // Saving is best-effort: the queue in memory still works, a
      // reload just will not bring the edits back.
    }
  }

  load(room: string): SavedState | null {
    let raw: string | null;
    try {
      raw = this.store.get(OfflineOutbox.keyFor(room));
    } catch {
      return null;
    }
    if (raw === null) return null;
    const state = parseState(raw);
    if (state === null || state.room !== room) {
      this.clear(room);
      return null;
    }
    return state;
  }

  clear(room: string): void {
    try {
      this.store.remove(OfflineOutbox.keyFor(room));
    } catch {
      // Nothing to do: the box is gone or was never there.
    }
  }
}

/** Parses saved JSON back into a state, or null when it is not one:
 * a corrupt or half-written box never crashes the client. */
function parseState(raw: string): SavedState | null {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const state = value as Record<string, unknown>;
  if (
    typeof state.room !== "string" ||
    typeof state.text !== "string" ||
    typeof state.version !== "number" ||
    typeof state.nextSeq !== "number" ||
    !Array.isArray(state.attrs) ||
    !state.attrs.every(isAttrs) ||
    !Array.isArray(state.pending)
  ) {
    return null;
  }
  const pending: SavedPendingOp[] = [];
  for (const entry of state.pending) {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) return null;
    const e = entry as Record<string, unknown>;
    if (
      typeof e.seq !== "number" ||
      typeof e.baseVersion !== "number" ||
      typeof e.echoed !== "boolean" ||
      !isOp(e.op)
    ) {
      return null;
    }
    pending.push({ seq: e.seq, op: e.op as Op, baseVersion: e.baseVersion, echoed: e.echoed });
  }
  return {
    room: state.room,
    text: state.text,
    attrs: state.attrs as Attrs[],
    version: state.version,
    nextSeq: state.nextSeq,
    pending,
  };
}
