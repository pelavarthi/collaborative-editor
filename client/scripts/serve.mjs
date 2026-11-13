// Tiny static server for the client: serves this directory (index.html and
// dist/) on http://localhost:3000. Run with `npm start` from client/.
// The editor itself still talks to the Rust server on ws://localhost:8080.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(fileURLToPath(import.meta.url), "..", "..");
const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".json": "application/json",
};

const server = createServer(async (req, res) => {
  const path = normalize(join(root, decodeURIComponent(req.url?.split("?")[0] ?? "/")));
  if (!path.startsWith(root)) {
    res.writeHead(403);
    res.end("forbidden");
    return;
  }
  const file = path.endsWith("/") ? join(path, "index.html") : path;
  try {
    const data = await readFile(file);
    res.writeHead(200, { "content-type": types[extname(file)] ?? "application/octet-stream" });
    res.end(data);
  } catch {
    res.writeHead(404);
    res.end("not found");
  }
});

server.listen(3000, () => console.log("collab client at http://localhost:3000"));
