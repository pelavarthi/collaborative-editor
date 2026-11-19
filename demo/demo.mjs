// Scripted two-client demo: no browser needed. Alice and Bob join the
// same room, type at the same spot at the same time, and both end up
// with the same text — that is the whole point of the editor. Then one
// of them bolds a word and the other sees the formatting land.
//
// Usage: node demo/demo.mjs [ws://127.0.0.1:8080]
// Or run demo/run.sh, which starts a throwaway server for you.

const url = process.argv[2] ?? process.env.COLLAB_WS_URL ?? "ws://127.0.0.1:8080";
const room = `demo-${Date.now()}`;

function applyOp(text, attrs, op) {
  if ("Insert" in op) {
    const { pos, text: insert } = op.Insert;
    const styled = Array.from(insert, () => ({ ...(op.Insert.attrs ?? { bold: false }) }));
    return {
      text: text.slice(0, pos) + insert + text.slice(pos),
      attrs: [...attrs.slice(0, pos), ...styled, ...attrs.slice(pos)],
    };
  }
  if ("Delete" in op) {
    const { pos, len } = op.Delete;
    return {
      text: text.slice(0, pos) + text.slice(pos + len),
      attrs: [...attrs.slice(0, pos), ...attrs.slice(pos + len)],
    };
  }
  const { pos, len, change } = op.Format;
  const next = attrs.slice();
  for (let i = pos; i < pos + len && i < next.length; i++) {
    next[i] = { ...next[i], bold: change.bold ?? next[i].bold };
  }
  return { text, attrs: next };
}

class DemoClient {
  constructor(name, site) {
    this.name = name;
    this.site = site;
    this.seq = 0;
    this.text = "";
    this.attrs = [];
    this.version = 0;
    this.waiters = [];
    this.presenceSeen = [];
    this.ws = new WebSocket(url);
    this.ws.addEventListener("message", (event) => this.onMessage(JSON.parse(event.data)));
    this.ws.addEventListener("error", (event) => this.fail(`websocket error: ${event.message}`));
  }

  fail(message) {
    console.error(`[${this.name}] ${message}`);
    process.exit(1);
  }

  onMessage(msg) {
    switch (msg.type) {
      case "joined":
        this.text = msg.text;
        this.attrs = msg.attrs ?? Array.from(msg.text, () => ({ bold: false }));
        this.version = msg.version;
        break;
      case "op":
        ({ text: this.text, attrs: this.attrs } = applyOp(this.text, this.attrs, msg.op));
        this.version = msg.version;
        break;
      case "ack":
        this.version = msg.version;
        break;
      case "presence":
        this.presenceSeen.push(msg.presence);
        break;
      case "error":
        this.fail(`server error: ${msg.message}`);
        break;
    }
    this.waiters = this.waiters.filter((wait) => !wait(msg));
  }

  send(msg) {
    this.ws.send(JSON.stringify(msg));
  }

  join() {
    this.send({ type: "join", room, client_id: this.site, name: this.name });
  }

  op(op) {
    // Local echo: apply at once, then send. The ack later just
    // confirms it; remote ops arrive already transformed, so they
    // apply as-is.
    ({ text: this.text, attrs: this.attrs } = applyOp(this.text, this.attrs, op));
    this.send({ type: "op", base_version: this.version, seq: ++this.seq, op });
  }

  waitFor(predicate, what) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), 5000);
      this.waiters.push((msg) => {
        if (predicate(msg)) {
          clearTimeout(timer);
          resolve(msg);
          return true;
        }
        return false;
      });
    });
  }

  waitForVersion(version) {
    if (this.version >= version) return Promise.resolve();
    return this.waitFor((msg) => msg.type === "op" || msg.type === "ack", `version ${version}`)
      .then(() => this.waitForVersion(version));
  }
}

function open(ws) {
  return new Promise((resolve) => ws.addEventListener("open", resolve, { once: true }));
}

const alice = new DemoClient("alice", 1);
const bob = new DemoClient("bob", 2);
await Promise.all([open(alice.ws), open(bob.ws)]);
alice.join();
bob.join();
await Promise.all([
  alice.waitFor((m) => m.type === "joined", "alice to join"),
  bob.waitFor((m) => m.type === "joined", "bob to join"),
]);
console.log(`both joined room ${room} (empty document, version 0)`);

// Both type at position 0 at the same time, from the same version.
// The server transforms one past the other; the tie is broken by
// site id, so the order is deterministic and both clients converge.
alice.op({ Insert: { site: 1, pos: 0, text: "Hello " } });
bob.op({ Insert: { site: 2, pos: 0, text: "world!" } });
await Promise.all([alice.waitForVersion(2), bob.waitForVersion(2)]);
console.log(`after concurrent typing at position 0:`);
console.log(`  alice: ${JSON.stringify(alice.text)}`);
console.log(`  bob:   ${JSON.stringify(bob.text)}`);
if (alice.text !== bob.text) {
  console.error("clients diverged — demo failed");
  process.exit(1);
}

// One more line, typed after both have seen everything.
alice.op({ Insert: { site: 1, pos: alice.text.length, text: " Two editors, one document." } });
await Promise.all([alice.waitForVersion(3), bob.waitForVersion(3)]);

// Bob bolds the greeting; Alice sees the formatting arrive as an op.
const helloAt = bob.text.indexOf("Hello");
bob.op({ Format: { site: 2, pos: helloAt, len: 5, change: { bold: true } } });
await Promise.all([alice.waitForVersion(4), bob.waitForVersion(4)]);
const bolded = alice.attrs.slice(helloAt, helloAt + 5).every((a) => a.bold);
console.log(`after bob bolds "Hello": bold on alice's copy: ${bolded}`);

// Presence: alice moves her caret to the end; bob's client sees it.
alice.send({ type: "presence", cursor: alice.text.length, selection: null });
await bob.waitFor((m) => m.type === "presence" && m.presence.client_id === 1, "bob to see alice");
console.log(`bob sees alice's cursor at ${alice.text.length}`);

console.log(`\nfinal document (version ${alice.version}): ${JSON.stringify(alice.text)}`);
console.log("converged: both clients hold the same text at the same version.");
process.exit(bolded ? 0 : 1);
