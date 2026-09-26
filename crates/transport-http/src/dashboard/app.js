// Dashboard for onchain-data-mcp. Vanilla JS, no external resources (works offline).
// All data comes from /admin/api/* with the dashboard password + X-BDM-Admin header.
// DOM is built with textContent only (no innerHTML). The CSP forbids inline styles, so never
// set a `style` attribute: use classes, or CSSOM (`el.style.x = …`) which the CSP allows.
"use strict";

// ------------------------------------------------------------------ utilities

const $ = (sel) => document.querySelector(sel);
const TOKEN_KEY = "bdm_admin_token";

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "style") throw new Error("style attributes are blocked by the CSP; use a class");
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (k === "text") el.textContent = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

const SVG = "http://www.w3.org/2000/svg";
function s(tag, attrs, ...kids) {
  const el = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs || {})) el.setAttribute(k, v);
  for (const kid of kids) el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  return el;
}

// Lucide-style 24px paths.
const ICONS = {
  refresh: "M21 12a9 9 0 0 0-9-9 9.75 9.75 0 0 0-6.74 2.74L3 8M3 3v5h5M3 12a9 9 0 0 0 9 9 9.75 9.75 0 0 0 6.74-2.74L21 16M16 16h5v5",
  up: "M12 19V5M5 12l7-7 7 7",
  down: "M12 5v14M19 12l-7 7-7-7",
  x: "M18 6 6 18M6 6l12 12",
  check: "M20 6 9 17l-5-5",
  copy: "M8 8h12v12H8zM16 8V4H4v12h4",
  lock: "M5 11h14v10H5zM8 11V7a4 4 0 0 1 8 0v4",
  ext: "M15 3h6v6M10 14 21 3M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6",
  grip: "M9 5h.01M9 12h.01M9 19h.01M15 5h.01M15 12h.01M15 19h.01",
  alert: "M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z",
  play: "M6 4v16l14-8z",
  pause: "M6 4h4v16H6zM14 4h4v16h-4z",
  map: "M3 6l6-3 6 3 6-3v15l-6 3-6-3-6 3zM9 3v15M15 6v15",
  dash: "M3 3h7v9H3zM14 3h7v5h-7zM14 12h7v9h-7zM3 16h7v5H3z",
  key: "M21 2l-9.6 9.6M15.5 7.5l3 3L22 7l-3-3M7.5 21a5.5 5.5 0 1 0 0-11 5.5 5.5 0 0 0 0 11z",
  route: "M6 3v12M18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM6 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM18 9a9 9 0 0 1-9 9",
  wrench: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.8-3.8a6 6 0 0 1-7.9 7.9l-6.9 6.9a2.1 2.1 0 0 1-3-3l6.9-6.9a6 6 0 0 1 7.9-7.9z",
  users: "M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM22 21v-2a4 4 0 0 0-3-3.9M16 3.1a4 4 0 0 1 0 7.8",
  terminal: "M4 17l6-6-6-6M12 19h8",
  menu: "M4 6h16M4 12h16M4 18h16",
  logout: "M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9",
  download: "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3",
  activity: "M22 12h-4l-3 9L9 3l-3 9H2",
};
function icon(name) {
  return s("svg", { class: "i", viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", "stroke-width": "1.5", "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true" },
    s("path", { d: ICONS[name] }));
}

const store = {
  get(k) { try { return sessionStorage.getItem(k); } catch { return null; } },
  set(k, v) { try { sessionStorage.setItem(k, v); } catch { /* private mode */ } },
  del(k) { try { sessionStorage.removeItem(k); } catch { /* ignore */ } },
  getLocal(k) { try { return localStorage.getItem(k); } catch { return null; } },
  setLocal(k, v) { try { localStorage.setItem(k, v); } catch { /* ignore */ } },
};

const fmtN = (n) => (n === null || n === undefined ? "—" : Number(n).toLocaleString());
/** Local time as `2026-09-25 23:26:53` (24-hour, same everywhere on the dashboard). */
function stamp(d) {
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
const fmtT = (t) => (t ? stamp(new Date(t)) : "—");
const fmtD = (t) => (t ? stamp(new Date(t)).slice(0, 10) : "—");
const clamp = (t, n) => (t && t.length > n ? `${t.slice(0, n - 1).trimEnd()}…` : t || "");
const hosted = () => !!cache.config && cache.config.mode === "hosted";

function say(text, kind = "info") {
  const m = h("div", { class: `toast ${kind}` }, h("span", { class: "toast-k" }, kind === "err" ? "[ERR]" : kind === "ok" ? "[OK]" : "[SYS]"), " ", text);
  $("#status").append(m);
  setTimeout(() => m.remove(), kind === "err" ? 12000 : 5000);
}

function badge(text, cls = "") { return h("span", { class: `badge ${cls}` }, text); }

function stateBadge(state, until) {
  const label = { ok: "ok", warning: "warning", reserve: "reserve reached", exhausted: "exhausted" }[state] || state;
  return badge(until && state === "exhausted" ? `exhausted until ${fmtT(until)}` : label, state);
}

const SOURCE_LABEL = { vendor_api: "vendor API", headers: "headers", estimated: "estimated" };

function lockNote(env) {
  return env ? h("span", { class: "lock", title: `Set by environment variable ${env}` }, icon("lock"), `locked by env (${env})`) : null;
}

function extLink(href, text) {
  return h("a", { href, target: "_blank", rel: "noopener noreferrer", class: "ext" }, text, icon("ext"), h("span", { class: "sr-only" }, " (opens in a new tab)"));
}

function emptyState(text) { return h("div", { class: "empty" }, h("span", { "aria-hidden": "true" }, "// "), text); }

function btn(label, onclick, { cls = "", ico = null, type = "button", aria = null, disabled = false, title = null } = {}) {
  return h("button", { type, class: `cut btn ${cls}`, onclick, "aria-label": aria, disabled: disabled ? true : null, title }, ico ? icon(ico) : null, label);
}

/** Toggle switch: a real checkbox (keyboard + screen reader) with a styled track. */
function toggle(id, label, checked, { disabled = false, onchange } = {}) {
  const box = h("input", { type: "checkbox", id, class: "sw-in", checked: checked ? true : null, disabled: disabled ? true : null });
  if (onchange) box.addEventListener("change", () => onchange(box.checked));
  return h("label", { class: "sw", for: id }, box, h("span", { class: "sw-track", "aria-hidden": "true" }), h("span", { class: "sw-label" }, label));
}

/** Chamfered input wrapper with the terminal `>` prefix. */
function field(input) { return h("div", { class: "cut in" }, h("span", { class: "in-pfx", "aria-hidden": "true" }, ">"), input); }

function glitch(tag, text, attrs = {}) { return h(tag, { ...attrs, class: `glitch ${attrs.class || ""}`, "data-text": text }, text); }

/** Page header: glitched h1 + typewriter subtitle with a trailing cursor. */
function pageHead(title, sub, ...actions) {
  const typer = h("span", { class: "typer" }, sub);
  typer.style.setProperty("--n", String(sub.length)); // CSSOM: allowed by the CSP
  return h("header", { class: "page-head" },
    h("div", {}, h("p", { class: "crumb", "aria-hidden": "true" }, `sys://${cache.page || ""}`), glitch("h1", title, { tabindex: "-1" }), h("p", { class: "sub" }, typer)),
    actions.length ? h("div", { class: "head-actions" }, actions) : null);
}

function card(title, ...kids) {
  return h("section", { class: "cut card" }, title ? h("h2", { class: "card-title" }, title) : null, kids);
}

async function copyText(text) {
  try { await navigator.clipboard.writeText(text); say("Copied to clipboard", "ok"); }
  catch { say("Copy failed; select the text manually", "err"); }
}

// ------------------------------------------------------------------ API

class Unauthorized extends Error {}

function headers(json) {
  const hd = { Authorization: `Bearer ${store.get(TOKEN_KEY) || ""}`, "X-BDM-Admin": "1" };
  if (json) hd["Content-Type"] = "application/json";
  return hd;
}

async function api(path, { method = "GET", body } = {}) {
  const r = await fetch(path, { method, headers: headers(body !== undefined), body: body === undefined ? undefined : JSON.stringify(body) });
  if (r.status === 401) throw new Unauthorized();
  const text = await r.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = { raw: text }; }
  if (!r.ok) {
    const err = new Error(describeError(data, r.status));
    err.data = data;
    throw err;
  }
  return data;
}

function describeError(data, status) {
  if (data && Array.isArray(data.errors)) return data.errors.map((i) => `${i.path}: ${i.message}`).join("; ");
  if (data && data.error) return `${data.error.code}: ${data.error.message}`;
  return `HTTP ${status}`;
}

/** Apply config edits. Validation errors go to `errEl` (next to the field) when given, else a toast. */
async function edit(edits, okText = "Saved", { errEl = null, rerender = true } = {}) {
  if (errEl) errEl.textContent = "";
  try {
    const r = await api("/admin/api/config", { method: "PUT", body: { edits } });
    say(okText + (r.warnings && r.warnings.length ? ` (${r.warnings.length} warning(s): ${r.warnings.map((w) => w.message).join("; ")})` : ""), "ok");
    if (rerender) await render();
    return true;
  } catch (e) {
    if (errEl && e.data && Array.isArray(e.data.errors)) errEl.textContent = e.data.errors.map((i) => i.message).join("; ");
    else handle(e);
    return false;
  }
}

function handle(e) {
  if (e instanceof Unauthorized) showLogin("Wrong or expired dashboard password.");
  else say(e.message || String(e), "err");
}

async function download(path, filename) {
  try {
    const r = await fetch(path, { headers: headers(false) });
    if (r.status === 401) throw new Unauthorized();
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const url = URL.createObjectURL(await r.blob());
    const a = h("a", { href: url, download: filename });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } catch (e) { handle(e); }
}

// ------------------------------------------------------------------ shell

const NAV = [
  { id: "setup", label: "Setup guide", icon: "map" },
  { id: "overview", label: "Overview", icon: "dash" },
  { id: "providers", label: "Providers", icon: "key" },
  { id: "routing", label: "Routing", icon: "route" },
  { id: "tools", label: "Tools & Chains", icon: "wrench" },
  { id: "clients", label: "Clients", icon: "users", hostedOnly: true },
  { id: "connect", label: "Connect", icon: "terminal" },
];
const PAGES = { setup, overview, providers, routing, tools, clients, connect };
const ALIAS = { quota: "providers", vendors: "providers", chains: "tools" }; // old bookmarks
let streamAbort = null;
const cache = { config: null };

function stopStream() { if (streamAbort) { streamAbort.abort(); streamAbort = null; } }

function showLogin(msg) {
  stopStream();
  store.del(TOKEN_KEY);
  cache.config = null;
  $("#app").hidden = true;
  $("#view").replaceChildren();
  $("#login").hidden = false;
  $("#login-err").textContent = msg || "";
  document.title = "Sign in · onchain-data-mcp";
  $("#token").focus();
}

function anyKeySet(cfg) { return cfg.vendors.some((v) => v.keys.some((k) => k.set)); }

function currentPage() {
  let raw = (location.hash || "").slice(1).split("/")[0];
  if (ALIAS[raw]) { raw = ALIAS[raw]; history.replaceState(null, "", `#${raw}`); }
  if (raw === "clients" && cache.config && !hosted()) { history.replaceState(null, "", "#overview"); return "overview"; }
  if (PAGES[raw]) return raw;
  if (cache.config && !anyKeySet(cache.config) && !store.getLocal("bdm_setup_done")) {
    history.replaceState(null, "", "#setup/1"); // pin it so saving the first key doesn't leave the guide
    return "setup";
  }
  return "overview";
}

function buildNav() {
  $("#nav").replaceChildren(...NAV.map((n) => h("li", { "data-page": n.id },
    h("a", { href: `#${n.id}` }, icon(n.icon), h("span", {}, n.label)))));
}

async function render() {
  stopStream();
  if (!store.get(TOKEN_KEY)) return showLogin();
  $("#login").hidden = true;
  $("#app").hidden = false;
  const view = $("#view");
  if (!view.children.length) view.replaceChildren(h("p", { class: "loading" }, "> loading", h("span", { class: "cursor", "aria-hidden": "true" })));
  try {
    cache.config = await api("/admin/api/config");
    const page = currentPage();
    cache.page = page;
    for (const li of document.querySelectorAll("#nav li")) {
      const item = NAV.find((n) => n.id === li.dataset.page);
      li.hidden = !!(item && item.hostedOnly && !hosted());
      const a = li.querySelector("a");
      if (li.dataset.page === page) a.setAttribute("aria-current", "page");
      else a.removeAttribute("aria-current");
    }
    const label = (NAV.find((n) => n.id === page) || { label: page }).label;
    document.title = `${label} · onchain-data-mcp`;
    const mode = $("#mode");
    mode.textContent = hosted() ? "hosted" : "self-hosted";
    mode.className = `tag ${hosted() ? "hosted" : ""}`;
    view.replaceChildren(await PAGES[page]());
  } catch (e) {
    if (e instanceof Unauthorized) return handle(e);
    view.replaceChildren(card("Signal lost",
      h("p", { class: "field-err" }, `Could not load this page: ${e.message || e}`),
      btn("Retry", () => render(), { ico: "refresh" })));
  }
}

function lockedPath(prefix) {
  const hit = (cache.config.locked || []).find((l) => l.path === prefix || l.path.startsWith(prefix + ".") || prefix.startsWith(l.path + "."));
  return hit ? hit.env : null;
}

/** Accessible tab bar (arrow keys move + activate). `onPick(id)` swaps the panel. */
function tabBar(label, tabs, active, onPick, panelId) {
  const pick = (id, focus) => {
    onPick(id);
    for (const b of bar.children) {
      const on = b.dataset.id === id;
      b.setAttribute("aria-selected", String(on));
      b.tabIndex = on ? 0 : -1;
      if (on && focus) b.focus();
    }
  };
  const bar = h("div", { class: "tabs", role: "tablist", "aria-label": label },
    tabs.map((t) => h("button", { type: "button", role: "tab", id: `tab-${panelId}-${t.id}`, "data-id": t.id, "aria-selected": String(t.id === active), "aria-controls": panelId, tabindex: t.id === active ? "0" : "-1",
      onclick: () => pick(t.id, false),
      onkeydown: (e) => {
        const i = tabs.findIndex((x) => x.id === e.currentTarget.dataset.id);
        const j = e.key === "ArrowRight" ? (i + 1) % tabs.length : e.key === "ArrowLeft" ? (i - 1 + tabs.length) % tabs.length : e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1 : -1;
        if (j >= 0) { e.preventDefault(); pick(tabs[j].id, true); }
      } }, t.label)));
  return bar;
}

// ------------------------------------------------------------------ charts + meters

function meter(pct, state, label) {
  const bar = h("span", { class: "meter-fill" });
  bar.style.width = `${Math.min(100, Math.max(0, pct))}%`; // CSSOM, not a style attribute
  return h("div", { class: "meter-row" },
    h("div", { class: `meter ${state || ""}`, role: "meter", "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": String(Math.min(100, pct)), "aria-label": label }, bar),
    h("span", { class: "meter-txt" }, label));
}

/** 30-day bar chart. `series` = [[day, n], …]; optional dashed `budget` line. */
function barChart(series, { label, unit, budget = null, compact = false }) {
  // The SVG stretches to its box (preserveAspectRatio none), so every label is HTML: it stays
  // readable at any width. Lines use non-scaling strokes (CSS).
  const W = 300, H = 100;
  const max = Math.max(1, ...series.map((d) => d[1]), budget || 0);
  const bw = W / Math.max(1, series.length);
  const total = series.reduce((a, d) => a + d[1], 0);
  const peak = series.reduce((a, d) => (d[1] > a[1] ? d : a), series[0] || ["—", 0]);
  const y = (n) => H - ((H - 4) * n) / max;
  const svg = s("svg", { viewBox: `0 0 ${W} ${H}`, preserveAspectRatio: "none", class: `chart${compact ? " compact" : ""}`, role: "img",
    "aria-label": `${label}: ${fmtN(total)} ${unit} over ${series.length} days, peak ${fmtN(peak[1])} on ${peak[0]}` });
  for (const f of [0.5, 1]) svg.append(s("line", { x1: "0", x2: String(W), y1: String(y(max * f)), y2: String(y(max * f)), class: "grid-line" }));
  series.forEach(([day, n], i) => {
    const bh = n === 0 ? 0.8 : Math.max(1.5, ((H - 4) * n) / max);
    svg.append(s("rect", { x: String(i * bw + bw * 0.15), y: String(H - bh), width: String(bw * 0.7), height: String(bh), class: n === 0 ? "bar zero" : "bar" },
      s("title", {}, `${day}: ${fmtN(n)} ${unit}`)));
  });
  if (budget) svg.append(s("line", { x1: "0", x2: String(W), y1: String(y(budget)), y2: String(y(budget)), class: "budget-line" }, s("title", {}, `daily effective budget ${fmtN(budget)}`)));
  const legend = h("div", { class: "chart-legend" },
    h("span", {}, "total ", h("b", {}, fmtN(total)), ` ${unit}`),
    h("span", {}, "peak ", h("b", {}, fmtN(peak[1])), ` · ${peak[0]}`),
    h("span", {}, "scale ", h("b", {}, fmtN(max))),
    budget ? h("span", { class: "lg-budget" }, "daily budget ", h("b", {}, fmtN(budget))) : null);
  const axis = series.length ? h("div", { class: "chart-axis", "aria-hidden": "true" }, h("span", {}, series[0][0]), h("span", {}, series[series.length - 1][0])) : null;
  const table = h("details", { class: "data-table" }, h("summary", {}, "Show data as a table"),
    h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, h("th", { scope: "col" }, "Day"), h("th", { scope: "col", class: "num" }, unit))),
      h("tbody", {}, series.map(([d, n]) => h("tr", {}, h("td", {}, d), h("td", { class: "num" }, fmtN(n))))))));
  return h("figure", { class: "chart-wrap" }, legend, svg, axis, table);
}

// ------------------------------------------------------------------ providers (vendors + quota)

const TIER_LABEL = {
  1: "TIER 1 · FREE · NO KEY",
  2: "TIER 2 · FREE KEY · BIG LIMIT",
  3: "TIER 3 · FREE KEY · SMALL LIMIT",
  4: "TIER 4 · PAID · OFF BY DEFAULT",
};
const tierOf = (v) => (TIER_LABEL[v.tier] ? v.tier : 4); // missing tier sorts last

function sortedVendors(vendors) {
  return vendors.filter((v) => v.id !== "rpc")
    .sort((a, b) => tierOf(a) - tierOf(b) || a.display_name.localeCompare(b.display_name));
}

const STATUS_OF = (v) => (v.status.status === "active" ? "active" : v.status.status === "missing_key" ? "missing" : "disabled");

function statusBadge(v) {
  const st = v.status.status;
  if (st === "active") return badge("active", "ok");
  if (st === "missing_key") return badge("needs key", "warn");
  return badge(v.status.unverified ? "disabled · unverified" : "disabled", "off");
}

function worstWindow(qv) {
  return qv ? qv.windows.filter((w) => w.used_pct !== null && w.used_pct !== undefined).sort((a, b) => b.used_pct - a.used_pct)[0] : null;
}

function vendorCard(v, qv) {
  const status = v.status.status;
  const keys = v.keys.map((k) => {
    const err = h("p", { class: "field-err", id: `err-${v.id}-${k.field}`, "aria-live": "polite" });
    const input = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false", id: `key-${v.id}-${k.field}`, placeholder: k.set ? "set (hidden) · paste to replace" : "paste key", disabled: k.locked_by ? true : null, "aria-describedby": err.id });
    return h("form", { class: "key-form", onsubmit: (e) => {
      e.preventDefault();
      if (!input.value) return;
      const value = input.value;
      input.value = ""; // never keep the key in the DOM
      edit([{ path: ["keys", v.id, k.field], value }], `${v.display_name} ${k.field} saved to secrets.toml`, { errEl: err });
    } },
      h("label", { for: input.id, class: "lbl" }, k.env, " ", badge(k.set ? "set" : "missing", k.set ? "ok" : "warn")),
      h("div", { class: "row nowrap" }, field(input), k.locked_by ? lockNote(k.locked_by) : btn("Save", null, { type: "submit", cls: "small", aria: `Save ${k.env}` })),
      err);
  });
  const result = h("div", { class: "test-out", "aria-live": "polite" });
  const test = async () => {
    result.replaceChildren(h("span", { class: "dim" }, "> probing…"));
    try {
      const r = await api(`/admin/api/vendors/${encodeURIComponent(v.id)}/test`, { method: "POST" });
      result.replaceChildren(r.ok ? badge(`OK · ${r.method} · ${r.latency_ms} ms`, "ok") : badge(`Failed: ${r.message}`, "bad"));
    } catch (e) { result.replaceChildren(); handle(e); }
  };
  const worst = worstWindow(qv);
  const usage = worst
    ? meter(worst.used_pct, worst.state, `${worst.kind.replace("_", " ")} · ${fmtN(worst.used)} / ${fmtN(worst.effective)} ${qv.unit} · ${worst.used_pct}%`)
    : h("p", { class: "dim small" }, qv && qv.windows.length ? "Rate limit only: not metered." : "No budget configured.");
  return h("article", { class: `cut card vcard tier-${tierOf(v)}`, "data-status": STATUS_OF(v), "aria-labelledby": `v-${v.id}` },
    h("header", { class: "vcard-head" },
      h("div", {}, h("h3", { id: `v-${v.id}` }, v.display_name), h("p", { class: "mono dim small" }, v.id)),
      h("div", { class: "badges" }, statusBadge(v), qv && qv.state !== "ok" ? stateBadge(qv.state, qv.exhausted_until) : null)),
    h("div", { class: "badges" },
      TIER_LABEL[v.tier] ? badge(TIER_LABEL[v.tier], `tier t${v.tier}`) : null,
      v.free_tier_verified ? null : badge("free tier unverified", "warn"),
      v.signup_url ? extLink(v.signup_url, "Get a key") : null),
    v.note ? h("p", { class: "note" }, v.note) : null,
    keys.length ? h("div", { class: "stack" }, keys) : h("p", { class: "dim small" }, "Keyless: no credentials needed."),
    h("div", { class: "usage" }, h("p", { class: "lbl" }, "Usage"), usage),
    h("div", { class: "vcard-foot" },
      toggle(`en-${v.id}`, "Enabled", v.enabled, { disabled: !!v.enabled_locked_by, onchange: (on) => edit([{ path: ["vendors", v.id, "enabled"], value: on }], `${v.display_name} ${on ? "enabled" : "disabled"}`) }),
      lockNote(v.enabled_locked_by),
      btn("Test", test, { cls: "small", ico: "play", disabled: status !== "active", aria: `Test ${v.display_name}`,
        title: status === "active" ? "One cheap call (usage endpoint, eth_blockNumber, getSlot or an FX rate)" : "Only active vendors can be tested" }),
      result),
    qv ? budgetDetails(qv) : null);
}

function budgetDetails(v) {
  const reserveLock = lockedPath(`vendors.${v.vendor}.reserve_pct`);
  const reserveInput = h("input", { type: "number", min: "0", max: "100", inputmode: "numeric", value: String(v.reserve_pct), id: `res-${v.vendor}`, readonly: reserveLock ? true : null });
  const daily = v.windows.find((w) => w.kind === "daily");
  return h("details", { class: "budget" },
    h("summary", {}, "Budget, reserve & 30-day usage"),
    h("div", { class: "stack budget-body" },
    h("p", { class: "dim small" },
      `Unit: ${v.unit} · reset: ${v.reset.replace("_", " ")} · on exhausted: ${v.on_exhausted.replace("_", " ")} · alerts at ${v.alert_pct.join("%, ")}%`,
      v.plan ? ` · plan: ${v.plan}` : "", v.reported_at ? ` · vendor data ${fmtT(v.reported_at)}` : ""),
    v.report_error ? h("p", { class: "field-err" }, `Usage API error: ${v.report_error}`) : null,
    v.estimated_only ? h("p", { class: "dim small" }, "No usage API is confirmed for this vendor: numbers are local estimates (plus rate-limit headers when the vendor sends them).") : null,
    v.windows.length ? h("div", { class: "stack" }, v.windows.map((w) => windowBlock(v, w))) : h("p", { class: "dim small" }, "No limits configured and no usage yet."),
    h("form", { class: "row reserve", onsubmit: (e) => {
      e.preventDefault();
      edit([{ path: ["vendors", v.vendor, "reserve_pct"], value: Number(reserveInput.value) }], `Reserve for ${v.vendor} saved`);
    } },
      h("label", { for: reserveInput.id, class: "lbl" }, "Reserve %"), field(reserveInput),
      reserveLock ? lockNote(reserveLock) : btn("Save", null, { type: "submit", cls: "small", aria: `Save ${v.vendor} reserve` })),
    v.series && v.series.length ? barChart(v.series, { label: `${v.display_name} daily usage`, unit: v.unit, budget: daily && daily.effective, compact: true }) : null,
    v.breakdown ? breakdown(v.breakdown) : null));
}

function budgetInput(v, w, which) {
  const value = w[which];
  const locked = w[`${which}_locked_by`];
  const id = `${which}-${v.vendor}-${w.kind}`;
  const input = h("input", { type: "number", min: "0", inputmode: "numeric", id, value: value === null ? "" : String(value), placeholder: "none", readonly: locked ? true : null });
  const save = async (e) => {
    e.preventDefault();
    try {
      const raw = input.value.trim();
      const r = await api(`/admin/api/vendors/${encodeURIComponent(v.vendor)}/budget`, { method: "POST", body: { which, window: w.kind, value: raw === "" ? null : Number(raw) } });
      say(`${v.vendor} ${w.kind} ${which} saved` + (r.warnings.length ? ` (${r.warnings.map((x) => x.message).join("; ")})` : ""), "ok");
      render();
    } catch (err) { handle(err); }
  };
  return h("form", { class: "budget-in", onsubmit: save },
    h("label", { for: id, class: "lbl" }, which),
    h("div", { class: "row nowrap" }, field(input), locked ? lockNote(locked) : btn("Set", null, { type: "submit", cls: "small", aria: `Save ${v.vendor} ${w.kind} ${which}` })));
}

function windowBlock(v, w) {
  const alt = w.sources.filter((x) => x.source !== w.source).map((x) => `${SOURCE_LABEL[x.source]}: ${fmtN(x.used)}${x.comparable ? "" : " (other unit)"}`).join(", ");
  return h("div", { class: "window" },
    h("div", { class: "row spread" }, h("h4", {}, w.kind === "live" ? "live (headers)" : w.kind.replace("_", " ")), stateBadge(w.state, w.state === "exhausted" ? w.resets_at : null)),
    w.used === null ? h("p", { class: "dim small" }, "Not tracked (token bucket).") : h("div", {},
      w.used_pct !== null ? meter(w.used_pct, w.state, `${fmtN(w.used)} used · ${fmtN(w.remaining)} left`) : h("p", {}, `${fmtN(w.used)} used`)),
    h("dl", { class: "facts small" },
      h("dt", {}, "Effective"), h("dd", {}, fmtN(w.effective)),
      h("dt", {}, "Source"), h("dd", {}, w.source ? SOURCE_LABEL[w.source] : "—", alt ? ` (${alt})` : ""),
      h("dt", {}, "Burn / day"), h("dd", {}, fmtN(w.burn_per_day)),
      h("dt", {}, "Runs out"), h("dd", {}, w.runs_out_at ? fmtT(w.runs_out_at) : w.burn_per_day ? "not before reset" : "—"),
      h("dt", {}, "Resets"), h("dd", {}, fmtT(w.resets_at)),
      w.alerts.length ? [h("dt", {}, "Alerts"), h("dd", {}, `${w.alerts.join("%, ")}%`)] : null),
    w.kind === "live" ? null : h("div", { class: "row" }, budgetInput(v, w, "limit"), budgetInput(v, w, "cap")));
}

function breakdown(b) {
  const list = (title, rows) => h("div", {}, h("h4", {}, title),
    rows.length ? h("ol", { class: "rank" }, rows.map(([k, n]) => h("li", {}, h("span", { class: "mono" }, k || "-"), h("span", { class: "num" }, fmtN(n))))) : h("p", { class: "dim small" }, "none"));
  return h("div", {}, h("h4", {}, "This month by…"),
    h("div", { class: "breakdown" }, list("Tool", b.tools), list("Method", b.methods), list("Chain", b.chains), list("Client", b.clients)));
}

let providerFilter = "all";
function providerGrid(vendors, quotaBy) {
  const list = sortedVendors(vendors);
  const cards = list.map((v) => vendorCard(v, quotaBy[v.id]));
  const count = (st) => list.filter((v) => st === "all" || STATUS_OF(v) === st).length;
  const apply = () => {
    for (const c of cards) c.hidden = providerFilter !== "all" && c.dataset.status !== providerFilter;
    for (const b of filters.querySelectorAll("button")) b.setAttribute("aria-pressed", String(b.dataset.f === providerFilter));
  };
  const filters = h("div", { class: "filters", role: "group", "aria-label": "Filter providers" },
    [["all", "All"], ["active", "Active"], ["missing", "Needs key"], ["disabled", "Disabled"]].map(([f, label]) =>
      h("button", { type: "button", class: "chip", "data-f": f, "aria-pressed": "false", onclick: () => { providerFilter = f; apply(); } }, label, h("span", { class: "chip-n" }, String(count(f))))));
  const grid = h("div", { class: "grid cards" }, cards);
  apply();
  return h("div", {}, filters, list.length ? grid : emptyState("No vendors in the registry."));
}

async function providers() {
  const q = await api("/admin/api/quota");
  const quotaBy = Object.fromEntries(q.vendors.map((v) => [v.vendor, v]));
  const days = new Map();
  for (const v of q.vendors) for (const [d, n] of v.series || []) days.set(d, (days.get(d) || 0) + n);
  const combined = [...days.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1));
  const refresh = async () => {
    try { await api("/admin/api/quota/refresh", { method: "POST" }); say("Refreshed from vendor usage APIs", "ok"); render(); } catch (e) { handle(e); }
  };
  return h("div", { class: "page" },
    pageHead("Providers", "Keys, tiers, tests and quota for every data vendor.",
      btn("Refresh usage", refresh, { cls: "small", ico: "refresh" }),
      btn("Export CSV", () => download("/admin/api/quota.csv", "quota.csv"), { cls: "small ghost", ico: "download" })),
    card("Usage · last 30 days",
      h("p", { class: "dim small" }, "Sum of every vendor's local metering (credits and requests added together). Open a provider's budget panel for its own chart with the daily budget line. ",
        `Generated ${fmtT(q.generated_at)}.`),
      combined.some((d) => d[1] > 0) ? barChart(combined, { label: "All providers daily usage", unit: "units" }) : emptyState("No usage recorded yet.")),
    h("p", { class: "dim small legend" },
      "Keys are write-only: saved to config/secrets.toml (mode 0600) and never shown again. Effective budget = min(cap, limit × (1 − reserve)); routing skips a vendor once it is reached. Sources: ",
      badge("vendor API", "src"), " ", badge("headers", "src"), " ", badge("estimated", "src"), "."),
    providerGrid(cache.config.vendors, quotaBy));
}

// ------------------------------------------------------------------ routing

const MAIN_CAPS = ["evm_rpc", "solana_rpc", "price", "token_balances", "transfer_history", "swap_quote", "token_risk", "fx"];

function vendorHasKey(id) {
  const v = cache.config.vendors.find((x) => x.id === id);
  return !!v && v.keys.some((k) => k.set);
}
function vendorKeyless(id) {
  const v = cache.config.vendors.find((x) => x.id === id);
  return !v || !v.requires_key;
}

/** Reorderable vendor list for one capability. Returns the element with `.editor` = { edits(), preset(kind), dirty() }. */
function orderEditor(cap, view, chain, locked) {
  const cfg = cache.config;
  let items = [...view.vendors];
  const list = h("ol", { class: "order", "aria-label": `${cap} order` });
  const known = [...new Set([...view.registered, ...cfg.vendors.map((v) => v.id), ...cfg.custom_rpc.map((c) => c.name)])].sort();
  const addSel = h("select", { "aria-label": `Add vendor to ${cap}`, class: "sel small" });
  const move = (i, j, focusSel) => {
    const [m] = items.splice(i, 1); items.splice(j, 0, m); draw();
    if (focusSel) { const b = list.children[j] && list.children[j].querySelector(focusSel); if (b && !b.disabled) b.focus(); }
  };
  const draw = () => {
    list.replaceChildren(...items.map((v, i) => {
      const li = h("li", { draggable: locked ? null : "true", class: "order-item" },
        locked ? null : h("span", { class: "grip", "aria-hidden": "true" }, icon("grip")),
        h("span", { class: "pos" }, String(i + 1).padStart(2, "0")),
        h("span", { class: "name" }, v),
        !view.effective || view.registered.includes(v) ? null : badge("not registered", "warn"),
        vendorHasKey(v) ? badge("key", "src") : null,
        locked ? null : h("span", { class: "order-btns" },
          h("button", { type: "button", class: "ibtn up", "aria-label": `Move ${v} up`, disabled: i === 0 ? true : null, onclick: () => move(i, i - 1, ".up") }, icon("up")),
          h("button", { type: "button", class: "ibtn down", "aria-label": `Move ${v} down`, disabled: i === items.length - 1 ? true : null, onclick: () => move(i, i + 1, ".down") }, icon("down")),
          h("button", { type: "button", class: "ibtn danger", "aria-label": `Remove ${v}`, onclick: () => { items.splice(i, 1); draw(); } }, icon("x"))));
      li.addEventListener("dragstart", (e) => { li.classList.add("dragging"); e.dataTransfer.setData("text/plain", String(i)); e.dataTransfer.effectAllowed = "move"; });
      li.addEventListener("dragend", () => li.classList.remove("dragging"));
      li.addEventListener("dragover", (e) => { e.preventDefault(); li.classList.add("over"); });
      li.addEventListener("dragleave", () => li.classList.remove("over"));
      li.addEventListener("drop", (e) => {
        e.preventDefault();
        li.classList.remove("over");
        const from = Number(e.dataTransfer.getData("text/plain"));
        if (Number.isInteger(from) && from !== i) move(from, i);
      });
      return li;
    }));
    if (!items.length) list.append(h("li", { class: "dim small order-empty" }, "empty: save is disabled, use Reset to inherit"));
    addSel.replaceChildren(h("option", { value: "" }, "+ add vendor…"), ...known.filter((v) => !items.includes(v)).map((v) => h("option", { value: v }, v)));
  };
  addSel.addEventListener("change", () => { if (addSel.value) { items.push(addSel.value); draw(); } });
  draw();

  const base = chain ? ["routing", "chains", chain.id, cap] : ["routing", "defaults", cap];
  // Remove alias keys (e.g. routing.chains.base) so the canonical CAIP-2 key is the one that applies.
  const aliasClears = chain ? chain.aliases.map((a) => ({ path: ["routing", "chains", a, cap], value: null })) : [];
  const editor = {
    cap, locked,
    dirty: () => JSON.stringify(items) !== JSON.stringify(view.vendors),
    edits: () => (items.length ? [...aliasClears, { path: base, value: items }] : []),
    preset: (kind) => {
      if (locked) return;
      if (kind === "reset") { items = []; draw(); return; }
      const first = kind === "free" ? vendorKeyless : vendorHasKey;
      items = [...items.filter(first), ...items.filter((v) => !first(v))];
      draw();
    },
  };
  const save = () => {
    if (!items.length) return say("An order needs at least one vendor (use Reset to inherit instead).", "err");
    edit(editor.edits(), `${cap} order saved; routing table swapped`);
  };
  const reset = () => edit([...aliasClears, { path: base, value: null }], `${cap} override removed`);
  const canReset = chain ? view.level === "chain" : view.level === "default";

  const eff = view.effective
    ? h("div", { class: "eff" }, h("span", { class: "lbl" }, "Effective"),
        view.effective.length ? view.effective.map((r) => badge(r.usable ? r.vendor : `${r.vendor}: ${r.reason}`, r.usable ? "ok" : "bad")) : h("span", { class: "dim" }, "none"))
    : h("p", { class: "dim small" }, "Chain-bound capability: see the per-chain tabs for the effective order.");
  const el = h("section", { class: "cut card cap" },
    h("header", { class: "row spread" },
      h("h3", {}, cap, " ", badge(`from ${view.level.replace("_", "-")}`, "src")),
      locked ? lockNote(locked) : null),
    list, eff,
    view.warnings.length ? h("ul", { class: "warns" }, view.warnings.map((w) => h("li", { class: "lock" }, icon("alert"), w))) : null,
    locked ? null : h("div", { class: "row cap-foot" }, addSel,
      btn("Save", save, { cls: "small", aria: `Save ${cap} order` }),
      canReset ? btn("Reset", reset, { cls: "small ghost", aria: `Reset ${cap} to inherited` }) : null));
  el.editor = editor;
  return el;
}

/** Tabs (Defaults + one per chain) of order editors. `caps` limits the capabilities shown. */
let routingTab = "default";
function routingEditor(caps) {
  const cfg = cache.config;
  const chains = cfg.chains.filter((c) => c.enabled);
  const tabs = [{ id: "default", label: "Defaults" }, ...chains.map((c) => ({ id: c.id, label: c.name }))];
  if (!tabs.some((t) => t.id === routingTab)) routingTab = "default";
  const panel = h("div", { role: "tabpanel", id: "routing-panel", tabindex: "0" });
  let editors = [];
  const fill = () => {
    const chain = chains.find((c) => c.id === routingTab);
    panel.setAttribute("aria-labelledby", `tab-routing-panel-${routingTab}`);
    editors = [];
    const grid = h("div", { class: "grid caps" });
    for (const cap of cfg.capabilities) {
      if (caps && !caps.includes(cap)) continue;
      const o = cfg.orders[cap];
      const view = chain ? o.chains[chain.id] : o.default;
      if (!view) continue;
      const ed = orderEditor(cap, view, chain, chain ? view.locked_by : o.default_locked_by);
      editors.push(ed);
      grid.append(ed);
    }
    panel.replaceChildren(editors.length ? grid : emptyState("No capabilities apply to this scope."));
  };
  fill();
  const applyAll = (kind) => editors.forEach((e) => e.editor.preset(kind));
  const saveAll = async () => {
    const edits = editors.filter((e) => e.editor.dirty()).flatMap((e) => e.editor.edits());
    if (!edits.length) return say("Nothing changed.", "info");
    await edit(edits, "Routing saved; routing table swapped");
  };
  const resetAll = async () => {
    const chain = chains.find((c) => c.id === routingTab);
    const edits = editors.filter((e) => !e.editor.locked).flatMap((e) => {
      const cap = e.editor.cap;
      const base = chain ? ["routing", "chains", chain.id, cap] : ["routing", "defaults", cap];
      const aliases = chain ? chain.aliases.map((a) => ({ path: ["routing", "chains", a, cap], value: null })) : [];
      return [...aliases, { path: base, value: null }];
    });
    await edit(edits, "Built-in order restored for this scope");
  };
  const presets = h("div", { class: "presets" },
    h("span", { class: "lbl" }, "Presets"),
    btn("Free-first", () => applyAll("free"), { cls: "small ghost" }),
    btn("Keys-first", () => applyAll("keys"), { cls: "small ghost" }),
    btn("Reset to built-in", resetAll, { cls: "small ghost" }),
    btn("Save all changes", saveAll, { cls: "small solid" }));
  return h("div", {}, tabBar("Routing scope", tabs, routingTab, (id) => { routingTab = id; fill(); }, "routing-panel"), presets, panel);
}

async function routing() {
  return h("div", { class: "page" },
    pageHead("Routing", "Primary first, then fallbacks. Drag or use the arrows."),
    h("p", { class: "dim small legend" }, "Most specific wins: operation > chain > defaults > built-in. Presets reorder every list in the tab; nothing is saved until you press Save. The effective row shows what routing uses right now."),
    routingEditor(null));
}

// ------------------------------------------------------------------ tools & chains

const STRATEGIES = ["", "failover", "quorum", "aggregate", "fan_out", "hedged"];

function toolsEditor() {
  const cfg = cache.config;
  const current = cfg.settings.server.tool_profile;
  const profileLock = lockedPath("server.tool_profile");
  const profile = h("select", { id: "profile", class: "sel", disabled: profileLock ? true : null },
    [...cfg.profiles, "all", "custom"].map((p) => h("option", { value: p, selected: current === p ? true : null }, p)));
  profile.addEventListener("change", () => edit([{ path: ["server", "tool_profile"], value: profile.value }], `Tool profile set to ${profile.value}`));
  const custom = current === "custom";
  const enabledTools = cfg.settings.server.enabled_tools || [];
  const visible = cfg.operations.filter((o) => o.visible).length;

  const toolRows = cfg.operations.map((op) => {
    const on = custom ? enabledTools.includes(op.name) : op.enabled;
    const change = (checked) => {
      if (custom) {
        const next = checked ? [...new Set([...enabledTools, op.name])] : enabledTools.filter((n) => n !== op.name);
        edit([{ path: ["server", "enabled_tools"], value: next }], `${op.name} ${checked ? "added to" : "removed from"} the custom profile`);
      } else {
        edit([{ path: ["operations", op.name, "enabled"], value: checked }], `${op.name} ${checked ? "enabled" : "disabled"}`);
      }
    };
    return h("li", { class: `tool ${op.visible ? "" : "off"}` },
      toggle(`op-${op.name}`, op.name, on, { disabled: !!op.locked_by, onchange: change }),
      h("div", { class: "tool-meta" },
        op.visible ? badge("exposed", "ok") : badge("hidden", "off"), badge(op.domain, "src"), lockNote(op.locked_by),
        h("p", { class: "desc", title: op.description }, clamp(op.description, 150)),
        h("p", { class: "dim small" }, op.profiles.length ? `profiles: ${op.profiles.join(", ")}` : "no profile")));
  });

  const advRows = cfg.operations.map((op) => {
    const locked = op.locked_by;
    const strat = h("select", { class: "sel small", "aria-label": `${op.name} strategy`, disabled: locked ? true : null },
      STRATEGIES.map((x) => h("option", { value: x, selected: (op.strategy || "") === x ? true : null }, x || "default")));
    strat.addEventListener("change", () => edit([{ path: ["operations", op.name, "strategy"], value: strat.value || null }], `${op.name} strategy saved`));
    const ttl = h("input", { type: "number", min: "0", inputmode: "numeric", "aria-label": `${op.name} cache TTL seconds`, value: op.cache_ttl_secs === null ? "" : String(op.cache_ttl_secs), placeholder: op.default_cache_ttl_secs === null ? "none" : `${op.default_cache_ttl_secs} (default)`, readonly: locked ? true : null });
    const saveTtl = () => edit([{ path: ["operations", op.name, "cache_ttl_secs"], value: ttl.value === "" ? null : Number(ttl.value) }], `${op.name} TTL saved`);
    return h("tr", {},
      h("th", { scope: "row" }, h("code", {}, op.name)),
      h("td", {}, strat),
      h("td", {}, h("div", { class: "row nowrap" }, field(ttl), locked ? lockNote(locked) : btn("Save", saveTtl, { cls: "small ghost", aria: `Save ${op.name} TTL` }))));
  });

  return h("div", { class: "stack" },
    h("div", { class: "row" }, h("label", { for: "profile", class: "lbl" }, "Tool profile"), profile, lockNote(profileLock), badge(`${visible} / ${cfg.operations.length} tools exposed`, "src")),
    h("p", { class: "dim small" }, custom
      ? "Custom profile: switch on the tools to expose (saved to server.enabled_tools)."
      : "A profile keeps each agent's tool list small. Switch a tool off to disable it everywhere (operations.<tool>.enabled). In hosted mode each client key's own profile applies."),
    cfg.operations.length ? h("ul", { class: "toollist" }, toolRows) : emptyState("No tools registered."),
    h("details", { class: "adv" }, h("summary", {}, "Advanced: strategy and cache TTL per tool"),
      h("div", { class: "scroll" }, h("table", {},
        h("thead", {}, h("tr", {}, ["Tool", "Strategy", "Cache TTL (s)"].map((t) => h("th", { scope: "col" }, t)))),
        h("tbody", {}, advRows)))));
}

function chainsTable() {
  const cfg = cache.config;
  const rows = cfg.chains.map((c) => {
    const key = c.override_key;
    const err = h("p", { class: "field-err", "aria-live": "polite" });
    const rpcs = h("textarea", { class: "ta", rows: String(Math.max(2, c.public_rpc.length)), "aria-label": `${c.name} public RPC URLs, one per line`, readonly: c.locked_by ? true : null, spellcheck: "false" }, c.public_rpc.join("\n"));
    const saveRpc = () => {
      const list = rpcs.value.split("\n").map((x) => x.trim()).filter(Boolean);
      edit([{ path: ["chain_overrides", key, "public_rpc"], value: list }], `${c.name} public RPCs saved`, { errEl: err });
    };
    return h("tr", {},
      h("th", { scope: "row" }, h("b", {}, c.name), h("div", { class: "mono dim small" }, c.id), c.aliases.length ? h("div", { class: "dim small" }, `aliases: ${c.aliases.join(", ")}`) : null),
      h("td", {}, badge(c.family, c.family === "evm" ? "src" : "sol")),
      h("td", {}, toggle(`ch-${key}`, "Enabled", c.enabled, { disabled: !!c.locked_by, onchange: (on) => edit([{ path: ["chain_overrides", key, "enabled"], value: on }], `${c.name} ${on ? "enabled" : "disabled"}`) }), lockNote(c.locked_by)),
      h("td", {}, c.finality.policy, c.finality.default ? ` (${c.finality.default})` : "", c.finality.note ? h("div", { class: "dim small" }, c.finality.note) : null),
      h("td", { class: "rpc-cell" }, rpcs, err, c.locked_by ? null : btn("Save RPCs", saveRpc, { cls: "small ghost", aria: `Save ${c.name} public RPCs` })),
      h("td", {}, c.explorer ? extLink(c.explorer, "explorer") : "—"));
  });
  return h("div", { class: "stack" },
    h("p", { class: "dim small" }, "Keyless public RPCs are the last-resort fallback. Keyed endpoints go in [custom_rpc] or vendor keys, so they stay out of this list."),
    cfg.custom_rpc.length ? h("p", { class: "row" }, h("span", { class: "lbl" }, "Custom RPC"), cfg.custom_rpc.map((c) => badge(`${c.name} → ${c.chain}`, "src"))) : null,
    rows.length ? h("div", { class: "scroll" }, h("table", { class: "chains" },
      h("thead", {}, h("tr", {}, ["Chain", "Family", "Enabled", "Finality", "Public RPCs", "Explorer"].map((t) => h("th", { scope: "col" }, t)))),
      h("tbody", {}, rows))) : emptyState("No chains in the registry."));
}

async function tools() {
  return h("div", { class: "page" },
    pageHead("Tools & Chains", "What agents can call, and on which networks."),
    card("Tools", toolsEditor()),
    card("Chains", chainsTable()));
}

// ------------------------------------------------------------------ connect

function snippet(title, text) {
  return h("div", { class: "cut term snippet" },
    h("div", { class: "term-bar" }, h("span", { class: "dots", "aria-hidden": "true" }, h("i"), h("i"), h("i")), h("h3", {}, title),
      btn("Copy", () => copyText(text), { cls: "small ghost", ico: "copy", aria: `Copy ${title}` })),
    h("pre", { tabindex: "0", "aria-label": title }, h("code", {}, text)));
}

let connectTab = "desktop";
async function connectPanel() {
  const c = await api("/admin/api/connect");
  const isHosted = c.mode === "hosted";
  const base = c.public_url || c.http_url;
  const tool = c.sample_tool || "<tool>";
  const NAME = "onchain-data";
  const stdio = { mcpServers: { [NAME]: { command: c.binary_path, args: ["--config-dir", c.config_dir] } } };
  const httpCfg = { mcpServers: { [NAME]: isHosted ? { url: c.mcp_url, headers: { Authorization: "Bearer <client key>" } } : { url: c.mcp_url } } };
  const q = (x) => `'${String(x).replace(/'/g, "'\\''")}'`;
  const curl = [`curl -X POST ${q(`${base}/v1/tools/${tool}`)}`, "  -H 'content-type: application/json'", isHosted ? "  -H 'Authorization: Bearer <client key>'" : null, "  -d '{}'"].filter(Boolean).join(" \\\n");
  const hostedNote = isHosted ? h("p", { class: "dim small" }, "Hosted: every client needs a key. ", h("a", { href: "#clients" }, "Create a client key"), ", then replace <client key>.") : null;
  const panes = {
    desktop: () => [
      h("p", { class: "dim small" }, "Claude Desktop → Settings → Developer → Edit Config. Paste into claude_desktop_config.json and restart Claude Desktop."),
      snippet("claude_desktop_config.json", JSON.stringify(stdio, null, 2))],
    code: () => [
      h("p", { class: "dim small" }, "Run one of these in a terminal. stdio starts the server per session; HTTP talks to this running server."),
      snippet("claude mcp add (stdio)", `claude mcp add ${NAME} -- ${q(c.binary_path)} --config-dir ${q(c.config_dir)}`),
      snippet("claude mcp add (HTTP)", `claude mcp add --transport http ${NAME} ${q(c.mcp_url)}${isHosted ? " --header 'Authorization: Bearer <client key>'" : ""}`), hostedNote],
    cursor: () => [
      h("p", { class: "dim small" }, "Paste into ~/.cursor/mcp.json (global) or .cursor/mcp.json in a project, then enable the server in Cursor Settings → MCP."),
      snippet("~/.cursor/mcp.json (stdio)", JSON.stringify(stdio, null, 2)),
      snippet("~/.cursor/mcp.json (HTTP)", JSON.stringify(httpCfg, null, 2)), hostedNote],
    http: () => [
      h("p", { class: "dim small" }, "Any MCP client that speaks streamable HTTP, or plain REST."),
      snippet("mcp.json (HTTP)", JSON.stringify(httpCfg, null, 2)),
      snippet(`curl /v1/tools/${tool}`, curl),
      h("p", { class: "dim small" }, "OpenAPI: ", h("code", {}, `${base}/openapi.json`), " · tool list: ", h("code", {}, `${base}/v1/tools`)), hostedNote],
  };
  const tabs = [{ id: "desktop", label: "Claude Desktop" }, { id: "code", label: "Claude Code" }, { id: "cursor", label: "Cursor" }, { id: "http", label: "HTTP / REST" }];
  const panel = h("div", { role: "tabpanel", id: "connect-panel", class: "stack", tabindex: "-1" });
  const fill = (id) => { connectTab = id; panel.setAttribute("aria-labelledby", `tab-connect-panel-${id}`); panel.replaceChildren(...panes[id]().filter(Boolean)); };
  fill(connectTab);
  return h("div", { class: "stack" },
    h("div", { class: "holo" },
      h("dl", { class: "facts" },
        h("dt", {}, "Mode"), h("dd", {}, isHosted ? "hosted" : "self-hosted"),
        h("dt", {}, "Tool profile"), h("dd", {}, c.tool_profile),
        h("dt", {}, "MCP (HTTP)"), h("dd", { class: "mono" }, c.mcp_url),
        h("dt", {}, isHosted ? "Public URL" : "HTTP"), h("dd", { class: "mono" }, base),
        h("dt", {}, "Binary"), h("dd", { class: "mono" }, c.binary_path),
        h("dt", {}, "Config dir"), h("dd", { class: "mono" }, c.config_dir))),
    tabBar("MCP client", tabs, connectTab, fill, "connect-panel"),
    panel);
}

async function connect() {
  return h("div", { class: "page" },
    pageHead("Connect", "Copy a snippet into your MCP client."),
    await connectPanel());
}

// ------------------------------------------------------------------ setup guide

const STEPS = [
  { n: 1, label: "Keys" },
  { n: 2, label: "Tools" },
  { n: 3, label: "Connect" },
];

function stepsDone() { try { return JSON.parse(store.getLocal("bdm_setup_steps") || "{}"); } catch { return {}; } }
function markStep(n) { const d = stepsDone(); d[n] = true; store.setLocal("bdm_setup_steps", JSON.stringify(d)); }
const dismissSetup = () => store.setLocal("bdm_setup_done", "1");

async function setup() {
  const cfg = cache.config;
  const step = Math.min(STEPS.length, Math.max(1, Number((location.hash.split("/")[1]) || 1)));
  const done = stepsDone();
  done[1] = done[1] || anyKeySet(cfg);
  const rail = h("ol", { class: "rail", "aria-label": "Setup progress" }, STEPS.map((st) =>
    h("li", { class: done[st.n] ? "done" : "" }, h("a", { href: `#setup/${st.n}`, "aria-current": st.n === step ? "step" : null },
      h("span", { class: "n", "aria-hidden": "true" }, done[st.n] ? icon("check") : String(st.n).padStart(2, "0")),
      h("span", { class: "rail-lbl" }, st.label), done[st.n] ? h("span", { class: "sr-only" }, " (done)") : null))));

  let body, blurb;
  if (step === 1) {
    blurb = "Add API keys for the vendors you want, then Test each one. Tier 1 vendors work without a key. Keys go to config/secrets.toml (mode 0600) and are never shown again.";
    const q = await api("/admin/api/quota");
    body = providerGrid(cfg.vendors, Object.fromEntries(q.vendors.map((v) => [v.vendor, v])));
  } else if (step === 2) {
    blurb = "Pick the profile that matches your agent, then fine-tune which tools are exposed.";
    body = toolsEditor();
  } else {
    blurb = "Copy a snippet into your MCP client.";
    body = await connectPanel();
  }
  const last = step === STEPS.length;
  const next = h("a", { href: last ? "#overview" : `#setup/${step + 1}`, class: "cut btn solid", onclick: () => { markStep(step); if (last) dismissSetup(); } }, last ? "Finish setup" : "Continue");
  const prev = step > 1 ? h("a", { href: `#setup/${step - 1}`, class: "cut btn ghost" }, "Back") : null;
  return h("div", { class: "page" },
    pageHead("Setup guide", `Step ${step} of ${STEPS.length} · ${STEPS[step - 1].label}`,
      h("a", { href: "#overview", class: "cut btn ghost small", onclick: dismissSetup }, "Dismiss guide")),
    rail,
    h("p", { class: "legend" }, blurb),
    body,
    h("div", { class: "wiz-foot" }, prev, next));
}

// ------------------------------------------------------------------ overview

function stat(label, value, cls = "") {
  return h("div", { class: `stat ${cls}` }, h("dt", {}, label), h("dd", {}, String(value)));
}

async function overview() {
  const [health, q, calls] = await Promise.all([
    api("/admin/api/health"), api("/admin/api/quota"), api("/admin/api/calls?limit=50"),
  ]);
  const cfg = cache.config;
  const quotaBy = Object.fromEntries(q.vendors.map((v) => [v.vendor, v]));
  const ops = Object.values(health.ops || {});
  const total = ops.reduce((a, x) => a + x.calls, 0);
  const errs = ops.reduce((a, x) => a + x.errors, 0);
  const warnings = cfg.warnings || [];
  const vendors = cfg.vendors.filter((v) => v.id !== "rpc");
  const count = (st) => vendors.filter((v) => STATUS_OF(v) === st).length;
  const open = health.vendors.filter((v) => v.breaker !== "closed").length;
  const strained = q.vendors.filter((v) => v.state !== "ok").length;

  // `rpc` is generic code over the routed chain RPC: its traffic already shows on the RPC vendor
  // that served it (e.g. public), so it is hidden here like on Providers.
  const rows = health.vendors
    .filter((v) => v.vendor !== "rpc" && (v.ok || v.failed || (quotaBy[v.vendor] && quotaBy[v.vendor].state !== "ok") || v.breaker !== "closed"))
    .sort((a, b) => (b.ok + b.failed) - (a.ok + a.failed))
    .map((v) => {
      const qv = quotaBy[v.vendor];
      const worst = worstWindow(qv);
      const n = v.ok + v.failed;
      return h("tr", {},
        h("th", { scope: "row" }, qv ? qv.display_name : v.vendor, h("div", { class: "mono dim small" }, v.vendor)),
        h("td", {}, badge(v.breaker.replace("_", "-"), v.breaker === "closed" ? "ok" : "bad")),
        h("td", { class: "num" }, v.latency_ms === null ? "—" : `${v.latency_ms} ms`),
        h("td", { class: "num" }, n ? `${((v.failed * 100) / n).toFixed(1)}%` : "—"),
        h("td", { class: "num" }, fmtN(v.ok)),
        h("td", {}, worst ? meter(worst.used_pct, worst.state, `${worst.kind}: ${worst.used_pct}%`) : h("span", { class: "dim" }, "no budget")),
        h("td", {}, qv ? stateBadge(qv.state, qv.exhausted_until) : "—"));
    });

  return h("div", { class: "page" },
    pageHead("Overview", "Live status of the aggregator."),
    !anyKeySet(cfg) ? h("p", { class: "cut callout" }, icon("alert"), h("span", {}, "No provider keys yet: keyless vendors only. ", h("a", { href: "#setup/1" }, "Open the setup guide"), " to add keys, pick tools and connect a client.")) : null,
    h("section", { class: "holo hud", "aria-labelledby": "hud-title" },
      h("h2", { id: "hud-title", class: "sr-only" }, "Status"),
      h("dl", { class: "stats" },
        stat("Mode", health.mode === "hosted" ? "hosted" : "self-hosted", "accent"),
        stat("Version", health.version),
        stat("Tools", health.tools),
        stat("Calls since start", fmtN(total), "cyan"),
        stat("Errors", total ? `${fmtN(errs)} · ${((errs * 100) / total).toFixed(1)}%` : fmtN(errs), errs ? "bad" : ""),
        health.mode === "hosted" ? stat("Client keys", health.clients, "magenta") : null)),
    warnings.length ? card("Config warnings", h("ul", { class: "warns" }, warnings.map((w) => h("li", { class: "lock" }, icon("alert"), h("code", {}, w.path), " ", w.message)))) : null,
    h("div", { class: "grid ov" },
      card("Vendor health",
        h("dl", { class: "mini-stats" },
          stat("Active", count("active"), "accent"),
          stat("Need key", count("missing"), count("missing") ? "warn" : ""),
          stat("Disabled", count("disabled")),
          stat("Breaker open", open, open ? "bad" : ""),
          stat("Quota strained", strained, strained ? "warn" : "")),
        h("p", { class: "dim small" }, "Vendors with traffic, an open breaker or a strained quota. Latency is a moving average; the bar shows the most-used window. ", h("a", { href: "#providers" }, "All providers")),
        rows.length ? h("div", { class: "scroll" }, h("table", {},
          h("thead", {}, h("tr", {}, ["Vendor", "Breaker", "Latency", "Errors", "OK", "Quota", "State"].map((t, i) => h("th", { scope: "col", class: i >= 2 && i <= 4 ? "num" : null }, t)))),
          h("tbody", {}, rows))) : emptyState("No vendor traffic yet.")),
      liveFeed(calls.calls)));
}

// ------------------------------------------------------------------ live feed (terminal)

function feedLine(c) {
  const t = new Date(c.ts);
  return h("li", { class: `ln ${c.ok ? "ok" : "bad"}` },
    h("time", { datetime: t.toISOString() }, stamp(t).slice(11)),
    h("span", { class: "st" }, c.ok ? "OK " : "ERR"),
    h("span", { class: "op" }, `> ${c.op}`),
    h("span", { class: "ch" }, c.chain || "—"),
    h("span", { class: "pv" }, c.provider ? `via ${c.provider}` : "via —",
      c.cached ? h("span", { class: "flag cache" }, " [cache]") : null,
      c.fallback ? h("span", { class: "flag fb" }, " [fallback]") : null),
    h("span", { class: "ms" }, `${c.latency_ms}ms`),
    h("span", { class: "cl" }, c.client || "local"),
    c.ok ? null : h("span", { class: "code" }, c.error_code || "error"));
}

const FEED_MAX = 200;
function liveFeed(initial) {
  const list = h("ol", { class: "feed", "aria-live": "polite", "aria-relevant": "additions", "aria-label": "Recent calls, newest first" }, initial.slice(0, FEED_MAX).map(feedLine));
  const state = h("span", { class: "feed-state", id: "stream-state" }, "connecting");
  let paused = false;
  let pending = [];
  const push = (c) => {
    if (paused) { pending.unshift(c); pending.length = Math.min(pending.length, FEED_MAX); pauseBtn.lastChild.textContent = `Resume (${pending.length})`; return; }
    list.prepend(feedLine(c));
    while (list.children.length > FEED_MAX) list.lastChild.remove();
    empty.hidden = true;
  };
  const pauseBtn = btn("Pause", () => {
    paused = !paused;
    pauseBtn.replaceChildren(icon(paused ? "play" : "pause"), paused ? "Resume" : "Pause");
    pauseBtn.setAttribute("aria-pressed", String(paused));
    state.classList.toggle("paused", paused);
    if (!paused) { const p = pending; pending = []; p.reverse().forEach(push); }
  }, { cls: "small ghost", ico: "pause" });
  pauseBtn.setAttribute("aria-pressed", "false");
  const empty = h("p", { class: "feed-empty" }, "> waiting for the first call", h("span", { class: "cursor", "aria-hidden": "true" }));
  empty.hidden = initial.length > 0;
  startStream(push, state);
  return h("section", { class: "cut term feed-card", "aria-labelledby": "feed-title" },
    h("div", { class: "term-bar" }, h("span", { class: "dots", "aria-hidden": "true" }, h("i"), h("i"), h("i")),
      h("h2", { id: "feed-title" }, "tail -f calls"), state, pauseBtn),
    h("div", { class: "term-body feed-body", tabindex: "0", role: "region", "aria-labelledby": "feed-title" }, empty, list));
}

// SSE over fetch (EventSource cannot send the admin headers).
async function startStream(push, stateEl) {
  const ctl = new AbortController();
  streamAbort = ctl;
  const setState = (text, cls) => { stateEl.textContent = text; stateEl.dataset.state = cls; };
  try {
    const r = await fetch("/admin/api/calls/stream", { headers: headers(false), signal: ctl.signal });
    if (r.status === 401) throw new Unauthorized();
    if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`);
    setState("live", "live");
    const reader = r.body.pipeThrough(new TextDecoderStream()).getReader();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += value;
      let i;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const chunk = buf.slice(0, i);
        buf = buf.slice(i + 2);
        const data = chunk.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join("\n");
        if (!data) continue;
        let evt;
        try { evt = JSON.parse(data); } catch { continue; /* ignore malformed event */ }
        push(evt);
      }
    }
  } catch (e) {
    if (ctl.signal.aborted) return;
    if (e instanceof Unauthorized) return handle(e);
    setState("reconnecting", "down");
  }
  if (!ctl.signal.aborted && streamAbort === ctl) {
    setState("reconnecting", "down");
    setTimeout(() => { if (streamAbort === ctl) startStream(push, stateEl); }, 3000);
  }
}

// ------------------------------------------------------------------ clients (hosted only)

async function clients() {
  const data = await api("/admin/api/clients");
  const keyBox = h("div", { "aria-live": "assertive" });
  const name = h("input", { id: "client-name", required: true, maxlength: "100", autocomplete: "off", placeholder: "e.g. acme-prod" });
  const create = async (e) => {
    e.preventDefault();
    try {
      const r = await api("/admin/api/clients", { method: "POST", body: { name: name.value } });
      name.value = "";
      const done = btn("Done", () => { keyBox.replaceChildren(); render(); }, { cls: "small ghost" });
      keyBox.replaceChildren(h("div", { class: "cut term key-once" },
        h("div", { class: "term-bar" }, h("span", { class: "dots", "aria-hidden": "true" }, h("i"), h("i"), h("i")), h("h3", {}, `key for ${r.client.name}`)),
        h("div", { class: "term-body" },
          h("p", { class: "warn-txt" }, icon("alert"), "Copy it now: it is shown once and stored only as a SHA-256 hash."),
          h("pre", { tabindex: "0", "aria-label": "New client key" }, h("code", {}, r.key)),
          h("div", { class: "row" }, btn("Copy key", () => copyText(r.key), { cls: "small solid", ico: "copy" }), done))));
      done.previousSibling && done.previousSibling.focus();
    } catch (err) { handle(err); }
  };
  const d = data.defaults;
  const table = data.clients.length ? h("div", { class: "scroll" }, h("table", { class: "clients" },
    h("thead", {}, h("tr", {}, ["Client", "Status", "Today", "This month", "Top tools", "Limits (override)", ""].map((t) => h("th", { scope: "col" }, t)))),
    h("tbody", {}, data.clients.map(clientRow)))) : emptyState("No client keys yet. Create one above.");
  return h("div", { class: "page" },
    pageHead("Clients", "Keys, limits and usage for each customer."),
    card("New client key",
      h("p", { class: "dim small" }, `Defaults [clients.default]: ${fmtN(d.requests_per_minute)} req/min · ${fmtN(d.daily_requests)} req/day · ${fmtN(d.monthly_credits)} credits/month · profile ${d.tool_profile || "server"} `, lockNote(data.defaults_locked_by)),
      h("form", { class: "row", onsubmit: create },
        h("label", { for: "client-name", class: "lbl" }, "Client name"), field(name), btn("Create key", null, { type: "submit", cls: "solid" })),
      keyBox),
    card(`Client keys · ${data.clients.length}`, table));
}

function clientRow(c) {
  const l = c.limits || {};
  const num = (f, label) => {
    const id = `${f}-${c.id}`;
    return h("div", { class: "budget-in" }, h("label", { for: id, class: "lbl" }, label),
      field(h("input", { type: "number", min: "0", inputmode: "numeric", id, name: f, value: l[f] === undefined || l[f] === null ? "" : String(l[f]), placeholder: String(c.effective_limits[f] ?? "none"), disabled: c.active ? null : true })));
  };
  const profile = h("select", { name: "tool_profile", class: "sel small", disabled: c.active ? null : true, "aria-label": `${c.name} tool profile` },
    ["", "all", ...cache.config.profiles, "custom"].map((p) => h("option", { value: p, selected: (l.tool_profile || "") === p ? true : null }, p || `default (${c.effective_limits.tool_profile || "server"})`)));
  const form = h("form", { class: "row limits", onsubmit: async (e) => {
    e.preventDefault();
    const fd = new FormData(form);
    const limits = {};
    for (const f of ["requests_per_minute", "daily_requests", "monthly_credits"]) if (fd.get(f) !== "") limits[f] = Number(fd.get(f));
    if (fd.get("tool_profile")) limits.tool_profile = fd.get("tool_profile");
    try {
      await api(`/admin/api/clients/${encodeURIComponent(c.id)}`, { method: "PATCH", body: { limits: Object.keys(limits).length ? limits : null } });
      say(`Limits for ${c.name} saved`, "ok");
      render();
    } catch (err) { handle(err); }
  } }, num("requests_per_minute", "rpm"), num("daily_requests", "daily"), num("monthly_credits", "credits/mo"), profile,
    c.active ? btn("Save", null, { type: "submit", cls: "small", aria: `Save limits for ${c.name}` }) : null);
  const revoke = async () => {
    if (!confirm(`Revoke the key for ${c.name}? Clients using it get 401 immediately.`)) return;
    try { await api(`/admin/api/clients/${encodeURIComponent(c.id)}`, { method: "DELETE" }); say(`${c.name} revoked`, "ok"); render(); } catch (e) { handle(e); }
  };
  return h("tr", {},
    h("th", { scope: "row" }, h("b", {}, c.name), h("div", { class: "mono dim small" }, c.id), h("div", { class: "dim small" }, `created ${fmtD(c.created_at)}`)),
    h("td", {}, c.active ? badge("active", "ok") : badge(`revoked ${fmtD(c.revoked_at)}`, "bad")),
    h("td", {}, `${fmtN(c.today.requests)} req`, c.today.throttled ? h("div", {}, badge(`${c.today.throttled} throttled`, "warn")) : null),
    h("td", {}, `${fmtN(c.month.requests)} req · ${fmtN(c.month.credits)} credits`, c.month.throttled ? h("div", { class: "dim small" }, `${c.month.throttled} throttled`) : null),
    h("td", {}, c.top_tools.length ? h("ol", { class: "rank" }, c.top_tools.map(([t, n]) => h("li", {}, h("span", { class: "mono" }, t || "-"), h("span", { class: "num" }, `${fmtN(n)} credits`)))) : h("span", { class: "dim" }, "—")),
    h("td", {}, form),
    h("td", {}, c.active ? btn("Revoke", revoke, { cls: "small danger", aria: `Revoke ${c.name}` }) : null));
}

// ------------------------------------------------------------------ boot

function setMenu(open) {
  $("#menu").setAttribute("aria-expanded", String(open));
  $("#app").classList.toggle("menu-open", open);
}

/** One-click link printed by `onchain-data-mcp password`: /dashboard#login=<password>.
 * Stores it and strips it from the URL (and history). Runs at boot and on hashchange, since
 * opening the link in a tab already on /dashboard doesn't reload the page. */
function consumeLoginHash() {
  if (!location.hash.includes("login=")) return;
  const pw = new URLSearchParams(location.hash.slice(1)).get("login");
  if (pw) store.set(TOKEN_KEY, pw);
  history.replaceState(null, "", location.pathname);
}

document.addEventListener("DOMContentLoaded", () => {
  consumeLoginHash();
  buildNav();
  $("#reload").replaceChildren(icon("refresh"), h("span", { class: "hide-sm" }, "Reload config"));
  $("#logout").replaceChildren(icon("logout"), h("span", { class: "hide-sm" }, "Sign out"));
  $("#reload").setAttribute("aria-label", "Reload config from disk");
  $("#logout").setAttribute("aria-label", "Sign out");
  $("#menu").replaceChildren(icon("menu"));
  $("#menu").addEventListener("click", () => setMenu($("#menu").getAttribute("aria-expanded") !== "true"));
  $("#nav").addEventListener("click", (e) => { if (e.target.closest("a")) setMenu(false); });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && $("#app").classList.contains("menu-open")) { setMenu(false); $("#menu").focus(); } });
  $("#login-form").addEventListener("submit", (e) => {
    e.preventDefault();
    store.set(TOKEN_KEY, $("#token").value.trim());
    $("#token").value = "";
    $("#login-err").textContent = "";
    render();
  });
  $("#logout").addEventListener("click", () => showLogin());
  $("#reload").addEventListener("click", async () => {
    try {
      const r = await api("/admin/api/reload", { method: "POST" });
      say(`Config reloaded from disk${r.warnings.length ? ` (${r.warnings.length} warning(s))` : ""}`, "ok");
      render();
    } catch (e) { handle(e); }
  });
  window.addEventListener("hashchange", async () => {
    consumeLoginHash();
    await render();
    const h1 = $("#view h1");
    (h1 || $("#main")).focus({ preventScroll: true });
    window.scrollTo(0, 0);
  });
  render();
});
