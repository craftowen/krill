// Executed browser modules with a deterministic clock, sockets and fetch boundary.
const test = require("node:test"),
  assert = require("node:assert/strict"),
  vm = require("node:vm"),
  fs = require("node:fs"),
  path = require("node:path"),
  crypto = require("node:crypto");
const assets = path.join(__dirname, "../src/mobile/assets"),
  read = (n) => fs.readFileSync(path.join(assets, n), "utf8"),
  enc = new TextEncoder(),
  flush = async () => {
    for (let i = 0; i < 40; i++) await Promise.resolve();
  };
const KEY = "krill-mobile-error-reporting-v1",
  OUTBOX = "krill-mobile-error-outbox-v1";
function harness({
  store = new Map(),
  dnt,
  gpc,
  capacity = Infinity,
  href = "https://krill-mobile.pages.dev/m/0123456789abcdef/#secret-qr",
  quota = false,
  initialNow = 2000000,
} = {}) {
  let now = initialNow,
    next = 0;
  const timers = new Map(),
    listeners = {},
    requests = [],
    sockets = [],
    context = vm.createContext({
      TextEncoder,
      TextDecoder,
      Uint8Array,
      Uint32Array,
      DataView,
      URL,
      URLSearchParams,
      AbortController,
      crypto: crypto.webcrypto,
      Date: class extends Date {
        static now() {
          return now;
        }
      },
      navigator: {
        userAgent: "iPhone Safari account-private-ua",
        onLine: true,
        doNotTrack: dnt,
        globalPrivacyControl: gpc,
      },
      location: new URL(href),
      document: { visibilityState: "visible" },
      history: { replaceState() {} },
      localStorage: {
        getItem: (k) => (store.has(k) ? store.get(k) : null),
        setItem: (k, v) => {
          if (quota) throw Error("quota");
          const updated = new Map(store).set(k, String(v));
          const size = [...updated].reduce(
            (n, [key, value]) => n + key.length + value.length,
            0,
          );
          if (size > capacity) throw Error("quota");
          store.set(k, String(v));
        },
        removeItem: (k) => {
          if (quota) throw Error("quota");
          store.delete(k);
        },
      },
      setTimeout: (fn, ms = 0) => {
        const id = ++next;
        timers.set(id, { fn, at: now + ms });
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
      addEventListener: (name, fn) => (listeners[name] ||= []).push(fn),
    });
  const C = {
    enc,
    concat: (...bytes) => Uint8Array.from(bytes.flatMap((b) => Array.from(b))),
    unhex: () => new Uint8Array(32),
    unbase64: () => new Uint8Array(32),
    hex: (b) => Buffer.from(b).toString("hex"),
    sha256: (b) =>
      new Uint8Array(crypto.createHash("sha256").update(b).digest()),
    Channel: class {
      seal(b) {
        return b;
      }
      open(b) {
        return b;
      }
    },
  };
  class Socket {
    constructor(url) {
      this.url = url;
      this.handlers = {};
      this.sent = [];
      sockets.push(this);
    }
    addEventListener(n, f) {
      (this.handlers[n] ||= []).push(f);
    }
    removeEventListener(n, f) {
      this.handlers[n] = (this.handlers[n] || []).filter((x) => x !== f);
    }
    emit(n, e = {}) {
      for (const fn of [...(this.handlers[n] || [])]) fn(e);
    }
    send(b) {
      this.sent.push(b);
    }
    close() {
      this.closed = true;
    }
  }
  let collector = async (_url, options) => ({
      ok: true,
      json: async () => ({
        schema_version: 1,
        accepted_ids: JSON.parse(options.body).events.map((e) => e.event_id),
        duplicate_ids: [],
      }),
    }),
    discovery = async () => ({ status: 503, ok: false });
  context.fetch = async (url, options = {}) => {
    if (url.startsWith("https://errors.krill.page/")) {
      requests.push({ url, options, body: JSON.parse(options.body) });
      return collector(url, options);
    }
    return discovery(url, options);
  };
  context.MkCrypto = C;
  context.WebSocket = Socket;
  vm.runInContext(read("diagnostics.js"), context);
  vm.runInContext(read("client.js"), context);
  const diagnostics = context.KrillMobile.diagnostics;
  async function tick(ms = 0) {
    now += ms;
    for (const [id, t] of [...timers])
      if (t.at <= now) {
        timers.delete(id);
        t.fn();
      }
    await flush();
  }
  function emit(name, event) {
    for (const fn of listeners[name] || []) fn(event);
  }
  function transport({ endpoint, url = "wss://private-desktop.test/ws" } = {}) {
    return context.KrillMobile.transport.create({
      endpoint,
      url,
      credentials: endpoint?.credentials || {
        device_id: "account-device-secret",
        credential: "credential-secret",
      },
      warmTerminal: false,
    });
  }
  async function start(t, stage = 0) {
    const pending = t.connect();
    await flush();
    const socket = sockets.at(-1);
    for (let n = 1; n <= stage; n++) {
      if (n === 1) socket.emit("open");
      if (n === 2)
        socket.emit("message", {
          data: JSON.stringify({ version: 2, challenge: "challenge-secret" }),
        });
      if (n === 3) socket.emit("message", { data: enc.encode("{}") });
      if (n === 4)
        socket.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 1]) });
      await flush();
    }
    return { pending, socket };
  }
  function response(socket, data, ok = true) {
    const bytes = enc.encode(
        JSON.stringify(
          ok
            ? { ok: true, data }
            : { ok: false, error: "terminal-private-content" },
        ),
      ),
      length = [];
    let n = bytes.length;
    do {
      const b = n & 127;
      n >>>= 7;
      length.push(b | (n ? 128 : 0));
    } while (n);
    socket.emit("message", {
      data: Uint8Array.from([0, 0, 0, 0, 101, 6, ...length, ...bytes]),
    });
  }
  return {
    context,
    diagnostics,
    store,
    requests,
    sockets,
    timers,
    tick,
    emit,
    transport,
    start,
    response,
    setCollector: (fn) => (collector = fn),
    setDiscovery: (fn) => (discovery = fn),
    events: () => requests.flatMap((r) => r.body.events),
  };
}
const event = (h, extra = {}) =>
  h.diagnostics.report("mobile_runtime_error", {
    phase: "runtime",
    reason: "runtime_error",
    ...extra,
  });
test("automatic collection needs no settings visit; saved opt-out survives reload and no past errors replay", async () => {
  const h = harness();
  assert.equal(h.diagnostics.enabled(), true);
  assert.equal(h.store.get(KEY), "1");
  event(h);
  await h.tick();
  assert.equal(h.events().length, 1);
  h.diagnostics.setEnabled(false);
  assert.equal(h.store.get(KEY), "");
  const off = harness({ store: h.store });
  assert.equal(off.diagnostics.enabled(), false);
  event(off);
  await off.tick();
  assert.equal(off.requests.length, 0);
  off.diagnostics.setEnabled(true);
  await off.tick();
  assert.equal(off.requests.length, 0);
  event(off);
  await off.tick();
  assert.equal(off.requests.length, 1);
});
for (const saved of ["off", "", "0", "on", "1", "corrupt"])
  test(`saved preference ${JSON.stringify(saved)} migrates without reversing explicit off`, () => {
    const h = harness({ store: new Map([[KEY, saved]]) });
    assert.equal(h.diagnostics.enabled(), ["on", "1"].includes(saved));
    if (saved !== "corrupt")
      assert.equal(h.store.get(KEY), ["on", "1"].includes(saved) ? "1" : "");
  });
for (const signal of [
  { dnt: "1" },
  { dnt: "yes" },
  { gpc: true },
  { dnt: "1", gpc: true },
])
  test(`browser privacy signal ${JSON.stringify(signal)} wins over default and saved on`, async () => {
    for (const store of [new Map(), new Map([[KEY, "on"]])]) {
      const h = harness({ ...signal, store });
      assert.equal(h.diagnostics.enabled(), false);
      assert.equal(h.diagnostics.setEnabled(true), false);
      event(h);
      await h.tick();
      assert.equal(h.requests.length, 0);
      assert.equal(h.store.has(OUTBOX), false);
    }
  });
test("error then close1006 reports automatically before desktop auth, preserves route and actual code", async () => {
  const h = harness();
  const t = h.transport(),
    { pending, socket } = await h.start(t);
  socket.emit("error");
  await pending;
  socket.emit("close", { code: 1006 });
  await h.tick();
  const events = h.events();
  assert.equal(
    events.filter((e) => e.diagnostics.reason === "websocket_error").length,
    1,
  );
  assert(events.some((e) => e.diagnostics.reason === "websocket_error"));
  const closed = events.find((e) => e.diagnostics.ws_code === 1006);
  assert.equal(closed.diagnostics.route, "direct");
  assert.equal(closed.diagnostics.phase, "socket_open");
  assert.match(closed.diagnostics.attempt_id, /^[0-9a-f]{32}$/);
  assert.equal(closed.diagnostics.elapsed_ms, 0);
  assert.equal(closed.build_id, "0123456789abcdef");
  assert.equal(closed.os, "ios");
  const request = h.requests[0];
  assert.equal(request.url, "https://errors.krill.page/v1/mobile-events");
  assert.equal(request.options.credentials, "omit");
  assert.equal(request.options.mode, "cors");
  assert.equal(request.options.referrerPolicy, "no-referrer");
  assert.equal(request.options.redirect, "error");
  assert.equal(request.options.headers["Content-Type"], "application/json");
  assert(request.options.signal);
  assert.doesNotMatch(
    JSON.stringify(events),
    /private-desktop|account-device|credential-secret|secret-qr|account-private-ua/,
  );
  t.close();
  if (process.env.MOBILE_DIAGNOSTICS_FIXTURE_PATH)
    fs.writeFileSync(
      process.env.MOBILE_DIAGNOSTICS_FIXTURE_PATH,
      JSON.stringify({ schema_version: 1, events }),
    );
});
for (const [stage, phase] of [
  "socket_open",
  "challenge",
  "authentication",
  "ipc",
  "snapshot",
].entries())
  test(`deadline reports stalled ${phase}`, async () => {
    const h = harness();
    h.diagnostics.setEnabled(true);
    const t = h.transport(),
      { pending } = await h.start(t, stage);
    await h.tick(20000);
    await pending;
    await h.tick();
    const e = h.events().find((e) => e.diagnostics.reason === "timeout");
    assert(e);
    assert.equal(e.diagnostics.phase, phase);
    assert.equal(e.diagnostics.elapsed_ms, 20000);
    assert.equal(
      e.error_code,
      stage === 2 ? "mobile_auth_failed" : "mobile_connect_failed",
    );
    t.close();
  });
for (const status of [401, 403, 503])
  test(`relay discovery preserves actual HTTP ${status} independently of opaque WS failure`, async () => {
    const h = harness();
    h.store.set(
      "krill-mobile-v2",
      JSON.stringify({
        device_id: "a".repeat(64),
        credential: "b".repeat(64),
        rid: "c".repeat(32),
        hash: "0123456789abcdef",
      }),
    );
    h.diagnostics.setEnabled(true);
    h.setDiscovery(async () => ({ ok: false, status }));
    const endpoint = h.context.KrillMobile.transport.bootstrap(),
      t = h.transport({ endpoint }),
      { pending, socket } = await h.start(t);
    socket.emit("error");
    await pending;
    await h.tick();
    const e = h
      .events()
      .find((e) => e.error_code === "mobile_discovery_failed");
    assert.equal(e.diagnostics.http_status, status);
    assert.equal(e.diagnostics.phase, "discovery");
    assert.equal(e.diagnostics.route, "relay");
    assert.equal(
      e.diagnostics.reason,
      status === 503 ? "http_error" : "auth_required",
    );
    assert(
      h
        .events()
        .filter((e) => e.diagnostics.reason === "websocket_error")
        .every((e) => e.diagnostics.http_status === undefined),
    );
    t.close();
  });
test("discovery timeout has no invented HTTP status; cancellation sends no discovery failure", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const trace = h.diagnostics.attempt();
  h.store.set(
    "krill-mobile-v2",
    JSON.stringify({ device_id: "a", credential: "b", rid: "c".repeat(32) }),
  );
  const endpoint = h.context.KrillMobile.transport.bootstrap();
  h.setDiscovery(
    (_url, { signal }) =>
      new Promise((_, reject) =>
        signal.addEventListener("abort", () =>
          reject(Error("secret network address")),
        ),
      ),
  );
  const pending = endpoint.locate({ diagnosticsTrace: trace });
  await h.tick(5000);
  await pending;
  await h.tick();
  const e = h.events().find((e) => e.error_code === "mobile_discovery_failed");
  assert.equal(e.diagnostics.reason, "timeout");
  assert.equal(e.diagnostics.http_status, undefined);
  const count = h.events().length,
    controller = new AbortController(),
    cancel = endpoint.locate({
      diagnosticsTrace: h.diagnostics.attempt(),
      signal: controller.signal,
    });
  controller.abort();
  await cancel;
  await h.tick();
  assert.equal(h.events().length, count);
});
test("successful discovery fallback is reflected in the next socket route", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  h.store.set(
    "krill-mobile-v2",
    JSON.stringify({
      device_id: "a",
      credential: "b",
      rid: "c".repeat(32),
      hash: "0123456789abcdef",
    }),
  );
  h.setDiscovery(async () => ({
    ok: true,
    status: 200,
    json: async () => ({
      hash: "0123456789abcdef",
      url: "https://private.trycloudflare.com",
    }),
  }));
  const endpoint = h.context.KrillMobile.transport.bootstrap(),
    t = h.transport({ endpoint }),
    first = await h.start(t);
  first.socket.emit("error");
  await first.pending;
  await h.tick();
  await h.tick();
  const next = h.sockets.at(-1);
  assert.match(next.url, /private.trycloudflare.com/);
  next.emit("close", { code: 1006 });
  await flush();
  await h.tick();
  assert(
    h
      .events()
      .some(
        (e) =>
          e.diagnostics.route === "tunnel" && e.diagnostics.ws_code === 1006,
      ),
  );
  t.close();
});
for (const code of [1008, 4009])
  test(`auth close ${code} records authentication rejection`, async () => {
    const h = harness();
    h.diagnostics.setEnabled(true);
    const t = h.transport(),
      { pending, socket } = await h.start(t, 2);
    socket.emit("close", { code });
    await pending;
    await h.tick();
    assert(
      h
        .events()
        .some(
          (e) =>
            e.error_code === "mobile_auth_failed" &&
            e.diagnostics.phase === "authentication" &&
            e.diagnostics.reason === "auth_rejected" &&
            e.diagnostics.ws_code === code,
        ),
    );
    t.close();
  });
test("RPC rejection includes no request, response, error message or terminal content", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  h.response(socket, { sessions: [] });
  await pending;
  const rpc = t.rpc({ action: "input", text: "terminal-private-content" });
  await flush();
  h.response(socket, {}, false);
  await assert.rejects(rpc);
  await h.tick();
  assert(
    h
      .events()
      .some(
        (e) =>
          e.error_code === "mobile_rpc_failed" && e.diagnostics.phase === "rpc",
      ),
  );
  assert.doesNotMatch(
    JSON.stringify(h.events()),
    /terminal-private-content|device_id|credential/,
  );
  t.close();
});
test("tailscale route is an enum without private address leakage", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport({ url: "ws://100.100.2.3:9898/ws" }),
    { pending, socket } = await h.start(t);
  socket.emit("close", { code: 1006 });
  await pending;
  await h.tick();
  assert(h.events().some((e) => e.diagnostics.route === "tailscale"));
  assert.doesNotMatch(JSON.stringify(h.events()), /100.100.2.3/);
  t.close();
});
test("global runtime, rejection, resource hooks sanitize exception names, asset files and stack frames", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const base = "https://krill-mobile.pages.dev/m/0123456789abcdef/";
  h.emit("error", {
    error: {
      name: "TypeError",
      message: "raw-secret",
      stack: `TypeError: raw-secret\n at privateName (${base}app.js?token=secret:12:34)\nprivateName@${base}screens/settings.js:2:3\n at secret (https://evil.test/app.js:1:2)\n at privateName (${base}private-account.js:2:3)`,
    },
    filename: base + "app.js?auth=secret",
    lineno: 12,
    colno: 34,
  });
  h.emit("unhandledrejection", {
    reason: {
      name: "CustomSecretError",
      message: "secret",
      stack: `private@${base}client.js:42:5`,
    },
  });
  h.emit("error", { target: { src: base + "screens/chat.js?secret" } });
  h.emit("error", { target: { href: base + "app.css?secret" } });
  h.emit("error", {
    target: { src: "https://errors.krill.page/v1/mobile-events" },
  });
  await h.tick();
  const events = h.events();
  assert.equal(events.length, 4);
  assert.deepEqual(events[0].diagnostics.stack_frames, [
    { source_file: "app.js", line: 12, column: 34 },
    { source_file: "screens/settings.js", line: 2, column: 3 },
  ]);
  assert.equal(events[1].diagnostics.error_name, "Unknown");
  assert.equal(events[2].error_code, "mobile_asset_failed");
  assert.equal(events[3].error_code, "mobile_asset_failed");
  assert.equal(events[3].diagnostics.source_file, undefined);
  assert.doesNotMatch(
    JSON.stringify(events),
    /raw-secret|privateName|token|https:|evil|CustomSecretError/,
  );
});
test("bounded retry deduplicates event IDs, collector failures never recurse, ACK duplicates settle", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  h.setCollector(async () => {
    throw Error("collector-secret");
  });
  event(h);
  event(h);
  await h.tick();
  assert.equal(h.events().length, 1);
  const first = h.events()[0].event_id;
  await h.tick(2000);
  assert.equal(h.events()[1].event_id, first);
  h.setCollector(async () => ({
    ok: true,
    json: async () => ({
      schema_version: 1,
      accepted_ids: [],
      duplicate_ids: [first],
    }),
  }));
  await h.tick(4000);
  assert.equal(h.store.has(OUTBOX), false);
  await h.tick(60000);
  assert.equal(h.requests.length, 3);
  assert.equal(h.diagnostics.recent.length, 2);
});
test("reports and identifiers stay in memory; reload discards legacy disk outbox without sending it", async () => {
  const h = harness();
  h.setCollector(async () => {
    throw Error();
  });
  event(h, {
    page_id: "a".repeat(32),
    stack_frames: [{ source_file: "app.js", line: 2, column: 3 }],
  });
  assert.equal(h.store.has(OUTBOX), false);
  await h.tick();
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.page_id, undefined);
  assert.deepEqual([...h.store.keys()], [KEY]);
  // A legacy persisted report must be deleted, even if it is otherwise valid.
  h.store.set(
    OUTBOX,
    JSON.stringify({
      schema_version: 1,
      items: [{ event: h.events()[0], at: 2000000, tries: 0 }],
      retry_at: 2000000,
    }),
  );
  const reloaded = harness({ store: h.store });
  assert.equal(reloaded.store.has(OUTBOX), false);
  await reloaded.tick(2000);
  assert.equal(reloaded.events().length, 0);
  assert.equal(reloaded.timers.size, 0);
});
test("queue max32; event4KiB, request64KiB, breadcrumbs12, stack8 and closed fields", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  for (let i = 0; i < 40; i++)
    event(h, {
      line: i,
      message: "secret",
      stack: "secret",
      url: "https://secret/",
      ws_code: 999,
      http_status: 999,
      attempt_id: "not-hex",
      breadcrumbs: Array.from({ length: 20 }, () => ({
        phase: "rpc",
        reason: "timeout",
        elapsed_ms: 2,
        secret: "secret",
      })),
      stack_frames: Array.from({ length: 10 }, () => ({
        source_file: "app.js",
        line: 1,
        column: 2,
        secret: "secret",
      })),
    });
  assert.equal(h.store.has(OUTBOX), false);
  await h.tick();
  await h.tick(2000);
  assert.equal(h.events().length, 32);
  for (const req of h.requests) {
    assert(req.body.events.length <= 16);
    assert(Buffer.byteLength(req.options.body) <= 65536);
    for (const e of req.body.events) {
      assert(Buffer.byteLength(JSON.stringify(e)) <= 4096);
      assert.equal(e.diagnostics.breadcrumbs.length, 12);
      assert.equal(e.diagnostics.stack_frames.length, 8);
      assert.equal(e.diagnostics.ws_code, undefined);
      assert.equal(e.diagnostics.http_status, undefined);
      assert.equal(e.diagnostics.attempt_id, undefined);
    }
  }
  assert.doesNotMatch(JSON.stringify(h.events()), /secret|https:/);
});
test("withdraw aborts active request and clears memory queue/retry; late ACK cannot resurrect it", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  let resolve, signal;
  h.setCollector((_url, options) => {
    signal = options.signal;
    return new Promise((done) => (resolve = done));
  });
  event(h);
  await h.tick();
  assert.equal(h.requests.length, 1);
  h.diagnostics.setEnabled(false);
  assert.equal(signal.aborted, true);
  assert.equal(h.store.has(OUTBOX), false);
  resolve({
    ok: true,
    json: async () => ({
      schema_version: 1,
      accepted_ids: [],
      duplicate_ids: [],
    }),
  });
  await flush();
  await h.tick(60000);
  assert.equal(h.requests.length, 1);
  assert.equal(h.timers.size, 0);
  h.diagnostics.setEnabled(true);
  await h.tick();
  assert.equal(h.requests.length, 1);
});
test("cross-tab opt-out aborts active request and clears outbox; quota failures keep UI usable", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  let signal;
  h.setCollector(
    (_url, o) =>
      new Promise((_, reject) => {
        signal = o.signal;
        signal.addEventListener("abort", () => reject(Error()));
      }),
  );
  event(h);
  await h.tick();
  h.store.set(KEY, "off");
  h.emit("storage", { key: KEY });
  await flush();
  assert(signal.aborted);
  assert.equal(h.store.has(OUTBOX), false);
  await h.tick(60000);
  assert.equal(h.requests.length, 1);
  const q = harness({ quota: true });
  assert.doesNotThrow(() => q.diagnostics.setEnabled(true));
  assert.doesNotThrow(() => event(q));
  await q.tick();
  assert.equal(q.events().length, 0);
  assert.doesNotThrow(() => q.diagnostics.setEnabled(false));
});
test("collector timeout aborts only diagnostics and retry budget terminates", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  h.setCollector(
    (_url, o) =>
      new Promise((_, reject) =>
        o.signal.addEventListener("abort", () => reject(Error())),
      ),
  );
  event(h);
  await h.tick();
  for (let i = 0; i < 4; i++) {
    await h.tick(8000);
    await h.tick(16000);
  }
  assert.equal(h.requests.length, 4);
  await h.tick(60000);
  assert.equal(h.timers.size, 0);
  assert.equal(h.store.has(OUTBOX), false);
  assert.equal(h.diagnostics.recent.length, 1);
});

test("disconnected Settings control uses automatic diagnostics directly, with English/Korean and privacy signal state", () => {
  for (const signal of [{}, { dnt: "1" }, { gpc: true }])
    for (const lang of ["en", "ko"]) {
      const h = harness(signal),
        blocked = signal.dnt === "1" || signal.gpc === true;
      vm.runInContext(read("i18n.js"), h.context);
      h.context.KrillMobile.i18n.configure(lang);
      vm.runInContext(read("screens/settings.js"), h.context);
      class Element {
        constructor(tag, text, cls) {
          this.tag = tag;
          this.text = text || "";
          this.children = [];
          this.attrs = {};
          this.dataset = {};
          this.listeners = {};
          this.classList = { add() {} };
        }
        append(...nodes) {
          this.children.push(...nodes);
        }
        setAttribute(k, v) {
          this.attrs[k] = v;
        }
        addEventListener(k, f) {
          this.listeners[k] = f;
        }
      }
      const el = (...a) => new Element(...a),
        root = el("main"),
        ctx = {
          el,
          state: {
            preferences: {},
            connection: { phase: "disconnected" },
            snapshot: null,
          },
          route: {},
          header: () => el("header"),
          t: h.context.KrillMobile.i18n.t,
          button: (label) => el("button", label),
          linkRow: (label) => el("a", label),
          icon: () => el("svg"),
          savePreferences: () => {
            throw Error("must not need desktop or general preferences");
          },
        };
      h.context.KrillMobile.screens.settings(root, ctx);
      const descendants = (node) => [
          node,
          ...node.children.flatMap(descendants),
        ],
        nodes = descendants(root),
        toggle = nodes.find((n) => n.dataset.focusKey === "error-reporting");
      assert(toggle);
      assert.equal(toggle.checked, !blocked);
      assert.equal(toggle.disabled, blocked);
      assert.equal(
        toggle.attrs["aria-label"],
        lang === "ko"
          ? "기술적 오류 자동 수집"
          : "Automatic technical error collection",
      );
      const notice = nodes.find(
        (n) => n.href === "https://krill.page/privacy#mobile-errors",
      );
      assert(notice);
      assert.equal(
        notice.text,
        lang === "ko" ? "오류 수집 안내" : "Error collection notice",
      );
      assert.equal(notice.referrerPolicy, "no-referrer");
      assert.equal(notice.rel, "noreferrer");
      const about = el("main");
      h.context.KrillMobile.screens.settings(about, {
        ...ctx,
        route: { subpage: "about" },
      });
      const aboutNodes = descendants(about);
      assert(
        aboutNodes.some(
          (n) => n.href === "https://krill.page/privacy#mobile-errors",
        ),
      );
      assert(
        aboutNodes.some((n) =>
          n.text.includes(lang === "ko" ? "메모리에만" : "only in memory"),
        ),
      );
      if (!blocked) {
        toggle.checked = true;
        toggle.listeners.change();
        assert.equal(h.diagnostics.enabled(), true);
        assert.equal(h.store.get(KEY), "1");
        toggle.checked = false;
        toggle.listeners.change();
        assert.equal(h.diagnostics.enabled(), false);
        assert.equal(h.store.get(KEY), "");
      }
    }
});

test("recent diagnostics cannot mutate queued reports; DNT reload never restores prior errors", async () => {
  const h = harness();
  event(h, { stack_frames: [{ source_file: "app.js", line: 1, column: 2 }] });
  h.diagnostics.recent[0].stack_frames[0].source_file = "private-secret";
  await h.tick();
  assert.equal(h.events()[0].diagnostics.stack_frames[0].source_file, "app.js");
  assert.doesNotMatch(JSON.stringify(h.events()), /private-secret/);
  event(h, { line: 2 });
  assert.equal(h.store.has(OUTBOX), false);
  const off = harness({ store: h.store, dnt: "1" });
  await off.tick();
  assert.equal(off.events().length, 0);
});

test("late close preserves an observable server cooldown code after opaque onerror", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t);
  socket.emit("error");
  await pending;
  socket.emit("close", { code: 1013 });
  await h.tick();
  assert(
    h
      .events()
      .some(
        (e) =>
          e.diagnostics.ws_code === 1013 &&
          e.diagnostics.phase === "socket_open",
      ),
  );
  assert.equal(
    h.events().filter((e) => e.diagnostics.reason === "websocket_error").length,
    1,
  );
  t.close();
});

test("429 Retry-After holds the memory retry budget; reload cannot restore reports or cooldown", async () => {
  const h = harness();
  h.setCollector(async () => ({
    ok: false,
    status: 429,
    headers: { get: () => "60" },
  }));
  event(h);
  await h.tick();
  assert.equal(h.requests.length, 1);
  assert.equal(h.store.has(OUTBOX), false);
  await h.tick(59000);
  assert.equal(h.requests.length, 1);
  const reloaded = harness({ store: h.store, initialNow: 2059000 });
  await reloaded.tick(60000);
  assert.equal(reloaded.requests.length, 0);
  await h.tick(1000);
  assert.equal(h.requests.length, 2);
  assert.equal(h.events()[0].event_id, h.events()[1].event_id);
  await h.tick(60000);
  await h.tick(60000);
  await h.tick(60000);
  assert.equal(h.requests.length, 4);
  assert.equal(h.timers.size, 0);
});
test("partial ACK removes only accepted reports and Retry-After delays the remainder and newly queued errors", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  h.setCollector(async (_url, o) => ({
    ok: true,
    headers: { get: () => "60" },
    json: async () => ({
      schema_version: 1,
      accepted_ids: [JSON.parse(o.body).events[0].event_id],
      duplicate_ids: [],
    }),
  }));
  event(h, { line: 1 });
  event(h, { line: 2 });
  await h.tick();
  const first = h.events().map((e) => e.event_id);
  assert.equal(h.store.has(OUTBOX), false);
  event(h, { line: 3 });
  await h.tick(59000);
  assert.equal(h.requests.length, 1);
  h.setCollector(async (_url, o) => ({
    ok: true,
    json: async () => ({
      schema_version: 1,
      accepted_ids: JSON.parse(o.body).events.map((e) => e.event_id),
      duplicate_ids: [],
    }),
  }));
  await h.tick(1000);
  assert.equal(h.requests.length, 2);
  assert.equal(h.requests[1].body.events.length, 2);
  assert(h.requests[1].body.events.every((e) => e.event_id !== first[0]));
  assert.equal(h.store.has(OUTBOX), false);
});
for (const header of ["-1", "NaN", "86401", "Infinity", "0.5"])
  test(`invalid Retry-After ${header} cannot create an unbounded cooldown`, async () => {
    const h = harness();
    h.diagnostics.setEnabled(true);
    h.setCollector(async () => ({ ok: false, headers: { get: () => header } }));
    event(h);
    await h.tick();
    await h.tick(2000);
    assert.equal(h.requests.length, 2);
  });
test("long Retry-After expires reports at TTL instead of retaining or sending them after expiry", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  h.setCollector(async () => ({ ok: false, headers: { get: () => "86400" } }));
  event(h);
  await h.tick();
  await h.tick(1799999);
  assert.equal(h.requests.length, 1);
  await h.tick(1);
  assert.equal(h.requests.length, 1);
  assert.equal(h.store.has(OUTBOX), false);
  assert.equal(h.timers.size, 0);
});

test("full origin quota permits opt-out after default-on reserve and legacy on migration; reload and other tabs stay off", async () => {
  for (const saved of [undefined, "on", "1"]) {
    const store = new Map([["unrelated", "private-pairing-data"]]);
    if (saved !== undefined) store.set(KEY, saved);
    const h = harness({ store });
    // Fill the origin after automatic collection reserved its tiny preference.
    const capacity = [...store].reduce(
      (n, [k, v]) => n + k.length + v.length,
      0,
    );
    const tab = harness({ store, capacity });
    assert.equal(tab.diagnostics.enabled(), true);
    tab.diagnostics.setEnabled(false);
    assert.equal(store.get(KEY), "");
    assert.equal(store.get("unrelated"), "private-pairing-data");
    assert.equal(harness({ store, capacity }).diagnostics.enabled(), false);
    h.emit("storage", { key: KEY });
    assert.equal(h.diagnostics.enabled(), false);
    event(h);
    await h.tick();
    assert.equal(h.requests.length, 0);
  }
});
test("already-full origin with no preference fails closed and preserves unrelated storage across reload", async () => {
  const store = new Map([["unrelated", "private"]]);
  for (let n = 0; n < 2; n++) {
    const h = harness({ store, capacity: 16 });
    assert.equal(h.diagnostics.enabled(), false);
    h.diagnostics.setEnabled(false);
    event(h);
    await h.tick();
    assert.equal(h.requests.length, 0);
    assert.equal(store.get("unrelated"), "private");
  }
});
test("only diagnostics outbox may be reclaimed to reserve preference on a full origin", () => {
  const store = new Map([
    ["unrelated", "private"],
    [OUTBOX, "x".repeat(100)],
  ]);
  const h = harness({ store, capacity: 160 });
  assert.equal(h.diagnostics.enabled(), true);
  h.diagnostics.setEnabled(false);
  assert.equal(store.get(KEY), "");
  assert.equal(store.get("unrelated"), "private");
  assert.equal(store.has(OUTBOX), false);
});
test("blocked storage never enables automatic or explicit collection", async () => {
  const h = harness({ quota: true });
  assert.equal(h.diagnostics.enabled(), false);
  assert.equal(h.diagnostics.setEnabled(true), false);
  event(h);
  await h.tick();
  assert.equal(h.requests.length, 0);
});

test("global DNT disables collection and a changed GPC signal drops queued reports before send", async () => {
  const h = harness();
  h.context.doNotTrack = "1";
  event(h);
  await h.tick();
  assert.equal(h.requests.length, 0);
  h.context.doNotTrack = "0";
  event(h, { line: 2 });
  assert.equal(h.store.has(OUTBOX), false);
  h.context.navigator.globalPrivacyControl = true;
  await h.tick();
  assert.equal(h.requests.length, 0);
  assert.equal(h.store.has(OUTBOX), false);
});
test("cross-tab saved legacy off is migrated and queue is cleared, while explicit re-enable does not replay", async () => {
  const h = harness();
  event(h);
  h.store.set(KEY, "off");
  h.emit("storage", { key: KEY });
  assert.equal(h.store.get(KEY), "");
  assert.equal(h.store.has(OUTBOX), false);
  await h.tick();
  assert.equal(h.requests.length, 0);
  h.store.set(KEY, "on");
  h.emit("storage", { key: KEY });
  assert.equal(h.diagnostics.enabled(), true);
  assert.equal(h.store.get(KEY), "1");
  await h.tick();
  assert.equal(h.requests.length, 0);
});
