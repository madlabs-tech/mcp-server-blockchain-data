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
  return env ? h("span", { class: "lock", title: `Set by environment variable ${env}` }, `🔒 locked by env (${env})`) : null;
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

/** Apply config edits; reports warnings/errors and re-renders the current page. */
async function edit(edits, okText = "Saved") {
  try {
    const r = await api("/admin/api/config", { method: "PUT", body: { edits } });
    say(okText + (r.warnings && r.warnings.length ? ` (${r.warnings.length} warning(s): ${r.warnings.map((w) => w.message).join("; ")})` : ""), "ok");
    await render();
    return true;
  } catch (e) {
    handle(e);
    return false;
  }
}

function handle(e) {
  if (e instanceof Unauthorized) {
    showLogin("Session expired or token invalid.");
  } else {
    say(e.message || String(e), "err");
  }
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

const PAGES = { overview, quota, vendors, routing, tools, chains, clients };
let streamAbort = null;
let cache = { config: null };

function showLogin(msg) {
  store.del("ems_admin_token");
  $("#login").hidden = false;
  $("#view").replaceChildren();
  $("#logout").hidden = true;
  $("#reload").hidden = true;
  if (msg) say(msg, "err");
  $("#token").focus();
}

function currentPage() {
  const p = (location.hash || "#overview").slice(1).split("/")[0];
  return PAGES[p] ? p : "overview";
}

async function render() {
  if (streamAbort) { streamAbort.abort(); streamAbort = null; }
  if (!store.get("ems_admin_token")) return showLogin();
  $("#login").hidden = true;
  $("#logout").hidden = false;
  $("#reload").hidden = false;
  const page = currentPage();
  for (const a of document.querySelectorAll("#nav a")) {
    if (a.getAttribute("href") === `#${page}`) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  document.title = `${page[0].toUpperCase()}${page.slice(1)} · Aggregator Dashboard`;
  try {
    cache.config = await api("/admin/api/config");
    const mode = $("#mode");
    mode.hidden = false;
    mode.textContent = cache.config.mode === "hosted" ? "hosted" : "self-hosted";
    const view = await PAGES[page]();
    $("#view").replaceChildren(view);
  } catch (e) { handle(e); }
}

function lockedPath(prefix) {
  const hit = (cache.config.locked || []).find((l) => l.path === prefix || l.path.startsWith(prefix + ".") || prefix.startsWith(l.path + "."));
  return hit ? hit.env : null;
}

// ------------------------------------------------------------------ overview

async function overview() {
  const [health, q, calls] = await Promise.all([
    api("/admin/api/health"), api("/admin/api/quota"), api("/admin/api/calls?limit=50"),
  ]);
  const quotaBy = Object.fromEntries(q.vendors.map((v) => [v.vendor, v]));
  const ops = Object.values(health.ops || {});
  const calls24 = ops.reduce((a, s) => a + s.calls, 0);
  const errs = ops.reduce((a, s) => a + s.errors, 0);
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
      h("p", { class: "muted" }, "Latency is an exponential moving average. The quota bar shows the most-used window."),
      h("div", { class: "scroll" }, h("table", {},
        h("thead", {}, h("tr", {}, ["Vendor", "Breaker", "Latency", "Error rate", "OK", "Quota", "State"].map((t, i) => h("th", { scope: "col", class: i >= 2 && i <= 4 ? "num" : null }, t)))),
        h("tbody", {}, rows))),
    ),
    h("section", { class: "panel" },
      h("h2", {}, "Live calls"),
      h("p", { class: "muted", id: "stream-state" }, "Connecting…"),
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
    h("span", { class: "muted" }, label));
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
    if (stateEl()) stateEl().textContent = "Live: new calls appear at the top. REST calls are logged; MCP calls once the server enables the call observer.";
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
    h("button", { type: "button", onclick: async () => {
      try { await api("/admin/api/quota/refresh", { method: "POST" }); say("Refreshed from vendor usage APIs", "ok"); render(); } catch (e) { handle(e); }
    } }, "Refresh from vendors"),
    h("button", { type: "button", class: "ghost", onclick: () => download("/admin/api/quota.csv", "quota.csv") }, "Export CSV"),
  );
  return h("div", {},
    h("section", { class: "panel" },
      h("div", { class: "row spread" }, h("h1", {}, "Quota"), actions),
      h("p", { class: "muted" },
        "Effective budget = min(cap, limit × (1 − reserve)). Routing skips a vendor once the most pessimistic source reaches it. ",
        "Sources: ", badge("vendor API", "src"), " the vendor's usage endpoint, ", badge("headers", "src"), " rate-limit headers, ",
        badge("estimated", "src"), " local metering with the cost table."),
      h("p", { class: "muted" }, `Generated ${fmtT(q.generated_at)}.`),
    ),
    h("div", { class: "grid" }, q.vendors.map(quotaCard)),
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
    h("p", { class: "muted" },
      `Unit: ${v.unit} · reset: ${v.reset.replace("_", " ")} · on exhausted: ${v.on_exhausted.replace("_", " ")} · alerts at ${v.alert_pct.join("%, ")}%`,
      v.plan ? ` · plan: ${v.plan}` : "",
      v.reported_at ? ` · vendor data ${fmtT(v.reported_at)}` : ""),
    v.report_error ? h("p", { class: "msg err" }, `Usage API error: ${v.report_error}`) : null,
    v.estimated_only ? h("p", { class: "muted" }, "No usage API is confirmed for this vendor: numbers are local estimates (plus rate-limit headers when the vendor sends them).") : null,
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
      reserveLock ? lockNote(reserveLock) : h("button", { type: "submit", class: "small" }, "Save reserve")),
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
  return h("td", {}, input, locked ? h("div", {}, lockNote(locked)) : h("button", { type: "button", class: "small ghost", onclick: save, "aria-label": `Save ${v.vendor} ${w.kind} ${which}` }, "Save"));
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
    h("td", {}, w.source ? badge(SOURCE_LABEL[w.source], "src") : "—", alt ? h("div", { class: "muted" }, alt) : null),
    h("td", { class: "num" }, fmtN(w.burn_per_day)),
    h("td", {}, w.runs_out_at ? fmtT(w.runs_out_at) : w.burn_per_day ? "not before reset" : "—"),
    h("td", {}, fmtT(w.resets_at)),
    h("td", {}, stateBadge(w.state, w.state === "exhausted" ? w.resets_at : null), w.alerts.length ? h("div", { class: "muted" }, `alert ${w.alerts.join("%, ")}%`) : null),
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
  const rows = cfg.vendors.filter((v) => v.id !== "rpc").map((v) => {
    const result = h("div", { class: "muted", "aria-live": "polite" });
    const enabledBox = h("input", { type: "checkbox", id: `en-${v.id}`, checked: v.enabled ? true : null, disabled: v.enabled_locked_by ? true : null });
    enabledBox.addEventListener("change", () => edit([{ path: ["vendors", v.id, "enabled"], value: enabledBox.checked }], `${v.id} ${enabledBox.checked ? "enabled" : "disabled"}`));
    const keys = v.keys.map((k) => {
      const input = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false", id: `key-${v.id}-${k.field}`, placeholder: k.set ? "set (hidden)" : "not set", disabled: k.locked_by ? true : null });
      return h("form", { class: "row", onsubmit: (e) => {
        e.preventDefault();
        if (!input.value) return;
        const value = input.value;
        input.value = ""; // never keep the key in the DOM
        edit([{ path: ["keys", v.id, k.field], value }], `${v.id} ${k.field} saved to secrets.toml`);
      } },
        h("label", { for: input.id, class: "inline" }, h("code", {}, k.env)),
        badge(k.set ? "set" : "missing", k.set ? "ok" : "warn"),
        input,
        k.locked_by ? lockNote(k.locked_by) : h("button", { type: "submit", class: "small" }, "Save"));
    });
    const test = async () => {
      result.textContent = "Testing…";
      try {
        const r = await api(`/admin/api/vendors/${encodeURIComponent(v.id)}/test`, { method: "POST" });
        result.textContent = r.ok ? `OK via ${r.method} in ${r.latency_ms} ms` : `Failed (${r.method || r.status && r.status.status}): ${r.message}`;
        result.className = r.ok ? "badge ok" : "badge bad";
      } catch (e) { result.textContent = ""; handle(e); }
    };
    return h("tr", {},
      h("td", {}, h("b", {}, v.display_name), h("div", { class: "muted mono" }, v.id),
        v.signup_url ? h("div", {}, h("a", { href: v.signup_url, target: "_blank", rel: "noopener noreferrer" }, "Sign up / keys")) : null),
      h("td", {}, badge(v.status.status.replace("_", " "), v.status.status === "active" ? "ok" : v.status.status === "missing_key" ? "warn" : ""),
        v.free_tier_verified ? null : h("div", {}, badge("free tier unverified", "warn"))),
      h("td", {}, h("label", { for: enabledBox.id, class: "inline" }, enabledBox, "enabled"), lockNote(v.enabled_locked_by)),
      h("td", {}, v.keys.length ? keys : h("span", { class: "muted" }, "keyless")),
      h("td", {}, h("button", { type: "button", class: "small ghost", onclick: test }, "Test"), result),
      h("td", { class: "muted" }, v.note || ""),
    );
  });
  return h("section", { class: "panel" },
    h("h1", {}, "Vendors"),
    h("p", { class: "muted" }, "Keys are write-only: they are saved to config/secrets.toml (mode 0600) and never shown again. Keys set by environment variables are locked here. Test makes one cheap call (usage endpoint, eth_blockNumber, getSlot or an FX rate)."),
    h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, ["Vendor", "Status", "Enabled", "API keys", "Test", "Notes"].map((t) => h("th", { scope: "col" }, t)))),
      h("tbody", {}, rows))));
}

// ------------------------------------------------------------------ routing

let routingTab = "default";

async function routing() {
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
  for (const cap of cfg.capabilities) {
    const o = cfg.orders[cap];
    const view = chain ? o.chains[chain.id] : o.default;
    if (!view) continue;
    const locked = chain ? view.locked_by : o.default_locked_by;
    panel.append(orderEditor(cap, view, chain, locked));
  }
  return h("section", { class: "panel" },
    h("h1", {}, "Routing"),
    h("p", { class: "muted" }, "Order = primary, then fallbacks. Most specific wins: operation > chain > defaults > built-in. Drag to reorder (or use the arrow buttons), then save. The effective order shows what routing will actually use right now."),
    tablist, panel);
}

function orderEditor(cap, view, chain, locked) {
  const cfg = cache.config;
  let items = [...view.vendors];
  const list = h("ol", { class: "order", "aria-label": `${cap} order` });
  const known = [...new Set([...view.registered, ...cfg.vendors.map((v) => v.id), ...cfg.custom_rpc.map((c) => c.name)])].sort();
  const addSel = h("select", { "aria-label": `Add vendor to ${cap}` }, h("option", { value: "" }, "add vendor…"));
  const draw = () => {
    list.replaceChildren(...items.map((v, i) => {
      const li = h("li", { draggable: locked ? null : "true", "data-i": String(i) },
        h("span", { class: "pos" }, `${i + 1}.`),
        h("span", { class: "name" }, v),
        view.registered.includes(v) ? null : badge("not registered", "warn"),
        locked ? null : h("button", { type: "button", class: "small ghost", "aria-label": `Move ${v} up`, disabled: i === 0 ? true : null, onclick: () => { [items[i - 1], items[i]] = [items[i], items[i - 1]]; draw(); } }, "↑"),
        locked ? null : h("button", { type: "button", class: "small ghost", "aria-label": `Move ${v} down`, disabled: i === items.length - 1 ? true : null, onclick: () => { [items[i + 1], items[i]] = [items[i], items[i + 1]]; draw(); } }, "↓"),
        locked ? null : h("button", { type: "button", class: "small ghost", "aria-label": `Remove ${v}`, onclick: () => { items.splice(i, 1); draw(); } }, "✕"));
      li.addEventListener("dragstart", (e) => { li.classList.add("dragging"); e.dataTransfer.setData("text/plain", String(i)); e.dataTransfer.effectAllowed = "move"; });
      li.addEventListener("dragend", () => li.classList.remove("dragging"));
      li.addEventListener("dragover", (e) => { e.preventDefault(); li.classList.add("over"); });
      li.addEventListener("dragleave", () => li.classList.remove("over"));
      li.addEventListener("drop", (e) => {
        e.preventDefault();
        const from = Number(e.dataTransfer.getData("text/plain"));
        if (Number.isInteger(from) && from !== i) { const [m] = items.splice(from, 1); items.splice(i, 0, m); draw(); }
      });
      return li;
    }));
    addSel.replaceChildren(h("option", { value: "" }, "add vendor…"), ...known.filter((v) => !items.includes(v)).map((v) => h("option", { value: v }, v)));
  };
  addSel.addEventListener("change", () => { if (addSel.value) { items.push(addSel.value); draw(); } });
  draw();

  const base = chain ? ["routing", "chains", chain.id, cap] : ["routing", "defaults", cap];
  // Remove alias keys (e.g. routing.chains.base) so the canonical CAIP-2 key is the one that applies.
  const aliasClears = chain ? chain.aliases.map((a) => ({ path: ["routing", "chains", a, cap], value: null })) : [];
  const save = () => {
    if (!items.length) return say("An order needs at least one vendor (use Reset to inherit instead).", "err");
    edit([...aliasClears, { path: base, value: items }], `${cap} order saved; routing table swapped`);
  };
  const reset = () => edit([...aliasClears, { path: base, value: null }], `${cap} override removed`);

  const eff = view.effective
    ? h("div", { class: "eff" }, h("span", { class: "muted" }, "Effective: "),
        view.effective.length ? view.effective.map((r) => badge(r.usable ? r.vendor : `${r.vendor}: ${r.reason}`, r.usable ? "ok" : "bad")) : h("span", { class: "muted" }, "none"))
    : h("p", { class: "muted" }, "Chain-bound capability: see the per-chain tabs for the effective order.");
  return h("div", { class: "cap" },
    h("div", { class: "row spread" },
      h("h2", {}, h("code", {}, cap), " ", badge(`from: ${view.level.replace("_", "-")}`, "src")),
      locked ? lockNote(locked) : h("div", { class: "row" }, addSel,
        h("button", { type: "button", class: "small", onclick: save }, "Save"),
        (chain ? view.level === "chain" : view.level === "default") ? h("button", { type: "button", class: "small ghost", onclick: reset }, "Reset to inherited") : null)),
    list, eff,
    view.warnings.length ? h("ul", {}, view.warnings.map((w) => h("li", { class: "lock" }, w))) : null);
}

// ------------------------------------------------------------------ tools

const STRATEGIES = ["", "failover", "quorum", "aggregate", "fan_out", "hedged"];

async function tools() {
  const cfg = cache.config;
  const profileLock = lockedPath("server.tool_profile");
  const profile = h("select", { id: "profile", disabled: profileLock ? true : null },
    ["all", ...cfg.profiles, "custom"].map((p) => h("option", { value: p, selected: cfg.settings.server.tool_profile === p ? true : null }, p)));
  profile.addEventListener("change", () => edit([{ path: ["server", "tool_profile"], value: profile.value }], "Tool profile saved"));
  const rows = cfg.operations.map((op) => {
    const locked = op.locked_by;
    const en = h("input", { type: "checkbox", id: `op-${op.name}`, checked: op.enabled ? true : null, disabled: locked ? true : null });
    en.addEventListener("change", () => edit([{ path: ["operations", op.name, "enabled"], value: en.checked }], `${op.name} ${en.checked ? "enabled" : "disabled"}`));
    const strat = h("select", { "aria-label": `${op.name} strategy`, disabled: locked ? true : null },
      STRATEGIES.map((x) => h("option", { value: x, selected: (op.strategy || "") === x ? true : null }, x || "default")));
    strat.addEventListener("change", () => edit([{ path: ["operations", op.name, "strategy"], value: strat.value || null }], `${op.name} strategy saved`));
    const ttl = h("input", { type: "number", min: "0", "aria-label": `${op.name} cache TTL seconds`, value: op.cache_ttl_secs === null ? "" : String(op.cache_ttl_secs), placeholder: op.default_cache_ttl_secs === null ? "none" : `${op.default_cache_ttl_secs} (default)`, readonly: locked ? true : null });
    const saveTtl = () => edit([{ path: ["operations", op.name, "cache_ttl_secs"], value: ttl.value === "" ? null : Number(ttl.value) }], `${op.name} TTL saved`);
    return h("tr", {},
      h("td", {}, h("code", {}, op.name), h("div", { class: "muted" }, op.description)),
      h("td", {}, op.domain),
      h("td", {}, op.profiles.join(", ")),
      h("td", {}, op.visible ? badge("visible", "ok") : badge("hidden", "")),
      h("td", {}, h("label", { for: en.id, class: "inline" }, en, "enabled")),
      h("td", {}, strat),
      h("td", {}, ttl, locked ? lockNote(locked) : h("button", { type: "button", class: "small ghost", onclick: saveTtl }, "Save")),
    );
  });
  return h("section", { class: "panel" },
    h("h1", {}, "Tools"),
    h("div", { class: "row" }, h("label", { for: "profile" }, "Server tool profile"), profile, lockNote(profileLock)),
    h("p", { class: "muted" }, "The profile keeps each agent's tool list small. In hosted mode, each client key's own profile applies."),
    h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, ["Tool", "Domain", "Profiles", "Visible", "Enabled", "Strategy", "Cache TTL (s)"].map((t) => h("th", { scope: "col" }, t)))),
      h("tbody", {}, rows))));
}

// ------------------------------------------------------------------ chains

async function chains() {
  const cfg = cache.config;
  const rows = cfg.chains.map((c) => {
    const key = c.override_key;
    const en = h("input", { type: "checkbox", id: `ch-${key}`, checked: c.enabled ? true : null, disabled: c.locked_by ? true : null });
    en.addEventListener("change", () => edit([{ path: ["chain_overrides", key, "enabled"], value: en.checked }], `${c.name} ${en.checked ? "enabled" : "disabled"}`));
    const rpcs = h("textarea", { "aria-label": `${c.name} public RPC URLs, one per line`, readonly: c.locked_by ? true : null }, c.public_rpc.join("\n"));
    const saveRpc = () => {
      const list = rpcs.value.split("\n").map((x) => x.trim()).filter(Boolean);
      edit([{ path: ["chain_overrides", key, "public_rpc"], value: list }], `${c.name} public RPCs saved`);
    };
    return h("tr", {},
      h("td", {}, h("b", {}, c.name), h("div", { class: "muted mono" }, c.id), c.aliases.length ? h("div", { class: "muted" }, `aliases: ${c.aliases.join(", ")}`) : null),
      h("td", {}, c.family),
      h("td", {}, h("label", { for: en.id, class: "inline" }, en, "enabled"), lockNote(c.locked_by)),
      h("td", {}, c.finality.policy, c.finality.default ? ` (${c.finality.default})` : "", c.finality.note ? h("div", { class: "muted" }, c.finality.note) : null),
      h("td", {}, rpcs, c.locked_by ? null : h("button", { type: "button", class: "small ghost", onclick: saveRpc }, "Save RPCs")),
      h("td", {}, c.explorer ? h("a", { href: c.explorer, target: "_blank", rel: "noopener noreferrer" }, "explorer") : "—"),
    );
  });
  return h("section", { class: "panel" },
    h("h1", {}, "Chains"),
    h("p", { class: "muted" }, "Keyless public RPCs are the last-resort fallback. Keyed endpoints go in [custom_rpc] or vendor keys, so they stay out of this list."),
    cfg.custom_rpc.length ? h("p", {}, "Custom RPC endpoints: ", cfg.custom_rpc.map((c) => badge(`${c.name} → ${c.chain}`, "src"))) : null,
    h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, ["Chain", "Family", "Enabled", "Finality", "Public RPCs", "Explorer"].map((t) => h("th", { scope: "col" }, t)))),
      h("tbody", {}, rows))));
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
      const code = h("code", {}, r.key);
      keyBox.replaceChildren(h("div", { class: "key-once" },
        h("b", {}, `Key for “${r.client.name}” (${r.client.id}). Copy it now: it is shown once and stored only as a SHA-256 hash.`),
        h("div", {}, code),
        h("button", { type: "button", class: "small", onclick: async () => {
          try { await navigator.clipboard.writeText(r.key); say("Copied", "ok"); } catch { say("Copy failed; select the key manually", "err"); }
        } }, "Copy"),
        h("button", { type: "button", class: "small ghost", onclick: () => { keyBox.replaceChildren(); reloadClients(); } }, "Done")));
    } catch (err) { handle(err); }
  };
  const d = data.defaults;
  const table = h("div", { class: "scroll" }, h("table", {},
    h("thead", {}, h("tr", {}, ["Client", "Status", "Today", "This month", "Top tools", "Limits (override)", ""].map((t) => h("th", { scope: "col" }, t)))),
    h("tbody", {}, data.clients.map(clientRow))));
  return h("div", {},
    h("section", { class: "panel" },
      h("h1", {}, "Clients"),
      data.mode !== "hosted" ? h("p", { class: "msg info" }, "Self-hosted mode: client keys are only enforced when mode = \"hosted\".") : null,
      h("p", { class: "muted" }, `Defaults [clients.default]: ${fmtN(d.requests_per_minute)} req/min · ${fmtN(d.daily_requests)} req/day · ${fmtN(d.monthly_credits)} credits/month · profile ${d.tool_profile || "server"}`, " ", lockNote(data.defaults_locked_by)),
      h("form", { class: "row", onsubmit: create },
        h("label", { for: "client-name" }, "New client name"), name, h("button", { type: "submit" }, "Create key")),
      keyBox),
    h("section", { class: "panel" }, table));
}

function reloadClients() { render(); }

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
    c.active ? h("button", { type: "submit", class: "small" }, "Save") : null);
  const revoke = async () => {
    if (!confirm(`Revoke the key for ${c.name}? Clients using it get 401 immediately.`)) return;
    try { await api(`/admin/api/clients/${encodeURIComponent(c.id)}`, { method: "DELETE" }); say(`${c.name} revoked`, "ok"); render(); } catch (e) { handle(e); }
  };
  return h("tr", {},
    h("td", {}, h("b", {}, c.name), h("div", { class: "muted mono" }, c.id), h("div", { class: "muted" }, `created ${fmtD(c.created_at)}`)),
    h("td", {}, c.active ? badge("active", "ok") : badge(`revoked ${fmtD(c.revoked_at)}`, "bad")),
    h("td", {}, `${fmtN(c.today.requests)} req`, c.today.throttled ? h("div", {}, badge(`${c.today.throttled} throttled`, "warn")) : null),
    h("td", {}, `${fmtN(c.month.requests)} req · ${fmtN(c.month.credits)} credits`, c.month.throttled ? h("div", { class: "muted" }, `${c.month.throttled} throttled`) : null),
    h("td", {}, c.top_tools.length ? h("ol", {}, c.top_tools.map(([t, n]) => h("li", {}, h("span", { class: "mono" }, t || "-"), ` ${fmtN(n)}`))) : h("span", { class: "muted" }, "—")),
    h("td", {}, form),
    h("td", {}, c.active ? h("button", { type: "button", class: "small danger", onclick: revoke }, "Revoke") : null),
  );
}

// ------------------------------------------------------------------ boot

function applyTheme(t) {
  if (t) document.documentElement.setAttribute("data-theme", t);
  else document.documentElement.removeAttribute("data-theme");
}

document.addEventListener("DOMContentLoaded", () => {
  applyTheme(store.getLocal("ems_theme"));
  $("#theme").addEventListener("click", () => {
    const dark = document.documentElement.getAttribute("data-theme") === "dark"
      || (!document.documentElement.getAttribute("data-theme") && matchMedia("(prefers-color-scheme: dark)").matches);
    const next = dark ? "light" : "dark";
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
