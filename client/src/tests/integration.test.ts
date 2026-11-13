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
