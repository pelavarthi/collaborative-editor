// Binds a <textarea> to a CollabClient: typing edits locally at once
// (local echo) and sends the op, remote ops rewrite the box while keeping
// the caret on the same text, and everyone else's cursor shows up in the
// presence list with their colour.
//
// The textarea shows plain text, so formatting is applied from the
// toolbar and shown in a read-only preview below it: the preview renders
// the document's spans as bold, italic, and links, built with DOM calls
// so document text never becomes HTML.
//
// A client that joined read-only gets a locked box instead: the
// textarea is read-only and the format buttons are disabled, so the
// viewer can watch but not type.
//
// The textarea counts caret positions in UTF-16 units; the protocol
// counts characters (code points), so the caret is converted both ways.
//
// `bindDocList` renders the documents list on the home page: one row
// per document with its title, preview, version and last save, an Open
// button, and a button that copies a read-only share link.

import type { CollabClient } from "./client.js";
import { chars, shiftPosition } from "./editor.js";
import type { Selection } from "./protocol.js";

/** Code-point offset of a UTF-16-unit offset in `text`. */
export function unitsToCodePoints(text: string, units: number): number {
  return chars(text.slice(0, units)).length;
}

/** UTF-16-unit offset of a code-point offset in `text`. */
export function codePointsToUnits(text: string, codePoints: number): number {
  return chars(text).slice(0, codePoints).join("").length;
}

/** The 1-based line and column of a code-point offset. */
export function lineCol(text: string, pos: number): { line: number; col: number } {
  const before = chars(text).slice(0, pos).join("");
  const lines = before.split("\n");
  return { line: lines.length, col: chars(lines[lines.length - 1] ?? "").length + 1 };
}

/** A share link for a document: this page with the room set, and
 * `mode=read` on a read-only link. The user's name, if any, carries
 * over, so a shared link keeps who they are. */
export function docUrl(roomId: string, readOnly: boolean): string {
  const next = new URLSearchParams(window.location.search);
  next.set("room", roomId);
  if (readOnly) next.set("mode", "read");
  else next.delete("mode");
  return `${window.location.origin}${window.location.pathname}?${next.toString()}`;
}

/** Copies `text` and briefly shows "Copied!" on the button. Falls
 * back to a hidden textarea when the clipboard API is unavailable. */
export async function copyText(text: string, button: HTMLButtonElement): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const scratch = document.createElement("textarea");
    scratch.value = text;
    document.body.appendChild(scratch);
    scratch.select();
    document.execCommand("copy");
    scratch.remove();
  }
  const label = button.textContent;
  button.textContent = "Copied!";
  setTimeout(() => {
    button.textContent = label;
  }, 1500);
}

/** "Nov 16, 2026, 8:04 AM", or "not saved yet" for a room that has
 * never been written to disk. */
export function formatUpdated(updatedAt: number | null): string {
  if (updatedAt === null) return "not saved yet";
  return new Date(updatedAt * 1000).toLocaleString();
}

/** Opens a document: this page with the room set and read mode off. */
function openRoom(id: string): void {
  const next = new URLSearchParams(window.location.search);
  next.set("room", id);
  next.delete("mode");
  window.location.search = next.toString();
}

/** Renders the documents list into `listEl` and wires the form that
 * creates a new document by name. Re-renders whenever the client
 * reports a new list. */
export function bindDocList(
  listEl: HTMLElement,
  form: HTMLFormElement,
  nameInput: HTMLInputElement,
  client: CollabClient,
): void {
  const render = () => {
    listEl.innerHTML = "";
    if (client.docs.length === 0) {
      listEl.textContent = "No documents yet — create one below.";
      return;
    }
    for (const doc of client.docs) {
      const row = document.createElement("div");
      row.className = "doc";
      const title = document.createElement("strong");
      title.textContent = doc.title;
      row.appendChild(title);
      const preview = document.createElement("div");
      preview.className = "preview";
      preview.textContent = doc.preview === "" ? "Empty document" : doc.preview;
      row.appendChild(preview);
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = `v${doc.version} · ${formatUpdated(doc.updated_at)}`;
      row.appendChild(meta);
      const open = document.createElement("button");
      open.type = "button";
      open.textContent = "Open";
      open.addEventListener("click", () => openRoom(doc.id));
      row.appendChild(open);
      const copy = document.createElement("button");
      copy.type = "button";
      copy.textContent = "Copy read-only link";
      copy.addEventListener("click", () => void copyText(docUrl(doc.id, true), copy));
      row.appendChild(copy);
      listEl.appendChild(row);
    }
  };
  client.onChange = (change) => {
    if (change.kind === "docs") render();
  };
  render();
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const id = nameInput.value.trim();
    if (id === "") return;
    openRoom(id);
  });
}

function selectionOf(area: HTMLTextAreaElement): Selection | null {
  const start = unitsToCodePoints(area.value, area.selectionStart ?? 0);
  const end = unitsToCodePoints(area.value, area.selectionEnd ?? 0);
  if (start === end) return null;
  return { anchor: start, head: end };
}

export function bindEditor(
  area: HTMLTextAreaElement,
  statusEl: HTMLElement,
  presenceEl: HTMLElement,
  client: CollabClient,
  formatBar: HTMLElement,
  previewEl: HTMLElement,
): void {
  const renderPreview = () => {
    previewEl.innerHTML = "";
    for (const span of client.editor.spans()) {
      let node: Node = document.createTextNode(span.text);
      if (span.attrs.italic) {
        const em = document.createElement("em");
        em.appendChild(node);
        node = em;
      }
      if (span.attrs.bold) {
        const strong = document.createElement("strong");
        strong.appendChild(node);
        node = strong;
      }
      if (span.attrs.link !== null && /^https?:\/\//.test(span.attrs.link)) {
        const link = document.createElement("a");
        link.href = span.attrs.link;
        link.target = "_blank";
        link.rel = "noopener";
        link.appendChild(node);
        node = link;
      }
      const wrapper = document.createElement("span");
      wrapper.appendChild(node);
      previewEl.appendChild(wrapper);
    }
    if (client.editor.text === "") {
      previewEl.textContent = "Nothing here yet — type above, then bold it, italicize it, or link it.";
    }
  };
  const renderPresence = () => {
    const others = [...client.presences.values()].sort((a, b) => a.client_id - b.client_id);
    presenceEl.innerHTML = "";
    for (const p of others) {
      const { line, col } = lineCol(client.editor.text, p.cursor);
      const row = document.createElement("div");
      row.className = "peer";
      const dot = document.createElement("span");
      dot.className = "dot";
      dot.style.background = p.color;
      row.appendChild(dot);
      row.appendChild(
        document.createTextNode(`${p.name ?? `user ${p.client_id}`} — line ${line}, col ${col}`),
      );
      presenceEl.appendChild(row);
    }
    if (others.length === 0) {
      presenceEl.textContent = "No one else is here yet.";
    }
  };

  const renderStatus = () => {
    const state =
      client.status === "connected"
        ? "connected"
        : client.status === "connecting"
          ? "connecting…"
          : client.status === "reconnecting"
            ? "reconnecting…"
            : client.status === "offline"
              ? "offline — edits saved on this device"
              : "disconnected";
    const unsent = client.pendingCount > 0 ? ` · ${client.pendingCount} unsent` : "";
    const mode = client.readOnly ? " · read-only" : "";
    statusEl.textContent = client.lastError
      ? `${state} · room ${client.room} · v${client.editor.version}${unsent}${mode} · ${client.lastError}`
      : `${state} · room ${client.room} · v${client.editor.version}${unsent}${mode}`;
  };

  /** A viewer gets a locked box: read-only text and no format buttons. */
  const applyMode = () => {
    area.readOnly = client.readOnly;
    formatBar.querySelectorAll("button").forEach((button) => {
      button.disabled = client.readOnly;
    });
  };

  const renderAll = () => {
    renderStatus();
    renderPresence();
    renderPreview();
    applyMode();
  };

  /** The highlighted range in code-point positions, or null when the
   * selection is empty: formatting needs something to format. */
  const selectionRange = (): { start: number; end: number } | null => {
    const start = unitsToCodePoints(area.value, area.selectionStart ?? 0);
    const end = unitsToCodePoints(area.value, area.selectionEnd ?? 0);
    if (start === end) return null;
    return { start, end };
  };

  const toggleAttr = (which: "bold" | "italic") => {
    const range = selectionRange();
    if (range === null) return;
    const all = client.editor.attrs
      .slice(range.start, range.end)
      .every((a) => a[which]);
    client.formatRange(range.start, range.end - range.start, { [which]: !all });
    area.focus();
  };

  const setLink = () => {
    const range = selectionRange();
    if (range === null) return;
    const current = client.editor.attrs[range.start]?.link ?? "";
    const url = window.prompt("Link URL (empty to remove the link):", current || "https://");
    if (url === null) return;
    const trimmed = url.trim();
    client.formatRange(range.start, range.end - range.start, {
      link: trimmed === "" ? null : trimmed,
    });
    area.focus();
  };

  formatBar.querySelector("#boldBtn")?.addEventListener("click", () => toggleAttr("bold"));
  formatBar.querySelector("#italicBtn")?.addEventListener("click", () => toggleAttr("italic"));
  formatBar.querySelector("#linkBtn")?.addEventListener("click", setLink);

  const reportCaret = () => {
    if (client.status !== "connected") return;
    const cursor = unitsToCodePoints(area.value, area.selectionStart ?? 0);
    client.sendPresence(cursor, selectionOf(area));
  };

  area.addEventListener("input", () => {
    // Local echo: the box already shows the new text; the client applies
    // it to its own copy and sends the op behind it.
    client.localEdit(area.value);
    reportCaret();
    renderAll();
  });
  area.addEventListener("keyup", reportCaret);
  area.addEventListener("click", reportCaret);
  area.addEventListener("select", reportCaret);

  client.onChange = (change) => {
    if (change.kind === "remote" || change.kind === "joined") {
      if (area.value !== client.editor.text) {
        // Keep the caret on the same text: shift it past each remote op.
        let start = unitsToCodePoints(area.value, area.selectionStart ?? 0);
        let end = unitsToCodePoints(area.value, area.selectionEnd ?? 0);
        for (const op of change.ops ?? []) {
          start = shiftPosition(start, op);
          end = shiftPosition(end, op);
        }
        area.value = client.editor.text;
        const startUnits = codePointsToUnits(area.value, start);
        const endUnits = codePointsToUnits(area.value, end);
        area.setSelectionRange(startUnits, endUnits);
      }
    }
    renderAll();
  };
  client.onError = () => renderAll();
}
