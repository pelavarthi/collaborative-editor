// The WebSocket client: joins a room, sends local edits with local echo,
// and applies the ops and presence the server sends back.
//
// Every local edit is queued until the server acks it. An op goes out
// with a sequence number, and the ack carries that number back, so the
// client knows exactly which edit was confirmed. If the socket drops,
// the edits still waiting stay in the queue: the client reconnects on
// its own, rejoins, and resends them, and the server answers a resend
// from memory instead of applying the op twice.
//
// The socket is injected so tests can drive the client without a network:
// pass a factory that returns a fake. In the browser and in Node, the
// default factory wraps the global WebSocket.

import { EditorState } from "./editor.js";
import {
  decodeServer,
  encodeClient,
  type ClientMessage,
  type Op,
  type Presence,
  type Selection,
  type ServerMessage,
} from "./protocol.js";

/** The smallest socket shape the client needs. */
export interface Socket {
  send(data: string): void;
  close(): void;
  onopen: (() => void) | null;
  onmessage: ((data: string) => void) | null;
  onclose: (() => void) | null;
  onerror: ((error: unknown) => void) | null;
}

/** Adapts a real WebSocket (browser or Node) to the Socket shape. */
export function wrapWebSocket(ws: WebSocket): Socket {
  const socket: Socket = {
    send: (data) => ws.send(data),
    close: () => ws.close(),
    onopen: null,
    onmessage: null,
    onclose: null,
    onerror: null,
  };
  ws.onopen = () => socket.onopen?.();
  ws.onmessage = (event) => socket.onmessage?.(String(event.data));
  ws.onclose = () => socket.onclose?.();
  ws.onerror = (event) => socket.onerror?.(event);
  return socket;
}

export type ChangeKind = "joined" | "local" | "remote" | "presence" | "status";

export interface Change {
  kind: ChangeKind;
  /** The remote ops behind this change, for moving the caret past them. */
  ops?: Op[];
}

export type SocketFactory = (url: string) => Socket;

/** One local edit waiting for the server to confirm it. */
interface PendingOp {
  /** Counts this client's ops from 1 up; a resend keeps its number. */
  seq: number;
  op: Op;
  /** The server version the op was made against. */
  baseVersion: number;
  /** True while the op is already in the local text (local echo). A
   * rejoin replaces the text, so a resend after one is not echoed. */
  echoed: boolean;
  /** True once the op has gone out on the current socket. */
  sent: boolean;
}

export interface CollabClientOptions {
  /** How long to wait before reconnect attempt `n` (0-based). Defaults
   * to 500ms, doubling each time up to 8s. */
  reconnectDelay?: (attempt: number) => number;
}

export class CollabClient {
  readonly editor: EditorState;
  readonly site: number;
  room = "";
  status: "connecting" | "connected" | "reconnecting" | "closed" = "connecting";
  presences = new Map<number, Presence>();
  lastError: string | null = null;

  /** Fired after every state change; the UI re-reads the client. */
  onChange: (change: Change) => void = () => {};
  onError: (message: string) => void = () => {};

  private socket: Socket | null = null;
  private readonly createSocket: SocketFactory;
  private readonly reconnectDelay: (attempt: number) => number;
  private url = "";
  private name: string | undefined;
  private pending: PendingOp[] = [];
  private nextSeq = 1;
  private reconnectAttempt = 0;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private closedByUser = false;

  constructor(
    site: number,
    createSocket: SocketFactory = (url) => wrapWebSocket(new WebSocket(url)),
    options: CollabClientOptions = {},
  ) {
    this.site = site;
    this.editor = new EditorState(site);
    this.createSocket = createSocket;
    this.reconnectDelay =
      options.reconnectDelay ?? ((attempt) => Math.min(500 * 2 ** attempt, 8000));
  }

  /** How many of this client's edits the server has not confirmed yet. */
  get pendingCount(): number {
    return this.pending.length;
  }

  connect(url: string, room: string, name?: string): void {
    this.url = url;
    this.room = room;
    this.name = name;
    this.closedByUser = false;
    this.reconnectAttempt = 0;
    this.status = "connecting";
    this.openSocket();
  }

  /** A keystroke: echo it locally at once, then queue the ops it made. */
  localEdit(newText: string): void {
    const ops = this.editor.localEdit(newText);
    for (const op of ops) {
      this.pending.push({
        seq: this.nextSeq++,
        op,
        baseVersion: this.editor.version,
        echoed: true,
        sent: false,
      });
    }
    this.flush();
    if (ops.length > 0) this.onChange({ kind: "local", ops });
  }

  /** Reports where this client's caret is and what it has highlighted. */
  sendPresence(cursor: number, selection: Selection | null): void {
    this.send({ type: "presence", cursor, selection });
  }

  /** Asks for just the ops missed since `baseVersion` (reconnect catch-up). */
  sendSync(baseVersion: number): void {
    this.send({ type: "sync", base_version: baseVersion });
  }

  /** Drops the socket the way a network failure would: the client then
   * reconnects on its own and resends whatever it had not had acked.
   * `close()` is the deliberate goodbye; this is the accidental one. */
  dropConnection(): void {
    this.socket?.close();
  }

  close(): void {
    this.closedByUser = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.status = "closed";
    this.onChange({ kind: "status" });
    this.socket?.close();
  }

  private openSocket(): void {
    const socket = this.createSocket(this.url);
    this.socket = socket;
    socket.onopen = () => {
      // The first message must be a join; without a room there is
      // nothing to do.
      const join: ClientMessage =
        this.name === undefined
          ? { type: "join", room: this.room, client_id: this.site }
          : { type: "join", room: this.room, client_id: this.site, name: this.name };
      socket.send(encodeClient(join));
    };
    socket.onmessage = (data) => {
      let msg: ServerMessage;
      try {
        msg = decodeServer(data);
      } catch (err) {
        this.fail(err instanceof Error ? err.message : String(err));
        return;
      }
      this.handleMessage(msg);
    };
    socket.onclose = () => {
      this.socket = null;
      if (this.closedByUser) {
        this.status = "closed";
      } else {
        this.scheduleReconnect();
        return;
      }
      this.onChange({ kind: "status" });
    };
    socket.onerror = (err) => {
      this.fail(err instanceof Error ? err.message : "websocket error");
    };
  }

  private scheduleReconnect(): void {
    this.status = "reconnecting";
    this.onChange({ kind: "status" });
    const delay = this.reconnectDelay(this.reconnectAttempt++);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (this.closedByUser) return;
      this.status = "connecting";
      this.onChange({ kind: "status" });
      this.openSocket();
    }, delay);
  }

  /** Sends every queued op that has not gone out on this socket yet. */
  private flush(): void {
    if (this.status !== "connected" || this.socket === null) return;
    for (const entry of this.pending) {
      if (entry.sent) continue;
      try {
        this.send({
          type: "op",
          base_version: entry.baseVersion,
          seq: entry.seq,
          op: entry.op,
        });
      } catch {
        // The socket died between the status check and the send; the op
        // stays queued and goes out on the next connection instead.
        break;
      }
      entry.sent = true;
    }
  }

  private send(msg: Parameters<typeof encodeClient>[0]): void {
    this.socket?.send(encodeClient(msg));
  }

  private fail(message: string): void {
    this.lastError = message;
    this.onError(message);
  }

  private handleMessage(msg: ServerMessage): void {
    switch (msg.type) {
      case "joined":
        this.editor.replace(msg.text, msg.version);
        this.presences = new Map(msg.presences.map((p) => [p.client_id, p]));
        this.status = "connected";
        this.reconnectAttempt = 0;
        // The rejoin replaced the text, so the echo of every queued op
        // is gone with it: resend them, and let the ack put each one
        // back as the server applied it.
        for (const entry of this.pending) {
          entry.echoed = false;
          entry.sent = false;
        }
        this.onChange({ kind: "joined" });
        this.flush();
        break;
      case "op":
        this.editor.applyRemote(msg.op, msg.version);
        this.onChange({ kind: "remote", ops: [msg.op] });
        break;
      case "ack": {
        const at = this.pending.findIndex((entry) => entry.seq === msg.seq);
        if (at >= 0) {
          const [entry] = this.pending.splice(at, 1);
          if (entry !== undefined && !entry.echoed && msg.op !== null) {
            // The echo was wiped by a rejoin: apply the op as the
            // server applied it, already transformed past anything
            // this client had not seen.
            this.editor.applyRemote(msg.op, msg.version);
          } else {
            // An ack for an op sent earlier can arrive after a newer
            // broadcast already moved the version on; never move it back.
            this.editor.version = Math.max(this.editor.version, msg.version);
          }
          // Known edge: an echoed op that transformed into a no-op on
          // the server (msg.op is null) is still in the local text.
          // It only happens when a concurrent edit deleted the exact
          // range the op touched; a resync recovers from it.
        } else {
          // A duplicate ack, e.g. for an op resent after a lost ack:
          // the op is already gone from the queue, so just take the
          // version.
          this.editor.version = Math.max(this.editor.version, msg.version);
        }
        this.onChange({ kind: "status" });
        break;
      }
      case "ops": {
        const ops: Op[] = [];
        msg.ops.forEach((op, i) => {
          this.editor.applyRemote(op, msg.from_version + i + 1);
          ops.push(op);
        });
        this.onChange({ kind: "remote", ops });
        break;
      }
      case "snapshot":
        this.editor.replace(msg.text, msg.version);
        this.onChange({ kind: "remote" });
        break;
      case "presence":
        this.presences.set(msg.presence.client_id, msg.presence);
        this.onChange({ kind: "presence" });
        break;
      case "presence_left":
        this.presences.delete(msg.client_id);
        this.onChange({ kind: "presence" });
        break;
      case "error":
        // The server drops a client that fell too far behind its
        // broadcast buffer and asks it to rejoin; catch up from the
        // missed ops first, then the reconnect rejoins as usual.
        if (msg.message === "missed messages, please rejoin") {
          this.sendSync(this.editor.version);
        } else {
          this.fail(msg.message);
        }
        break;
    }
  }
}
