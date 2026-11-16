// Entry point: two pages in one, picked by the URL.
//
// With ?room=notes it is the editor: connects to the server on
// localhost:8080, joins that room, and binds the editor UI. `name`
// is the display name on the cursor; `mode=read` joins as a viewer,
// who can watch but not type.
//
// Without a room it is the documents list: connects, asks the server
// what documents exist, and shows them newest first with a form to
// create a new one by name.
//
// A random client id is picked per page load; it is the site id the
// server uses to break ties between two clients inserting at the same spot.

import { CollabClient } from "./client.js";
import { bindDocList, bindEditor, copyText, docUrl } from "./ui.js";

const params = new URLSearchParams(window.location.search);
const room = params.get("room");
const name = params.get("name") || undefined;
const readOnly = params.get("mode") === "read";

const editorSection = document.getElementById("editorSection") as HTMLElement;
const docsSection = document.getElementById("docsSection") as HTMLElement;

const site = Math.floor(Math.random() * 2 ** 31) + 1;
const client = new CollabClient(site);

if (room !== null && room !== "") {
  docsSection.hidden = true;

  const area = document.getElementById("editor") as HTMLTextAreaElement;
  const statusEl = document.getElementById("status") as HTMLElement;
  const presenceEl = document.getElementById("presence") as HTMLElement;
  const formatBar = document.getElementById("formatbar") as HTMLElement;
  const previewEl = document.getElementById("preview") as HTMLElement;
  const roomInput = document.getElementById("room") as HTMLInputElement;
  const nameInput = document.getElementById("name") as HTMLInputElement;
  const joinForm = document.getElementById("join") as HTMLFormElement;
  const copyEditLink = document.getElementById("copyEditLink") as HTMLButtonElement;
  const copyReadLink = document.getElementById("copyReadLink") as HTMLButtonElement;

  roomInput.value = room;
  if (name) nameInput.value = name;

  bindEditor(area, statusEl, presenceEl, client, formatBar, previewEl);
  client.connect("ws://localhost:8080", room, name, readOnly);

  // Share links: this page as it is for an editor, and with
  // mode=read for a viewer who can watch but not type.
  copyEditLink.addEventListener("click", () => void copyText(docUrl(room, false), copyEditLink));
  copyReadLink.addEventListener("click", () => void copyText(docUrl(room, true), copyReadLink));

  joinForm.addEventListener("submit", (event) => {
    event.preventDefault();
    const next = new URLSearchParams(window.location.search);
    next.set("room", roomInput.value.trim() || "notes");
    if (nameInput.value.trim()) next.set("name", nameInput.value.trim());
    else next.delete("name");
    window.location.search = next.toString();
  });
} else {
  editorSection.hidden = true;

  const docsEl = document.getElementById("docs") as HTMLElement;
  const newDocForm = document.getElementById("newDoc") as HTMLFormElement;
  const newDocName = document.getElementById("newDocName") as HTMLInputElement;

  bindDocList(docsEl, newDocForm, newDocName, client);
  client.connectForList("ws://localhost:8080");
}
