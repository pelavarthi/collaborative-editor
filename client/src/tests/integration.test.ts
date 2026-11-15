// End-to-end: two real TypeScript clients talking to the real Rust server
// over real WebSockets. Skips when the server binary has not been built
// (run `cargo build -p collab-server` from the repo root first).

import test from "node:test";
import assert from "node:assert/strict";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { CollabClient } from "../client.js";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const serverBin = join(repoRoot, "target", "debug", "collab-server");

function waitFor(label: string, check: () => boolean, timeoutMs = 15000): Promise<void> {
  const started = Date.now();
  return new Promise((resolve, reject) => {
    const tick = () => {
      if (check()) return resolve();
      if (Date.now() - started > timeoutMs) return reject(new Error(`timed out: ${label}`));
      setTimeout(tick, 25);
    };
    tick();
  });
}

async function startServer(): Promise<{ proc: ChildProcess; addr: string; dir: string }> {
  const dir = mkdtempSync(join(tmpdir(), "collab-integration-"));
  const proc = spawn(serverBin, ["127.0.0.1:0", dir], { stdio: ["ignore", "pipe", "pipe"] });
  const addr = await new Promise<string>((resolve, reject) => {
    let out = "";
    const timer = setTimeout(() => reject(new Error("server did not print its address")), 5000);
    proc.stdout?.on("data", (chunk: Buffer) => {
      out += chunk.toString();
      const match = /listening on (\S+)/.exec(out);
      if (match?.[1]) {
        clearTimeout(timer);
        resolve(match[1]);
      }
    });
    proc.on("error", (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
  return { proc, addr, dir };
}

async function connectClient(site: number, addr: string, room: string): Promise<CollabClient> {
  const client = new CollabClient(site);
  client.connect(`ws://${addr}`, room);
  await waitFor(`client ${site} to join`, () => client.status === "connected");
  return client;
}

test(
  "a client that drops reconnects and its queued edits land",
  { skip: !existsSync(serverBin) },
  async (t) => {
    const { proc, addr, dir } = await startServer();
    t.after(() => {
      proc.kill();
      rmSync(dir, { recursive: true, force: true });
    });

    const a = await connectClient(1, addr, "drop-and-return");
    const b = await connectClient(2, addr, "drop-and-return");
    t.after(() => {
      a.close();
      b.close();
    });

    a.localEdit("hello");
    await waitFor("B to receive hello", () => b.editor.text === "hello");
    // Wait for A's ack too, so only the offline edit is pending below.
    await waitFor("A's first edit to be acked", () => a.pendingCount === 0);

    // A drops off the network and keeps typing while it is down.
    a.dropConnection();
    await waitFor("A to report reconnecting", () => a.status === "reconnecting");
    a.localEdit("hello world");
    assert.equal(a.pendingCount, 1);
    assert.equal(b.editor.text, "hello");

    // A rejoins on its own, resends the queued edit, and both sides
    // end up with the same text.
    await waitFor("A to reconnect", () => a.status === "connected");
    await waitFor("B to receive the queued edit", () => b.editor.text === "hello world");
    assert.equal(a.editor.text, "hello world");
    assert.equal(a.pendingCount, 0);
  },
);

test(
  "two clients type together and converge",
  { skip: !existsSync(serverBin) },
  async (t) => {
    const { proc, addr, dir } = await startServer();
    t.after(() => {
      proc.kill();
      rmSync(dir, { recursive: true, force: true });
    });

    const a = await connectClient(1, addr, "integration");
    const b = await connectClient(2, addr, "integration");
    t.after(() => {
      a.close();
      b.close();
    });

    // A types; B sees it without typing anything itself.
    a.localEdit("hello");
    await waitFor("B to receive hello", () => b.editor.text === "hello");
    assert.equal(a.editor.text, "hello");

    // B continues the sentence; A sees it.
    b.localEdit("hello world");
    await waitFor("A to receive hello world", () => a.editor.text === "hello world");
    assert.equal(b.editor.text, "hello world");

    // Presence: A's caret at the end shows up on B.
    a.sendPresence(11, null);
    await waitFor("B to see A's caret", () => b.presences.get(1)?.cursor === 11);

    // Both type at the same spot at the same time: the server orders the
    // ops, and both clients must end up with the same text.
    a.localEdit("hello world!");
    b.localEdit("hello world?");
    await waitFor(
      "clients to converge",
      () => a.editor.text === b.editor.text && a.editor.text.length === 13,
    );
    assert.match(a.editor.text, /hello world[?!][?!]/);
  },
);

test(
  "formatting travels between clients",
  { skip: !existsSync(serverBin) },
  async (t) => {
    const { proc, addr, dir } = await startServer();
    t.after(() => {
      proc.kill();
      rmSync(dir, { recursive: true, force: true });
    });

    const a = await connectClient(1, addr, "rich-text");
    const b = await connectClient(2, addr, "rich-text");
    t.after(() => {
      a.close();
      b.close();
    });

    a.localEdit("hello world");
    await waitFor("B to receive hello world", () => b.editor.text === "hello world");
    await waitFor("A's edit to be acked", () => a.pendingCount === 0);

    // A bolds "hello": B sees the bold without the text changing.
    a.formatRange(0, 5, { bold: true });
    await waitFor("B to see the bold", () => b.editor.attrs[0]?.bold === true);
    assert.equal(b.editor.attrs[4]?.bold, true);
    assert.equal(b.editor.attrs[5]?.bold, false);
    assert.equal(a.editor.attrs[0]?.bold, true);

    // A late joiner gets the bold with the text.
    const c = await connectClient(3, addr, "rich-text");
    t.after(() => c.close());
    assert.equal(c.editor.text, "hello world");
    assert.equal(c.editor.attrs[0]?.bold, true);
    assert.equal(c.editor.attrs[5]?.bold, false);
  },
);
