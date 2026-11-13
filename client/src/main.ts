// Entry point: reads the room and name from the URL (?room=notes&name=pranav),
// connects to the server on localhost:8080, and binds the editor UI.
//
// A random client id is picked per page load; it is the site id the
// server uses to break ties between two clients inserting at the same spot.

import { CollabClient } from "./client.js";
import { bindEditor } from "./ui.js";

const params = new URLSearchParams(window.location.search);
const room = params.get("room") || "notes";
const name = params.get("name") || undefined;

const area = document.getElementById("editor") as HTMLTextAreaElement;
const statusEl = document.getElementById("status") as HTMLElement;
const presenceEl = document.getElementById("presence") as HTMLElement;
const roomInput = document.getElementById("room") as HTMLInputElement;
const nameInput = document.getElementById("name") as HTMLInputElement;
const joinForm = document.getElementById("join") as HTMLFormElement;

roomInput.value = room;
if (name) nameInput.value = name;

const site = Math.floor(Math.random() * 2 ** 31) + 1;
const client = new CollabClient(site);
bindEditor(area, statusEl, presenceEl, client);
client.connect("ws://localhost:8080", room, name);

joinForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const next = new URLSearchParams(window.location.search);
  next.set("room", roomInput.value.trim() || "notes");
  if (nameInput.value.trim()) next.set("name", nameInput.value.trim());
  else next.delete("name");
  window.location.search = next.toString();
});
