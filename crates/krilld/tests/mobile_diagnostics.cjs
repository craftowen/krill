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
  OUTBOX = "krill-mobile-error-outbox-v1",
  settle = async (h) => {
    await h.tick(10000);
    await h.tick();
  };
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
    docListeners = {},
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
      document: {
        visibilityState: "visible",
        hidden: false,
        addEventListener: (n, fn) => (docListeners[n] ||= []).push(fn),
        removeEventListener: (n, fn) => {
          docListeners[n] = (docListeners[n] || []).filter((x) => x !== fn);
        },
      },
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
    emitDoc: (name, event = {}) => {
      for (const fn of [...(docListeners[name] || [])]) fn(event);
    },
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
for (const href of [
  "http://127.0.0.1:9898/m/0123456789abcdef/",
  "http://localhost:9898/m/0123456789abcdef/",
  "http://[::1]:9898/m/0123456789abcdef/",
])
  test(`loopback origin ${href} never uploads mobile diagnostics`, async () => {
    const h = harness({ href });
    assert.equal(h.diagnostics.enabled(), false);
    assert.equal(h.diagnostics.setEnabled(true), false);
    event(h);
    await h.tick();
    assert.equal(h.requests.length, 0);
  });
test("error then close1006 files one report before desktop auth, preserves route and actual code", async () => {
  const h = harness();
  const t = h.transport(),
    { pending, socket } = await h.start(t);
  socket.emit("error");
  await pending;
  socket.emit("close", { code: 1006 });
  await settle(h);
  const events = h.events();
  assert.equal(events.length, 1);
  const closed = events[0];
  assert.equal(closed.diagnostics.ws_code, 1006);
  assert(
    closed.diagnostics.breadcrumbs.some((c) => c.reason === "websocket_error"),
  );
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
    await settle(h);
    const e = h.events().find((e) => e.diagnostics.reason === "timeout");
    assert(e);
    assert.equal(e.diagnostics.phase, phase);
    assert.equal(e.diagnostics.elapsed_ms, 20000);
    assert.equal(e.error_code, "mobile_connect_failed");
    assert.equal(t.blocked, false);
    assert(t.state.retryAt < h.context.Date.now() + 20000);
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
    await settle(h);
    assert.equal(h.events().length, 1);
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
  await settle(h);
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
  await settle(h);
  assert.equal(h.events().length, count);
});
for (const code of [1008, 4009])
  test(`auth close ${code} records authentication rejection`, async () => {
    const h = harness();
    h.diagnostics.setEnabled(true);
    const t = h.transport(),
      { pending, socket } = await h.start(t, 2);
    socket.emit("close", { code });
    await pending;
    await settle(h);
    assert.equal(h.events().length, 1);
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
test("desktop ok:false reply is an application answer, not a transport report", async () => {
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
  await settle(h);
  assert.equal(h.events().length, 0);
  t.close();
});
test("RPC timeout files one report with no request, follow-on or own-close duplicates", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  socket.close = () =>
    Promise.resolve().then(() => socket.emit("close", { code: 1000 }));
  h.response(socket, { sessions: [] });
  await pending;
  const first = t.rpc({ action: "input", text: "terminal-private-content" }),
    queued = t.rpc({ action: "git" });
  await flush();
  await h.tick(15000);
  await assert.rejects(first);
  await h.tick(0);
  await establish(h, h.sockets.at(-1));
  h.response(h.sockets.at(-1), { files: [] });
  await queued;
  await settle(h);
  const events = h.events();
  assert.equal(events.length, 1);
  assert.equal(events[0].error_code, "mobile_rpc_failed");
  assert.equal(events[0].diagnostics.phase, "rpc");
  assert.equal(events[0].diagnostics.reason, "timeout");
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
  await settle(h);
  assert(h.events().some((e) => e.diagnostics.route === "tailscale"));
  assert.doesNotMatch(JSON.stringify(h.events()), /100.100.2.3/);
  t.close();
});
test("MagicDNS ts.net host is labeled tailscale", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport({ url: "wss://desktop.tail123.ts.net/ws" }),
    { pending, socket } = await h.start(t);
  socket.emit("close", { code: 1006 });
  await pending;
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.route, "tailscale");
  assert.doesNotMatch(JSON.stringify(h.events()), /tail123/);
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
          // The control lives on the Settings → Privacy page (Orca-style settings menu).
          route: { subpage: "privacy" },
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
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.ws_code, 1013);
  assert.equal(h.events()[0].diagnostics.phase, "socket_open");
  assert(
    h.events()[0].diagnostics.breadcrumbs.some(
      (c) => c.reason === "websocket_error",
    ),
  );
  t.close();
});

async function establish(h, socket) {
  for (const data of [
    undefined,
    JSON.stringify({ version: 2, challenge: "challenge-secret" }),
    enc.encode("{}"),
    Uint8Array.from([0, 0, 0, 0, 1]),
  ]) {
    socket.emit(data === undefined ? "open" : "message", { data });
    await flush();
  }
  h.response(socket, { sessions: [] });
  await flush();
}
async function fail(h) {
  const socket = h.sockets.at(-1);
  socket.emit("error");
  socket.emit("close", { code: 1006 });
  await flush();
}
const relayHost = (h) =>
  h.store.set(
    "krill-mobile-v2",
    JSON.stringify({
      device_id: "a".repeat(64),
      credential: "b".repeat(64),
      rid: "c".repeat(32),
      hash: "0123456789abcdef",
    }),
  );
test("an outage across automatic retries files one report carrying the retries as breadcrumbs", async () => {
  const h = harness();
  const t = h.transport();
  t.connect();
  await flush();
  for (const delay of [1000, 2000, 4000, 8000, 15000, 15000, 15000]) {
    const sockets = h.sockets.length;
    await fail(h);
    await h.tick(delay);
    assert.equal(h.sockets.length, sockets + 1, "retried");
  }
  await settle(h);
  const events = h.events();
  assert.equal(events.length, 1);
  assert.equal(events[0].diagnostics.ws_code, 1006);
  const crumbs = events[0].diagnostics.breadcrumbs;
  assert(crumbs.length <= 12);
  assert.equal(crumbs.filter((c) => c.ws_code === 1006).length, 4);
  t.close();
});
test("a failure that recovers inside the hold window files nothing", async () => {
  const h = harness();
  const t = h.transport();
  t.connect();
  await flush();
  await fail(h);
  await h.tick(1000);
  await establish(h, h.sockets.at(-1));
  await h.tick(60000);
  assert.equal(h.events().length, 0);
  t.close();
});
test("a failure while hidden is held and dropped when the connection recovers on return", async () => {
  const h = harness();
  h.context.document.visibilityState = "hidden";
  const t = h.transport();
  t.connect();
  await flush();
  await fail(h);
  for (let i = 0; i < 6; i++) await h.tick(60000);
  assert.equal(h.events().length, 0);
  h.context.document.visibilityState = "visible";
  t.reconnect();
  await flush();
  await establish(h, h.sockets.at(-1));
  await h.tick(60000);
  assert.equal(h.events().length, 0);
  t.close();
});
test("an overdue hold timer from a frozen hidden page waits for recovery after return", async () => {
  const h = harness();
  h.context.document.visibilityState = "hidden";
  const t = h.transport();
  t.connect();
  await flush();
  await fail(h);
  h.context.document.visibilityState = "visible";
  await h.tick(600000);
  t.reconnect();
  await flush();
  await establish(h, h.sockets.at(-1));
  await settle(h);
  assert.equal(h.events().length, 0);
  t.close();
});
test("a failure while hidden that persists after return files one visible report", async () => {
  const h = harness();
  h.context.document.visibilityState = "hidden";
  const t = h.transport();
  t.connect();
  await flush();
  await fail(h);
  await h.tick(60000);
  assert.equal(h.events().length, 0);
  h.context.document.visibilityState = "visible";
  for (let i = 0; i < 4; i++) await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.visibility, "visible");
  t.close();
});
test("connect deadline and our own close file one timeout report", async () => {
  const h = harness();
  const t = h.transport(),
    { pending, socket } = await h.start(t, 1);
  socket.close = () =>
    Promise.resolve().then(() => socket.emit("close", { code: 1000 }));
  await h.tick(20000);
  await pending;
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.reason, "timeout");
  t.close();
});
test("a drop after a long-lived connection still files one report", async () => {
  const h = harness();
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  h.response(socket, { sessions: [] });
  await pending;
  await h.tick(360000);
  socket.emit("close", { code: 1006 });
  await flush();
  await settle(h);
  const events = h.events();
  assert.equal(events.length, 1);
  assert.equal(events[0].error_code, "mobile_connect_failed");
  assert.equal(events[0].diagnostics.phase, "ipc");
  assert.equal(events[0].diagnostics.ws_code, 1006);
  assert(events[0].diagnostics.elapsed_ms < 20000);
  t.close();
});
test("relay outage with unreachable discovery files one report", async () => {
  const h = harness();
  relayHost(h);
  h.setDiscovery(async () => {
    throw new TypeError("Load failed");
  });
  const endpoint = h.context.KrillMobile.transport.bootstrap(),
    t = h.transport({ endpoint }),
    { pending } = await h.start(t);
  await fail(h);
  await pending;
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.ws_code, 1006);
  t.close();
});
test("relay discovery 403 after an opaque socket failure is the one reported cause", async () => {
  const h = harness();
  relayHost(h);
  h.setDiscovery(async () => ({ ok: false, status: 403 }));
  const endpoint = h.context.KrillMobile.transport.bootstrap(),
    t = h.transport({ endpoint }),
    { pending } = await h.start(t);
  await fail(h);
  await pending;
  await settle(h);
  const events = h.events();
  assert.equal(events.length, 1);
  assert.equal(events[0].error_code, "mobile_discovery_failed");
  assert.equal(events[0].diagnostics.reason, "auth_required");
  assert.equal(events[0].diagnostics.http_status, 403);
  assert(events[0].diagnostics.breadcrumbs.some((c) => c.ws_code === 1006));
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
test("a read whose socket died is retried on the next connection and files no report", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  h.response(socket, { sessions: [] });
  await pending;
  const read = t.rpc({ action: "git" });
  read.catch(() => {});
  await flush();
  await h.tick(15000);
  await h.tick(0);
  await establish(h, h.sockets.at(-1));
  h.response(h.sockets.at(-1), { files: ["retried"] });
  assert.equal((await read).files[0], "retried");
  await settle(h);
  assert.equal(h.events().length, 0);
  t.close();
});
test("slow challenge at 19980ms completes at 20500ms without timeout report while genuine stage-2 timeout files mobile_connect_failed without lockout", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 1);
  await h.tick(19980);
  socket.emit("message", {
    data: JSON.stringify({ version: 2, challenge: "challenge-secret" }),
  });
  await flush();
  await h.tick(520);
  socket.emit("message", { data: enc.encode("{}") });
  await flush();
  socket.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 1]) });
  await flush();
  h.response(socket, { sessions: [] });
  await pending;
  assert.equal(t.ready, true);
  await settle(h);
  assert.equal(h.events().length, 0);
  t.close();

  const h2 = harness();
  h2.diagnostics.setEnabled(true);
  const t2 = h2.transport(),
    stalled = await h2.start(t2, 2);
  await h2.tick(20000);
  await stalled.pending;
  await settle(h2);
  const events = h2.events();
  assert.equal(events.length, 1);
  assert.equal(events[0].error_code, "mobile_connect_failed");
  assert.equal(events[0].diagnostics.phase, "authentication");
  assert.equal(events[0].diagnostics.reason, "timeout");
  assert.equal(t2.blocked, false);
  assert(t2.state.retryAt < h2.context.Date.now() + 20000);
  t2.close();
});
test("unsent RPC and hidden visibilitychange feed disarm file zero mobile_rpc_failed while mid-flight dropped mutation files websocket_closed", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t1 = h.transport(),
    s1 = await h.start(t1, 4);
  h.response(s1.socket, {
    sessions: [],
    etag: "e1",
    capabilities: { change_feed: true },
  });
  await s1.pending;
  await h.tick(2500);
  h.response(s1.socket, { unchanged: true, etag: "e1", feed: 2500 });
  await flush();
  h.context.document.hidden = true;
  h.context.document.visibilityState = "hidden";
  h.emitDoc("visibilitychange");
  await flush();
  const opened = h.sockets.length;
  await h.tick(5000);
  await h.tick(0);
  assert.equal(h.sockets.length, opened, "the background does not expire the feed disarm");
  assert.equal(t1.ready, true);
  h.context.document.hidden = false;
  h.context.document.visibilityState = "visible";
  h.emitDoc("visibilitychange");
  h.response(s1.socket, { unchanged: true, etag: "e1", feed: 0 });
  await settle(h);
  assert.equal(
    h.events().filter((e) => e.error_code === "mobile_rpc_failed").length,
    0,
  );
  t1.close();

  const t2 = h.transport(),
    s2 = await h.start(t2, 4);
  h.response(s2.socket, { sessions: [] });
  await s2.pending;
  s2.socket.emit("close", { code: 1006 });
  await flush();
  let unsentError;
  await t2.rpc({ action: "input", text: "never-sent" }).catch((e) => {
    unsentError = e;
  });
  assert.equal(unsentError.diagnosticReason, "websocket_closed");
  await h.tick(0);
  await establish(h, h.sockets.at(-1));
  await settle(h);
  assert.equal(
    h.events().filter((e) => e.error_code === "mobile_rpc_failed").length,
    0,
  );
  t2.close();

  const t3 = h.transport(),
    s3 = await h.start(t3, 4);
  h.response(s3.socket, { sessions: [] });
  await s3.pending;
  await h.tick(1000);
  const sent = t3.rpc({ action: "input", text: "mid-flight" }),
    queuedUnsent = t3.rpc({ action: "input", text: "queued-behind" });
  await flush();
  await h.tick(250);
  s3.socket.emit("close", { code: 1006 });
  await assert.rejects(sent);
  await assert.rejects(queuedUnsent);
  await h.tick(0);
  await establish(h, h.sockets.at(-1));
  await settle(h);
  const rpcEvents = h
    .events()
    .filter((e) => e.error_code === "mobile_rpc_failed");
  assert.equal(rpcEvents.length, 1);
  assert.equal(rpcEvents[0].diagnostics.reason, "websocket_closed");
  assert.equal(rpcEvents[0].diagnostics.elapsed_ms, 250);
  t3.close();

  const hProto = harness();
  hProto.diagnostics.setEnabled(true);
  const t4 = hProto.transport(),
    s4 = await hProto.start(t4, 4);
  hProto.response(s4.socket, { sessions: [] });
  await s4.pending;
  const bad = t4.rpc({ action: "git" });
  await flush();
  s4.socket.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 99]) });
  await assert.rejects(bad, /Unexpected daemon response/);
  await settle(hProto);
  assert.equal(hProto.events().length, 1);
  assert.equal(hProto.events()[0].error_code, "mobile_rpc_failed");
  assert.equal(hProto.events()[0].diagnostics.reason, "protocol_error");
  t4.close();
});
test("offline discovery reports network_offline and mark(discovery) http_status never leaks onto socket_open failure", async () => {
  const h = harness();
  relayHost(h);
  h.diagnostics.setEnabled(true);
  h.context.navigator.onLine = false;
  h.setDiscovery(async () => {
    throw new TypeError("Failed to fetch");
  });
  const endpoint = h.context.KrillMobile.transport.bootstrap();
  const traceA = h.diagnostics.attempt();
  await endpoint.locate({ diagnosticsTrace: traceA });
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].error_code, "mobile_discovery_failed");
  assert.equal(h.events()[0].diagnostics.reason, "network_offline");

  const peer = endpoint.forHost(endpoint.profiles()[0].id),
    traceB = h.diagnostics.attempt();
  await peer.locate({ diagnosticsTrace: traceB });
  await settle(h);
  assert.equal(h.events().length, 2);
  assert.equal(h.events()[1].diagnostics.reason, "network_offline");

  const h2 = harness();
  h2.diagnostics.setEnabled(true);
  const trace = h2.diagnostics.attempt();
  trace.mark("discovery", { route: "relay", http_status: 200 });
  trace.mark("socket_open", { route: "relay" });
  trace.report("mobile_connect_failed", { reason: "websocket_error" });
  await settle(h2);
  const [leakCheck] = h2.events();
  assert.equal(leakCheck.error_code, "mobile_connect_failed");
  assert.equal(leakCheck.diagnostics.phase, "socket_open");
  assert.equal(leakCheck.diagnostics.http_status, undefined);
  assert.equal(
    leakCheck.diagnostics.breadcrumbs.find((c) => c.phase === "discovery")
      ?.http_status,
    200,
  );
});
test("extension runtime errors, extension unhandled rejections and empty or base resource URLs emit zero reports", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const base = "https://krill-mobile.pages.dev/m/0123456789abcdef/";
  h.emit("error", {
    message: "Script error.",
    filename: "",
    lineno: 0,
    colno: 0,
    error: null,
  });
  h.emit("error", {
    filename: "chrome-extension://abcdef/inject.js",
    lineno: 3,
    colno: 9,
    error: {
      name: "TypeError",
      message: "ext",
      stack: "TypeError: ext\n at chrome-extension://abcdef/inject.js:3:9",
    },
  });
  h.emit("unhandledrejection", {
    reason: {
      name: "TypeError",
      message: "ext",
      stack: "TypeError: ext\n at chrome-extension://abcdef/bg.js:11:4",
    },
  });
  h.emit("unhandledrejection", {
    reason: {
      name: "Error",
      message: "ext",
      stack: "run@moz-extension://abcdef/bg.js:2:1",
    },
  });
  h.emit("error", { target: { src: "" } });
  h.emit("error", { target: { src: "   " } });
  h.emit("error", { target: { src: base } });
  await h.tick();
  assert.equal(h.events().length, 0);

  h.emit("error", {
    filename: "",
    lineno: 0,
    colno: 0,
    error: {
      name: "RangeError",
      message: "bad",
      stack: `RangeError: bad\n at fn (${base}client.js:77:8)`,
    },
  });
  await h.tick();
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].error_code, "mobile_runtime_error");
  assert.equal(h.events()[0].diagnostics.source_file, "client.js");
  assert.equal(h.events()[0].diagnostics.line, 77);
  assert.equal(h.events()[0].diagnostics.column, 8);
});

test("decorative icon and manifest load failures file nothing while the stylesheet still reports", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const base = "https://krill-mobile.pages.dev/m/0123456789abcdef/";
  for (const name of [
    "icon.svg",
    "icon-192.png",
    "icon-512.png",
    "icon-maskable-512.png",
    "apple-touch-icon.png",
    "manifest.json",
  ])
    h.emit("error", { target: { src: base + name, href: base + name } });
  await h.tick();
  assert.equal(h.events().length, 0);
  h.emit("error", { target: { href: base + "app.css" } });
  await h.tick();
  assert.deepEqual(
    h.events().map((event) => event.error_code),
    ["mobile_asset_failed"],
  );
});

test("mid-snapshot close reports snapshot phase and active retry reaching snapshot defers episode release until ok", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  socket.emit("close", { code: 1006 });
  await pending;
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.phase, "snapshot");
  assert.equal(h.events()[0].diagnostics.ws_code, 1006);
  t.close();

  const h2 = harness();
  h2.diagnostics.setEnabled(true);
  const t2 = h2.transport(),
    first = await h2.start(t2, 4);
  await h2.tick(20000);
  await first.pending;
  await h2.tick(1000);
  const retrySocket = h2.sockets.at(-1);
  for (const data of [
    undefined,
    JSON.stringify({ version: 2, challenge: "challenge-secret" }),
    enc.encode("{}"),
    Uint8Array.from([0, 0, 0, 0, 1]),
  ]) {
    retrySocket.emit(data === undefined ? "open" : "message", { data });
    await flush();
  }
  await h2.tick(15000);
  assert.equal(h2.events().length, 0);
  h2.response(retrySocket, { sessions: [] });
  await flush();
  await settle(h2);
  assert.equal(h2.events().length, 0);
  t2.close();
});

test("post-ok drop upgrades late close across generation bump, updates held report on retry and allows higher-weight auth report after flush", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  h.response(socket, { sessions: [] });
  await pending;
  socket.emit("error");
  await flush();
  await h.tick(0);
  socket.emit("close", { code: 1006 });
  await flush();
  await h.tick(50);
  await fail(h);
  await settle(h);
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].diagnostics.ws_code, 1006);
  assert.equal(h.events()[0].diagnostics.phase, "socket_open");
  assert.equal(h.events()[0].diagnostics.elapsed_ms, 50);

  const retrySocket = h.sockets.at(-1);
  retrySocket.emit("open");
  await flush();
  retrySocket.emit("message", {
    data: JSON.stringify({ version: 2, challenge: "challenge-secret" }),
  });
  await flush();
  retrySocket.emit("close", { code: 1008 });
  await flush();
  await settle(h);
  assert.equal(h.events().length, 2);
  assert.equal(h.events()[1].error_code, "mobile_auth_failed");
  assert.equal(h.events()[1].diagnostics.ws_code, 1008);
  t.close();
});

test("background sleep rebases attempt elapsed_ms instead of clamping wake breadcrumbs to 300000ms", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const trace = h.diagnostics.attempt();
  trace.mark("socket_open", { route: "relay" });
  h.context.document.visibilityState = "hidden";
  await h.tick(300000);
  h.context.document.visibilityState = "visible";
  trace.report("mobile_connect_failed", {
    reason: "websocket_closed",
    ws_code: 1006,
    route: "relay",
  });
  await settle(h);
  assert.equal(h.events().length, 1);
  assert(h.events()[0].diagnostics.elapsed_ms < 1000);
});

test("expanded read RPC actions retry across disconnects, serial queue starts deadline on send and keepalive or reconnected timeout files no duplicate rpc error", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const t = h.transport(),
    { pending, socket } = await h.start(t, 4);
  h.response(socket, {
    sessions: [],
    etag: "e1",
    capabilities: { change_feed: true },
  });
  await pending;

  for (const action of [
    "orchestration",
    "command_catalog",
    "mention_files",
    "composer_models",
  ]) {
    const p1 = t.rpcLatest(action, { action }),
      p2 = t.rpcLatest(action, { action });
    assert.equal(p1, p2);
    await flush();
    const active = h.sockets.at(-1);
    active.emit("close", { code: 1006 });
    await flush();
    await h.tick(0);
    await establish(h, h.sockets.at(-1));
    h.response(h.sockets.at(-1), { ok_action: action });
    assert.equal((await p1).ok_action, action);
    assert.equal(t.updateBusy, false);
  }

  const slow = t.rpc({ action: "git" }),
    behind = t.rpc({ action: "accounts" });
  await flush();
  await h.tick(12000);
  h.response(h.sockets.at(-1), { slow: true });
  assert.equal((await slow).slow, true);
  await flush();
  await h.tick(5000);
  h.response(h.sockets.at(-1), { behind: true });
  assert.equal((await behind).behind, true);

  const stalledRead = t.rpc({ action: "orchestration" });
  stalledRead.catch(() => {});
  await flush();
  h.sockets.at(-1).emit("close", { code: 1006 });
  await flush();
  await h.tick(20000);
  await assert.rejects(stalledRead);
  await settle(h);
  assert.equal(
    h.events().filter((e) => e.error_code === "mobile_rpc_failed").length,
    0,
  );
  t.close();
});

test("relay stage-2 transport closes do not trigger auth lockout and aborted locate records no discovery breadcrumb", async () => {
  const h = harness();
  relayHost(h);
  h.diagnostics.setEnabled(true);
  const endpoint = h.context.KrillMobile.transport.bootstrap(),
    t = h.transport({ endpoint });
  for (let i = 0; i < 3; i++) {
    const { pending, socket } = await h.start(t, 2);
    socket.emit("close", { code: 1011 });
    await pending;
    assert.equal(t.blocked, false);
    assert(t.state.retryAt < h.context.Date.now() + 20000);
    await h.tick(15000);
  }
  t.close();

  const trace = h.diagnostics.attempt(),
    controller = new AbortController();
  controller.abort();
  await endpoint.locate({ diagnosticsTrace: trace, signal: controller.signal });
  assert.equal(trace.episode.crumbs.length, 0);
});

test("document URL runtime errors map source_file to index.html", async () => {
  const h = harness();
  h.diagnostics.setEnabled(true);
  const base = "https://krill-mobile.pages.dev/m/0123456789abcdef/";
  h.emit("error", {
    filename: base,
    lineno: 76,
    colno: 5094,
    error: new TypeError("inline failure"),
  });
  await h.tick();
  assert.equal(h.events().length, 1);
  assert.equal(h.events()[0].error_code, "mobile_runtime_error");
  assert.equal(h.events()[0].diagnostics.source_file, "index.html");
  assert.equal(h.events()[0].diagnostics.line, 76);
  assert.equal(h.events()[0].diagnostics.column, 5094);
});
