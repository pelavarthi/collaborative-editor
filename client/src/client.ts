// The WebSocket client: joins a room, sends local edits with local echo,
// and applies the ops and presence the server sends back.
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

export class CollabClient {
  readonly editor: EditorState;
  readonly site: number;
  room = "";
  status: "connecting" | "connected" | "closed" = "connecting";
  presences = new Map<number, Presence>();
  lastError: string | null = null;

  /** Fired after every state change; the UI re-reads the client. */
  onChange: (change: Change) => void = () => {};
  onError: (message: string) => void = () => {};

  private socket: Socket | null = null;
  private readonly createSocket: SocketFactory;

  constructor(site: number, createSocket: SocketFactory = (url) => wrapWebSocket(new WebSocket(url))) {
    this.site = site;
    this.editor = new EditorState(site);
    this.createSocket = createSocket;
  }

  connect(url: string, room: string, name?: string): void {
    this.room = room;
    this.status = "connecting";
    const socket = this.createSocket(url);
    this.socket = socket;
    socket.onopen = () => {
      // The first message must be a join; without a room there is
      // nothing to do.
      const join: ClientMessage =
        name === undefined
          ? { type: "join", room, client_id: this.site }
          : { type: "join", room, client_id: this.site, name };
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
      this.status = "closed";
      this.onChange({ kind: "status" });
    };
    socket.onerror = (err) => {
      this.fail(err instanceof Error ? err.message : "websocket error");
    };
  }

  /** A keystroke: echo it locally at once, then send the ops it made. */
  localEdit(newText: string): void {
    const ops = this.editor.localEdit(newText);
    for (const op of ops) {
      this.send({ type: "op", base_version: this.editor.version, op });
    }
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

  close(): void {
    this.socket?.close();
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
        this.onChange({ kind: "joined" });
        break;
      case "op":
        this.editor.applyRemote(msg.op, msg.version);
        this.onChange({ kind: "remote", ops: [msg.op] });
        break;
      case "ack":
        // An ack for an op sent earlier can arrive after a newer
        // broadcast already moved the version on; never move it back.
        this.editor.version = Math.max(this.editor.version, msg.version);
        this.onChange({ kind: "status" });
        break;
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
        this.fail(msg.message);
        break;
    }
  }
}
