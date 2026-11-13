// Binds a <textarea> to a CollabClient: typing edits locally at once
// (local echo) and sends the op, remote ops rewrite the box while keeping
// the caret on the same text, and everyone else's cursor shows up in the
// presence list with their colour.
//
// The textarea counts caret positions in UTF-16 units; the protocol
// counts characters (code points), so the caret is converted both ways.

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
): void {
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
          : "disconnected";
    statusEl.textContent = client.lastError
      ? `${state} · room ${client.room} · v${client.editor.version} · ${client.lastError}`
      : `${state} · room ${client.room} · v${client.editor.version}`;
  };

  const renderAll = () => {
    renderStatus();
    renderPresence();
  };

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
