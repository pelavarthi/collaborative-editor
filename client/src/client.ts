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
// Offline mode: the client keeps working while the socket is down —
// typing still echoes and queues — and it saves the text and the queue
// to the offline outbox after each change, so a reload while offline
// brings the edits back. The status says "offline" while the browser
// reports no network, and the client reconnects the moment it is back.
//
// Conflicts on reconnect are settled by the server: it transforms each
// resent op past the ops the client missed, so both sides converge.
// Two cases the transform cannot fix are handled here instead. When an
// ack says an edit transformed into a no-op, the edit is still in the
// local text, so the client drops and rejoins for a fresh snapshot.
// When the server says the client's base version is one it no longer
// has, the queued edits cannot be replayed at all: they are dropped,
// the client rejoins, and the user is told their offline edits were lost.
//
// The socket is injected so tests can drive the client without a network:
// pass a factory that returns a fake. In the browser and in Node, the
// default factory wraps the global WebSocket.

import { EditorState } from "./editor.js";
import { OfflineOutbox } from "./offline.js";
import {
  decodeServer,
  encodeClient,
  type ClientMessage,
  type DocInfo,
  type FormatChange,
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

export type ChangeKind = "joined" | "local" | "remote" | "presence" | "status" | "docs";

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
  /** True when the browser has network: defaults to navigator.onLine,
   * true where there is no navigator. Tests pass a switchable one. */
  isOnline?: () => boolean;
  /** Where offline edits are saved: defaults to localStorage. */
  outbox?: OfflineOutbox;
}

export class CollabClient {
  readonly editor: EditorState;
  readonly site: number;
  room = "";
  status: "connecting" | "connected" | "reconnecting" | "offline" | "closed" = "connecting";
  presences = new Map<number, Presence>();
  lastError: string | null = null;
  /** The documents list from the last `list` answer, newest first. */
  docs: DocInfo[] = [];
  /** True when this client joined as a viewer: it can watch but not
   * type. Set when joining read-only, and confirmed by the Joined
   * message when it arrives. */
  readOnly = false;

  /** Fired after every state change; the UI re-reads the client. */
  onChange: (change: Change) => void = () => {};
  onError: (message: string) => void = () => {};

  private socket: Socket | null = null;
  private socketOpen = false;
  private readonly createSocket: SocketFactory;
  private readonly reconnectDelay: (attempt: number) => number;
  private readonly isOnline: () => boolean;
  private readonly outbox: OfflineOutbox;
  private url = "";
  private name: string | undefined;
  /** The join to send once the socket opens: kept after joining, so
   * a reconnect rejoins the same room on its own. */
  private pendingJoin: { room: string; name: string | undefined; readOnly: boolean } | null = null;
  /** True when the socket should ask for the documents list instead
   * of joining, until `join` picks a room. */
  private listOnOpen = false;
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
    this.isOnline =
      options.isOnline ??
      (() => typeof navigator === "undefined" || navigator.onLine !== false);
    this.outbox = options.outbox ?? new OfflineOutbox();
    // The browser knows it is offline before the socket fails, and
    // knows the moment it is back: both go through networkChanged.
    if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
      window.addEventListener("online", () => this.networkChanged());
      window.addEventListener("offline", () => this.networkChanged());
    }
  }

  /** How many of this client's edits the server has not confirmed yet. */
  get pendingCount(): number {
    return this.pending.length;
  }

  connect(url: string, room: string, name?: string, readOnly = false): void {
    this.url = url;
    this.join(room, name, readOnly);
    this.closedByUser = false;
    this.reconnectAttempt = 0;
    this.status = "connecting";
    // A reload while offline brings back the saved text and queue, so
    // the edits typed before it are not lost. A viewer restores
    // nothing: its edits would be refused by the server anyway.
    if (!readOnly) this.restoreOutbox(room);
    this.openSocket();
  }

  /** Connects without joining, to ask for the documents list first.
   * Call `join` once the user picks one. */
  connectForList(url: string): void {
    this.url = url;
    this.listOnOpen = true;
    this.closedByUser = false;
    this.reconnectAttempt = 0;
    this.status = "connecting";
    this.openSocket();
  }

  /** Joins a room: on the open socket, or on the next one if it is
   * not open yet. `readOnly` joins as a viewer that can watch but
   * not type; the server confirms it in the Joined message. */
  join(room: string, name?: string, readOnly = false): void {
    this.room = room;
    this.name = name;
    this.readOnly = readOnly;
    this.listOnOpen = false;
    this.pendingJoin = { room, name, readOnly };
    this.sendJoinIfReady();
  }

  /** Asks the server for the documents list. */
  listDocs(): void {
    this.send({ type: "list" });
  }

  /** A keystroke: echo it locally at once, then queue the ops it made.
   * A viewer has nothing to echo: its edits would be refused. Works
   * while offline too: the op stays queued, and the outbox save keeps
   * it through a reload. */
  localEdit(newText: string): void {
    if (this.readOnly) return;
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
    this.saveOutbox();
    this.flush();
    if (ops.length > 0) this.onChange({ kind: "local", ops });
  }

  /**
   * Formats a range: bold, italic, or a link. Echoes it locally at
   * once like a keystroke, then queues the op for the server. An empty
   * range or a change that touches nothing sends nothing. A viewer
   * cannot format either: its edits would be refused.
   */
  formatRange(pos: number, len: number, change: FormatChange): void {
    if (this.readOnly) return;
    const op = this.editor.formatRange(pos, len, change);
    if (op === null) return;
    this.pending.push({
      seq: this.nextSeq++,
      op,
      baseVersion: this.editor.version,
      echoed: true,
      sent: false,
    });
    this.saveOutbox();
    this.flush();
    this.onChange({ kind: "local", ops: [op] });
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

  /** The browser's network state changed: go offline at once when it
   * drops, and reconnect right away when it comes back instead of
   * waiting out the backoff timer. */
  networkChanged(): void {
    if (this.closedByUser || this.status === "closed") return;
    if (!this.isOnline()) {
      // The socket close reports it on its own when one is open; this
      // covers the gap while the client is already down.
      if (!this.socketOpen && (this.status === "reconnecting" || this.status === "connecting")) {
        this.status = "offline";
        this.onChange({ kind: "status" });
      }
      return;
    }
    if (this.status === "offline" || this.status === "reconnecting") {
      this.reconnectNow();
    }
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
      this.socketOpen = true;
      // A join queued before the socket opened goes out now; a
      // reconnect rejoins the same room on its own. Otherwise, when
      // the client connected just to list, ask for the documents.
      this.sendJoinIfReady();
      if (this.pendingJoin === null && this.listOnOpen) {
        this.listOnOpen = false;
        this.send({ type: "list" });
      }
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
      this.socketOpen = false;
      if (this.closedByUser) {
        this.status = "closed";
      } else if (!this.isOnline()) {
        // The browser is offline: no point backing off and retrying
        // into a dead network. It stays offline until the network
        // comes back and networkChanged reconnects it.
        this.status = "offline";
        this.onChange({ kind: "status" });
        return;
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

  /** Reconnects right away, dropping any backoff timer that is
   * waiting: the network is back, so there is nothing to wait for. */
  private reconnectNow(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.reconnectAttempt = 0;
    this.status = "connecting";
    this.onChange({ kind: "status" });
    this.openSocket();
  }

  /** Sends the queued join when the socket is open. `read_only` is
   * only sent when true, so an editor's join keeps the old shape. */
  private sendJoinIfReady(): void {
    if (!this.socketOpen || this.pendingJoin === null) return;
    const { room, name, readOnly } = this.pendingJoin;
    this.send({
      type: "join",
      room,
      client_id: this.site,
      ...(name !== undefined ? { name } : {}),
      ...(readOnly ? { read_only: true } : {}),
    });
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
        this.editor.replace(msg.text, msg.version, msg.attrs);
        this.presences = new Map(msg.presences.map((p) => [p.client_id, p]));
        this.readOnly = msg.read_only ?? false;
        this.status = "connected";
        this.reconnectAttempt = 0;
        // The rejoin replaced the text, so the echo of every queued op
        // is gone with it: resend them, and let the ack put each one
        // back as the server applied it.
        this.forgetEchoes();
        this.saveOutbox();
        this.onChange({ kind: "joined" });
        this.flush();
        break;
      case "docs":
        this.docs = msg.docs;
        this.onChange({ kind: "docs" });
        break;
      case "op":
        this.editor.applyRemote(msg.op, msg.version);
        this.saveOutbox();
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
          if (entry !== undefined && entry.echoed && msg.op === null) {
            // The server dropped this edit — it transformed into
            // nothing — but the edit is still in the local text, so
            // the local copy is now wrong. Only a fresh snapshot
            // fixes it: drop and rejoin. The other queued edits
            // survive: after the rejoin they are resent with
            // echoed false, so a second null ack for them is harmless.
            this.resyncAfterConflict("an edit conflicted and was dropped; resyncing", false);
            break;
          }
        } else {
          // A duplicate ack, e.g. for an op resent after a lost ack:
          // the op is already gone from the queue, so just take the
          // version.
          this.editor.version = Math.max(this.editor.version, msg.version);
        }
        this.saveOutbox();
        this.onChange({ kind: "status" });
        break;
      }
      case "ops": {
        const ops: Op[] = [];
        msg.ops.forEach((op, i) => {
          this.editor.applyRemote(op, msg.from_version + i + 1);
          ops.push(op);
        });
        this.saveOutbox();
        this.onChange({ kind: "remote", ops });
        break;
      }
      case "snapshot":
        this.editor.replace(msg.text, msg.version, msg.attrs);
        // Like a rejoin, the snapshot replaces the text, so the echo
        // of every queued op is gone with it: resend them, and let
        // the acks put each one back as the server applied it.
        this.forgetEchoes();
        this.saveOutbox();
        this.onChange({ kind: "remote" });
        this.flush();
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
        } else if (msg.message.startsWith("base version")) {
          // The queued edits were written against a version the server
          // no longer has — its snapshot moved past them while this
          // client was offline, or the server restarted empty. They
          // cannot be replayed: their positions belong to a document
          // that is gone, so dropping them all is the only safe move.
          // The client rejoins for a fresh copy and the user is told
          // their offline edits were lost, instead of the document
          // silently diverging or corrupting.
          this.resyncAfterConflict(
            "edits made while offline could not be replayed; resynced from the server",
            true,
          );
        } else {
          this.fail(msg.message);
        }
        break;
    }
  }

  /** The text was replaced wholesale (a rejoin or a snapshot), so the
   * echo of every queued op is gone with it: resend them, and let the
   * acks put each one back as the server applied it. */
  private forgetEchoes(): void {
    for (const entry of this.pending) {
      entry.echoed = false;
      entry.sent = false;
    }
  }

  /** The local copy cannot be trusted any more, so pull a fresh one:
   * drop the socket and let the reconnect rejoin the room. When
   * `dropPending` is set, the queued edits are unreplayable and go
   * with it; otherwise they are resent after the rejoin. */
  private resyncAfterConflict(message: string, dropPending: boolean): void {
    if (dropPending) {
      this.pending = [];
      this.outbox.clear(this.room);
    }
    this.lastError = message;
    this.onError(message);
    this.saveOutbox();
    this.dropConnection();
  }

  /** Saves the text as it stands plus the edits the server has not
   * confirmed yet. Empty when everything is acked, so a reload after
   * a clean session restores nothing. */
  private saveOutbox(): void {
    if (this.pending.length === 0) {
      this.outbox.clear(this.room);
      return;
    }
    this.outbox.save({
      room: this.room,
      text: this.editor.text,
      attrs: this.editor.attrs,
      version: this.editor.version,
      nextSeq: this.nextSeq,
      pending: this.pending.map((entry) => ({
        seq: entry.seq,
        op: entry.op,
        baseVersion: entry.baseVersion,
        echoed: entry.echoed,
      })),
    });
  }

  /** Brings back what was saved before a reload while offline: the
   * text, with the queued edits in it, and the queue itself. The
   * sequence numbers come back too, so a resent op keeps the number
   * the server may already have answered. */
  private restoreOutbox(room: string): void {
    const saved = this.outbox.load(room);
    if (saved === null) return;
    this.editor.replace(saved.text, saved.version, saved.attrs);
    this.pending = saved.pending.map((entry) => ({ ...entry, sent: false }));
    this.nextSeq = saved.nextSeq;
  }
}
