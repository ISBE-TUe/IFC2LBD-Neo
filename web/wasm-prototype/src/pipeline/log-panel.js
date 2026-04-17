// ---------------------------------------------------------------------------
// log-panel.js — Bottom collapsible log panel
// ---------------------------------------------------------------------------

let logEl = null;
let toggleEl = null;
let collapsed = false;

export function initLogPanel() {
  logEl = document.querySelector("#log-output");
  toggleEl = document.querySelector("#log-toggle");

  toggleEl?.addEventListener("click", () => {
    collapsed = !collapsed;
    logEl?.classList.toggle("collapsed", collapsed);
    toggleEl.textContent = collapsed ? "▸ Log" : "▾ Log";
  });
}

export function log(line) {
  if (!logEl) return;
  const entry = document.createElement("div");
  entry.className = "log-entry";
  const ts = document.createElement("span");
  ts.className = "log-ts";
  ts.textContent = new Date().toISOString().slice(11, 23);
  entry.appendChild(ts);
  const msg = document.createElement("span");
  msg.textContent = ` ${line}`;
  entry.appendChild(msg);
  logEl.appendChild(entry);
  logEl.scrollTop = logEl.scrollHeight;
}
