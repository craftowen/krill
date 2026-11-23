// Krill Bots mobile screens: #/bots, #/bots/:id, and #/up-next (safe DOM helpers only).
(() => {
const M = (globalThis.KrillMobile ||= {});
M.screens ||= {};
const sync = (parent, nodes) => {
const active = document.activeElement;
const scroll = [];
for (let p = parent; p; p = p.parentElement) scroll.push([p, p.scrollTop, p.scrollLeft]);
const wanted = new Set(nodes);
for (const n of [...parent.children]) if (!wanted.has(n)) n.remove();
let before = null;
for (const n of [...nodes].reverse()) {
if (n.parentElement !== parent || (n.nextSibling !== before && n !== active && !n.contains(active))) {
parent.insertBefore(n, before);
}
before = n;
}
if (active && parent.contains(active) && document.activeElement !== active) {
active.focus({ preventScroll: true });
}
for (const [p, top, left] of scroll) {
p.scrollTop = top;
p.scrollLeft = left;
}
};
function captureAnchor(port, selector) {
if (!(port.clientHeight > 0)) return null;
const nodes = Array.from(port.querySelectorAll(selector));
const focused = nodes.filter((n) => n.contains(document.activeElement)).at(-1);
if (!focused && !(port.scrollTop > 0)) return null;
const top = port.getBoundingClientRect().top;
const node = focused || nodes.find((n) => n.getBoundingClientRect().bottom > top);
return node ? { node, top: node.getBoundingClientRect().top } : null;
}
function restoreAnchor(port, anchor) {
if (anchor && anchor.node.isConnected !== false && port.contains(anchor.node)) {
port.scrollTop += anchor.node.getBoundingClientRect().top - anchor.top;
}
}
const formatText = (ctx) => (s, ...v) =>
v.reduce((out, val, i) => out.replaceAll(`{${i}}`, String(val)), ctx.t?.(s, ...v) ?? s);
const refusalAgentLabel = (agent) => {
const id = String(agent || "").trim().toLowerCase();
if (id === "agy" || id === "antigravity") return "Antigravity";
if (id === "claude") return "Claude Code";
if (id === "codex") return "Codex";
if (id === "opencode" || id === "opencode2") return "OpenCode";
if (id === "cursor") return "Cursor";
if (id === "hermes") return "Hermes";
if (id === "gemini") return "Gemini";
return M.agentName?.(id) || agent || "Agent";
};
/** Reads a structured bot chat refusal carried on an RPC error (`e1779f39`, `5fba3d89`). */
const readBotRefusal = (err) => {
const candidate = err?.data?.refusal || err?.response?.error?.data?.refusal || err?.refusal;
return candidate && typeof candidate === "object" && typeof candidate.code === "string"
? candidate
: null;
};
/** Formats a bot chat refusal in phone-side words (never calling a paired desktop "this computer"). */
const mobileBotRefusalText = (refusal, t) => {
if (!refusal || typeof refusal !== "object") return t("Couldn’t start the chat");
const agent = refusalAgentLabel(refusal.agent);
const detail = typeof refusal.detail === "string" ? refusal.detail.trim() : "";
switch (refusal.code) {
case "cli_too_old":
if (refusal.installed_version && refusal.minimum_version) {
return t(
"Your {0} CLI is {1}. Chats need {2} or newer.",
agent,
refusal.installed_version,
refusal.minimum_version,
);
}
if (refusal.minimum_version) {
return t("Your {0} CLI is too old. Chats need {1} or newer.", agent, refusal.minimum_version);
}
return t("Your {0} CLI is older than chats support.", agent);
case "cli_not_installed":
return t("{0} CLI is not installed on desktop.", agent);
case "bot_paused":
return detail
? t("Bot '{0}' is paused. Resume it to start a chat.", detail)
: t("This bot is paused. Resume it to start a chat.");
case "unknown_agent":
return t("Agent '{0}' is not available on desktop.", agent);
case "invalid_launch_config":
return detail
? t("Couldn’t start {0} chat: {1}", agent, detail)
: t("{0} launch configuration is invalid.", agent);
case "host_unsupported":
return detail ? t(detail) : t("Chats are not supported on desktop.");
case "auth_required":
return t("Sign in to {0} on desktop to start a chat.", agent);
case "quota_exceeded":
return detail
? t("{0} quota reached: {1}", agent, detail)
: t("{0} usage quota has been reached.", agent);
case "spawn_failed":
return detail
? t("Couldn’t start {0} chat: {1}", agent, detail)
: t("Couldn’t start {0} chat.", agent);
default:
return detail ? t(detail) : t("Couldn’t start the chat");
}
};
const safeMsg = (e, t) => {
const refusal = readBotRefusal(e);
if (refusal) return mobileBotRefusalText(refusal, t);
const raw = String(e?.message || e || "").trim();
if (!raw) return t("Request failed. Check the desktop connection and try again.");
if (/os error|sqlite|rusqlite|secrets\.enc|vault|dpapi|\/Users\/|\/home\/|\/var\/|\/tmp\/|\\\\/i.test(raw)) {
return t("Request failed. Check the desktop connection and try again.");
}
return t(raw);
};
const safeHref = (url) => (/^https?:\/\//i.test(String(url || "").trim()) ? String(url).trim() : null);
const isValidColor = (color) => /^#[0-9a-f]{3,8}$/i.test(String(color || "").trim());
const isSafeAvatarSrc = (mime, b64) =>
/^image\/(?:png|jpeg|webp)$/i.test(String(mime || "").trim()) &&
typeof b64 === "string" &&
b64.length > 0 &&
/^[A-Za-z0-9+/=]+$/.test(b64);
const statusTone = (status) => {
if (status === "needs_you") return "waiting";
if (status === "working" || status === "running") return "working";
if (status === "done" || status === "connected" || status === "active") return "connected";
if (status === "error" || status === "blocked") return "blocked";
return "unknown";
};
const statusLabel = (status, t) => {
if (status === "needs_you") return t("Needs You");
if (status === "working") return t("Working");
if (status === "running") return t("Running");
if (status === "planned") return t("Planned");
if (status === "done") return t("Done");
if (status === "canceled") return t("Canceled");
if (status === "connected") return t("Connected");
if (status === "connecting") return t("Connecting…");
if (status === "disconnected") return t("Disconnected");
if (status === "disabled") return t("Disabled");
if (status === "paused") return t("Paused");
if (status === "active") return t("Active");
if (status === "error") return t("Error");
return t(String(status || "Unknown"));
};
const errorCategoryLabel = (category, t) => {
if (!category) return null;
if (category === "signed_out") return t("Signed out");
if (category === "usage_limit") return t("Usage limit reached");
if (category === "not_set_up") return t("Not set up");
if (category === "folder_missing") return t("Folder missing");
if (category === "agent_error") return t("Agent error");
return t(String(category));
};
const formatTimestamp = (ms, t) => {
const n = Number(ms);
if (!Number.isFinite(n) || n <= 0) return t("Never");
try {
const d = new Date(n);
const now = new Date();
const sameDay =
d.getFullYear() === now.getFullYear() &&
d.getMonth() === now.getMonth() &&
d.getDate() === now.getDate();
return sameDay
? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
: d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
} catch {
return t("Never");
}
};
const questionLine = (q) => {
const first = String(q || "")
.split("\n")
.map((l) => l.trim())
.find(Boolean) || "";
return first
.replace(/^[#>\-*+\s]+/, "")
.replace(/\*\*|__|`/g, "")
.trim();
};
const isOwnerActor = (actor) => Boolean(actor && (actor === "owner" || Object.hasOwn(actor, "owner")));
const eventActorName = (ev, bots, t) => {
const a = ev?.actor;
if (isOwnerActor(a)) return t("You");
if (a?.bot?.bot_id) return botDisplayName(bots, a.bot.bot_id);
return t("Auto");
};
const eventStatusTarget = (ev) => {
const raw = String(ev?.text || "");
const parts = raw.split("-> ");
return parts.length > 1 ? parts[parts.length - 1].trim() : null;
};
const answeredDecisionLines = (item, askEv) => {
const decs = Array.isArray(askEv?.decisions) ? askEv.decisions : [];
const answered = decs.map(() => null);
const heads = decs.map((d) => `${questionLine(d.question)} →`);
const events = Array.isArray(item?.events) ? item.events : [];
for (const ev of events) {
if (Number(ev?.id || 0) <= Number(askEv?.id || 0)) continue;
if (ev?.kind !== "commented" || !isOwnerActor(ev?.actor)) continue;
const txt = String(ev.text || "").trim();
if (!txt) continue;
if (decs.length === 1) {
if (!answered[0]) answered[0] = txt;
continue;
}
for (const line of txt.split("\n").map((l) => l.trim())) {
let best = -1;
let bestLen = -1;
heads.forEach((hd, i) => {
if (hd.length > 2 && line.startsWith(hd) && hd.length > bestLen) {
best = i;
bestLen = hd.length;
}
});
if (best >= 0 && !answered[best]) {
const val = line.slice(heads[best].length).trim();
if (val) answered[best] = val;
}
}
}
return answered;
};
const waitingQuestion = (item) => {
if (!item || item.status === "done" || item.status === "canceled") return null;
const events = Array.isArray(item.events) ? item.events : [];
let current = null;
for (let i = 0; i < events.length; i++) {
const ev = events[i];
if (!ev) continue;
if (ev.kind === "status") {
const to = eventStatusTarget(ev);
if (to === "done" || to === "canceled") {
current = null;
} else if (to === "needs_you") {
if (Array.isArray(ev.decisions) && ev.decisions.length) {
current = ev;
} else {
let sameWrite = false;
for (let j = i - 1; j >= 0; j--) {
if (events[j]?.kind === "commented") {
sameWrite = Boolean(current && events[j].id === current.id && events[j].created_at_ms === ev.created_at_ms);
break;
}
}
if (!sameWrite) current = null;
}
}
continue;
}
if (Array.isArray(ev.decisions) && ev.decisions.length) {
current = ev;
}
}
if (!current) return null;
const answered = answeredDecisionLines(item, current);
const openIndices = [];
answered.forEach((a, i) => {
if (!a) openIndices.push(i);
});
if (!openIndices.length) return null;
if (item.status !== "needs_you" && !answered.some(Boolean)) return null;
return { event: current, decisions: current.decisions, answered, openIndices };
};
const latestDecisions = waitingQuestion;
const latestBotContext = (item) => {
const events = Array.isArray(item?.events) ? item.events : [];
let fallback = null;
for (let i = events.length - 1; i >= 0; i--) {
const ev = events[i];
if (!ev || ev.kind !== "commented") continue;
const txt = String(ev.text || "").trim();
if (!txt) continue;
if (!isOwnerActor(ev.actor)) return { event: ev, text: txt };
if (!fallback) fallback = { event: ev, text: txt };
}
return fallback;
};
const formatItemLinkNode = (el, link, t, compact = false) => {
const [kind, val] = Object.entries(link || {})[0] || [];
if (!val) return null;
const raw = String(val);
if (kind === "dispatch") {
if (compact) return null;
const chip = el("span", t("Linked task"), "bot-link-chip muted");
chip.setAttribute("aria-label", `${t("Linked task")}: ${raw}`);
return chip;
}
const href = kind === "url" || kind === "pr" ? safeHref(raw) : null;
if (href) {
let label = raw;
const prMatch = kind === "pr" ? raw.match(/\/pull\/(\d+)/) : null;
if (prMatch) {
label = `PR #${prMatch[1]}`;
} else {
try {
const u = new URL(raw);
label = `${u.hostname}${u.pathname === "/" ? "" : u.pathname}`.slice(0, 42);
} catch {
label = `${kind.toUpperCase()}: ${raw}`;
}
}
const a = el("a", label, "bot-link-chip");
a.href = href;
a.target = "_blank";
a.rel = "noopener noreferrer";
return a;
}
const base = kind === "file" ? raw.split(/[\\/]/).filter(Boolean).pop() || raw : raw;
return el("span", kind === "file" ? `${t("File")}: ${base}` : `${kind}: ${base}`, "bot-link-chip muted");
};
const botDisplayName = (bots, botId) => bots.find((b) => b.id === botId)?.name || botId;
function renderAvatar(el, bot, avatarInfo) {
const hasColor = isValidColor(bot?.avatar_color);
const badge = el("span", undefined, `bot-avatar${hasColor ? " has-color" : ""}`);
if (hasColor) badge.style.backgroundColor = bot.avatar_color;
if (avatarInfo && isSafeAvatarSrc(avatarInfo.mime, avatarInfo.data_base64)) {
const img = el("img", undefined, "bot-avatar-img");
img.alt = "";
img.src = `data:${avatarInfo.mime};base64,${avatarInfo.data_base64}`;
badge.append(img);
return badge;
}
badge.textContent = bot?.emoji || String(bot?.name || "B").slice(0, 1).toUpperCase();
return badge;
}
const rpcHost = (ctx, hostId, req) => {
try {
return Promise.resolve(hostId && ctx.rpcForHost ? ctx.rpcForHost(hostId, req) : ctx.rpc(req));
} catch (e) {
return Promise.reject(e);
}
};
const refreshHost = (ctx, hostId) => {
try {
return Promise.resolve(hostId && ctx.refreshHost ? ctx.refreshHost(hostId) : ctx.refresh?.());
} catch (e) {
return Promise.reject(e);
}
};
const isActivelySnoozed = (item, now = Date.now()) => Number(item?.snoozed_until_ms || 0) > now;
async function fetchBotItemsWithStatus(ctx, hostId, req, noStatusHosts) {
const hKey = hostId ?? "local";
const nextReq = { ...req };
if (nextReq.status && noStatusHosts?.has(hKey)) delete nextReq.status;
try {
return await rpcHost(ctx, hostId, nextReq);
} catch (err) {
if (nextReq.status && err?.definitive) {
noStatusHosts?.add(hKey);
const fallbackReq = { ...nextReq };
delete fallbackReq.status;
return await rpcHost(ctx, hostId, fallbackReq);
}
throw err;
}
}
const openBotDetail = (ctx, botId, hostId, tab) => {
if (hostId && ctx.activateHost) ctx.activateHost(hostId);
if (ctx.state?.ui) {
ctx.state.ui.botPendingTab = tab || "chat";
ctx.state.ui.activeBotGroup = null;
}
ctx.go(`/bots/${encodeURIComponent(botId)}`);
};
/** Ensures a bot's Home Chat starts after an agent switch; resolves with the worded refusal, or null (`5fba3d89`). */
async function ensureBotHomeChat(ctx, t, botId, hostId) {
try {
await rpcHost(ctx, hostId, {
action: "bots.chat.open",
bot_id: botId,
});
return null;
} catch (error) {
const refusal = readBotRefusal(error);
return refusal ? mobileBotRefusalText(refusal, t) : null;
}
}
async function openBotTerminalSession(ctx, t, sessionId, hostId) {
const sid = Number(sessionId);
if (!Number.isSafeInteger(sid) || sid <= 0) throw Error(t("Could not open bot chat"));
if (hostId && ctx.activateHost) ctx.activateHost(hostId);
try {
await refreshHost(ctx, hostId);
} catch {}
if (ctx.signal.aborted) return;
const session = ctx.state.sessions?.get?.(sid);
const group =
(session && ctx.state.groups?.get?.(session.groupKey)) ||
(session ? { key: session.groupKey, sessions: [session] } : null);
if (group) {
const key = ctx.sessionKey?.(sid);
if (key) {
ctx.savePreferences?.({
sessionViewModes: { ...ctx.state.preferences?.sessionViewModes, [key]: "term" },
});
}
ctx.go(ctx.groupRoute(group, sid));
} else {
ctx.go("/projects");
}
}
function sortBotsForDisplay(bots, items, botSessions) {
const liveWork = new Set(
(botSessions || [])
.filter((s) => s.live && s.working)
.map((s) => `${s._hostId ?? ""}:${s.bot_id}`),
);
return bots.slice().sort((a, b) => {
const aNeeds = items.some((i) => i.owner_bot_id === a.id && (i._hostId ?? null) === (a._hostId ?? null) && i.status === "needs_you") ? 0 : 1;
const bNeeds = items.some((i) => i.owner_bot_id === b.id && (i._hostId ?? null) === (b._hostId ?? null) && i.status === "needs_you") ? 0 : 1;
if (aNeeds !== bNeeds) return aNeeds - bNeeds;
const aWork = liveWork.has(`${a._hostId ?? ""}:${a.id}`) ? 0 : 1;
const bWork = liveWork.has(`${b._hostId ?? ""}:${b.id}`) ? 0 : 1;
if (aWork !== bWork) return aWork - bWork;
return Number(!!a.paused) - Number(!!b.paused);
});
}
/** C: Top horizontal avatar quick-switch rail (all bots + groups + + New Bot). */
function renderBotRail(ctx, t, opts) {
const { el, button } = ctx;
const { bots, groups, items, botSessions, avatars, activeBotId, activeGroupId, activeGroupHost, ready, onPickBot, onPickGroup, onNewBot } = opts;
if (!bots.length && !groups.length) return null;
const nav = el("nav", undefined, "bot-rail");
nav.setAttribute("aria-label", t("Quick switch"));
const scroller = el("div", undefined, "bot-rail-scroll");
const sorted = sortBotsForDisplay(bots, items, botSessions);
for (const bot of sorted) {
const needs = items.filter(
(i) => i.owner_bot_id === bot.id && (i._hostId ?? null) === (bot._hostId ?? null) && i.status === "needs_you",
).length;
const working = (botSessions || []).some(
(s) => s.bot_id === bot.id && (s._hostId ?? null) === (bot._hostId ?? null) && s.live && s.working,
);
const tone = bot.paused ? "unknown" : needs > 0 ? "waiting" : working ? "working" : "connected";
const isActive = !activeGroupId && activeBotId === bot.id;
const avKey = bot._hostId ? `${bot._hostId}:${bot.id}` : bot.id;
const itemBtn = button(
"",
() => onPickBot(bot),
`bot-rail-item${isActive ? " active" : ""}${needs > 0 ? " needs" : ""}`,
);
itemBtn.dataset.botRailId = bot.id;
itemBtn.setAttribute("aria-label", needs > 0 ? `${bot.name} (${t("{0} needs you", needs)})` : bot.name);
if (isActive) itemBtn.setAttribute("aria-current", "true");
const ring = el("span", undefined, "bot-rail-ring");
ring.append(renderAvatar(el, bot, avatars.get(avKey)));
ring.append(el("span", undefined, `bot-rail-dot ${tone}`));
if (needs > 0) ring.append(el("span", needs > 99 ? "99+" : String(needs), "bot-rail-badge"));
itemBtn.append(ring, el("span", bot.name, "bot-rail-name"));
scroller.append(itemBtn);
}
for (const g of groups || []) {
const isActive = activeGroupId === g.id && (activeGroupHost === undefined || (activeGroupHost ?? null) === (g._hostId ?? null));
const gBtn = button(
"",
() => onPickGroup(g),
`bot-rail-item is-group${isActive ? " active" : ""}`,
);
gBtn.dataset.groupRailId = g.id;
gBtn.setAttribute("aria-label", g.name);
if (isActive) gBtn.setAttribute("aria-current", "true");
const ring = el("span", undefined, "bot-rail-ring");
const av = el("span", undefined, "bot-avatar bot-group-avatar");
av.append(ctx.icon("chat"));
ring.append(av);
gBtn.append(ring, el("span", g.name, "bot-rail-name"));
scroller.append(gBtn);
}
if (onNewBot) {
const addBtn = button("", onNewBot, "bot-rail-item is-add");
addBtn.setAttribute("aria-label", t("New Bot"));
addBtn.disabled = !ready();
const ring = el("span", undefined, "bot-rail-ring");
const av = el("span", undefined, "bot-avatar bot-add-avatar");
av.append(ctx.icon("plus"));
ring.append(av);
addBtn.append(ring, el("span", t("New Bot"), "bot-rail-name"));
scroller.append(addBtn);
}
nav.append(scroller);
return nav;
}
/** D: One-tap Needs You decision card deck (auto-collapses to null when 0 urgent items). */
function renderDecisionDeck(ctx, t, opts) {
const { el, button } = ctx;
const { cards, bots, ready, deckIndex, onNext, onReload, onOpenBot, compact } = opts;
if (!cards.length) return null;
const idx = ((deckIndex % cards.length) + cards.length) % cards.length;
const cur = cards[idx];
const deck = el("section", undefined, `bot-decision-deck${compact ? " compact" : ""}`);
const card = el(
"article",
undefined,
`bot-decision-card ${cur.kind === "schedule_error" ? "blocked bot-automation-run-row" : "waiting"}`,
);
card.dataset.decisionKind = cur.kind;
const head = el("div", undefined, "bot-decision-card-head");
const kindLabel =
cur.kind === "access"
? t("Access request")
: cur.kind === "gate"
? t("Approval requested")
: cur.kind === "schedule_error"
? t("Failed automation")
: t("Needs attention");
const pill = el("span", `${kindLabel} ${idx + 1}/${cards.length}`, `bot-status-pill ${cur.kind === "schedule_error" ? "blocked" : "waiting"}`);
head.append(pill);
if (cur.botId && onOpenBot) {
head.append(
button(botDisplayName(bots, cur.botId), () => onOpenBot(cur.botId, cur.hostId), "text-button bot-owner-chip"),
);
} else if (cur.hostName) {
head.append(el("span", cur.hostName, "device-badge"));
}
card.append(head);
card.append(el("strong", cur.title, "bot-decision-card-title"));
if (cur.subtitle) {
card.append(
el("p", cur.subtitle, cur.kind === "schedule_error" ? "account-error bot-decision-card-sub" : "muted bot-decision-card-sub"),
);
}
const actions = el("div", undefined, "bot-decision-card-actions");
const finishMutation = async (hostId, toastMsg) => {
if (toastMsg) ctx.toast?.(toastMsg);
await refreshHost(ctx, hostId).catch(() => {});
ctx.syncTabBar?.();
await onReload();
};
if (cur.kind === "item") {
const latest = waitingQuestion(cur.item);
if (latest && latest.openIndices.length) {
const dIdx = latest.openIndices[0];
const firstDec = latest.decisions[dIdx];
if (firstDec.question && firstDec.question !== cur.title) {
card.append(el("p", firstDec.question, "bot-decision-q"));
}
const optsList = Array.isArray(firstDec.options) ? firstDec.options : [];
if (!firstDec.multi && optsList.length > 0) {
optsList.forEach((opt, optIdx) => {
const isRec = firstDec.recommended === optIdx;
const btn = button(
"",
async () => {
if (!ready()) return;
const multiQ = latest.decisions.length > 1;
await rpcHost(ctx, cur.hostId, {
action: "bots.item.answer",
item_id: cur.item.uid,
option_index: optIdx,
decision_id: `${latest.event.id}:${dIdx}`,
answer_text: multiQ ? `${questionLine(firstDec.question)} → ${opt}` : opt,
});
await finishMutation(cur.hostId, t("Answer sent"));
},
`${isRec ? "primary" : "secondary"} small bot-opt-btn`,
);
btn.setAttribute("aria-label", opt);
btn.append(el("span", opt));
if (isRec) btn.append(el("span", t("Recommended"), "bot-rec-badge"));
btn.disabled = !ready();
actions.append(btn);
});
} else {
if (firstDec.multi) head.append(el("span", t("Multi-select"), "bot-status-pill"));
const answerBtn = button(
t("Answer question"),
() => {
if (opts.onOpenItem) {
opts.onOpenItem(cur);
return;
}
openItemDetailSheet(ctx, t, {
item: cur.item,
bots,
multiHost: Boolean(cur.hostName),
ready,
onReload,
draftKey: `bot-item:${cur.hostId ? `${cur.hostId}:${cur.item.uid}` : cur.item.uid}`,
});
},
"primary small",
);
answerBtn.disabled = !ready();
actions.append(answerBtn);
}
} else {
const ctxNote = latestBotContext(cur.item);
if (ctxNote && ctxNote.text !== cur.subtitle) {
card.append(el("p", ctxNote.text, "muted bot-decision-card-sub"));
}
const doneBtn = button(
t("Done"),
async () => {
if (!ready()) return;
await rpcHost(ctx, cur.hostId, {
action: "bots.item.update",
item_id: cur.item.uid,
status: "done",
});
await finishMutation(cur.hostId, t("Done"));
},
"primary small",
);
const snoozeBtn = button(
t("Snooze 4h"),
async () => {
if (!ready()) return;
await rpcHost(ctx, cur.hostId, {
action: "bots.item.update",
item_id: cur.item.uid,
snoozed_until_ms: { set: Date.now() + 4 * 3600 * 1000 },
});
await finishMutation(cur.hostId, t("Snooze 4h"));
},
"secondary small",
);
doneBtn.disabled = snoozeBtn.disabled = !ready();
actions.append(doneBtn, snoozeBtn);
}
} else if (cur.kind === "access") {
const approveBtn = button(
t("Approve"),
async () => {
if (!ready()) return;
await rpcHost(ctx, cur.hostId, { action: "bots.access.decide", token: cur.request.token, approve: true });
await finishMutation(cur.hostId, t("Access approved"));
},
"primary small",
);
const denyBtn = button(
t("Deny"),
async () => {
if (!ready()) return;
await rpcHost(ctx, cur.hostId, { action: "bots.access.decide", token: cur.request.token, approve: false });
await finishMutation(cur.hostId, t("Access declined"));
},
"secondary small",
);
approveBtn.disabled = denyBtn.disabled = !ready();
actions.append(approveBtn, denyBtn);
} else if (cur.kind === "gate") {
(cur.gate.options || []).forEach((opt, optIdx) => {
const b = button(
t(opt),
async () => {
if (!ready()) return;
await rpcHost(ctx, cur.hostId, { action: "gate_reply", id: cur.gate.id, decision: opt });
await finishMutation(cur.hostId, t("Decision sent."));
},
optIdx === 0 ? "primary small" : "secondary small",
);
b.disabled = !ready();
actions.append(b);
});
} else if (cur.kind === "schedule_error") {
const retryBtn = button(
t("Run now"),
async () => {
if (!ready()) return;
try {
await rpcHost(ctx, cur.hostId, {
action: "bots.schedule.run",
bot_id: cur.botId,
schedule_id: cur.scheduleId,
});
await finishMutation(cur.hostId, t("Schedule started"));
} catch (err) {
ctx.toast?.(safeMsg(err, t));
}
},
"primary small",
);
retryBtn.disabled = !ready();
actions.append(retryBtn);
}
if (cards.length > 1 && onNext) {
const nextBtn = button(t("Next"), onNext, "ghost small bot-deck-next");
actions.append(nextBtn);
}
card.append(actions);
deck.append(card);
return deck;
}
function createAccessSection(ctx, t) {
const { el, button } = ctx;
const section = el("section", undefined, "bots-section");
const head = el("div", undefined, "home-section-heading");
const titleEl = el("h2", t("Access & pairing"), "section-title");
const countEl = el("small", "", "muted");
head.append(titleEl, countEl);
const box = el("div", undefined, "inset-group bots-access-card");
const redeemForm = el("form", undefined, "bots-redeem-form");
const hostSelect = el("select");
hostSelect.setAttribute("aria-label", t("Desktop"));
const codeInput = el("input");
codeInput.type = "text";
codeInput.placeholder = t("Enter pairing code");
codeInput.setAttribute("aria-label", t("Enter pairing code"));
let curReady = () => false;
let curReload = async () => {};
let curHosts = [];
let selectedHostId = null;
hostSelect.addEventListener("change", () => {
selectedHostId = hostSelect.value || null;
});
const submitRedeem = async () => {
if (!curReady()) return;
const code = codeInput.value.trim();
if (!code) throw Error(t("Enter pairing code"));
const targetId = curHosts.length > 1 ? selectedHostId : (curHosts[0]?.id ?? null);
await rpcHost(ctx, targetId, { action: "bots.access.redeem", code });
if (ctx.signal.aborted) return;
codeInput.value = "";
ctx.toast?.(t("Pairing code redeemed"));
await curReload();
};
const redeemBtn = button(t("Redeem"), submitRedeem, "secondary small");
redeemForm.addEventListener("submit", (e) => {
e.preventDefault();
ctx.act(submitRedeem);
});
redeemForm.append(codeInput, redeemBtn);
section.append(head, box);
return {
update({ requests, bots, ready, reload, multiHost, targetHosts = [] }) {
curReady = ready;
curReload = reload;
curHosts = targetHosts;
const pending = requests.filter((r) => r.status === "pending");
countEl.hidden = !pending.length;
countEl.textContent = pending.length ? String(pending.length) : "";
const boxNodes = [];
for (const req of pending) {
const row = el("div", undefined, "bots-row");
const copy = el("div", undefined, "bots-row-main");
const meta = [req.source, botDisplayName(bots, req.bot_id), multiHost && req._hostName ? req._hostName : null]
.filter(Boolean)
.join(" · ");
copy.append(
el("strong", req.sender_name || req.sender_id || t("Sender")),
el("small", meta, "muted"),
);
const actions = el("div", undefined, "bots-row-actions");
const decide = async (approve) => {
if (!ready()) return;
await rpcHost(ctx, req._hostId, { action: "bots.access.decide", token: req.token, approve });
ctx.toast?.(approve ? t("Access approved") : t("Access declined"));
await refreshHost(ctx, req._hostId).catch(() => {});
ctx.syncTabBar?.();
await reload();
};
const allow = button(t("Approve"), () => decide(true), "primary small");
const deny = button(t("Deny"), () => decide(false), "ghost small");
allow.disabled = deny.disabled = !ready();
actions.append(allow, deny);
row.append(copy, actions);
boxNodes.push(row);
}
if (targetHosts.length > 1) {
if (!targetHosts.some((h) => (h.id ?? null) === selectedHostId)) {
selectedHostId = targetHosts[0]?.id ?? null;
}
const sig = targetHosts.map((h) => `${h.id ?? ""}:${h.name || ""}`).join("\0");
if (hostSelect.dataset.hostSig !== sig) {
hostSelect.dataset.hostSig = sig;
hostSelect.replaceChildren();
for (const h of targetHosts) {
const opt = el("option", h.name || t("Desktop"));
opt.value = h.id ?? "";
hostSelect.append(opt);
}
}
if (hostSelect.value !== (selectedHostId ?? "")) hostSelect.value = selectedHostId ?? "";
sync(redeemForm, [hostSelect, codeInput, redeemBtn]);
} else {
selectedHostId = targetHosts[0]?.id ?? null;
sync(redeemForm, [codeInput, redeemBtn]);
}
const off = !ready();
hostSelect.disabled = codeInput.disabled = redeemBtn.disabled = off;
boxNodes.push(redeemForm);
sync(box, boxNodes);
return section;
},
};
}
const multiPicks = new Map();
function buildDecisionNodes(ctx, t, opts) {
const { el, button } = ctx;
const { item, disabled, ready, onSubmitAnswer, onRerender } = opts;
const waiting = waitingQuestion(item);
if (!waiting) return [];
const { event, decisions, answered } = waiting;
const multiQ = decisions.length > 1;
const nodes = [];
decisions.forEach((d, dIdx) => {
const group = el("div", undefined, "bot-decision-box");
if (answered[dIdx]) {
group.append(el("p", `${questionLine(d.question)} → ${answered[dIdx]}`, "bot-decision-answered"));
nodes.push(group);
return;
}
const qHead = el("div", undefined, "bot-item-head");
qHead.append(el("p", d.question, "bot-decision-q"));
if (d.multi) qHead.append(el("span", t("Multi-select"), "bot-status-pill"));
group.append(qHead);
const optsList = Array.isArray(d.options) ? d.options : [];
if (!optsList.length) {
nodes.push(group);
return;
}
const btns = el("div", undefined, "bot-decision-options");
const pickKey = `${item._hostId || "local"}:${item.uid}:${event.id}:${dIdx}`;
if (d.multi) {
let picked = multiPicks.get(pickKey);
if (!picked) {
picked = new Set();
multiPicks.set(pickKey, picked);
}
optsList.forEach((opt, idx) => {
const isRec = d.recommended === idx;
const isChecked = picked.has(idx);
const b = button(
"",
() => {
if (disabled || !ready()) return;
if (picked.has(idx)) picked.delete(idx);
else picked.add(idx);
onRerender();
},
`secondary small bot-opt-btn is-multi${isChecked ? " active" : ""}`,
);
b.setAttribute("aria-label", opt);
b.setAttribute("aria-pressed", String(isChecked));
b.append(el("span", `${isChecked ? "✓ " : ""}${opt}`));
if (isRec) b.append(el("span", t("Recommended"), "bot-rec-badge"));
b.disabled = disabled;
btns.append(b);
});
group.append(btns);
const submitLbl = t("Submit ({0})", picked.size);
const submitBtn = button(
submitLbl,
async () => {
if (disabled || !ready()) return;
if (!picked.size) {
ctx.toast?.(t("Select at least one option"));
return;
}
const chosen = [...picked].sort((a, b) => a - b).map((i) => optsList[i]).filter(Boolean);
const joined = chosen.join(", ");
await onSubmitAnswer({
decision_id: `${event.id}:${dIdx}`,
answer_text: multiQ ? `${questionLine(d.question)} → ${joined}` : joined,
});
multiPicks.delete(pickKey);
},
"primary small",
);
submitBtn.disabled = disabled || picked.size === 0;
group.append(submitBtn);
} else {
optsList.forEach((opt, idx) => {
const isRec = d.recommended === idx;
const b = button(
"",
async () => {
if (disabled || !ready()) return;
await onSubmitAnswer({
option_index: idx,
decision_id: `${event.id}:${dIdx}`,
answer_text: multiQ ? `${questionLine(d.question)} → ${opt}` : opt,
});
},
`${isRec ? "primary" : "secondary"} small bot-opt-btn`,
);
b.setAttribute("aria-label", opt);
b.append(el("span", opt));
if (isRec) b.append(el("span", t("Recommended"), "bot-rec-badge"));
b.disabled = disabled;
btns.append(b);
});
group.append(btns);
}
nodes.push(group);
});
return nodes;
}
function openItemDetailSheet(ctx, t, opts) {
const { el, button } = ctx;
const { item, bots, multiHost, ready, onReload, draftKey } = opts;
let curItem = item;
ctx.sheet(curItem.title || t("Item details"), [], "", (body) => {
let busy = false;
const runMutation = async (fn, toastMsg, closeAfter = true) => {
if (busy || !ready()) return;
busy = true;
drawSheet();
try {
const res = await fn();
if (ctx.signal.aborted) return;
if (toastMsg) ctx.toast?.(toastMsg);
const shouldClose = typeof closeAfter === "function" ? closeAfter(res) : closeAfter;
if (shouldClose) ctx.closeSheet?.();
await refreshHost(ctx, curItem._hostId).catch(() => {});
ctx.syncTabBar?.();
await onReload();
} finally {
busy = false;
if (!ctx.signal.aborted) drawSheet();
}
};
const metaRow = el("div", undefined, "bot-sheet-meta");
const statusPill = el("span", statusLabel(curItem.status, t), `bot-status-pill ${statusTone(curItem.status)}`);
metaRow.append(statusPill);
if (multiHost && curItem._hostName) metaRow.append(el("span", curItem._hostName, "device-badge"));
const ownerBtn = button(
botDisplayName(bots, curItem.owner_bot_id),
() => {
ctx.closeSheet?.();
openBotDetail(ctx, curItem.owner_bot_id, curItem._hostId, "chat");
},
"text-button bot-owner-chip",
);
metaRow.append(ownerBtn);
const decisionsBox = el("div", undefined, "bot-item-decisions");
const form = el("form", undefined, "bot-item-form");
const input = el("textarea");
input.rows = 2;
const needsAnswer = () => curItem.status === "needs_you";
input.placeholder = t(needsAnswer() ? "Type your answer…" : "Add a comment…");
input.setAttribute("aria-label", t(needsAnswer() ? "Send answer" : "Comment"));
input.value = ctx.state.drafts?.get?.(draftKey) || "";
let composing = false;
input.addEventListener("compositionstart", () => { composing = true; });
input.addEventListener("compositionend", () => { composing = false; });
input.addEventListener("input", () => ctx.state.drafts?.set?.(draftKey, input.value));
input.addEventListener("keydown", (ev) => {
if (ev.key === "Enter" && (ev.isComposing || composing)) ev.preventDefault();
});
const submitBtn = button(
t(needsAnswer() ? "Send answer" : "Comment"),
() => submitText(),
needsAnswer() ? "primary small" : "secondary small",
);
const sheetVoice = M.voice?.attach?.(input, form, {
ctx,
targetKind: "item",
targetId: () => item.uid,
rpc: (req) => rpcHost(ctx, item._hostId, req),
});
const submitText = () => {
const text = input.value.trim();
if (!text) throw Error(t("Comment cannot be empty"));
sheetVoice?.onSend?.(text);
const answering = needsAnswer();
return runMutation(
async () => {
if (answering) {
const res = await rpcHost(ctx, curItem._hostId, {
action: "bots.item.answer",
item_id: curItem.uid,
answer_text: text,
});
if (res?.item) curItem = { ...res.item, _hostId: curItem._hostId, _hostName: curItem._hostName };
} else {
const res = await rpcHost(ctx, curItem._hostId, {
action: "bots.item.update",
item_id: curItem.uid,
comment: text,
});
if (res?.item) curItem = { ...res.item, _hostId: curItem._hostId, _hostName: curItem._hostName };
}
input.value = "";
ctx.state.drafts?.delete?.(draftKey);
},
t(answering ? "Answer sent" : "Comment"),
true,
);
};
form.addEventListener("submit", (ev) => {
ev.preventDefault();
if (ev.isComposing || composing) return;
ctx.act(submitText);
});
form.append(input, submitBtn);
const isClosed = () => curItem.status === "done" || curItem.status === "canceled";
const isSnoozed = () => Number(curItem.snoozed_until_ms || 0) > Date.now();
const actionsGrid = el("div", undefined, "bot-sheet-actions");
const chatBtn = button(
t("Open chat"),
() => {
ctx.closeSheet?.();
openBotDetail(ctx, curItem.owner_bot_id, curItem._hostId, "chat");
},
"secondary small",
);
const doneBtn = button(
t("Done"),
() => {
const comment = input.value.trim();
return runMutation(
async () => {
await rpcHost(ctx, curItem._hostId, {
action: "bots.item.update",
item_id: curItem.uid,
status: "done",
...(comment ? { comment } : {}),
});
if (comment) {
input.value = "";
ctx.state.drafts?.delete?.(draftKey);
}
},
t("Done"),
true,
);
},
"secondary small",
);
const snoozeLbl = t(isSnoozed() ? "Unsnooze" : "Snooze 4h");
const snoozeBtn = button(
snoozeLbl,
() =>
runMutation(
() =>
rpcHost(ctx, curItem._hostId, {
action: "bots.item.update",
item_id: curItem.uid,
snoozed_until_ms: isSnoozed() ? "clear" : { set: Date.now() + 4 * 3600 * 1000 },
}),
snoozeLbl,
true,
),
"secondary small",
);
const linkForm = el("form", undefined, "bot-item-form");
linkForm.hidden = true;
linkForm.noValidate = true;
const linkInput = el("input");
linkInput.type = "text";
linkInput.placeholder = "https://...";
linkInput.setAttribute("aria-label", t("Link URL"));
const saveLink = () => {
const raw = linkInput.value.trim();
if (!raw) throw Error(t("Enter a link"));
const link = /^https?:\/\//i.test(raw) ? { url: raw } : { file: raw };
return runMutation(
() => rpcHost(ctx, curItem._hostId, { action: "bots.item.link", item_id: curItem.uid, link }),
t("Link added"),
true,
);
};
linkForm.addEventListener("submit", (ev) => {
ev.preventDefault();
ctx.act(saveLink);
});
const saveLinkBtn = button(t("Add link"), saveLink, "primary small");
linkForm.append(linkInput, saveLinkBtn);
const linkBtn = button(
t("Add link"),
() => {
linkForm.hidden = !linkForm.hidden;
if (!linkForm.hidden) linkInput.focus();
},
"secondary small",
);
const cancelBtn = button(
t("Cancel item"),
() =>
runMutation(
() => rpcHost(ctx, curItem._hostId, { action: "bots.item.update", item_id: curItem.uid, status: "canceled" }),
t("Canceled"),
true,
),
"secondary small danger",
);
actionsGrid.append(chatBtn, doneBtn, snoozeBtn, linkBtn, cancelBtn);
const timeline = el("div", undefined, "bot-timeline");
function drawTimeline() {
const events = Array.isArray(curItem.events) ? curItem.events : [];
if (!events.length) {
timeline.replaceChildren(el("p", t("No activity yet."), "muted"));
return;
}
const rows = events.map((ev) => {
const mine = isOwnerActor(ev.actor);
const row = el("div", undefined, `bot-timeline-item${mine ? " mine" : ""}`);
const head = el("div", undefined, "bot-timeline-head");
head.append(el("strong", eventActorName(ev, bots, t)), el("span", formatTimestamp(ev.created_at_ms, t)));
row.append(head);
if (ev.text) row.append(el("p", ev.text, "bot-timeline-body"));
return row;
});
timeline.replaceChildren(...rows);
}
function drawSheet() {
const off = busy || !ready();
statusPill.textContent = statusLabel(curItem.status, t);
statusPill.className = `bot-status-pill ${statusTone(curItem.status)}`;
const decNodes = buildDecisionNodes(ctx, t, {
item: curItem,
disabled: off,
ready,
onRerender: drawSheet,
onSubmitAnswer: (payload) => {
const waitingBefore = waitingQuestion(curItem);
const hadMore = Boolean(waitingBefore && waitingBefore.openIndices.length > 1);
return runMutation(
async () => {
const res = await rpcHost(ctx, curItem._hostId, {
action: "bots.item.answer",
item_id: curItem.uid,
...payload,
});
if (res?.item) {
curItem = { ...res.item, _hostId: curItem._hostId, _hostName: curItem._hostName };
} else if (hadMore && payload?.answer_text) {
const prevEvents = Array.isArray(curItem.events) ? curItem.events : [];
const nextId = prevEvents.reduce((m, e) => Math.max(m, Number(e?.id || 0)), 0) + 1;
curItem = {
...curItem,
events: [
...prevEvents,
{
id: nextId,
kind: "commented",
actor: { owner: { device: "mobile" } },
text: payload.answer_text,
decisions: [],
created_at_ms: Date.now(),
},
],
};
}
return res;
},
t("Answer sent"),
(res) => {
if (res?.item) {
const remaining = waitingQuestion(curItem);
return !(curItem.status === "needs_you" && remaining && remaining.openIndices.length > 0);
}
return !hadMore;
},
);
},
});
decisionsBox.replaceChildren(...decNodes);
decisionsBox.hidden = !decNodes.length;
drawTimeline();
input.disabled = submitBtn.disabled = off;
doneBtn.disabled = snoozeBtn.disabled = cancelBtn.disabled = off || isClosed();
linkBtn.disabled = linkInput.disabled = saveLinkBtn.disabled = off;
}
const nodes = [metaRow];
if (curItem.description) nodes.push(el("p", curItem.description, "bot-item-desc muted"));
if (curItem.done_means) nodes.push(el("p", `${t("Done means")}: ${curItem.done_means}`, "bot-item-done muted"));
const linkChips = (curItem.links || []).map((l) => formatItemLinkNode(el, l, t, false)).filter(Boolean);
if (linkChips.length) {
const lw = el("div", undefined, "bot-item-links");
lw.append(...linkChips);
nodes.push(lw);
}
nodes.push(decisionsBox, form, actionsGrid, linkForm);
nodes.push(el("p", t("Activity"), "sheet-field-label"));
nodes.push(timeline);
body.append(...nodes);
drawSheet();
});
}
function renderItemsList(ctx, t, opts) {
const { el, button } = ctx;
const { items, bots, showBot, ready, onReload, cardMap, multiHost, hideEmpty, container, collapsible, expandedIds, onToggleExpand, sheetDetail } = opts;
const wrap = container || el("div", undefined, "bots-items-list");
if (!items.length) {
sync(wrap, hideEmpty ? [] : [el("p", t("No items match this filter."), "empty")]);
return wrap;
}
const nodes = [];
for (const item of items) {
const cardKey = item._hostId ? `${item._hostId}:${item.uid}` : item.uid;
let entry = cardMap.get(cardKey);
const draftKey = `bot-item:${cardKey}`;
if (!entry) {
const card = el("article", undefined, `bot-item-card${collapsible ? " is-collapsible" : ""}`);
card.dataset.item = item.uid;
const header = el("div", undefined, "bot-item-head");
const headLeft = el("div", undefined, "bot-title-line");
const badge = el("span", undefined, "bot-status-pill");
const hostBadge = el("span", undefined, "device-badge");
headLeft.append(badge, hostBadge);
const headRight = el("div", undefined, "bot-item-head-right");
const botBtn = button(
"",
() => openBotDetail(ctx, entry.current.owner_bot_id, entry.current._hostId, "work"),
"text-button bot-owner-chip",
);
const toggleBtn = button(
t("Details"),
() => entry.onToggle?.(),
"ghost small bot-item-toggle",
);
headRight.append(botBtn);
if (collapsible) headRight.append(toggleBtn);
header.append(headLeft, headRight);
const title = el("strong", undefined, "bot-item-title");
const contextBox = el("div", undefined, "bot-item-context");
const contextHead = el("div", undefined, "bot-item-context-head");
const contextWho = el("strong");
const contextTime = el("span");
contextHead.append(contextWho, contextTime);
const contextText = el("p", undefined, "bot-item-context-text");
contextBox.append(contextHead, contextText);
const decisionsBox = el("div", undefined, "bot-item-decisions");
const detailsWrap = el("div", undefined, "bot-item-details");
const desc = el("p", undefined, "bot-item-desc muted");
const doneMeans = el("p", undefined, "bot-item-done muted");
const links = el("div", undefined, "bot-item-links");
const form = el("form", undefined, "bot-item-form");
const input = el("textarea");
input.rows = 2;
input.placeholder = t("Add a comment or answer…");
input.setAttribute("aria-label", t("Comment"));
input.value = ctx.state.drafts?.get?.(draftKey) || "";
let composing = false;
input.addEventListener("compositionstart", () => { composing = true; });
input.addEventListener("compositionend", () => { composing = false; });
input.addEventListener("input", () => ctx.state.drafts?.set?.(draftKey, input.value));
input.addEventListener("keydown", (ev) => {
if (ev.key === "Enter" && (ev.isComposing || composing)) ev.preventDefault();
});
const controls = el("div", undefined, "bot-item-controls");
const submit = button(t("Comment"), () => entry.onComment(), "secondary small");
const cardVoice = M.voice?.attach?.(input, controls, {
ctx,
targetKind: "item",
targetId: () => entry.current?.uid,
rpc: (req) => rpcHost(ctx, entry.current?._hostId, req),
});
const doneBtn = button(t("Done"), () => entry.onDone(), "primary small");
const snoozeBtn = button(t("Snooze 4h"), () => entry.onSnooze(), "ghost small");
const cancelBtn = button(t("Cancel item"), () => entry.onCancel(), "ghost small");
const linkBtn = button(t("Add link"), () => entry.onLink(), "ghost small");
controls.append(submit, doneBtn, snoozeBtn, linkBtn, cancelBtn);
form.append(input, controls);
const foot = el("div", undefined, "bot-item-foot");
const sheetDetailBtn = button(t("Details"), () => entry.onOpenSheet?.(), "secondary small bot-item-sheet-btn");
const chatJumpBtn = button(t("Open chat"), () => openBotDetail(ctx, entry.current.owner_bot_id, entry.current._hostId, "chat"), "ghost small");
foot.append(sheetDetailBtn, chatJumpBtn);
detailsWrap.append(desc, doneMeans, form);
card.append(header, title, contextBox, links, decisionsBox, detailsWrap);
if (sheetDetail) card.append(foot);
entry = {
card,
badge,
hostBadge,
botBtn,
toggleBtn,
title,
contextBox,
contextWho,
contextTime,
contextText,
detailsWrap,
desc,
doneMeans,
links,
decisionsBox,
form,
input,
controls,
submit,
doneBtn,
snoozeBtn,
cancelBtn,
linkBtn,
foot,
sheetDetailBtn,
chatJumpBtn,
busy: false,
current: item,
onToggle: null,
onOpenSheet: null,
};
cardMap.set(cardKey, entry);
const finishReload = async () => {
await refreshHost(ctx, entry.current._hostId).catch(() => {});
ctx.syncTabBar?.();
await entry.onReload?.();
};
const mutate = async (patch) => {
if (entry.busy || !ready()) return;
entry.busy = true;
entry.sync?.();
try {
await rpcHost(ctx, entry.current._hostId, {
action: "bots.item.update",
item_id: entry.current.uid,
...patch,
});
if (ctx.signal.aborted) return;
if (patch.comment) {
input.value = "";
ctx.state.drafts?.delete?.(draftKey);
}
await finishReload();
} finally {
entry.busy = false;
entry.sync?.();
}
};
const submitAnswer = async (payload, clearInput = false) => {
if (entry.busy || !ready()) return;
entry.busy = true;
entry.sync?.();
try {
await rpcHost(ctx, entry.current._hostId, {
action: "bots.item.answer",
item_id: entry.current.uid,
...payload,
});
if (ctx.signal.aborted) return;
if (clearInput) {
input.value = "";
ctx.state.drafts?.delete?.(draftKey);
}
ctx.toast?.(t("Answer sent"));
await finishReload();
} finally {
entry.busy = false;
entry.sync?.();
}
};
entry.submitAnswer = submitAnswer;
entry.onComment = () => {
const text = input.value.trim();
if (!text) throw Error(t("Comment cannot be empty"));
cardVoice?.onSend?.(text);
if (entry.current.status === "needs_you") {
return submitAnswer({ answer_text: text }, true);
}
return mutate({ comment: text });
};
form.addEventListener("submit", (e) => {
e.preventDefault();
if (e.isComposing || composing) return;
ctx.act(entry.onComment);
});
entry.onDone = () => mutate({ status: "done", ...(input.value.trim() ? { comment: input.value.trim() } : {}) });
entry.onSnooze = () => {
const snoozed = Number(entry.current.snoozed_until_ms || 0) > Date.now();
return mutate({
snoozed_until_ms: snoozed ? "clear" : { set: Date.now() + 4 * 3600 * 1000 },
});
};
entry.onCancel = () => mutate({ status: "canceled" });
entry.onLink = () => {
ctx.sheet(t("Add link"), [], t("Attach a URL or reference to this item."), (body) => {
const urlInput = el("input");
urlInput.type = "text";
urlInput.placeholder = "https://...";
urlInput.setAttribute("aria-label", t("Link URL"));
const add = async () => {
const raw = urlInput.value.trim();
if (!raw) throw Error(t("Enter a link"));
const link = /^https?:\/\//i.test(raw) ? { url: raw } : { file: raw };
await rpcHost(ctx, entry.current._hostId, {
action: "bots.item.link",
item_id: entry.current.uid,
link,
});
ctx.closeSheet?.();
ctx.toast?.(t("Link added"));
await entry.onReload?.();
};
urlInput.addEventListener("keydown", (ev) => {
if (ev.key === "Enter") {
ev.preventDefault();
ctx.act(add);
}
});
body.append(urlInput, button(t("Add link"), add, "primary"));
});
};
}
entry.current = item;
entry.sync = syncItem;
entry.onReload = onReload;
entry.onToggle = collapsible ? () => onToggleExpand?.(cardKey) : null;
entry.onOpenSheet = sheetDetail
? () => openItemDetailSheet(ctx, t, { item: entry.current, bots, multiHost, ready, onReload, draftKey })
: null;
function syncItem() {
const cur = entry.current;
const disabled = entry.busy || !ready();
const waiting = waitingQuestion(cur);
const hasDecisions = Boolean(waiting && waiting.openIndices.length);
const isNeedsTextOnly = cur.status === "needs_you" && !hasDecisions;
const isExpanded = sheetDetail ? isNeedsTextOnly : (!collapsible || !!expandedIds?.has(cardKey));
entry.card.className = `bot-item-card${collapsible ? " is-collapsible" : ""}${cur.status === "needs_you" ? " waiting" : ""}`;
entry.badge.textContent = statusLabel(cur.status, t);
entry.badge.className = `bot-status-pill ${statusTone(cur.status)}`;
entry.hostBadge.hidden = !multiHost || !cur._hostName;
if (!entry.hostBadge.hidden) entry.hostBadge.textContent = cur._hostName;
entry.botBtn.hidden = !showBot;
if (showBot) entry.botBtn.textContent = botDisplayName(bots, cur.owner_bot_id);
entry.toggleBtn.hidden = !collapsible;
if (collapsible) {
const lbl = t(isExpanded ? "Hide" : "Details");
entry.toggleBtn.textContent = lbl;
entry.toggleBtn.setAttribute("aria-label", lbl);
entry.toggleBtn.setAttribute("aria-expanded", String(isExpanded));
}
entry.title.textContent = cur.title || "";
const latestCtx = latestBotContext(cur);
const fallbackCtx = sheetDetail ? (cur.description || cur.done_means || "") : "";
if (latestCtx) {
entry.contextWho.textContent = `${eventActorName(latestCtx.event, bots, t)} · ${t("Latest update")}`;
entry.contextTime.textContent = formatTimestamp(latestCtx.event.created_at_ms, t);
entry.contextText.textContent = latestCtx.text;
entry.contextBox.hidden = false;
} else if (fallbackCtx) {
entry.contextWho.textContent = cur.description ? botDisplayName(bots, cur.owner_bot_id) : t("Done means");
entry.contextTime.textContent = "";
entry.contextText.textContent = fallbackCtx;
entry.contextBox.hidden = false;
} else {
entry.contextBox.hidden = true;
}
const decisionNodes = buildDecisionNodes(ctx, t, {
item: cur,
disabled,
ready,
onRerender: syncItem,
onSubmitAnswer: (payload) => entry.submitAnswer(payload, false),
});
entry.decisionsBox.replaceChildren(...decisionNodes);
entry.decisionsBox.hidden = !decisionNodes.length;
entry.detailsWrap.hidden = !isExpanded;
entry.desc.hidden = sheetDetail || !cur.description;
entry.desc.textContent = cur.description || "";
entry.doneMeans.hidden = sheetDetail || !cur.done_means;
entry.doneMeans.textContent = cur.done_means ? `${t("Done means")}: ${cur.done_means}` : "";
const linkNodes = (cur.links || [])
.map((link) => formatItemLinkNode(el, link, t, Boolean(sheetDetail)))
.filter(Boolean);
entry.links.replaceChildren(...linkNodes);
entry.links.hidden = !linkNodes.length;
const isClosed = cur.status === "done" || cur.status === "canceled";
const isSnoozed = Number(cur.snoozed_until_ms || 0) > Date.now();
const snoozeLabel = t(isSnoozed ? "Unsnooze" : "Snooze 4h");
const submitLbl = t(cur.status === "needs_you" ? "Send answer" : "Comment");
entry.submit.textContent = submitLbl;
entry.submit.setAttribute("aria-label", submitLbl);
entry.submit.className = cur.status === "needs_you" ? "primary small" : "secondary small";
entry.input.placeholder = t(cur.status === "needs_you" ? "Type your answer…" : "Add a comment or answer…");
entry.snoozeBtn.textContent = snoozeLabel;
entry.snoozeBtn.setAttribute("aria-label", snoozeLabel);
entry.doneBtn.hidden = entry.snoozeBtn.hidden = entry.cancelBtn.hidden = entry.linkBtn.hidden = Boolean(sheetDetail);
entry.submit.disabled = disabled;
entry.doneBtn.disabled = disabled || isClosed;
entry.snoozeBtn.disabled = disabled || isClosed;
entry.cancelBtn.disabled = disabled || isClosed;
entry.linkBtn.disabled = disabled;
entry.input.disabled = disabled;
entry.foot.hidden = !sheetDetail;
if (sheetDetail) {
const evCount = Array.isArray(cur.events) ? cur.events.length : 0;
entry.sheetDetailBtn.textContent = evCount ? t("Details ({0})", evCount) : t("Details");
entry.sheetDetailBtn.setAttribute("aria-label", t("Details"));
entry.sheetDetailBtn.disabled = disabled;
entry.chatJumpBtn.disabled = !ready();
}
}
syncItem();
nodes.push(entry.card);
}
sync(wrap, nodes);
return wrap;
}
function renderSuggestionsSection(ctx, t, suggestions, bots, ready, reload, showBot, multiHost, headerAction) {
const { el, button } = ctx;
if (!suggestions.length && !headerAction) return null;
const sec = el("section", undefined, "bots-section");
const head = el("div", undefined, "home-section-heading");
head.append(el("h2", t("Suggestions"), "section-title"));
if (suggestions.length) head.append(el("small", String(suggestions.length), "muted"));
if (headerAction) head.append(headerAction);
const group = el("div", undefined, "inset-group");
if (!suggestions.length) {
group.append(el("p", t("No items match this filter."), "empty"));
}
for (const s of suggestions) {
const row = el("div", undefined, "bots-row");
const main = el("div", undefined, "bots-row-main");
main.append(
el("strong", s.title),
el("small", [showBot ? botDisplayName(bots, s.bot_id) : null, multiHost && s._hostName ? s._hostName : null, s.why].filter(Boolean).join(" · "), "muted"),
);
if (s.ask) main.append(el("small", s.ask, "muted"));
const actions = el("div", undefined, "bots-row-actions");
const startBtn = button(
t("Start"),
async () => {
const res = await rpcHost(ctx, s._hostId, {
action: "bots.suggestions.start",
bot_id: s.bot_id,
suggestion_id: s.id,
});
if (ctx.signal.aborted) return;
if (Number.isSafeInteger(Number(res?.session_id)) && Number(res.session_id) > 0) {
if (s._hostId && ctx.activateHost) ctx.activateHost(s._hostId);
await refreshHost(ctx, s._hostId).catch(() => {});
openBotDetail(ctx, s.bot_id, s._hostId, "chat");
return;
}
ctx.toast?.(t("Suggestion started"));
await reload();
},
"primary small",
);
const dismissBtn = button(
t("Dismiss"),
async () => {
await rpcHost(ctx, s._hostId, {
action: "bots.suggestions.dismiss",
bot_id: s.bot_id,
suggestion_id: s.id,
});
await reload();
},
"ghost small",
);
startBtn.disabled = dismissBtn.disabled = !ready();
actions.append(startBtn, dismissBtn);
row.append(main, actions);
group.append(row);
}
sec.append(head, group);
return sec;
}
function openCreateBotSheet(ctx, t, reload, targetHosts = []) {
const { el, button } = ctx;
let pickedHostId = targetHosts[0]?.id || null;
const agentsFor = (hid) => (targetHosts.find((h) => h.id === hid)?.snapshot?.agents || ctx.state.snapshot?.agents || []);
ctx.sheet(t("New Bot"), [], t("Create an autonomous bot with its own workspace and soul."), (body) => {
const hostSelect = el("select");
hostSelect.setAttribute("aria-label", t("Desktop"));
const agentSelect = el("select");
agentSelect.setAttribute("aria-label", t("Agent"));
const fillAgents = (hid) => {
const rawAgents = agentsFor(hid);
const agentChoices = rawAgents.length
? rawAgents
: [
{ id: "claude", name: "Claude Code" },
{ id: "codex", name: "Codex" },
{ id: "agy", name: "Antigravity" },
];
const prev = agentSelect.value;
agentSelect.replaceChildren();
for (const a of agentChoices) {
const opt = el("option", a.name || a.id);
opt.value = a.id;
agentSelect.append(opt);
}
agentSelect.value = agentChoices.some((a) => a.id === prev) ? prev : agentChoices[0].id;
};
fillAgents(pickedHostId);
if (targetHosts.length > 1) {
for (const h of targetHosts) {
const opt = el("option", h.name);
opt.value = h.id;
hostSelect.append(opt);
}
hostSelect.value = pickedHostId;
hostSelect.addEventListener("change", () => {
pickedHostId = hostSelect.value || null;
fillAgents(pickedHostId);
});
}
const nameInput = el("input");
nameInput.type = "text";
nameInput.placeholder = t("Bot name");
nameInput.setAttribute("aria-label", t("Bot name"));
const tplSelect = el("select");
tplSelect.setAttribute("aria-label", t("Template"));
const blankOpt = el("option", t("Blank bot"));
blankOpt.value = "";
tplSelect.append(blankOpt);
tplSelect.value = "";
const descInput = el("input");
descInput.type = "text";
descInput.placeholder = t("Description (optional)");
descInput.setAttribute("aria-label", t("Description"));
const emojiInput = el("input");
emojiInput.type = "text";
emojiInput.placeholder = t("Emoji (optional)");
emojiInput.setAttribute("aria-label", t("Emoji"));
let templates = [];
rpcHost(ctx, pickedHostId, { action: "bots.templates" })
.then((res) => {
if (ctx.signal.aborted) return;
templates = res?.templates || [];
for (const tpl of templates) {
const opt = el("option", tpl.name);
opt.value = tpl.id;
tplSelect.append(opt);
}
})
.catch(() => {});
tplSelect.addEventListener("change", () => {
const chosen = templates.find((x) => x.id === tplSelect.value);
if (!chosen) return;
if (!nameInput.value.trim()) nameInput.value = chosen.name;
if (!descInput.value.trim()) descInput.value = chosen.description || "";
if (!emojiInput.value.trim() && chosen.mascot) emojiInput.value = chosen.mascot;
});
const create = async () => {
const name = nameInput.value.trim();
if (!name) throw Error(t("Bot name is required"));
await rpcHost(ctx, pickedHostId, {
action: "bots.create",
name,
agent_id: agentSelect.value || "claude",
description: descInput.value.trim(),
...(emojiInput.value.trim() ? { emoji: emojiInput.value.trim() } : {}),
...(tplSelect.value ? { template_id: tplSelect.value } : {}),
});
ctx.closeSheet?.();
ctx.toast?.(t("Bot created"));
try {
await refreshHost(ctx, pickedHostId);
} catch {}
await reload();
};
if (targetHosts.length > 1) {
body.append(el("label", t("Desktop"), "sheet-field-label"), hostSelect);
}
body.append(
el("label", t("Template"), "sheet-field-label"),
tplSelect,
el("label", t("Name"), "sheet-field-label"),
nameInput,
el("label", t("Agent"), "sheet-field-label"),
agentSelect,
el("label", t("Description"), "sheet-field-label"),
descInput,
el("label", t("Emoji"), "sheet-field-label"),
emojiInput,
button(t("Create Bot"), create, "primary"),
);
});
}
function openCreateGroupSheet(ctx, t, bots, reload, targetHosts = []) {
const { el, button } = ctx;
const hostIdsWithBots = new Set(bots.map((b) => b._hostId ?? null));
let pickedHostId =
targetHosts.find((h) => hostIdsWithBots.has(h.id ?? null))?.id ??
targetHosts[0]?.id ??
null;
const botsFor = (hid) => bots.filter((b) => (b._hostId ?? null) === (hid ?? null));
const selected = new Set();
const resetSelection = () => {
selected.clear();
for (const b of botsFor(pickedHostId).slice(0, 2)) selected.add(b.id);
};
resetSelection();
ctx.sheet(t("New Group Chat"), [], t("Coordinate multiple bots in shared threads."), (body) => {
const hostSelect = el("select");
hostSelect.setAttribute("aria-label", t("Desktop"));
const nameInput = el("input");
nameInput.type = "text";
nameInput.placeholder = t("Group name");
nameInput.setAttribute("aria-label", t("Group name"));
const descInput = el("input");
descInput.type = "text";
descInput.placeholder = t("Description (optional)");
descInput.setAttribute("aria-label", t("Description"));
const picks = el("div", undefined, "bot-decision-options");
const renderPicks = () => {
const hostBots = botsFor(pickedHostId);
picks.replaceChildren();
if (!hostBots.length) {
picks.append(el("p", t("No bots configured yet."), "muted"));
return;
}
for (const b of hostBots) {
const chip = button(b.name, () => {
if (selected.has(b.id)) selected.delete(b.id);
else selected.add(b.id);
chip.setAttribute("aria-pressed", String(selected.has(b.id)));
chip.className = selected.has(b.id) ? "filter-chip active" : "filter-chip";
}, selected.has(b.id) ? "filter-chip active" : "filter-chip");
chip.setAttribute("aria-pressed", String(selected.has(b.id)));
picks.append(chip);
}
};
if (targetHosts.length > 1) {
for (const h of targetHosts) {
const opt = el("option", h.name || t("Desktop"));
opt.value = h.id ?? "";
hostSelect.append(opt);
}
hostSelect.value = pickedHostId ?? "";
hostSelect.addEventListener("change", () => {
pickedHostId = hostSelect.value || null;
resetSelection();
renderPicks();
});
}
renderPicks();
const submit = async () => {
const name = nameInput.value.trim();
if (!name) throw Error(t("Group name is required"));
if (!selected.size) throw Error(t("Select at least one bot"));
await rpcHost(ctx, pickedHostId, {
action: "bots.groups.create",
name,
bot_ids: [...selected],
description: descInput.value.trim(),
});
ctx.closeSheet?.();
ctx.toast?.(t("Group created"));
await reload();
};
if (targetHosts.length > 1) {
body.append(el("label", t("Desktop"), "sheet-field-label"), hostSelect);
}
body.append(
nameInput,
descInput,
el("label", t("Bots"), "sheet-field-label"),
picks,
button(t("Create Group"), submit, "primary"),
);
});
}
M.screens.bots = (root, ctx) => {
const { el, button } = ctx;
const t = formatText(ctx);
const botId = ctx.route?.botId || null;
const allHosts = () => ctx.getConnections?.() || [];
const filteredHosts = () => (ctx.filteredConnections ? ctx.filteredConnections() : allHosts());
const botHosts = () => {
const f = filteredHosts();
if (!f.length) {
return ctx.state.connection?.phase === "ready" && !ctx.state.cached && !!ctx.state.snapshot?.capabilities?.bots
? [{ id: null, name: ctx.state.snapshot?.host?.display_name || "", active: true, connection: ctx.state.connection, snapshot: ctx.state.snapshot, cached: false }]
: [];
}
return f.filter((h) => h.connection?.phase === "ready" && !h.cached && h.snapshot?.capabilities?.bots);
};
const ready = () => botHosts().length > 0;
const multiHost = () => allHosts().length > 1;
if (botId) {
return mountBotDetail(root, ctx, t, botId, () => ctx.state.connection?.phase === "ready" && !ctx.state.cached);
}
const screen = el("section", undefined, "details bots screen");
const header = ctx.header("Bots", "/");
header.classList.add("title-header");
const filterPill = ctx.deviceFilterPill ? ctx.deviceFilterPill() : null;
const newBtn = button(t("New Bot"), () => openCreateBotSheet(ctx, t, () => load(true), botHosts()), "icon-button new-bot-btn", "plus");
const refreshBtn = button(t("Refresh"), () => load(true), "icon-button refresh-button", "activity");
if (filterPill) header.append(filterPill);
header.append(newBtn, refreshBtn);
const list = el("div", undefined, "detail-list bots-list");
screen.append(header, list);
root.append(screen);
let bots = [];
const botsCursors = new Map();
const noStatusFilterHosts = new Set();
let items = [];
let requests = [];
let statuses = [];
let groups = [];
let botSessions = [];
let hubMode = "bots";
let activeGroupId = ctx.state?.ui?.activeBotGroup?.id || null;
let activeGroupHost = ctx.state?.ui?.activeBotGroup?.hostId ?? null;
let activeThreadId = null;
let groupView = null;
let deckIndex = 0;
if (ctx.state?.ui?.activeBotGroup) {
hubMode = "groups";
ctx.state.ui.activeBotGroup = null;
}
const avatars = new Map();
const recentRows = new Map();
const recentSec = el("section", undefined, "bots-section bots-recent-section");
const recentHead = el("div", undefined, "home-section-heading");
recentHead.append(el("h2", t("Recent sessions"), "section-title"), button(t("View all"), () => ctx.go("/projects", true), "text-button bots-section-action"));
const recentBox = el("div", undefined, "recent-session-list inset-group");
recentSec.append(recentHead, recentBox);
const accessCtrl = createAccessSection(ctx, t);
// Persistent group chat section & promoted composer so background draw() preserves focus, draft, and reply mode.
const groupSec = el("section", undefined, "bots-section");
const groupHead = el("div", undefined, "home-section-heading");
const groupTitleEl = el("h2", t("Group chats"), "section-title");
const createGrpBtn = button(`+ ${t("New group")}`, () => openCreateGroupSheet(ctx, t, bots, () => load(true), botHosts()), "filter-chip");
groupHead.append(groupTitleEl, createGrpBtn);
const groupListBox = el("div", undefined, "inset-group");
const groupPanel = el("section", undefined, "bot-group-panel bot-group-promoted");
const groupPromotedHead = el("div", undefined, "bot-group-promoted-head");
const groupBackBtn = button(
t("Back to bots"),
() => {
activeGroupId = null;
activeGroupHost = null;
groupView = null;
lastGroupDraftKey = null;
hubMode = "bots";
draw();
},
"ghost small",
"back",
);
const groupPromotedTitleWrap = el("div", undefined, "bots-row-main");
const groupPromotedTitle = el("strong", t("Group chats"));
const groupPromotedMembers = el("small", "", "muted");
groupPromotedTitleWrap.append(groupPromotedTitle, groupPromotedMembers);
groupPromotedHead.append(groupBackBtn, groupPromotedTitleWrap);
const groupTop = el("div", undefined, "bot-group-toolbar");
const threadBar = el("div", undefined, "bot-decision-options");
const groupMsgBox = el("div", undefined, "bot-group-messages promoted");
const groupSendForm = el("form", undefined, "bot-item-form");
const groupMentions = el("div", undefined, "bot-decision-options");
const groupTextarea = el("textarea");
groupTextarea.rows = 2;
groupTextarea.placeholder = t("Message group (@mention bots)…");
groupTextarea.setAttribute("aria-label", t("Group message"));
let groupReplyMode = "mentions";
let lastGroupDraftKey = null;
const currentGroupDraftKey = () => (activeGroupId ? `bot-group:${activeGroupHost || ""}:${activeGroupId}` : null);
groupTextarea.addEventListener("input", () => {
const k = currentGroupDraftKey();
if (k) ctx.state.drafts?.set?.(k, groupTextarea.value);
});
const groupModeSelect = el("select");
groupModeSelect.setAttribute("aria-label", t("Reply mode"));
for (const [val, lbl] of [
["mentions", t("Mentioned bots")],
["all", t("All bots")],
]) {
const opt = el("option", lbl);
opt.value = val;
groupModeSelect.append(opt);
}
groupModeSelect.value = groupReplyMode;
groupModeSelect.addEventListener("change", () => {
groupReplyMode = groupModeSelect.value || "mentions";
});
const submitGroup = async () => {
if (!ready() || !activeGroupId) return;
const text = groupTextarea.value.trim();
if (!text) throw Error(t("Enter a message"));
groupVoice?.onSend?.(text);
const dk = currentGroupDraftKey();
await rpcHost(ctx, activeGroupHost, {
action: "bots.groups.send",
group_id: activeGroupId,
...(activeThreadId ? { thread_id: activeThreadId } : {}),
text,
reply_mode: groupModeSelect.value || groupReplyMode,
});
if (ctx.signal.aborted) return;
groupTextarea.value = "";
if (dk) ctx.state.drafts?.delete?.(dk);
await loadGroup(activeGroupId, activeThreadId, activeGroupHost);
};
const groupSendBtn = button(t("Send"), submitGroup, "primary small");
const groupControls = el("div", undefined, "bot-item-controls");
groupControls.append(groupModeSelect, groupSendBtn);
const groupVoice = M.voice?.attach?.(groupTextarea, groupControls, {
ctx,
targetKind: "group_chat",
targetId: () => activeGroupId,
rpc: (req) => rpcHost(ctx, activeGroupHost, req),
beforeNode: groupSendBtn,
});
groupSendForm.addEventListener("submit", (e) => {
e.preventDefault();
ctx.act(submitGroup);
});
groupSendForm.append(groupMentions, groupTextarea, groupControls);
groupPanel.append(groupPromotedHead, groupTop, groupMsgBox, groupSendForm);
groupSec.append(groupHead, groupListBox);
let loading = false;
let loaded = false;
let failure = null;
let loadSeq = 0;
let lastHostSig = "";
const hostSig = () => botHosts().map((h) => h.id ?? "local").sort().join(",");
const fetchAvatars = (listItems) => {
for (const b of listItems) {
const avKey = b._hostId ? `${b._hostId}:${b.id}` : b.id;
if (b.avatar && !avatars.has(avKey)) {
avatars.set(avKey, null);
rpcHost(ctx, b._hostId, { action: "bots.avatar.read", bot_id: b.id })
.then((av) => {
if (!ctx.signal.aborted && av) {
avatars.set(avKey, av);
draw();
}
})
.catch(() => {});
}
}
};
async function loadGroup(groupId, threadId, hostId = activeGroupHost) {
if (!ready() || !groupId) return;
const res = await rpcHost(ctx, hostId, {
action: "bots.groups.read",
group_id: groupId,
...(threadId ? { thread_id: threadId } : {}),
limit: 60,
});
if (ctx.signal.aborted) return;
groupView = res?.group || null;
activeGroupId = groupView?.group?.id || groupId;
activeGroupHost = hostId ?? null;
activeThreadId =
threadId ||
groupView?.threads?.[0]?.thread_id ||
groupView?.messages?.at?.(-1)?.thread_id ||
null;
draw();
}
async function load(reset = false) {
const targets = botHosts();
lastHostSig = hostSig();
if (!targets.length) {
bots = [];
items = [];
requests = [];
statuses = [];
groups = [];
botSessions = [];
botsCursors.clear();
loaded = true;
draw();
return;
}
const seq = ++loadSeq;
loading = true;
failure = null;
draw();
try {
const settled = await Promise.allSettled(
targets.map(async (h) => {
const [botsRes, itemsRes, accessRes, statusRes, groupRes, sessRes] = await Promise.all([
rpcHost(ctx, h.id, { action: "bots.list", limit: 50 }),
fetchBotItemsWithStatus(
ctx,
h.id,
{ action: "bots.items", include_closed: false, status: "needs_you", limit: 100 },
noStatusFilterHosts,
),
rpcHost(ctx, h.id, { action: "bots.access.list", limit: 50 }),
rpcHost(ctx, h.id, { action: "bots.connectors.status" }),
rpcHost(ctx, h.id, { action: "bots.groups.list", limit: 50 }),
rpcHost(ctx, h.id, { action: "bots.sessions.list" }).catch(() => ({ sessions: [] })),
]);
const tag = (arr) => (h.id ? (arr || []).map((x) => ({ ...x, _hostId: h.id, _hostName: h.name })) : (arr || []));
return {
host: h,
bots: tag(botsRes?.bots),
cursor: botsRes?.next_cursor || null,
items: tag(itemsRes?.items),
requests: tag(accessRes?.requests),
statuses: tag(statusRes?.statuses),
groups: tag(groupRes?.groups),
sessions: tag(sessRes?.sessions),
};
}),
);
if (ctx.signal.aborted || seq !== loadSeq) return;
const perHost = [];
const failedHosts = [];
settled.forEach((outcome, i) => {
if (outcome.status === "fulfilled") {
perHost.push(outcome.value);
} else {
failedHosts.push({ name: targets[i]?.name || "", message: safeMsg(outcome.reason, t) });
}
});
if (perHost.length) {
bots = perHost.flatMap((r) => r.bots);
botsCursors.clear();
for (const r of perHost) {
if (r.cursor) {
botsCursors.set(r.host.id ?? null, {
hostId: r.host.id ?? null,
hostName: r.host.name || "",
cursor: r.cursor,
});
}
}
items = perHost.flatMap((r) => r.items);
requests = perHost.flatMap((r) => r.requests);
statuses = perHost.flatMap((r) => r.statuses);
groups = perHost.flatMap((r) => r.groups);
botSessions = perHost.flatMap((r) => r.sessions);
loaded = true;
const matchedGroup = activeGroupId
? groups.find((g) => g.id === activeGroupId && (!activeGroupHost || (g._hostId ?? null) === (activeGroupHost ?? null)))
: null;
if (matchedGroup) {
activeGroupHost = matchedGroup._hostId ?? null;
await loadGroup(matchedGroup.id, activeThreadId, activeGroupHost).catch(() => {});
}
fetchAvatars(bots);
}
if (failedHosts.length) {
failure = failedHosts
.map((f) => (multiHost() && f.name ? `${f.name}: ${f.message}` : f.message))
.join(" · ");
}
} catch (e) {
if (ctx.signal.aborted || seq !== loadSeq) return;
failure = safeMsg(e, t);
} finally {
if (!ctx.signal.aborted && seq === loadSeq) {
loading = false;
draw();
}
}
}
async function loadMoreBots() {
const nextEntry = botsCursors.values().next().value;
if (!nextEntry || !ready()) return;
const seq = loadSeq;
const { hostId, hostName, cursor } = nextEntry;
const res = await rpcHost(ctx, hostId, { action: "bots.list", limit: 50, cursor });
if (ctx.signal.aborted || seq !== loadSeq) return;
const resolvedName = botHosts().find((h) => (h.id ?? null) === hostId)?.name || hostName;
const seen = new Set(bots.map((b) => `${b._hostId ?? ""}:${b.id}`));
const added = [];
for (const b of res?.bots || []) {
const k = `${hostId ?? ""}:${b.id}`;
if (!seen.has(k)) {
const entry = hostId ? { ...b, _hostId: hostId, _hostName: resolvedName } : { ...b };
bots.push(entry);
added.push(entry);
}
}
if (res?.next_cursor) {
botsCursors.set(hostId, { hostId, hostName: resolvedName, cursor: res.next_cursor });
} else {
botsCursors.delete(hostId);
}
fetchAvatars(added);
draw();
}
function renderGroupPanel() {
createGrpBtn.disabled = !ready() || !bots.length;
const listNodes = [];
if (!groups.length) {
listNodes.push(el("p", t("No group chats yet."), "muted empty"));
} else {
for (const g of groups) {
const isActive = activeGroupId === g.id && (activeGroupHost ?? null) === (g._hostId ?? null);
const row = button(
"",
() => {
if (isActive) {
activeGroupId = null;
activeGroupHost = null;
groupView = null;
lastGroupDraftKey = null;
draw();
} else {
hubMode = "groups";
return loadGroup(g.id, null, g._hostId ?? null);
}
},
`bots-group-row${isActive ? " active" : ""}`,
);
const main = el("span", undefined, "bots-row-main");
const sub = [(g.bot_ids || []).map((id) => botDisplayName(bots, id)).join(", "), multiHost() && g._hostName ? g._hostName : null].filter(Boolean).join(" · ");
main.append(el("strong", g.name), el("small", sub, "muted"));
row.append(main, ctx.icon(isActive ? "chevronDown" : "chevronRight"));
listNodes.push(row);
}
}
sync(groupListBox, listNodes);
if (activeGroupId && groupView) {
groupPromotedTitle.textContent = groupView.group?.name || t("Group chats");
groupPromotedMembers.textContent = (groupView.group?.bot_ids || []).map((id) => botDisplayName(bots, id)).join(", ");
const threadNodes = [];
for (const th of groupView.threads || []) {
const tb = button(
th.title || t("Thread"),
() => loadGroup(activeGroupId, th.thread_id, activeGroupHost),
th.thread_id === activeThreadId ? "filter-chip active" : "filter-chip",
);
tb.disabled = !ready();
threadNodes.push(tb);
}
const newThread = button(
`+ ${t("Thread")}`,
async () => {
const prevIds = new Set((groupView?.threads || []).map((th) => th.thread_id));
const res = await rpcHost(ctx, activeGroupHost, { action: "bots.groups.new_thread", group_id: activeGroupId });
if (res?.group) {
groupView = res.group;
const created = (groupView.threads || []).find((th) => !prevIds.has(th.thread_id));
activeThreadId = created?.thread_id || groupView.threads?.[0]?.thread_id || null;
draw();
} else {
await loadGroup(activeGroupId, null, activeGroupHost);
}
},
"filter-chip",
);
newThread.disabled = !ready();
threadNodes.push(newThread);
sync(threadBar, threadNodes);
const topNodes = [threadBar];
if (groupView.active_run) {
const stopBtn = button(
t("Stop"),
async () => {
await rpcHost(ctx, activeGroupHost, {
action: "bots.groups.stop",
group_id: activeGroupId,
...(activeThreadId ? { thread_id: activeThreadId } : {}),
});
await loadGroup(activeGroupId, activeThreadId, activeGroupHost);
},
"ghost small",
);
stopBtn.disabled = !ready();
topNodes.push(stopBtn);
}
sync(groupTop, topNodes);
const msgNodes = [];
const msgs = groupView.messages || [];
if (!msgs.length) {
msgNodes.push(el("p", t("No messages yet."), "muted"));
} else {
for (const msg of msgs) {
const item = el("div", undefined, "bot-group-msg");
item.append(
el("strong", `${msg.sender_name || msg.sender_id}${msg.in_progress ? ` · ${t("Working")}` : ""}`),
el("p", msg.body || ""),
);
msgNodes.push(item);
}
}
sync(groupMsgBox, msgNodes);
const draftKey = currentGroupDraftKey();
if (draftKey !== lastGroupDraftKey) {
lastGroupDraftKey = draftKey;
groupTextarea.value = ctx.state.drafts?.get?.(draftKey) || "";
}
const mentionNodes = [];
for (const bid of groupView.group?.bot_ids || []) {
const b = bots.find((x) => x.id === bid && (x._hostId ?? null) === (activeGroupHost ?? null)) || bots.find((x) => x.id === bid);
const slug = b?.slug || bid;
const chip = button(`@${slug}`, () => {
groupTextarea.value = `${groupTextarea.value ? `${groupTextarea.value.trimEnd()} ` : ""}@${slug} `;
const dk = currentGroupDraftKey();
if (dk) ctx.state.drafts?.set?.(dk, groupTextarea.value);
groupTextarea.focus();
}, "filter-chip");
mentionNodes.push(chip);
}
sync(groupMentions, mentionNodes);
if (groupModeSelect.value !== groupReplyMode) groupModeSelect.value = groupReplyMode;
groupTextarea.disabled = groupSendBtn.disabled = groupModeSelect.disabled = !ready();
sync(groupSec, [groupHead, groupListBox, groupPanel]);
} else {
sync(groupSec, [groupHead, groupListBox]);
}
return groupSec;
}
function renderRecentSessionsSection(hosts) {
if (!M.homeRecents || !hosts.length) return null;
const values = M.homeRecents(hosts).slice(0, 3);
if (!values.length) return null;
const multi = multiHost();
const nodes = values.map((v) => {
const key = JSON.stringify([v.host.id, v.locator.groupKey, v.session.id]);
let e = recentRows.get(key);
if (!e) {
const row = button(
"",
() => {
const current = M.homeRecents(filteredHosts()).find(
(r) => JSON.stringify([r.host.id, r.locator.groupKey, r.session.id]) === key,
);
if (current) ctx.openRecent(current.host.id, current.locator);
},
"recent-session-row",
);
const copy = el("span", undefined, "needs-copy");
const title = el("strong");
const meta = el("small");
const dot = ctx.dot("Unknown");
row.dataset.host = v.host.id;
row.dataset.session = String(v.session.id);
copy.append(title, meta);
row.append(dot, copy, ctx.icon("chevronRight"));
e = { row, title, meta, dot };
recentRows.set(key, e);
}
const s = v.session;
const agent = M.sessionAgent?.(s) ?? s.agent;
const label =
(M.cleanTitle?.(s.title) ?? s.title) ||
M.sessionPathLabel?.(s, v.host.snapshot?.host?.platform || v.host.platform) ||
`${t("Terminal")} #${s.id}`;
e.title.textContent = label;
e.meta.textContent = [multi ? v.host.name : null, s.group_label, agent ? M.agentName?.(agent) || agent : null]
.filter(Boolean)
.join(" · ");
e.meta.querySelector(".agent-icon")?.remove();
if (agent) e.meta.prepend(M.agentIcon?.(agent, ctx) || ctx.icon("terminal"));
e.dot.className = `status-dot ${String(s.state || "Unknown").toLowerCase()}`;
e.row.disabled = v.host.connection.phase !== "ready";
e.row.setAttribute("aria-label", `${t("Resume session")}: ${label} · ${v.host.name}`);
return e.row;
});
sync(recentBox, nodes);
return recentSec;
}
function buildHubDecisionCards(hosts) {
const cards = [];
for (const item of items) {
if (item.status === "needs_you") {
cards.push({
kind: "item",
botId: item.owner_bot_id,
hostId: item._hostId,
hostName: multiHost() ? item._hostName : null,
title: item.title,
subtitle: item.description || "",
item,
});
}
}
for (const req of requests) {
if (req.status === "pending") {
cards.push({
kind: "access",
botId: req.bot_id,
hostId: req._hostId,
hostName: multiHost() ? req._hostName : null,
title: req.sender_name || req.sender_id || t("Sender"),
subtitle: [req.source, botDisplayName(bots, req.bot_id)].filter(Boolean).join(" · "),
request: req,
});
}
}
for (const h of hosts) {
const o = M.confirmationRequests ? M.confirmationRequests(h.snapshot?.orchestration) : { gates: [] };
for (const g of o.gates || []) {
cards.push({
kind: "gate",
botId: null,
hostId: h.id,
hostName: multiHost() ? h.name : null,
title: g.question,
subtitle: "",
gate: g,
});
}
}
return cards;
}
function draw() {
if (ctx.signal.aborted) return;
const anchor = captureAnchor(list, ".recent-session-row,.bots-section");
const hosts = filteredHosts();
const counts = M.todoCounts ? M.todoCounts(hosts) : { botNeeds: 0, botAccess: 0, gates: 0, questions: 0, sessions: 0, total: 0 };
const liveBotNeeds = loaded ? items.filter((i) => i.status === "needs_you").length : counts.botNeeds;
const liveBotAccess = loaded ? requests.filter((r) => r.status === "pending").length : counts.botAccess;
const totalAttention = liveBotNeeds + liveBotAccess + counts.gates + counts.questions + counts.sessions;
const isLegacyOnly = hosts.length > 0 && !!M.legacyNoBots?.(hosts);
newBtn.disabled = !ready();
refreshBtn.disabled = loading || !ready();
const nodes = [];
if (!ready() && !isLegacyOnly) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
if (failure) nodes.push(el("p", failure, "account-error"));
// C: Top Avatar Quick-Switch Rail
if (!isLegacyOnly && (bots.length || groups.length)) {
const rail = renderBotRail(ctx, t, {
bots,
groups,
items,
botSessions,
avatars,
activeBotId: null,
activeGroupId,
activeGroupHost,
ready,
onPickBot: (b) => openBotDetail(ctx, b.id, b._hostId, "chat"),
onPickGroup: (g) => {
hubMode = "groups";
return loadGroup(g.id, null, g._hostId ?? null);
},
onNewBot: () => openCreateBotSheet(ctx, t, () => load(true), botHosts()),
});
if (rail) nodes.push(rail);
}
const groupOpen = !!(activeGroupId && groupView);
if (groupOpen) {
nodes.push(renderGroupPanel());
if (!isLegacyOnly && ready()) {
nodes.push(accessCtrl.update({ requests, bots, ready, reload: () => load(false), multiHost: multiHost(), targetHosts: botHosts() }));
}
} else {
// Top 1-line conditional banner (shown only when >=1 attention item)
if (totalAttention > 0) {
const bannerBtn = button("", () => ctx.go("/up-next", true), "todo-summary-banner");
const label = t(
totalAttention === 1 ? "{0} request needs your attention" : "{0} requests need your attention",
totalAttention,
);
bannerBtn.setAttribute("aria-label", label);
const dot = ctx.dot("waiting");
const title = el("strong", label, "todo-banner-title");
const tag = el("span", t("Review"), "bot-status-pill waiting");
bannerBtn.append(dot, title, tag, ctx.icon("chevronRight"));
nodes.push(bannerBtn);
}
// D: One-tap Needs You Decision Card Deck (auto-collapses when 0 urgent cards)
if (!isLegacyOnly) {
const deck = renderDecisionDeck(ctx, t, {
cards: buildHubDecisionCards(hosts),
bots,
ready,
deckIndex,
onNext: () => {
deckIndex++;
draw();
},
onReload: () => load(false),
onOpenBot: (bid, hid) => openBotDetail(ctx, bid, hid, "work"),
});
if (deck) nodes.push(deck);
}
if (loading && !bots.length && !isLegacyOnly) {
const loadingBox = el("div", undefined, "account-placeholder");
loadingBox.append(el("span", undefined, "spinner"), el("p", t("Loading bots…")));
nodes.push(loadingBox);
} else if (isLegacyOnly) {
const legacySec = el("section", undefined, "bots-section");
const legacyBox = el("div", undefined, "bots-empty-card inset-group");
legacyBox.append(el("p", t("Update Krill desktop to use Bots."), "empty"));
legacySec.append(legacyBox);
nodes.push(legacySec);
} else {
// A: Hub segment switcher (Your bots / Group chats)
if (bots.length || groups.length) {
const seg = el("div", undefined, "bot-seg-tabs");
seg.setAttribute("role", "tablist");
for (const [id, lbl] of [
["bots", `${t("Your bots")} (${bots.length})`],
["groups", `${t("Group chats")} (${groups.length})`],
]) {
const tabBtn = button(
lbl,
() => {
hubMode = id;
draw();
},
`bot-seg-tab${hubMode === id ? " active" : ""}`,
);
tabBtn.setAttribute("role", "tab");
tabBtn.setAttribute("aria-selected", String(hubMode === id));
tabBtn.dataset.hubMode = id;
seg.append(tabBtn);
}
nodes.push(seg);
}
if (hubMode === "groups") {
nodes.push(renderGroupPanel());
} else {
const botsSec = el("section", undefined, "bots-section");
const botsHead = el("div", undefined, "home-section-heading");
botsHead.append(el("h2", t("Your bots"), "section-title"), el("small", String(bots.length), "muted"));
botsSec.append(botsHead);
if (!bots.length) {
const emptyBox = el("div", undefined, "bots-empty-card inset-group");
emptyBox.append(
el("p", t("No bots configured yet."), "empty"),
button(t("Create Bot"), () => openCreateBotSheet(ctx, t, () => load(true), botHosts()), "primary"),
);
botsSec.append(emptyBox);
} else {
const sortedBots = sortBotsForDisplay(bots, items, botSessions);
const group = el("div", undefined, "inset-group");
const showDev = multiHost();
for (const bot of sortedBots) {
const openItems = items.filter((i) => i.owner_bot_id === bot.id && (i._hostId ?? null) === (bot._hostId ?? null) && i.status === "needs_you").length;
const botErrors = statuses.filter((s) => s.bot_id === bot.id && (s._hostId ?? null) === (bot._hostId ?? null) && s.enabled && s.state === "error").length;
const liveSessions = botSessions.filter((s) => s.bot_id === bot.id && (s._hostId ?? null) === (bot._hostId ?? null) && s.live);
const workingCount = liveSessions.filter((s) => s.working).length;
const avKey = bot._hostId ? `${bot._hostId}:${bot.id}` : bot.id;
const row = el("div", undefined, "bot-card-row");
const tap = button("", () => openBotDetail(ctx, bot.id, bot._hostId, "chat"), "bot-card-main");
tap.setAttribute("aria-label", bot.name);
const avatar = renderAvatar(el, bot, avatars.get(avKey));
const copy = el("span", undefined, "bots-row-main");
const titleLine = el("span", undefined, "bot-title-line");
titleLine.append(
el("strong", bot.name),
el(
"span",
t(bot.paused ? "Paused" : workingCount > 0 ? "Working" : "Active"),
`bot-status-pill ${bot.paused ? "unknown" : workingCount > 0 ? "working" : "connected"}`,
),
);
if (openItems > 0) titleLine.append(el("span", t("{0} needs you", openItems), "bot-status-pill waiting"));
if (botErrors > 0) titleLine.append(el("span", t("Connector error"), "bot-status-pill blocked"));
if (showDev && bot._hostName) titleLine.append(el("span", bot._hostName, "device-badge"));
copy.append(titleLine);
const liveLine = [
refusalAgentLabel(bot.agent_id),
workingCount > 0
? workingCount === 1
? t("1 active")
: t("{0} active", workingCount)
: liveSessions.length > 0
? liveSessions.length === 1
? t("1 live chat")
: t("{0} live chats", liveSessions.length)
: null,
bot.description || null,
]
.filter(Boolean)
.join(" · ");
if (liveLine) copy.append(el("small", liveLine, "muted"));
tap.append(avatar, copy, ctx.icon("chevronRight"));
// Open Chat keeps bot context inside Bot Workspace (#/bots/:id Chat tab)
const chatBtn = button(
t("Open Chat"),
() => openBotDetail(ctx, bot.id, bot._hostId, "chat"),
"secondary small bot-chat-btn",
);
chatBtn.disabled = !ready() || !!bot.paused;
if (bot.paused) chatBtn.title = t("Paused bots cannot start chat");
row.append(tap, chatBtn);
group.append(row);
}
botsSec.append(group);
if (botsCursors.size > 0) {
const more = button(t("Load more"), loadMoreBots, "secondary");
more.disabled = !ready();
botsSec.append(more);
}
}
nodes.push(botsSec);
}
}
// Bottom section: Recent project sessions (max 3 + "View all" -> Projects)
const recentNode = renderRecentSessionsSection(hosts);
if (recentNode) nodes.push(recentNode);
if (!isLegacyOnly && ready()) {
nodes.push(accessCtrl.update({ requests, bots, ready, reload: () => load(false), multiHost: multiHost(), targetHosts: botHosts() }));
}
}
sync(list, nodes);
restoreAnchor(list, anchor);
}
load();
return {
update() {
const nextSig = hostSig();
if (nextSig !== lastHostSig) {
load(false);
} else {
draw();
}
},
};
};
function mountBotDetail(root, ctx, t, botId, ready) {
const { el, button } = ctx;
const screen = el("section", undefined, "details bots bot-detail bot-workspace screen");
const header = ctx.header(botId, "/bots");
const headerSub = el("small", "", "header-sub");
header.querySelector(".header-copy")?.append(headerSub);
const refreshBtn = button(t("Refresh"), () => load(), "icon-button refresh-button", "activity");
header.append(refreshBtn);
const railMount = el("div", undefined, "bot-workspace-rail");
const tabBar = el("div", undefined, "bot-seg-tabs bot-workspace-tabs");
tabBar.setAttribute("role", "tablist");
// Persistent panes so embedded M.chat.mount stays mounted without losing drafts/IME/scroll
const chatPane = el("div", undefined, "bot-workspace-pane bot-chat-pane");
const chatTop = el("div", undefined, "bot-chat-top");
const chatStage = el("div", undefined, "bot-chat-stage");
chatPane.append(chatTop, chatStage);
const list = el("div", undefined, "detail-list bots-list bot-workspace-scroll");
screen.append(header, railMount, tabBar, chatPane, list);
root.append(screen);
let activeTab = ctx.state?.ui?.botPendingTab || "chat";
if (!["chat", "work", "automations", "settings"].includes(activeTab)) activeTab = "chat";
if (ctx.state?.ui) ctx.state.ui.botPendingTab = null;
let bot = null;
let allBots = [];
let allItems = [];
let allGroups = [];
let allBotSessions = [];
let items = [];
let itemsCursor = null;
let statuses = [];
let suggestions = [];
let requests = [];
let upcoming = null;
let botSessions = [];
let activeSessionId = null;
let chatHandle = null;
let chatMountedSid = null;
let chatStarting = false;
let chatRefusal = null;
let deckIndex = 0;
let includeClosed = false;
let includeSnoozed = false;
let soulLoaded = false;
let soulLoading = false;
let soulText = "";
let memoryText = "";
let allowMemoryDelete = false;
let loading = false;
let failure = null;
let loadSeq = 0;
const avatars = new Map();
const cardMap = new Map();
const expandedItems = new Set();
const h1 = header.querySelector("h1");
// Persistent sections/inputs in Bot Workspace so background draw() preserves focus, cursor, scroll, and drafts.
const accessCtrl = createAccessSection(ctx, t);
const itemSec = el("section", undefined, "bots-section");
const itemHead = el("div", undefined, "home-section-heading");
itemHead.append(el("h2", t("Items"), "section-title"));
const addItemBtn = button(`+ ${t("New Item")}`, openCreateItemSheet, "filter-chip");
itemHead.append(addItemBtn);
const itemFilters = el("div", undefined, "bot-decision-options");
const closedBtn = button(t("Include closed"), () => {
includeClosed = !includeClosed;
return load();
}, "filter-chip");
const snoozeBtn = button(t("Include snoozed"), () => {
includeSnoozed = !includeSnoozed;
return load();
}, "filter-chip");
itemFilters.append(closedBtn, snoozeBtn);
const itemsWrap = el("div", undefined, "bots-items-list");
const moreItemsBtn = button(t("Load more"), loadMoreItems, "secondary");
itemSec.append(itemHead, itemFilters, itemsWrap);
const replyDetails = el("details", undefined, "bot-reply-fold");
replyDetails.append(el("summary", t("External reply"), "bot-reply-fold-summary"));
const replyBox = el("div", undefined, "inset-group bot-reply-card");
const replyDraftKey = `bot-reply:${botId}`;
const replyForm = el("form", undefined, "bot-item-form");
const replyInput = el("textarea");
replyInput.rows = 2;
replyInput.placeholder = t("Send reply to latest linked external thread…");
replyInput.setAttribute("aria-label", t("External reply"));
replyInput.value = ctx.state.drafts?.get?.(replyDraftKey) || "";
replyInput.addEventListener("input", () => ctx.state.drafts?.set?.(replyDraftKey, replyInput.value));
const submitReply = async () => {
if (!ready()) return;
const text = replyInput.value.trim();
if (!text) throw Error(t("Enter a reply"));
replyVoice?.onSend?.(text);
await ctx.rpc({ action: "bots.reply", bot_id: botId, text });
if (ctx.signal.aborted) return;
replyInput.value = "";
ctx.state.drafts?.delete?.(replyDraftKey);
ctx.toast?.(t("Reply posted"));
};
const replyBtn = button(t("Send reply"), submitReply, "secondary small");
const replyControls = el("div", undefined, "bot-item-controls");
replyControls.append(replyBtn);
const replyVoice = M.voice?.attach?.(replyInput, replyControls, {
ctx,
targetKind: "bot_chat",
targetId: () => botId,
beforeNode: replyBtn,
});
replyForm.addEventListener("submit", (e) => {
e.preventDefault();
ctx.act(submitReply);
});
replyForm.append(replyInput, replyControls);
replyBox.append(replyForm);
replyDetails.append(replyBox);
const docSec = el("section", undefined, "bots-section");
const docHead = el("div", undefined, "home-section-heading");
docHead.append(el("h2", t("Soul & Memory"), "section-title"));
const docCard = el("div", undefined, "inset-group bot-docs-card");
const soulArea = el("textarea");
soulArea.rows = 4;
soulArea.setAttribute("aria-label", t("Soul"));
soulArea.addEventListener("input", () => (soulText = soulArea.value));
const saveSoul = button(
t("Save Soul"),
async () => {
await ctx.rpc({ action: "bots.soul.set", bot_id: botId, content: soulArea.value });
ctx.toast?.(t("Soul saved"));
},
"secondary small",
);
const memArea = el("textarea");
memArea.rows = 4;
memArea.setAttribute("aria-label", t("Memory"));
memArea.addEventListener("input", () => (memoryText = memArea.value));
const delToggle = button(
t("Protect facts"),
() => {
allowMemoryDelete = !allowMemoryDelete;
draw();
},
"filter-chip",
);
const saveMem = button(
t("Save Memory"),
async () => {
const res = await ctx.rpc({
action: "bots.memory.update",
bot_id: botId,
content: memArea.value,
allow_deletions: allowMemoryDelete,
});
if (ctx.signal.aborted) return;
memoryText = res?.content ?? memoryText;
if (memArea.value !== memoryText) memArea.value = memoryText;
const restored = (res?.restored_facts || []).length;
ctx.toast?.(restored ? t("Memory saved ({0} facts protected)", restored) : t("Memory saved"));
draw();
},
"secondary small",
);
const loadDocsBtn = button(t("Load"), () => loadDocs(), "filter-chip");
docHead.append(loadDocsBtn);
docCard.append(
el("label", "SOUL.md", "sheet-field-label"),
soulArea,
saveSoul,
el("label", "MEMORY.md", "sheet-field-label"),
memArea,
delToggle,
saveMem,
);
docSec.append(docHead);
function unmountChat() {
if (chatHandle) {
chatHandle.unmount?.();
chatHandle = null;
}
chatMountedSid = null;
}
function ensureBotSessionEntry(wantSid) {
if (!Number.isSafeInteger(wantSid) || wantSid <= 0) return;
const existing = ctx.state.sessions?.get?.(wantSid);
if (existing && !existing._synthetic) {
lastKnownBotSession = existing;
return;
}
const s = botSessions.find((x) => Number(x.session_id) === wantSid);
if (lastKnownBotSession && Number(lastKnownBotSession.id) === wantSid) {
ctx.state.sessions?.set?.(wantSid, {
...lastKnownBotSession,
state: s ? (s.working ? "Working" : "Done") : lastKnownBotSession.state,
});
return;
}
if (!existing) {
const key = `bot:${botId}`;
const synth = {
id: wantSid,
agent: bot?.agent_id || "",
title: s?.label || bot?.name || botId,
state: s?.working ? "Working" : "Done",
exited: false,
groupKey: key,
_synthetic: true,
};
ctx.state.sessions?.set?.(wantSid, synth);
}
}
let lastKnownBotSession = null;
let loaded = false;
let wasReady = ready();
const detailNoStatusHosts = new Set();
function ensureChatMounted() {
const wantSid = Number(activeSessionId);
const canMount = Number.isSafeInteger(wantSid) && wantSid > 0 && typeof M.chat?.mount === "function";
if (!canMount) {
unmountChat();
renderChatEmptyState();
return;
}
ensureBotSessionEntry(wantSid);
if (chatMountedSid === wantSid && chatHandle) {
chatHandle.setVisible?.(activeTab === "chat");
chatHandle.update?.();
return;
}
unmountChat();
chatStage.replaceChildren();
chatMountedSid = wantSid;
chatHandle = M.chat.mount(chatStage, {
ctx,
sessionId: wantSid,
agentIcon: M.agentIcon,
visible: activeTab === "chat",
botSession: true,
onSwitchTerm: () => openBotTerminalSession(ctx, t, wantSid, null),
});
}
async function startOrOpenBotChat(promptText) {
if (chatStarting || !ready()) return;
chatStarting = true;
chatRefusal = null;
draw();
try {
const res = await ctx.rpc({
action: "bots.chat.open",
bot_id: botId,
...(promptText ? { prompt: promptText } : {}),
});
if (ctx.signal.aborted) return;
const sid = Number(res?.session_id);
if (!Number.isSafeInteger(sid) || sid <= 0) throw Error(t("Could not open bot chat"));
activeSessionId = sid;
await refreshHost(ctx, null).catch(() => {});
await load();
} catch (error) {
if (ctx.signal.aborted) return;
const refusal = readBotRefusal(error);
const msg = refusal ? mobileBotRefusalText(refusal, t) : safeMsg(error, t);
chatRefusal = msg;
throw Object.assign(Error(msg), { data: error?.data });
} finally {
chatStarting = false;
if (!ctx.signal.aborted) draw();
}
}
function renderChatEmptyState() {
const box = el("div", undefined, "bot-chat-empty");
if (chatRefusal) {
box.append(el("p", chatRefusal, "account-error"));
}
if (bot?.paused) {
box.append(
el("p", t("This bot is paused. Resume it to start a chat."), "muted"),
button(
t("Resume"),
async () => {
await ctx.rpc({ action: "bots.update", bot_id: botId, paused: false });
ctx.toast?.(t("Bot resumed"));
await refreshHost(ctx, null).catch(() => {});
await load();
},
"primary",
),
);
} else {
box.append(el("p", t("Send a message to start chatting with this bot."), "muted"));
const draftKey = `bot-first-chat:${botId}`;
const form = el("form", undefined, "bot-first-chat-form");
const input = el("textarea");
input.rows = 2;
input.placeholder = t("Message this bot…");
input.setAttribute("aria-label", t("Message this bot…"));
input.value = ctx.state.drafts?.get?.(draftKey) || "";
input.addEventListener("input", () => ctx.state.drafts?.set?.(draftKey, input.value));
const sendPrompt = async () => {
const text = input.value.trim();
if (text) firstChatVoice?.onSend?.(text);
await startOrOpenBotChat(text || undefined);
if (!ctx.signal.aborted) {
input.value = "";
ctx.state.drafts?.delete?.(draftKey);
}
};
form.addEventListener("submit", (e) => {
e.preventDefault();
ctx.act(sendPrompt);
});
const row = el("div", undefined, "bot-item-controls");
const startBtn = button(
t(chatStarting ? "Starting chat…" : "Start chat"),
sendPrompt,
"primary small",
);
startBtn.disabled = !ready() || chatStarting;
input.disabled = !ready() || chatStarting;
row.append(startBtn);
const firstChatVoice = M.voice?.attach?.(input, row, {
ctx,
targetKind: "bot_chat",
targetId: () => botId,
beforeNode: startBtn,
});
form.append(input, row);
box.append(form);
}
chatStage.replaceChildren(box);
}
async function loadDocs() {
if (!ready() || soulLoading) return;
soulLoading = true;
try {
const [sRes, mRes] = await Promise.all([
ctx.rpc({ action: "bots.soul.get", bot_id: botId }),
ctx.rpc({ action: "bots.memory.get", bot_id: botId }),
]);
if (ctx.signal.aborted) return;
soulText = sRes?.content || "";
memoryText = mRes?.content || "";
soulArea.value = soulText;
memArea.value = memoryText;
soulLoaded = true;
} finally {
soulLoading = false;
if (!ctx.signal.aborted) draw();
}
}
async function load() {
if (!ready()) {
draw();
return;
}
const seq = ++loadSeq;
loading = true;
failure = null;
draw();
try {
const [botsRes, itemsRes, allItemsRes, statusRes, sugRes, accRes, upRes, sessRes, grpRes] = await Promise.all([
ctx.rpc({ action: "bots.list", limit: 100 }),
ctx.rpc({
action: "bots.items",
bot_id: botId,
include_closed: includeClosed,
include_snoozed: includeSnoozed,
limit: 25,
}),
fetchBotItemsWithStatus(
ctx,
null,
{ action: "bots.items", include_closed: false, status: "needs_you", limit: 100 },
detailNoStatusHosts,
).catch(() => ({ items: [] })),
ctx.rpc({ action: "bots.connectors.status", bot_id: botId }),
ctx.rpc({ action: "bots.suggestions.list", bot_id: botId, include_dismissed: false }),
ctx.rpc({ action: "bots.access.list", bot_id: botId, limit: 50 }),
rpcHost(ctx, null, { action: "bots.upcoming", bot_id: botId }).catch(() => null),
rpcHost(ctx, null, { action: "bots.sessions.list" }).catch(() => ({ sessions: [] })),
rpcHost(ctx, null, { action: "bots.groups.list", limit: 50 }).catch(() => ({ groups: [] })),
]);
if (ctx.signal.aborted || seq !== loadSeq) return;
allBots = botsRes?.bots || [];
bot = allBots.find((b) => b.id === botId) || null;
if (h1 && bot?.name) h1.textContent = bot.name;
items = itemsRes?.items || [];
itemsCursor = itemsRes?.next_cursor || null;
allItems = (allItemsRes?.items || []).length ? allItemsRes.items : items;
statuses = statusRes?.statuses || [];
suggestions = sugRes?.suggestions || [];
requests = accRes?.requests || [];
upcoming = upRes && typeof upRes === "object" ? upRes : null;
allBotSessions = sessRes?.sessions || [];
botSessions = allBotSessions.filter((s) => s.bot_id === botId);
allGroups = grpRes?.groups || [];
const liveForBot = botSessions.filter((s) => s.live);
if (activeSessionId && !liveForBot.some((s) => Number(s.session_id) === Number(activeSessionId))) {
activeSessionId = liveForBot[0]?.session_id ?? null;
} else if (!activeSessionId && liveForBot.length) {
activeSessionId = liveForBot[0].session_id;
}
loaded = true;
for (const b of allBots) {
if (b.avatar && !avatars.has(b.id)) {
avatars.set(b.id, null);
rpcHost(ctx, null, { action: "bots.avatar.read", bot_id: b.id })
.then((av) => {
if (!ctx.signal.aborted && av?.data_base64) {
avatars.set(b.id, av);
draw();
}
})
.catch(() => {});
}
}
} catch (e) {
if (ctx.signal.aborted || seq !== loadSeq) return;
failure = safeMsg(e, t);
} finally {
if (!ctx.signal.aborted && seq === loadSeq) {
loading = false;
draw();
}
}
}
async function loadMoreItems() {
if (!itemsCursor || !ready()) return;
const res = await ctx.rpc({
action: "bots.items",
bot_id: botId,
include_closed: includeClosed,
include_snoozed: includeSnoozed,
limit: 25,
cursor: itemsCursor,
});
if (ctx.signal.aborted) return;
const seen = new Set(items.map((x) => x.uid));
for (const item of res?.items || []) if (!seen.has(item.uid)) items.push(item);
itemsCursor = res?.next_cursor || null;
draw();
}
function openCreateItemSheet() {
ctx.sheet(t("New Item"), [], t("Track a task or decision for this bot."), (body) => {
const titleInput = el("input");
titleInput.type = "text";
titleInput.placeholder = t("Item title");
titleInput.setAttribute("aria-label", t("Item title"));
const descInput = el("textarea");
descInput.rows = 3;
descInput.placeholder = t("Details (optional)");
descInput.setAttribute("aria-label", t("Details"));
const create = async () => {
const title = titleInput.value.trim();
if (!title) throw Error(t("Title is required"));
await ctx.rpc({
action: "bots.item.create",
bot_id: botId,
title,
...(descInput.value.trim() ? { description: descInput.value.trim() } : {}),
status: "planned",
});
ctx.closeSheet?.();
ctx.toast?.(t("Item created"));
await load();
};
body.append(titleInput, descInput, button(t("Create Item"), create, "primary"));
});
}
function openEditAgentSheet() {
if (!bot) return;
ctx.sheet(t("Agent"), [], t("Change this bot's provider, model, or effort."), (body) => {
const snapshotAgents = ctx.state.snapshot?.agents || [];
const baseChoices = [
{ id: "claude", name: "Claude Code" },
{ id: "codex", name: "Codex" },
{ id: "agy", name: "Antigravity" },
{ id: "opencode", name: "OpenCode" },
{ id: "cursor", name: "Cursor CLI" },
{ id: "pi", name: "Pi" },
{ id: "grok", name: "Grok" },
{ id: "omp", name: "OMP" },
];
const choices = [];
const seen = new Set();
for (const a of [...snapshotAgents, ...baseChoices]) {
if (a?.id && !seen.has(a.id)) {
seen.add(a.id);
choices.push({ id: a.id, name: a.name || M.agentName?.(a.id) || a.id });
}
}
const curAgent = bot.agent_id === "antigravity" ? "agy" : bot.agent_id || "claude";
if (!seen.has(curAgent)) choices.unshift({ id: curAgent, name: M.agentName?.(curAgent) || curAgent });
const agentSelect = el("select");
agentSelect.setAttribute("aria-label", t("Agent"));
for (const a of choices) {
const opt = el("option", a.name);
opt.value = a.id;
agentSelect.append(opt);
}
agentSelect.value = curAgent;
const modelSelect = el("select");
modelSelect.setAttribute("aria-label", t("Model"));
const effortSelect = el("select");
effortSelect.setAttribute("aria-label", t("Effort"));
const EFFORT_LABEL = {
"": "Default",
low: "Low",
medium: "Medium",
high: "High",
xhigh: "Extra high",
max: "Max",
};
const fillEfforts = (agentId, selected) => {
const levels =
agentId === "claude"
? ["", "low", "medium", "high", "max"]
: ["codex", "grok", "pi", "omp"].includes(agentId)
? ["", "low", "medium", "high", "xhigh"]
: ["", "low", "medium", "high", "xhigh", "max"];
effortSelect.replaceChildren();
for (const lvl of levels) {
const opt = el("option", t(EFFORT_LABEL[lvl] || lvl));
opt.value = lvl;
effortSelect.append(opt);
}
effortSelect.value = levels.includes(selected) ? selected : "";
};
let modelSeq = 0;
const fillModels = (agentId, selectedModel) => {
const seq = ++modelSeq;
modelSelect.replaceChildren();
const defOpt = el("option", t("Default"));
defOpt.value = "";
modelSelect.append(defOpt);
if (selectedModel) {
const curOpt = el("option", selectedModel);
curOpt.value = selectedModel;
modelSelect.append(curOpt);
modelSelect.value = selectedModel;
} else {
modelSelect.value = "";
}
rpcHost(ctx, null, { action: "bots.models", agent_id: agentId })
.then((res) => {
if (ctx.signal.aborted || seq !== modelSeq) return;
const listItems = Array.isArray(res?.models) ? res.models : [];
modelSelect.replaceChildren();
const d = el("option", t("Default"));
d.value = "";
modelSelect.append(d);
const ids = new Set([""]);
for (const m of listItems) {
if (!m?.id || ids.has(m.id)) continue;
ids.add(m.id);
const opt = el("option", m.label || m.id);
opt.value = m.id;
modelSelect.append(opt);
}
if (selectedModel && !ids.has(selectedModel)) {
const curOpt = el("option", selectedModel);
curOpt.value = selectedModel;
modelSelect.append(curOpt);
}
modelSelect.value = selectedModel || "";
})
.catch(() => {});
};
fillModels(curAgent, bot.model || "");
fillEfforts(curAgent, bot.effort || "");
agentSelect.addEventListener("change", () => {
const next = agentSelect.value || "claude";
fillModels(next, "");
fillEfforts(next, "");
});
const save = async () => {
const nextAgent = agentSelect.value || "claude";
const nextModel = modelSelect.value || "";
const nextEffort = effortSelect.value || "";
const agentChanged = nextAgent !== curAgent;
const res = await ctx.rpc({
action: "bots.update",
bot_id: botId,
agent_id: nextAgent,
...(nextModel ? { model: nextModel } : { clear_model: true }),
...(nextEffort ? { effort: nextEffort } : { clear_effort: true }),
});
if (res?.bot) bot = res.bot;
ctx.closeSheet?.();
if (agentChanged && !bot?.paused) {
const refusal = await ensureBotHomeChat(ctx, t, botId);
ctx.toast?.(refusal || t("Saved"), refusal ? 3000 : undefined);
} else {
ctx.toast?.(t("Saved"));
}
await refreshHost(ctx, null).catch(() => {});
await load();
};
body.append(
el("label", t("Agent"), "sheet-field-label"),
agentSelect,
el("label", t("Model"), "sheet-field-label"),
modelSelect,
el("label", t("Effort"), "sheet-field-label"),
effortSelect,
button(t("Save"), save, "primary"),
);
});
}
function buildBotDecisionCards() {
const cards = [];
const now = Date.now();
for (const item of items) {
if (item.status === "needs_you" && !isActivelySnoozed(item, now)) {
cards.push({
kind: "item",
botId,
hostId: null,
title: item.title,
subtitle: item.description || "",
item,
});
}
}
for (const req of requests) {
if (req.status === "pending") {
cards.push({
kind: "access",
botId,
hostId: null,
title: req.sender_name || req.sender_id || t("Sender"),
subtitle: req.source || "",
request: req,
});
}
}
for (const run of upcoming?.schedules || []) {
if (run?.last_refusal || run?.last_error) {
const cfg = (bot?.schedules || []).find((s) => s.id === run.id);
cards.push({
kind: "schedule_error",
botId,
hostId: null,
scheduleId: run.id,
title: cfg?.name || run.id,
subtitle: run.last_refusal
? mobileBotRefusalText(run.last_refusal, t)
: String(run.last_error || ""),
});
}
}
return cards;
}
function renderChatTopArea() {
const topNodes = [];
if (!ready()) topNodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
if (failure) topNodes.push(el("p", failure, "account-error"));
// Thread / session switcher bar + New Chat + Open in Terminal
const threadBar = el("div", undefined, "bot-thread-bar");
const liveSessions = botSessions.filter((s) => s.live);
liveSessions.forEach((s, idx) => {
const sid = Number(s.session_id);
const lbl = idx === 0 ? t("Home Chat") : s.label || `#${sid}`;
const chip = button(
lbl,
() => {
activeSessionId = sid;
draw();
},
`filter-chip bot-thread-chip${Number(activeSessionId) === sid ? " active" : ""}`,
);
if (Number(activeSessionId) === sid) chip.setAttribute("aria-current", "true");
threadBar.append(chip);
});
const newChatBtn = button(
`+ ${t("New Chat")}`,
() => startOrOpenBotChat(),
"filter-chip bot-new-chat-btn",
);
newChatBtn.disabled = !ready() || !!bot?.paused || chatStarting;
threadBar.append(newChatBtn);
if (Number.isSafeInteger(Number(activeSessionId)) && Number(activeSessionId) > 0) {
const termBtn = button(
t("Terminal"),
() => openBotTerminalSession(ctx, t, activeSessionId, null),
"ghost small bot-term-btn",
"terminal",
);
termBtn.setAttribute("aria-label", t("Open in terminal"));
threadBar.append(termBtn);
}
topNodes.push(threadBar);
// Worker / activity progress strip
const workingSessions = liveSessions.filter((s) => s.working);
const orch = M.confirmationRequests ? M.confirmationRequests(ctx.state.snapshot?.orchestration) : { tasks: [] };
const runningTasks = (orch.tasks || []).filter((tk) => tk.status === "running" || tk.status === "in_progress");
if (workingSessions.length || runningTasks.length) {
const strip = el("div", undefined, "bot-worker-strip");
strip.append(ctx.dot("working"));
const primary = workingSessions[0];
const summary = workingSessions.length
? `${t("Active workers")} · ${workingSessions.length === 1 ? t("1 active") : t("{0} active", workingSessions.length)}`
: `${t("Active workers")} · ${runningTasks[0].title}`;
strip.append(el("span", summary, "bot-worker-copy"));
if (primary) {
const stopBtn = button(
t("Stop"),
async () => {
await ctx.rpc({ action: "bots.sessions.stop", bot_id: botId, session_id: Number(primary.session_id) });
ctx.toast?.(t("Session stopped"));
await load();
},
"ghost small",
);
stopBtn.disabled = !ready();
strip.append(stopBtn);
}
topNodes.push(strip);
}
// D: Bot-scoped One-Tap Decision Card Deck right above chat feed
const deck = renderDecisionDeck(ctx, t, {
cards: buildBotDecisionCards(),
bots: allBots.length ? allBots : bot ? [bot] : [],
ready,
deckIndex,
compact: true,
onNext: () => {
deckIndex++;
draw();
},
onReload: () => load(),
});
if (deck) topNodes.push(deck);
sync(chatTop, topNodes);
}
function renderWorkNodes() {
const nodes = [];
if (!ready()) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
if (failure) nodes.push(el("p", failure, "account-error"));
// Persistent Items section with collapsible summary cards
addItemBtn.disabled = !ready();
closedBtn.className = includeClosed ? "filter-chip active" : "filter-chip";
snoozeBtn.className = includeSnoozed ? "filter-chip active" : "filter-chip";
closedBtn.disabled = snoozeBtn.disabled = !ready();
renderItemsList(ctx, t, {
items,
bots: bot ? [bot] : [],
showBot: false,
ready,
onReload: load,
cardMap,
container: itemsWrap,
collapsible: true,
expandedIds: expandedItems,
onToggleExpand: (key) => {
if (expandedItems.has(key)) expandedItems.delete(key);
else expandedItems.add(key);
draw();
},
});
moreItemsBtn.disabled = !ready();
sync(itemSec, itemsCursor ? [itemHead, itemFilters, itemsWrap, moreItemsBtn] : [itemHead, itemFilters, itemsWrap]);
nodes.push(itemSec);
// Suggestions section with Look Around trigger
const lookBtn = button(
t("Look Around"),
async () => {
const res = await ctx.rpc({ action: "bots.look_around", bot_id: botId });
ctx.toast?.(t("{0} suggestions ready", (res?.suggestions || []).length));
await load();
},
"secondary small",
);
lookBtn.disabled = !ready() || !!bot?.paused;
const sugNode = renderSuggestionsSection(ctx, t, suggestions, bot ? [bot] : [], ready, load, false, false, lookBtn);
if (sugNode) nodes.push(sugNode);
if (requests.some((r) => r.status === "pending")) {
nodes.push(accessCtrl.update({ requests, bots: bot ? [bot] : [], ready, reload: load, multiHost: false }));
}
// Persistent collapsible External Reply (keeps textarea identity, focus, and draft across draw)
replyInput.disabled = replyBtn.disabled = !ready();
nodes.push(replyDetails);
return nodes;
}
function renderAutomationsNodes() {
const nodes = [];
if (!ready()) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
if (failure) nodes.push(el("p", failure, "account-error"));
// Proactive Look Around card
const laSec = el("section", undefined, "bots-section");
laSec.append(el("h2", t("Look Around"), "section-title"));
const laBox = el("div", undefined, "inset-group");
const laRow = el("div", undefined, "bots-row");
const laMain = el("div", undefined, "bots-row-main");
laMain.append(
el("strong", t("Look Around")),
el(
"small",
`${t("Last run")}: ${formatTimestamp(upcoming?.look_around_last_ms, t)} · ${t("Next run")}: ${formatTimestamp(upcoming?.look_around_next_ms, t)}`,
"muted",
),
);
const laBtn = button(
t("Run now"),
async () => {
const res = await ctx.rpc({ action: "bots.look_around", bot_id: botId });
ctx.toast?.(t("{0} suggestions ready", (res?.suggestions || []).length));
await load();
},
"secondary small",
);
laBtn.disabled = !ready() || !!bot?.paused;
laRow.append(laMain, laBtn);
laBox.append(laRow);
laSec.append(laBox);
nodes.push(laSec);
// Full Scheduled Automations list (failed + running + active + paused)
const autoSec = el("section", undefined, "bots-section");
const cfgSchedules = Array.isArray(bot?.schedules) ? bot.schedules : [];
const upMap = new Map((upcoming?.schedules || []).map((u) => [u.id, u]));
const merged = [];
const seenIds = new Set();
for (const cfg of cfgSchedules) {
seenIds.add(cfg.id);
merged.push({ cfg, run: upMap.get(cfg.id) || null });
}
for (const run of upcoming?.schedules || []) {
if (!seenIds.has(run.id)) merged.push({ cfg: null, run });
}
merged.sort((a, b) => {
const aFail = a.run?.last_refusal || a.run?.last_error ? 0 : 1;
const bFail = b.run?.last_refusal || b.run?.last_error ? 0 : 1;
if (aFail !== bFail) return aFail - bFail;
const aRun = a.run?.running ? 0 : 1;
const bRun = b.run?.running ? 0 : 1;
if (aRun !== bRun) return aRun - bRun;
return Number(a.cfg?.enabled === false) - Number(b.cfg?.enabled === false);
});
const autoHead = el("div", undefined, "home-section-heading");
autoHead.append(el("h2", t("Automations"), "section-title"), el("small", String(merged.length), "muted"));
autoSec.append(autoHead);
const group = el("div", undefined, "inset-group");
if (!merged.length) {
group.append(el("p", t("No scheduled automations configured."), "empty"));
} else {
for (const entry of merged) {
const { cfg, run } = entry;
const sid = cfg?.id || run?.id || "";
const failed = !!(run?.last_refusal || run?.last_error);
const running = !!run?.running;
const enabled = cfg ? cfg.enabled !== false : true;
const row = el("div", undefined, `bots-row bot-automation-row${failed ? " bot-automation-run-row" : ""}`);
const main = el("div", undefined, "bots-row-main");
const titleLine = el("div", undefined, "bot-title-line");
titleLine.append(
el("strong", cfg?.name || sid),
el(
"span",
t(failed ? "Error" : running ? "Running" : enabled ? "Active" : "Paused"),
`bot-status-pill ${failed ? "blocked" : running ? "working" : enabled ? "connected" : "unknown"}`,
),
);
main.append(titleLine);
const metaParts = [
cfg?.cron || null,
run?.next_ms ? `${t("Next run")}: ${formatTimestamp(run.next_ms, t)}` : null,
run?.last_ms ? `${t("Last run")}: ${formatTimestamp(run.last_ms, t)}` : null,
].filter(Boolean);
if (metaParts.length) main.append(el("small", metaParts.join(" · "), "muted"));
if (failed) {
const errText = run.last_refusal
? mobileBotRefusalText(run.last_refusal, t)
: String(run.last_error || "");
main.append(el("small", errText, "account-error"));
}
const actions = el("div", undefined, "bots-row-actions");
const runBtn = button(
t("Run now"),
async () => {
try {
await ctx.rpc({ action: "bots.schedule.run", bot_id: botId, schedule_id: sid });
ctx.toast?.(t("Schedule started"));
await load();
} catch (err) {
ctx.toast?.(safeMsg(err, t));
}
},
"primary small",
);
runBtn.disabled = !ready() || running;
actions.append(runBtn);
if (cfg) {
const toggleBtn = button(
t(enabled ? "Pause" : "Enable"),
async () => {
const res = await ctx.rpc({
action: "bots.schedule.toggle",
bot_id: botId,
schedule_id: sid,
enabled: !enabled,
});
if (res?.bot) bot = res.bot;
ctx.toast?.(t("Schedule updated"));
await load();
},
"secondary small",
);
toggleBtn.disabled = !ready();
actions.append(toggleBtn);
}
row.append(main, actions);
group.append(row);
}
}
autoSec.append(group);
nodes.push(autoSec);
return nodes;
}
function renderSettingsNodes() {
const nodes = [];
if (!ready()) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
if (failure) nodes.push(el("p", failure, "account-error"));
if (bot) {
const cfgSec = el("section", undefined, "bots-section");
cfgSec.append(el("h2", t("Settings"), "section-title"));
const cfgGroup = el("div", undefined, "inset-group");
// Bot Status + Pause / Resume
const statusRow = el("div", undefined, "bots-row");
const statusMain = el("div", undefined, "bots-row-main");
statusMain.append(
el("strong", t("Bot status")),
el("small", t(bot.paused ? "Paused" : "Active"), "muted"),
);
const pauseBtn = button(
t(bot.paused ? "Resume" : "Pause"),
async () => {
const nextPaused = !bot.paused;
const res = await ctx.rpc({ action: "bots.update", bot_id: botId, paused: nextPaused });
if (res?.bot) bot = res.bot;
ctx.toast?.(t(nextPaused ? "Bot paused" : "Bot resumed"));
await refreshHost(ctx, null).catch(() => {});
await load();
},
bot.paused ? "primary small" : "secondary small",
);
pauseBtn.disabled = !ready();
statusRow.append(statusMain, pauseBtn);
cfgGroup.append(statusRow);
// Provider & Model (supports agy / Antigravity, claude, codex, etc.)
const agentRow = el("div", undefined, "bots-row");
const agentMain = el("div", undefined, "bots-row-main");
agentMain.append(
el("strong", t("Provider & model")),
el(
"small",
[refusalAgentLabel(bot.agent_id), bot.model || t("Default"), bot.effort || null].filter(Boolean).join(" · "),
"muted",
),
);
const agentBtn = button(t("Agent"), openEditAgentSheet, "secondary small");
agentBtn.disabled = !ready();
agentRow.append(agentMain, agentBtn);
cfgGroup.append(agentRow);
cfgSec.append(cfgGroup);
nodes.push(cfgSec);
}
// Connectors health summary (human-readable error category labels, M-15)
const connSec = el("section", undefined, "bots-section");
connSec.append(el("h2", t("Connectors"), "section-title"));
const connGroup = el("div", undefined, "inset-group");
if (!statuses.length) {
connGroup.append(el("p", t("No connectors configured."), "empty"));
} else {
for (const st of statuses) {
const row = el("div", undefined, "bots-row");
const main = el("div", undefined, "bots-row-main");
main.append(
el("strong", st.source),
el(
"small",
[statusLabel(st.state, t), st.account_label, errorCategoryLabel(st.error_category, t)].filter(Boolean).join(" · "),
"muted",
),
);
row.append(main, ctx.dot(statusTone(st.state)));
connGroup.append(row);
}
}
connSec.append(connGroup);
nodes.push(connSec);
// Persistent Soul & Memory (keeps textarea identity, focus, cursor, and draft across draw, M-12)
const docsBtnLabel = t(soulLoaded ? "Reload" : "Load");
if (loadDocsBtn.textContent !== docsBtnLabel) loadDocsBtn.textContent = docsBtnLabel;
loadDocsBtn.setAttribute("aria-label", docsBtnLabel);
loadDocsBtn.disabled = !ready() || soulLoading;
if (soulLoaded) {
const delLabel = t(allowMemoryDelete ? "Deletions allowed" : "Protect facts");
if (delToggle.textContent !== delLabel) delToggle.textContent = delLabel;
delToggle.setAttribute("aria-label", delLabel);
delToggle.className = allowMemoryDelete ? "filter-chip active" : "filter-chip";
soulArea.disabled = saveSoul.disabled = memArea.disabled = saveMem.disabled = !ready();
sync(docSec, [docHead, docCard]);
} else {
sync(docSec, [docHead]);
}
nodes.push(docSec);
return nodes;
}
function draw() {
if (ctx.signal.aborted) return;
refreshBtn.disabled = loading || !ready();
if (bot) {
const working = botSessions.some((s) => s.live && s.working);
headerSub.textContent = [
refusalAgentLabel(bot.agent_id),
bot.model || null,
t(bot.paused ? "Paused" : working ? "Working" : "Active"),
]
.filter(Boolean)
.join(" · ");
}
// C: Compact Avatar Quick-Switch Rail inside Bot Workspace
const rail = renderBotRail(ctx, t, {
bots: allBots.length ? allBots : bot ? [bot] : [],
groups: allGroups,
items: allItems,
botSessions: allBotSessions,
avatars,
activeBotId: botId,
activeGroupId: null,
ready,
onPickBot: (b) => {
if (b.id !== botId) openBotDetail(ctx, b.id, b._hostId, activeTab);
},
onPickGroup: (g) => {
if (ctx.state?.ui) ctx.state.ui.activeBotGroup = { id: g.id, hostId: g._hostId ?? null };
ctx.go("/bots");
},
onNewBot: () => openCreateBotSheet(ctx, t, () => load(), []),
});
railMount.replaceChildren(...(rail ? [rail] : []));
railMount.hidden = !rail;
// A: 4 Segment Tabs (Chat / Work / Automations / Settings)
const now = Date.now();
const needsCount = items.filter((i) => i.status === "needs_you" && !isActivelySnoozed(i, now)).length + requests.filter((r) => r.status === "pending").length;
const autoErrCount = (upcoming?.schedules || []).filter((s) => s?.last_refusal || s?.last_error).length;
const tabDefs = [
["chat", t("Chat"), 0, false],
["work", t("Work"), needsCount, false],
["automations", t("Automations"), autoErrCount, true],
["settings", t("Settings"), 0, false],
];
const tabNodes = tabDefs.map(([id, lbl, badgeCount, isErr]) => {
const b = button(
"",
() => {
activeTab = id;
if (id === "settings" && !soulLoaded && !soulLoading) loadDocs().catch(() => {});
draw();
},
`bot-seg-tab${activeTab === id ? " active" : ""}`,
);
b.setAttribute("role", "tab");
b.setAttribute("aria-selected", String(activeTab === id));
b.setAttribute("aria-label", lbl);
b.dataset.botTab = id;
b.append(el("span", lbl));
if (badgeCount > 0) {
b.append(el("span", badgeCount > 99 ? "99+" : String(badgeCount), `bot-seg-badge${isErr ? " blocked" : ""}`));
}
return b;
});
sync(tabBar, tabNodes);
const inChat = activeTab === "chat";
chatPane.hidden = !inChat;
list.hidden = inChat;
if (inChat) {
renderChatTopArea();
ensureChatMounted();
} else {
chatHandle?.setVisible?.(false);
const paneNodes =
activeTab === "work"
? renderWorkNodes()
: activeTab === "automations"
? renderAutomationsNodes()
: renderSettingsNodes();
sync(list, paneNodes);
}
}
load();
return {
update() {
const nowReady = ready();
if (nowReady && (!wasReady || (!loaded && !loading && !failure))) {
wasReady = nowReady;
load();
return;
}
wasReady = nowReady;
draw();
if (activeTab === "chat") {
ensureBotSessionEntry(Number(activeSessionId));
chatHandle?.update?.();
}
},
cleanup() {
unmountChat();
},
};
}
M.screens["up-next"] = (root, ctx) => {
const { el, button } = ctx;
const t = formatText(ctx);
const allHosts = () => ctx.getConnections?.() || [];
const filteredHosts = () => {
const f = ctx.filteredConnections ? ctx.filteredConnections() : allHosts();
if (f.length) return f;
return ctx.state.snapshot
? [{ id: null, name: ctx.state.snapshot?.host?.display_name || "", active: true, connection: ctx.state.connection, snapshot: ctx.state.snapshot, cached: !!ctx.state.cached }]
: [];
};
const botHosts = () =>
filteredHosts().filter(
(h) => h.connection?.phase === "ready" && !h.cached && !!h.snapshot?.capabilities?.bots,
);
const ready = () => botHosts().length > 0;
const multiHost = () => allHosts().length > 1;
const screen = el("section", undefined, "details bots up-next screen");
const header = ctx.header("To Do", "/");
header.classList.add("title-header");
const filterPill = ctx.deviceFilterPill ? ctx.deviceFilterPill() : null;
const refreshBtn = button(t("Refresh"), () => load(true), "icon-button refresh-button", "activity");
if (filterPill) header.append(filterPill);
header.append(refreshBtn);
const list = el("div", undefined, "detail-list bots-list");
screen.append(header, list);
root.append(screen);
let filter = "needs_you";
let bots = [];
let items = [];
const itemsCursors = new Map();
const noStatusFilterHosts = new Set();
let requests = [];
let suggestions = [];
let failedAutomations = [];
let loading = false;
let failure = null;
let loadSeq = 0;
let lastHostSig = "";
const cardMap = new Map();
const orchEntries = new Map();
// Persistent containers so background draw() preserves focus inside items, access, and orchestration forms.
const accessCtrl = createAccessSection(ctx, t);
const orchSec = el("section", undefined, "bots-section todo-orch-section");
const orchHead = el("div", undefined, "home-section-heading");
const orchCountEl = el("small", "", "muted");
orchHead.append(el("h2", t("Confirmation requests"), "section-title"), orchCountEl);
const orchWrap = el("div", undefined, "bots-items-list");
orchSec.append(orchHead, orchWrap);
const makeGroupHead = (label, dotCls, countEl) => {
const head = el("div", undefined, "todo-group-head");
const title = el("h2", undefined, "todo-group-title");
title.append(el("span", undefined, `todo-group-dot ${dotCls}`), el("span", label));
head.append(title, countEl);
return head;
};
const filterBar = el("div", undefined, "todo-filter-bar");
const filterButtons = [
["needs_you", t("Needs You")],
["open", t("All Open")],
["snoozed", t("Snoozed")],
["closed", t("Closed")],
].map(([id, lbl]) => {
const btn = button(lbl, () => {
filter = id;
return load(true);
}, "filter-chip");
btn.dataset.filter = id;
filterBar.append(btn);
return btn;
});
const choicesSec = el("section", undefined, "bots-section todo-action-group");
choicesSec.dataset.todoGroup = "choices";
const choicesCountEl = el("small", "", "muted");
const choicesHead = makeGroupHead(t("Choices needed"), "", choicesCountEl);
const itemsWrap = el("div", undefined, "bots-items-list");
const moreBtn = button(t("Load more"), loadMore, "secondary");
const approvalsSec = el("section", undefined, "bots-section todo-action-group");
approvalsSec.dataset.todoGroup = "approvals";
const approvalsCountEl = el("small", "", "muted");
const approvalsHead = makeGroupHead(t("Approvals needed"), "approval", approvalsCountEl);
const blockedSec = el("section", undefined, "bots-section todo-action-group");
blockedSec.dataset.todoGroup = "blocked";
const blockedCountEl = el("small", "", "muted");
const blockedHead = makeGroupHead(t("Blocked & errors"), "blocked", blockedCountEl);
const clearCard = el("div", undefined, "todo-clear-card inset-group");
const clearTitle = el("div", undefined, "todo-clear-title");
clearTitle.append(ctx.icon("check"), el("strong", t("All caught up")));
clearCard.append(clearTitle, el("p", t("Your agents are working autonomously."), "muted"));
const hostSig = () => botHosts().map((h) => h.id ?? "local").sort().join(",");
const itemRequestParams = (reqFilter, cursor) => ({
action: "bots.items",
include_closed: reqFilter === "closed",
include_snoozed: reqFilter === "snoozed",
...(reqFilter === "needs_you"
? { status: "needs_you" }
: reqFilter === "closed"
? { status: "closed" }
: reqFilter === "snoozed"
? { status: "snoozed" }
: {}),
limit: 25,
...(cursor ? { cursor } : {}),
});
async function fetchBotItems(hostId, reqFilter, cursor) {
return fetchBotItemsWithStatus(ctx, hostId, itemRequestParams(reqFilter, cursor), noStatusFilterHosts);
}
async function load(reset = true) {
const targets = botHosts();
lastHostSig = hostSig();
if (!targets.length) {
bots = [];
items = [];
itemsCursors.clear();
requests = [];
suggestions = [];
failedAutomations = [];
draw();
return;
}
const seq = ++loadSeq;
const reqFilter = filter;
loading = true;
failure = null;
draw();
try {
const settled = await Promise.allSettled(
targets.map(async (h) => {
const [botsRes, itemsRes, accessRes, sugRes] = await Promise.all([
rpcHost(ctx, h.id, { action: "bots.list", limit: 100 }),
fetchBotItems(h.id, reqFilter, null),
rpcHost(ctx, h.id, { action: "bots.access.list", limit: 50 }),
rpcHost(ctx, h.id, { action: "bots.suggestions.list", include_dismissed: false }),
]);
const tag = (arr) => (h.id ? (arr || []).map((x) => ({ ...x, _hostId: h.id, _hostName: h.name })) : (arr || []));
const hostBots = tag(botsRes?.bots);
const schedBots = hostBots.filter((b) => Array.isArray(b.schedules) && b.schedules.length).slice(0, 8);
const autoFails = [];
if (schedBots.length) {
const upList = await Promise.all(
schedBots.map((b) =>
rpcHost(ctx, h.id, { action: "bots.upcoming", bot_id: b.id })
.then((res) => ({ bot: b, res }))
.catch(() => null),
),
);
for (const entry of upList) {
if (!entry?.res?.schedules) continue;
for (const run of entry.res.schedules) {
if (run?.last_refusal || run?.last_error) {
const cfg = (entry.bot.schedules || []).find((s) => s.id === run.id);
autoFails.push({
hostId: h.id ?? null,
hostName: h.name || "",
botId: entry.bot.id,
scheduleId: run.id,
title: cfg?.name || run.id,
detail: run.last_refusal ? mobileBotRefusalText(run.last_refusal, t) : String(run.last_error || ""),
});
}
}
}
}
return {
host: h,
bots: hostBots,
items: tag(itemsRes?.items),
cursor: itemsRes?.next_cursor || null,
requests: tag(accessRes?.requests),
suggestions: tag(sugRes?.suggestions),
autoFails,
};
}),
);
if (ctx.signal.aborted || seq !== loadSeq) return;
const perHost = [];
const failedHosts = [];
settled.forEach((outcome, i) => {
if (outcome.status === "fulfilled") {
perHost.push(outcome.value);
} else {
failedHosts.push({ name: targets[i]?.name || "", message: safeMsg(outcome.reason, t) });
}
});
if (perHost.length) {
bots = perHost.flatMap((r) => r.bots);
items = perHost.flatMap((r) => r.items);
itemsCursors.clear();
for (const r of perHost) {
if (r.cursor) {
itemsCursors.set(r.host.id ?? null, {
hostId: r.host.id ?? null,
hostName: r.host.name || "",
cursor: r.cursor,
});
}
}
requests = perHost.flatMap((r) => r.requests);
suggestions = perHost.flatMap((r) => r.suggestions);
failedAutomations = perHost.flatMap((r) => r.autoFails);
}
if (failedHosts.length) {
failure = failedHosts
.map((f) => (multiHost() && f.name ? `${f.name}: ${f.message}` : f.message))
.join(" · ");
}
} catch (e) {
if (ctx.signal.aborted || seq !== loadSeq) return;
failure = safeMsg(e, t);
} finally {
if (!ctx.signal.aborted && seq === loadSeq) {
loading = false;
draw();
}
}
}
async function loadMore() {
const nextEntry = itemsCursors.values().next().value;
if (!nextEntry || !ready()) return;
const seq = loadSeq;
const reqFilter = filter;
const { hostId, hostName, cursor } = nextEntry;
const res = await fetchBotItems(hostId, reqFilter, cursor);
if (ctx.signal.aborted || seq !== loadSeq || filter !== reqFilter) return;
const resolvedName = botHosts().find((h) => (h.id ?? null) === hostId)?.name || hostName;
const seen = new Set(items.map((x) => `${x._hostId ?? ""}:${x.uid}`));
for (const item of res?.items || []) {
const k = `${hostId ?? ""}:${item.uid}`;
if (!seen.has(k)) {
items.push(hostId ? { ...item, _hostId: hostId, _hostName: resolvedName } : { ...item });
}
}
if (res?.next_cursor) {
itemsCursors.set(hostId, { hostId, hostName: resolvedName, cursor: res.next_cursor });
} else {
itemsCursors.delete(hostId);
}
draw();
}
function renderWaitingSessionsSection(hosts) {
const waiting = [];
for (const h of hosts) {
for (const s of h.snapshot?.sessions || []) {
if (!s.exited && (s.state === "Waiting" || s.state === "Blocked")) {
waiting.push({ host: h, session: s, key: s.worktree_key || (s.cwd ? `cwd:${s.cwd}` : `unknown:${s.id}`) });
}
}
}
waiting.sort((a, b) => (a.session.state === "Waiting" ? 0 : 1) - (b.session.state === "Waiting" ? 0 : 1));
if (!waiting.length) return { count: 0, node: null };
const sec = el("section", undefined, "bots-section todo-sessions-section");
const head = el("div", undefined, "home-section-heading");
head.append(el("h2", t("Waiting sessions"), "section-title"), el("small", String(waiting.length), "muted"));
const box = el("div", undefined, "inset-group recent-session-list");
const showDev = multiHost();
for (const w of waiting) {
const s = w.session;
const agent = M.sessionAgent?.(s) ?? s.agent;
const label =
(M.cleanTitle?.(s.title) ?? s.title) ||
M.sessionPathLabel?.(s, w.host.snapshot?.host?.platform || w.host.platform) ||
`${t("Terminal")} #${s.id}`;
const row = button(
"",
() => {
if (w.host.id && ctx.openRecent) ctx.openRecent(w.host.id, { id: s.id, groupKey: w.key });
else {
const grp = ctx.state.groups?.get?.(w.key) || { key: w.key, sessions: [s] };
ctx.go(ctx.groupRoute(grp, s.id));
}
},
"recent-session-row",
);
row.dataset.session = String(s.id);
if (w.host.id) row.dataset.host = w.host.id;
row.disabled = w.host.connection?.phase !== "ready" || !!w.host.cached;
row.setAttribute("aria-label", `${t("Open")}: ${label}`);
const copy = el("span", undefined, "needs-copy");
copy.append(
el("strong", label),
el("small", [showDev ? w.host.name : null, s.group_label, agent ? M.agentName?.(agent) || agent : null].filter(Boolean).join(" · ")),
);
const statePill = el("span", t(s.state === "Waiting" ? "Waiting for you" : "Blocked"), `bot-status-pill ${s.state === "Waiting" ? "waiting" : "blocked"}`);
row.append(ctx.dot(s.state), copy, statePill, ctx.icon("chevronRight"));
box.append(row);
}
sec.append(head, box);
return { count: waiting.length, node: sec };
}
function renderOrchestrationSection(hosts) {
const pending = [];
for (const h of hosts) {
const o = M.confirmationRequests ? M.confirmationRequests(h.snapshot?.orchestration) : { gates: [], questions: [], runs: [], tasks: [] };
const runMap = new Map((o.runs || []).map((r) => [r.id, r]));
const taskMap = new Map((o.tasks || []).map((tk) => [tk.id, tk]));
for (const g of o.gates || []) pending.push({ kind: "gate", host: h, item: g, runMap, taskMap });
for (const q of o.questions || []) pending.push({ kind: "question", host: h, item: q, runMap, taskMap });
}
if (!pending.length) {
for (const [k, e] of orchEntries) if (!e.pending) orchEntries.delete(k);
return { count: 0, node: null };
}
orchCountEl.textContent = String(pending.length);
const showDev = multiHost();
const liveKeys = new Set();
const cardNodes = [];
for (const p of pending) {
const key = `${p.host.id || "local"}:${p.kind}:${p.item.id}`;
liveKeys.add(key);
const hostReady = () => p.host.connection?.phase === "ready" && !p.host.cached;
let e = orchEntries.get(key);
if (!e) {
const card = el("article", undefined, "gate-card");
card.dataset.orchestrationId = p.item.id;
const top = el("div", undefined, "bot-item-head");
const eyebrow = el("small", t(p.kind === "gate" ? "Approval requested" : "Reply requested"), "eyebrow");
const hostBadge = el("span", undefined, "device-badge");
top.append(eyebrow, hostBadge);
const scope = el("p", undefined, "muted");
const heading = el("h2");
const body = el("p");
const status = el("p", undefined, "muted action-status");
status.setAttribute("role", "status");
card.append(top, scope, heading, body);
e = { key, kind: p.kind, id: p.item.id, host: p.host, card, hostBadge, scope, heading, body, status, pending: false, sent: false, message: "", options: new Map() };
if (p.kind === "gate") {
e.actions = el("div", undefined, "gate-options");
card.append(e.actions, status);
} else {
const form = el("form");
const input = el("textarea");
const submit = el("button", t("Send reply"), "primary");
input.placeholder = t("Your reply");
input.setAttribute("aria-label", t("Your reply"));
input.required = true;
const dKey = `orch:${key}`;
input.value = ctx.state.drafts?.get?.(dKey) || "";
input.addEventListener("input", () => (input.value ? ctx.state.drafts?.set?.(dKey, input.value) : ctx.state.drafts?.delete?.(dKey)), { signal: ctx.signal });
submit.type = "submit";
form.append(input, submit);
card.append(form, status);
Object.assign(e, { input, submit, dKey });
form.addEventListener("submit", (ev) => {
ev.preventDefault();
respondOrch(e);
}, { signal: ctx.signal });
}
orchEntries.set(key, e);
}
e.host = p.host;
e.hostBadge.hidden = !showDev || !p.host.name;
if (!e.hostBadge.hidden) e.hostBadge.textContent = p.host.name;
const task = p.taskMap.get(p.item.task_id);
const run = p.runMap.get(p.item.run_id || task?.run_id);
e.scope.textContent = [run?.title, task?.title].filter(Boolean).join(" · ");
e.scope.hidden = !e.scope.textContent;
e.heading.textContent = p.kind === "gate" ? p.item.question : p.item.summary;
e.body.textContent = p.kind === "question" ? p.item.body || "" : "";
e.body.hidden = !e.body.textContent;
e.status.textContent = e.message || (!hostReady() ? t("Reconnect to reply.") : "");
e.status.hidden = !e.status.textContent;
if (p.kind === "gate") {
const btns = [];
for (const opt of p.item.options || []) {
let b = e.options.get(opt);
if (!b) {
b = button(t(opt), () => respondOrch(e, opt), "secondary");
e.options.set(opt, b);
}
b.disabled = e.pending || e.sent || !hostReady();
btns.push(b);
}
sync(e.actions, btns);
} else {
e.submit.disabled = e.pending || e.sent || !hostReady();
e.input.readOnly = e.sent;
}
cardNodes.push(e.card);
}
async function respondOrch(e, decision) {
if (e.pending || e.sent) return;
if (e.host.connection?.phase !== "ready" || e.host.cached) {
e.message = t("Reconnect to reply.");
draw();
return;
}
const submitted = e.input?.value;
if (e.kind === "question" && !submitted?.trim()) return;
e.pending = true;
e.message = t("Sending…");
draw();
try {
await rpcHost(ctx, e.host.id, e.kind === "gate" ? { action: "gate_reply", id: e.id, decision } : { action: "question_reply", id: e.id, body: submitted });
if (ctx.signal.aborted) return;
e.sent = true;
e.message = t(e.kind === "gate" ? "Decision sent." : "Reply sent.");
if (e.input && e.input.value === submitted) {
ctx.state.drafts?.delete?.(e.dKey);
e.input.value = "";
}
draw();
await refreshHost(ctx, e.host.id).catch(() => {});
ctx.syncTabBar?.();
} catch (err) {
e.message = `${t("Could not send reply")}: ${safeMsg(err, t)}`;
} finally {
e.pending = false;
if (!ctx.signal.aborted) draw();
}
}
for (const [k, e] of orchEntries) if (!liveKeys.has(k) && !e.pending) orchEntries.delete(k);
sync(orchWrap, cardNodes);
return { count: pending.length, node: orchSec };
}
function renderFailedAutomationsNode() {
if (!failedAutomations.length) return { count: 0, node: null };
const wrap = el("div", undefined, "bots-items-list");
const showDev = multiHost();
for (const f of failedAutomations) {
const card = el("article", undefined, "bot-decision-card blocked bot-automation-run-row");
const head = el("div", undefined, "bot-decision-card-head");
const left = el("div", undefined, "bot-title-line");
left.append(el("span", t("Failed automation"), "bot-status-pill blocked"));
if (showDev && f.hostName) left.append(el("span", f.hostName, "device-badge"));
head.append(left, button(botDisplayName(bots, f.botId), () => openBotDetail(ctx, f.botId, f.hostId, "automations"), "text-button bot-owner-chip"));
card.append(head, el("strong", f.title, "bot-decision-card-title"));
if (f.detail) card.append(el("p", f.detail, "account-error bot-decision-card-sub"));
const actions = el("div", undefined, "bot-decision-card-actions");
const retry = button(
t("Run now"),
async () => {
if (!ready()) return;
try {
await rpcHost(ctx, f.hostId, { action: "bots.schedule.run", bot_id: f.botId, schedule_id: f.scheduleId });
ctx.toast?.(t("Schedule started"));
await load(true);
} catch (err) {
ctx.toast?.(safeMsg(err, t));
}
},
"primary small",
);
retry.disabled = !ready();
actions.append(retry);
card.append(actions);
wrap.append(card);
}
return { count: failedAutomations.length, node: wrap };
}
function draw() {
if (ctx.signal.aborted) return;
const hosts = filteredHosts();
const hasBotsCap = botHosts().length > 0;
refreshBtn.disabled = loading || !hosts.some((h) => h.connection?.phase === "ready");
const nodes = [];
if (failure) nodes.push(el("p", failure, "account-error"));
if (hasBotsCap) {
for (const btn of filterButtons) {
btn.className = filter === btn.dataset.filter ? "filter-chip active" : "filter-chip";
btn.disabled = !ready();
}
nodes.push(filterBar);
}
const now = Date.now();
const visibleItems =
filter === "needs_you"
? items.filter((i) => i.status === "needs_you")
: filter === "closed"
? items.filter((i) => i.status === "done" || i.status === "canceled")
: filter === "snoozed"
? items.filter((i) => Number(i.snoozed_until_ms || 0) > now)
: items;
if (filter === "needs_you") {
const waitingSec = renderWaitingSessionsSection(hosts);
const orchSecRes = renderOrchestrationSection(hosts);
const autoFailRes = renderFailedAutomationsNode();
const pendingAccess = requests.filter((r) => r.status === "pending");
const sugNode = renderSuggestionsSection(ctx, t, suggestions, bots, ready, () => load(true), true, multiHost());
const totalEverything =
visibleItems.length +
orchSecRes.count +
pendingAccess.length +
waitingSec.count +
autoFailRes.count +
suggestions.length;
if (totalEverything === 0 && !loading) {
nodes.push(clearCard);
} else {
// D-1: Choices needed (bot needs_you items with A-inline decision cards + C-detail sheet)
if (hasBotsCap && (visibleItems.length || itemsCursors.size > 0)) {
choicesCountEl.textContent = String(visibleItems.length);
renderItemsList(ctx, t, {
items: visibleItems,
bots,
showBot: true,
ready,
onReload: () => load(true),
cardMap,
multiHost: multiHost(),
hideEmpty: true,
sheetDetail: true,
container: itemsWrap,
});
const cNodes = [choicesHead, itemsWrap];
if (itemsCursors.size > 0) {
moreBtn.disabled = !ready();
cNodes.push(moreBtn);
}
sync(choicesSec, cNodes);
nodes.push(choicesSec);
}
// D-2: Approvals needed (orchestration gates/questions + bot access requests)
const approvalCount = orchSecRes.count + pendingAccess.length;
if (approvalCount > 0) {
approvalsCountEl.textContent = String(approvalCount);
const aNodes = [approvalsHead];
if (orchSecRes.node) aNodes.push(orchSecRes.node);
if (pendingAccess.length) {
aNodes.push(accessCtrl.update({ requests, bots, ready, reload: () => load(true), multiHost: multiHost(), targetHosts: botHosts() }));
}
sync(approvalsSec, aNodes);
nodes.push(approvalsSec);
}
// D-3: Blocked & errors (waiting/blocked terminal sessions + failed automations)
const blockedCount = waitingSec.count + autoFailRes.count;
if (blockedCount > 0) {
blockedCountEl.textContent = String(blockedCount);
const bNodes = [blockedHead];
if (waitingSec.node) bNodes.push(waitingSec.node);
if (autoFailRes.node) bNodes.push(autoFailRes.node);
sync(blockedSec, bNodes);
nodes.push(blockedSec);
}
// D-4: Suggestions
if (sugNode) nodes.push(sugNode);
}
} else if (hasBotsCap) {
renderItemsList(ctx, t, {
items: visibleItems,
bots,
showBot: true,
ready,
onReload: () => load(true),
cardMap,
multiHost: multiHost(),
hideEmpty: false,
sheetDetail: true,
container: itemsWrap,
});
const fNodes = [itemsWrap];
if (itemsCursors.size > 0) {
moreBtn.disabled = !ready();
fNodes.push(moreBtn);
}
sync(choicesSec, fNodes);
nodes.push(choicesSec);
} else {
nodes.push(clearCard);
}
sync(list, nodes);
}
load(true);
return {
update() {
const nextSig = hostSig();
if (nextSig !== lastHostSig) {
load(true);
} else {
draw();
}
},
};
};
})();
