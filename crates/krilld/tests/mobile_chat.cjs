// Chat failure-path regressions. No daemon, user session, network or browser required.
"use strict";
const assert = require("node:assert/strict"),
  fs = require("node:fs"),
  path = require("node:path"),
  vm = require("node:vm");
class Element extends EventTarget {
  constructor(tag, text, cls) {
    super();
    this.tag = tag;
    this._text = text;
    this.className = cls;
    this.children = [];
    this.dataset = {};
    this.attrs = {};
    this.style = {};
    this.scrollTop = 0;
    this.clientHeight = 100;
    this.value = "";
  }
  get textContent() {
    if (this.children.length)
      return (
        (this._text || "") +
        this.children.map((c) => c.textContent ?? "").join("")
      );
    return this._text;
  }
  set textContent(v) {
    for (const n of this.children) n.parentElement = null;
    this.children = [];
    this._text = v;
  }
  append(...nodes) {
    for (const n of nodes) {
      n.remove();
      n.parentElement = this;
      this.children.push(n);
    }
  }
  insertBefore(node, next) {
    node.remove();
    const i = this.children.indexOf(next);
    node.parentElement = this;
    if (i < 0) this.children.push(node);
    else this.children.splice(i, 0, node);
  }
  replaceChildren(...nodes) {
    for (const n of this.children) n.parentElement = null;
    this.children = [];
    this._text = undefined;
    this.append(...nodes);
  }
  remove() {
    if (this.parentElement) {
      this.parentElement.children = this.parentElement.children.filter(
        (n) => n !== this,
      );
      this.parentElement = null;
    }
  }
  setAttribute(k, v) {
    this.attrs[k] = v;
  }
  descendants() {
    return this.children.flatMap((n) => [n, ...n.descendants()]);
  }
  get scrollHeight() {
    return this.children.length * 100;
  }
  get offsetTop() {
    return this.parentElement
      ? this.parentElement.children.indexOf(this) * 100
      : 0;
  }
  click() {
    this.action?.();
  }
}
const deferred = () => {
  let resolve, reject;
  const promise = new Promise((a, b) => {
    resolve = a;
    reject = b;
  });
  return { promise, resolve, reject };
};
const flush = async () => {
  for (let i = 0; i < 30; i++) await Promise.resolve();
};
function fixture(options = {}) {
  const buttons = [],
    calls = [],
    sheets = [],
    errors = [],
    tasks = [],
    timers = new Map();
  let id = 0,
    term = 0;
  const copied = [];
  const doc = Object.assign(new EventTarget(), {
    hidden: false,
    visibilityState: "visible",
    createElementNS: (_, tag) => new Element(tag),
    body: new Element("body"),
    execCommand: () => true,
  });
  const sandbox = {
    Event,
    document: doc,
    isSecureContext: true,
    navigator: {
      clipboard: {
        writeText: async (text) => {
          copied.push(text);
        },
      },
    },
    Date: options.Date || Date,
    setTimeout: (fn, ms) => {
      fn.ms = ms;
      timers.set(++id, fn);
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
    KrillMobile: {},
    MkKeyboard: options.MkKeyboard,
    ResizeObserver: options.ResizeObserver,
  };
  vm.createContext(sandbox);
  vm.runInContext(
    fs.readFileSync(
      path.join(__dirname, "../src/mobile/assets/screens/chat.js"),
      "utf8",
    ),
    sandbox,
  );
  if (options.lang) {
    vm.runInContext(
      fs.readFileSync(
        path.join(__dirname, "../src/mobile/assets/i18n.js"),
        "utf8",
      ),
      sandbox,
    );
    sandbox.KrillMobile.i18n.configure(options.lang);
  }
  const state = {
    sessions: new Map([[7, options.session || { id: 7, state: "Waiting", cwd: "/work" }]]),
    connection: { phase: options.phase || "ready" },
    drafts: new Map(),
    snapshot: { capabilities: options.capabilities || {} },
  };
  const root = new Element("root"),
    el = (...args) => new Element(...args);
  let handler =
    options.handler ||
    (async (request) =>
      request.action === "chat_poll"
        ? {
            generation: "g1",
            agent: "claude",
            bound: true,
            items: [],
            next_offset: request.offset,
          }
        : { sent: true });
  let prepare = async () => ({ mime: "image/jpeg", data: "encoded" });
  const ctx = {
    el,
    state,
    ...(options.lang ? { t: sandbox.KrillMobile.i18n.t } : {}),
    ...(options.icon ? { icon: options.icon } : {}),
    button(label, action, cls) {
      const b = el("button", label, cls);
      b.action = action;
      buttons.push(b);
      return b;
    },
    rpc: async (request) => {
      calls.push(request);
      return handler(request);
    },
    photo: (...args) => prepare(...args),
    error(e) {
      errors.push(e?.message || String(e));
    },
    act(fn) {
      const p = Promise.resolve()
        .then(fn)
        .catch((e) => errors.push(e));
      tasks.push(p);
      return p;
    },
    refresh: async () => {},
    sheet(title, _, __, render) {
      const body = el("body");
      render(body);
      sheets.push({ title, body });
    },
  };
  if (options.voice) sandbox.KrillMobile.voice = options.voice;
  let view = sandbox.KrillMobile.chat.mount(root, {
    ctx,
    sessionId: 7,
    visible: options.visible,
    botSession: options.botSession,
    getQuestion: options.getQuestion,
    onAnswerQuestion: options.onAnswerQuestion,
    onSwitchTerm: () => term++,
  });
  const find = (cls) => root.descendants().find((n) => n.className === cls),
    button = (label) =>
      root
        .descendants()
        .find((n) => n.tag === "button" && n.textContent === label);
  return {
    setQuestion: (...args) => view.setQuestion(...args),
    openQuestion: () => view.openQuestion(),
    insertText: (text) => view.insertText(text),
    root,
    state,
    ctx,
    calls,
    buttons,
    sheets,
    errors,
    tasks,
    find,
    button,
    timers,
    doc,
    copied,
    M: sandbox.KrillMobile,
    get term() {
      return term;
    },
    get entry() {
      return state.chatCache.get(7);
    },
    set handler(h) {
      handler = h;
    },
    set prepare(h) {
      prepare = h;
    },
    update() {
      view.update();
    },
    setVisible(v) {
      view.setVisible(v);
    },
    remount() {
      view.unmount();
      view = sandbox.KrillMobile.chat.mount(root, {
        ctx,
        sessionId: 7,
        onSwitchTerm: () => term++,
      });
    },
    dispose() {
      view.unmount();
    },
    input(text) {
      const input = find("chat-input");
      input.value = text;
      input.selectionStart = input.selectionEnd = text.length;
      input.dispatchEvent(new Event("input"));
    },
    submit() {
      find("chat-composer").dispatchEvent(
        new Event("submit", { cancelable: true }),
      );
    },
    async poll(data) {
      handler = async (req) =>
        req.action === "chat_poll" ? data : { sent: true };
      view.update();
      await flush();
    },
  };
}
function unitServer(f) {
  let total = 0;
  const page = (q) => {
    const start = q.offset,
      end = Math.min(total, start + 128, q.end_offset ?? Infinity);
    return {
      generation: "g1",
      items: Array.from({ length: end - start }, (_, i) => ({
        id: start + ":" + i,
        kind: "assistant",
        text: "Message " + (start + i),
      })),
      next_offset: end,
      bound: true,
    };
  };
  f.handler = async (q) => page(q);
  return {
    page,
    set total(n) {
      total = n;
    },
  };
}
async function historyRace() {
  const f = fixture();
  await flush();
  const server = unitServer(f);
  for (let n = 1; n <= 1050; n++) {
    server.total = n;
    f.update();
    await flush();
  }
  const earlier = deferred(),
    live = deferred();
  f.handler = (q) =>
    q.end_offset !== undefined ? earlier.promise : live.promise;
  f.button("Load earlier messages").action();
  await flush();
  f.update();
  await flush();
  earlier.resolve(server.page({ offset: 49, end_offset: 50 }));
  await flush();
  const before = Array.from(f.entry.items, (x) => x.id);
  assert.equal(f.entry.items[0].text, "Message 49");
  server.total = 1060;
  live.resolve(server.page({ offset: 1050 }));
  await flush();
  const after = Array.from(f.entry.items, (x) => x.id);
  console.log(
    "historyRace first/last:",
    f.entry.items[0].text,
    f.entry.items.at(-1).text,
  );
  f.dispose();
  assert.deepEqual(
    after,
    before,
    "a live poll queued during earlier-page load must not mutate the historical view",
  );
}
function scroll(feed, top) {
  feed.scrollTop = top;
  feed.dispatchEvent(new Event("scroll"));
}
const historyPage = (start, end) => ({
  generation: "g1",
  agent: "claude",
  bound: true,
  page_start: start,
  next_offset: end,
  items: Array.from({ length: end - start }, (_, i) => ({
    id: `message${start + i}`,
    kind: "assistant",
    text: `Message ${start + i}`,
  })),
});
const historyHandler = async (req) =>
  req.end_offset !== undefined
    ? historyPage(Math.max(0, req.end_offset - 10), req.end_offset)
    : req.latest
      ? historyPage(200, 230)
      : historyPage(req.offset, req.offset);
async function automaticHistory() {
  const f = fixture({ handler: historyHandler });
  await flush();
  const feed = f.find("chat-feed");
  assert.equal(f.calls.length, 1, "initial tail does not eagerly backfill");
  feed.dispatchEvent(new Event("scroll"));
  assert.equal(f.calls.length, 1, "bottom pin event does not backfill");
  Object.defineProperty(feed, "scrollHeight", {
    configurable: true,
    value: 300,
  });
  scroll(feed, 100);
  assert.equal(
    f.calls.length,
    1,
    "layout-induced scroll clamp does not backfill",
  );
  delete feed.scrollHeight;
  scroll(feed, 500);
  const pending = deferred();
  f.handler = (req) =>
    req.end_offset !== undefined ? pending.promise : historyHandler(req);
  scroll(feed, 119);
  scroll(feed, 80);
  scroll(feed, 30);
  scroll(feed, 0);
  await flush();
  assert.equal(
    f.calls.length,
    2,
    "burst of upward scroll events issues a single automatic request",
  );
  assert.equal(f.calls.at(-1).end_offset, 200);
  assert.equal(f.calls.at(-1).latest, true);
  assert.equal(f.button("Load earlier messages").disabled, true);
  // The reader keeps moving while the page is in flight: preserve the partial card now visible.
  scroll(feed, 450);
  const anchor = feed.children[4],
    screenTop = anchor.offsetTop - feed.scrollTop;
  pending.resolve(historyPage(190, 200));
  await flush();
  assert.equal(
    anchor.offsetTop - feed.scrollTop,
    screenTop,
    "response preserves current partial-card anchor, not the request-time scroll position",
  );
  assert.equal(feed.scrollTop, 1450);
  assert.equal(feed.children[14], anchor);
  assert(f.entry.live);
  assert.equal(f.entry.live.items[0].id, "message200");
  feed.dispatchEvent(new Event("scroll"));
  await flush();
  assert.equal(
    f.calls.length,
    2,
    "prepend-generated scroll event does not request another page",
  );
  f.update();
  for (const [id, fn] of [...f.timers]) {
    f.timers.delete(id);
    await fn();
  }
  assert.equal(f.calls.length, 2, "reading history pauses live polling");
  f.handler = historyHandler;
  scroll(feed, 100);
  await flush();
  assert.equal(f.calls.length, 3, "a new upward scroll reads the next page");
  assert.equal(f.calls.at(-1).end_offset, 190);
  assert.equal(f.entry.items[0].id, "message180");
  assert.equal(
    new Set(f.entry.items.map((x) => x.id)).size,
    f.entry.items.length,
  );
  f.button("Back to latest").action();
  await flush();
  assert.equal(f.entry.items[0].id, "message200");
  assert.equal(feed.scrollTop, feed.scrollHeight);
  f.dispose();
  const live = deferred(),
    queued = fixture({ handler: historyHandler });
  await flush();
  const qfeed = queued.find("chat-feed");
  queued.handler = (req) =>
    req.end_offset !== undefined ? historyHandler(req) : live.promise;
  queued.update();
  await flush();
  scroll(qfeed, 110);
  scroll(qfeed, 20);
  assert.equal(
    queued.calls.length,
    2,
    "upward intent waits for an active live poll",
  );
  live.resolve(historyPage(230, 231));
  await flush();
  assert.equal(
    queued.calls.length,
    3,
    "live poll finally drains the queued upward intent",
  );
  assert.equal(queued.calls.at(-1).end_offset, 200);
  assert.equal(queued.entry.items[0].id, "message190");
  queued.dispose();
  const cancel = fixture({ handler: historyHandler });
  await flush();
  const cfeed = cancel.find("chat-feed"),
    late = deferred();
  cancel.handler = (req) =>
    req.end_offset !== undefined ? late.promise : historyHandler(req);
  scroll(cfeed, 100);
  await flush();
  assert.equal(
    cancel.button("Back to latest").hidden,
    false,
    "latest is available during the first earlier request",
  );
  cancel.button("Back to latest").action();
  late.resolve(historyPage(190, 200));
  await flush();
  assert.equal(cancel.entry.live, undefined);
  assert.equal(cancel.entry.items[0].id, "message200");
  assert.equal(cfeed.scrollTop, cfeed.scrollHeight);
  assert.equal(
    cancel.calls.filter((c) => c.end_offset !== undefined).length,
    1,
  );
  assert.equal(
    cancel.calls.length,
    3,
    "latest polling resumes after the cancelled page settles",
  );
  cancel.dispose();
  const nested = fixture({ handler: historyHandler });
  await flush();
  scroll(nested.find("chat-feed"), 100);
  await flush();
  const older = deferred();
  nested.handler = (req) =>
    req.end_offset !== undefined ? older.promise : historyHandler(req);
  scroll(nested.find("chat-feed"), 90);
  await flush();
  nested.button("Back to latest").action();
  older.resolve(historyPage(180, 190));
  await flush();
  assert.equal(nested.entry.live, null);
  assert.equal(
    nested.entry.items[0].id,
    "message200",
    "late second historical page cannot undo latest restore",
  );
  nested.dispose();
  const capped = fixture({ handler: async () => historyPage(200, 1200) });
  await flush();
  const capFeed = capped.find("chat-feed"),
    capPending = deferred();
  capped.handler = () => capPending.promise;
  scroll(capFeed, 100);
  await flush();
  scroll(capFeed, 99850);
  const capAnchor = capFeed.children[998],
    capTop = capAnchor.offsetTop - capFeed.scrollTop;
  capPending.resolve(historyPage(190, 200));
  await flush();
  assert.equal(capped.entry.items.length, 1000);
  assert.equal(capFeed.children[998], capAnchor);
  assert.equal(
    capAnchor.offsetTop - capFeed.scrollTop,
    capTop,
    "a delayed prepend cannot evict the reader moved to the clipped tail",
  );
  assert.equal(
    capped.entry.items[0].id,
    "message200",
    "prepend is deferred when the current reader would fall outside the cap",
  );
  capped.dispose();
  console.log(
    "PASS automatic history: real scroll intent, no mount/layout/prepend backfill, burst single-flight, response-time anchor, live-poll queued intent, paused history and latest cancellation",
  );
}
async function automaticHistoryFailures() {
  const f = fixture({ handler: historyHandler });
  await flush();
  const feed = f.find("chat-feed");
  let attempts = 0;
  f.handler = async (req) => {
    if (req.end_offset !== undefined) {
      attempts++;
      throw Error("earlier unavailable");
    }
    return historyHandler(req);
  };
  scroll(feed, 110);
  await flush();
  assert.equal(attempts, 1);
  assert.equal(f.entry.live, undefined);
  assert.equal(f.button("Load earlier messages").disabled, false);
  scroll(feed, 70);
  scroll(feed, 20);
  scroll(feed, 0);
  f.update();
  await flush();
  assert.equal(
    attempts,
    1,
    "same-boundary failure cannot storm as upward events continue",
  );
  scroll(feed, 400);
  scroll(feed, 100);
  await flush();
  assert.equal(
    attempts,
    2,
    "moving away and back explicitly rearms automatic retry",
  );
  f.handler = historyHandler;
  f.button("Retry").action();
  await flush();
  assert.equal(
    f.entry.items[0].id,
    "message190",
    "error Retry retries the historical page",
  );
  f.dispose();
  const manual = fixture({ handler: historyHandler });
  await flush();
  const mfeed = manual.find("chat-feed");
  manual.handler = async () => {
    throw Error("busy");
  };
  scroll(mfeed, 100);
  await flush();
  manual.handler = historyHandler;
  manual.button("Load earlier messages").action();
  await flush();
  assert.equal(
    manual.entry.items[0].id,
    "message190",
    "manual fallback can retry a blocked automatic boundary",
  );
  manual.dispose();
  // Reject the whole page without changing items/pages/live, including missing generation.
  for (const bad of [
    { next_offset: 199 },
    { generation: "g2" },
    { generation: undefined },
    { reset: true },
    { page_start: 200 },
    { page_start: -1 },
  ]) {
    const invalid = fixture({ handler: historyHandler });
    await flush();
    const before = invalid.entry.items.slice(),
      pages = JSON.stringify(invalid.entry.pages);
    invalid.handler = async () => ({ ...historyPage(190, 200), ...bad });
    scroll(invalid.find("chat-feed"), 100);
    await flush();
    assert.deepEqual(invalid.entry.items, before);
    assert.equal(JSON.stringify(invalid.entry.pages), pages);
    assert.equal(invalid.entry.live, undefined);
    assert.equal(invalid.button("Load earlier messages").disabled, false);
    invalid.dispose();
  }
  const duplicate = fixture({ handler: historyHandler });
  await flush();
  duplicate.handler = async () => ({
    ...historyPage(190, 200),
    items: [
      historyPage(190, 200).items[0],
      historyPage(190, 200).items[0],
      historyPage(200, 201).items[0],
    ],
  });
  scroll(duplicate.find("chat-feed"), 100);
  await flush();
  assert.equal(
    new Set(duplicate.entry.items.map((x) => x.id)).size,
    duplicate.entry.items.length,
    "duplicates inside an incoming page are dropped as well",
  );
  duplicate.dispose();
  console.log(
    "PASS automatic history failures: no retry storm, move-away/manual/error retry, exact end/generation/start/reset validation and duplicate ID defense",
  );
}
async function inactiveHistory() {
  const modes = {
    terminal: (f) => f.setVisible(false),
    document: (f) => {
      f.doc.hidden = true;
      f.doc.visibilityState = "hidden";
      f.doc.dispatchEvent(new Event("visibilitychange"));
    },
    cached: (f) => {
      f.state.cached = true;
      f.update();
    },
    offline: (f) => {
      f.state.connection.phase = "disconnected";
      f.update();
    },
    host: (f) => {
      f.state.chatCache = new Map();
      f.state.drafts = new Map();
    },
    disposed: (f) => f.dispose(),
    aborted: (f) => (f.ctx.signal = { aborted: true }),
  };
  for (const [name, deactivate] of Object.entries(modes)) {
    const f = fixture({ handler: historyHandler });
    await flush();
    const feed = f.find("chat-feed"),
      entry = f.entry;
    deactivate(f);
    scroll(feed, 100);
    f.button("Load earlier messages").action();
    await flush();
    assert.equal(f.calls.length, 1, `${name} does not request older pages`);
    assert.equal(entry.items[0].id, "message200");
    f.dispose();
    const stale = fixture({ handler: historyHandler });
    await flush();
    const pending = deferred(),
      sfeed = stale.find("chat-feed"),
      old = stale.entry;
    stale.handler = () => pending.promise;
    scroll(sfeed, 100);
    await flush();
    deactivate(stale);
    pending.resolve(historyPage(190, 200));
    await flush();
    assert.equal(
      old.items[0].id,
      "message200",
      `${name} drops late older results`,
    );
    assert.equal(old.live, undefined);
    assert.equal(stale.calls.length, 2, `${name} does not issue a follow-up`);
    stale.dispose();
    const live = fixture({ handler: historyHandler });
    await flush();
    const waiting = deferred(),
      previous = live.entry;
    live.handler = () => waiting.promise;
    live.update();
    await flush();
    deactivate(live);
    waiting.resolve(historyPage(230, 231));
    await flush();
    assert.equal(
      previous.items.length,
      30,
      `${name} drops a late live delta as well`,
    );
    assert.equal(previous.offset, 230);
    live.dispose();
  }
  // Even a hidden-then-visible transition before completion fences the earlier request.
  const revived = fixture({ handler: historyHandler });
  await flush();
  const pending = deferred();
  revived.handler = (req) =>
    req.end_offset !== undefined ? pending.promise : historyHandler(req);
  scroll(revived.find("chat-feed"), 100);
  await flush();
  revived.setVisible(false);
  revived.setVisible(true);
  pending.resolve(historyPage(190, 200));
  await flush();
  assert.equal(revived.entry.items[0].id, "message200");
  assert.equal(revived.entry.live, undefined);
  revived.dispose();
  // Queued scroll intent is discarded when the pane is hidden before the live poll settles.
  const queued = fixture({ handler: historyHandler });
  await flush();
  const live = deferred();
  queued.handler = () => live.promise;
  queued.update();
  await flush();
  scroll(queued.find("chat-feed"), 100);
  queued.setVisible(false);
  live.resolve(historyPage(230, 230));
  await flush();
  assert.equal(queued.calls.length, 2);
  assert.equal(queued.entry.live, undefined);
  queued.dispose();
  console.log(
    "PASS inactive history: terminal/document hidden, cached, offline, changed host, disposed/aborted do not request/apply earlier pages; hidden cycle and queued intent fenced",
  );
}
async function renderBudgets() {
  const f = fixture({ visible: false });
  await flush();
  const initial = f.calls.length;
  for (let n = 0; n < 10; n++) {
    f.update();
    for (const [id, fn] of [...f.timers]) {
      f.timers.delete(id);
      await fn();
    }
    await flush();
  }
  assert.equal(
    f.calls.length,
    initial,
    "hidden repeated update/timer polls are zero",
  );
  assert.equal(f.timers.size, 0);
  f.setVisible(true);
  await flush();
  assert.equal(
    f.calls.length,
    initial + 1,
    "visibility restore catches up once",
  );
  assert.equal(f.timers.size, 1);
  f.doc.hidden = true;
  f.doc.visibilityState = "hidden";
  f.doc.dispatchEvent(new Event("visibilitychange"));
  assert.equal(f.timers.size, 0);
  f.update();
  await flush();
  assert.equal(f.calls.length, initial + 1);
  f.doc.hidden = false;
  f.doc.visibilityState = "visible";
  f.doc.dispatchEvent(new Event("visibilitychange"));
  await flush();
  assert.equal(
    f.calls.length,
    initial + 2,
    "document foreground catches up once",
  );
  f.input("");
  assert(!f.state.drafts.has(7), "empty drafts do not pin session caches");
  f.dispose();
  const incremental = fixture();
  await flush();
  await incremental.poll({
    bound: true,
    generation: "g1",
    next_offset: 500,
    items: Array.from({ length: 500 }, (_, i) => ({
      id: "i" + i,
      kind: "assistant",
      text: "Message " + i,
    })),
  });
  const liveFeed = incremental.find("chat-feed");
  let reads = 0;
  const original = liveFeed.children;
  Object.defineProperty(liveFeed, "children", {
    configurable: true,
    get() {
      reads++;
      return original;
    },
    set(value) {
      original.splice(0, original.length, ...value);
    },
  });
  await incremental.poll({
    bound: true,
    generation: "g1",
    next_offset: 500,
    items: [],
  });
  assert.equal(
    reads,
    0,
    "live no-op poll does not traverse/reconcile existing feed children",
  );
  await incremental.poll({
    bound: true,
    generation: "g1",
    next_offset: 501,
    items: [{ id: "append", kind: "assistant", text: "Append" }],
  });
  assert(reads < 10, "append reconciliation only touches the appended node");
  incremental.dispose();
  const cache = fixture();
  await flush();
  cache.dispose();
  cache.state.chatCache.clear();
  for (let n = 0; n < 20; n++)
    cache.state.chatCache.set(n, { items: [], retainedBytes: 100 });
  cache.state.drafts.set(0, "unsent");
  cache.state.chatCache.get(1).photo = { status: "ready" };
  cache.state.chatCache.get(2).sending = true;
  cache.state.chatCache.get(3).mounts = 1;
  cache.state.chatCache.get(4).live = { items: [], offset: 1 };
  cache.state.chatCache.get(5).questionDraft = { selected: [1] };
  cache.M.chat.pruneCache(cache.state);
  assert.equal(cache.state.chatCache.size, 8);
  for (const n of [0, 1, 2, 3, 4, 5]) assert(cache.state.chatCache.has(n));
  cache.state.chatCache.set(99, { retainedBytes: 9 * 1024 * 1024 });
  cache.M.chat.pruneCache(cache.state);
  assert(
    !cache.state.chatCache.has(99),
    "byte pressure removes inactive clean entries",
  );
  for (const n of [0, 1, 2, 3, 4, 5]) assert(cache.state.chatCache.has(n));
  const pages = fixture();
  await flush();
  const server = unitServer(pages);
  for (let n = 1; n <= 2400; n++) {
    server.total = n;
    pages.update();
    await flush();
  }
  assert(pages.entry.pages.length <= 2048);
  assert.equal(pages.entry.items.length, 1000);
  assert.equal(pages.entry.items[0].text, "Message 1400");
  pages.button("Load earlier messages").action();
  await flush();
  assert.equal(
    pages.entry.items[0].text,
    "Message 1399",
    "pruned page metadata retains exact preceding byte boundary",
  );
  const feed = pages.find("chat-feed"),
    nodes = feed.children.slice();
  let touched = 0;
  const insert = feed.insertBefore.bind(feed);
  feed.insertBefore = (...args) => {
    touched++;
    return insert(...args);
  };
  pages.update();
  await flush();
  assert.equal(
    touched,
    0,
    "paused/no-op update never reconciles feed children",
  );
  assert.deepEqual(feed.children, nodes);
  pages.dispose();
  const bounded = fixture();
  await flush();
  await bounded.poll({
    bound: true,
    generation: "g1",
    next_offset: 100,
    items: Array.from({ length: 100 }, (_, i) => ({
      id: "b" + i,
      kind: "assistant",
      text: "x".repeat(15000),
    })),
  });
  assert(bounded.entry.retainedBytes <= 2 * 1024 * 1024);
  assert(bounded.entry.items.length < 100);
  assert.equal(
    bounded.entry.items[0]._clipped,
    true,
    "byte-trim retains replay information for its partial page",
  );
  bounded.dispose();
  const lazy = fixture();
  await flush();
  await lazy.poll({
    bound: true,
    generation: "g1",
    next_offset: 1,
    items: [
      { id: "huge", kind: "reasoning", text: "reasoning line\n".repeat(5000) },
    ],
  });
  const details = lazy.find("chat-reasoning");
  assert.equal(
    details.children.length,
    1,
    "large closed reasoning only creates its summary",
  );
  details.open = true;
  details.dispatchEvent(new Event("toggle"));
  assert.equal(details.children.length, 2);
  details.open = false;
  details.dispatchEvent(new Event("toggle"));
  assert.equal(lazy.entry.disclosures.size, 0);
  lazy.dispose();
  console.log(
    "PASS render budgets: hidden poll 0, one catch-up, cache 8 with draft/photo/sending/active pins, pages <=2048, exact older boundary, lazy disclosure",
  );
}
async function tailFollow() {
  const observers = [];
  class RO {
    constructor(fn) {
      this.fn = fn;
      this.targets = new Set();
      observers.push(this);
    }
    observe(n) {
      this.targets.add(n);
    }
    unobserve(n) {
      this.targets.delete(n);
    }
    disconnect() {
      this.targets.clear();
    }
  }
  const page = (from, to) => ({
    generation: "g1",
    agent: "claude",
    bound: true,
    page_start: 0,
    next_offset: to,
    items: Array.from({ length: to - from }, (_, i) => ({
      id: "t" + (from + i),
      kind: "assistant",
      text: "Message " + (from + i),
    })),
  });
  const f = fixture({ ResizeObserver: RO });
  await flush();
  await f.poll(page(0, 30));
  f.dispose();
  // A cached transcript paints at the tail synchronously, before the reopen poll answers.
  const reopen = deferred();
  f.handler = () => reopen.promise;
  f.remount();
  const feed = f.find("chat-feed"),
    ro = observers.at(-1);
  assert.equal(feed.children.length, 30);
  assert.equal(
    feed.scrollTop,
    feed.scrollHeight,
    "first chat render is already at the latest message",
  );
  assert(
    ro.targets.has(feed) && ro.targets.has(feed.children.at(-1)),
    "feed and rendered cards are observed for late layout",
  );
  Object.defineProperty(feed, "scrollHeight", {
    value: 5000,
    configurable: true,
  });
  ro.fn([]);
  assert.equal(
    feed.scrollTop,
    5000,
    "late code/image/disclosure growth keeps a following reader at the bottom",
  );
  delete feed.scrollHeight;
  reopen.resolve(page(0, 30));
  await flush();
  feed.scrollTop = feed.scrollHeight - feed.clientHeight;
  feed.dispatchEvent(new Event("scroll"));
  assert.equal(feed.scrollTop, 2900, "scroll near the tail keeps following");
  // Scrolling up by 15px within the bottom 60px band must disengage following immediately so ResizeObserver / new messages do not snap back to bottom.
  feed.scrollTop = feed.scrollHeight - feed.clientHeight - 15;
  feed.dispatchEvent(new Event("scroll"));
  Object.defineProperty(feed, "scrollHeight", {
    value: 5500,
    configurable: true,
  });
  ro.fn([]);
  assert.equal(
    feed.scrollTop,
    2885,
    "upward scroll within bottom 60px disengages following",
  );
  delete feed.scrollHeight;
  feed.scrollTop = 400;
  feed.dispatchEvent(new Event("scroll"));
  Object.defineProperty(feed, "scrollHeight", {
    value: 6000,
    configurable: true,
  });
  ro.fn([]);
  assert.equal(
    feed.scrollTop,
    400,
    "late growth never drags a reader who scrolled up",
  );
  delete feed.scrollHeight;
  await f.poll(page(30, 32));
  assert.equal(
    feed.children.filter((c) => c.className !== "chat-unread-divider").length,
    32,
  );
  assert.equal(feed.children[30].className, "chat-unread-divider");
  assert.equal(
    feed.scrollTop,
    400,
    "new messages do not pull a reader who scrolled up",
  );
  feed.scrollTop = feed.scrollHeight - feed.clientHeight;
  feed.dispatchEvent(new Event("scroll"));
  assert(
    !feed.children.some((c) => c.className === "chat-unread-divider"),
    "returning to bottom clears unread divider",
  );
  await f.poll(page(32, 33));
  assert.equal(
    feed.scrollTop,
    feed.scrollHeight,
    "returning to the bottom resumes following",
  );
  f.dispose();
  assert.equal(ro.targets.size, 0, "unmount disconnects the observer");
  console.log(
    "PASS chat tail: cached first render at bottom, ResizeObserver re-pin while following, scrolled-up reader kept in place, bottom resumes",
  );
}
// 연결 전에 숨은 채 마운트돼도 ready 뒤 첫 페이지를 받아, Chat을 누른 프레임에 내용이 있다. 구버전 데몬이 latest를 거부하면 처음부터 읽는다.
async function hiddenPrefetchAndOldDaemon() {
  const page = {
    generation: "g1",
    agent: "claude",
    bound: true,
    page_start: 0,
    items: [{ id: "a", kind: "assistant", text: "hello" }],
    next_offset: 10,
  };
  const f = fixture({
    visible: false,
    phase: "connecting",
    handler: async () => page,
  });
  await flush();
  assert.equal(f.calls.length, 0);
  f.state.connection.phase = "ready";
  f.update();
  await flush();
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].latest, true);
  f.update();
  await flush();
  assert.equal(
    f.calls.length,
    1,
    "hidden Chat stops polling after the first page",
  );
  f.setVisible(true);
  assert(
    f.root.descendants().some((n) => n.tag === "article"),
    "tapping Chat shows the prefetched page in the same frame",
  );
  f.dispose();
  const old = fixture({
    visible: true,
    handler: async (r) => {
      if (r.latest) throw Error("invalid mobile authentication or frame");
      return {
        ...page,
        items: [{ id: "b", kind: "assistant", text: "old" }],
        next_offset: 5,
      };
    },
  });
  old.state.snapshot = { capabilities: { hybrid_chat: true } };
  old.remount();
  await flush();
  assert.equal(old.calls.length, 3);
  assert.equal(old.calls[2].latest, undefined);
  assert.equal(old.calls[2].offset, 0);
  assert(
    old.root.descendants().some((n) => n.tag === "article"),
    "old daemon still shows the transcript",
  );
  old.dispose();
  const current = fixture({
    visible: true,
    handler: async () => {
      throw Error("invalid mobile authentication or frame");
    },
  });
  current.state.snapshot = { capabilities: { chat_tail: true } };
  current.remount();
  await flush();
  assert(
    current.calls.length > 0 && current.calls.every((c) => c.latest),
    "a chat_tail daemon never drops the tail request",
  );
  current.dispose();
  let fails = 0;
  const flaky = fixture({
    visible: false,
    handler: async () => {
      fails++;
      throw Error("busy");
    },
  });
  await flush();
  for (let i = 0; i < 5; i++) {
    flaky.update();
    await flush();
  }
  assert.equal(fails, 2, "hidden Chat stops retrying after two failures");
  flaky.dispose();
  console.log(
    "PASS hidden Chat prefetches once after connecting; chat_tail-less daemon rejection falls back to offset polling",
  );
}
async function orcaComposerAndMeta() {
  const f = fixture();
  await flush();
  await f.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    items: [
      { id: "0:0", kind: "user", text: "hi", at: "2026-10-24T03:48:10.000Z" },
      { id: "0:1", kind: "assistant", text: "done" },
    ],
    next_offset: 10,
  });
  const metas = f.root.descendants().filter((n) => n.className === "chat-meta");
  assert.equal(metas.length, 2, "user and agent messages carry a meta row");
  assert.equal(metas[0].children[0].className, "icon-button chat-copy");
  assert.equal(metas[0].children[1].className, "chat-time");
  assert.equal(metas[0].children[1].attrs.datetime, "2026-10-24T03:48:10.000Z");
  assert.equal(
    metas[1].children.length,
    1,
    "a message without a record time shows only the copy button",
  );
  assert.equal(
    f.find("chat-input").placeholder,
    "Message {0}",
    "a known agent gets the per-agent placeholder (this fixture has no translator)",
  );
  assert.equal(
    f.buttons.find((b) => b.className === "composer-pill").hidden,
    true,
    "old daemons do not get a fictional model action",
  );
  f.dispose();
  console.log(
    "PASS Orca composer: copy/time under messages and capability-gated model action",
  );
}
async function markdownFormatting() {
  const f = fixture();
  await flush();
  const text =
    '**실패 지점은 확인했지만, 복구는 아직 못 했습니다.**\n\n- `pwd`뿐 아니라 **셸을 쓰지 않는 자바스크립트 한 줄**도 실패합니다.\n1. 첫 번째 단계\n2. **두 번째** 단계\n> 인용 첫 줄\n> 인용 `코드` 둘째 줄\n| 항목 | 상태 |\n| --- | :---: |\n| `build` | **통과** |\n### **확인 결과**\n[공식 문서](https://example.com/docs)와 *강조*, snake_case, \\*그대로\\*\n```js\nconst raw = "**그대로** <img src=x onerror=alert(1)>";\n```\n<img src=x onerror=alert(1)> [차단](javascript:alert)';
  await f.poll({
    generation: "g1",
    agent: "codex",
    bound: true,
    next_offset: 1,
    items: [{ id: "markdown", kind: "assistant", text }],
  });
  const nodes = f.find("chat-md").descendants(),
    byTag = (tag) => nodes.filter((n) => n.tag === tag);
  assert.deepEqual(
    byTag("strong").map((n) => n.children.map((c) => c.textContent).join("")),
    [
      "실패 지점은 확인했지만, 복구는 아직 못 했습니다.",
      "셸을 쓰지 않는 자바스크립트 한 줄",
      "두 번째",
      "통과",
      "확인 결과",
    ],
  );
  assert.equal(byTag("code")[0].textContent, "pwd");
  assert.equal(byTag("ol").length, 1);
  assert.equal(byTag("ol")[0].className, "chat-ol");
  assert.equal(byTag("ul").length, 1);
  assert.equal(byTag("li").length, 3);
  assert.equal(byTag("ol")[0].children.filter((n) => n.tag === "li").length, 2);
  assert.equal(byTag("blockquote").length, 1);
  assert.equal(byTag("blockquote")[0].className, "chat-quote");
  assert(
    byTag("blockquote")[0]
      .descendants()
      .some((n) => n.tag === "code" && n.textContent === "코드"),
  );
  assert.equal(byTag("table").length, 1);
  assert.equal(byTag("table")[0].className, "chat-table");
  assert.equal(byTag("th").length, 2);
  assert.equal(byTag("td").length, 2);
  assert.equal(byTag("em").length, 1);
  assert.equal(byTag("a").length, 1, "only safe links become anchors");
  assert.equal(byTag("a")[0].attrs.href, "https://example.com/docs");
  assert.equal(
    byTag("pre")[0].children[0].textContent,
    'const raw = "**그대로** <img src=x onerror=alert(1)>";',
    "code blocks retain literal Markdown and HTML",
  );
  assert.equal(byTag("img").length, 0, "message HTML is displayed as text");
  assert(nodes.some((n) => n.textContent?.includes("snake_case")));
  assert.equal(
    byTag("strong").some((n) => n.textContent?.includes("그대로")),
    false,
  );
  const chatInput = f.find("chat-input");
  Object.defineProperty(chatInput, "scrollHeight", {
    configurable: true,
    get: () => 200,
  });
  f.input("long multiline");
  assert.equal(
    chatInput.style.height,
    "140px",
    "sizeInput clamps to 140px max",
  );
  Object.defineProperty(chatInput, "scrollHeight", {
    configurable: true,
    get: () => 18,
  });
  f.input("short");
  assert.equal(chatInput.style.height, "28px", "sizeInput clamps to 28px min");
  f.dispose();
  console.log(
    "PASS mobile Markdown: Korean bold, inline code, ol/blockquote/table, headings, emphasis, safe links, literal code/HTML and sizeInput",
  );
}
async function quickActions() {
  const f = fixture();
  await flush();
  assert.equal(f.insertText("\r\n"), false);
  const input = f.find("chat-input");
  assert.equal(f.insertText("/krill-orchestration "), true);
  assert.equal(input.value, "/krill-orchestration ");
  assert.equal(input.selectionEnd, input.value.length);
  input.value = "use OLD please";
  input.selectionStart = 4;
  input.selectionEnd = 7;
  f.insertText("/ship-dev");
  assert.equal(input.value, "use /ship-dev please");
  assert.equal(f.state.drafts.get(7), input.value);
  assert.equal(input.selectionStart, 13);
  assert(!f.calls.some((q) => q.action === "prompt"), "insert never submits");
  input.selectionStart = input.selectionEnd = undefined;
  f.insertText("\r\nnext\r\nline\r\n");
  assert(input.value.endsWith("next\nline"));
  f.remount();
  assert.equal(f.find("chat-input").value, "use /ship-dev pleasenext\nline");
  f.dispose();
}
// Like Orca, a send tap sends the field even if the IME still reports a composition.
async function imeSendTap() {
  const f = fixture();
  await flush();
  const input = f.find("chat-input");
  f.input("키보드 열린 채로 보내기 ㅎ");
  input.dispatchEvent(new Event("compositionstart"));
  f.submit();
  await flush();
  assert.equal(
    f.calls.find((q) => q.action === "prompt")?.text,
    "키보드 열린 채로 보내기 ㅎ",
    "send tap during IME composition sends the text",
  );
  f.dispose();
}
// Same-generation tail refresh is a merge, including open disclosures and reader anchors.
async function stableLatestRefresh() {
  let now = 10000;
  const Clock = class extends Date {
      static now() {
        return now;
      }
    },
    items = Array.from({ length: 30 }, (_, i) => ({
      id: "stable" + i,
      kind: i === 0 ? "reasoning" : "assistant",
      text: "Message " + i,
    })),
    page = (items) => ({
      generation: "g1",
      agent: "claude",
      bound: true,
      items,
      page_start: 0,
      next_offset: items.length,
    });
  const f = fixture({ Date: Clock, handler: async () => page(items) });
  await flush();
  const feed = f.find("chat-feed"),
    card = feed.children[0],
    details = f.find("chat-reasoning");
  details.open = true;
  details.dispatchEvent(new Event("toggle"));
  feed.scrollTop = 400;
  feed.dispatchEvent(new Event("scroll"));
  const reader = feed.children[4];
  f.setVisible(false);
  now += 5000;
  f.handler = async () =>
    page([...items, { id: "next", kind: "assistant", text: "New reply" }]);
  f.setVisible(true);
  await flush();
  assert.equal(f.calls.at(-1).latest, true);
  assert.equal(feed.children[0], card);
  assert.equal(f.find("chat-reasoning"), details);
  assert(details.open);
  assert.equal(f.entry.disclosures.size, 1);
  assert.equal(feed.scrollTop, 400);
  assert.equal(feed.children[4], reader);
  // A shifted window retains overlapping cards, and a changed item updates its content in place.
  f.setVisible(false);
  now += 5000;
  f.handler = async () =>
    page([
      ...items
        .slice(10)
        .map((item) =>
          item.id === "stable12" ? { ...item, text: "Updated reply" } : item,
        ),
      { id: "last", kind: "assistant", text: "Latest" },
    ]);
  const updated = feed.children[12];
  f.setVisible(true);
  await flush();
  assert.equal(feed.children[0], card);
  assert.equal(feed.children[12], updated);
  assert(updated.descendants().some((n) => n.textContent === "Updated reply"));
  assert.equal(feed.scrollTop, 400);
  // Trim from above the reader while keeping the surviving card's offset.
  const many = Array.from({ length: 1000 }, (_, i) => ({
    id: "many" + i,
    kind: "assistant",
    text: "Many " + i,
  }));
  await f.poll({
    generation: "g2",
    bound: true,
    reset: true,
    items: many,
    next_offset: 1000,
  });
  feed.scrollTop = 400;
  feed.dispatchEvent(new Event("scroll"));
  const kept = feed.children[4];
  await f.poll({
    generation: "g2",
    bound: true,
    items: [{ id: "many1000", kind: "assistant", text: "New" }],
    next_offset: 1001,
  });
  assert.equal(feed.children[3], kept);
  assert.equal(feed.scrollTop, 300);
  // Model chrome above the feed to prove error show/hide compensates the reader's viewport top.
  const status = feed.parentElement.children[1];
  feed.getBoundingClientRect = () => ({ top: status.hidden ? 0 : 50 });
  for (const child of feed.children)
    child.getBoundingClientRect = () => ({
      top: (status.hidden ? 0 : 50) + child.offsetTop - feed.scrollTop,
      bottom: (status.hidden ? 0 : 50) + child.offsetTop + 100 - feed.scrollTop,
    });
  const readerTop = kept.getBoundingClientRect().top;
  f.handler = async () => {
    throw Error("offline");
  };
  f.update();
  await flush();
  assert.equal(kept.getBoundingClientRect().top, readerTop);
  await f.poll({ generation: "g2", bound: true, items: [], next_offset: 1001 });
  assert.equal(kept.getBoundingClientRect().top, readerTop);
  f.dispose();
  console.log(
    "PASS CLS chat: latest merge retains cards/disclosure/following, updates content, trims and error chrome preserve reader anchor",
  );
}
function key(f, value, options = {}) {
  const event = new Event("keydown", { cancelable: true });
  Object.assign(event, { key: value, ...options });
  f.find("chat-input").dispatchEvent(event);
  return event;
}
async function composerDiscovery() {
  const capabilities = {
    command_catalog: true,
    mention_files: true,
    composer_models: true,
  };
  const f = fixture({ capabilities });
  await flush();
  let catalogs = 0;
  const mention = deferred();
  f.handler = async (req) => {
    if (req.action === "command_catalog") {
      catalogs++;
      return {
        supported: true,
        items: [
          {
            name: "review",
            text: "$review",
            kind: "Skill",
            description: "Review code",
          },
          {
            name: "help",
            text: "/help",
            kind: "Command",
            description: "CLI help",
          },
          { name: "unsafe", text: "/bad\nexecute", kind: "Command" },
        ],
      };
    }
    if (req.action === "mention_files") return mention.promise;
    if (req.action === "composer_models")
      return {
        supported: true,
        current: "a",
        items: [
          { id: "a", label: "Model A" },
          { id: "b", label: "Model B" },
        ],
      };
    if (req.action === "set_model") return { applied: true };
    return { bound: true, agent: "codex", items: [], next_offset: 0 };
  };
  f.input("/");
  await flush();
  assert.equal(
    f.find("chat-composer-options").children.length,
    2,
    "catalog includes installed skills and commands, drops control text",
  );
  assert.equal(f.find("chat-input").attrs["aria-expanded"], "true");
  key(f, "ArrowDown");
  assert.equal(
    f.find("chat-composer-options").children[1].attrs["aria-selected"],
    "true",
  );
  const selected = key(f, "Enter");
  assert(selected.defaultPrevented);
  assert.equal(f.find("chat-input").value, "/help ");
  assert.equal(f.state.drafts.get(7), "/help ");
  assert(!f.calls.some((c) => c.action === "prompt"), "selection never sends");
  f.input("/review");
  await flush();
  assert.equal(catalogs, 1, "same provider/workspace catalog is cached");
  key(f, "Enter");
  assert.equal(
    f.find("chat-input").value,
    "$review ",
    "provider invocation text is preserved",
  );
  f.input("/");
  await flush();
  f.find("chat-input").dispatchEvent(new Event("compositionstart"));
  assert.equal(f.find("chat-composer-suggestions").hidden, true);
  assert.equal(key(f, "Enter", { isComposing: true }).defaultPrevented, false);
  f.find("chat-input").dispatchEvent(new Event("compositionend"));
  await flush();
  key(f, "Escape");
  assert.equal(f.find("chat-composer-suggestions").hidden, true);
  f.input("Inspect @src");
  for (const [id, fn] of [...f.timers]) {
    if (id === 1) continue;
    f.timers.delete(id);
    fn();
  }
  await flush();
  mention.resolve({
    items: [
      { path: 'src/a "quoted" file.js' },
      { path: "../escape" },
      { path: "/absolute" },
      { path: "line\nbreak" },
    ],
    truncated: true,
  });
  await flush();
  assert.equal(
    f.find("chat-composer-options").children.length,
    1,
    "only safe relative paths are inserted",
  );
  key(f, "Enter");
  assert.equal(
    f.find("chat-input").value,
    "Inspect " + JSON.stringify('src/a \"quoted\" file.js') + " ",
  );
  assert(!f.calls.some((c) => c.action === "prompt"));
  f.buttons.find((b) => b.className === "composer-pill").action();
  await flush();
  assert.equal(f.find("chat-composer-options").children[0].children.length, 2);
  key(f, "ArrowDown");
  key(f, "Enter");
  await flush();
  assert.equal(f.term, 0);
  assert.equal(f.find("composer-model-name").textContent, "Model B");
  assert.equal(
    f.buttons.find((b) => b.className === "composer-pill").attrs["aria-label"],
    "Model B",
  );
  assert(f.calls.some((c) => c.action === "set_model" && c.model === "b"));
  assert(
    !f.calls.some((c) => c.action === "prompt"),
    "model selection uses only explicit RPC",
  );
  f.dispose();
  const unsupported = fixture({
    capabilities,
    handler: async (req) =>
      req.action === "composer_models"
        ? {
            supported: false,
            items: [{ id: "launch-only", label: "Launch only" }],
            reason: "No live control",
          }
        : { bound: true, agent: "codex", items: [], next_offset: 0 },
  });
  await flush();
  unsupported.buttons.find((b) => b.className === "composer-pill").action();
  await flush();
  assert.equal(unsupported.find("chat-composer-options").children.length, 0);
  assert(
    unsupported
      .find("chat-composer-suggestions")
      .descendants()
      .some((n) => n.textContent === "No live control"),
  );
  assert.equal(unsupported.term, 0);
  unsupported.dispose();
  const changing = deferred(),
    model = fixture({
      capabilities,
      handler: async (req) =>
        req.action === "composer_models"
          ? { supported: true, items: [{ id: "real", label: "Real model" }] }
          : req.action === "set_model"
            ? changing.promise
            : { bound: true, agent: "codex", items: [], next_offset: 0 },
    });
  await flush();
  model.input("unsent draft");
  model.buttons.find((b) => b.className === "composer-pill").action();
  await flush();
  key(model, "Enter");
  await flush();
  assert.equal(
    model.find("composer-model-name").textContent,
    "Model",
    "pending request is not a model change",
  );
  assert.equal(model.find("primary composer-send").disabled, true);
  assert.equal(model.find("chat-input").value, "unsent draft");
  changing.resolve({ applied: false, reason: "Native composer busy" });
  await flush();
  assert.equal(model.find("composer-model-name").textContent, "Model");
  assert(
    model
      .find("chat-composer-suggestions")
      .descendants()
      .some((n) => n.textContent === "Native composer busy"),
  );
  assert.equal(model.find("primary composer-send").disabled, false);
  assert.equal(model.term, 0);
  model.dispose();
  const commandModel = fixture({
    capabilities,
    handler: async (req) =>
      req.action === "command_catalog"
        ? { items: [{ name: "model", text: "/model", kind: "Command" }] }
        : req.action === "composer_models"
          ? { supported: true, items: [{ id: "real", label: "Real model" }] }
          : { bound: true, agent: "codex", items: [], next_offset: 0 },
  });
  await flush();
  commandModel.input("/model");
  await flush();
  key(commandModel, "Enter");
  await flush();
  assert.equal(commandModel.find("chat-input").value, "");
  assert.equal(
    commandModel.find("chat-composer-options").children[0].children[0].children[0]
      .textContent,
    "Real model",
  );
  assert(
    !commandModel.calls.some((c) => c.action === "prompt"),
    "native /model selection routes inline",
  );
  assert.equal(commandModel.term, 0);
  key(commandModel, "Escape");
  commandModel.input("/model keep this draft");
  commandModel.find("chat-input").selectionStart = commandModel.find(
    "chat-input",
  ).selectionEnd = 6;
  commandModel.find("chat-input").dispatchEvent(new Event("input"));
  await flush();
  key(commandModel, "Enter");
  await flush();
  assert.equal(
    commandModel.find("chat-input").value,
    " keep this draft",
    "inline model routing preserves text outside the unchanged trigger",
  );
  key(commandModel, "Escape");
  assert.equal(commandModel.find("chat-input").value, " keep this draft");
  assert(!commandModel.calls.some((c) => c.action === "prompt"));
  commandModel.dispose();
  const late = deferred(),
    race = fixture({ capabilities });
  await flush();
  race.handler = (req) =>
    req.action === "command_catalog"
      ? late.promise
      : Promise.resolve({
          bound: true,
          agent: "codex",
          items: [],
          next_offset: 0,
        });
  race.input("/");
  await flush();
  race.input("new draft");
  late.resolve({ items: [{ text: "$stale", name: "stale" }] });
  await flush();
  assert.equal(race.find("chat-composer-suggestions").hidden, true);
  assert.equal(race.find("chat-input").value, "new draft");
  race.dispose();
  const detached = deferred(),
    host = fixture({ capabilities });
  await flush();
  host.handler = (req) =>
    req.action === "command_catalog" ? detached.promise : Promise.resolve({});
  host.input("/");
  await flush();
  host.state.chatCache = new Map();
  detached.resolve({ items: [{ text: "/stale", name: "stale" }] });
  await flush();
  assert.equal(
    host.find("chat-composer-options").children.length,
    0,
    "late old-host results do not populate choices",
  );
  host.dispose();
  console.log(
    "PASS composer discovery: provider text, skills+commands, safe path text, keyboard/IME, no auto-send, cache/stale-host guards, truthful models without Term",
  );
}
async function composerRaces() {
  const capabilities = {
      command_catalog: true,
      mention_files: true,
      composer_models: true,
    },
    modelButton = (f) =>
      f.buttons.find(
        (b) =>
          b.className === "composer-pill" && f.root.descendants().includes(b),
      );
  for (const field of [
    "account_id",
    "profile_id",
    "provider",
    "foreground",
    "groupKey",
    "command_context",
  ]) {
    const f = fixture({ capabilities });
    await flush();
    const pending = deferred(),
      sess = f.state.sessions.get(7);
    sess[field] = "A";
    f.update();
    await flush();
    f.handler = (req) =>
      req.action === "command_catalog" ? pending.promise : historyHandler(req);
    f.input("/private");
    await flush();
    assert.equal(f.calls.at(-1).action, "command_catalog");
    sess[field] = "B";
    f.update();
    pending.resolve({ items: [{ text: "/private-A", name: "private-A" }] });
    await flush();
    assert.equal(
      f.find("chat-composer-suggestions").hidden,
      true,
      `${field} change hides pending old catalog`,
    );
    assert.equal(f.find("chat-composer-options").children.length, 0);
    key(f, "Enter");
    assert.equal(f.find("chat-input").value, "/private");
    f.handler = async (req) =>
      req.action === "command_catalog"
        ? { items: [{ text: "/private-B", name: "private-B" }] }
        : historyHandler(req);
    f.input("/private");
    await flush();
    const old = f.find("chat-composer-options").children[0];
    assert.equal(old.children[0].textContent, "/private-B");
    sess[field] = "C";
    old.action();
    assert.equal(
      f.find("chat-input").value,
      "/private",
      "a stale rendered button cannot insert across a scope change even before update",
    );
    f.dispose();
  }
  const raw = fixture({
    capabilities,
    handler: async (req) => ({
      ...(await historyHandler(req)),
      agent: "codex",
    }),
  });
  await flush();
  raw.state.sessions.get(7).agent = "codex";
  raw.update();
  await flush();
  const rawCatalog = deferred();
  raw.handler = (req) =>
    req.action === "command_catalog"
      ? rawCatalog.promise
      : Promise.resolve({
          generation: "g1",
          agent: "codex",
          bound: true,
          items: [],
          next_offset: req.offset,
        });
  raw.input("/");
  await flush();
  raw.state.sessions.get(7).agent = "claude";
  raw.update();
  await flush();
  assert.equal(
    raw.entry.agent,
    "codex",
    "transcript agent can lag behind the raw snapshot agent",
  );
  rawCatalog.resolve({ items: [{ text: "/codex-only", name: "codex-only" }] });
  await flush();
  assert.equal(raw.find("chat-composer-suggestions").hidden, true);
  assert.equal(
    raw.find("chat-composer-options").children.length,
    0,
    "raw agent change fences a catalog even while entry.agent masks chatAgent",
  );
  assert.equal(raw.find("chat-input").value, "/");
  raw.dispose();
  // A -> B -> A changes must not revive the original request or its cached catalog.
  const cycle = fixture({ capabilities });
  await flush();
  const catalog = deferred(),
    sess = cycle.state.sessions.get(7);
  sess.account_id = "A";
  cycle.update();
  cycle.handler = (req) =>
    req.action === "command_catalog" ? catalog.promise : historyHandler(req);
  cycle.input("/");
  await flush();
  sess.account_id = "B";
  cycle.update();
  sess.account_id = "A";
  cycle.update();
  catalog.resolve({ items: [{ text: "/old-A", name: "old-A" }] });
  await flush();
  assert.equal(cycle.find("chat-composer-suggestions").hidden, true);
  cycle.handler = async (req) =>
    req.action === "command_catalog"
      ? { items: [{ text: "/fresh-A", name: "fresh-A" }] }
      : historyHandler(req);
  cycle.input("/");
  await flush();
  assert.equal(
    cycle.find("chat-composer-options").children[0].children[0].textContent,
    "/fresh-A",
  );
  assert.equal(
    cycle.calls.filter((q) => q.action === "command_catalog").length,
    2,
  );
  cycle.dispose();
  const known = {
    supported: true,
    current: "a",
    items: [
      { id: "a", label: "Model A" },
      { id: "b", label: "Model B" },
    ],
  };
  for (const data of [
    { supported: false, current: null, items: [] },
    { ...known, current: null },
    { ...known, current: "unlisted" },
    null,
  ]) {
    const f = fixture({ capabilities });
    await flush();
    f.handler = (req) =>
      req.action === "composer_models" ? known : historyHandler(req);
    modelButton(f).action();
    await flush();
    assert.equal(f.find("composer-model-name").textContent, "Model A");
    f.button("Done").action();
    f.handler = async (req) => {
      if (req.action !== "composer_models") return historyHandler(req);
      if (data === null) throw Error("model unavailable");
      return data;
    };
    modelButton(f).action();
    assert.equal(
      f.find("composer-model-name").textContent,
      "Model",
      "fresh model authority is uncertain until the reply",
    );
    await flush();
    assert.equal(f.find("composer-model-name").textContent, "Model");
    assert.equal(f.entry.model, null);
    assert.equal(
      f.calls.filter((q) => q.action === "composer_models").length,
      2,
      "every reopen queries current live support/current",
    );
    f.dispose();
  }
  const staleRead = fixture({ capabilities });
  await flush();
  const modelRead = deferred();
  staleRead.handler = (req) =>
    req.action === "composer_models" ? modelRead.promise : historyHandler(req);
  modelButton(staleRead).action();
  await flush();
  staleRead.state.sessions.get(7).profile_id = "new-profile";
  staleRead.update();
  modelRead.resolve(known);
  await flush();
  assert.equal(staleRead.find("chat-composer-suggestions").hidden, true);
  assert.equal(staleRead.find("composer-model-name").textContent, "Model");
  staleRead.dispose();
  for (const applied of [true, false]) {
    const f = fixture({ capabilities });
    await flush();
    const pending = deferred();
    f.handler = (req) =>
      req.action === "composer_models"
        ? known
        : req.action === "set_model"
          ? pending.promise
          : historyHandler(req);
    f.input("draft before model");
    modelButton(f).action();
    await flush();
    key(f, "ArrowDown");
    key(f, "Enter");
    await flush();
    const token = f.entry.modelPending;
    assert(token);
    assert.equal(f.find("primary composer-send").disabled, true);
    f.remount();
    await flush();
    f.input("newer unsent draft");
    assert.equal(f.entry.modelPending, token);
    assert.equal(
      f.find("primary composer-send").disabled,
      true,
      "pending model operation locks remounted Send",
    );
    assert.equal(modelButton(f).disabled, true);
    f.submit();
    modelButton(f).action();
    await flush();
    assert(!f.calls.some((q) => q.action === "prompt"));
    assert.equal(f.calls.filter((q) => q.action === "set_model").length, 1);
    assert.equal(
      f.calls.filter((q) => q.action === "composer_models").length,
      1,
      "remount cannot start another model picker while pending",
    );
    pending.resolve({ applied, model: "b" });
    await flush();
    assert.equal(f.entry.modelPending, null);
    assert.equal(f.find("primary composer-send").disabled, false);
    assert.equal(f.find("composer-model-name").textContent, applied ? "Model B" : "Model A");
    assert.equal(f.find("chat-input").value, "newer unsent draft");
    assert.equal(f.state.drafts.get(7), "newer unsent draft");
    f.dispose();
  }
  const changed = fixture({ capabilities });
  await flush();
  const applying = deferred();
  changed.handler = (req) =>
    req.action === "composer_models"
      ? known
      : req.action === "set_model"
        ? applying.promise
        : historyHandler(req);
  modelButton(changed).action();
  await flush();
  key(changed, "ArrowDown");
  key(changed, "Enter");
  await flush();
  const cs = changed.state.sessions.get(7);
  cs.account_id = "B";
  changed.update();
  cs.account_id = undefined;
  changed.update();
  applying.resolve({ applied: true, model: "b" });
  await flush();
  assert.equal(changed.entry.modelPending, null);
  assert.equal(
    changed.entry.model,
    null,
    "old scope success cannot claim a model after an A -> B -> A scope transition",
  );
  assert.equal(changed.find("composer-model-name").textContent, "Model");
  changed.dispose();
  const host = fixture({ capabilities });
  await flush();
  const oldSet = deferred(),
    newSet = deferred();
  let count = 0;
  host.handler = (req) =>
    req.action === "composer_models"
      ? known
      : req.action === "set_model"
        ? ++count === 1
          ? oldSet.promise
          : newSet.promise
        : historyHandler(req);
  modelButton(host).action();
  await flush();
  key(host, "ArrowDown");
  key(host, "Enter");
  await flush();
  const oldEntry = host.entry;
  host.state.chatCache = new Map();
  host.state.drafts = new Map([[7, "new host draft"]]);
  host.remount();
  await flush();
  modelButton(host).action();
  await flush();
  key(host, "ArrowDown");
  key(host, "Enter");
  await flush();
  const newToken = host.entry.modelPending;
  oldSet.resolve({ applied: true, model: "old-host-model" });
  await flush();
  assert.equal(
    host.entry.modelPending,
    newToken,
    "old host completion cannot release the new host lock",
  );
  assert.equal(host.entry.model.id, "a");
  assert.equal(host.find("primary composer-send").disabled, true);
  assert.equal(host.state.drafts.get(7), "new host draft");
  assert.equal(oldEntry.modelPending, null);
  newSet.resolve({ applied: true, model: "b" });
  await flush();
  assert.equal(host.find("composer-model-name").textContent, "Model B");
  host.dispose();
  const pinned = fixture({ capabilities });
  await flush();
  const wait = deferred();
  pinned.handler = (req) =>
    req.action === "composer_models"
      ? known
      : req.action === "set_model"
        ? wait.promise
        : historyHandler(req);
  modelButton(pinned).action();
  await flush();
  key(pinned, "ArrowDown");
  key(pinned, "Enter");
  await flush();
  const entry = pinned.entry;
  pinned.dispose();
  for (let n = 100; n < 120; n++)
    pinned.state.chatCache.set(n, { items: [], retainedBytes: 100 });
  pinned.M.chat.pruneCache(pinned.state);
  assert.equal(
    pinned.state.chatCache.get(7),
    entry,
    "a pending model operation pins its shared entry even without draft/mount",
  );
  wait.resolve({ applied: false });
  await flush();
  assert.equal(entry.modelPending, null);
  console.log(
    "PASS composer races: account/profile/provider epochs fence catalogs/models; uncertain current clears, live model reads stay fresh; shared model operation locks remount, preserves edits, fences old host and pins pending cache",
  );
}
async function composerProgress() {
  const f = fixture();
  await flush();
  const sent = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? sent.promise
      : {
          generation: "g1",
          bound: true,
          agent: "claude",
          items: [],
          next_offset: 0,
        };
  f.input("Review this");
  f.submit();
  assert.equal(f.find("badge").textContent, "Sending");
  assert.equal(
    f.find("chat-live-text").textContent,
    "Review this",
    "the badge says Sending and the line keeps the sent draft",
  );
  await flush();
  sent.resolve({ sent: true });
  await flush();
  assert.equal(
    f.find("chat-live-text").textContent,
    "Accepted. Awaiting agent response… · Review this",
  );
  assert.equal(f.find("chat-input").value, "");
  const sess = f.state.sessions.get(7);
  sess.state = "Working";
  sess.state_since_unix_ns = "2";
  f.update();
  await flush();
  assert.equal(f.find("badge").textContent, "Working");
  assert.equal(
    f.find("chat-live-text").textContent,
    "",
    "a working agent has picked up the message",
  );
  await f.poll({
    generation: "g1",
    bound: true,
    agent: "claude",
    items: [
      { id: "u", kind: "user", text: "Review this" },
      {
        id: "t",
        kind: "tool_call",
        tool_name: "read_file",
        file_path: "src/main.rs",
      },
    ],
    next_offset: 3,
  });
  assert(f.find("chat-live-text").textContent.includes("read_file"));
  assert(
    !f.find("chat-live-text").textContent.includes("Review this"),
    "actual transcript confirmation removes provisional acknowledgement",
  );
  sess.state = "Waiting";
  sess.state_since_unix_ns = "3";
  f.update();
  await flush();
  assert.equal(f.find("chat-live-text").textContent, "Waiting for input");
  sess.state = "Done";
  sess.state_since_unix_ns = "4";
  f.update();
  await flush();
  assert.equal(f.find("chat-live-text").textContent, "Completed");
  f.state.connection.phase = "disconnected";
  f.update();
  assert.equal(
    f.find("chat-live-text").textContent,
    "Offline. Reconnect to update activity.",
  );
  f.dispose();
  const failure = fixture();
  await flush();
  failure.handler = async (req) => {
    if (req.action === "prompt") throw Error("timeout");
    return { items: [], next_offset: 0 };
  };
  failure.input("keep draft");
  failure.submit();
  await flush();
  assert.equal(failure.state.drafts.get(7), "keep draft");
  assert.equal(failure.find("chat-input").value, "keep draft");
  assert.equal(
    failure.find("chat-live-text").textContent,
    "Send not confirmed. Draft kept; check activity before retrying. · keep draft",
  );
  assert.equal(failure.find("primary composer-send").disabled, false);
  failure.state.connection.phase = "disconnected";
  failure.update();
  assert.equal(
    failure.find("chat-live-text").textContent,
    "Send not confirmed · Offline. Reconnect to update activity. · keep draft",
    "losing the connection never hides the failed send",
  );
  failure.dispose();
  const refreshed = fixture();
  await flush();
  refreshed.ctx.refresh = async () => {
    throw Error("snapshot failed");
  };
  refreshed.input("accepted request");
  refreshed.submit();
  await flush();
  assert.equal(refreshed.find("chat-input").value, "");
  assert.equal(
    refreshed.find("chat-live-text").textContent,
    "Accepted. Awaiting agent response… · accepted request",
    "refresh failure does not relabel an accepted prompt as send failure",
  );
  refreshed.dispose();
  const stale = fixture();
  await flush();
  stale.state.sessions.get(7).state = "Done";
  stale.input("new work");
  stale.submit();
  await flush();
  await stale.poll({
    generation: "g1",
    bound: true,
    items: [{ id: "newtool", kind: "tool_call", tool_name: "search" }],
    next_offset: 1,
  });
  stale.update();
  await flush();
  assert.notEqual(
    stale.find("chat-live-text").textContent,
    "Completed",
    "a stale pre-send Done snapshot is not completion evidence",
  );
  stale.dispose();
  const queued = fixture();
  await flush();
  queued.input("old host");
  queued.submit();
  queued.state.chatCache = new Map();
  await flush();
  assert(
    !queued.calls.some((c) => c.action === "prompt"),
    "host replacement before the queued action fences the send",
  );
  queued.dispose();
  console.log(
    "PASS composer progress: immediate provisional acknowledgement, accepted vs observed work/tool/wait/done/offline, transcript reconciliation and failure draft retry",
  );
}
async function opaqueCursorReplay() {
  // Actual mounted mobile code, with the independently observed decreasing token pair.
  const end = 5160216417328039,
    partial = 2908416603642792;
  assert(
    Number.isSafeInteger(end) && Number.isSafeInteger(partial) && partial < end,
  );
  const rows = (start, count) =>
    Array.from({ length: count }, (_, i) => ({
      id: `${start}:${i}`,
      kind: "assistant",
      text: `Record ${start}:${i}`,
    }));
  const f = fixture({
    handler: async () => ({
      generation: "g1",
      agent: "opencode",
      bound: true,
      page_start: 0,
      next_offset: end,
      items: rows(0, 1),
    }),
  });
  await flush();
  await f.poll({
    generation: "g1",
    agent: "opencode",
    bound: true,
    next_offset: partial,
    items: rows(end, 128),
  });
  assert.deepEqual(
    Array.from(f.entry.pages, (p) => [p.start, p.next]),
    [
      [0, end],
      [end, partial],
    ],
  );
  // Clip inside the decreasing-token page. Exact replay must restore its missing rows.
  for (let n = 0; n < 7; n++)
    await f.poll({
      generation: "g1",
      agent: "opencode",
      bound: true,
      next_offset: partial + n + 1,
      items: rows(partial + n, 128),
    });
  const first = f.entry.items[0];
  assert.equal(first._pageStart, end);
  assert(first._clipped);
  f.handler = async (req) => {
    assert.equal(req.offset, end);
    assert.equal(req.end_offset, partial);
    return {
      generation: "g1",
      bound: true,
      page_start: end,
      next_offset: partial,
      items: rows(end, 128),
    };
  };
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.errors.length, 0);
  assert.equal(f.entry.items[0].id, `${end}:0`);
  assert(f.entry.live);
  // Then replay the old end after later appends; it remains byte-for-byte the observed end.
  f.handler = async (req) => {
    assert.equal(req.offset, 0);
    assert.equal(req.end_offset, end);
    return {
      generation: "g1",
      bound: true,
      page_start: 0,
      next_offset: end,
      items: rows(0, 1),
    };
  };
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.entry.items[0].id, "0:0");
  assert.equal(
    f.root
      .descendants()
      .find(
        (n) =>
          n.attrs.role === "status" &&
          n.children.some((c) => c.textContent === "Retry"),
      ).hidden,
    true,
  );
  assert.equal(
    new Set(f.entry.items.map((i) => i.id)).size,
    f.entry.items.length,
  );
  f.handler = async (req) => ({
    generation: "g1",
    agent: "opencode",
    bound: true,
    next_offset: req.offset,
    items: [],
  });
  f.button("Back to latest").action();
  await flush();
  // A bounded read that fails closed discards stale state and restarts from latest.
  let calls = 0;
  f.handler = async (req) => {
    calls++;
    return req.latest
      ? {
          generation: "g1",
          agent: "opencode",
          bound: true,
          page_start: 0,
          next_offset: end,
          items: rows(0, 1),
        }
      : {
          generation: "g1",
          bound: false,
          reset: true,
          next_offset: 0,
          items: [],
        };
  };
  f.update();
  await flush();
  assert.equal(calls, 2);
  assert.equal(f.entry.items.length, 1);
  assert.equal(f.entry.offset, end);
  assert.equal(f.entry.pages.length, 1);
  f.dispose();
  // Eviction/deadline on the first earlier read must leave a usable escape to a
  // fresh latest boundary, without discarding the current paint until it succeeds.
  const stale = fixture({
    handler: async () => ({
      generation: "g1",
      agent: "opencode",
      bound: true,
      page_start: partial,
      next_offset: end,
      items: rows(partial, 2),
    }),
  });
  await flush();
  const old = stale.entry.items.slice();
  stale.handler = async () => ({
    generation: "g1",
    bound: false,
    reset: true,
    next_offset: 0,
    items: [],
  });
  stale.button("Load earlier messages").action();
  await flush();
  assert.deepEqual(stale.entry.items, old);
  assert.equal(stale.entry.live, undefined);
  assert.equal(stale.button("Back to latest").hidden, false);
  stale.handler = async (req) => {
    assert(req.latest);
    assert.equal(req.offset, 0);
    return {
      generation: "g1",
      agent: "opencode",
      bound: true,
      page_start: 100,
      next_offset: end + 1,
      items: rows(100, 2),
    };
  };
  stale.button("Back to latest").action();
  await flush();
  assert.equal(stale.entry.items[0].id, "100:0");
  assert.deepEqual(
    Array.from(stale.entry.pages, (p) => [p.start, p.next]),
    [[100, end + 1]],
  );
  stale.dispose();
  console.log(
    "PASS opaque SQLite tokens: decreasing end-to-partial remembered, clipped replay, immutable old end, live resource reset and expired-history latest recovery",
  );
}
async function recoveryPaint() {
  const E = 5160216417328039,
    P = 2908416603642792;
  const row = (n) => ({
    id: "stable" + n,
    kind: "assistant",
    text: "Row " + n,
  });
  const page = (start, end, items, extra = {}) => ({
    generation: "g1",
    agent: "opencode",
    bound: true,
    page_start: start,
    next_offset: end,
    items,
    ...extra,
  });
  const metadata = (f) =>
    JSON.stringify(
      [
        "pages",
        "offset",
        "generation",
        "agent",
        "bound",
        "loaded",
        "initialized",
        "retainedBytes",
        "lastSuccess",
      ].map((k) => f.entry[k]),
    );
  const unchanged = (f, old, cards, meta) => {
    assert.deepEqual(f.entry.items, old);
    assert.deepEqual(f.find("chat-feed").children, cards);
    assert.equal(metadata(f), meta);
    assert.equal(f.button("Retry").parentElement.hidden, false);
  };
  // Exact independent regressions: unbound latest after live reset, and after expired history.
  for (const origin of [
    "live",
    "history",
    "nested-history",
    "stale-mount",
    "legacy",
  ]) {
    for (const failure of ["unbound", "throw", "null"]) {
      const earlierResponse = (req) => origin === "nested-history" && req.end_offset === P
        ? page(P - 1, P, [row(4)])
        : page(0, 0, [], { bound: false, reset: true });
      const f = fixture({
        capabilities: { chat_tail: true },
        handler: async (req) => req.end_offset !== undefined
          ? earlierResponse(req)
          : page(P, E, [row(5), row(6)]),
      });
      await flush();
      if (origin === "nested-history") {
        f.handler = async () => page(P - 1, P, [row(4)]);
        f.button("Load earlier messages").action();
        await flush();
        assert(f.entry.live);
      }
      if (origin === "history" || origin === "nested-history") {
        f.handler = async () => page(0, 0, [], { bound: false, reset: true });
        f.button("Load earlier messages").action();
        await flush();
      }
      if (origin === "legacy") {
        f.entry.noTail = true;
        f.state.snapshot.capabilities = {};
      }
      const old = f.entry.items.slice(),
        cards = f.find("chat-feed").children.slice(),
        meta = metadata(f),
        live = f.entry.live;
      let n = 0;
      f.handler = async (req) => {
        if (req.end_offset !== undefined) return earlierResponse(req);
        n++;
        if (
          (origin === "live" || origin === "legacy") &&
          !req.latest &&
          req.offset !== 0
        )
          return page(0, 0, [], { bound: false, reset: true });
        assert.equal(req.offset, 0);
        if (origin !== "legacy") assert(req.latest);
        if (failure === "throw") throw Error("still overloaded");
        return failure === "null"
          ? null
          : page(0, 0, [], { bound: false, reset: false });
      };
      if (origin === "stale-mount") {
        f.entry.lastSuccess = 1;
        f.remount();
      } else if (origin === "history" || origin === "nested-history")
        f.button("Back to latest").action();
      else f.update();
      await flush();
      if (origin === "legacy") {
        assert.equal(n, 1);
        f.button("Retry").action();
        await flush();
      } else assert.equal(n, origin === "live" ? 2 : 1);
      // Remount creates new DOM nodes, but must retain the same transcript and page metadata.
      if (origin === "stale-mount") {
        assert.deepEqual(f.entry.items, old);
        assert.equal(f.entry.offset, E);
      } else unchanged(f, old, cards, meta);
      assert.equal(f.entry.live, live);
      assert.equal(
        f.button("Back to latest").hidden,
        false,
        origin + "/" + failure + " retains latest control",
      );
      // Recovery intent survives remount and every Retry goes straight to a fresh tail.
      if (origin === "history" && failure === "unbound") {
        f.remount();
        await flush();
        assert.deepEqual(f.entry.items, old);
      }
      f.handler = async (req) => {
        if (req.end_offset !== undefined) return earlierResponse(req);
        assert.equal(req.offset, 0);
        assert.equal(req.latest, origin === "legacy" ? undefined : true);
        return page(100, E + 1, [row(100)]);
      };
      f.button("Retry").action();
      await flush();
      assert.deepEqual(
        Array.from(f.entry.items, (x) => x.id),
        ["stable100"],
      );
      assert.deepEqual(
        Array.from(f.entry.pages, (p) => [p.start, p.next]),
        [[100, E + 1]],
      );
      assert.equal(f.entry.offset, E + 1);
      assert.equal(f.entry.live, null);
      assert.equal(f.button("Retry").parentElement.hidden, true);
      f.dispose();
    }
  }
  // A bound empty replacement is authoritative, and new generation data cannot merge old text.
  const empty = fixture({ handler: async () => page(P, E, [row(5)]) });
  await flush();
  empty.handler = async (req) =>
    req.latest
      ? page(0, 0, [], { generation: "g2" })
      : page(0, 0, [], { bound: false, reset: true });
  empty.update();
  await flush();
  assert.equal(empty.entry.items.length, 0);
  assert.equal(empty.entry.generation, "g2");
  assert.equal(
    empty.find("chat-empty").children[0].textContent,
    "Start a conversation",
  );
  empty.dispose();
  console.log(
    "PASS recovery paint: exact live-reset and expired-history unbound regressions; thrown/null failures, nested history, stale remount and legacy retry preserve paint/pages until successful fresh tail; bound empty clears",
  );
}
async function recoveryOwnership() {
  const oldPage = historyPage(200, 230),
    fresh = { ...historyPage(0, 1), generation: "fresh" };
  // An observed A -> B -> A identity cycle cannot revive A's pending response.
  for (const field of [
    "account_id",
    "profile_id",
    "provider",
    "agent",
    "cwd",
    "groupKey",
    "command_context",
  ]) {
    for (const earlier of [false, true]) {
      const f = fixture({ handler: async () => oldPage });
      await flush();
      const pending = deferred(),
        sess = f.state.sessions.get(7),
        original = sess[field];
      f.handler = () => pending.promise;
      if (earlier) f.button("Load earlier messages").action();
      else f.update();
      await flush();
      sess[field] = "other";
      f.update();
      assert.equal(
        f.entry.items.length,
        0,
        field + " identity clears old private paint",
      );
      assert.equal(f.find("chat-feed").children.length, 0);
      sess[field] = original;
      f.update();
      f.handler = async () => fresh;
      pending.resolve(earlier ? historyPage(190, 200) : historyPage(230, 231));
      await flush();
      f.update();
      await flush();
      assert.deepEqual(
        Array.from(f.entry.items, (x) => x.id),
        ["message0"],
      );
      assert.equal(f.entry.generation, "fresh");
      f.dispose();
    }
  }
  // Check a changed identity even when the snapshot update callback has not run yet.
  const silent = fixture({ handler: async () => oldPage });
  await flush();
  const stale = deferred();
  silent.handler = () => stale.promise;
  silent.update();
  silent.state.sessions.get(7).account_id = "new";
  stale.resolve(historyPage(230, 231));
  await flush();
  assert.equal(silent.entry.items.length, 0);
  assert.equal(silent.find("chat-feed").children.length, 0);
  silent.dispose();
  // Reconnect epoch/transport changes fence both the initial and second (fresh-tail) await.
  for (const stage of ["live", "tail", "earlier"])
    for (const change of [
      "link",
      "transport",
      "host",
      "session",
      "abort",
      "unmount",
      "navigation",
    ]) {
      const f = fixture({ handler: async () => oldPage });
      await flush();
      const old = f.entry,
        items = old.items.slice(),
        pages = JSON.stringify(old.pages),
        pending = deferred();
      f.handler = (req) =>
        stage === "tail" && !req.latest
          ? Promise.resolve({ ...oldPage, reset: true, items: [] })
          : pending.promise;
      if (stage === "earlier") f.button("Load earlier messages").action();
      else f.update();
      await flush();
      if (change === "link") f.state.connection.generation = 1;
      if (change === "transport") f.ctx.transport = {};
      if (change === "host") {
        f.state.chatCache = new Map();
        f.state.drafts = new Map();
      }
      if (change === "session") f.state.sessions.delete(7);
      if (change === "abort") f.ctx.signal = { aborted: true };
      if (change === "unmount") f.dispose();
      if (change === "navigation") f.button("Back to latest").action();
      // Do not allow a deliberately resumed poll to obscure whether this response was rejected.
      f.handler = async () => {
        throw Error("later retry");
      };
      pending.resolve(stage === "earlier" ? historyPage(190, 200) : fresh);
      await flush();
      assert.deepEqual(
        old.items,
        items,
        stage + "/" + change + " discards stale response",
      );
      assert.equal(JSON.stringify(old.pages), pages);
      assert.equal(old.offset, 230);
      f.dispose();
    }
  console.log(
    "PASS recovery ownership: identity changes clear private paint and fence A-B-A/no-update replies; link epoch, transport, host, session, abort, unmount and page navigation fence live/fresh-tail/earlier responses",
  );
}

async function liveQuestionSheet() {
  let question = {
      title: "Choose a way",
      identity: "q1",
      token: "t1",
      enabled: true,
      multiple: false,
      choices: [
        { label: "Keep current flow", checked: null },
        { label: "Change flow", checked: null },
      ],
    },
    replies = [],
    pending = deferred();
  const f = fixture({
    getQuestion: () => question,
    onAnswerQuestion: async (token, selected) => {
      replies.push({ token, selected: Array.from(selected) });
      return pending.promise;
    },
  });
  await flush();
  f.input("keep my chat draft");
  f.setQuestion(question);
  assert.equal(f.find("chat-live-question").hidden, false);
  assert.equal(f.find("chat-input").disabled, true);
  f.submit();
  await flush();
  assert(
    !f.calls.some((c) => c.action === "prompt"),
    "normal composer cannot write into the active native question",
  );
  f.button("Answer question").action();
  const sheet = f.sheets.at(-1),
    nodes = () => sheet.body.descendants(),
    choices = () =>
      nodes().filter((n) => n.className === "question-reply-choice"),
    send = nodes().find((n) => n.className === "primary question-reply-send");
  assert(send.disabled);
  choices()[1].action();
  assert.equal(replies.length, 0, "selecting only creates a local draft");
  assert.equal(choices()[1].attrs["aria-checked"], "true");
  assert(!send.disabled);
  send.action();
  send.action();
  await flush();
  assert.deepEqual(replies, [{ token: "t1", selected: [1] }]);
  assert(send.disabled, "pending reply is not sent twice");
  pending.reject(Error("delivery uncertain"));
  await flush();
  assert.equal(choices()[1].attrs["aria-checked"], "true");
  assert(
    nodes().some((n) => n.textContent === "delivery uncertain"),
    "failure is shown in the sheet without losing selection",
  );
  pending = deferred();
  question = { ...question, token: "t2" };
  f.setQuestion(question);
  send.action();
  await flush();
  assert.equal(replies.at(-1).token, "t2", "retry rechecks the live target");
  pending.resolve();
  await flush();
  assert.equal(f.entry.questionDraft.selected.length, 0);
  assert.equal(f.entry.questionSending, false);
  f.setQuestion(null);
  assert.equal(f.find("chat-input").value, "keep my chat draft");
  assert.equal(f.find("chat-input").disabled, false);
  f.dispose();
  question = {
    ...question,
    identity: "multi",
    multiple: true,
    token: "m1",
    choices: [
      { label: "First", checked: true },
      { label: "Second", checked: false },
    ],
  };
  const multi = fixture({
    getQuestion: () => question,
    onAnswerQuestion: async () => {
      throw Error("must not send");
    },
  });
  await flush();
  multi.setQuestion(question);
  multi.openQuestion();
  const body = multi.sheets.at(-1).body,
    rows = body
      .descendants()
      .filter((n) => n.className === "question-reply-choice");
  assert.equal(rows[0].attrs["aria-checked"], "true");
  rows[1].action();
  assert.deepEqual(Array.from(multi.entry.questionDraft.selected), [0, 1]);
  rows[0].action();
  rows[1].action();
  assert.equal(
    body
      .descendants()
      .find((n) => n.className === "primary question-reply-send").disabled,
    false,
    "native multi-select permits explicit empty submission",
  );
  assert(
    multi.entry.questionDraft.changed,
    "cleared selections still pin the draft",
  );
  rows[0].action();
  rows[1].action();
  multi.openQuestion();
  assert.deepEqual(
    Array.from(multi.entry.questionDraft.selected),
    [0, 1],
    "closing/reopening retains the local multi-select draft",
  );
  multi.setQuestion(null);
  assert.deepEqual(
    Array.from(multi.entry.questionDraft.selected),
    [0, 1],
    "disconnect retains the draft",
  );
  multi.setQuestion(question);
  const active = multi.sheets.at(-1).body,
    submit = active
      .descendants()
      .find((n) => n.className === "primary question-reply-send");
  question = { ...question, identity: "new", token: "m2" };
  multi.setQuestion(question);
  assert(submit.disabled);
  assert(
    active
      .descendants()
      .some((n) => String(n.textContent).includes("question changed")),
  );
  multi.setQuestion(null, { settled: true });
  assert.equal(
    multi.entry.questionDraft,
    null,
    "completed questions release drafts/cache/update pins",
  );
  multi.dispose();
  // Inline transcript question card matching liveQuestion: opens automatically and 1-tap option immediately proceeds.
  const inlineReplies = [];
  let inlineLive = {
    identity: "capture-q",
    token: "cap-1",
    title: "지금 찍을까요?",
    enabled: true,
    multiple: false,
    choices: [
      {
        label:
          "지금 찍어 (Recommended) 10초쯤 기다려 주시면 설정 열기, 시스템 순서로 찍어 둡니다",
        checked: null,
      },
      {
        label: "찍지 마 글 초안만 쓰고 캡처는 하지 않습니다",
        checked: null,
      },
      { label: "Type something.", checked: null },
      { label: "Chat about this", checked: null },
    ],
  };
  const inline = fixture({
    getQuestion: () => inlineLive,
    onAnswerQuestion: async (token, selected) => {
      inlineReplies.push({ token, selected: Array.from(selected) });
    },
  });
  await flush();
  await inline.poll({
    generation: "g-inline",
    items: [
      {
        id: "q-old",
        kind: "question",
        title: "이전",
        text: "Old question",
        multi_select: false,
        options: [{ label: "Run" }, { label: "Skip" }],
      },
      { id: "a-mid", kind: "assistant", text: "계속합니다." },
      {
        id: "q-cap",
        kind: "question",
        title: "캡처",
        text: "지금 찍을까요?",
        multi_select: false,
        options: [
          {
            label: "지금 찍어 (Recommended)",
            description: "10초쯤 기다려 주시면 설정 열기, 시스템 순서로 찍어 둡니다",
          },
          {
            label: "찍지 마",
            description: "글 초안만 쓰고 캡처는 하지 않습니다",
          },
        ],
      },
    ],
    next_offset: 10,
    bound: true,
  });
  inline.setQuestion(inlineLive);
  const qCards = inline.root
    .descendants()
    .filter((n) => n.className === "chat-card chat-question");
  assert.equal(qCards[0].dataset.liveQuestion, "false");
  assert.equal(qCards[1].dataset.liveQuestion, "true");
  assert.equal(qCards[1].children[0].open, true);
  assert.equal(
    inline.find("chat-live-question").hidden,
    true,
    "matched inline question card suppresses duplicate bottom banner while following",
  );
  inline.button("찍지 마 — 글 초안만 쓰고 캡처는 하지 않습니다").action();
  await flush();
  assert.deepEqual(inlineReplies, [{ token: "cap-1", selected: [1] }]);
  inlineLive = null;
  inline.setQuestion(null, { settled: true });
  await flush();
  assert.equal(qCards[1].dataset.liveQuestion, "false");
  assert.equal(qCards[1].children[0].open, false);
  inline.dispose();
  console.log(
    "PASS A+B current-question card/sheet: local selection, inline 1-tap proceed, disabled composer preserves its draft, duplicate send, failed-send retention, fresh retry token, multi-select/reopen, changed-question gate",
  );
}


async function workRunCoalescing() {
  const ko = fixture({ lang: "ko" });
  await flush();
  await ko.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    next_offset: 8,
    items: [
      { id: "u1", kind: "user", text: "리뷰해줘" },
      { id: "r1", kind: "reasoning", text: "파일 구조를 먼저 살핀다" },
      {
        id: "c1",
        kind: "tool_call",
        tool_id: "t1",
        tool_name: "Read",
        file_path: "/work/src/lib.rs",
        text: "/work/src/lib.rs",
      },
      { id: "o1", kind: "tool_result", tool_id: "t1", text: "pub fn run() {}" },
      { id: "r2", kind: "reasoning", text: "테스트를 실행해 확인한다" },
      {
        id: "c2",
        kind: "tool_call",
        tool_id: "t2",
        tool_name: "Bash",
        title: "cargo nextest",
        text: "cargo nextest run",
      },
      { id: "o2", kind: "tool_result", tool_id: "t2", text: "1 passed" },
      { id: "a1", kind: "assistant", text: "검토를 마쳤습니다." },
    ],
  });
  const koFeed = ko.find("chat-feed"),
    koRuns = ko.root
      .descendants()
      .filter((n) => n.className === "chat-tool-run");
  assert.equal(
    koRuns.length,
    1,
    "interleaved reasoning and tool items coalesce into one work run",
  );
  assert.deepEqual(
    koFeed.children.map((n) => n.className),
    ["chat-card chat-user", "chat-tool-run", "chat-card chat-assistant"],
  );
  const koRun = koRuns[0];
  assert.equal(koRun.open, false, "coalesced work run defaults closed");
  assert.equal(
    koRun.children[0].textContent,
    "파일 1개 확인 · 명령 1번 실행 · cargo nextest",
  );
  koRun.open = true;
  koRun.dispatchEvent(new Event("toggle"));
  assert.deepEqual(
    koRun.children[1].children.map((n) => n.className),
    [
      "chat-card chat-reasoning",
      "chat-tool-pair",
      "chat-card chat-reasoning",
      "chat-tool-pair",
    ],
  );
  assert.deepEqual(
    koRun.children[1].children[1].children.map((n) => n.className),
    ["chat-card chat-tool_call", "chat-card chat-tool_result"],
  );
  assert.deepEqual(
    koRun.children[1].children[1].children.map((n) => n.children[0].className),
    ["chat-tool-call", "chat-tool-result"],
  );
  assert.deepEqual(
    koRun.children[1].children[3].children.map((n) => n.className),
    ["chat-card chat-tool_call", "chat-card chat-tool_result"],
  );
  ko.dispose();

  const thoughtsKo = fixture({ lang: "ko" });
  await flush();
  await thoughtsKo.poll({
    generation: "g1",
    bound: true,
    next_offset: 2,
    items: [
      { id: "tk1", kind: "reasoning", text: "첫 번째 가설 점검\n세부 내용" },
      { id: "tk2", kind: "reasoning", text: "두 번째 경계 조건 확인\n세부 내용" },
    ],
  });
  const thoughtRun = thoughtsKo.find("chat-tool-run");
  assert(thoughtRun, "two contiguous reasoning items coalesce into a work run");
  assert.equal(thoughtRun.open, false);
  assert.equal(
    thoughtRun.children[0].textContent,
    "생각 2회 · 두 번째 경계 조건 확인",
  );
  thoughtsKo.dispose();

  const orphanEn = fixture({ lang: "en" });
  await flush();
  await orphanEn.poll({
    generation: "g1",
    bound: true,
    next_offset: 1,
    items: [
      { id: "or1", kind: "tool_result", text: "\n  orphan stdout line\nmore" },
    ],
  });
  assert.equal(
    orphanEn.find("chat-tool-run").children[0].textContent,
    "Tool activity · orphan stdout line",
  );
  orphanEn.dispose();

  const stream = fixture({ lang: "en" });
  await flush();
  await stream.poll({
    generation: "g1",
    bound: true,
    page_start: 0,
    next_offset: 1,
    items: [{ id: "sr1", kind: "reasoning", text: "Checking config" }],
  });
  const streamFeed = stream.find("chat-feed");
  assert.deepEqual(
    streamFeed.children.map((n) => n.className),
    ["chat-card chat-reasoning"],
  );
  assert.equal(stream.find("chat-tool-run"), undefined);
  await stream.poll({
    generation: "g1",
    bound: true,
    page_start: 1,
    next_offset: 3,
    items: [
      {
        id: "sc1",
        kind: "tool_call",
        tool_id: "st1",
        tool_name: "Read",
        file_path: "/work/cfg.toml",
        text: "/work/cfg.toml",
      },
      { id: "so1", kind: "tool_result", tool_id: "st1", text: "ok" },
    ],
  });
  assert.deepEqual(
    streamFeed.children.map((n) => n.className),
    ["chat-tool-run"],
    "streaming tool delta promotes preceding solo reasoning into one closed work run",
  );
  const promoted = streamFeed.children[0];
  assert.equal(promoted.open, false);
  assert.equal(
    promoted.children[0].textContent,
    "Read 1 file · /work/cfg.toml",
  );
  assert.deepEqual(
    promoted.children[1].children.map((n) => n.className),
    ["chat-card chat-reasoning", "chat-tool-pair"],
  );
  await stream.poll({
    generation: "g1",
    bound: true,
    page_start: 3,
    next_offset: 5,
    items: [
      { id: "sa1", kind: "assistant", text: "Config looks valid." },
      { id: "sr2", kind: "reasoning", text: "Standalone follow-up thought" },
    ],
  });
  assert.deepEqual(
    streamFeed.children.map((n) => n.className),
    [
      "chat-tool-run",
      "chat-card chat-assistant",
      "chat-card chat-reasoning",
    ],
  );
  stream.dispose();

  const paged = fixture({
    lang: "en",
    handler: async (req) =>
      req.end_offset !== undefined
        ? {
            generation: "g1",
            agent: "claude",
            bound: true,
            page_start: 0,
            next_offset: 100,
            items: [
              { id: "u0", kind: "user", text: "Check project" },
              {
                id: "c1",
                kind: "tool_call",
                tool_id: "p1",
                tool_name: "Read",
                file_path: "/work/first.rs",
                text: "/work/first.rs",
              },
              { id: "o1", kind: "tool_result", tool_id: "p1", text: "fn a()" },
            ],
          }
        : {
            generation: "g1",
            agent: "claude",
            bound: true,
            page_start: 100,
            next_offset: 200,
            items: [
              {
                id: "c2",
                kind: "tool_call",
                tool_id: "p2",
                tool_name: "Bash",
                title: "step 2",
                text: "echo 2",
              },
              { id: "o2", kind: "tool_result", tool_id: "p2", text: "2" },
              { id: "a2", kind: "assistant", text: "Reply 1" },
              { id: "a3", kind: "assistant", text: "Reply 2" },
              { id: "a4", kind: "assistant", text: "Reply 3" },
            ],
          },
  });
  await flush();
  const pagedFeed = paged.find("chat-feed"),
    openRun = paged.find("chat-tool-run");
  openRun.open = true;
  openRun.dispatchEvent(new Event("toggle"));
  const holdEarlier = deferred();
  paged.handler = (req) =>
    req.end_offset !== undefined
      ? holdEarlier.promise
      : Promise.resolve({
          generation: "g1",
          bound: true,
          page_start: 200,
          next_offset: 200,
          items: [],
        });
  scroll(pagedFeed, 200);
  scroll(pagedFeed, 25);
  await flush();
  const anchorOffset = openRun.offsetTop - pagedFeed.scrollTop;
  holdEarlier.resolve({
    generation: "g1",
    agent: "claude",
    bound: true,
    page_start: 0,
    next_offset: 100,
    items: [
      { id: "u0", kind: "user", text: "Check project" },
      {
        id: "c1",
        kind: "tool_call",
        tool_id: "p1",
        tool_name: "Read",
        file_path: "/work/first.rs",
        text: "/work/first.rs",
      },
      { id: "o1", kind: "tool_result", tool_id: "p1", text: "fn a()" },
    ],
  });
  await flush();
  assert.equal(
    pagedFeed.children[1],
    openRun,
    "loadEarlier prepend into an open work run keeps the exact details DOM node",
  );
  assert.equal(openRun.open, true, "open state survives loadEarlier prepend");
  assert.equal(
    openRun.offsetTop - pagedFeed.scrollTop,
    anchorOffset,
    "readerAnchor keeps the open work run pinned in viewport across prepend",
  );
  assert.equal(
    openRun.children[0].textContent,
    "Read 1 file · Ran 1 command · step 2",
  );
  assert.equal(openRun.children[1].children.length, 2);
  paged.remount();
  assert.equal(
    paged.find("chat-tool-run").open,
    true,
    "migrated disclosure key survives remount after loadEarlier",
  );
  paged.dispose();

  const trimmed = fixture({ lang: "en" });
  await flush();
  await trimmed.poll({
    generation: "g1",
    bound: true,
    page_start: 0,
    next_offset: 1000,
    items: [
      { id: "tc0", kind: "tool_call", tool_name: "Read", file_path: "a.rs" },
      { id: "tc1", kind: "tool_call", tool_name: "Read", file_path: "b.rs" },
      ...Array.from({ length: 998 }, (_, i) => ({
        id: `ta${i}`,
        kind: "assistant",
        text: `Msg ${i}`,
      })),
    ],
  });
  const trimRun = trimmed.find("chat-tool-run");
  trimRun.open = true;
  trimRun.dispatchEvent(new Event("toggle"));
  await trimmed.poll({
    generation: "g1",
    bound: true,
    page_start: 1000,
    next_offset: 1001,
    items: [{ id: "ta998", kind: "assistant", text: "Msg 998" }],
  });
  assert.equal(
    trimmed.find("chat-tool-run"),
    trimRun,
    "front trim across MAX_RENDERED_ITEMS preserves the surviving work run DOM node",
  );
  assert.equal(trimRun.open, true);
  assert.equal(trimRun.children[0].textContent, "Read 1 file · b.rs");
  trimmed.dispose();

  const boundaries = fixture({ lang: "en" });
  await flush();
  await boundaries.poll({
    generation: "g1",
    bound: true,
    next_offset: 8,
    items: [
      { id: "br0", kind: "reasoning", text: "Solo thought" },
      { id: "ba1", kind: "assistant", text: "Interim message\nSecond line" },
      { id: "bc1", kind: "tool_call", tool_name: "Read", file_path: "x.rs" },
      {
        id: "bq1",
        kind: "question",
        text: "Proceed?",
        options: [{ label: "Yes" }],
      },
      { id: "bc2", kind: "tool_call", tool_name: "Bash", text: "ls" },
      { id: "bp1", kind: "permission", title: "Allow write", text: "Write y" },
      { id: "br2", kind: "reasoning", text: "Preparing edit" },
      { id: "bc3", kind: "tool_call", tool_name: "Edit", file_path: "y.rs" },
    ],
  });
  assert.deepEqual(
    boundaries.find("chat-feed").children.map((n) => n.className),
    [
      "chat-card chat-reasoning",
      "chat-card chat-assistant",
      "chat-tool-run",
      "chat-card chat-question",
      "chat-tool-run",
      "chat-card chat-permission",
      "chat-tool-run",
    ],
    "solo reasoning stays top-level and question/permission items break runs",
  );
  assert.equal(boundaries.find("chat-reasoning").tag, "details");
  boundaries.dispose();

  const css = fs.readFileSync(
    path.join(__dirname, "../src/mobile/assets/app.css"),
    "utf8",
  );
  assert(
    css.includes(
      ".chat-tool-run{border:0;border-radius:0;color:var(--muted-foreground);background:transparent;padding:0}",
    ),
    "work run container is borderless",
  );
  assert(
    css.includes(
      ".chat-tool-items{display:flex;flex-direction:column;gap:2px;margin-left:7px;margin-top:2px;padding:0 0 0 13px;border-left:1px solid var(--border)}",
    ),
    "work run items use Orca 1px left vertical guide line",
  );
  assert(
    css.includes(
      ".chat-tool-items>:is(.chat-card,.chat-tool-pair){border:0;border-radius:0;padding:2px 0;background:transparent}",
    ),
    "work run items drop heavy row dividers",
  );
  assert(
    css.includes(
      ".chat-tool-pair{display:flex;flex-direction:column;gap:2px;min-width:0}",
    ),
  );
  assert(
    css.includes(
      ".chat-tool-pair>.chat-card{border:0;border-radius:0;padding:0;background:transparent}",
    ),
    "paired tool cards strip nested card borders",
  );
  assert(
    css.includes(
      "#mobile-app :is(button,summary){min-width:var(--touch-target);min-height:var(--touch-target)}",
    ),
    "44px touch target floor remains intact",
  );
  console.log(
    "PASS work-run coalescing: interleaved thoughts/tools, streaming promotion, loadEarlier/trim DOM & anchor retention, question/permission boundaries and borderless guide-line CSS",
  );
}

async function codeBlockHeaderWrapAndCopy() {
  const icon = (name) => {
    const svg = new Element("svg"),
      p = new Element("path");
    p.setAttribute("d", name === "check" ? "M20 6 9 17l-5-5" : "");
    svg.append(p);
    return svg;
  };
  const f = fixture({ icon });
  await flush();
  assert.equal(f.M.chat.fenceLang("ts"), "TypeScript");
  assert.equal(f.M.chat.fenceLang("tsx"), "TSX");
  assert.equal(f.M.chat.fenceLang("js"), "JavaScript");
  assert.equal(f.M.chat.fenceLang("py"), "Python");
  assert.equal(f.M.chat.fenceLang("rs"), "Rust");
  assert.equal(f.M.chat.fenceLang("rust"), "Rust");
  assert.equal(f.M.chat.fenceLang("sh"), "Bash");
  assert.equal(f.M.chat.fenceLang("bash"), "Bash");
  assert.equal(f.M.chat.fenceLang("yml"), "YAML");
  assert.equal(f.M.chat.fenceLang("yaml"), "YAML");
  assert.equal(f.M.chat.fenceLang("md"), "Markdown");
  assert.equal(
    f.M.chat.fenceLang('  TypeScript\u0007 title="app.ts" {1,3}'),
    "TypeScript",
  );
  assert.equal(f.M.chat.fenceLang(""), "Code");
  assert.equal(f.M.chat.fenceLang("   \u001b\u0000 "), "Code");
  assert.deepEqual(
    Array.from(
      f.M.chat.parseRichBlocks(
        '서론\n````md\n```rust\nlet x = 1;\n```\n````\nuse ```inline``` safely\n~~~sh\nprintf "streaming"',
      ),
      (b) => ({ ...b }),
    ),
    [
      { kind: "prose", text: "서론" },
      {
        kind: "code",
        lang: "Markdown",
        code: "```rust\nlet x = 1;\n```",
        blockIndex: 0,
      },
      { kind: "prose", text: "use ```inline``` safely" },
      {
        kind: "code",
        lang: "Bash",
        code: 'printf "streaming"',
        blockIndex: 1,
      },
    ],
  );

  const baseBlocks =
    '안내 문장\n```rs\nfn first() -> i32 { 1 }\n```\n중간 설명\n```TypeScript title="app.ts"\nexport const answer = 42;\n```\n```\nplain snippet\n```\n```py\nprint("part 1")';
  await f.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    next_offset: 1,
    items: [{ id: "m1", kind: "assistant", text: baseBlocks }],
  });
  const wraps = () =>
    f.root.descendants().filter((n) => n.className === "chat-code-wrap");
  assert.equal(wraps().length, 4);
  for (const wrap of wraps()) {
    assert.equal(wrap.children[0].className, "chat-code-head");
    assert.equal(wrap.children[0].children[0].className, "chat-code-lang");
    assert.equal(wrap.children[0].children[1].className, "chat-code-actions");
    assert.equal(
      wrap.children[0].children[1].children[0].className,
      "chat-code-wrap-btn",
    );
    assert.equal(
      wrap.children[0].children[1].children[1].className,
      "chat-code-copy",
    );
    assert.equal(wrap.children[1].tag, "pre");
    assert.equal(wrap.children[1].className, "chat-code");
    assert.equal(wrap.children[1].children[0].tag, "code");
  }
  assert.deepEqual(
    wraps().map((w) => w.children[0].children[0].textContent),
    ["Rust", "TypeScript", "Code", "Python"],
  );
  const wrapBtns = () =>
    wraps().map((w) => w.children[0].children[1].children[0]);
  const copyBtns = () =>
    wraps().map((w) => w.children[0].children[1].children[1]);
  assert.deepEqual(
    copyBtns().map((b) => b.textContent),
    ["Copy", "Copy", "Copy", "Copy"],
  );
  assert.equal(wrapBtns()[1].attrs["aria-pressed"], "false");
  assert.equal(wrapBtns()[1].attrs["aria-label"], "Wrap lines");
  assert.equal(wraps()[1].children[1].dataset.wrapped, undefined);

  // Toggle line-wrap on block 1 and verify disclosure + data-wrapped persist across streaming & non-incremental polls.
  wrapBtns()[1].click();
  assert.equal(wrapBtns()[1].attrs["aria-pressed"], "true");
  assert.equal(wrapBtns()[1].attrs["aria-label"], "Disable line wrap");
  assert.equal(wraps()[1].children[1].dataset.wrapped, "true");
  assert.equal(f.entry.disclosures.get("code-wrap:g1:m1:1"), true);

  const timersBefore = new Set(f.timers.keys());
  copyBtns()[1].click();
  await flush();
  assert.equal(
    f.copied.at(-1),
    "export const answer = 42;",
    "clicking individual code copy copies only that block's code",
  );
  assert.equal(copyBtns()[1].dataset.copied, "true");
  assert.equal(copyBtns()[1].textContent, "Copied");
  assert.equal(copyBtns()[1].attrs["aria-label"], "Copied");
  assert(
    copyBtns()[1].descendants().some((n) => n.tag === "svg"),
    "copied state renders check SVG mark",
  );
  assert.equal(copyBtns()[0].dataset.copied, undefined);
  const addedTimers = [...f.timers.entries()].filter(
    ([id]) => !timersBefore.has(id),
  );
  assert.equal(addedTimers.length, 1);
  assert.equal(addedTimers[0][1].ms, 1500);

  // Streaming incremental update + same-generation reset both preserve Copied and code-wrap by blockIndex.
  await f.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    next_offset: 2,
    items: [
      {
        id: "m1",
        kind: "assistant",
        text: baseBlocks + '\nprint("part 2")',
      },
    ],
  });
  assert.equal(copyBtns()[1].dataset.copied, "true");
  assert.equal(wrapBtns()[1].attrs["aria-pressed"], "true");
  assert.equal(wraps()[1].children[1].dataset.wrapped, "true");

  await f.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    reset: true,
    next_offset: 3,
    items: [
      {
        id: "m1",
        kind: "assistant",
        text: baseBlocks + '\nprint("part 2")\n```\n완료',
      },
    ],
  });
  assert.equal(
    copyBtns()[1].dataset.copied,
    "true",
    "same-generation streaming reset preserves Copied state by blockIndex",
  );
  assert.equal(
    wraps()[1].children[1].dataset.wrapped,
    "true",
    "same-generation streaming reset preserves line-wrap disclosure by blockIndex",
  );

  // Non-incremental tail refresh keeps code-wrap disclosure in entry.disclosures.
  f.setVisible(false);
  f.setVisible(true);
  await f.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    next_offset: 4,
    items: [
      {
        id: "m1",
        kind: "assistant",
        text: baseBlocks + '\nprint("part 2")\n```\n완료',
      },
      { id: "m2", kind: "assistant", text: "trailing reply" },
    ],
  });
  assert.equal(f.entry.disclosures.get("code-wrap:g1:m1:1"), true);

  // Expiring the 1.5s timer reverts the post-stream live button in place.
  const [copyTimerId, copyTimerFn] = addedTimers[0];
  f.timers.delete(copyTimerId);
  copyTimerFn();
  assert.equal(copyBtns()[1].dataset.copied, undefined);
  assert.equal(copyBtns()[1].textContent, "Copy");
  assert.equal(copyBtns()[1].attrs["aria-label"], "Copy");
  assert.equal(copyBtns()[1].children.length, 0);
  f.dispose();

  const ko = fixture({ lang: "ko", icon });
  await flush();
  await ko.poll({
    generation: "g-ko",
    agent: "claude",
    bound: true,
    next_offset: 1,
    items: [{ id: "k1", kind: "assistant", text: "```\necho 안녕\n```" }],
  });
  const koLang = ko.find("chat-code-lang"),
    koWrapBtn = ko.find("chat-code-wrap-btn"),
    koBtn = ko.find("chat-code-copy");
  assert.equal(koLang.textContent, "코드");
  assert.equal(koWrapBtn.attrs["aria-label"], "줄 바꿈 켜기");
  koWrapBtn.click();
  assert.equal(koWrapBtn.attrs["aria-label"], "줄 바꿈 끄기");
  assert.equal(koBtn.textContent, "복사");
  koBtn.click();
  await flush();
  assert.equal(ko.copied.at(-1), "echo 안녕");
  assert.equal(koBtn.dataset.copied, "true");
  assert.equal(koBtn.textContent, "복사됨");
  ko.dispose();
  console.log(
    "PASS mobile code block normalization, wrap toggle, individual copy, and streaming-preserved 1.5s Copied feedback",
  );
}

async function messageRowsFoldingAndReasoning() {
  const short8Lines = Array.from({ length: 8 }, (_, i) => `line ${i + 1}`).join(
      "\n",
    ),
    long9Lines = Array.from({ length: 9 }, (_, i) => `line ${i + 1}`).join("\n"),
    exact600 = "a".repeat(600),
    over601 = "a".repeat(601);
  const f = fixture({ lang: "en" });
  await flush();
  await f.poll({
    generation: "g-rows",
    agent: "claude",
    bound: true,
    next_offset: 6,
    items: [
      { id: "u-short", kind: "user", text: short8Lines },
      { id: "u-exact", kind: "user", text: exact600 },
      { id: "u-9lines", kind: "user", text: long9Lines },
      { id: "u-601", kind: "user", text: over601 },
      {
        id: "r-1",
        kind: "reasoning",
        text: "Thinking step one",
        status: "completed",
      },
      { id: "a-1", kind: "assistant", text: "Done." },
    ],
  });

  // User & assistant rows never emit <header class="chat-role"> in DOM.
  assert.equal(
    f.root.descendants().filter((n) => n.className === "chat-role").length,
    0,
    "user and assistant cards emit no chat-role header in DOM",
  );
  const userCards = f.root
    .descendants()
    .filter((n) => n.className === "chat-card chat-user");
  assert.equal(userCards.length, 4);
  for (const card of userCards) {
    assert.equal(card.children[0].className, "chat-user-bubble");
    assert.equal(card.children[1].className, "chat-meta");
  }
  assert.equal(
    userCards[0].descendants().some((n) => n.className === "chat-user-fold"),
    false,
    "8-line user prompt is not folded",
  );
  assert.equal(
    userCards[1].descendants().some((n) => n.className === "chat-user-fold"),
    false,
    "600-char user prompt is not folded",
  );
  const fold9 = userCards[2]
      .descendants()
      .find((n) => n.className === "chat-user-fold"),
    toggle9 = userCards[2]
      .descendants()
      .find((n) => n.className === "ghost small chat-user-fold-toggle"),
    fold601 = userCards[3]
      .descendants()
      .find((n) => n.className === "chat-user-fold"),
    toggle601 = userCards[3]
      .descendants()
      .find((n) => n.className === "ghost small chat-user-fold-toggle");
  assert(fold9 && toggle9, "9-line user prompt folds");
  assert(fold601 && toggle601, "601-char user prompt folds");
  assert.equal(fold9.dataset.folded, "true");
  assert.equal(toggle9.textContent, "Show full message");
  assert.equal(toggle9.attrs["aria-expanded"], "false");
  toggle9.click();
  assert.equal(fold9.dataset.folded, "false");
  assert.equal(toggle9.textContent, "Show less");
  assert.equal(toggle9.attrs["aria-expanded"], "true");
  assert.equal(f.entry.disclosures.get("user:g-rows:u-9lines"), true);

  // Non-incremental update keeps user fold disclosure.
  await f.poll({
    generation: "g-rows",
    agent: "claude",
    bound: true,
    reset: true,
    next_offset: 7,
    items: [
      { id: "u-9lines", kind: "user", text: long9Lines },
      {
        id: "r-1",
        kind: "reasoning",
        text: "Thinking step one",
        status: "completed",
      },
      { id: "a-2", kind: "assistant", text: "Follow-up" },
    ],
  });
  const toggleAfter = f.find("ghost small chat-user-fold-toggle"),
    foldAfter = f.find("chat-user-fold");
  assert.equal(foldAfter.dataset.folded, "false");
  assert.equal(toggleAfter.textContent, "Show less");

  // Reasoning single-line disclosure (brain SVG + Thought/Reasoning + chevron) + 22px indented body.
  const reasonCard = f.find("chat-card chat-reasoning"),
    reasonDetails = f.find("chat-reasoning");
  assert(reasonCard && reasonDetails && reasonDetails.tag === "details");
  assert.equal(reasonDetails.children.length, 1);
  assert.equal(
    reasonDetails.children[0].children[0].className,
    "chat-reasoning-icon",
  );
  assert(
    reasonDetails.children[0].children[0]
      .descendants()
      .some((n) => n.tag === "svg"),
  );
  assert.equal(
    reasonDetails.children[0].children[1].className,
    "chat-reasoning-headline",
  );
  assert.equal(reasonDetails.children[0].children[1].textContent, "Thought");
  assert.equal(
    reasonDetails.children[0].children[2].className,
    "chat-reasoning-caret",
  );
  reasonDetails.open = true;
  reasonDetails.dispatchEvent(new Event("toggle"));
  assert.equal(reasonDetails.children.length, 2);
  assert.equal(reasonDetails.children[1].className, "chat-reasoning-body");
  f.dispose();

  // Korean labels and optimistic "Sending…" status.
  const sending = deferred();
  const ko = fixture({
    lang: "ko",
    handler: async (req) =>
      req.action === "prompt"
        ? sending.promise
        : { generation: "g-k", agent: "claude", bound: true, items: [], next_offset: 0 },
  });
  await flush();
  ko.input("가".repeat(601));
  ko.submit();
  const opt = ko.find("chat-card chat-user chat-optimistic");
  assert.equal(opt.dataset.phase, "sending");
  assert.equal(
    opt.descendants().find((n) => n.className === "chat-optimistic-status")
      ?.textContent,
    "전송 중…",
  );
  assert.equal(
    opt.descendants().find((n) => n.className === "ghost small chat-user-fold-toggle")
      ?.textContent,
    "전체 메시지 보기",
  );
  sending.resolve({ sent: true });
  await flush();
  assert.equal(opt.dataset.phase, "accepted");
  assert.equal(
    opt.descendants().find((n) => n.className === "chat-optimistic-status"),
    undefined,
  );
  ko.dispose();
  console.log(
    "PASS Orca message rows: no role headers, intrinsic bubble folding, Sending… status, and quiet reasoning disclosure",
  );
}

async function workRunProgressElapsedAndPreamble() {
  // 1) Preamble folding (pre-run & mid-run <=160 scalar single-line) + Korean 160/161 boundary + final answer preservation.
  const exact160Ko = "가".repeat(160),
    over161Ko = "가".repeat(161);
  const foldKo = fixture({ lang: "ko" });
  await flush();
  await foldKo.poll({
    generation: "g1",
    agent: "claude",
    bound: true,
    next_offset: 8,
    items: [
      { id: "fu1", kind: "user", text: "확인해서 고쳐줘" },
      {
        id: "fp0",
        kind: "assistant",
        text: "먼저 관련 파일을 읽고 확인하겠습니다.",
        at: "2026-11-16T00:00:00Z",
      },
      {
        id: "fc1",
        kind: "tool_call",
        tool_id: "ft1",
        tool_name: "Read",
        title: "Read chat.js",
        at: "2026-11-16T00:00:01Z",
      },
      {
        id: "fr1",
        kind: "tool_result",
        tool_id: "ft1",
        text: "function renderItems() {}",
        at: "2026-11-16T00:00:05Z",
      },
      {
        id: "fp1",
        kind: "assistant",
        text: exact160Ko,
        at: "2026-11-16T00:00:06Z",
      },
      {
        id: "fc2",
        kind: "tool_call",
        tool_id: "ft2",
        tool_name: "Edit",
        title: "Edit chat.js",
        at: "2026-11-16T00:00:07Z",
      },
      {
        id: "fr2",
        kind: "tool_result",
        tool_id: "ft2",
        text: "updated",
        at: "2026-11-16T00:00:12Z",
      },
      {
        id: "ff1",
        kind: "assistant",
        text: "모든 수정과 검증을 완료했습니다.",
        at: "2026-11-16T00:00:13Z",
      },
    ],
  });
  const foldFeed = foldKo.find("chat-feed");
  assert.deepEqual(
    foldFeed.children.map((n) => n.className),
    ["chat-card chat-user", "chat-tool-run", "chat-card chat-assistant"],
    "short pre-run and mid-run preambles fold into one work run while final answer stays a body card",
  );
  const foldedRun = foldFeed.children[1],
    finalCard = foldFeed.children[2];
  assert.equal(foldedRun.dataset.active, undefined);
  assert.equal(foldedRun.dataset.elapsed, "12초");
  assert.equal(
    foldedRun.children[0].textContent,
    "파일 1개 확인 · 파일 1개 수정 · Edit chat.js · 12초",
  );
  assert.deepEqual(
    foldedRun.children[1].children.map((n) => n.className),
    [
      "chat-card chat-work-note",
      "chat-tool-pair",
      "chat-card chat-work-note",
      "chat-tool-pair",
    ],
  );
  assert.equal(foldedRun.children[1].children[0].children[0].textContent, "메모");
  assert.equal(
    foldedRun.children[1].children[0].children[1].textContent,
    "먼저 관련 파일을 읽고 확인하겠습니다.",
  );
  assert.equal(
    foldedRun.children[1].children[2].children[1].textContent,
    exact160Ko,
  );
  // ToolLine rows render inline icon + muted verb + target without badge pills.
  const firstToolHead =
    foldedRun.children[1].children[1].children[0].children[0].children[0];
  assert.equal(firstToolHead.children[0].className, "chat-tool-icon");
  assert.equal(firstToolHead.children[1].className, "chat-tool-verb");
  assert.equal(firstToolHead.children[1].textContent, "확인");
  assert.equal(firstToolHead.children[2].className, "chat-tool-title");
  assert.equal(
    firstToolHead.descendants().some((n) => n.className === "badge"),
    false,
  );
  assert(
    finalCard.descendants().some((n) => n.className === "chat-md"),
    "final assistant card retains full chat-md body tone",
  );
  foldKo.dispose();

  // 2) Reject folding for 161-scalar Korean, multiline, code fence, or markdown block starters.
  const unfold = fixture({ lang: "en" });
  await flush();
  await unfold.poll({
    generation: "g1",
    bound: true,
    next_offset: 9,
    items: [
      { id: "na1", kind: "assistant", text: over161Ko },
      { id: "nc1", kind: "tool_call", tool_name: "Read", file_path: "a.rs" },
      { id: "na2", kind: "assistant", text: "Check ```js\nx\n``` next" },
      { id: "nc2", kind: "tool_call", tool_name: "Read", file_path: "b.rs" },
      { id: "na3", kind: "assistant", text: "- bullet list item" },
      { id: "nc3", kind: "tool_call", tool_name: "Read", file_path: "c.rs" },
      { id: "na4", kind: "assistant", text: "# Heading block" },
      { id: "nc4", kind: "tool_call", tool_name: "Read", file_path: "d.rs" },
      { id: "na5", kind: "assistant", text: "1. Numbered step" },
    ],
  });
  assert.deepEqual(
    unfold.find("chat-feed").children.map((n) => n.className),
    [
      "chat-card chat-assistant",
      "chat-tool-run",
      "chat-card chat-assistant",
      "chat-tool-run",
      "chat-card chat-assistant",
      "chat-tool-run",
      "chat-card chat-assistant",
      "chat-tool-run",
      "chat-card chat-assistant",
    ],
    "161-char Korean, codefence, bullet, heading, and trailing solo assistant remain top-level cards",
  );
  unfold.dispose();

  // 3) Live streaming: short preamble promotes when tool arrives, active Working header shows m/n + current tool title,
  //    and completion shows 1m 04s elapsed (or omits elapsed when timestamps are missing/invalid/<1s).
  let nowMs = Date.parse("2026-11-16T01:00:00Z");
  class FakeDate extends Date {
    static now() {
      return nowMs;
    }
  }
  const live = fixture({ lang: "en", Date: FakeDate });
  live.state.sessions.get(7).state = "Working";
  await flush();
  await live.poll({
    generation: "g1",
    bound: true,
    page_start: 0,
    next_offset: 2,
    items: [
      { id: "lu1", kind: "user", text: "Fix header" },
      {
        id: "lp1",
        kind: "assistant",
        text: "Reading the chat screen source.",
        at: "2026-11-16T01:00:00Z",
      },
    ],
  });
  const liveFeed = live.find("chat-feed");
  assert.deepEqual(
    liveFeed.children.map((n) => n.className),
    ["chat-card chat-user", "chat-card chat-assistant"],
  );

  nowMs = Date.parse("2026-11-16T01:00:03Z");
  await live.poll({
    generation: "g1",
    bound: true,
    page_start: 2,
    next_offset: 3,
    items: [
      {
        id: "lc1",
        kind: "tool_call",
        tool_id: "lt1",
        tool_name: "Read",
        title: "Read screens/chat.js",
        at: "2026-11-16T01:00:01Z",
      },
    ],
  });
  assert.deepEqual(
    liveFeed.children.map((n) => n.className),
    ["chat-card chat-user", "chat-tool-run"],
    "streaming tool call absorbs preceding short preamble into active work run",
  );
  const liveRun = liveFeed.children[1];
  assert.equal(liveRun.dataset.active, "true");
  assert.equal(liveRun.dataset.progress, "0/1");
  assert.equal(liveRun.dataset.elapsed, "3s");
  assert.equal(
    liveRun.children[0].textContent,
    "Working · 0/1 · Read screens/chat.js · Read 1 file · 3s",
  );

  nowMs = Date.parse("2026-11-16T01:00:15Z");
  await live.poll({
    generation: "g1",
    bound: true,
    page_start: 3,
    next_offset: 5,
    items: [
      {
        id: "lr1",
        kind: "tool_result",
        tool_id: "lt1",
        text: "ok",
        at: "2026-11-16T01:00:08Z",
      },
      {
        id: "lc2",
        kind: "tool_call",
        tool_id: "lt2",
        tool_name: "Edit",
        title: "Edit screens/chat.js",
        at: "2026-11-16T01:00:10Z",
      },
    ],
  });
  assert.equal(liveRun.dataset.active, "true");
  assert.equal(liveRun.dataset.progress, "1/2");
  assert.equal(liveRun.dataset.elapsed, "15s");
  assert.equal(
    liveRun.children[0].textContent,
    "Working · 1/2 · Edit screens/chat.js · Read 1 file · Changed 1 file · 15s",
  );

  nowMs = Date.parse("2026-11-16T01:01:05Z");
  live.state.sessions.get(7).state = "Done";
  await live.poll({
    generation: "g1",
    bound: true,
    page_start: 5,
    next_offset: 7,
    items: [
      {
        id: "lr2",
        kind: "tool_result",
        tool_id: "lt2",
        text: "done",
        at: "2026-11-16T01:01:04Z",
      },
      {
        id: "lf1",
        kind: "assistant",
        text: "Header progress and elapsed time are ready.",
        at: "2026-11-16T01:01:05Z",
      },
    ],
  });
  assert.deepEqual(
    liveFeed.children.map((n) => n.className),
    ["chat-card chat-user", "chat-tool-run", "chat-card chat-assistant"],
  );
  assert.equal(liveRun.dataset.active, undefined);
  assert.equal(liveRun.dataset.progress, undefined);
  assert.equal(liveRun.dataset.elapsed, "1m 04s");
  assert.equal(
    liveRun.children[0].textContent,
    "Read 1 file · Changed 1 file · Edit screens/chat.js · 1m 04s",
  );
  live.dispose();

  // 4) Invalid or sub-second timestamps omit elapsed badge cleanly.
  const noElapsed = fixture({ lang: "en" });
  await flush();
  await noElapsed.poll({
    generation: "g1",
    bound: true,
    next_offset: 2,
    items: [
      {
        id: "ne1",
        kind: "tool_call",
        tool_id: "nt1",
        tool_name: "Bash",
        title: "git status",
        at: "not-a-timestamp",
      },
      {
        id: "ne2",
        kind: "tool_result",
        tool_id: "nt1",
        text: "clean",
        at: "2026-11-16T01:00:00.400Z",
      },
    ],
  });
  const neRun = noElapsed.find("chat-tool-run");
  assert.equal(neRun.dataset.elapsed, undefined);
  assert.equal(neRun.children[0].textContent, "Ran 1 command · git status");
  noElapsed.dispose();
  console.log(
    "PASS work-run progress, elapsed & preamble folding: 160-scalar boundary, active m/n + tool title, 12s/1m 04s elapsed and omission",
  );
}

async function effortOrdering() {
  const f = fixture({
    capabilities: { chat_composer: true, composer_models: true, model_set: true },
    handler: async (req) => {
      if (req.action === "chat_composer")
        return {
          agent: "codex",
          model: "gpt-6.1-sol",
          effort: "high",
          effort_supported: true,
          models: [
            {
              id: "gpt-6.1-sol",
              label: "GPT-6.1 Sol",
              efforts: ["max", "xhigh", "high", "medium", "low"],
            },
          ],
          efforts: [],
        };
      if (req.action === "composer_models")
        return {
          supported: true,
          current: "gpt-6.1-sol",
          items: [
            {
              id: "gpt-6.1-sol",
              label: "GPT-6.1 Sol",
              efforts: ["max", "xhigh", "high", "medium", "low"],
            },
          ],
        };
      return { generation: "g-eff", agent: "codex", bound: true, items: [], next_offset: 0 };
    },
  });
  await flush();
  f.find("composer-pill").action();
  await flush();
  const chips = f
    .find("chat-composer-options")
    .children.flatMap((row) => row.children || []);
  assert.deepEqual(
    chips.slice(1).map((c) => c.children[0].textContent),
    ["Low", "Medium", "High", "Extra high", "Max"],
    "effort chips sort lowest-to-highest even when backend reports highest-first",
  );
  f.dispose();
  console.log("PASS effort chips sort lowest-to-highest");
}

async function main() {
  await codeBlockHeaderWrapAndCopy();
  await messageRowsFoldingAndReasoning();
  await effortOrdering();
  await require("./chat_release_ui.cjs")({fixture,flush,deferred,scroll,historyPage});
  await workRunCoalescing();
  await workRunProgressElapsedAndPreamble();
  await liveQuestionSheet();
  await recoveryPaint();
  await recoveryOwnership();
  await opaqueCursorReplay();
  await composerDiscovery();
  await composerRaces();
  await composerProgress();
  await quickActions();
  await imeSendTap();
  await markdownFormatting();
  await orcaComposerAndMeta();
  await hiddenPrefetchAndOldDaemon();
  await tailFollow();
  await stableLatestRefresh();
  await renderBudgets();
  const cold = deferred(),
    recent = Array.from({ length: 128 }, (_, i) => ({
      id: `872:${i}`,
      kind: "assistant",
      text: `Message ${872 + i}`,
    }));
  let now = Date.now();
  const chatClock = class extends Date {
    static now() {
      return now;
    }
  };
  const warm = fixture({
    visible: false,
    Date: chatClock,
    handler: () => cold.promise,
  });
  assert.equal(warm.calls[0].latest, true);
  assert.equal(
    warm.find("chat-empty"),
    undefined,
    "no false empty transcript before the first reply",
  );
  cold.resolve({
    generation: "g1",
    agent: "claude",
    bound: true,
    items: recent,
    page_start: 872,
    next_offset: 1000,
    has_earlier: true,
  });
  await flush();
  assert.equal(warm.calls.length, 1, "one request reaches the current tail");
  assert.equal(warm.entry.items.at(-1).text, "Message 999");
  const preparedFeed = warm.find("chat-feed"),
    preparedLast = preparedFeed.children.at(-1);
  warm.handler = async (req) =>
    req.latest
      ? {
          generation: "g1",
          bound: true,
          page_start: 744,
          next_offset: 872,
          items: Array.from({ length: 128 }, (_, i) => ({
            id: `744:${i}`,
            kind: "assistant",
            text: `Message ${744 + i}`,
          })),
        }
      : {
          generation: "g1",
          bound: true,
          page_start: 1000,
          next_offset: 1000,
          items: [],
        };
  warm.setVisible(true);
  assert.equal(
    preparedFeed.scrollTop,
    preparedFeed.scrollHeight,
    "first visible frame is pinned to the latest message",
  );
  await flush();
  warm.input("draft");
  warm.setVisible(false);
  warm.setVisible(true);
  await flush();
  assert.equal(warm.find("chat-feed"), preparedFeed);
  assert.equal(preparedFeed.children.at(-1), preparedLast);
  assert.equal(warm.find("chat-input").value, "draft");
  const warmAnchor = preparedFeed.children[0],
    warmCount = warm.calls.length;
  preparedFeed.scrollTop = 0;
  preparedFeed.dispatchEvent(new Event("scroll"));
  warm.update();
  await flush();
  assert.equal(
    warm.calls.length,
    warmCount + 1,
    "upward scroll loads one earlier page without a button click",
  );
  assert.equal(warm.calls.at(-1).latest, true);
  assert.equal(warm.calls.at(-1).end_offset, 872);
  assert.equal(warm.entry.items[0].text, "Message 744");
  assert.equal(preparedFeed.children[128], warmAnchor);
  assert.equal(preparedFeed.scrollTop, 12800);
  warm.button("Back to latest").action();
  await flush();
  assert.equal(warm.entry.items[0].text, "Message 872");
  assert.equal(preparedFeed.scrollTop, preparedFeed.scrollHeight);
  warm.setVisible(false);
  now += 5000;
  warm.handler = async () => ({
    generation: "g1",
    bound: true,
    page_start: 9872,
    next_offset: 10000,
    items: [{ id: "9872:0", kind: "assistant", text: "Message 9999" }],
  });
  warm.setVisible(true);
  await flush();
  assert.equal(
    warm.calls.at(-1).latest,
    true,
    "after a long stay in Term, skip the backlog in one tail request",
  );
  assert.equal(warm.entry.items.at(-1).text, "Message 9999");
  const unchangedTail = preparedFeed.children.at(-1);
  warm.setVisible(false);
  now += 5000;
  warm.setVisible(true);
  await flush();
  assert.equal(
    preparedFeed.children.at(-1),
    unchangedTail,
    "an unchanged tail refresh keeps the existing message nodes",
  );
  warm.dispose();
  console.log(
    "PASS first response opens at latest, hidden prefetch survives mode flips, backward paging anchors and explicit latest pins",
  );
  await historyRace();
  await automaticHistory();
  await automaticHistoryFailures();
  await inactiveHistory();
  const tools = fixture();
  await flush();
  await tools.poll({
    generation: "g1",
    bound: true,
    next_offset: 1,
    items: [
      { id: "user", kind: "user", text: "Please review" },
      {
        id: "call",
        kind: "tool_call",
        tool_name: "Read",
        file_path: "/work/a.rs",
        text: "Large tool arguments",
      },
      { id: "result", kind: "tool_result", text: "Long tool output" },
      { id: "answer", kind: "assistant", text: "Here is the answer" },
    ],
  });
  const run = tools.find("chat-tool-run");
  assert(run);
  assert(!run.open);
  assert(!tools.find("chat-tool-call").open);
  assert(!tools.find("chat-tool-result").open);
  assert.equal(tools.find("chat-feed").children.length, 3);
  assert(run.children[0].textContent.includes("Read"));
  run.open = true;
  tools.find("chat-tool-call").open = true;
  await tools.poll({
    generation: "g1",
    bound: true,
    next_offset: 2,
    items: [{ id: "more", kind: "assistant", text: "More context" }],
  });
  assert.equal(tools.find("chat-tool-run"), run);
  assert(run.open);
  assert(tools.find("chat-tool-call").open);
  tools.dispose();
  console.log(
    "PASS message-first chat groups adjacent tools, defaults closed and preserves disclosure on deltas",
  );
  const failedTools = fixture();
  await flush();
  await failedTools.poll({
    generation: "g1",
    bound: true,
    next_offset: 1,
    items: [
      {
        id: "failed-call",
        kind: "tool_call",
        tool_name: "Read",
        text: "Read the file",
      },
      {
        id: "failed-result",
        kind: "tool_result",
        is_error: true,
        text: "Permission denied",
      },
    ],
  });
  const failedRun = failedTools.find("chat-tool-run");
  assert(failedRun);
  assert(!failedRun.open);
  assert(
    failedRun.children[0].textContent.includes("Tool error"),
    "a collapsed tool group must expose failures without opening it",
  );
  assert(failedRun.children[0].textContent.includes("Read"));
  failedTools.dispose();
  console.log("PASS collapsed tool group exposes tool failure in its summary");
  let f = fixture();
  await flush();
  const pending = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? pending.promise
      : { generation: "g1", items: [], next_offset: req.offset, bound: true };
  f.input("first");
  f.submit();
  await flush();
  f.input("second");
  f.submit();
  await flush();
  assert.equal(f.calls.filter((c) => c.action === "prompt").length, 1);
  pending.resolve({ sent: true });
  await flush();
  assert.equal(f.find("chat-input").value, "second");
  assert.equal(f.state.drafts.get(7), "second");
  const next = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? next.promise
      : { generation: "g1", items: [], next_offset: req.offset, bound: true };
  f.submit();
  await flush();
  f.remount();
  await flush();
  f.input("third");
  f.submit();
  await flush();
  assert.equal(f.calls.filter((c) => c.action === "prompt").length, 2);
  next.resolve({ sent: true });
  await flush();
  assert.equal(f.state.drafts.get(7), "third");
  assert.equal(f.find("chat-input").value, "third");
  f.dispose();
  console.log(
    "PASS TR-A02 delayed success preserves edits, remount draft and duplicate-submit lock",
  );
  // The current mount must consume an older mount's successful completion, without another edit.
  f = fixture();
  await flush();
  const remounted = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? remounted.promise
      : { generation: "g1", items: [], next_offset: req.offset, bound: true };
  f.input("send once");
  f.submit();
  await flush();
  f.remount();
  await flush();
  assert.equal(f.find("chat-input").value, "send once");
  remounted.resolve({ sent: true });
  await flush();
  assert.equal(f.find("chat-input").value, "");
  assert.equal(f.state.drafts.has(7), false);
  f.submit();
  await flush();
  assert.equal(f.calls.filter((c) => c.action === "prompt").length, 1);
  f.dispose();
  // Editing back to exactly the same text is still a newer draft and must survive completion.
  f = fixture();
  await flush();
  const edited = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? edited.promise
      : { generation: "g1", items: [], next_offset: req.offset, bound: true };
  f.input("same text");
  f.submit();
  await flush();
  f.remount();
  await flush();
  f.input("new edit");
  f.input("same text");
  edited.resolve({ sent: true });
  await flush();
  assert.equal(f.find("chat-input").value, "same text");
  assert.equal(f.state.drafts.get(7), "same text");
  f.dispose();
  console.log(
    "PASS P1 composer completion clears unchanged remount, preserves even identical-text newer edits, no duplicate send",
  );
  f = fixture();
  await flush();
  const q = {
      id: "q",
      kind: "question",
      text: "Choose",
      multi_select: true,
      options: [{ label: "Safe" }, { label: "Fast" }],
    },
    reason = { id: "r", kind: "reasoning", text: "Thought" },
    permission = { id: "p", kind: "permission", text: "Approve?" };
  await f.poll({
    generation: "g1",
    items: [q, reason, permission],
    next_offset: 10,
    bound: true,
  });
  const card = f.find("chat-card chat-question"),
    custom = card.descendants().find((n) => n.tag === "input"),
    details = f.find("chat-reasoning");
  f.button("Safe").action();
  custom.value = "keep notes";
  custom.dispatchEvent(new Event("input"));
  details.open = true;
  assert.equal(f.button("Copy answer").hidden, false);
  f.button("Copy answer").action();
  await flush();
  assert.equal(f.copied.at(-1), "Safe, keep notes");
  await f.poll({
    generation: "g1",
    items: [{ id: "a", kind: "assistant", text: "delta" }],
    next_offset: 20,
    bound: true,
  });
  assert.equal(f.find("chat-card chat-question"), card);
  assert.equal(custom.value, "keep notes");
  assert.equal(details.open, true);
  assert.equal(f.button("Safe").attrs["aria-pressed"], "true");
  f.button("Confirm in Term").action();
  assert.equal(f.term, 1);
  assert(
    !f.calls.some((c) => ["approve", "answer_question"].includes(c.action)),
  );
  await f.poll({
    generation: "g2",
    items: [],
    next_offset: 0,
    reset: true,
    bound: true,
  });
  assert.equal(
    f.root.descendants().filter((n) => n.tag === "article").length,
    0,
  );
  assert.equal(f.entry.items.length, 0);
  assert.equal(f.entry.bound, true);
  assert.equal(f.entry.agent, "");
  assert.equal(
    f.find("chat-empty").children[0].textContent,
    "Start a conversation",
  );
  f.dispose();
  console.log(
    "PASS TR-A01/06/07 historical cards direct to Term, Copy answer copies draft, DOM notes/selection/expansion survive, successful empty replacement clears",
  );
  f = fixture();
  await flush();
  f.handler = async () => {
    throw new Error("history unavailable");
  };
  f.update();
  await flush();
  const status = f.root
    .descendants()
    .find(
      (n) =>
        n.attrs.role === "status" &&
        n.children.some((c) => c.textContent === "Retry"),
    );
  assert.equal(status.hidden, false);
  assert.match(status.children[0].textContent, /history unavailable/);
  assert.match(status.children[0].textContent, /\d\d:\d\d:\d\d/);
  f.handler = async (req) => ({
    generation: "g1",
    items: [],
    next_offset: req.offset,
    bound: true,
  });
  f.button("Retry").action();
  await flush();
  assert.equal(status.hidden, true);
  f.dispose();
  console.log(
    "PASS TR-A08 poll failure includes 24h HH:MM:SS last update, explicit retry recovers",
  );
  // Photo picker uses ctx.el, so intercept just its file input to dispatch the actual change callback.
  f = fixture();
  await flush();
  let picker;
  const oldEl = f.ctx.el;
  f.ctx.el = (...args) => {
    const n = oldEl(...args);
    if (n.tag === "input") picker = n;
    return n;
  };
  // mount captures el, so remount after installing instrumentation.
  f.remount();
  await flush();
  const photo1 = deferred();
  f.prepare = () => photo1.promise;
  f.button("Photo").action();
  picker.files = [{ name: "first.jpg" }];
  picker.dispatchEvent(new Event("change"));
  await flush();
  assert.equal(f.entry.photo.status, "pending");
  f.input("with image");
  f.submit();
  await flush();
  assert(!f.calls.some((c) => c.action === "prompt"));
  photo1.reject(new Error("compression failed"));
  await flush();
  assert.equal(f.entry.photo.status, "error");
  assert(f.button("Retry photo"));
  f.submit();
  await flush();
  assert(!f.calls.some((c) => c.action === "prompt"));
  f.prepare = async () => ({ mime: "image/jpeg", data: "image1" });
  f.button("Retry photo").action();
  await flush();
  assert.equal(f.entry.photo.status, "ready");
  const thumb = f.find("chat-photo-thumb");
  assert(thumb);
  assert.equal(thumb.attrs.src, "data:image/jpeg;base64,image1");
  f.submit();
  await flush();
  const sent = f.calls.find((c) => c.action === "prompt");
  assert.equal(sent.text, "with image");
  assert.equal(sent.photo.data, "image1");
  assert.equal(f.entry.photo, null);
  assert(!f.calls.some((c) => c.action === "photo"));
  assert.equal(f.find("chat-input").value, "");
  const stale = deferred();
  f.prepare = () => stale.promise;
  f.button("Photo").action();
  picker.files = [{ name: "stale.jpg" }];
  picker.dispatchEvent(new Event("change"));
  await flush();
  f.button("Remove photo").action();
  stale.resolve({ mime: "image/jpeg", data: "stale" });
  await flush();
  assert.equal(f.entry.photo, null);
  // Submit rejection must retain both draft and prepared photo for retry.
  f.prepare = async () => ({ mime: "image/jpeg", data: "retry-image" });
  f.button("Photo").action();
  picker.files = [{ name: "retry.jpg" }];
  picker.dispatchEvent(new Event("change"));
  await flush();
  f.handler = async (req) => {
    if (req.action === "prompt") throw new Error("send failed");
    return { generation: "g1", items: [], next_offset: req.offset };
  };
  f.input("retry draft");
  f.submit();
  await flush();
  assert.equal(f.state.drafts.get(7), "retry draft");
  assert.equal(f.entry.photo.prepared.data, "retry-image");
  f.dispose();
  console.log(
    "PASS TR-A11 preparation pending/error/retry, no early send, one ordered request, remove race, failed-send retention",
  );
  f = fixture();
  await flush();
  await f.poll({
    generation: "g1",
    items: [
      {
        id: "tool",
        kind: "tool_call",
        file_path: "/work/src/a.rs",
        text: "read",
      },
    ],
    next_offset: 1,
    bound: true,
  });
  f.handler = async (req) =>
    req.action === "git" ? { patch: "", diff_status: "no_current_diff" } : {};
  f.button("Current diff").action();
  await flush();
  assert.match(
    f.sheets[0].body.children[0].textContent,
    /historical tool record/,
  );
  assert.equal(f.calls.at(-1).path, "src/a.rs");
  f.dispose();
  console.log(
    "PASS TR-A12 current empty diff has historical-context explanation",
  );
  f = fixture();
  await flush();
  const items = Array.from({ length: 1050 }, (_, i) => ({
    id: `row${i}`,
    kind: "assistant",
    text: `Message ${i}`,
  }));
  f.handler = async (req) => {
    const end = Math.min(
      items.length,
      req.offset + 128,
      req.end_offset ?? Infinity,
    );
    return {
      generation: "g1",
      items: items.slice(req.offset, end),
      next_offset: end,
      bound: true,
    };
  };
  for (let i = 0; i < 9; i++) {
    f.update();
    await flush();
  }
  assert.equal(f.entry.items.length, 1000);
  assert.equal(f.entry.items[0].id, "row50");
  assert.equal(f.button("Load earlier messages").hidden, false);
  const feed = f.find("chat-feed"),
    anchor = feed.children[0];
  feed.scrollTop = 0;
  const oldTop = anchor.offsetTop;
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.entry.items.length, 1000);
  assert.equal(f.entry.items[0].id, "row0");
  assert.equal(f.find("chat-feed").children[50], anchor);
  assert.equal(feed.scrollTop, anchor.offsetTop - oldTop);
  assert(f.entry.live);
  const count = f.calls.length;
  f.update();
  await flush();
  assert.equal(f.calls.length, count);
  f.button("Back to latest").action();
  await flush();
  assert.equal(f.entry.items[0].id, "row50");
  assert.equal(f.entry.items.at(-1).id, "row1049");
  assert.equal(f.entry.offset, 1050);
  f.dispose();
  // Repeated previous-page reads must use prior offsets, not merely replay the first clipped page.
  f = fixture();
  await flush();
  const longer = Array.from({ length: 1350 }, (_, i) => ({
    id: `long${i}`,
    kind: "assistant",
    text: `Older ${i}`,
  }));
  const longPage = async (req) => {
    const end = Math.min(
      longer.length,
      req.offset + 128,
      req.end_offset ?? Infinity,
    );
    return {
      generation: "g1",
      items: longer.slice(req.offset, end),
      next_offset: end,
      bound: true,
    };
  };
  f.handler = longPage;
  for (let i = 0; i < 11; i++) {
    f.update();
    await flush();
  }
  assert.equal(f.entry.items[0].id, "long350");
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.entry.items[0].id, "long256");
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.entry.items[0].id, "long128");
  assert.equal(f.calls.at(-1).offset, 128);
  const before = f.entry.items[0];
  f.handler = async () => {
    throw new Error("earlier page failed");
  };
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.entry.items[0], before);
  assert.equal(f.button("Load earlier messages").disabled, false);
  f.handler = longPage;
  f.button("Load earlier messages").action();
  await flush();
  assert.equal(f.entry.items[0].id, "long0");
  assert.equal(f.entry.items.at(-1).id, "long999");
  assert.equal(f.entry.items.length, 1000);
  assert.equal(f.button("Load earlier messages").hidden, true);
  f.dispose();
  console.log(
    "PASS TR-A14 visible 1000 cap, real earlier RPC, stable anchor, history pause and latest restore",
  );
  // Realistic byte offsets and page-local IDs: every append was observed as a one-item page.
  // A later unbounded replay would return 128 items with different IDs for the same records.
  f = fixture();
  await flush();
  const records = Array.from({ length: 1050 }, (_, i) => ({
    kind: "assistant",
    text: i === 48 || i === 49 ? "Identical text" : `Message ${i}`,
  }));
  const offsets = [0];
  for (const record of records)
    offsets.push(
      offsets.at(-1) + Buffer.byteLength(JSON.stringify(record) + "\n"),
    );
  let total = 0;
  f.handler = async (req) => {
    const begin = offsets.indexOf(req.offset);
    assert(begin >= 0, "request starts at a recorded byte boundary");
    const end = Math.min(
      total,
      begin + 128,
      req.end_offset === undefined ? total : offsets.indexOf(req.end_offset),
    );
    assert(end >= begin, "replay end is a recorded byte boundary");
    return {
      generation: "g1",
      items: records
        .slice(begin, end)
        .map((item, i) => ({ ...item, id: `${req.offset}:${i}` })),
      next_offset: offsets[end],
      bound: true,
    };
  };
  for (total = 1; total <= 1050; total++) {
    f.update();
    await flush();
  }
  const smallIds = (start) =>
    Array.from({ length: 1000 }, (_, i) => `${offsets[start + i]}:0`);
  assert.deepEqual(
    Array.from(f.entry.items, (item) => item.id),
    smallIds(50),
  );
  assert.equal(f.entry.items[0]._clipped, undefined);
  const firstCard = f.find("chat-feed").children[0];
  f.find("chat-feed").scrollTop = 0;
  for (let start = 49; start >= 0; start--) {
    f.button("Load earlier messages").action();
    await flush();
    assert.equal(f.calls.at(-1).offset, offsets[start]);
    assert.equal(f.calls.at(-1).end_offset, offsets[start + 1]);
    assert.deepEqual(
      Array.from(f.entry.items, (item) => item.id),
      smallIds(start),
    );
    assert.equal(f.entry.items.length, 1000);
    assert.equal(f.errors.length, 0);
    if (start === 49) {
      assert.equal(f.find("chat-feed").children[1], firstCard);
      assert.equal(f.find("chat-feed").scrollTop, 100);
    }
    if (start === 48) {
      assert.equal(f.entry.items[0].text, "Identical text");
      assert.equal(f.entry.items[1].text, "Identical text");
      assert.notEqual(f.entry.items[0].id, f.entry.items[1].id);
    }
  }
  assert.equal(f.button("Load earlier messages").hidden, true);
  assert.equal(new Set(f.entry.items.map((item) => item.id)).size, 1000);
  f.button("Back to latest").action();
  await flush();
  assert.deepEqual(
    Array.from(f.entry.items, (item) => item.id),
    smallIds(50),
  );
  assert.equal(f.entry.offset, offsets[1050]);
  f.dispose();
  console.log(
    "PASS P1 1050 one-item append pages replay exact byte ends, 50 earlier reads stay unique/chronological, identical text retained",
  );
  f = fixture();
  await flush();
  const previousHost = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? previousHost.promise
      : { generation: "old", items: [], next_offset: 0 };
  f.input("same text");
  f.submit();
  await flush();
  f.state.drafts = new Map([[7, "same text"]]);
  f.state.chatCache = new Map();
  f.remount();
  await flush();
  previousHost.resolve({ sent: true });
  await flush();
  assert.equal(f.state.drafts.get(7), "same text");
  assert.equal(f.find("chat-input").value, "same text");
  f.dispose();
  console.log(
    "PASS late previous-host prompt completion cannot erase the new host draft with an identical session ID/text",
  );
  // Scrolled-up / historical submit immediately exits history, renders optimistic user bubble, pins bottom, and deduplicates on poll.
  f = fixture();
  await flush();
  await f.poll({
    generation: "g1",
    page_start: 100,
    items: [
      { id: "m1", kind: "assistant", text: "Earlier 1" },
      { id: "m2", kind: "assistant", text: "Earlier 2" },
      { id: "m3", kind: "assistant", text: "Latest 3" },
    ],
    next_offset: 300,
    bound: true,
  });
  f.handler = async (req) => ({
    generation: "g1",
    page_start: 0,
    items: [{ id: "m0", kind: "assistant", text: "Oldest 0" }],
    next_offset: 100,
    bound: true,
  });
  const feedEl = f.find("chat-feed");
  feedEl.scrollTop = 0;
  f.button("Load earlier messages").action();
  await flush();
  assert(f.entry.live, "entered historical reading mode");
  feedEl.scrollTop = 0;
  const promptDeferred = deferred(),
    pollDeferred = deferred();
  f.handler = async (req) =>
    req.action === "prompt"
      ? promptDeferred.promise
      : req.action === "chat_poll"
        ? pollDeferred.promise
        : {};
  const inputEl = f.find("chat-input");
  let heightWrites = 0,
    currentHeight = inputEl.style.height;
  Object.defineProperty(inputEl.style, "height", {
    configurable: true,
    get: () => currentHeight,
    set: (v) => {
      heightWrites++;
      currentHeight = v;
    },
  });
  f.input("방금 음성으로 입력한 메시지");
  const writesAfterInput = heightWrites;
  f.update();
  f.update();
  assert.equal(
    heightWrites,
    writesAfterInput,
    "idle background updates do not thrash textarea style.height during dictation",
  );
  f.submit();
  assert.equal(f.entry.live, null, "submit exits history mode immediately");
  const optimistic = feedEl.children.at(-1);
  assert.equal(
    optimistic?.className,
    "chat-card chat-user chat-optimistic",
    "optimistic user bubble mounts immediately at the bottom of feed",
  );
  assert.equal(optimistic.dataset.phase, "sending");
  assert(
    optimistic.descendants().some((n) => n.textContent === "방금 음성으로 입력한 메시지"),
  );
  assert.equal(
    feedEl.scrollTop,
    feedEl.scrollHeight,
    "feed scrolls immediately to the very bottom on submit",
  );
  promptDeferred.resolve({ sent: true });
  await flush();
  assert.equal(inputEl.value, "");
  assert.equal(
    feedEl.children.at(-1)?.dataset.phase,
    "accepted",
    "optimistic user bubble stays visible after composer clears before chat_poll",
  );
  assert.equal(feedEl.scrollTop, feedEl.scrollHeight);
  pollDeferred.resolve({
    generation: "g1",
    page_start: 300,
    items: [
      { id: "u4", kind: "user", text: "방금 음성으로 입력한 메시지" },
      { id: "a5", kind: "assistant", text: "답변입니다" },
    ],
    next_offset: 400,
    bound: true,
  });
  await flush();
  assert(
    !feedEl.children.some((n) => n.className?.includes("chat-optimistic")),
    "optimistic bubble removes cleanly when transcript user item arrives",
  );
  assert.equal(
    feedEl.children.filter((n) => n.className?.includes("chat-user")).length,
    1,
    "exactly one user bubble remains without duplicates",
  );
  assert.equal(feedEl.scrollTop, feedEl.scrollHeight);
  // Native dictation commit without input event survives blur and unmount.
  inputEl.value = "음성 받아쓰기 마지막 문장";
  inputEl.dispatchEvent(new Event("blur"));
  assert.equal(f.state.drafts.get(7), "음성 받아쓰기 마지막 문장");
  f.dispose();
  console.log(
    "PASS scrolled-up submit jumps to bottom with optimistic user bubble, reconciles without duplicate, and protects dictation input",
  );
  await agyStartupAndModelApplyUx();
  await unboundEmptyAndVisibilityTransitions();
}

async function agyStartupAndModelApplyUx() {
  let clock = 100000;
  const Clock = class extends Date {
    static now() {
      return clock;
    }
  };
  const poll1 = deferred(),
    modelsDeferred = deferred(),
    setModelDeferred = deferred();
  let pollCount = 0,
    composerProbes = 0,
    settled = false;
  const f = fixture({
    Date: Clock,
    capabilities: {
      chat_composer: true,
      composer_models: true,
      set_model: true,
      set_effort: true,
    },
    handler: async (req) => {
      if (req.action === "chat_poll") {
        pollCount++;
        if (pollCount === 1) return poll1.promise;
        return {
          generation: "g-agy",
          agent: "agy",
          bound: true,
          items: [],
          next_offset: 0,
        };
      }
      if (req.action === "chat_composer") {
        composerProbes++;
        return settled
          ? {
              agent: "agy",
              supported: true,
              effort_supported: true,
              model: "gemini-4-carbon",
              model_label: "Gemini 4 Carbon",
              effort: "high",
              models: [
                {
                  id: "gemini-4-carbon",
                  label: "Gemini 4 Carbon",
                  efforts: ["low", "medium", "high"],
                },
                {
                  id: "gemini-4-argon",
                  label: "Gemini 4 Argon",
                  efforts: ["low", "medium", "high"],
                },
              ],
            }
          : {
              agent: "agy",
              supported: false,
              effort_supported: false,
              model: null,
              model_label: "Gemini 4 Carbon",
              effort: "high",
              models: [],
            };
      }
      if (req.action === "composer_models") return modelsDeferred.promise;
      if (req.action === "set_model") return setModelDeferred.promise;
      return {};
    },
  });
  const sess = f.state.sessions.get(7);
  sess.agent = "agy";
  f.update();
  await flush();
  // Snapshot upgrades synthetic expected session identity while poll1 is still in flight.
  sess.profile_id = "agy";
  sess.command_context = "ctx-agy";
  f.update();
  poll1.resolve({
    generation: "g-old",
    agent: "agy",
    bound: false,
    items: [],
    next_offset: 0,
  });
  await flush();
  assert.equal(
    pollCount,
    2,
    "scope upgrade during in-flight initial poll reloads immediately without waiting 1.5s",
  );
  assert.equal(
    f.find("chat-empty")?.children[0]?.textContent,
    "Start a conversation",
    "empty state renders as soon as post-upgrade poll finishes",
  );
  const pill = f.find("composer-pill"),
    modelName = f.find("composer-model-name"),
    effortLabel = f.find("composer-model-effort");
  assert.equal(
    pill.hidden,
    false,
    "controllable provider keeps model pill visible while cold catalog settles",
  );
  assert.equal(modelName.textContent, "Gemini 4 Carbon");
  assert.equal(effortLabel.hidden, false);
  assert.equal(effortLabel.textContent, "High");
  assert.equal(effortLabel.dataset.effort, "high");
  const probesBefore = composerProbes;
  clock += 1600;
  f.update();
  await flush();
  assert(
    composerProbes > probesBefore,
    "startup status probes retry after 1.5s instead of locking out for 15s",
  );
  // Cold picker open shows Loading…, settles catalog, and renders model + effort groups + note.
  pill.click();
  const suggestions = f.find("chat-composer-suggestions");
  assert.equal(suggestions.hidden, false);
  assert.equal(suggestions.dataset.kind, "models");
  assert(
    suggestions.descendants().some((n) => n.textContent === "Loading…"),
    "cold model picker displays Loading… immediately",
  );
  settled = true;
  modelsDeferred.resolve({
    agent: "agy",
    supported: true,
    current: "gemini-4-carbon",
    note: "Antigravity also saves this as the default for new sessions.",
    items: [
      {
        id: "gemini-4-carbon",
        label: "Gemini 4 Carbon",
        efforts: ["low", "medium", "high"],
      },
      {
        id: "gemini-4-argon",
        label: "Gemini 4 Argon",
        efforts: ["low", "medium", "high"],
      },
    ],
  });
  await flush();
  assert(
    suggestions
      .descendants()
      .some(
        (n) =>
          n.textContent ===
          "Antigravity also saves this as the default for new sessions.",
      ),
    "provider note is surfaced in picker header",
  );
  const options = suggestions
    .descendants()
    .filter((n) => n.attrs?.role === "option");
  const argonOption = options.find((n) =>
    n.descendants().some((c) => c.textContent === "Gemini 4 Argon"),
  );
  const highOption = options.find((n) =>
    n.descendants().some((c) => c.textContent === "High"),
  );
  assert(argonOption, "Gemini 4 Argon chip rendered");
  assert(highOption, "High effort chip rendered");
  assert.equal(highOption.attrs["aria-current"], "true");
  argonOption.click();
  await flush();
  assert.equal(pill.disabled, true);
  f.update();
  assert.equal(
    pill.disabled,
    true,
    "background update does not re-enable model pill while set_model is in flight",
  );
  assert.equal(
    modelName.textContent,
    "Gemini 4 Carbon",
    "unconfirmed target model is never optimistically shown on composer pill",
  );
  assert(
    suggestions.descendants().some((n) => n.textContent === "Applying…"),
    "in-flight model change shows Applying… feedback",
  );
  setModelDeferred.resolve({
    applied: true,
    supported: true,
    model: "gemini-4-argon",
    label: "Gemini 4 Argon",
    effort: "high",
  });
  await flush();
  assert.equal(suggestions.hidden, true);
  assert.equal(suggestions.dataset.kind, undefined);
  assert.equal(modelName.textContent, "Gemini 4 Argon");
  assert.equal(
    effortLabel.hidden,
    false,
    "confirmed effort label remains visible after model switch",
  );
  assert.equal(effortLabel.textContent, "High");
  assert.equal(effortLabel.dataset.effort, "high");
  assert.equal(pill.dataset.applied, "true");
  f.dispose();
  // Jump-to-latest button stays hidden while following live items and reveals when scrolled up
  {
    const page = (from, to) => ({
      ...historyPage(from, to),
      items: Array.from({ length: to - from }, (_, i) => ({
        id: "j" + (from + i),
        kind: "assistant",
        text: "Message " + (from + i),
      })),
    });
    const jumpFx = fixture({
      capabilities: { chat_tail: true },
      handler: async (q) =>
        q.end_offset !== undefined
          ? page(Math.max(0, q.end_offset - 30), q.end_offset)
          : q.latest
            ? page(60, 90)
            : page(q.offset, q.offset),
    });
    await flush();
    const feed = jumpFx.find("chat-feed"),
      jumpBtn = jumpFx.find("chat-jump-bottom");
    assert.equal(jumpBtn.hidden, true, "jump button hides while following");
    scroll(feed, 800);
    scroll(feed, 400);
    await flush();
    assert.equal(jumpBtn.hidden, false, "jump button appears after scrolling up");
    jumpBtn.action();
    await flush();
    assert.equal(jumpBtn.hidden, true, "clicking jump button returns to latest");
    jumpFx.dispose();
  }
  console.log(
    "PASS agy startup reload, 1.5s boot probe, cold picker Loading…/note, Applying… lock, and confirmed effort retention",
  );

  // OWE-1349: Working turns switch Send to Stop, send a single interrupt RPC, ignore double-taps, lock duplicate taps, and preserve drafts.
  {
    const stopChat = fixture({
      capabilities: { chat_interrupt: true },
      handler: async (req) => {
        if (req.action === "chat_poll")
          return {
            generation: "g-stop",
            agent: "claude",
            bound: true,
            items: [],
            next_offset: req.offset,
          };
        if (req.action === "interrupt") return { interrupted: true };
        return { sent: true };
      },
    });
    await flush();
    const sendBtn = stopChat.find("primary composer-send");
    assert.equal(sendBtn.dataset.mode, "send");
    assert.equal(sendBtn.type, "submit");
    assert.equal(sendBtn.attrs["aria-label"], "Send");
    assert.equal(sendBtn.textContent, "Send");

    stopChat.input("작성 중인 모바일 초안");
    stopChat.state.sessions.set(7, {
      id: 7,
      state: "Working",
      state_since_unix_ns: "1001",
      cwd: "/work",
    });
    stopChat.update();
    await flush();

    assert.equal(sendBtn.dataset.mode, "stop");
    assert.equal(sendBtn.type, "button");
    assert.equal(sendBtn.attrs["aria-label"], "Stop");
    assert.equal(sendBtn.textContent, "Stop");
    assert.equal(sendBtn.disabled, false);

    // Submitting form while Working must be blocked and preserve the draft.
    stopChat.submit();
    await flush();
    assert(!stopChat.calls.some((c) => c.action === "prompt"));
    assert.equal(stopChat.find("chat-input").value, "작성 중인 모바일 초안");
    assert.equal(stopChat.state.drafts.get(7), "작성 중인 모바일 초안");

    // Double-tap (event.detail > 1) must be ignored before latching.
    const dbl = new Event("click");
    dbl.detail = 2;
    sendBtn.dispatchEvent(dbl);
    await flush();
    assert.equal(
      stopChat.calls.filter((c) => c.action === "interrupt").length,
      0,
    );
    assert.equal(sendBtn.disabled, false);

    // First Stop tap sends interrupt RPC once and locks duplicate taps for this Working turn.
    sendBtn.dispatchEvent(new Event("click"));
    await flush();
    assert.equal(
      stopChat.calls.filter((c) => c.action === "interrupt").length,
      1,
    );
    assert.deepEqual(
      { ...stopChat.calls.find((c) => c.action === "interrupt") },
      { action: "interrupt", session: 7 },
    );
    assert.equal(sendBtn.disabled, true);
    assert.equal(stopChat.find("chat-input").value, "작성 중인 모바일 초안");
    assert.equal(stopChat.state.drafts.get(7), "작성 중인 모바일 초안");

    // Duplicate taps while Working (including after remount) must NOT send a second interrupt.
    sendBtn.dispatchEvent(new Event("click"));
    await flush();
    stopChat.remount();
    await flush();
    const remountedBtn = stopChat.find("primary composer-send");
    assert.equal(remountedBtn.dataset.mode, "stop");
    assert.equal(remountedBtn.disabled, true);
    remountedBtn.dispatchEvent(new Event("click"));
    await flush();
    assert.equal(
      stopChat.calls.filter((c) => c.action === "interrupt").length,
      1,
    );

    // Leaving Working clears the stop latch and restores Send with the draft intact.
    stopChat.state.sessions.set(7, {
      id: 7,
      state: "Waiting",
      state_since_unix_ns: "2002",
      cwd: "/work",
    });
    stopChat.update();
    await flush();
    assert.equal(remountedBtn.dataset.mode, "send");
    assert.equal(remountedBtn.type, "submit");
    assert.equal(remountedBtn.attrs["aria-label"], "Send");
    assert.equal(remountedBtn.disabled, false);
    assert.equal(stopChat.find("chat-input").value, "작성 중인 모바일 초안");

    // Entering a new Working turn re-enables Stop for one fresh interrupt.
    stopChat.state.sessions.set(7, {
      id: 7,
      state: "Working",
      state_since_unix_ns: "3003",
      cwd: "/work",
    });
    stopChat.update();
    await flush();
    assert.equal(remountedBtn.dataset.mode, "stop");
    assert.equal(remountedBtn.disabled, false);
    remountedBtn.dispatchEvent(new Event("click"));
    await flush();
    assert.equal(
      stopChat.calls.filter((c) => c.action === "interrupt").length,
      2,
    );
    stopChat.dispose();

    // Korean i18n check for Stop label.
    const koChat = fixture({
      lang: "ko",
      capabilities: { chat_interrupt: true },
    });
    koChat.state.sessions.set(7, {
      id: 7,
      state: "Working",
      state_since_unix_ns: "4004",
      cwd: "/work",
    });
    koChat.update();
    await flush();
    assert.equal(
      koChat.find("primary composer-send").attrs["aria-label"],
      "정지",
    );
    koChat.dispose();
  }
  console.log(
    "PASS working stop button single interrupt, double-tap guard, duplicate tap latch, draft retention, and next-turn reset",
  );
}
async function unboundEmptyAndVisibilityTransitions() {
  // 1) Unbound / empty Windows Claude Code sessions must always show .chat-view and an active .chat-composer
  // and must never raise a spurious "Transcript changed" error when an empty transcript stays unbound.
  let pollSeq = 0;
  const unbound = fixture({
    visible: true,
    capabilities: { chat_tail: true, chat_composer: true, composer_models: true },
    handler: async (req) => {
      if (req.action === "chat_composer") return { supported: false };
      if (req.action === "chat_poll") {
        pollSeq++;
        if (pollSeq === 1)
          return {
            generation: "g0",
            agent: "claude",
            bound: true,
            items: [],
            next_offset: 0,
            page_start: 0,
            has_more: false,
          };
        return {
          generation: "",
          agent: "claude",
          bound: false,
          items: [],
          next_offset: 0,
          page_start: 0,
          has_more: false,
        };
      }
      return { sent: true };
    },
  });
  await flush();
  assert.equal(pollSeq, 1);
  assert(unbound.find("chat-view"));
  assert(unbound.find("chat-feed"));
  assert(unbound.find("chat-composer"));
  assert(!unbound.find("chat-composer").hidden);
  assert.equal(unbound.find("chat-input").disabled, false);
  assert.equal(unbound.find("primary composer-send").disabled, false);
  // Second poll transitions from empty-bound to unbound: must NOT trigger "Back to latest" or poll error.
  for (const fn of [...unbound.timers.values()]) fn();
  await flush();
  const statusEl = unbound.root.descendants().find((n) => n.attrs?.role === "status");
  assert.equal(
    statusEl.hidden,
    true,
    "empty unbound transcript must not display Transcript changed poll error",
  );
  assert.equal(
    unbound.button("Back to latest").hidden,
    true,
    "empty unbound transcript must not show Back to latest recovery button",
  );
  assert(unbound.find("chat-empty"));
  unbound.dispose();

  // 2) Hidden -> visible transition while a hidden poll is still in flight must queue a visible follow-up poll
  // and re-measure .chat-input height once visible.
  const firstPoll = deferred();
  let mode = "hidden";
  const switched = fixture({
    visible: false,
    capabilities: { chat_tail: true, chat_composer: true },
    handler: async (req) => {
      if (req.action !== "chat_poll") return { supported: true, agent: "claude" };
      if (mode === "hidden") return firstPoll.promise;
      return {
        generation: "g-win",
        agent: "claude",
        bound: true,
        items: [{ id: "m1", kind: "assistant", text: "Visible after switch" }],
        next_offset: 64,
        page_start: 0,
        has_more: false,
      };
    },
  });
  let isShown = false;
  const input = switched.find("chat-input");
  Object.defineProperty(input, "clientWidth", {
    get: () => (isShown ? 320 : 0),
  });
  Object.defineProperty(input, "scrollHeight", {
    get: () => (isShown ? 88 : 28),
  });
  switched.input("line1\nline2\nline3");
  await flush();
  assert.equal(input.style.height, "28px");

  // User taps Chat while hidden poll is still inFlight.
  isShown = true;
  mode = "visible";
  switched.setVisible(true);
  assert.equal(
    input.style.height,
    "88px",
    "visibilityChanged must resize .chat-input when becoming visible",
  );
  firstPoll.resolve({
    generation: "",
    agent: "claude",
    bound: false,
    items: [],
    next_offset: 0,
    page_start: 0,
    has_more: false,
  });
  await flush();
  assert(
    switched.find("chat-feed").textContent.includes("Visible after switch"),
    "in-flight hidden poll must not drop visible refresh poll on setVisible(true)",
  );
  switched.dispose();

  // 3) Live question disables model button even when syncAgent runs after composer status load.
  const qChat = fixture({
    visible: true,
    capabilities: { chat_tail: true, chat_composer: true, composer_models: true },
    getQuestion: () => ({
      token: "q-tok",
      prompt: "Pick?",
      options: [{ label: "Yes" }],
    }),
    handler: async (req) =>
      req.action === "chat_composer"
        ? { supported: true, agent: "claude", model: "opus", model_label: "Opus" }
        : {
            generation: "gq",
            agent: "claude",
            bound: true,
            items: [],
            next_offset: 0,
            page_start: 0,
            has_more: false,
          },
  });
  qChat.setQuestion({
    token: "q-tok",
    identity: "q1",
    prompt: "Pick?",
    enabled: true,
    options: [{ label: "Yes" }],
  });
  await flush();
  assert.equal(
    qChat.find("composer-pill").disabled,
    true,
    "modelBtn must remain disabled while liveQuestion is active",
  );
  qChat.dispose();
  console.log(
    "PASS unbound empty composer stays usable without error, hidden->visible poll queue & input resize, and modelBtn disabled on liveQuestion",
  );

  // 4) MkKeyboard.keepFocus covers attachment/leading/contextMeter without blocking Send button focus transfer (IME commit).
  const kept = [];
  const imeFx = fixture({
    MkKeyboard: {
      keepFocus(container) {
        kept.push(container);
      },
    },
  });
  await flush();
  const hasNode = (rootNode, target) =>
    rootNode === target || rootNode.descendants().includes(target);
  const sendNode = imeFx.find("primary composer-send");
  const modelNode = imeFx.find("composer-pill");
  const attachNode = imeFx.find("icon-button composer-icon");
  const ctxNode = imeFx.find("composer-context");
  assert(
    !kept.some((c) => hasNode(c, sendNode)),
    "sendBtn must stay outside keepFocus so tapping Send commits IME composition",
  );
  assert(kept.some((c) => hasNode(c, modelNode)));
  assert(kept.some((c) => hasNode(c, attachNode)));
  assert(kept.some((c) => hasNode(c, ctxNode)));
  imeFx.dispose();
  console.log(
    "PASS MkKeyboard.keepFocus protects composer accessories without capturing Send",
  );

  // 5) Phase 1B: Markdown overhaul, zero-dep highlighting, colored Diff sheet, Tab/Enter submit, <dialog> modal model sheet, skeleton & streaming dots.
  {
    let draftSaved = 0;
    const p1b = fixture({
      capabilities: {
        chat_tail: true,
        chat_composer: true,
        composer_models: true,
        command_catalog: true,
      },
      handler: async (req) => {
        if (req.action === "command_catalog")
          return { items: [{ name: "compact", text: "/compact", kind: "Command" }] };
        if (req.action === "composer_models")
          return {
            supported: true,
            current: "m1",
            items: [{ id: "m1", label: "Model 1", efforts: ["low", "high"] }],
          };
        if (req.action === "git")
          return { patch: "@@ -1 +1 @@\n-old\n+new" };
        if (req.action === "chat_poll")
          return {
            generation: "g-p1b",
            agent: "claude",
            bound: true,
            next_offset: 10,
            items: [
              { id: "unk", kind: "unknown_future_kind", text: "ignored" },
              {
                id: "md2",
                kind: "assistant",
                text:
                  "Paragraph line 1\nParagraph line 2\n\n#### H4\n##### H5\n###### H6\n---\n- [ ] todo item\n- [x] **done** ~~old~~ _ital_ snake_case\n  - nested\n| A | B |\n| --- | --- |\n| 1 | 2 |\n```rust\nfn main() { let x = 42; }\n```",
              },
              {
                id: "t1",
                kind: "tool_call",
                tool_name: "Edit",
                file_path: "/work/a.rs",
                text: "edited",
              },
            ],
          };
        return { sent: true };
      },
    });
    p1b.ctx.saveDrafts = () => {
      draftSaved++;
    };
    const skel = p1b.find("chat-skeleton");
    assert.equal(skel.hidden, false, "skeleton shows before first poll settles");
    await flush();
    assert.equal(skel.hidden, true, "skeleton hides once items load");

    const feedNodes = p1b.find("chat-feed").children;
    assert(
      !feedNodes.some((n) => n.className?.includes("unknown_future_kind")),
      "unknown item kinds are skipped cleanly",
    );
    const md = p1b.find("chat-md"),
      mdDesc = md.descendants();
    assert(mdDesc.some((n) => n.tag === "p" && n.textContent === "Paragraph line 1 Paragraph line 2"));
    assert(mdDesc.some((n) => n.tag === "h5" && n.textContent === "H4"));
    assert(mdDesc.some((n) => n.tag === "h6" && n.textContent === "H5"));
    assert(mdDesc.some((n) => n.tag === "hr" && n.className === "chat-hr"));
    assert(mdDesc.some((n) => n.tag === "del" && n.textContent === "old"));
    assert(mdDesc.some((n) => n.tag === "em" && n.textContent === "ital"));
    assert(mdDesc.some((n) => n.className === "chat-task" && n.dataset.checked === "true"));
    assert(mdDesc.some((n) => n.className === "chat-task" && n.dataset.checked === "false"));
    assert(mdDesc.some((n) => n.className === "chat-table-wrap"), "tables use chat-table-wrap not chat-code");
    const codeNode = mdDesc.find((n) => n.tag === "code" && n.textContent.includes("fn main()"));
    assert.equal(codeNode.textContent, "fn main() { let x = 42; }");
    assert(codeNode.descendants().some((n) => n.className === "tok-kw" && n.textContent === "fn"));
    assert(codeNode.descendants().some((n) => n.className === "tok-num" && n.textContent === "42"));

    // Diff sheet uses colored diff-added/diff-deleted/diff-hunk lines
    p1b.button("Current diff").action();
    await flush();
    const sheetPre = p1b.sheets.at(-1).body.children[0];
    assert(sheetPre.descendants().some((n) => n.className?.includes("diff-hunk")));
    assert(sheetPre.descendants().some((n) => n.className?.includes("diff-deleted")));
    assert(sheetPre.descendants().some((n) => n.className?.includes("diff-added")));

    // Tab accepts slash autocomplete and Enter submits when suggestions closed
    p1b.input("/");
    await flush();
    const tabEv = new Event("keydown", { cancelable: true });
    tabEv.key = "Tab";
    p1b.find("chat-input").dispatchEvent(tabEv);
    assert(tabEv.defaultPrevented);
    assert.equal(p1b.find("chat-input").value, "/compact ");
    assert(draftSaved > 0);

    // Streaming dots toggle on Working state
    const dots = p1b.find("chat-streaming-dots");
    assert.equal(dots.hidden, true);
    p1b.state.sessions.get(7).state = "Working";
    p1b.update();
    assert.equal(dots.hidden, false);
    p1b.state.sessions.get(7).state = "Waiting";
    p1b.update();
    assert.equal(dots.hidden, true);

    const enterEv = new Event("keydown", { cancelable: true });
    enterEv.key = "Enter";
    p1b.find("chat-input").dispatchEvent(enterEv);
    await flush();
    assert(enterEv.defaultPrevented);
    assert(p1b.calls.some((c) => c.action === "prompt" && c.text === "/compact "));

    // Model picker renders inside modal <dialog class="chat-model-sheet">
    let modalShown = false;
    const dlg = p1b.find("chat-composer-box").descendants().find((n) => n.tag === "dialog");
    p1b.buttons.find((b) => b.className === "composer-pill").action();
    await flush();
    const mountedDlg = p1b.find("chat-composer-suggestions").descendants().find((n) => n.tag === "dialog");
    assert(mountedDlg && mountedDlg.className === "chat-model-sheet");
    mountedDlg.showModal = () => { modalShown = true; mountedDlg.open = true; };
    assert.equal(dlg, undefined);
    assert.equal(mountedDlg.attrs["aria-modal"], "true");

    p1b.dispose();
    console.log("PASS Phase 1B: Markdown, highlighting, diff sheet, Tab/Enter submit, modal <dialog>, skeleton and streaming dots");
  }

  // Phase 2B: Queue/Steer on Working, 2s Stop cooldown, inline diffs, unread divider, rate-limit banner, live_prompt_reply cards.
  {
    let livePromptPayload = {
      kind: "numbered_menu",
      prompt: "Proceed with this plan?",
      options: [
        { number: 1, label: "Yes, approve plan", selected: true, checked: false },
        { number: 2, label: "No, keep planning", selected: false, checked: false },
      ],
      cursor: 0,
      checked: [],
      is_plan_approval: true,
      allows_other: false,
    };
    let rateLimited = true;
    const routes = [];
    let pollStep = 0;
    let queuedPollData = null;
    const p2b = fixture({
      lang: "en",
      capabilities: {
        chat_tail: true,
        chat_interrupt: true,
        chat_composer: true,
        composer_models: true,
        prompt_steer: true,
        live_prompt_reply: true,
      },
      handler: async (req) => {
        if (req.action === "chat_composer") {
          return {
            agent: "claude",
            supported: true,
            model: "claude-opus-5-5",
            rate_limited: rateLimited,
            live_prompt: livePromptPayload,
          };
        }
        if (req.action === "chat_poll") {
          if (queuedPollData !== null) {
            const d = queuedPollData;
            queuedPollData = null;
            return d;
          }
          pollStep++;
          if (pollStep === 1) {
            return {
              generation: "g-2b",
              agent: "claude",
              bound: true,
              page_start: 0,
              next_offset: 100,
              items: [
                {
                  id: "m1",
                  kind: "user",
                  text: "implement feature",
                },
                {
                  id: "m2",
                  kind: "tool_call",
                  tool_name: "Edit",
                  file_path: "src/lib.rs",
                  text: "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
                },
                {
                  id: "m3",
                  kind: "permission",
                  tool_name: "ExitPlanMode",
                  text: "Proceed with this plan?",
                },
              ],
            };
          }
          if (pollStep === 2) {
            return {
              generation: "g-2b",
              agent: "claude",
              bound: true,
              page_start: 100,
              next_offset: 200,
              items: [
                {
                  id: "m4",
                  kind: "assistant",
                  text: "New unread reply",
                },
              ],
            };
          }
          return {
            generation: "g-2b",
            agent: "claude",
            bound: true,
            page_start: 200,
            next_offset: 200,
            items: [],
          };
        }
        if (req.action === "interrupt") return { interrupted: true };
        return { sent: true };
      },
    });
    p2b.ctx.go = (path) => routes.push(path);
    await flush();

    // WorkRun diff summary
    const diffSummary = p2b.find("chat-run-diff-summary");
    assert(diffSummary.textContent.includes("+1 -1"), "WorkRun header shows +1 -1 diff stats");

    // Rate-limit banner navigates to /accounts on Switch account click
    const rlBanner = p2b.find("chat-rate-limit-banner");
    assert.equal(rlBanner.hidden, false);
    p2b.button("Switch account").action();
    await flush();
    assert.equal(routes.at(-1), "/accounts");

    // ExitPlanMode badge and numbered permission choice dispatch live_prompt_reply
    const planBadge = p2b.find("chat-plan-badge");
    assert.equal(planBadge.textContent, "Plan approval");
    p2b.button("Yes, approve plan").action();
    await flush();
    assert(
      p2b.calls.some(
        (c) =>
          c.action === "live_prompt_reply" &&
          c.choice === 0 &&
          c.expected_prompt === "Proceed with this plan?",
      ),
    );

    // Queue + Steer + 2s Stop cooldown while Working
    p2b.state.sessions.set(7, {
      id: 7,
      state: "Working",
      state_since_unix_ns: "501",
      cwd: "/work",
    });
    p2b.update();
    await flush();
    p2b.input("follow-up task");
    const qBtn = p2b.button("Queue"),
      sBtn = p2b.button("Steer"),
      sendBtn = p2b.find("primary composer-send");
    assert.equal(sendBtn.dataset.mode, "stop");
    assert.equal(qBtn.hidden, false);
    assert.equal(sBtn.hidden, false);

    qBtn.action();
    await flush();
    assert.equal(p2b.find("chat-input").value, "");
    assert.equal(p2b.find("chat-queued-strip").hidden, false);
    assert.equal(p2b.find("chat-queued-strip").children.length, 1);

    // Cmd+Enter while Working steers immediately without touching queue
    p2b.input("steer now");
    const cmdEnter = new Event("keydown", { cancelable: true });
    cmdEnter.key = "Enter";
    cmdEnter.metaKey = true;
    p2b.find("chat-input").dispatchEvent(cmdEnter);
    await flush();
    assert(
      p2b.calls.some(
        (c) => c.action === "prompt" && c.text === "steer now" && c.mode === "steer",
      ),
    );

    // Clicking Stop starts cooldown and disables Stop
    sendBtn.dispatchEvent(new Event("click"));
    await flush();
    assert.equal(sendBtn.disabled, true);

    // Leaving Working for Done auto-flushes queued item FIFO
    p2b.state.sessions.set(7, {
      id: 7,
      state: "Done",
      state_since_unix_ns: "502",
      cwd: "/work",
    });
    p2b.update();
    await flush();
    assert(
      p2b.calls.some((c) => c.action === "prompt" && c.text === "follow-up task" && !c.mode),
    );
    assert.equal(p2b.find("chat-queued-strip").hidden, true);

    // Inline UnreadDivider appears only when new items arrive while scrolled up (!following)
    const feed = p2b.find("chat-feed");
    feed.clientHeight = 100;
    Object.defineProperty(feed, "scrollHeight", { value: 1000, configurable: true });
    feed.scrollTop = 200;
    feed.dispatchEvent(new Event("scroll"));
    feed.scrollTop = 40;
    feed.dispatchEvent(new Event("scroll"));
    delete feed.scrollHeight;
    queuedPollData = {
      generation: "g-2b",
      agent: "claude",
      bound: true,
      page_start: 120,
      next_offset: 180,
      items: [{ id: "m-unread", kind: "assistant", text: "New unread reply" }],
    };
    p2b.update();
    await flush();
    assert(
      feed.descendants().some((n) => n.className === "chat-unread-divider"),
      "unread divider appears before new items received while scrolled up",
    );
    p2b.find("chat-jump-bottom").action();
    await flush();
    assert(!feed.descendants().some((n) => n.className === "chat-unread-divider"));

    // Yes/No live permission prompt dispatches yes_no: true on Approve
    livePromptPayload = {
      kind: "yes_no",
      prompt: "Deploy to production? (y/n)",
    };
    queuedPollData = {
      generation: "g-2b",
      agent: "claude",
      bound: true,
      page_start: 150,
      next_offset: 200,
      items: [
        {
          id: "perm-yn",
          kind: "permission",
          tool_name: "Bash",
          text: "Deploy to production? (y/n)",
        },
      ],
    };
    p2b.update();
    await flush();
    p2b.button("Approve").action();
    await flush();
    assert(
      p2b.calls.some(
        (c) =>
          c.action === "live_prompt_reply" &&
          c.yes === true &&
          c.expected_prompt === "Deploy to production? (y/n)",
      ),
    );

    // Without live_prompt_reply capability, permission falls back to historical Confirm in Term card
    const legacy = fixture({
      capabilities: { chat_tail: true, chat_composer: true },
      handler: async (req) => {
        if (req.action === "chat_composer") {
          return {
            agent: "claude",
            supported: true,
            live_prompt: { kind: "yes_no", prompt: "Allow?" },
          };
        }
        return {
          generation: "g-legacy",
          agent: "claude",
          bound: true,
          page_start: 0,
          next_offset: 10,
          items: [{ id: "p-old", kind: "permission", tool_name: "Bash", text: "Allow?" }],
        };
      },
    });
    await flush();
    assert(legacy.button("Confirm in Term"));
    assert(!legacy.buttons.some((b) => b.textContent === "Approve"));
    legacy.dispose();

    // Live Question with allows_other & Other option submits write-in via live_prompt_reply
    livePromptPayload = {
      kind: "numbered_menu",
      prompt: "Pick deployment mode",
      options: [
        { number: 1, label: "Staging", selected: true, checked: false },
        { number: 2, label: "Other", selected: false, checked: false },
      ],
      cursor: 0,
      checked: [],
      is_plan_approval: false,
      allows_other: true,
    };
    queuedPollData = {
      generation: "g-2b",
      agent: "claude",
      bound: true,
      page_start: 200,
      next_offset: 300,
      items: [
        {
          id: "q1",
          kind: "question",
          title: "Deploy",
          text: "Pick deployment mode",
          options: [
            { label: "Staging", description: "" },
            { label: "Other", description: "" },
          ],
        },
      ],
    };
    p2b.update();
    await flush();
    p2b.setQuestion({
      identity: "q-other",
      token: "tok-other",
      title: "Pick deployment mode",
      multiple: false,
      enabled: true,
      choices: [
        { label: "Staging", selected: true },
        { label: "Other", selected: false },
      ],
    });
    await flush();
    const qInput = p2b
      .find("chat-question-options")
      .parentElement.children.find((n) => n.tag === "input");
    assert.equal(qInput.hidden, false);
    qInput.value = "Canary 10%";
    qInput.dispatchEvent(new Event("input"));
    p2b.button("Send other").action();
    await flush();
    assert(
      p2b.calls.some(
        (c) =>
          c.action === "live_prompt_reply" &&
          c.choice === 1 &&
          c.custom_text === "Canary 10%",
      ),
    );

    p2b.dispose();
    console.log("PASS Phase 2B: Queue/Steer, Stop cooldown, WorkRun diffs, unread divider, rate-limit banner, live_prompt_reply");
  }

  // Phase 3A: chat_push subscribe/unsubscribe and chat_delta/chat_stale dispatch.
  {
    let pollReply = {
      generation: "g-3a",
      agent: "claude",
      bound: true,
      page_start: 0,
      next_offset: 100,
      items: [{ id: "0:0", kind: "assistant", text: "initial tail" }],
    };
    const p3a = fixture({
      lang: "en",
      capabilities: {
        chat_tail: true,
        chat_push: true,
        chat_composer: true,
        live_prompt_reply: true,
      },
      handler: async (req) => {
        if (req.action === "chat_composer") {
          return { agent: "claude", supported: true, model: "claude-opus-5-5" };
        }
        return pollReply;
      },
    });
    await flush();
    assert(
      p3a.calls.some(
        (c) => c.action === "chat_subscribe" && c.session === 7 && c.offset === 100,
      ),
      "expected initial chat_subscribe after poll",
    );
    const pollsBefore = p3a.calls.filter((c) => c.action === "chat_poll").length;
    const subsBefore = p3a.calls.filter((c) => c.action === "chat_subscribe").length;

    p3a.M.chat.onPush({
      push: "chat_delta",
      session: 7,
      data: {
        generation: "g-3a",
        agent: "claude",
        bound: true,
        page_start: 100,
        next_offset: 160,
        items: [{ id: "100:0", kind: "assistant", text: "pushed delta reply" }],
      },
    });
    await flush();
    assert.equal(p3a.entry.offset, 160);
    assert(p3a.entry.items.some((i) => i.text === "pushed delta reply"));
    assert.equal(p3a.calls.filter((c) => c.action === "chat_poll").length, pollsBefore);
    assert.equal(p3a.calls.filter((c) => c.action === "chat_subscribe").length, subsBefore);

    const statusCallsBefore = p3a.calls.filter((c) => c.action === "chat_composer").length;
    p3a.M.chat.onPush({
      push: "chat_delta",
      session: 7,
      data: {
        generation: "g-3a",
        agent: "claude",
        bound: true,
        page_start: 160,
        next_offset: 200,
        items: [{ id: "160:0", kind: "permission", tool_name: "Bash", text: "Allow?" }],
      },
    });
    await flush();
    assert(
      p3a.calls.filter((c) => c.action === "chat_composer").length > statusCallsBefore,
      "permission delta should trigger status refresh",
    );

    pollReply = {
      generation: "g-3a",
      agent: "claude",
      bound: true,
      page_start: 200,
      next_offset: 240,
      items: [{ id: "200:0", kind: "assistant", text: "stale fallback item" }],
    };
    p3a.M.chat.onPush({ push: "chat_stale", session: 7 });
    await flush();
    assert(p3a.calls.filter((c) => c.action === "chat_poll").length > pollsBefore);
    assert(p3a.entry.items.some((i) => i.text === "stale fallback item"));

    p3a.dispose();
    await flush();
    assert(
      p3a.calls.some((c) => c.action === "chat_unsubscribe" && c.session === 7),
      "dispose should unsubscribe chat push",
    );
    console.log("PASS Phase 3A: chat_subscribe/unsubscribe and chat_delta/chat_stale push client");
  }

  // Phase 3B: In-chat search bar, multi-photo attachments (<=4), and message actions <dialog>.
  {
    const p3b = fixture({
      lang: "en",
      capabilities: { chat_tail: true, chat_composer: true },
      handler: async () => ({
        generation: "g-3b",
        agent: "claude",
        bound: true,
        page_start: 0,
        next_offset: 50,
        items: [
          { id: "0:0", kind: "assistant", text: "alpha first finding" },
          { id: "0:1", kind: "user", text: "alpha second turn" },
          { id: "0:2", kind: "assistant", text: "gamma summary" },
        ],
      }),
    });
    await flush();

    // 1. In-chat search bar toggles, counts, highlights, and navigates hits.
    const searchBar = p3b.find("chat-search-bar");
    assert.equal(searchBar.hidden, true);
    p3b.find("icon-button composer-icon chat-search-toggle").action();
    assert.equal(searchBar.hidden, false);
    const searchInput = p3b.find("chat-search-input");
    searchInput.value = "alpha";
    searchInput.dispatchEvent(new Event("input"));
    assert.equal(p3b.find("chat-search-count muted").textContent, "1 / 2");
    assert.equal(
      p3b.root.descendants().filter((n) => String(n.className || "").includes("chat-search-hit")).length,
      2,
    );
    p3b.button("Next").action();
    assert.equal(p3b.find("chat-search-count muted").textContent, "2 / 2");
    p3b.button("Close").action();
    assert.equal(searchBar.hidden, true);
    assert.equal(
      p3b.root.descendants().filter((n) => String(n.className || "").includes("chat-search-hit")).length,
      0,
    );

    // 2. Multi-attachment (<= 4 photos) sends `photos`, rejects/caps 5th.
    const picker = p3b.root
      .descendants()
      .find((n) => n.tag === "input" && n.type === "file");
    assert.equal(picker.multiple, true);
    let picSeq = 0;
    p3b.prepare = async (_s, file) => ({
      mime: "image/png",
      data: `img-${++picSeq}-${file.name}`,
    });
    picker.files = [{ name: "a.png" }, { name: "b.png" }];
    picker.dispatchEvent(new Event("change"));
    await flush();
    assert.equal(p3b.entry.photo.name, "a.png");
    assert.equal(
      p3b.root.descendants().filter((n) => n.className === "chat-attachment-chip").length,
      2,
    );
    picker.files = [
      { name: "c.png" },
      { name: "d.png" },
      { name: "e.png" },
    ];
    picker.dispatchEvent(new Event("change"));
    await flush();
    assert.equal(p3b.entry.attachments.length, 4);
    assert(p3b.errors.some((e) => e.includes("Maximum 4 photos")));
    p3b.input("send multi");
    p3b.submit();
    await flush();
    const multiPrompt = p3b.calls.find(
      (c) => c.action === "prompt" && c.text === "send multi",
    );
    assert(multiPrompt, "multi-photo prompt must submit");
    assert.equal(multiPrompt.photos.length, 4);
    assert.equal(multiPrompt.photo, undefined);
    assert.equal(p3b.entry.photo, null);

    // 3. Message actions <dialog> sheet quotes into composer and switches to Term.
    const moreButtons = p3b.root
      .descendants()
      .filter((n) => n.className === "icon-button chat-msg-more");
    assert(moreButtons.length >= 2);
    moreButtons[0].action();
    const sheet = p3b.find("chat-actions-sheet");
    assert.equal(sheet.open, true);
    p3b.button("Quote").action();
    assert.equal(sheet.open, false);
    assert.equal(p3b.find("chat-input").value, "> alpha first finding\n\n");

    const termBefore = p3b.term;
    moreButtons[0].action();
    p3b.button("Jump to Term").action();
    assert.equal(p3b.term, termBefore + 1);

    p3b.dispose();
    console.log("PASS Phase 3B: in-chat search, multi-attachment (<=4), and message actions <dialog>");
  }

  // Queued prompt failure preserves text + photo attachments instead of dropping data
  {
    let failNextPrompt = true;
    const fx = fixture({
      capabilities: { chat_tail: true, chat_interrupt: true, prompt_steer: true },
      handler: async (req) => {
        if (req.action === "prompt" && failNextPrompt) {
          throw new Error("Connection interrupted");
        }
        if (req.action === "prompt") return { sent: true };
        if (req.action === "chat_poll") {
          return { generation: "g-q", agent: "claude", bound: true, items: [], page_start: 0, next_offset: 0 };
        }
        return { supported: false };
      },
    });
    fx.state.sessions.set(7, { id: 7, state: "Working", state_since_unix_ns: "1", cwd: "/work" });
    fx.update();
    await flush();

    const picker = fx.root.descendants().find((n) => n.tag === "input" && n.type === "file");
    fx.prepare = async (_s, f) => ({ mime: "image/png", data: `q-${f.name}` });
    picker.files = [{ name: "shot.png" }];
    picker.dispatchEvent(new Event("change"));
    await flush();
    fx.input("queued with photo");
    fx.button("Queue").action();
    await flush();
    assert.equal(fx.find("chat-input").value, "");
    assert.equal(fx.entry.attachments.length, 0);

    // Second queued prompt stays behind while first fails into empty composer draft
    fx.input("second queued");
    fx.button("Queue").action();
    await flush();

    fx.state.sessions.set(7, { id: 7, state: "Done", state_since_unix_ns: "2", cwd: "/work" });
    fx.update();
    await flush();
    assert.equal(
      fx.find("chat-input").value,
      "queued with photo",
      "failed queued prompt restores text to composer draft when draft is empty",
    );
    assert.equal(fx.state.drafts.get(7), "queued with photo");
    assert.equal(fx.entry.attachments.length, 1);
    assert.equal(fx.entry.attachments[0].prepared.data, "q-shot.png");
    assert.equal(
      fx.calls.filter((c) => c.action === "prompt").length,
      1,
      "second queued item must wait when first fails instead of jumping ahead",
    );

    // Case 2: if user already typed a new draft, failed queued prompt returns to head of queue with Retry
    fx.button("Remove queued").action();
    await flush();
    fx.state.sessions.set(7, { id: 7, state: "Working", state_since_unix_ns: "3", cwd: "/work" });
    fx.update();
    await flush();
    // Composer still holds "queued with photo" + photo; clear photo and queue a text prompt, then type a new draft
    fx.entry.attachments = [];
    fx.entry.photo = null;
    fx.input("queued kept behind draft");
    fx.button("Queue").action();
    await flush();
    fx.input("brand new draft");

    fx.state.sessions.set(7, { id: 7, state: "Done", state_since_unix_ns: "4", cwd: "/work" });
    fx.update();
    await flush();
    assert.equal(fx.find("chat-input").value, "brand new draft", "new user draft is never overwritten");
    assert.equal(fx.entry.queuedPrompts.length, 1);
    assert.equal(fx.entry.queuedPrompts[0].text, "queued kept behind draft");
    assert.equal(fx.find("chat-queued-strip").hidden, false);

    failNextPrompt = false;
    const retryQueueBtn = fx
      .find("chat-queued-strip")
      .descendants()
      .find((n) => n.textContent === "Retry");
    assert(retryQueueBtn, "paused queued item renders Retry button");
    retryQueueBtn.action();
    await flush();
    assert(
      fx.calls.some((c) => c.action === "prompt" && c.text === "queued kept behind draft"),
      "Retry dispatches the preserved queued item",
    );
    assert.equal(fx.find("chat-queued-strip").hidden, true);
    assert.equal(fx.find("chat-input").value, "brand new draft");

    fx.dispose();
    console.log("PASS failed queued prompts preserve text/photos without dropping or clobbering active drafts");
  }

  // Floating bulk work-process toggle button above jump-to-bottom.
  {
    const iconPaths = (btn) =>
      btn
        .descendants()
        .filter((n) => n.tag === "path")
        .map((n) => n.attrs.d);
    const EXPAND_PATHS = ["m7 15 5 5 5-5", "m7 9 5-5 5 5"];
    const COLLAPSE_PATHS = ["m7 20 5-5 5 5", "m7 4 5 5 5-5"];

    // 1. Hidden when chat has no work process blocks (empty, user/assistant/question/permission only).
    const plain = fixture({ lang: "ko" });
    await flush();
    const plainToggle = plain.find("chat-process-toggle");
    assert(plainToggle, "floating work-process toggle button mounts in chat view");
    assert.equal(plainToggle.hidden, true, "hidden in empty chat");
    await plain.poll({
      generation: "g-plain",
      agent: "claude",
      bound: true,
      next_offset: 3,
      items: [
        { id: "u1", kind: "user", text: "Line\n".repeat(12) },
        { id: "a1", kind: "assistant", text: "```js\nconsole.log(1)\n```" },
        {
          id: "q1",
          kind: "question",
          title: "Pick",
          text: "Choose one",
          options: [{ label: "A" }, { label: "B" }],
        },
      ],
    });
    assert.equal(
      plainToggle.hidden,
      true,
      "hidden when only user/assistant/question cards exist",
    );
    plain.dispose();

    // 2. Visible with work runs + standalone reasoning; syncs with manual toggles, bulk collapse/expand, active Working run, and preserves unrelated disclosures.
    const bulk = fixture({ lang: "ko" });
    bulk.state.sessions.set(7, { id: 7, state: "Working", cwd: "/work" });
    await flush();
    await bulk.poll({
      generation: "g-bulk",
      agent: "agy",
      bound: true,
      next_offset: 9,
      items: [
        { id: "u1", kind: "user", text: "작업 시작해줘" },
        { id: "r0", kind: "reasoning", text: "단독 생각 과정 내용입니다." },
        { id: "a0", kind: "assistant", text: "첫 번째 단계입니다.\n다음으로 도구를 실행합니다." },
        {
          id: "t1",
          kind: "tool_call",
          call_id: "c1",
          tool_name: "read_file",
          file_path: "/work/a.rs",
        },
        { id: "tr1", kind: "tool_result", call_id: "c1", text: "ok" },
        {
          id: "q1",
          kind: "question",
          title: "Confirm",
          text: "Proceed?",
          options: [{ label: "Yes" }, { label: "No" }],
        },
        { id: "a1", kind: "assistant", text: "두 번째 작업 진행합니다." },
        { id: "r2", kind: "reasoning", text: "진행 중 생각" },
        {
          id: "t2",
          kind: "tool_call",
          call_id: "c2",
          tool_name: "run_command",
          title: "cargo nextest",
          text: "cargo nextest",
        },
      ],
    });

    const toggleBtn = bulk.find("chat-process-toggle");
    const jumpBtn = bulk.find("chat-jump-bottom");
    const runs = bulk.root
      .descendants()
      .filter((n) => n.className === "chat-tool-run");
    const standaloneReasoning = bulk
      .find("chat-feed")
      .children.find((n) => n.className === "chat-card chat-reasoning")
      ?.children?.[0];
    const pastQuestion = bulk.find("chat-past-question");
    assert.equal(runs.length, 2, "two work runs (completed + active Working)");
    assert(standaloneReasoning, "standalone top-level reasoning exists");
    assert.equal(runs[1].dataset.active, "true", "tail run is active Working");
    assert.equal(jumpBtn.hidden, true, "jump button is hidden while following at bottom");
    assert.equal(
      toggleBtn.hidden,
      false,
      "work-process toggle remains visible even when jump button is hidden",
    );
    assert.equal(toggleBtn.dataset.expanded, "false");
    assert.equal(toggleBtn.attrs["aria-pressed"], "false");
    assert.equal(toggleBtn.attrs["aria-label"], "작업 과정 모두 펼치기");
    assert.equal(toggleBtn.title, "작업 과정 모두 펼치기");
    assert.deepEqual(iconPaths(toggleBtn), EXPAND_PATHS);

    // Open pastQuestion independently to prove bulk toggle never touches unrelated disclosures.
    pastQuestion.open = true;
    pastQuestion.dispatchEvent(new Event("toggle"));
    assert.equal(
      toggleBtn.dataset.expanded,
      "false",
      "opening a question card does not flip work-process toggle state",
    );

    // Bulk expand when all work processes are closed.
    toggleBtn.action();
    assert.equal(runs[0].open, true);
    assert.equal(runs[1].open, true);
    assert.equal(standaloneReasoning.open, true);
    assert.equal(
      standaloneReasoning.children.length,
      2,
      "bulk expand synchronously populates lazy reasoning body",
    );
    assert.equal(toggleBtn.dataset.expanded, "true");
    assert.equal(toggleBtn.attrs["aria-pressed"], "true");
    assert.equal(toggleBtn.attrs["aria-label"], "작업 과정 모두 접기");
    assert.equal(toggleBtn.title, "작업 과정 모두 접기");
    assert.deepEqual(iconPaths(toggleBtn), COLLAPSE_PATHS);
    assert.equal(pastQuestion.open, true, "unrelated question card stays open");

    // Bulk collapse closes everything including active Working run.
    toggleBtn.action();
    assert.equal(runs[0].open, false);
    assert.equal(runs[1].open, false);
    assert.equal(standaloneReasoning.open, false);
    assert.equal(toggleBtn.dataset.expanded, "false");
    assert.equal(toggleBtn.attrs["aria-label"], "작업 과정 모두 펼치기");
    assert.deepEqual(iconPaths(toggleBtn), EXPAND_PATHS);
    assert.equal(pastQuestion.open, true, "unrelated question card is untouched on collapse");

    // Manually opening a single run (the active Working run) switches button to collapse-all, and clicking closes it.
    runs[1].open = true;
    runs[1].dispatchEvent(new Event("toggle"));
    assert.equal(toggleBtn.dataset.expanded, "true");
    assert.equal(toggleBtn.attrs["aria-label"], "작업 과정 모두 접기");
    assert.deepEqual(iconPaths(toggleBtn), COLLAPSE_PATHS);

    // Manually closing that run switches button back to expand-all.
    runs[1].open = false;
    runs[1].dispatchEvent(new Event("toggle"));
    assert.equal(toggleBtn.dataset.expanded, "false");
    assert.equal(toggleBtn.attrs["aria-label"], "작업 과정 모두 펼치기");

    // Manually open 1 of 3 targets, then bulk click collapses all.
    runs[0].open = true;
    runs[0].dispatchEvent(new Event("toggle"));
    assert.equal(toggleBtn.dataset.expanded, "true");
    toggleBtn.action();
    assert.equal(runs[0].open, false);
    assert.equal(runs[1].open, false);
    assert.equal(standaloneReasoning.open, false);
    assert.equal(toggleBtn.dataset.expanded, "false");

    // 3. Session switch isolates toggle state per session (no state leak from session 7 to session 8).
    toggleBtn.action(); // Expand all on session 7
    assert.equal(toggleBtn.dataset.expanded, "true");
    bulk.dispose();

    bulk.state.sessions.set(8, { id: 8, state: "Waiting", cwd: "/work2" });
    const s7Items = bulk.entry.items.slice();
    bulk.handler = async (req) =>
      req.action === "chat_poll"
        ? req.session === 8
          ? {
              generation: "g-s8",
              agent: "claude",
              bound: true,
              next_offset: 2,
              items: [
                { id: "s8-u", kind: "user", text: "다른 세션" },
                { id: "s8-t", kind: "tool_call", tool_name: "read_file", file_path: "/work2/b.rs" },
              ],
            }
          : {
              generation: "g-bulk",
              agent: "agy",
              bound: true,
              next_offset: s7Items.length,
              items: s7Items,
            }
        : { sent: true };
    const s8View = bulk.M.chat.mount(bulk.root, { ctx: bulk.ctx, sessionId: 8 });
    await flush();
    const s8Toggle = bulk.find("chat-process-toggle");
    const s8Run = bulk.find("chat-tool-run");
    assert.equal(s8Run.open, false, "new session starts collapsed without leaking previous session state");
    assert.equal(s8Toggle.hidden, false);
    assert.equal(s8Toggle.dataset.expanded, "false");
    assert.equal(s8Toggle.attrs["aria-label"], "작업 과정 모두 펼치기");
    s8View.unmount();

    // Switching back to session 7 restores session 7's open state accurately.
    const s7Back = bulk.M.chat.mount(bulk.root, { ctx: bulk.ctx, sessionId: 7 });
    await flush();
    assert.equal(bulk.find("chat-process-toggle").dataset.expanded, "true");
    assert.equal(bulk.find("chat-process-toggle").attrs["aria-label"], "작업 과정 모두 접기");
    s7Back.unmount();

    // 4. English locale labels.
    const en = fixture({ lang: "en" });
    await flush();
    await en.poll({
      generation: "g-en",
      agent: "claude",
      bound: true,
      next_offset: 1,
      items: [{ id: "t1", kind: "tool_call", tool_name: "read_file", file_path: "/a" }],
    });
    const enBtn = en.find("chat-process-toggle");
    assert.equal(enBtn.attrs["aria-label"], "Expand all work steps");
    enBtn.action();
    assert.equal(enBtn.attrs["aria-label"], "Collapse all work steps");
    en.dispose();

    console.log(
      "PASS floating bulk work-process toggle: visibility, expand/collapse all, manual sync, active run, unrelated disclosures, i18n, and session isolation",
    );
  }

  // Sweep W2 regressions: MB-21, MB-27, MB-28, MB-57, MB-58, MB-59, MB-64.
  {
    // MB-57: isEntryPinned treats queuedPrompts as pinned.
    const fPin = fixture({});
    assert.equal(fPin.M.chat.isEntryPinned({ mounts: 0, queuedPrompts: [{ id: 1, text: "queued" }] }), true, "MB-57: queuedPrompts pins chat entry");
    fPin.dispose();

    // MB-58: 10-digit epoch-second timestamps render in actual year, not 1970-01-21.
    const fTime = fixture({
      handler: async (req) =>
        req.action === "chat_poll"
          ? {
              bound: true,
              agent: "agy",
              items: [{ id: "agy-1", kind: "assistant", text: "Hi", at: "1735689600" }],
              next_offset: 1,
              reset: false,
            }
          : {},
    });
    await flush();
    const timeEl = fTime.find("chat-time");
    assert(timeEl, "MB-58: chat-time element rendered");
    assert(timeEl.attrs.datetime.startsWith("2025-01-01"), `MB-58: expected 2025-01-01, got ${timeEl.attrs.datetime}`);
    fTime.dispose();

    // MB-28: syncReadScope() nulling entry.progress during in-flight prompt does not throw TypeError.
    let resolvePrompt;
    const fProg = fixture({
      handler: (req) => {
        if (req.action === "prompt") return new Promise((r) => { resolvePrompt = r; });
        return Promise.resolve({ bound: true, agent: "claude", items: [], next_offset: 0, reset: false });
      },
    });
    await flush();
    fProg.input("hello");
    fProg.submit();
    await flush();
    // Change read scope mid-RPC -> syncReadScope() sets entry.progress = null
    fProg.state.sessions.set(7, { id: 7, agent: "claude", cwd: "/other-cwd", groupKey: "g2", state: "Done" });
    fProg.update();
    assert.equal(fProg.entry.progress, null, "MB-28: syncReadScope cleared entry.progress");
    resolvePrompt({ ok: true });
    await flush();
    assert.equal(fProg.errors.length, 0, "MB-28: no TypeError when entry.progress cleared mid-RPC");
    assert.equal(fProg.state.drafts.has(7), false, "MB-28: draft still cleared on prompt resolve");
    fProg.dispose();

    // MB-21: Multi-photo preparation passes adaptive maxBytes and oversized combined payload rejects cleanly before RPC.
    const photoOptsLog = [];
    const fPhoto = fixture({
      handler: async () => ({ bound: true, agent: "claude", items: [], next_offset: 0, reset: false }),
    });
    await flush();
    fPhoto.prepare = async (_s, file, opts) => {
      photoOptsLog.push({ name: file.name, opts });
      return { mime: "image/jpeg", data: "A".repeat(400000) };
    };
    const picker = fPhoto.root.descendants().find((n) => n.tag === "input" && n.type === "file");
    picker.files = [{ name: "p1.jpg", type: "image/jpeg", size: 100 }, { name: "p2.jpg", type: "image/jpeg", size: 100 }];
    picker.dispatchEvent(new Event("change"));
    await flush();
    assert(photoOptsLog.some((x) => x.opts && x.opts.maxBytes === Math.floor((480 * 1024) / 2)), "MB-21: multi-photo passes shared raw byte budget maxBytes");
    // Combined two 400KB base64 (~600KB raw > 512KB) must fail cleanly before ctx.rpc
    fPhoto.submit();
    await flush();
    assert.equal(fPhoto.calls.filter((c) => c.action === "prompt").length, 0, "MB-21: oversized combined photos rejected before rpc");
    assert(fPhoto.errors.some((e) => String(e).includes("Photos are too large")), "MB-21: shows clear combined photo limit error");
    fPhoto.dispose();

    // MB-27 (chat.js): botSession keeps ownsEntry true across snapshot wipe and upgrades synthetic -> real without clearing transcript.
    const fBot = fixture({
      session: { id: 7, agent: "claude", groupKey: "bot:b1", state: "Done", _synthetic: true },
      botSession: true,
      handler: async (req) =>
        req.action === "chat_poll"
          ? { bound: true, agent: "claude", items: [{ id: "m1", kind: "assistant", text: "Bot reply" }], next_offset: 1, reset: false }
          : {},
    });
    await flush();
    assert(fBot.find("chat-feed").textContent.includes("Bot reply"));
    // Wipe state.sessions (snapshot excluded bot session)
    fBot.state.sessions = new Map();
    fBot.update();
    await flush();
    assert(fBot.find("chat-feed").textContent.includes("Bot reply"), "MB-27: botSession keeps transcript when excluded from snapshot");
    // Real session arrives with different cwd/groupKey -> must NOT clear transcript
    fBot.state.sessions.set(7, { id: 7, agent: "claude", cwd: "/bots/b1", groupKey: "hex-real", state: "Done" });
    fBot.update();
    await flush();
    assert.equal(fBot.entry.items.length, 1, "MB-27: synthetic -> real session upgrade preserves transcript without clearTranscript wipe");
    fBot.dispose();

    // MB-59: clearTranscript re-observes composer as well as feed.
    const observed = [];
    const fResize = fixture({
      handler: async () => ({ bound: true, agent: "claude", items: [], next_offset: 0, reset: false }),
      ResizeObserver: class {
        observe(el) { observed.push(el.className); }
        disconnect() { observed.push("disconnect"); }
      },
    });
    await flush();
    fResize.state.sessions.set(7, { id: 7, agent: "claude", cwd: "/changed", groupKey: "g-changed", state: "Done" });
    fResize.update();
    await flush();
    const afterDisconnect = observed.slice(observed.lastIndexOf("disconnect") + 1);
    assert(afterDisconnect.some((c) => String(c).includes("chat-feed")), "MB-59: clearTranscript re-observes feed");
    assert(afterDisconnect.some((c) => String(c).includes("chat-composer")), "MB-59: clearTranscript re-observes composer");
    fResize.dispose();

    // MB-64: double-tapping non-icon copyText button restores original label ("Copy answer"), not "Copied".
    const fCopy = fixture({
      handler: async (req) =>
        req.action === "chat_poll"
          ? {
              bound: true,
              agent: "claude",
              items: [{ id: "q1", kind: "question", text: "Pick?", options: ["Yes", "No"] }],
              next_offset: 1,
              reset: false,
            }
          : {},
    });
    await flush();
    const optBtn = fCopy.root.descendants().find((b) => b.tag === "button" && String(b.textContent || "").includes("Yes"));
    if (optBtn) {
      optBtn.action();
      await flush();
      const copyBtn = fCopy.button("Copy answer");
      assert(copyBtn, "MB-64: Copy answer button rendered");
      copyBtn.action();
      await flush();
      assert.equal(copyBtn.textContent, "Copied");
      // Second tap while still showing "Copied"
      copyBtn.action();
      await flush();
      assert.equal(copyBtn.dataset.copyLabel, "Copy answer", "MB-64: original label preserved on double-tap");
      // Fire reset timer -> must restore "Copy answer", NOT "Copied"
      for (const fn of [...fCopy.timers.values()]) {
        if (fn.ms === 1500) fn();
      }
      assert.equal(copyBtn.textContent, "Copy answer", "MB-64: timer restores original label Copy answer");
    }
    fCopy.dispose();
    console.log("PASS sweep W2 chat regressions: MB-21, MB-27, MB-28, MB-57, MB-58, MB-59, MB-64");

    // R2M-12: Multi-select photoPicker gives ALL selected photos the shared per-photo budget up front,
    // AND adding a second photo while the first is still pending re-triggers the pending photo at half budget.
    {
      const calls = [];
      const fMulti = fixture({
        handler: async () => ({ bound: true, agent: "claude", items: [], next_offset: 0, reset: false }),
      });
      await flush();
      fMulti.prepare = async (_s, file, opts) => {
        calls.push({ name: file.name, maxBytes: opts?.maxBytes ?? null });
        return { mime: "image/jpeg", data: "A".repeat(1000) };
      };
      const picker = fMulti.root.descendants().find((n) => n.tag === "input" && n.type === "file");
      picker.files = [
        { name: "m1.jpg", type: "image/jpeg", size: 100 },
        { name: "m2.jpg", type: "image/jpeg", size: 100 },
        { name: "m3.jpg", type: "image/jpeg", size: 100 },
      ];
      picker.dispatchEvent(new Event("change"));
      await flush();
      const expectedThird = Math.floor((480 * 1024) / 3);
      assert.equal(calls.length, 3, "R2M-12: 3 photos prepared once each with final 3-way cap");
      assert(calls.every((c) => c.maxBytes === expectedThird), `R2M-12: all 3 photos got maxBytes=${expectedThird}, got ${JSON.stringify(calls)}`);
      fMulti.dispose();

      // Sequential pick while Photo 1 is STILL pending: Photo 1 must be re-triggered with half budget and ignore stale oversized completion.
      const seqCalls = [];
      let resolveFirstStale = null;
      const fSeq = fixture({
        handler: async () => ({ bound: true, agent: "claude", items: [], next_offset: 0, reset: false }),
      });
      await flush();
      fSeq.prepare = (_s, file, opts) => {
        seqCalls.push({ name: file.name, maxBytes: opts?.maxBytes ?? null });
        if (file.name === "s1.jpg" && !opts?.maxBytes) {
          return new Promise((r) => {
            resolveFirstStale = () => r({ mime: "image/jpeg", data: "B".repeat(600000) });
          });
        }
        return Promise.resolve({ mime: "image/jpeg", data: "C".repeat(200000) });
      };
      const pSeq = fSeq.root.descendants().find((n) => n.tag === "input" && n.type === "file");
      pSeq.files = [{ name: "s1.jpg", type: "image/jpeg", size: 100 }];
      pSeq.dispatchEvent(new Event("change"));
      // Photo 1 is still pending; pick Photo 2 immediately.
      pSeq.files = [{ name: "s2.jpg", type: "image/jpeg", size: 100 }];
      pSeq.dispatchEvent(new Event("change"));
      resolveFirstStale?.();
      await flush();
      const half = Math.floor((480 * 1024) / 2);
      assert(
        seqCalls.some((c) => c.name === "s1.jpg" && c.maxBytes === half),
        `R2M-12: pending s1.jpg re-triggered with maxBytes=${half}, calls=${JSON.stringify(seqCalls)}`,
      );
      assert.equal(fSeq.entry.attachments[0].prepared.data.length, 200000, "R2M-12: stale oversized pending s1.jpg resolution discarded");
      fSeq.dispose();
    }

    // R2M-13 + R2M-24: Live question card syncs write-in inputs when chat_composer resolves after chat_poll,
    // guards agent match, and chat_composer error on forced poll refresh never causes unhandledRejection.
    {
      let resolveComposer = null;
      const fQ = fixture({
        capabilities: { chat_composer: true, live_prompt_reply: true },
        handler: (req) => {
          if (req.action === "chat_poll") {
            return Promise.resolve({
              bound: true,
              agent: "claude",
              items: [
                {
                  id: "q-r2m13",
                  kind: "question",
                  title: "Target",
                  text: "Choose target",
                  options: [{ label: "Staging" }, { label: "Other" }],
                },
              ],
              next_offset: 1,
              reset: false,
            });
          }
          if (req.action === "chat_composer") {
            return new Promise((r) => {
              resolveComposer = r;
            });
          }
          return Promise.resolve({});
        },
      });
      await flush();
      fQ.setQuestion({
        identity: "q-r2m13",
        token: "tok-13",
        title: "Choose target",
        enabled: true,
        multiple: false,
        choices: [{ label: "Staging", selected: true }, { label: "Other", selected: false }],
      });
      await flush();
      const otherBtn = fQ.root.descendants().find((n) => String(n.className || "").includes("chat-other-submit"));
      assert(otherBtn, "R2M-13: Send other button exists on question card");
      assert.equal(otherBtn.hidden, true, "R2M-13: hidden before chat_composer delivers live_prompt");
      // Mismatched agent composerStatus must NOT unhide Send other.
      resolveComposer?.({
        agent: "codex",
        supported: true,
        model: "gpt-5",
        live_prompt: { kind: "numbered_menu", allows_other: true, prompt: "Choose target" },
      });
      await flush();
      assert.equal(otherBtn.hidden, true, "R2M-13: mismatched agent does not unhide write-in");
      // Now resolve a fresh chat_composer with matching agent -> syncAgent calls card._syncQuestion and unhides write-in.
      fQ.entry.composerStatusAt = 0;
      fQ.update();
      await flush();
      resolveComposer?.({
        agent: "claude",
        supported: true,
        model: "claude-sonnet-4",
        live_prompt: { kind: "numbered_menu", allows_other: true, prompt: "Choose target" },
      });
      await flush();
      assert.equal(otherBtn.hidden, false, "R2M-13: syncAgent calls card._syncQuestion and unhides Send other");
      // Also verify mismatched agent directly on entry.composerStatus keeps write-in hidden after syncQuestion.
      fQ.entry.composerStatus = {
        agent: "codex",
        supported: true,
        model: "gpt-5",
        live_prompt: { kind: "numbered_menu", allows_other: true, prompt: "Choose target" },
      };
      fQ.update();
      await flush();
      assert.equal(otherBtn.hidden, true, "R2M-13: guard entry.composerStatus.agent === chatAgent() hides write-in on mismatch");
      fQ.dispose();

      // R2M-24: Forced refreshStatus(true) triggered by incoming question/permission in applyPollPayload catches rejections.
      let unhandled = 0;
      const onUnhandled = () => { unhandled++; };
      process.on("unhandledRejection", onUnhandled);
      try {
        const fErr = fixture({
          capabilities: { chat_composer: true },
          handler: (req) => {
            if (req.action === "chat_poll") {
              return Promise.resolve({
                bound: true,
                agent: "claude",
                items: [{ id: "perm-r2m24", kind: "permission", text: "Allow tool?" }],
                next_offset: 1,
                reset: false,
              });
            }
            if (req.action === "chat_composer") {
              return Promise.reject(Error("composer boom"));
            }
            return Promise.resolve({});
          },
        });
        await flush();
        await new Promise((r) => setTimeout(r, 20));
        assert.equal(unhandled, 0, "R2M-24: failing forced refreshStatus in applyPollPayload must not trigger unhandledRejection");
        fErr.dispose();
      } finally {
        process.off("unhandledRejection", onUnhandled);
      }
      console.log("PASS sweep R2 chat regressions: R2M-12, R2M-13, R2M-24");

      // Regression: short assistant message followed by a long/multiline assistant message and a tool_call
      // must not mark the earlier short assistant message as an orphaned foldable note that passes null to insertBefore.
      let orphanUnhandled = 0;
      const onOrphanUnhandled = () => { orphanUnhandled++; };
      process.on("unhandledRejection", onOrphanUnhandled);
      try {
        const fOrphan = fixture({
          capabilities: { chat_tail: true },
          handler: async (req) => {
            if (req.action === "chat_poll" && req.latest) {
              return {
                generation: "g-orphan",
                agent: "claude",
                bound: true,
                page_start: 100,
                next_offset: 200,
                items: [
                  { id: "a1", kind: "assistant", text: "Short intro." },
                  { id: "a2", kind: "assistant", text: "Line one\nLine two with details." },
                  { id: "t1", kind: "tool_call", tool_name: "Bash", text: "ls" },
                  { id: "t2", kind: "tool_result", tool_name: "Bash", text: "ok" },
                ],
              };
            }
            if (req.action === "chat_poll") {
              return {
                generation: "g-orphan",
                agent: "claude",
                bound: true,
                page_start: 0,
                next_offset: 100,
                items: [
                  { id: "u0", kind: "user", text: "run check" },
                ],
              };
            }
            return {};
          },
        });
        await flush();
        await new Promise((r) => setTimeout(r, 20));
        const feed = fOrphan.find("chat-feed");
        assert(
          feed.children.every((node) => node !== null && node !== undefined),
          "feed never receives a null child when a short assistant precedes a non-foldable assistant and tool_call",
        );
        assert(
          feed.children.some(
            (n) =>
              n.tag === "article" &&
              [n, ...n.descendants()].some((d) =>
                String(d.textContent || "").includes("Short intro."),
              ),
          ),
          "short assistant message separated by a non-foldable assistant message renders as a normal card",
        );
        assert.equal(orphanUnhandled, 0, "fillHistory/loadEarlier must not emit unhandledRejection");
        fOrphan.dispose();
      } finally {
        process.off("unhandledRejection", onOrphanUnhandled);
      }
      console.log("PASS regression: orphaned foldable note across non-foldable assistant and loadEarlier");
    }
    // MOB-02, MOB-04, MOB-07, MOB-16 regressions in chat.js
    {
      let voiceOpts = null;
      let voiceCleaned = 0;
      const fMob = fixture({
        capabilities: { composer_models: true, chat_composer: true },
        voice: {
          attach(_ta, _host, opts) {
            voiceOpts = opts;
            return {
              el: null,
              onSend() {},
              setDisabled() {},
              cleanup() {
                voiceCleaned++;
              },
            };
          },
        },
        handler: async (req) => {
          if (req.action === "chat_poll") {
            return {
              generation: "g-mob",
              agent: "claude",
              bound: true,
              next_offset: 10,
              items: [
                { id: "m1", kind: "assistant", text: "alpha hit one" },
                { id: "m2", kind: "assistant", text: "alpha hit two" },
              ],
            };
          }
          if (req.action === "chat_composer_status") {
            return { supported: true, models: [{ id: "m1", label: "M1" }], effort_supported: false };
          }
          if (req.action === "composer_models") {
            return { supported: true, items: [{ id: "m1", label: "M1" }] };
          }
          return {};
        },
      });
      await flush();
      // MOB-02: invoking voice onInsert calls sizeInput(true) without throwing ReferenceError.
      assert(voiceOpts && typeof voiceOpts.onInsert === "function", "MOB-02: voice.attach receives onInsert");
      assert.doesNotThrow(() => voiceOpts.onInsert("dictated phrase"), "MOB-02: onInsert must not throw ReferenceError");
      // MOB-07: chooseModel() blurs document.activeElement before opening model sheet.
      let blurred = 0;
      const inputEl = fMob.find("chat-input");
      inputEl.blur = () => { blurred++; };
      fMob.doc.activeElement = inputEl;
      const modelBtn = fMob.find("composer-model-name")?.parentElement;
      assert(modelBtn && typeof modelBtn.action === "function", "model button exists");
      modelBtn.action();
      await flush();
      assert(blurred >= 1, "MOB-07: chooseModel() blurs activeElement before opening model sheet");
      // MOB-16: Chat Search Enter ignored during IME composition (isComposing / keyCode 229).
      const searchToggle = fMob.root.descendants().find((n) => n.attrs?.["aria-label"] === "Search messages");
      searchToggle?.action?.();
      await flush();
      const searchIn = fMob.find("chat-search-input");
      assert(searchIn, "chat search input exists");
      searchIn.value = "alpha";
      searchIn.dispatchEvent(new Event("input"));
      const countEl = fMob.find("chat-search-count");
      const beforeCount = countEl?.textContent;
      searchIn.dispatchEvent(Object.assign(new Event("keydown", { cancelable: true }), { key: "Enter", isComposing: true }));
      assert.equal(countEl?.textContent, beforeCount, "MOB-16: Enter with isComposing must not step search");
      searchIn.dispatchEvent(Object.assign(new Event("keydown", { cancelable: true }), { key: "Enter", keyCode: 229 }));
      assert.equal(countEl?.textContent, beforeCount, "MOB-16: Enter with keyCode 229 must not step search");
      // MOB-04: chat unmount calls voiceCtl.cleanup().
      fMob.dispose();
      assert.equal(voiceCleaned, 1, "MOB-04: chat unmount cleans up voiceCtl");
      console.log("PASS MOB-02/MOB-04/MOB-07/MOB-16 chat voice insert, unmount cleanup, model blur & search IME guard");
    }
  }
}
main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
