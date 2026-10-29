// Deterministic transport contracts; no daemon, phone, network or real clock wait.
"use strict";
const test = require("node:test"),
  assert = require("node:assert/strict"),
  vm = require("node:vm"),
  fs = require("node:fs"),
  path = require("node:path");
const source = fs.readFileSync(
  path.join(__dirname, "../src/mobile/assets/client.js"),
  "utf8",
);
const enc = new TextEncoder(),
  flush = async () => {
    for (let i = 0; i < 30; i++) await Promise.resolve();
  };
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => (resolve = done));
  return { promise, resolve };
};

test("foreground resume keeps a healthy slow snapshot on its existing connection", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  h.transport.resume(false);
  await flush();
  await h.tick(2000);
  assert.equal(
    h.transport.ready,
    true,
    "a snapshot within the RPC deadline is still in flight",
  );
  assert.equal(socket.closed, undefined);
  h.response(socket, { sessions: [], etag: "fresh" });
  await flush();
  assert.equal(h.transport.ready, true);
  h.transport.close();
});
function harness(
  saved = null,
  href = "https://phone.test/m/0123456789abcdef/",
  useEndpoint = false,
) {
  let now = 100000,
    nextTimer = 0;
  const timers = new Map(),
    sockets = [],
    events = [],
    redirects = [],
    store = new Map(saved ? [["krill-mobile-v2", JSON.stringify(saved)]] : []),
    url = new URL(href);
  url.replace = (href) => redirects.push(href);
  class Socket {
    constructor(url) {
      this.url = url;
      this.handlers = {};
      this.sent = [];
      this.readyState = 0;
      sockets.push(this);
    }
    addEventListener(name, fn) {
      (this.handlers[name] ||= []).push(fn);
    }
    removeEventListener(name, fn) {
      this.handlers[name] = (this.handlers[name] || []).filter((v) => v !== fn);
    }
    emit(name, event = {}) {
      if (name === "open") this.readyState = 1;
      for (const fn of [...(this.handlers[name] || [])]) fn(event);
    }
    send(data) {
      this.sent.push(data);
    }
    close() {
      this.readyState = 3;
      this.closed = true;
    }
  }
  const C = {
    enc,
    concat: (...bytes) => Uint8Array.from(bytes.flatMap((b) => Array.from(b))),
    unhex: () => new Uint8Array(32),
    unbase64: () => new Uint8Array(32),
    hex: (b) => Buffer.from(b).toString("hex"),
    sha256: (b) =>
      new Uint8Array(
        require("node:crypto").createHash("sha256").update(b).digest(),
      ),
    Channel: class {
      seal(b) {
        return b;
      }
      open(b) {
        return b;
      }
    },
  };
  const context = vm.createContext({
    TextEncoder,
    TextDecoder,
    Uint8Array,
    Uint32Array,
    DataView,
    URL,
    URLSearchParams,
    AbortController,
    MkCrypto: C,
    WebSocket: Socket,
    Date: class extends Date {
      static now() {
        return now;
      }
    },
    crypto: { getRandomValues: (b) => b },
    location: url,
    history: { replaceState() {} },
    navigator: {},
    localStorage: {
      getItem: (key) => store.get(key) || null,
      setItem: (key, value) => store.set(key, value),
      removeItem: (key) => store.delete(key),
    },
    setTimeout: (fn, ms) => {
      const id = ++nextTimer;
      timers.set(id, { fn, at: now + ms });
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
  });
  vm.runInContext(source, context);
  const endpoint = useEndpoint
    ? context.KrillMobile.transport.bootstrap()
    : null;
  const transport = context.KrillMobile.transport.create({
    credentials: endpoint?.credentials || { device_id: "aa", credential: "bb" },
    endpoint,
    url: "wss://desktop.test/ws",
    onStatus: (s) => events.push(s),
  });
  async function tick(ms) {
    now += ms;
    for (const [id, timer] of [...timers])
      if (timer.at <= now) {
        timers.delete(id);
        timer.fn();
      }
    await flush();
  }
  async function advance(socket, stage = 4) {
    if (stage >= 1) {
      socket.emit("open");
      await flush();
    }
    if (stage >= 2) {
      socket.emit("message", {
        data: JSON.stringify({ version: 2, challenge: "aa" }),
      });
      await flush();
    }
    if (stage >= 3) {
      socket.emit("message", { data: enc.encode("{}") });
      await flush();
    }
    if (stage >= 4) {
      socket.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 1]) });
      await flush();
    }
  }
  async function start(stage = 0) {
    const pending = transport.connect();
    await flush();
    const socket = sockets.at(-1);
    await advance(socket, stage);
    return { pending, socket };
  }
  function response(socket, data, ok = true) {
    const bytes = enc.encode(
        JSON.stringify(
          ok ? { ok: true, data } : { ok: false, error: "request rejected" },
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
      data: Uint8Array.from([0, 0, 0, 0, 0x65, 6, ...length, ...bytes]),
    });
  }
  return {
    context,
    transport,
    endpoint,
    tick,
    start,
    advance,
    response,
    sockets,
    events,
    store,
    redirects,
  };
}
for (const [stage, label] of [
  "open",
  "challenge",
  "authentication",
  "server hello",
  "snapshot",
].entries())
  test(`total connect deadline covers stalled ${label}`, async () => {
    const h = harness(),
      { pending } = await h.start(stage);
    await h.tick(20000);
    await pending;
    assert.equal(h.transport.ready, false);
    assert.match(
      h.events.find((e) => e.message.includes("timed out"))?.message || "",
      /timed out/,
    );
    h.transport.close();
  });
test("RPC total deadline breaks serial queue and never resends a mutation", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  assert.equal(h.transport.ready, true);
  const before = socket.sent.length,
    first = h.transport.rpc({ action: "input", text: "once" }),
    second = h.transport.rpc({ action: "snapshot" });
  first.catch(() => {});
  second.catch(() => {});
  await flush();
  assert.equal(socket.sent.length, before + 1);
  await h.tick(15000);
  await assert.rejects(first, /timed out/);
  await assert.rejects(second);
  assert.equal(h.transport.ready, false);
  assert.equal(socket.sent.length, before + 1);
  h.transport.close();
});
test("updates wait for queued RPCs and uncertain mutation results until explicit reconnect", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  assert.equal(h.transport.updateBusy, false);
  const read = h.transport.rpc({ action: "snapshot" }),
    mutation = h.transport.rpc({ action: "prompt", text: "once" });
  mutation.catch(() => {});
  assert.equal(h.transport.updateBusy, true);
  await flush();
  h.response(socket, { sessions: [] });
  await read;
  await flush();
  assert.equal(h.transport.updateBusy, true);
  await h.tick(15000);
  await assert.rejects(mutation, /timed out/);
  assert.equal(h.transport.updateBusy, true);
  const count = socket.sent.length;
  h.transport.resume(true);
  await flush();
  const resumed = h.sockets.at(-1);
  await h.advance(resumed);
  h.response(resumed, { sessions: [] });
  await flush();
  assert.equal(h.transport.updateBusy, true);
  h.transport.reconnect();
  await flush();
  assert.equal(socket.sent.length, count);
  const reconnected = h.sockets.at(-1);
  await h.advance(reconnected);
  h.response(reconnected, { sessions: [] });
  await flush();
  assert.equal(h.transport.updateBusy, false);
  h.transport.close();
});
test("a definitive daemon rejection does not leave automatic updates blocked", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  const rejected = h.transport.rpc({ action: "prompt", text: "rejected" });
  await flush();
  h.response(socket, {}, false);
  await assert.rejects(rejected, /request rejected/);
  assert.equal(h.transport.updateBusy, false);
  h.transport.close();
});
for (const stage of [0, 1])
  test(`version mismatch at handshake stage ${stage} pauses retries for a safe update`, async () => {
    const h = harness(
      {
        device_id: "first",
        credential: "11".repeat(32),
        rid: "a".repeat(32),
        hash: "0123456789abcdef",
      },
      undefined,
      true,
    );
    let checks = 0;
    h.endpoint.setUpdateHandler(() => checks++);
    h.context.fetch = async () => ({
      ok: true,
      json: async () => ({ hash: "fedcba9876543210", relay: true, url: "" }),
    });
    const { pending, socket } = await h.start(stage);
    socket.emit("close", { code: 1012 });
    await pending;
    assert.equal(checks, 1);
    assert.equal(h.endpoint.updatePending, true);
    assert.equal(h.transport.updateBusy, false);
    assert.equal(h.transport.state.retryAt, 0);
    await h.tick(30000);
    assert.equal(h.sockets.length, 1);
    h.endpoint.clearUpdatePending();
    h.transport.resume();
    await flush();
    assert.equal(h.sockets.length, 2);
    h.transport.close();
  });
test("replacement cancellation restores saved destination and credential together", () => {
  const saved = {
      device_id: "a".repeat(64),
      credential: "b".repeat(64),
      rid: "c".repeat(32),
      url: "https://saved.trycloudflare.com",
    },
    h = harness(
      saved,
      "https://phone.test/m/0123456789abcdef/#pair=aa&key=bb&r=" +
        "d".repeat(32),
    ),
    endpoint = h.context.KrillMobile.transport.bootstrap();
  assert.equal(endpoint.replacing, true);
  assert.match(endpoint.socketUrl(), /\/r\/dddd/);
  const restored = endpoint.restoreSaved();
  assert.equal(restored.device_id, saved.device_id);
  assert.match(endpoint.socketUrl(), /\/r\/cccc/);
  assert.equal(JSON.parse(h.store.get("krill-mobile-v2")).rid, saved.rid);
});
test("device capacity is blocked with cleanup guidance rather than expired QR diagnosis", async () => {
  const h = harness(),
    { pending, socket } = await h.start(2);
  socket.emit("close", { code: 4009 });
  await pending;
  assert.equal(h.transport.blocked, true);
  assert.match(h.transport.state.message, /device limit/);
  assert.doesNotMatch(h.transport.state.message, /expired/);
  h.transport.close();
});
test("cooldown exposes retryAt and explicit reconnect preserves server backoff", async () => {
  const h = harness(),
    { pending, socket } = await h.start(2);
  socket.emit("close", { code: 1013 });
  await pending;
  const retryAt = h.transport.state.retryAt;
  assert.ok(retryAt > 100000);
  assert.equal(h.transport.blocked, false);
  h.transport.reconnect();
  await flush();
  assert.equal(h.sockets.length, 1);
  assert.ok(h.transport.state.retryAt >= retryAt);
  h.transport.close();
});
test("fresh pairing can recover a blocked transport", async () => {
  const h = harness(),
    { pending, socket } = await h.start(2);
  socket.emit("close", { code: 1008 });
  await pending;
  assert.equal(h.transport.blocked, true);
  h.transport.setPair({ ticket: "new", secret: "secret" });
  await flush();
  assert.equal(h.transport.blocked, false);
  assert.equal(h.sockets.length, 2);
  h.transport.close();
});
test("warm terminal negotiates TERM_MODES with the daemon", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  const warm = h.sockets.at(-1);
  assert.notEqual(warm, socket);
  warm.emit("open");
  warm.emit("message", {
    data: JSON.stringify({ version: 2, challenge: "aa" }),
  });
  const bytes = warm.sent[2];
  let at = 5;
  const read = () => {
    let n = 0n,
      shift = 0n,
      b;
    do {
      b = bytes[at++];
      n |= BigInt(b & 127) << shift;
      shift += 7n;
    } while (b & 128);
    return n;
  };
  assert.equal(read(), 1n);
  assert.equal(read(), 1n);
  assert.ok((read() & 8192n) !== 0n);
  h.transport.close();
});
test("retry during authentication cancels old generation and starts a new attempt", async () => {
  const h = harness(),
    { pending, socket } = await h.start(2);
  h.transport.reconnect();
  await flush();
  await pending;
  assert.equal(h.transport.blocked, false);
  assert.equal(h.sockets.length, 2);
  assert.equal(socket.closed, true);
  assert.equal(h.transport.state.phase, "connecting");
  h.transport.close();
});
test("late open from expired connection cannot consume or send on the next attempt", async () => {
  const h = harness(),
    { pending, socket } = await h.start(0);
  await h.tick(20000);
  await pending;
  h.transport.reconnect();
  await flush();
  const next = h.sockets.at(-1);
  socket.emit("open");
  await flush();
  assert.equal(next.sent.length, 0);
  assert.equal(socket.sent.length, 0);
  assert.equal(h.transport.state.phase, "connecting");
  h.transport.close();
});
test("replacement cancellation returns to saved app version when QR was for another build", () => {
  const saved = {
      device_id: "a".repeat(64),
      credential: "b".repeat(64),
      rid: "c".repeat(32),
      hash: "fedcba9876543210",
    },
    h = harness(
      saved,
      "https://phone.test/m/0123456789abcdef/#pair=aa&key=bb&r=" +
        "d".repeat(32),
    ),
    endpoint = h.context.KrillMobile.transport.bootstrap();
  assert.equal(endpoint.restoreSaved().hash, saved.hash);
  assert.deepEqual(h.redirects, ["/m/fedcba9876543210/"]);
});
for (const located of [false, true])
  for (const phase of ["connecting", "ready"])
    test(`late locate(${located}) cannot disconnect or stop a ${phase} replacement connection`, async () => {
      const h = harness(
          { device_id: "aa", credential: "bb", rid: "c".repeat(32) },
          undefined,
          true,
        ),
        lookup = deferred();
      let ownership,
        fallbacks = 0;
      h.endpoint.locate = (options) => {
        ownership = options;
        return lookup.promise;
      };
      h.endpoint.fallback = () => fallbacks++;
      const { pending, socket } = await h.start();
      socket.emit("error");
      await flush();
      h.transport.reconnect();
      await flush();
      const replacement = h.sockets.at(-1);
      assert.notEqual(replacement, socket);
      if (phase === "ready") {
        await h.advance(replacement);
        h.response(replacement, { sessions: [] });
        await flush();
        assert.equal(h.transport.ready, true);
      }
      const events = h.events.length;
      lookup.resolve(located);
      await pending;
      await flush();
      assert.equal(replacement.closed, undefined);
      assert.equal(h.transport.ready, phase === "ready");
      assert.equal(h.transport.state.phase, phase);
      assert.equal(h.events.length, events);
      assert.equal(fallbacks, 0);
      assert.equal(ownership.isCurrent(), false);
      assert.equal(ownership.signal.aborted, true);
      if (phase === "connecting") {
        await h.advance(replacement);
        h.response(replacement, { sessions: [] });
        await flush();
        assert.equal(h.transport.ready, true);
      }
      h.transport.reconnect();
      await flush();
      const next = h.sockets.at(-1);
      assert.notEqual(next, replacement);
      await h.advance(next);
      h.response(next, { sessions: [] });
      await flush();
      assert.equal(h.transport.ready, true);
      h.transport.close();
    });
for (const delayed of ["fetch", "json"])
  test(`stale discovery ${delayed} success cannot change routing, credentials or location`, async () => {
    const saved = {
        device_id: "a".repeat(64),
        credential: "b".repeat(64),
        rid: "c".repeat(32),
        url: "https://saved.trycloudflare.com",
        hash: "0123456789abcdef",
      },
      h = harness(saved, undefined, true),
      fetchResult = deferred(),
      jsonResult = deferred();
    let signal,
      jsonReads = 0;
    const response = {
      ok: true,
      json() {
        jsonReads++;
        return jsonResult.promise;
      },
    };
    h.context.fetch = (_url, options) => {
      signal = options.signal;
      return fetchResult.promise;
    };
    const { pending, socket } = await h.start();
    socket.emit("error");
    await flush();
    assert.equal(signal.aborted, false);
    if (delayed === "json") {
      fetchResult.resolve(response);
      await flush();
      assert.equal(jsonReads, 1);
    }
    h.transport.reconnect();
    await flush();
    const replacement = h.sockets.at(-1);
    assert.notEqual(replacement, socket);
    await h.advance(replacement);
    h.response(replacement, { sessions: [] });
    await flush();
    assert.equal(h.transport.ready, true);
    const routing = h.endpoint.socketUrl(),
      stored = h.store.get("krill-mobile-v2"),
      credentials = JSON.stringify(h.transport.credentials),
      endpointCredentials = JSON.stringify(h.endpoint.authenticated({})),
      events = h.events.length;
    jsonResult.resolve({
      hash: "fedcba9876543210",
      url: "https://stale.trycloudflare.com",
      relay: false,
    });
    fetchResult.resolve(response);
    await pending;
    await flush();
    assert.equal(h.endpoint.socketUrl(), routing);
    assert.equal(h.store.get("krill-mobile-v2"), stored);
    assert.equal(JSON.stringify(h.transport.credentials), credentials);
    assert.equal(
      JSON.stringify(h.endpoint.authenticated({})),
      endpointCredentials,
    );
    assert.deepEqual(h.redirects, []);
    assert.equal(jsonReads, delayed === "json" ? 1 : 0);
    assert.equal(signal.aborted, true);
    assert.equal(replacement.closed, undefined);
    assert.equal(h.transport.ready, true);
    assert.equal(h.transport.state.phase, "ready");
    assert.equal(h.events.length, events);
    h.transport.close();
  });
test("closing a connection cancels pending discovery before a late JSON redirect", async () => {
  const saved = {
      device_id: "a".repeat(64),
      credential: "b".repeat(64),
      rid: "c".repeat(32),
      hash: "0123456789abcdef",
    },
    h = harness(saved, undefined, true),
    jsonResult = deferred();
  let signal;
  h.context.fetch = async (_url, options) => {
    signal = options.signal;
    return { ok: true, json: () => jsonResult.promise };
  };
  const { pending, socket } = await h.start();
  socket.emit("error");
  await flush();
  const stored = h.store.get("krill-mobile-v2");
  h.transport.close();
  jsonResult.resolve({
    hash: "fedcba9876543210",
    url: "https://stale.trycloudflare.com",
  });
  await pending;
  assert.equal(h.store.get("krill-mobile-v2"), stored);
  assert.deepEqual(h.redirects, []);
  assert.equal(signal.aborted, true);
  assert.equal(h.transport.ready, false);
});

test("pairing a second desktop keeps the first credential and switching restores its destination", () => {
  const saved = {
      device_id: "first",
      credential: "11".repeat(32),
      rid: "a".repeat(32),
      url: "https://first.trycloudflare.com",
      hash: "0123456789abcdef",
    },
    h = harness(saved, undefined, true),
    e = h.endpoint;
  assert.equal(e.profiles().length, 1);
  const first = e.profiles()[0].id;
  e.describeHost({ display_name: "Home PC", platform: "macos" });
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  e.authenticated({ device_id: "second", credential: "22".repeat(32) });
  e.describeHost({ display_name: "Work PC", platform: "windows" });
  assert.equal(e.profiles().length, 2);
  const second = e.profiles().find((h) => h.active).id;
  assert.notEqual(first, second);
  assert.match(e.socketUrl(), /\/r\/bbbb/);
  assert.equal(e.selectHost(first).device_id, "first");
  assert.match(e.socketUrl(), /\/r\/aaaa/);
  assert.equal(e.profiles().find((h) => h.active).name, "Home PC");
  assert.equal(e.selectHost(second).credential, "22".repeat(32));
  assert.match(e.socketUrl(), /\/r\/bbbb/);
  assert(!JSON.stringify(e.profiles()).includes("credential"));
  const reloaded = h.context.KrillMobile.transport.bootstrap();
  assert.equal(reloaded.credentials.device_id, "second");
  assert.equal(reloaded.profiles().length, 2);
  assert.equal(reloaded.selectHost(first).credential, saved.credential);
});
test("re-pairing the same relay desktop rotates its credential without adding a duplicate", () => {
  const h = harness(
      { device_id: "old", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint;
  e.describeHost({ display_name: "Same PC" });
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "a".repeat(32),
  ).use({replace:true});
  e.authenticated({ device_id: "new", credential: "33".repeat(32) });
  assert.equal(e.profiles().length, 1);
  assert.equal(e.profiles()[0].name, "Same PC");
  assert.equal(JSON.parse(h.store.get("krill-mobile-v2")).device_id, "new");
});
test("desktop aliases survive snapshots, reconnects, reloads and re-pairing without changing credentials", () => {
  const saved = {
      device_id: "first",
      credential: "11".repeat(32),
      rid: "a".repeat(32),
    },
    h = harness(saved, undefined, true),
    e = h.endpoint,
    id = e.profiles()[0].id;
  e.describeHost({ display_name: "Original hostname" });
  e.renameHost(id, "  작업용 맥  ");
  e.describeHost({ display_name: "Another hostname" });
  e.authenticated({});
  assert.equal(e.profiles()[0].name, "작업용 맥");
  assert.equal(e.credentials.credential, saved.credential);
  assert.equal(e.credentials.device_id, saved.device_id);
  const reloaded = h.context.KrillMobile.transport.bootstrap();
  assert.equal(reloaded.profiles()[0].name, "작업용 맥");
  reloaded
    .adopt(
      "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
        "a".repeat(32),
    )
    .use({replace:true});
  reloaded.authenticated({ device_id: "rotated", credential: "22".repeat(32) });
  reloaded.describeHost({ display_name: "System hostname" });
  assert.equal(reloaded.profiles().length, 1);
  assert.equal(reloaded.profiles()[0].name, "작업용 맥");
});
test("renaming an inactive desktop preserves the active host and failed saves preserve its old name", () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id;
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  e.authenticated({ device_id: "second", credential: "22".repeat(32) });
  const before = h.store.get("krill-mobile-v2");
  e.renameHost(first, "Home PC");
  assert.equal(e.profiles().find((h) => h.id === first).name, "Home PC");
  assert.equal(h.store.get("krill-mobile-v2"), before);
  for (const invalid of ["", "  ", "x".repeat(81)])
    assert.throws(() => e.renameHost(first, invalid), /desktop name/);
  h.context.localStorage.setItem = () => {
    throw Error("quota");
  };
  assert.throws(() => e.renameHost(first, "Lost edit"), /Could not save/);
  assert.equal(e.profiles().find((h) => h.id === first).name, "Home PC");
  assert.equal(
    e.profiles().find((h) => h.active).id,
    "relay:" + "b".repeat(32),
  );
  assert.equal(h.store.get("krill-mobile-v2"), before);
});
test("uncompleted pairing never changes the persisted active desktop", () => {
  const h = harness(
      {
        device_id: "original",
        credential: "11".repeat(32),
        rid: "a".repeat(32),
      },
      undefined,
      true,
    ),
    e = h.endpoint,
    before = h.store.get("krill-mobile-hosts-v1");
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  assert.equal(h.store.get("krill-mobile-hosts-v1"), before);
  assert.equal(e.profiles().length, 1);
  const restored = e.restoreSaved();
  assert.equal(restored.device_id, "original");
  assert.match(e.socketUrl(), /\/r\/aaaa/);
});
test("forgetting an inactive desktop leaves the active authentication intact", () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id;
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  e.authenticated({ device_id: "second", credential: "22".repeat(32) });
  const active = e.profiles().find((h) => h.active).id;
  e.forgetHost(first);
  assert.equal(e.profiles().length, 1);
  assert.equal(e.profiles()[0].id, active);
  assert.throws(() => e.selectHost(first), /no longer saved/);
  assert.equal(JSON.parse(h.store.get("krill-mobile-v2")).device_id, "second");
});
test("disconnecting the active or last desktop removes persisted credentials without reviving it on reload", () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id;
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  e.authenticated({ device_id: "second", credential: "22".repeat(32) });
  const active = e.profiles().find((h) => h.active).id,
    stale = e.forHost(active);
  assert.equal(e.forgetHost(active), first);
  assert.equal(e.profiles().length, 1);
  assert.equal(e.profiles()[0].active, true);
  assert.equal(e.restoreSaved().device_id, "first");
  assert.equal(JSON.parse(h.store.get("krill-mobile-v2")).device_id, "first");
  stale.describeHost({ display_name: "stale" });
  stale.authenticated({});
  assert.equal(e.profiles().length, 1);
  assert.equal(e.forgetHost(first), null);
  assert.equal(e.profiles().length, 0);
  assert.equal(e.restoreSaved(), null);
  assert.equal(h.store.has("krill-mobile-v2"), false);
  const reloaded = h.context.KrillMobile.transport.bootstrap();
  assert.equal(reloaded.profiles().length, 0);
  assert.equal(reloaded.credentials, null);
  h.transport.close();
});
test("a failed disconnect preserves the desktop and credentials and reports the storage error", () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    id = e.profiles()[0].id,
    before = h.store.get("krill-mobile-v2");
  h.context.localStorage.setItem = () => {
    throw Error("quota");
  };
  assert.throws(() => e.forgetHost(id), /Could not remove/);
  assert.equal(e.profiles()[0].active, true);
  assert.equal(e.restoreSaved().device_id, "first");
  assert.equal(h.store.get("krill-mobile-v2"), before);
  h.transport.close();
});
test("another tab cannot revive a removed pairing through snapshot, authentication or common persistence", async () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    a = h.endpoint,
    b = h.context.KrillMobile.transport.bootstrap(),
    id = a.profiles()[0].id,
    peer = b.forHost(id);
  a.forgetHost(id);
  peer.describeHost({ display_name: "stale" });
  peer.authenticated({});
  b.describeHost({ display_name: "stale root" });
  assert.throws(() => b.selectHost(id), /no longer saved/);
  h.context.fetch = async () => ({
    ok: true,
    json: async () => ({ hash: "0123456789abcdef", relay: true, url: "" }),
  });
  await peer.locate();
  b.renameHost(id, "Stale tab rename");
  assert.equal(h.store.has("krill-mobile-v2"), false);
  assert.equal(
    JSON.parse(h.store.get("krill-mobile-hosts-v1")).hosts.length,
    0,
  );
  assert.equal(
    h.context.KrillMobile.transport.bootstrap().profiles().length,
    0,
  );
  assert.equal(b.reloadHosts(), true);
  assert.equal(b.profiles().length, 0);
  // A new explicit QR pairing is allowed; the old peer still cannot save the removed device.
  b.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "a".repeat(32),
  ).use();
  b.authenticated({ device_id: "new-device", credential: "33".repeat(32) });
  peer.describeHost({ display_name: "stale" });
  assert.equal(b.profiles().length, 1);
  assert.equal(
    JSON.parse(h.store.get("krill-mobile-v2")).device_id,
    "new-device",
  );
  h.transport.close();
});
test("switching while discovery is pending aborts the old route update", async () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id;
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  const second = e.authenticated({
    device_id: "second",
    credential: "22".repeat(32),
  });
  e.selectHost(first);
  const transport = h.context.KrillMobile.transport.create({
      credentials: e.selectHost(first),
      endpoint: e,
    }),
    lookup = deferred();
  h.context.fetch = () => lookup.promise;
  const pending = transport.connect();
  await flush();
  h.sockets.at(-1).emit("error");
  await flush();
  transport.close();
  e.selectHost(second.id);
  const route = e.socketUrl();
  lookup.resolve({
    ok: true,
    json: async () => ({
      hash: "fedcba9876543210",
      url: "https://stale.trycloudflare.com",
    }),
  });
  await pending;
  assert.equal(e.socketUrl(), route);
  assert.deepEqual(h.redirects, []);
  assert.equal(JSON.parse(h.store.get("krill-mobile-v2")).device_id, "second");
});

test("unavailable storage keeps pairing and host selection usable in memory", () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id;
  h.context.localStorage.setItem = () => {
    throw Error("Storage disabled");
  };
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  assert.equal(
    e.authenticated({ device_id: "second", credential: "22".repeat(32) })
      .device_id,
    "second",
  );
  assert.equal(e.storageFailed, true);
  assert.equal(e.profiles().length, 2);
  assert.equal(e.selectHost(first).device_id, "first");
  assert.match(e.socketUrl(), /\/r\/aaaa/);
});
test("pool keeps two desktops authenticated concurrently and isolates equal session IDs", async () => {
  const h = harness(
      {
        device_id: "first",
        credential: "11".repeat(32),
        rid: "a".repeat(32),
        hash: "0123456789abcdef",
      },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id;
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  const second = e.authenticated({
    device_id: "second",
    credential: "22".repeat(32),
  }).id;
  e.selectHost(first);
  const changes = [],
    pool = h.context.KrillMobile.transport.pool({
      endpoint: e,
      onChange: (entry) => changes.push(entry.id),
    });
  pool.sync();
  await flush();
  assert.equal(h.sockets.length, 2);
  const a = h.sockets.find((s) => s.url.includes("/r/aaaa")),
    b = h.sockets.find((s) => s.url.includes("/r/bbbb"));
  assert(a && b);
  await h.advance(a);
  h.response(a, {
    host: { display_name: "First PC" },
    sessions: [{ id: 7, title: "first session" }],
  });
  await h.advance(b);
  h.response(b, {
    host: { display_name: "Second PC" },
    sessions: [{ id: 7, title: "second session" }],
  });
  await flush();
  assert.equal(pool.get(first).transport.ready, true);
  assert.equal(pool.get(second).transport.ready, true);
  assert.equal(pool.get(first).snapshot.sessions[0].title, "first session");
  assert.equal(pool.get(second).snapshot.sessions[0].title, "second session");
  assert.equal(
    h.sockets.length,
    2,
    "background connections do not warm terminal sockets",
  );
  const auth = (s) => JSON.parse(s.sent[0]);
  assert.equal(auth(a).device_id, "first");
  assert.equal(auth(b).device_id, "second");
  e.selectHost(second);
  pool.sync();
  assert.equal(a.closed, undefined);
  assert.equal(b.closed, undefined);
  assert.equal(h.sockets.length, 2);
  a.emit("close", { code: 1006 });
  await flush();
  assert.equal(pool.get(first).transport.ready, false);
  assert.equal(pool.get(second).transport.ready, true);
  assert.equal(pool.get(second).snapshot.sessions[0].title, "second session");
  e.forgetHost(first);
  const old = pool.get(first);
  pool.sync();
  assert.equal(pool.get(first), undefined);
  assert.equal(old.transport.ready, false);
  assert.equal(pool.get(second).transport.ready, true);
  pool.close();
  h.transport.close();
});
test("background discovery/auth cannot replace the active desktop or revive removed credentials", async () => {
  const h = harness(
      {
        device_id: "first",
        credential: "11".repeat(32),
        rid: "a".repeat(32),
        hash: "0123456789abcdef",
      },
      undefined,
      true,
    ),
    e = h.endpoint,
    first = e.profiles()[0].id,
    peer = e.forHost(first);
  e.adopt(
    "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" +
      "b".repeat(32),
  ).use();
  const second = e.authenticated({
    device_id: "second",
    credential: "22".repeat(32),
  }).id;
  h.context.fetch = async () => ({
    ok: true,
    json: async () => ({
      hash: "fedcba9876543210",
      url: "https://firstnew.trycloudflare.com",
    }),
  });
  await peer.locate();
  peer.authenticated({});
  peer.describeHost({ display_name: "First renamed" });
  assert.equal(e.profiles().find((p) => p.active).id, second);
  assert.equal(JSON.parse(h.store.get("krill-mobile-v2")).device_id, "second");
  assert.equal(h.redirects.length, 0);
  assert.match(peer.socketUrl(), /firstnew/);
  assert.equal(e.profiles().find((p) => p.id === first).name, "First renamed");
  assert.throws(() => peer.authenticated({ device_id: "second" }), /identity/);
  e.forgetHost(first);
  peer.describeHost({ display_name: "stale" });
  peer.authenticated({});
  assert.equal(e.profiles().length, 1);
  assert.equal(e.profiles()[0].id, second);
  h.transport.close();
});

test("storage notifications during a new pairing leave its endpoint and handshake intact", () => {
  const line = fs
    .readFileSync(path.join(__dirname, "../src/mobile/assets/app.js"), "utf8")
    .split("\n")
    .find((l) => l.includes("globalThis.addEventListener('storage'"));
  let handler,
    reloads = 0,
    syncs = 0,
    updates = 0;
  const context = vm.createContext({
    pairing: { ticket: "new-pair" },
    landing: false,
    endpoint: {
      reloadHosts() {
        reloads++;
        throw Error("pairing destination must not change");
      },
    },
    pool: {
      sync() {
        syncs++;
      },
    },
    update() {
      updates++;
    },
    addEventListener(name, fn) {
      assert.equal(name, "storage");
      handler = fn;
    },
  });
  vm.runInContext(line, context);
  handler({ key: "krill-mobile-hosts-v1" });
  context.pairing = null;
  context.landing = true;
  handler({ key: "krill-mobile-removed-v1:old-device" });
  assert.equal(reloads, 0);
  assert.equal(syncs, 2);
  assert.equal(updates, 2);
});
test("identical background host descriptions skip localStorage writes and credential rotation stays durable", () => {
  const h = harness(
      { device_id: "first", credential: "11".repeat(32), rid: "a".repeat(32) },
      undefined,
      true,
    ),
    e = h.endpoint,
    peer = e.forHost(e.profiles()[0].id);
  let writes = 0;
  const set = h.context.localStorage.setItem;
  h.context.localStorage.setItem = (...args) => {
    writes++;
    set(...args);
  };
  peer.authenticated({});
  peer.describeHost({ display_name: "Same PC", platform: "macos" });
  const once = writes;
  for (let i = 0; i < 20; i++)
    peer.describeHost({ display_name: "Same PC", platform: "macos" });
  peer.authenticated({});
  assert.equal(writes, once);
  assert.throws(() => peer.authenticated({ credential: "22".repeat(32) }), /credential changed/);
  assert.equal(
    JSON.parse(h.store.get("krill-mobile-v2")).credential,
    "11".repeat(32),
  );
  peer.describeHost({ display_name: "Renamed PC" });
  assert.equal(e.profiles()[0].name, "Renamed PC");
  h.transport.close();
});
test("latest read coalesces only unsent reads and preserves sent replies, mutation position and deadlines", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  const base = socket.sent.length;
  const blocker = h.transport.rpc({ action: "accounts" });
  await flush();
  const first = h.transport.rpcLatest("diff", { action: "git", session: 1 }),
    latest = h.transport.rpcLatest("diff", { action: "git", session: 3 });
  assert.equal(first, latest);
  const mutation = h.transport.rpc({ action: "prompt", text: "once" });
  await flush();
  assert.equal(socket.sent.length, base + 1);
  h.response(socket, { accounts: [] });
  await blocker;
  await flush();
  assert.equal(socket.sent.length, base + 2);
  assert(Buffer.from(socket.sent.at(-1)).includes(Buffer.from('"session":3')));
  h.response(socket, { diff: "newest" });
  assert.equal((await latest).diff, "newest");
  await flush();
  assert.equal(socket.sent.length, base + 3);
  assert(
    Buffer.from(socket.sent.at(-1)).includes(Buffer.from('"action":"prompt"')),
  );
  h.response(socket, { sent: true });
  await mutation;
  await assert.rejects(
    h.transport.rpcLatest("no", { action: "input", text: "forbidden" }),
    /Only reads/,
  );
  assert.equal(socket.sent.length, base + 3);
  h.transport.close();
});

test("only the selected host prewarms one reusable terminal connection and demotion closes its spare", async () => {
  const h = harness(),
    t = h.transport;
  t.setActive(false);
  const { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  assert.equal(h.sockets.length, 1);
  t.setActive(true);
  const warm = h.sockets.at(-1);
  assert.equal(h.sockets.length, 2);
  t.setActive(true);
  assert.equal(h.sockets.length, 2);
  warm.emit("open");
  warm.emit("message", {
    data: JSON.stringify({ version: 2, challenge: "aa" }),
  });
  await flush();
  assert.equal(warm.sent.length, 3);
  const taken = t.terminalConnect();
  assert.equal(taken.socket, warm);
  assert(taken.channel);
  assert.equal(h.sockets.length, 3);
  const spare = h.sockets.at(-1);
  t.setActive(false);
  assert.equal(spare.closed, true);
  assert.notEqual(
    warm.closed,
    true,
    "attached terminal remains owned by its view",
  );
  t.setActive(true);
  assert.equal(h.sockets.length, 4);
  t.close();
  assert.equal(h.sockets.at(-1).closed, true);
  taken.socket.close();
});

test("relay has priority after reload and host switch while the alternate remains available", () => {
  const saved = {
      device_id: "first",
      credential: "11".repeat(32),
      rid: "a".repeat(32),
      url: "https://stale.trycloudflare.com",
      hash: "0123456789abcdef",
    },
    h = harness(saved, undefined, true),
    e = h.endpoint,
    id = e.profiles()[0].id,
    peer = e.forHost(id);
  assert.match(peer.socketUrl(), /\/r\/aaaa/);
  peer.fallback(peer.socketUrl());
  assert.match(peer.socketUrl(), /stale/);
  peer.fallback(peer.socketUrl());
  peer.authenticated({});
  const stored = JSON.parse(h.store.get("krill-mobile-v2"));
  assert.equal(stored.preferred_route, "relay");
  assert.equal(stored.url, saved.url, "alternate remains available");
  const reload = h.context.KrillMobile.transport.bootstrap();
  assert.match(reload.socketUrl(), /\/r\/aaaa/);
  reload.selectHost(id);
  assert.match(reload.socketUrl(), /\/r\/aaaa/);
  assert.match(reload.forHost(id).socketUrl(), /\/r\/aaaa/);
  const again = reload.forHost(id);
  again.fallback(again.socketUrl());
  again.authenticated({});
  assert.equal(
    JSON.parse(h.store.get("krill-mobile-v2")).preferred_route,
    "tunnel",
  );
  assert.match(
    h.context.KrillMobile.transport.bootstrap().socketUrl(),
    /\/r\/aaaa/,
  );
  h.transport.close();
});

test("successful discovery route is attempted directly instead of being toggled back to relay", async () => {
  const saved = {
      device_id: "first",
      credential: "11".repeat(32),
      rid: "a".repeat(32),
      url: "https://stale.trycloudflare.com",
      hash: "0123456789abcdef",
    },
    h = harness(saved, undefined, true);
  h.context.fetch = async () => ({
    ok: true,
    json: async () => ({
      hash: saved.hash,
      url: "https://fresh.trycloudflare.com",
    }),
  });
  const { pending, socket } = await h.start();
  socket.emit("error");
  await pending;
  assert.match(h.endpoint.socketUrl(), /fresh/);
  await h.tick(0);
  assert.match(h.sockets.at(-1).url, /fresh/);
  h.transport.close();
});

test("a previously healthy connection redials without the one-second delay and still backs off repeated failures", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  const before = h.sockets.length;
  socket.emit("close", { code: 1006 });
  await flush();
  await h.tick(0);
  assert.equal(h.sockets.length, before + 1);
  const retry = h.sockets.at(-1);
  retry.emit("error");
  await flush();
  const failed = h.sockets.length;
  await h.tick(1999);
  assert.equal(h.sockets.length, failed);
  await h.tick(1);
  assert.equal(h.sockets.length, failed + 1);
  h.transport.close();
});

test("connection timing separates socket challenge proof authentication and first snapshot without endpoint secrets", async () => {
  const h = harness(),
    { pending, socket } = await h.start();
  await h.tick(40);
  await h.advance(socket, 1);
  await h.tick(30);
  socket.emit("message", {
    data: JSON.stringify({ version: 2, challenge: "aa" }),
  });
  await flush();
  await h.tick(20);
  socket.emit("message", { data: enc.encode("{}") });
  await flush();
  await h.tick(10);
  socket.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 1]) });
  await flush();
  await h.tick(50);
  h.response(socket, { sessions: [] });
  await pending;
  assert.equal(h.transport.ready, true);
  const t = h.transport.state.timings;
  assert.equal(t.route, "direct");
  assert.equal(t.socketOpenMs, 40);
  assert.equal(t.challengeMs, 70);
  assert.equal(t.proofSentMs, 70);
  assert.equal(t.authenticatedMs, 90);
  assert.equal(t.helloMs, 100);
  assert.equal(t.readyMs, 150);
  assert(!JSON.stringify(t).includes("desktop.test"));
  assert(!JSON.stringify(t).includes("credential"));
  h.transport.close();
});

test("selected terminal preauthentication overlaps the first snapshot instead of waiting for its payload", async () => {
  const h = harness();
  h.transport.setActive(false);
  const { pending, socket } = await h.start(3);
  h.transport.setActive(true);
  assert.equal(h.sockets.length, 1);
  socket.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 1]) });
  await flush();
  assert.equal(h.sockets.length, 2);
  assert.equal(
    h.transport.state.phase,
    "connecting",
    "snapshot deadline still applies",
  );
  const warm = h.sockets.at(-1);
  warm.emit("open");
  warm.emit("message", {
    data: JSON.stringify({ version: 2, challenge: "aa" }),
  });
  await flush();
  assert.equal(warm.sent.length, 3);
  h.response(socket, { sessions: [] });
  await pending;
  assert.equal(h.transport.state.phase, "ready");
  const taken = h.transport.terminalConnect();
  assert.equal(taken.socket, warm);
  h.transport.close();
  taken.socket.close();
});
test("reconnect sends the previous live etag and becomes ready at ServerHello while serializing RPCs behind snapshot", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { etag: "same", sessions: [{ id: 7 }] });
  await pending;
  h.transport.setActive(false);
  h.transport.reconnect();
  await flush();
  const next = h.sockets.at(-1);
  await h.advance(next, 3);
  assert.equal(h.transport.state.phase, "connecting");
  const request = next.sent[3];
  let at = 6,
    n = 0,
    shift = 0,
    b;
  do {
    b = request[at++];
    n |= (b & 127) << shift;
    shift += 7;
  } while (b & 128);
  assert.deepEqual(
    JSON.parse(new TextDecoder().decode(request.slice(at, at + n))),
    { action: "snapshot", since: "same" },
  );
  next.emit("message", { data: Uint8Array.from([0, 0, 0, 0, 1]) });
  await flush();
  assert.equal(h.transport.state.phase, "ready");
  assert.equal(h.transport.ready, true);
  assert.equal(h.transport.updateBusy, true);
  const before = next.sent.length,
    rpc = h.transport.rpc({ action: "git" });
  await flush();
  assert.equal(
    next.sent.length,
    before,
    "the first snapshot owns the next response",
  );
  h.response(next, { unchanged: true, etag: "same" });
  await flush();
  assert.equal(next.sent.length, before + 1);
  h.response(next, { files: [] });
  await rpc;
  assert.equal(h.transport.updateBusy, false);
  assert.equal(h.events.filter((e) => e.phase === "ready").length, 2);
  h.transport.close();
});
test("a reconnect snapshot timeout still disconnects an immediately ready transport", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { etag: "same", sessions: [] });
  await pending;
  h.transport.setActive(false);
  h.transport.reconnect();
  await flush();
  await h.advance(h.sockets.at(-1));
  assert.equal(h.transport.state.phase, "ready");
  await h.tick(20000);
  assert.equal(h.transport.ready, false);
  assert.match(h.transport.state.message, /timed out/);
  h.transport.close();
});
test("new pairing discards the prior live snapshot before authentication", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { etag: "old-host", sessions: [] });
  await pending;
  h.transport.setActive(false);
  h.transport.setPair({ ticket: "new", secret: "secret" });
  await flush();
  const next = h.sockets.at(-1);
  await h.advance(next);
  assert.equal(h.transport.state.phase, "connecting");
  assert(!new TextDecoder().decode(next.sent[3]).includes("old-host"));
  h.response(next, { etag: "new-host", sessions: [] });
  await flush();
  assert.equal(h.transport.state.phase, "ready");
  h.transport.close();
});
test("update start probes immediately once and completed probes suppress repeated lifecycle events", async () => {
  const h = harness(
      {
        device_id: "first",
        credential: "11".repeat(32),
        rid: "a".repeat(32),
        hash: "0123456789abcdef",
      },
      undefined,
      true,
    ),
    doc = new EventTarget(),
    events = new EventTarget();
  doc.hidden = false;
  h.context.document = doc;
  h.context.addEventListener = events.addEventListener.bind(events);
  h.context.removeEventListener = events.removeEventListener.bind(events);
  vm.runInContext(
    fs.readFileSync(
      path.join(__dirname, "../src/mobile/assets/updates.js"),
      "utf8",
    ),
    h.context,
  );
  let probes = 0;
  h.endpoint.probeUpdate = async () => {
    probes++;
    return null;
  };
  const api = h.context.KrillMobile.updates.create({ endpoint: h.endpoint });
  api.start();
  await flush();
  assert.equal(probes, 1);
  api.start();
  events.dispatchEvent(new Event("pageshow"));
  events.dispatchEvent(new Event("online"));
  doc.dispatchEvent(new Event("visibilitychange"));
  await flush();
  assert.equal(probes, 1);
  await h.tick(1000);
  events.dispatchEvent(new Event("online"));
  await flush();
  assert.equal(probes, 2);
  await api.check();
  assert.equal(probes, 3, "explicit discovery is never throttled");
  api.stop();
  h.transport.close();
});

test("resume keeps a healthy slow connection while the existing RPC deadline bounds refresh", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  h.transport.setActive(false);
  const sent = socket.sent.length;
  h.transport.resume();
  await flush();
  assert.equal(socket.sent.length, sent + 1);
  await h.tick(2000);
  assert.equal(h.transport.ready, true);
  assert.equal(socket.closed, undefined);
  h.transport.resume();
  await flush();
  assert.equal(
    socket.sent.length,
    sent + 1,
    "lifecycle events share the pending snapshot",
  );
  h.response(socket, { sessions: [] });
  await flush();
  assert.equal(h.transport.ready, true);
  assert.equal(socket.closed, undefined);
  h.transport.close();
});
test("resume refresh still disconnects a truly stalled transport at the RPC deadline", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  h.transport.setActive(false);
  h.transport.resume();
  await flush();
  await h.tick(15000);
  assert.equal(h.transport.ready, false);
  assert.equal(socket.closed, true);
  h.transport.close();
});

test("saved loopback endpoints never dial the phone or erase the paired desktop", () => {
  const saved = {
      device_id: "kept",
      credential: "11".repeat(32),
      origin: "http://127.0.0.1:1234",
      fixed: false,
    },
    h = harness(saved, undefined, true),
    id = h.endpoint.profiles()[0].id;
  assert.throws(() => h.endpoint.socketUrl(), /points to this phone/);
  assert.throws(
    () => h.endpoint.forHost(id).socketUrl(),
    /points to this phone/,
  );
  assert.equal(h.endpoint.credentials.device_id, "kept");
  assert.equal(
    JSON.parse(h.store.get("krill-mobile-v2")).credential,
    saved.credential,
  );
  h.transport.close();
});
test("HTTPS relay pages explain the Tailscale HTTP backup instead of dialing mixed content", () => {
  const saved = {
      device_id: "kept",
      credential: "11".repeat(32),
      origin: "http://100.64.1.2:1234",
      fixed: false,
    },
    h = harness(saved, undefined, true);
  assert.throws(() => h.endpoint.socketUrl(), /Tailscale QR code directly/);
  h.transport.close();
  const direct = harness(saved, "http://100.64.1.2:1234/", true);
  assert.equal(direct.endpoint.socketUrl(), "ws://100.64.1.2:1234/ws");
  direct.transport.close();
});

for (const status of [401, 403])
  test(`hosted discovery ${status} never asks the phone to sign in or discards pairing`, async () => {
    const saved = {
        device_id: "a".repeat(64),
        credential: "b".repeat(64),
        rid: "c".repeat(32),
        hash: "0123456789abcdef",
      },
      h = harness(saved, undefined, true);
    let options;
    h.context.fetch = async (_url, request) => {
      options = request;
      return { status, ok: false };
    };
    const { pending, socket } = await h.start();
    socket.emit("error");
    await pending;
    assert.equal(h.context.KrillMobile.relayAccount, undefined);
    assert.equal(options.credentials, "omit");
    assert.equal(h.transport.state.phase, "disconnected");
    assert.equal(h.transport.blocked, false);
    assert.doesNotMatch(h.transport.state.message, /Google|sign in/i);
    assert.equal(
      JSON.parse(h.store.get("krill-mobile-v2")).credential,
      saved.credential,
    );
    assert.deepEqual(h.redirects, []);
    h.transport.close();
  });

test("mobile client has no Google account entry points", () => {
  assert.doesNotMatch(
    source,
    /\/auth(?:[/?"']|$)|krill-relay-auth-required|Sign in with Google/,
  );
  for (const file of ["app.js", "screens/settings.js"])
    assert.doesNotMatch(
      fs.readFileSync(
        path.join(__dirname, "../src/mobile/assets", file),
        "utf8",
      ),
      /relayAccount|krill-relay-auth-required/,
    );
});
test("1013 shows one generic temporary-limit message for auth cooldown and relay overload while keeping backoff", async () => {
  const h = harness(),
    { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] });
  await pending;
  socket.emit("close", { code: 1013 });
  await flush();
  assert.equal(
    h.transport.state.message,
    "Connection temporarily limited. Wait before reconnecting.",
  );
  assert.equal(h.transport.blocked, false);
  const retryAt = h.transport.state.retryAt;
  assert.ok(
    retryAt >= 100000 + 65000,
    "conservative 65s cooldown is unchanged",
  );
  const opened = h.sockets.length;
  h.transport.reconnect();
  await flush();
  assert.equal(h.sockets.length, opened, "no reconnect before cooldown");
  assert.equal(
    h.transport.state.message,
    "Connection temporarily limited. Wait before reconnecting.",
  );
  h.transport.close();
});

test('saved loopback endpoints never dial the phone or erase the paired desktop',()=>{
 const saved={device_id:'kept',credential:'11'.repeat(32),origin:'http://127.0.0.1:1234',fixed:false},h=harness(saved,undefined,true),id=h.endpoint.profiles()[0].id;
 assert.throws(()=>h.endpoint.socketUrl(),/points to this phone/);
 assert.throws(()=>h.endpoint.forHost(id).socketUrl(),/points to this phone/);
 assert.equal(h.endpoint.credentials.device_id,'kept');assert.equal(JSON.parse(h.store.get('krill-mobile-v2')).credential,saved.credential);h.transport.close();
});
test('HTTPS relay pages explain the Tailscale HTTP backup instead of dialing mixed content',()=>{
 const saved={device_id:'kept',credential:'11'.repeat(32),origin:'http://100.64.1.2:1234',fixed:false},h=harness(saved,undefined,true);
 assert.throws(()=>h.endpoint.socketUrl(),/Tailscale QR code directly/);h.transport.close();
 const direct=harness(saved,'http://100.64.1.2:1234/',true);assert.equal(direct.endpoint.socketUrl(),'ws://100.64.1.2:1234/ws');direct.transport.close();
});

test('legacy HTTP Tailscale codes preserve a working HTTPS page and still pair standalone',()=>{
 const saved={device_id:'saved',credential:'11'.repeat(32),rid:'c'.repeat(32),hash:'0123456789abcdef'};
 for(const ip of ['100.64.0.0','100.64.1.2','100.127.255.255']){
  const h=harness(saved,undefined,true),link=`http://${ip}:52931/#pair=ab12&key=secret`,stored=h.store.get('krill-mobile-v2'),book=h.store.get('krill-mobile-hosts-v1'),route=h.endpoint.socketUrl();
  assert.throws(()=>h.endpoint.adopt(link),/older Tailscale address/);
  assert.deepEqual(h.redirects,[]);assert.equal(h.sockets.length,0);assert.equal(h.endpoint.socketUrl(),route);assert.equal(h.store.get('krill-mobile-v2'),stored);assert.equal(h.store.get('krill-mobile-hosts-v1'),book);h.transport.close();
  const direct=harness(null,link,true);assert.equal(direct.endpoint.pair.ticket,'ab12');assert.equal(direct.endpoint.pair.secret,'secret');assert.equal(direct.endpoint.socketUrl(),`ws://${ip}:52931/ws`);direct.transport.close();
 }
});
test('Tailscale pairing navigation rejects LAN, loopback, out-of-range and credential-bearing URLs',()=>{
 const h=harness(null,undefined,true);
 for(const origin of ['http://100.63.255.255:52931','http://100.128.0.0:52931','http://192.168.0.5:52931','http://127.0.0.1:52931','http://localhost:52931','http://100.64.1.2.evil.test:52931','http://user:pass@100.64.1.2:52931'])assert.throws(()=>h.endpoint.adopt(origin+'/#pair=ab12&key=secret'));
 assert.throws(()=>h.endpoint.adopt('http://100.64.1.2:52931/other/#pair=ab12&key=secret'));
 for(const hash of ['#pair=ab12','#key=secret','#pair=zz&key=secret'])assert.throws(()=>h.endpoint.adopt('http://100.64.1.2:52931/'+hash));
 assert.equal(h.redirects.length,0);assert.equal(h.sockets.length,0);h.transport.close();
});

test('Tailscale pairing rejects malformed URLs and empty pairing fields without changing saved relay state',()=>{
 const saved={device_id:'saved',credential:'11'.repeat(32),rid:'c'.repeat(32),hash:'0123456789abcdef'},h=harness(saved,undefined,true),stored=h.store.get('krill-mobile-v2'),book=h.store.get('krill-mobile-hosts-v1'),route=h.endpoint.socketUrl();
 for(const link of ['not a URL','http://100.64.1.256:52931/#pair=ab12&key=secret','http://100.64.1.2:99999/#pair=ab12&key=secret','http://user@100.64.1.2:52931/#pair=ab12&key=secret','http://:pass@100.64.1.2:52931/#pair=ab12&key=secret','http://[::1]:52931/#pair=ab12&key=secret','http://100.64.1.2:52931//#pair=ab12&key=secret','http://100.64.1.2:52931/#pair=&key=secret','http://100.64.1.2:52931/#pair=ab12&key='])assert.throws(()=>h.endpoint.adopt(link));
 assert.deepEqual(h.redirects,[]);assert.equal(h.sockets.length,0);assert.equal(h.endpoint.socketUrl(),route);assert.equal(h.store.get('krill-mobile-v2'),stored);assert.equal(h.store.get('krill-mobile-hosts-v1'),book);h.transport.close();
});

const desktopManifest = (id = "a".repeat(32)) => ({ host_id: id,
  relay: { origin: "https://phone.test", rid: id },
  tailscale: { origin: "https://desktop.tail123.ts.net:18449" },
});
const secureQr = (rid = "a".repeat(32)) => "https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r=" + rid + "&d=desktop.tail123.ts.net:18449";
async function candidateReply(h, reply) {
  const socket = h.sockets.at(-1);
  socket.emit("open");
  socket.emit("message", { data: JSON.stringify({ version: 2, challenge: "aa" }) });
  socket.emit("message", { data: enc.encode(JSON.stringify(reply)) });
  await flush();
  return socket;
}
test("adding authenticated Tailscale to a live relay keeps its owner, profile, credential and unused ticket", async () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) },
    h = harness(saved, undefined, true), e = h.endpoint, id = e.profiles()[0].id;
  e.renameHost(id, "My alias");
  const { pending, socket } = await h.start(4);
  h.response(socket, { sessions: [] }); await pending;
  const next = e.adopt(secureQr()), attached = next.attach();
  const probe = await candidateReply(h, { device_id: "phone", credential: null, desktop: desktopManifest() });
  await attached;
  assert.equal(JSON.parse(probe.sent[0]).token, null);
  assert.equal(JSON.parse(probe.sent[0]).device_id, "phone");
  assert.equal(probe.sent.length, 2, "auth-only candidate sends no IPC or workflow RPC");
  assert.equal(probe.closed, true);
  assert.equal(socket.closed, undefined);
  assert.equal(h.transport.ready, true);
  assert.equal(e.profiles().length, 1); assert.equal(e.profiles()[0].id, id);
  assert.equal(e.profiles()[0].name, "My alias");
  assert.equal(e.credentials.credential, saved.credential);
  const peer = e.forHost(id);
  assert.match(peer.socketUrl(), /\/r\//);
  assert.equal(peer.fallback(peer.socketUrl()), true);
  assert.equal(peer.socketUrl(), "wss://desktop.tail123.ts.net:18449/ws");
  assert.equal(h.redirects.length, 0); h.transport.close();
});
test("matching QR hints cannot merge a mismatched desktop or replace its saved credential", async () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) }, h = harness(saved, undefined, true), e = h.endpoint;
  const before = h.store.get("krill-mobile-hosts-v1"), next = e.adopt(secureQr());
  assert.throws(() => next.use(), /saved pairing/);
  const result = assert.rejects(next.attach(), /identity changed/);
  await candidateReply(h, { device_id: "phone", credential: null, desktop: desktopManifest("b".repeat(32)) });
  await result;
  assert.equal(h.store.get("krill-mobile-hosts-v1"), before);
  assert.equal(e.credentials.credential, saved.credential); h.transport.close();
});
test("opening a secure QR for a saved desktop keeps its saved pairing and attaches without ticket auth", () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) }, h = harness(saved, secureQr(), true);
  assert.equal(h.endpoint.pair, null); assert.equal(h.endpoint.credentials.device_id, "phone");
  assert.equal(new URLSearchParams(new URL(h.endpoint.initialRouteLink).hash.slice(1)).get("d"), "desktop.tail123.ts.net:18449"); h.transport.close();
});
test("active route loss uses the retained backup without replaying a submitted mutation", async () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) }, h = harness(saved, undefined, true);
  h.endpoint.authenticated({ desktop: desktopManifest() });
  const { pending, socket } = await h.start(4); h.response(socket, { sessions: [] }); await pending;
  const action = assert.rejects(h.transport.rpc({ action: "input", text: "once" }), /not resent/);
  await flush(); const writes = socket.sent.length;
  socket.emit("close", { code: 1006 }); await action; await h.tick(0);
  const backup = h.sockets.at(-1);
  assert.equal(backup.url, "wss://desktop.tail123.ts.net:18449/ws");
  assert.equal(socket.sent.length, writes); assert.equal(backup.sent.length, 0);
  assert.equal(h.transport.updateBusy, true, "unknown mutation delivery remains visible"); h.transport.close();
});
test("fresh compact Tailscale QR uses hosted storage and secure direct route first", () => {
  const h = harness(null, "https://phone.test/m/0123456789abcdef/#p=" + "A".repeat(43) + "&key=secret&r=" + "a".repeat(32) + "&d=desktop.tail123.ts.net:18449", true);
  assert.equal(h.endpoint.pair.ticket, "00".repeat(32));
  assert.equal(h.endpoint.socketUrl(), "wss://desktop.tail123.ts.net:18449/ws"); h.transport.close();
});

test("mid-auth route transport failure must try retained backup", async () => {
 const saved={device_id:"phone",credential:"11".repeat(32),rid:"a".repeat(32)};
 const h=harness(saved,undefined,true);h.endpoint.authenticated({desktop:desktopManifest()});
 const {pending,socket}=await h.start(2); socket.emit("close",{code:1006});await pending;await h.tick(1000);
 assert.equal(h.sockets.at(-1).url,"wss://desktop.tail123.ts.net:18449/ws");
 h.transport.close();
});

test("malformed compact pairing fragments are stripped without throwing or replacing saved credentials", () => {
  const h = harness({device_id:"phone",credential:"11".repeat(32),rid:"a".repeat(32)}), paths=[];
  h.context.history.replaceState = (_state,_title,url) => paths.push(url);
  for (const fragment of ["p=!", "p="+"A".repeat(44), "p="+"A".repeat(42)+"B", "pair=ab&p="+"A".repeat(43), "p="+"A".repeat(43)+"&d=http://bad.test"]) {
    h.context.location.hash="#"+fragment+"&key=secret";
    const endpoint = h.context.KrillMobile.transport.bootstrap();
    assert.equal(endpoint.pair,null); assert(endpoint.pairingError);
    assert.equal(endpoint.credentials.device_id,"phone"); assert(!paths.at(-1).includes("secret"));
    assert.throws(()=>endpoint.adopt("https://phone.test/m/0123456789abcdef/#"+fragment+"&key=secret"), /Not a Krill pairing code/);
  }
  h.transport.close();
});
test("pasted pairing links return route authentication failures to the existing action handler", async () => {
  const h=harness(), line=fs.readFileSync(path.join(__dirname,"../src/mobile/assets/app.js"),"utf8").split("\n").find(line=>line.includes("function pasteLink()"));
  let submit;
  Object.assign(h.context,{t:text=>text,endpoint:{adopt:()=>({})},closeSheet:()=>{},pairWith:()=>Promise.reject(Error("TLS unavailable")),el:()=>({value:secureQr(),addEventListener(){},setAttribute(){}}),button:(_text,action)=>{submit=action;return {};},sheet:(_title,_items,_copy,extra)=>extra({append(){}})});
  vm.runInContext(line+";globalThis.paste= pasteLink;",h.context);
  h.context.paste(); await assert.rejects(submit(),/TLS unavailable/); h.transport.close();
});

test("direct QR host hint never creates an unauthenticated relay route after relay ID rotation", () => {
  const hostId="a".repeat(32), h=harness(null,"https://phone.test/m/0123456789abcdef/#p="+"A".repeat(43)+"&key=secret&r="+hostId+"&d=desktop.tail123.ts.net:18449",true);
  h.endpoint.authenticated({device_id:"phone",credential:"11".repeat(32),desktop:{host_id:hostId}});
  assert.equal(h.endpoint.credentials.rid,null);
  assert.equal(h.endpoint.credentials.routes.relay,undefined);
  assert.equal(h.endpoint.forHost(h.endpoint.profiles()[0].id).socketUrl(),"wss://desktop.tail123.ts.net:18449/ws");
  h.transport.close();
});
test("authenticated existing relay is retained explicitly when a ready-only manifest omits it", () => {
  const hostId="a".repeat(32), h=harness({device_id:"phone",credential:"11".repeat(32),rid:hostId},undefined,true), peer=h.endpoint.forHost(h.endpoint.profiles()[0].id);
  assert.match(peer.socketUrl(), /\/r\//);
  peer.authenticated({device_id:"phone",credential:null,desktop:{host_id:hostId}});
  assert.equal(peer.credentials.routes.relay.rid,hostId);
  assert.equal(peer.credentials.routes.relay.origin,"https://phone.test"); h.transport.close();
});

test("a retained direct-first endpoint discovers the relay attached later without rebuilding its owner", async () => {
  const hostId="a".repeat(32), relayId="b".repeat(32), h=harness(null,"https://phone.test/m/0123456789abcdef/#p="+"A".repeat(43)+"&key=secret&r="+hostId+"&d=desktop.tail123.ts.net:18449",true);
  const auth=h.endpoint.authenticated({device_id:"phone",credential:"11".repeat(32),desktop:{host_id:hostId}}), peer=h.endpoint.forHost(auth.id);
  const attach=h.endpoint.adopt("https://phone.test/m/0123456789abcdef/#pair=ab&key=secret&r="+relayId+"&h="+hostId).attach();
  await candidateReply(h,{device_id:"phone",credential:null,desktop:{host_id:hostId}}); await attach;
  const requests=[];h.context.fetch=async url=>{requests.push(url);return {ok:true,json:async()=>({hash:"0123456789abcdef",url:"",relay:true})};};
  await peer.locate(); assert.equal(requests[0],"https://phone.test/r/"+relayId);
  assert.equal(h.endpoint.credentials.rid,relayId);
  assert(h.endpoint.updateIdentity().includes(relayId)); h.transport.close();
});

test("active peer authenticated relay rotation updates the retained bootstrap update identity", () => {
  const id="a".repeat(32), rotated="b".repeat(32), h=harness({device_id:"phone",credential:"11".repeat(32),rid:id},undefined,true), peer=h.endpoint.forHost(h.endpoint.profiles()[0].id);
  peer.socketUrl(); peer.authenticated({device_id:"phone",credential:null,desktop:{host_id:id,relay:{origin:"https://phone.test",rid:rotated}}});
  assert.equal(h.endpoint.credentials.rid,rotated);
  assert(h.endpoint.updateIdentity().includes(rotated)); h.transport.close();
});

test("route attachment resolves the current saved desktop after storage reload while authenticating", async () => {
 const hostId="a".repeat(32), h=harness({device_id:"phone",credential:"11".repeat(32),rid:hostId},undefined,true), e=h.endpoint;
 const attached=e.adopt(secureQr()).attach(), book=JSON.parse(h.store.get("krill-mobile-hosts-v1"));
 book.hosts[0].alias="Changed in another tab";h.store.set("krill-mobile-hosts-v1",JSON.stringify(book));e.reloadHosts();
 await candidateReply(h,{device_id:"phone",credential:null,desktop:desktopManifest()});await attached;
 const saved=JSON.parse(h.store.get("krill-mobile-hosts-v1")).hosts[0];
 assert.equal(saved.routes.tailscale.origin,"https://desktop.tail123.ts.net:18449");assert.equal(saved.alias,"Changed in another tab");h.transport.close();
});

test("attaching a proved backup recovers the retained blocked owner", async () => {
 const saved={device_id:"phone",credential:"11".repeat(32),rid:"a".repeat(32)},h=harness(saved,undefined,true),e=h.endpoint,id=e.profiles()[0].id;
 const pool=h.context.KrillMobile.transport.pool({endpoint:e});pool.sync();await flush();
 const entry=pool.get(id),owner=entry.transport,main=h.sockets.at(-1);
 await h.advance(main,2);main.emit("close",{code:1008});await flush();
 assert.equal(owner.blocked,true,"old sole route is auth-blocked");
 assert.throws(()=>owner.recoverRoute("wss://unsaved.tail123.ts.net/ws"),/address is unavailable/);
 assert.equal(owner.blocked,true,"an unregistered address cannot clear the authentication block");
 const app=fs.readFileSync(path.join(__dirname,"../src/mobile/assets/app.js"),"utf8"),line=app.split("\n").find(l=>l.includes("async function pairWith("));
 Object.assign(h.context,{pool,update(){},toast(){},t:s=>s,sheet(){},startPairing(){}});
 vm.runInContext(line+";globalThis.reviewPair=pairWith;",h.context);
 const attaching=h.context.reviewPair("",e.adopt(secureQr()));
 await candidateReply(h,{device_id:"phone",credential:null,desktop:desktopManifest()});await attaching;await flush();
 assert.equal(pool.get(id).transport,owner,"must retain the same mutation owner");
 assert.equal(owner.blocked,false,"new authenticated backup must unblock and retry the retained owner");
 assert.equal(h.sockets.at(-1).url,"wss://desktop.tail123.ts.net:18449/ws","retry should use the verified backup");
 pool.close();h.transport.close();
});

test("attaching a proved backup clears obsolete authentication cooldown", async () => {
 const saved={device_id:"phone",credential:"11".repeat(32),rid:"a".repeat(32)},h=harness(saved,undefined,true),e=h.endpoint,id=e.profiles()[0].id;
 const pool=h.context.KrillMobile.transport.pool({endpoint:e});pool.sync();await flush();
 const entry=pool.get(id),owner=entry.transport,main=h.sockets.at(-1);
 await h.advance(main,2);main.emit("close",{code:1013});await flush();
 assert(owner.state.retryAt>h.context.Date.now(),"old route has an authentication retry cooldown");
 const previousRetry=owner.state.retryAt;
 assert.throws(()=>owner.recoverRoute("wss://unsaved.tail123.ts.net/ws"),/address is unavailable/);
 assert.equal(owner.state.retryAt,previousRetry,"an unregistered address cannot clear retry timing");
 const app=fs.readFileSync(path.join(__dirname,"../src/mobile/assets/app.js"),"utf8"),line=app.split("\n").find(l=>l.includes("async function pairWith("));
 Object.assign(h.context,{pool,update(){},toast(){},t:s=>s,sheet(){},startPairing(){}});
 vm.runInContext(line+";globalThis.reviewPair=pairWith;",h.context);
 const attaching=h.context.reviewPair("",e.adopt(secureQr()));
 const probe=await candidateReply(h,{device_id:"phone",credential:null,desktop:desktopManifest()});await attaching;await flush();
 const recovery=h.sockets.at(-1);
 assert.notEqual(recovery,probe,"successful new route proof must start recovery immediately instead of waiting for old cooldown");
 assert.equal(pool.get(id).transport,owner,"must retain the same mutation owner");
 assert.equal(recovery.url,"wss://desktop.tail123.ts.net:18449/ws");
 await h.advance(recovery,4);h.response(recovery,{sessions:[]});await flush();assert.equal(owner.ready,true);
 pool.close();h.transport.close();
});

test("attaching a proved backup preserves an uncertain mutation while recovering", async () => {
 const saved={device_id:"phone",credential:"11".repeat(32),rid:"a".repeat(32)},h=harness(saved,undefined,true),e=h.endpoint,id=e.profiles()[0].id;
 const pool=h.context.KrillMobile.transport.pool({endpoint:e});pool.sync();await flush();
 const entry=pool.get(id),owner=entry.transport,main=h.sockets.at(-1);
 await h.advance(main,4);h.response(main,{sessions:[]});await flush();assert.equal(owner.ready,true);
 const marker="unknown-mutation-preserved",mutation=owner.rpc({action:"prompt",text:marker});mutation.catch(()=>{});await flush();
 main.emit("close",{code:1008});await assert.rejects(mutation);await flush();
 assert.equal(owner.blocked,true);assert.equal(owner.updateBusy,true);
 const app=fs.readFileSync(path.join(__dirname,"../src/mobile/assets/app.js"),"utf8"),line=app.split("\n").find(l=>l.includes("async function pairWith("));
 Object.assign(h.context,{pool,update(){},toast(){},t:s=>s,sheet(){},startPairing(){}});
 vm.runInContext(line+";globalThis.reviewPair=pairWith;",h.context);
 const attaching=h.context.reviewPair("",e.adopt(secureQr()));
 const probe=await candidateReply(h,{device_id:"phone",credential:null,desktop:desktopManifest()});await attaching;await flush();
 const recovery=h.sockets.at(-1);assert.notEqual(recovery,probe,"must begin recovery on the newly proved route");
 await h.advance(recovery,4);h.response(recovery,{sessions:[]});await flush();
 assert.equal(pool.get(id).transport,owner);assert.equal(owner.ready,true);assert.equal(owner.updateBusy,true,"adding a route is not acknowledgement of uncertain mutation delivery");
 const submissions=h.sockets.flatMap(socket=>socket.sent).filter(bytes=>typeof bytes!=="string"&&new TextDecoder().decode(bytes).includes(marker));
 assert.equal(submissions.length,1,"must not replay the previously submitted mutation on recovery");
 assert.equal(e.credentials.device_id,saved.device_id);assert.equal(e.credentials.credential,saved.credential);
 pool.close();h.transport.close();
});

test("attaching a proved backup keeps a healthy retained owner and its relay socket", async () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) },
    h = harness(saved, undefined, true), e = h.endpoint, id = e.profiles()[0].id,
    pool = h.context.KrillMobile.transport.pool({ endpoint: e });
  pool.sync(); await flush();
  const owner = pool.get(id).transport, main = h.sockets.at(-1);
  await h.advance(main, 4); h.response(main, { etag: "healthy-data", sessions: [] }); await flush();
  assert.equal(owner.ready, true);
  const app = fs.readFileSync(path.join(__dirname, "../src/mobile/assets/app.js"), "utf8"),
    line = app.split("\n").find((value) => value.includes("async function pairWith("));
  Object.assign(h.context, { pool, update() {}, toast() {}, t: (value) => value, sheet() {}, startPairing() {} });
  vm.runInContext(line + ";globalThis.pairThroughApp=pairWith;", h.context);
  const attaching = h.context.pairThroughApp("", e.adopt(secureQr())),
    probe = await candidateReply(h, { device_id: "phone", credential: null, desktop: desktopManifest() });
  await attaching; await flush();
  assert.equal(pool.get(id).transport, owner);
  assert.equal(owner.ready, true);
  assert.equal(main.closed, undefined);
  assert.equal(h.sockets.at(-1), probe, "a healthy owner opens no replacement main channel");
  assert.equal(e.credentials.credential, saved.credential);
  assert.equal(e.profiles().length, 1);
  const refreshing = owner.refresh(); await flush();
  assert(new TextDecoder().decode(main.sent.at(-1)).includes('"since":"healthy-data"'), "healthy route attachment retains its snapshot revision");
  h.response(main, { unchanged: true, etag: "healthy-data" }); await refreshing;
  assert.equal(pool.get(id).snapshot.etag, "healthy-data");
  pool.close(); h.transport.close();
});

test("authenticated route recovery restores the cleared desktop snapshot when daemon data is unchanged", async () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) },
    h = harness(saved, undefined, true), e = h.endpoint, id = e.profiles()[0].id,
    pool = h.context.KrillMobile.transport.pool({ endpoint: e }),
    snapshot = { etag: "same-desktop-data", sessions: [{ id: 7, title: "Restored session" }] };
  pool.sync(); await flush();
  const owner = pool.get(id).transport, main = h.sockets.at(-1);
  await h.advance(main, 4); h.response(main, snapshot); await flush();
  assert.equal(pool.get(id).snapshot.sessions[0].id, 7);
  main.emit("close", { code: 1008 }); await flush();
  assert.equal(pool.get(id).snapshot, null, "authentication rejection clears the displayed desktop snapshot");
  const app = fs.readFileSync(path.join(__dirname, "../src/mobile/assets/app.js"), "utf8"),
    line = app.split("\n").find((value) => value.includes("async function pairWith("));
  Object.assign(h.context, { pool, update() {}, toast() {}, t: (value) => value, sheet() {}, startPairing() {} });
  vm.runInContext(line + ";globalThis.pairThroughApp=pairWith;", h.context);
  const attaching = h.context.pairThroughApp("", e.adopt(secureQr()));
  await candidateReply(h, { device_id: "phone", credential: null, desktop: desktopManifest() });
  await attaching; await flush();
  const recovery = h.sockets.at(-1);
  await h.advance(recovery, 4);
  const request = recovery.sent.find((frame) => typeof frame !== "string" && new TextDecoder().decode(frame).includes('"action":"snapshot"'));
  assert(request, "the recovered owner requests its desktop snapshot");
  const query = new TextDecoder().decode(request), unchanged = query.includes('"since":"same-desktop-data"');
  h.response(recovery, unchanged ? { unchanged: true, etag: snapshot.etag } : snapshot);
  await flush();
  assert.equal(pool.get(id).snapshot?.sessions?.[0]?.id, 7, "the retained owner restores the session displayed before rejection");
  assert(!unchanged, "a cleared consumer requires a full snapshot even when the daemon etag is unchanged");
  assert.equal(pool.get(id).transport, owner);
  assert.equal(owner.ready, true);
  assert.equal(e.credentials.credential, saved.credential);
  pool.close(); h.transport.close();
});

test("explicit retry after authentication rejection restores the cleared desktop snapshot", async () => {
  const saved = { device_id: "phone", credential: "11".repeat(32), rid: "a".repeat(32) },
    h = harness(saved, undefined, true), e = h.endpoint, id = e.profiles()[0].id,
    pool = h.context.KrillMobile.transport.pool({ endpoint: e }),
    snapshot = { etag: "unchanged-manual-retry", sessions: [{ id: 9 }] };
  pool.sync(); await flush();
  const owner = pool.get(id).transport, main = h.sockets.at(-1);
  await h.advance(main, 4); h.response(main, snapshot); await flush();
  main.emit("close", { code: 1008 }); await flush();
  assert.equal(pool.get(id).snapshot, null);
  owner.reconnect(); await flush();
  const recovery = h.sockets.at(-1);
  await h.advance(recovery, 4);
  const request = recovery.sent.find((frame) => typeof frame !== "string" && new TextDecoder().decode(frame).includes('"action":"snapshot"'));
  assert(request);
  const unchanged = new TextDecoder().decode(request).includes('"since":"unchanged-manual-retry"');
  h.response(recovery, unchanged ? { unchanged: true, etag: snapshot.etag } : snapshot);
  await flush();
  assert.equal(pool.get(id).snapshot?.sessions?.[0]?.id, 9, "manual retry restores the snapshot that authentication rejection cleared");
  assert(!unchanged);
  assert.equal(pool.get(id).transport, owner);
  pool.close(); h.transport.close();
});
