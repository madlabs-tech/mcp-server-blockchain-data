// Dashboard for evm-mcp-server. Vanilla JS, no external resources (works offline).
// All data comes from /admin/api/* with the admin token + X-EMS-Admin header.
// DOM is built with textContent only (no innerHTML with data).
"use strict";

// ------------------------------------------------------------------ utilities

const $ = (sel) => document.querySelector(sel);

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
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
  sun: "M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z",
  moon: "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z",
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
};
function icon(name) {
  return s("svg", { class: "i", viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", "stroke-width": "2", "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true" },
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
const fmtT = (t) => (t ? new Date(t).toLocaleString() : "—");
const fmtD = (t) => (t ? new Date(t).toLocaleDateString() : "—");

function say(text, kind = "info") {
  const box = $("#status");
  const m = h("div", { class: `msg ${kind}` }, text);
  box.append(m);
  setTimeout(() => m.remove(), kind === "err" ? 12000 : 5000);
}

function badge(text, cls = "") {
  return h("span", { class: `badge ${cls}` }, text);
}

function stateBadge(state, until) {
  const label = { ok: "ok", warning: "warning", reserve: "reserve reached", exhausted: "exhausted" }[state] || state;
  return badge(until && state === "exhausted" ? `exhausted until ${fmtT(until)}` : label, state);
}

const SOURCE_LABEL = { vendor_api: "vendor API", headers: "headers", estimated: "estimated" };

function lockNote(env) {
  return env ? h("span", { class: "lock", title: `Set by environment variable ${env}` }, icon("lock"), `locked by env (${env})`) : null;
}

function extLink(href, text) {
  return h("a", { href, target: "_blank", rel: "noopener noreferrer" }, text, " ", icon("ext"));
}

const clamp = (t, n) => (t && t.length > n ? `${t.slice(0, n - 1).trimEnd()}…` : t || "");

function emptyState(text) { return h("div", { class: "empty" }, text); }

async function copyText(text) {
  try { await navigator.clipboard.writeText(text); say("Copied to clipboard", "ok"); }
  catch { say("Copy failed; select the text manually", "err"); }
}

// ------------------------------------------------------------------ API

class Unauthorized extends Error {}

function headers(json) {
  const hd = { Authorization: `Bearer ${store.get("ems_admin_token") || ""}`, "X-EMS-Admin": "1" };
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
  if (e instanceof Unauthorized) showLogin("Session expired or token invalid.");
  else say(e.message || String(e), "err");
}

async function download(path, filename) {
  try {
    const r = await fetch(path, { headers: headers(false) });
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

const PAGES = { setup, overview, quota, vendors, routing, tools, chains, clients };
let streamAbort = null;
const cache = { config: null };

function showLogin(msg) {
  store.del("ems_admin_token");
  $("#login").hidden = false;
  $("#view").replaceChildren();
  $("#logout").hidden = true;
  $("#reload").hidden = true;
  if (msg) say(msg, "err");
  $("#token").focus();
}

function anyKeySet(cfg) { return cfg.vendors.some((v) => v.keys.some((k) => k.set)); }

function currentPage() {
  const raw = (location.hash || "").slice(1).split("/")[0];
  if (PAGES[raw]) return raw;
  if (cache.config && !anyKeySet(cache.config) && !store.getLocal("ems_setup_done")) {
    history.replaceState(null, "", "#setup/1"); // pin it so saving the first key doesn't leave the wizard
    return "setup";
  }
  return "overview";
}

async function render() {
  if (streamAbort) { streamAbort.abort(); streamAbort = null; }
  if (!store.get("ems_admin_token")) return showLogin();
  $("#login").hidden = true;
  $("#logout").hidden = false;
  $("#reload").hidden = false;
  const view = $("#view");
  if (!view.children.length) view.replaceChildren(h("div", { class: "loading" }, "Loading…"));
  try {
    cache.config = await api("/admin/api/config");
    const page = currentPage();
    for (const a of document.querySelectorAll("#nav a")) {
      if (a.getAttribute("href") === `#${page}`) a.setAttribute("aria-current", "page");
      else a.removeAttribute("aria-current");
    }
    document.title = `${page[0].toUpperCase()}${page.slice(1)} · Aggregator Dashboard`;
    const mode = $("#mode");
    mode.hidden = false;
    mode.textContent = cache.config.mode === "hosted" ? "hosted" : "self-hosted";
    view.replaceChildren(await PAGES[page]());
  } catch (e) {
    if (e instanceof Unauthorized) return handle(e);
    view.replaceChildren(h("section", { class: "panel" },
      h("p", { class: "msg err" }, `Could not load this page: ${e.message || e}`),
      h("button", { type: "button", class: "btn", onclick: () => render() }, icon("refresh"), "Retry")));
  }
}

function lockedPath(prefix) {
  const hit = (cache.config.locked || []).find((l) => l.path === prefix || l.path.startsWith(prefix + ".") || prefix.startsWith(l.path + "."));
  return hit ? hit.env : null;
}

// ------------------------------------------------------------------ shared: vendor card

const TIER_A = ["alchemy", "helius", "quicknode", "coingecko", "goplus", "oneinch", "trm"];

function vendorCard(v) {
  const status = v.status.status;
  const statusBadge = status === "active" ? badge("active", "ok") : status === "missing_key" ? badge("missing key", "warn") : badge(v.status.unverified ? "disabled · unverified" : "disabled", "");
  const enabledBox = h("input", { type: "checkbox", id: `en-${v.id}`, checked: v.enabled ? true : null, disabled: v.enabled_locked_by ? true : null });
  enabledBox.addEventListener("change", () => edit([{ path: ["vendors", v.id, "enabled"], value: enabledBox.checked }], `${v.display_name} ${enabledBox.checked ? "enabled" : "disabled"}`));
  const keys = v.keys.map((k) => {
    const err = h("div", { class: "field-err", id: `err-${v.id}-${k.field}`, "aria-live": "polite" });
    const input = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false", id: `key-${v.id}-${k.field}`, placeholder: k.set ? "set (hidden) — paste to replace" : "paste key", disabled: k.locked_by ? true : null, "aria-describedby": err.id });
    return h("form", { class: "field", onsubmit: (e) => {
      e.preventDefault();
      if (!input.value) return;
      const value = input.value;
      input.value = ""; // never keep the key in the DOM
      edit([{ path: ["keys", v.id, k.field], value }], `${v.display_name} ${k.field} saved to secrets.toml`, { errEl: err });
    } },
      h("label", { for: input.id }, h("code", {}, k.env), " ", badge(k.set ? "set" : "missing", k.set ? "ok" : "warn")),
      h("div", { class: "row" }, input, k.locked_by ? lockNote(k.locked_by) : h("button", { type: "submit", class: "btn" }, "Save")),
      err);
  });
  const result = h("div", { class: "test-out", "aria-live": "polite" });
  const test = async () => {
    result.replaceChildren(h("span", { class: "muted" }, "Testing…"));
    try {
      const r = await api(`/admin/api/vendors/${encodeURIComponent(v.id)}/test`, { method: "POST" });
      result.replaceChildren(r.ok ? badge(`OK · ${r.method} · ${r.latency_ms} ms`, "ok") : badge(`Failed: ${r.message}`, "bad"));
    } catch (e) { result.replaceChildren(); handle(e); }
  };
  return h("section", { class: "panel vcard", "aria-labelledby": `v-${v.id}` },
    h("header", {}, h("h2", { id: `v-${v.id}` }, v.display_name), statusBadge, v.free_tier_verified ? null : badge("free tier unverified", "warn")),
    h("div", { class: "row small-text" }, h("span", { class: "muted mono" }, v.id), v.signup_url ? extLink(v.signup_url, "Sign up / get keys") : null),
    v.note ? h("p", { class: "note" }, v.note) : null,
    keys.length ? h("div", { class: "stack" }, keys) : h("p", { class: "muted small-text" }, "Keyless: no credentials needed."),
    h("div", { class: "foot" },
      h("label", { for: enabledBox.id, class: "inline" }, enabledBox, "Enabled"), lockNote(v.enabled_locked_by),
      h("button", { type: "button", class: "btn small", onclick: test, disabled: status === "active" ? null : true, title: status === "active" ? "One cheap call (usage endpoint, eth_blockNumber, getSlot or an FX rate)" : "Only active vendors can be tested" }, icon("play"), "Test"),
      result));
}

function vendorGrid(vendors) {
  const list = vendors.filter((v) => v.id !== "rpc");
  const a = TIER_A.map((id) => list.find((v) => v.id === id)).filter(Boolean);
  const rest = list.filter((v) => !TIER_A.includes(v.id));
  return h("div", { class: "grid cards" },
    h("h3", { class: "tier" }, "Tier A · recommended"), a.map(vendorCard),
    h("h3", { class: "tier" }, "Other providers"), rest.map(vendorCard));
}

// ------------------------------------------------------------------ shared: order editor

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
  const addSel = h("select", { "aria-label": `Add vendor to ${cap}` }, h("option", { value: "" }, "add vendor…"));
  const move = (i, j) => { const [m] = items.splice(i, 1); items.splice(j, 0, m); draw(); };
  const draw = () => {
    list.replaceChildren(...items.map((v, i) => {
      const li = h("li", { draggable: locked ? null : "true" },
        locked ? null : icon("grip"),
        h("span", { class: "pos" }, `${i + 1}.`),
        h("span", { class: "name" }, v),
        !view.effective || view.registered.includes(v) ? null : badge("not registered", "warn"),
        vendorHasKey(v) ? badge("key", "src") : null,
        locked ? null : h("button", { type: "button", class: "btn icon small ghost", "aria-label": `Move ${v} up`, disabled: i === 0 ? true : null, onclick: () => move(i, i - 1) }, icon("up")),
        locked ? null : h("button", { type: "button", class: "btn icon small ghost", "aria-label": `Move ${v} down`, disabled: i === items.length - 1 ? true : null, onclick: () => move(i, i + 1) }, icon("down")),
        locked ? null : h("button", { type: "button", class: "btn icon small ghost", "aria-label": `Remove ${v}`, onclick: () => { items.splice(i, 1); draw(); } }, icon("x")));
      li.addEventListener("dragstart", (e) => { li.classList.add("dragging"); e.dataTransfer.setData("text/plain", String(i)); e.dataTransfer.effectAllowed = "move"; });
      li.addEventListener("dragend", () => li.classList.remove("dragging"));
      li.addEventListener("dragover", (e) => { e.preventDefault(); li.classList.add("over"); });
      li.addEventListener("dragleave", () => li.classList.remove("over"));
      li.addEventListener("drop", (e) => {
        e.preventDefault();
        const from = Number(e.dataTransfer.getData("text/plain"));
        if (Number.isInteger(from) && from !== i) move(from, i);
      });
      return li;
    }));
    if (!items.length) list.append(h("li", { class: "muted" }, "empty: save is disabled, use Reset to inherit"));
    addSel.replaceChildren(h("option", { value: "" }, "add vendor…"), ...known.filter((v) => !items.includes(v)).map((v) => h("option", { value: v }, v)));
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
    ? h("div", { class: "eff" }, h("span", { class: "muted" }, "Effective: "),
        view.effective.length ? view.effective.map((r) => badge(r.usable ? r.vendor : `${r.vendor}: ${r.reason}`, r.usable ? "ok" : "bad")) : h("span", { class: "muted" }, "none"))
    : h("p", { class: "muted small-text" }, "Chain-bound capability: see the per-chain tabs for the effective order.");
  const el = h("div", { class: "cap" },
    h("div", { class: "row spread" },
      h("h2", {}, h("code", {}, cap), badge(`from: ${view.level.replace("_", "-")}`, "src")),
      locked ? lockNote(locked) : h("div", { class: "row" }, addSel,
        h("button", { type: "button", class: "btn small primary", onclick: save }, "Save"),
        canReset ? h("button", { type: "button", class: "btn small ghost", onclick: reset }, "Reset to inherited") : null)),
    list, eff,
    view.warnings.length ? h("ul", {}, view.warnings.map((w) => h("li", { class: "lock" }, icon("alert"), w))) : null);
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
  const panel = h("div", { role: "tabpanel", id: "routing-panel", "aria-labelledby": `tab-${routingTab}` });
  const tablist = h("div", { class: "tabs", role: "tablist", "aria-label": "Routing scope" },
    tabs.map((t) => h("button", { type: "button", role: "tab", id: `tab-${t.id}`, "aria-selected": String(t.id === routingTab), "aria-controls": "routing-panel", tabindex: t.id === routingTab ? "0" : "-1",
      onclick: () => { routingTab = t.id; render(); },
      onkeydown: (e) => {
        const i = tabs.findIndex((x) => x.id === routingTab);
        const j = e.key === "ArrowRight" ? (i + 1) % tabs.length : e.key === "ArrowLeft" ? (i - 1 + tabs.length) % tabs.length : -1;
        if (j >= 0) { routingTab = tabs[j].id; render().then(() => { const b = document.getElementById(`tab-${routingTab}`); if (b) b.focus(); }); }
      } }, t.label)));
  const chain = chains.find((c) => c.id === routingTab);
  const editors = [];
  for (const cap of cfg.capabilities) {
    if (caps && !caps.includes(cap)) continue;
    const o = cfg.orders[cap];
    const view = chain ? o.chains[chain.id] : o.default;
    if (!view) continue;
    const locked = chain ? view.locked_by : o.default_locked_by;
    const ed = orderEditor(cap, view, chain, locked);
    editors.push(ed);
    panel.append(ed);
  }
  if (!editors.length) panel.append(emptyState("No capabilities apply to this scope."));

  const applyAll = (kind) => editors.forEach((e) => e.editor.preset(kind));
  const saveAll = async () => {
    const edits = editors.filter((e) => e.editor.dirty()).flatMap((e) => e.editor.edits());
    if (!edits.length) return say("Nothing changed.", "info");
    await edit(edits, "Routing saved; routing table swapped");
  };
  const resetAll = async () => {
    const edits = editors.filter((e) => !e.editor.locked).flatMap((e) => {
      const cap = e.editor.cap;
      const base = chain ? ["routing", "chains", chain.id, cap] : ["routing", "defaults", cap];
      const aliases = chain ? chain.aliases.map((a) => ({ path: ["routing", "chains", a, cap], value: null })) : [];
      return [...aliases, { path: base, value: null }];
    });
    await edit(edits, "Built-in order restored for this scope");
  };
  const presets = h("div", { class: "presets" },
    h("span", { class: "muted small-text" }, "Presets:"),
    h("button", { type: "button", class: "btn small", onclick: () => applyAll("free") }, "Free-first"),
    h("button", { type: "button", class: "btn small", onclick: () => applyAll("keys") }, "Keys-first"),
    h("button", { type: "button", class: "btn small ghost", onclick: resetAll }, "Reset to built-in"),
    h("button", { type: "button", class: "btn small primary", onclick: saveAll }, "Save all changes"));
  return h("div", {}, tablist, presets, panel);
}

// ------------------------------------------------------------------ shared: tools editor

const STRATEGIES = ["", "failover", "quorum", "aggregate", "fan_out", "hedged"];

function toolsEditor() {
  const cfg = cache.config;
  const current = cfg.settings.server.tool_profile;
  const profileLock = lockedPath("server.tool_profile");
  const profile = h("select", { id: "profile", disabled: profileLock ? true : null },
    [...cfg.profiles, "all", "custom"].map((p) => h("option", { value: p, selected: current === p ? true : null }, p)));
  profile.addEventListener("change", () => edit([{ path: ["server", "tool_profile"], value: profile.value }], `Tool profile set to ${profile.value}`));
  const custom = current === "custom";
  const enabledTools = cfg.settings.server.enabled_tools || [];
  const visible = cfg.operations.filter((o) => o.visible).length;

  const toolCards = cfg.operations.map((op) => {
    const locked = op.locked_by;
    const on = custom ? enabledTools.includes(op.name) : op.enabled;
    const box = h("input", { type: "checkbox", id: `op-${op.name}`, checked: on ? true : null, disabled: locked ? true : null });
    box.addEventListener("change", () => {
      if (custom) {
        const next = box.checked ? [...new Set([...enabledTools, op.name])] : enabledTools.filter((n) => n !== op.name);
        edit([{ path: ["server", "enabled_tools"], value: next }], `${op.name} ${box.checked ? "added to" : "removed from"} the custom profile`);
      } else {
        edit([{ path: ["operations", op.name, "enabled"], value: box.checked }], `${op.name} ${box.checked ? "enabled" : "disabled"}`);
      }
    });
    return h("div", { class: `tool ${op.visible ? "" : "off"}` },
      h("label", { for: box.id, class: "inline" }, box,
        h("span", {}, h("code", {}, op.name), " ", op.visible ? badge("exposed", "ok") : badge("hidden", ""), lockNote(locked),
          h("div", { class: "desc", title: op.description }, clamp(op.description, 140), " · ", op.domain, " · ", op.profiles.join(", ") || "no profile"))));
  });

  const advRows = cfg.operations.map((op) => {
    const locked = op.locked_by;
    const strat = h("select", { "aria-label": `${op.name} strategy`, disabled: locked ? true : null },
      STRATEGIES.map((x) => h("option", { value: x, selected: (op.strategy || "") === x ? true : null }, x || "default")));
    strat.addEventListener("change", () => edit([{ path: ["operations", op.name, "strategy"], value: strat.value || null }], `${op.name} strategy saved`));
    const ttl = h("input", { type: "number", min: "0", "aria-label": `${op.name} cache TTL seconds`, value: op.cache_ttl_secs === null ? "" : String(op.cache_ttl_secs), placeholder: op.default_cache_ttl_secs === null ? "none" : `${op.default_cache_ttl_secs} (default)`, readonly: locked ? true : null });
    const saveTtl = () => edit([{ path: ["operations", op.name, "cache_ttl_secs"], value: ttl.value === "" ? null : Number(ttl.value) }], `${op.name} TTL saved`);
    return h("tr", {},
      h("td", {}, h("code", {}, op.name)),
      h("td", {}, strat),
      h("td", {}, h("div", { class: "row" }, ttl, locked ? lockNote(locked) : h("button", { type: "button", class: "btn small ghost", onclick: saveTtl }, "Save"))));
  });

  return h("div", {},
    h("div", { class: "row" }, h("label", { for: "profile" }, "Tool profile"), profile, lockNote(profileLock), badge(`${visible} of ${cfg.operations.length} tools exposed`, "src")),
    h("p", { class: "muted small-text" }, custom
      ? "Custom profile: tick the tools to expose (saved to server.enabled_tools)."
      : "A profile keeps each agent's tool list small. Untick a tool to disable it everywhere (operations.<tool>.enabled). In hosted mode each client key's own profile applies."),
    cfg.operations.length ? h("div", { class: "toollist" }, toolCards) : emptyState("No tools registered."),
    h("details", { class: "adv" }, h("summary", {}, "Advanced: strategy and cache TTL per tool"),
      h("div", { class: "scroll" }, h("table", {},
        h("thead", {}, h("tr", {}, ["Tool", "Strategy", "Cache TTL (s)"].map((t) => h("th", { scope: "col" }, t)))),
        h("tbody", {}, advRows)))));
}

// ------------------------------------------------------------------ shared: connect panel

function snippet(title, text, lang) {
  return h("div", { class: "snippet" },
    h("div", { class: "row" }, h("h3", {}, title), h("button", { type: "button", class: "btn small ghost", onclick: () => copyText(text), "aria-label": `Copy ${title}` }, icon("copy"), "Copy")),
    h("pre", { tabindex: "0", "data-lang": lang || "" }, text));
}

async function connectPanel() {
  const c = await api("/admin/api/connect");
  const hosted = c.mode === "hosted";
  const base = c.public_url || c.http_url;
  const tool = c.sample_tool || "<tool>";
  const stdio = { mcpServers: { "evm-mcp-server": { command: c.binary_path, args: ["--config-dir", c.config_dir] } } };
  const httpCfg = { mcpServers: { "evm-mcp-server": hosted ? { url: c.mcp_url, headers: { Authorization: "Bearer <client key>" } } : { url: c.mcp_url } } };
  const q = (x) => `'${String(x).replace(/'/g, "'\\''")}'`;
  const curl = [`curl -X POST ${q(`${base}/v1/tools/${tool}`)}`, "  -H 'content-type: application/json'", hosted ? "  -H 'Authorization: Bearer <client key>'" : null, "  -d '{}'"].filter(Boolean).join(" \\\n");
  return h("div", {},
    h("p", { class: "muted" }, `Generated from this server: mode ${hosted ? "hosted" : "self-hosted"}, tool profile ${c.tool_profile}. `,
      hosted ? h("span", {}, "Hosted mode: clients need a key. ", h("a", { href: "#clients" }, "Create a client key")) : "Self-hosted: no client key needed on localhost."),
    h("dl", { class: "facts" },
      h("dt", {}, "MCP (HTTP)"), h("dd", { class: "mono" }, c.mcp_url),
      h("dt", {}, hosted ? "Public URL" : "HTTP"), h("dd", { class: "mono" }, base),
      h("dt", {}, "Binary"), h("dd", { class: "mono" }, c.binary_path),
      h("dt", {}, "Config dir"), h("dd", { class: "mono" }, c.config_dir)),
    h("h2", { style: "margin-top:24px" }, "Claude Desktop (stdio)"),
    snippet("claude_desktop_config.json", JSON.stringify(stdio, null, 2), "json"),
    h("h2", {}, "Claude Code"),
    snippet("claude mcp add (stdio)", `claude mcp add evm-mcp-server -- ${q(c.binary_path)} --config-dir ${q(c.config_dir)}`, "sh"),
    snippet("claude mcp add (HTTP)", `claude mcp add --transport http evm-mcp-server ${q(c.mcp_url)}${hosted ? " --header 'Authorization: Bearer <client key>'" : ""}`, "sh"),
    h("h2", {}, "Cursor / any HTTP MCP client"),
    snippet(".cursor/mcp.json", JSON.stringify(httpCfg, null, 2), "json"),
    h("h2", {}, "REST"),
    snippet(`curl /v1/tools/${tool}`, curl, "sh"),
    h("p", { class: "muted small-text" }, "OpenAPI: ", h("code", {}, `${base}/openapi.json`), " · tool list: ", h("code", {}, `${base}/v1/tools`)));
}

// ------------------------------------------------------------------ setup wizard

const STEPS = [
  { n: 1, id: "providers", label: "Providers" },
  { n: 2, id: "routing", label: "Routing" },
  { n: 3, id: "tools", label: "Tools" },
  { n: 4, id: "connect", label: "Connect" },
];

function stepsDone() { try { return JSON.parse(store.getLocal("ems_setup_steps") || "{}"); } catch { return {}; } }
function markStep(n) { const d = stepsDone(); d[n] = true; store.setLocal("ems_setup_steps", JSON.stringify(d)); }

async function setup() {
  const cfg = cache.config;
  const step = Math.min(4, Math.max(1, Number((location.hash.split("/")[1]) || 1)));
  const done = stepsDone();
  done[1] = done[1] || anyKeySet(cfg);
  const rail = h("ol", { class: "rail", "aria-label": "Setup progress" }, STEPS.map((st) =>
    h("li", {}, h("a", { href: `#setup/${st.n}`, class: done[st.n] ? "done" : "", "aria-current": st.n === step ? "step" : null },
      h("span", { class: "n", "aria-hidden": "true" }, done[st.n] ? icon("check") : String(st.n)),
      h("span", { class: "lbl" }, st.label), done[st.n] ? h("span", { class: "sr-only" }, " (done)") : null))));

  let body, title, blurb;
  if (step === 1) {
    title = "Providers";
    blurb = "Add API keys for the vendors you want, then Test each one. Keys are saved to config/secrets.toml (mode 0600) and never shown again. Keyless vendors work out of the box.";
    body = vendorGrid(cfg.vendors);
  } else if (step === 2) {
    title = "Routing";
    blurb = "Primary first, then fallbacks. Presets reorder every list below; Save all applies them. Per-chain tabs override the defaults.";
    body = routingEditor(MAIN_CAPS);
  } else if (step === 3) {
    title = "Tools";
    blurb = "Pick the profile that matches your agent, then fine-tune which tools are exposed.";
    body = toolsEditor();
  } else {
    title = "Connect";
    blurb = "Copy a snippet into your MCP client.";
    body = await connectPanel();
  }
  const next = step < 4
    ? h("a", { href: `#setup/${step + 1}`, class: "btn primary", onclick: () => markStep(step) }, "Continue")
    : h("a", { href: "#overview", class: "btn primary", onclick: () => { markStep(4); store.setLocal("ems_setup_done", "1"); } }, "Finish setup");
  const prev = step > 1 ? h("a", { href: `#setup/${step - 1}`, class: "btn ghost" }, "Back") : h("a", { href: "#overview", class: "btn ghost", onclick: () => store.setLocal("ems_setup_done", "1") }, "Skip setup");
  return h("div", {},
    h("section", { class: "panel" },
      h("h1", {}, "Setup"), rail,
      h("h2", {}, `Step ${step} · ${title}`), h("p", { class: "muted" }, blurb),
      body,
      h("div", { class: "wiz-foot" }, prev, next)));
}

// ------------------------------------------------------------------ overview

async function overview() {
  const [health, q, calls] = await Promise.all([
    api("/admin/api/health"), api("/admin/api/quota"), api("/admin/api/calls?limit=50"),
  ]);
  const quotaBy = Object.fromEntries(q.vendors.map((v) => [v.vendor, v]));
  const ops = Object.values(health.ops || {});
  const calls24 = ops.reduce((a, x) => a + x.calls, 0);
  const errs = ops.reduce((a, x) => a + x.errors, 0);
  const warnings = cache.config.warnings || [];

  const rows = health.vendors
    .filter((v) => quotaBy[v.vendor] || v.ok || v.failed)
    .map((v) => {
      const qv = quotaBy[v.vendor];
      const worst = qv && qv.windows.filter((w) => w.used_pct !== null).sort((a, b) => b.used_pct - a.used_pct)[0];
      const total = v.ok + v.failed;
      return h("tr", {},
        h("td", {}, qv ? qv.display_name : v.vendor, " ", h("span", { class: "muted mono" }, v.vendor)),
        h("td", {}, badge(v.breaker.replace("_", "-"), v.breaker === "closed" ? "ok" : "bad")),
        h("td", { class: "num" }, v.latency_ms === null ? "—" : `${v.latency_ms} ms`),
        h("td", { class: "num" }, total ? `${((v.failed * 100) / total).toFixed(1)}%` : "—"),
        h("td", { class: "num" }, fmtN(v.ok)),
        h("td", {}, worst ? meter(worst.used_pct, worst.state, `${worst.kind}: ${worst.used_pct}%`) : h("span", { class: "muted" }, "no budget")),
        h("td", {}, qv ? stateBadge(qv.state, qv.exhausted_until) : "—"),
      );
    });

  const streamBody = h("tbody", {}, calls.calls.map(callRow));
  startStream(streamBody);

  return h("div", {},
    !anyKeySet(cache.config) ? h("p", { class: "msg info" }, "No provider keys yet: keyless vendors only. ", h("a", { href: "#setup" }, "Run setup"), " to add keys, pick routing and connect a client.") : null,
    h("section", { class: "panel" },
      h("h1", {}, "Overview"),
      h("div", { class: "stats" },
        stat("Mode", health.mode === "hosted" ? "hosted" : "self-hosted"),
        stat("Version", health.version),
        stat("Tools", health.tools),
        stat("Calls (since start)", fmtN(calls24)),
        stat("Errors", fmtN(errs)),
        stat("Client keys", health.clients),
      ),
      warnings.length ? h("div", {}, h("h3", {}, "Config warnings"), h("ul", {}, warnings.map((w) => h("li", {}, h("code", {}, w.path), " ", w.message)))) : null,
    ),
    h("section", { class: "panel" },
      h("h2", {}, "Vendors"),
      h("p", { class: "muted small-text" }, "Latency is an exponential moving average. The quota bar shows the most-used window."),
      rows.length ? h("div", { class: "scroll" }, h("table", {},
        h("thead", {}, h("tr", {}, ["Vendor", "Breaker", "Latency", "Error rate", "OK", "Quota", "State"].map((t, i) => h("th", { scope: "col", class: i >= 2 && i <= 4 ? "num" : null }, t)))),
        h("tbody", {}, rows))) : emptyState("No vendor activity yet."),
    ),
    h("section", { class: "panel" },
      h("h2", {}, "Live calls"),
      h("p", { class: "muted small-text", id: "stream-state" }, "Connecting…"),
      h("div", { class: "scroll stream" }, h("table", { "aria-describedby": "stream-state" },
        h("thead", {}, h("tr", {}, ["Time", "Tool", "Chain", "Provider", "Fallback", "Latency", "Client", "Result"].map((t) => h("th", { scope: "col" }, t)))),
        streamBody)),
    ),
  );
}

function stat(label, value) {
  return h("div", { class: "stat" }, h("span", { class: "muted" }, label), h("b", {}, value));
}

function meter(pct, state, label) {
  const bar = h("span", {});
  bar.style.width = `${Math.min(100, pct)}%`;
  return h("div", { class: "row" },
    h("div", { class: `meter ${state || ""}`, role: "meter", "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": String(Math.min(100, pct)), "aria-label": label }, bar),
    h("span", { class: "muted small-text" }, label));
}

function callRow(c) {
  return h("tr", {},
    h("td", {}, new Date(c.ts).toLocaleTimeString()),
    h("td", { class: "mono" }, c.op),
    h("td", { class: "mono" }, c.chain || "—"),
    h("td", {}, c.provider || "—", c.cached ? " " : null, c.cached ? badge("cache", "src") : null),
    h("td", {}, c.fallback ? badge("fallback", "warn") : "no"),
    h("td", { class: "num" }, `${c.latency_ms} ms`),
    h("td", { class: "mono" }, c.client || "local"),
    h("td", {}, c.ok ? badge("ok", "ok") : badge(c.error_code || "error", "bad")),
  );
}

// SSE over fetch (EventSource cannot send the admin headers).
async function startStream(tbody) {
  const ctl = new AbortController();
  streamAbort = ctl;
  const stateEl = () => $("#stream-state");
  try {
    const r = await fetch("/admin/api/calls/stream", { headers: headers(false), signal: ctl.signal });
    if (!r.ok || !r.body) throw new Error(`HTTP ${r.status}`);
    if (stateEl()) stateEl().textContent = tbody.children.length ? "Live: new calls appear at the top." : "Live: waiting for the first call.";
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
        try {
          tbody.prepend(callRow(JSON.parse(data)));
          while (tbody.children.length > 200) tbody.lastChild.remove();
        } catch { /* ignore malformed event */ }
      }
    }
  } catch (e) {
    if (ctl.signal.aborted) return;
    if (stateEl()) stateEl().textContent = `Stream disconnected (${e.message}); retrying…`;
  }
  if (!ctl.signal.aborted && streamAbort === ctl) setTimeout(() => { if (streamAbort === ctl) startStream(tbody); }, 3000);
}

// ------------------------------------------------------------------ quota

async function quota() {
  const q = await api("/admin/api/quota");
  const actions = h("div", { class: "row" },
    h("button", { type: "button", class: "btn", onclick: async () => {
      try { await api("/admin/api/quota/refresh", { method: "POST" }); say("Refreshed from vendor usage APIs", "ok"); render(); } catch (e) { handle(e); }
    } }, icon("refresh"), "Refresh from vendors"),
    h("button", { type: "button", class: "btn ghost", onclick: () => download("/admin/api/quota.csv", "quota.csv") }, "Export CSV"),
  );
  return h("div", {},
    h("section", { class: "panel" },
      h("div", { class: "row spread" }, h("h1", {}, "Quota"), actions),
      h("p", { class: "muted small-text" },
        "Effective budget = min(cap, limit × (1 − reserve)). Routing skips a vendor once the most pessimistic source reaches it. ",
        "Sources: ", badge("vendor API", "src"), " the vendor's usage endpoint, ", badge("headers", "src"), " rate-limit headers, ",
        badge("estimated", "src"), " local metering with the cost table."),
      h("p", { class: "muted small-text" }, `Generated ${fmtT(q.generated_at)}.`),
    ),
    q.vendors.length ? h("div", { class: "grid" }, q.vendors.map(quotaCard)) : emptyState("No vendors to report on."),
  );
}

function quotaCard(v) {
  const reserveLock = lockedPath(`vendors.${v.vendor}.reserve_pct`);
  const reserveInput = h("input", { type: "number", min: "0", max: "100", value: String(v.reserve_pct), id: `res-${v.vendor}`, readonly: reserveLock ? true : null });
  const windows = v.windows.map((w) => windowRow(v, w));
  return h("section", { class: "panel card", "aria-labelledby": `q-${v.vendor}` },
    h("h2", { id: `q-${v.vendor}` },
      v.display_name, h("span", { class: "muted mono" }, v.vendor),
      stateBadge(v.state, v.exhausted_until),
      badge(v.status.status.replace("_", " "), v.status.status === "active" ? "ok" : ""),
      v.estimated_only ? badge("estimated only", "warn") : badge("vendor API", "src")),
    h("p", { class: "muted small-text" },
      `Unit: ${v.unit} · reset: ${v.reset.replace("_", " ")} · on exhausted: ${v.on_exhausted.replace("_", " ")} · alerts at ${v.alert_pct.join("%, ")}%`,
      v.plan ? ` · plan: ${v.plan}` : "",
      v.reported_at ? ` · vendor data ${fmtT(v.reported_at)}` : ""),
    v.report_error ? h("p", { class: "msg err" }, `Usage API error: ${v.report_error}`) : null,
    v.estimated_only ? h("p", { class: "muted small-text" }, "No usage API is confirmed for this vendor: numbers are local estimates (plus rate-limit headers when the vendor sends them).") : null,
    windows.length
      ? h("div", { class: "scroll" }, h("table", {},
          h("thead", {}, h("tr", {}, ["Window", "Limit", "Cap", "Effective", "Used / remaining", "Source", "Burn / day", "Runs out", "Resets", "State"].map((t) => h("th", { scope: "col" }, t)))),
          h("tbody", {}, windows)))
      : h("p", { class: "muted" }, "No limits configured and no usage yet."),
    h("form", { class: "row", onsubmit: (e) => {
      e.preventDefault();
      edit([{ path: ["vendors", v.vendor, "reserve_pct"], value: Number(reserveInput.value) }], `Reserve for ${v.vendor} saved`);
    } },
      h("label", { for: `res-${v.vendor}`, class: "inline" }, "Reserve %", reserveInput),
      reserveLock ? lockNote(reserveLock) : h("button", { type: "submit", class: "btn small" }, "Save reserve")),
    v.series ? chart(v) : null,
    v.breakdown ? breakdown(v.breakdown) : null,
  );
}

function budgetCell(v, w, which) {
  const value = w[which];
  const locked = w[`${which}_locked_by`];
  if (w.kind === "live") return h("td", { class: "num" }, fmtN(value));
  const id = `${which}-${v.vendor}-${w.kind}`;
  const input = h("input", { type: "number", min: "0", id, value: value === null ? "" : String(value), placeholder: "none", readonly: locked ? true : null, "aria-label": `${v.vendor} ${w.kind} ${which}` });
  const save = async () => {
    try {
      const raw = input.value.trim();
      const r = await api(`/admin/api/vendors/${encodeURIComponent(v.vendor)}/budget`, { method: "POST", body: { which, window: w.kind, value: raw === "" ? null : Number(raw) } });
      say(`${v.vendor} ${w.kind} ${which} saved` + (r.warnings.length ? ` (${r.warnings.map((x) => x.message).join("; ")})` : ""), "ok");
      render();
    } catch (e) { handle(e); }
  };
  return h("td", {}, input, locked ? h("div", {}, lockNote(locked)) : h("button", { type: "button", class: "btn small ghost", onclick: save, "aria-label": `Save ${v.vendor} ${w.kind} ${which}` }, "Save"));
}

function windowRow(v, w) {
  const alt = w.sources.filter((x) => x.source !== w.source).map((x) => `${SOURCE_LABEL[x.source]}: ${fmtN(x.used)}${x.comparable ? "" : " (other unit)"}`).join(", ");
  return h("tr", {},
    h("th", { scope: "row" }, w.kind === "live" ? "live (headers)" : w.kind.replace("_", " ")),
    budgetCell(v, w, "limit"),
    budgetCell(v, w, "cap"),
    h("td", { class: "num" }, fmtN(w.effective)),
    h("td", {},
      w.used === null ? h("span", { class: "muted" }, "not tracked (token bucket)") : h("div", {},
        `${fmtN(w.used)} / ${fmtN(w.remaining)} left`,
        w.used_pct !== null ? meter(w.used_pct, w.state, `${w.used_pct}%`) : null)),
    h("td", {}, w.source ? badge(SOURCE_LABEL[w.source], "src") : "—", alt ? h("div", { class: "muted small-text" }, alt) : null),
    h("td", { class: "num" }, fmtN(w.burn_per_day)),
    h("td", {}, w.runs_out_at ? fmtT(w.runs_out_at) : w.burn_per_day ? "not before reset" : "—"),
    h("td", {}, fmtT(w.resets_at)),
    h("td", {}, stateBadge(w.state, w.state === "exhausted" ? w.resets_at : null), w.alerts.length ? h("div", { class: "muted small-text" }, `alert ${w.alerts.join("%, ")}%`) : null),
  );
}

function chart(v) {
  const data = v.series;
  const daily = v.windows.find((w) => w.kind === "daily");
  const W = 600, H = 130, pad = 18, top = 8;
  const max = Math.max(1, ...data.map((d) => d[1]), daily && daily.effective ? daily.effective : 0);
  const bw = (W - pad) / data.length;
  const total = data.reduce((a, d) => a + d[1], 0);
  const svg = s("svg", { viewBox: `0 0 ${W} ${H}`, class: "chart", role: "img", "aria-label": `${v.display_name}: ${fmtN(total)} ${v.unit} used over the last 30 days` });
  data.forEach(([day, n], i) => {
    const bh = n === 0 ? 1 : Math.max(1, ((H - pad - top) * n) / max);
    svg.append(s("rect", { x: String(pad + i * bw + 1), y: String(H - pad - bh), width: String(Math.max(1, bw - 2)), height: String(bh), class: n === 0 ? "bar zero" : "bar" },
      s("title", {}, `${day}: ${fmtN(n)} ${v.unit}`)));
  });
  if (daily && daily.effective) {
    const y = H - pad - ((H - pad - top) * daily.effective) / max;
    svg.append(s("line", { x1: String(pad), x2: String(W), y1: String(y), y2: String(y), class: "line" }, s("title", {}, `daily effective budget ${fmtN(daily.effective)}`)));
  }
  svg.append(s("text", { x: String(pad), y: String(H - 4) }, data[0][0]));
  svg.append(s("text", { x: String(W - 60), y: String(H - 4) }, data[data.length - 1][0]));
  svg.append(s("text", { x: "0", y: String(top + 8) }, fmtN(max)));
  return h("div", {}, h("h3", {}, "Last 30 days (local metering)"), svg);
}

function breakdown(b) {
  const list = (title, rows) => h("div", {}, h("h3", {}, title),
    rows.length ? h("ol", {}, rows.map(([k, n]) => h("li", {}, h("span", { class: "mono" }, k), ` ${fmtN(n)}`))) : h("p", { class: "muted" }, "none"));
  return h("div", {}, h("h3", {}, "This month by…"),
    h("div", { class: "breakdown" }, list("Tool", b.tools), list("Method", b.methods), list("Chain", b.chains), list("Client", b.clients)));
}

// ------------------------------------------------------------------ vendors

async function vendors() {
  const cfg = cache.config;
  return h("div", {},
    h("section", { class: "panel" },
      h("h1", {}, "Vendors"),
      h("p", { class: "muted small-text" }, "Keys are write-only: saved to config/secrets.toml (mode 0600) and never shown again. Keys set by environment variables are locked here. Test makes one cheap call (usage endpoint, eth_blockNumber, getSlot or an FX rate).")),
    cfg.vendors.length ? vendorGrid(cfg.vendors) : emptyState("No vendors in the registry."));
}

// ------------------------------------------------------------------ routing

async function routing() {
  return h("section", { class: "panel" },
    h("h1", {}, "Routing"),
    h("p", { class: "muted small-text" }, "Order = primary, then fallbacks. Most specific wins: operation > chain > defaults > built-in. Drag to reorder (or use the arrow buttons), then save. The effective order shows what routing will actually use right now."),
    routingEditor(null));
}

// ------------------------------------------------------------------ tools

async function tools() {
  return h("section", { class: "panel" }, h("h1", {}, "Tools"), toolsEditor());
}

// ------------------------------------------------------------------ chains

async function chains() {
  const cfg = cache.config;
  const rows = cfg.chains.map((c) => {
    const key = c.override_key;
    const en = h("input", { type: "checkbox", id: `ch-${key}`, checked: c.enabled ? true : null, disabled: c.locked_by ? true : null });
    en.addEventListener("change", () => edit([{ path: ["chain_overrides", key, "enabled"], value: en.checked }], `${c.name} ${en.checked ? "enabled" : "disabled"}`));
    const err = h("div", { class: "field-err", "aria-live": "polite" });
    const rpcs = h("textarea", { "aria-label": `${c.name} public RPC URLs, one per line`, readonly: c.locked_by ? true : null }, c.public_rpc.join("\n"));
    const saveRpc = () => {
      const list = rpcs.value.split("\n").map((x) => x.trim()).filter(Boolean);
      edit([{ path: ["chain_overrides", key, "public_rpc"], value: list }], `${c.name} public RPCs saved`, { errEl: err });
    };
    return h("tr", {},
      h("td", {}, h("b", {}, c.name), h("div", { class: "muted mono" }, c.id), c.aliases.length ? h("div", { class: "muted small-text" }, `aliases: ${c.aliases.join(", ")}`) : null),
      h("td", {}, c.family),
      h("td", {}, h("label", { for: en.id, class: "inline" }, en, "enabled"), lockNote(c.locked_by)),
      h("td", {}, c.finality.policy, c.finality.default ? ` (${c.finality.default})` : "", c.finality.note ? h("div", { class: "muted small-text" }, c.finality.note) : null),
      h("td", {}, rpcs, err, c.locked_by ? null : h("button", { type: "button", class: "btn small ghost", onclick: saveRpc }, "Save RPCs")),
      h("td", {}, c.explorer ? extLink(c.explorer, "explorer") : "—"),
    );
  });
  return h("section", { class: "panel" },
    h("h1", {}, "Chains"),
    h("p", { class: "muted small-text" }, "Keyless public RPCs are the last-resort fallback. Keyed endpoints go in [custom_rpc] or vendor keys, so they stay out of this list."),
    cfg.custom_rpc.length ? h("p", { class: "row" }, "Custom RPC endpoints: ", cfg.custom_rpc.map((c) => badge(`${c.name} → ${c.chain}`, "src"))) : null,
    rows.length ? h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, ["Chain", "Family", "Enabled", "Finality", "Public RPCs", "Explorer"].map((t) => h("th", { scope: "col" }, t)))),
      h("tbody", {}, rows))) : emptyState("No chains in the registry."));
}

// ------------------------------------------------------------------ clients

async function clients() {
  const data = await api("/admin/api/clients");
  const keyBox = h("div", { "aria-live": "assertive" });
  const name = h("input", { id: "client-name", required: true, maxlength: "100", autocomplete: "off" });
  const create = async (e) => {
    e.preventDefault();
    try {
      const r = await api("/admin/api/clients", { method: "POST", body: { name: name.value } });
      name.value = "";
      keyBox.replaceChildren(h("div", { class: "key-once" },
        h("b", {}, `Key for “${r.client.name}” (${r.client.id}). Copy it now: it is shown once and stored only as a SHA-256 hash.`),
        h("div", {}, h("code", {}, r.key)),
        h("div", { class: "row", style: "margin-top:8px" },
          h("button", { type: "button", class: "btn small", onclick: () => copyText(r.key) }, icon("copy"), "Copy"),
          h("button", { type: "button", class: "btn small ghost", onclick: () => { keyBox.replaceChildren(); render(); } }, "Done"))));
    } catch (err) { handle(err); }
  };
  const d = data.defaults;
  const table = data.clients.length ? h("div", { class: "scroll" }, h("table", {},
    h("thead", {}, h("tr", {}, ["Client", "Status", "Today", "This month", "Top tools", "Limits (override)", ""].map((t) => h("th", { scope: "col" }, t)))),
    h("tbody", {}, data.clients.map(clientRow)))) : emptyState("No client keys yet. Create one above.");
  return h("div", {},
    h("section", { class: "panel" },
      h("h1", {}, "Clients"),
      data.mode !== "hosted" ? h("p", { class: "msg info" }, "Self-hosted mode: client keys are only enforced when mode = \"hosted\".") : null,
      h("p", { class: "muted small-text" }, `Defaults [clients.default]: ${fmtN(d.requests_per_minute)} req/min · ${fmtN(d.daily_requests)} req/day · ${fmtN(d.monthly_credits)} credits/month · profile ${d.tool_profile || "server"}`, " ", lockNote(data.defaults_locked_by)),
      h("form", { class: "row", onsubmit: create },
        h("label", { for: "client-name" }, "New client name"), name, h("button", { type: "submit", class: "btn primary" }, "Create key")),
      keyBox),
    h("section", { class: "panel" }, table));
}

function clientRow(c) {
  const l = c.limits || {};
  const num = (field, label) => h("label", { class: "inline" }, label,
    h("input", { type: "number", min: "0", name: field, value: l[field] === undefined || l[field] === null ? "" : String(l[field]), placeholder: String(c.effective_limits[field] ?? "none"), disabled: c.active ? null : true }));
  const profile = h("select", { name: "tool_profile", disabled: c.active ? null : true, "aria-label": `${c.name} tool profile` },
    ["", "all", ...cache.config.profiles, "custom"].map((p) => h("option", { value: p, selected: (l.tool_profile || "") === p ? true : null }, p || `default (${c.effective_limits.tool_profile || "server"})`)));
  const form = h("form", { class: "row", onsubmit: async (e) => {
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
    c.active ? h("button", { type: "submit", class: "btn small" }, "Save") : null);
  const revoke = async () => {
    if (!confirm(`Revoke the key for ${c.name}? Clients using it get 401 immediately.`)) return;
    try { await api(`/admin/api/clients/${encodeURIComponent(c.id)}`, { method: "DELETE" }); say(`${c.name} revoked`, "ok"); render(); } catch (e) { handle(e); }
  };
  return h("tr", {},
    h("td", {}, h("b", {}, c.name), h("div", { class: "muted mono" }, c.id), h("div", { class: "muted small-text" }, `created ${fmtD(c.created_at)}`)),
    h("td", {}, c.active ? badge("active", "ok") : badge(`revoked ${fmtD(c.revoked_at)}`, "bad")),
    h("td", {}, `${fmtN(c.today.requests)} req`, c.today.throttled ? h("div", {}, badge(`${c.today.throttled} throttled`, "warn")) : null),
    h("td", {}, `${fmtN(c.month.requests)} req · ${fmtN(c.month.credits)} credits`, c.month.throttled ? h("div", { class: "muted small-text" }, `${c.month.throttled} throttled`) : null),
    h("td", {}, c.top_tools.length ? h("ol", {}, c.top_tools.map(([t, n]) => h("li", {}, h("span", { class: "mono" }, t || "-"), ` ${fmtN(n)}`))) : h("span", { class: "muted" }, "—")),
    h("td", {}, form),
    h("td", {}, c.active ? h("button", { type: "button", class: "btn small danger", onclick: revoke }, "Revoke") : null),
  );
}

// ------------------------------------------------------------------ boot

function isDark() {
  const t = document.documentElement.getAttribute("data-theme");
  return t === "dark" || (!t && matchMedia("(prefers-color-scheme: dark)").matches);
}
function applyTheme(t) {
  if (t) document.documentElement.setAttribute("data-theme", t);
  else document.documentElement.removeAttribute("data-theme");
  const b = $("#theme");
  b.replaceChildren(icon(isDark() ? "sun" : "moon"));
  b.setAttribute("aria-label", isDark() ? "Switch to light mode" : "Switch to dark mode");
}

document.addEventListener("DOMContentLoaded", () => {
  applyTheme(store.getLocal("ems_theme"));
  $("#reload").replaceChildren(icon("refresh"));
  $("#theme").addEventListener("click", () => {
    const next = isDark() ? "light" : "dark";
    store.setLocal("ems_theme", next);
    applyTheme(next);
  });
  $("#login-form").addEventListener("submit", (e) => {
    e.preventDefault();
    store.set("ems_admin_token", $("#token").value.trim());
    $("#token").value = "";
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
  window.addEventListener("hashchange", () => { render(); $("#main").focus(); });
  render();
});
