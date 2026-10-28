// Browser-only, opt-in operational diagnostics. Never serialize exceptions or URLs.
(() => {
  const M = (globalThis.KrillMobile ||= {});
  if (M.diagnostics) return;
  const ENDPOINT = "https://errors.krill.page/v1/mobile-events",
    KEY = "krill-mobile-error-reporting-v1",
    OUTBOX = "krill-mobile-error-outbox-v1";
  const phases = new Set([
    "idle",
    "discovery",
    "socket_open",
    "challenge",
    "authentication",
    "ipc",
    "snapshot",
    "rpc",
    "runtime",
    "resource",
  ]);
  const routes = new Set(["relay", "tunnel", "tailscale", "direct", "unknown"]);
  const reasons = new Set([
    "websocket_error",
    "websocket_closed",
    "timeout",
    "http_error",
    "auth_required",
    "auth_rejected",
    "protocol_error",
    "network_offline",
    "runtime_error",
    "unhandled_rejection",
    "resource_error",
    "unknown",
  ]);
  const codes = new Set([
    "mobile_connect_failed",
    "mobile_discovery_failed",
    "mobile_auth_failed",
    "mobile_rpc_failed",
    "mobile_runtime_error",
    "mobile_unhandled_rejection",
    "mobile_asset_failed",
  ]);
  const names = new Set([
    "Error",
    "TypeError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "URIError",
    "EvalError",
    "DOMException",
  ]);
  const files = new Set([
    "index.html",
    "app.js",
    "client.js",
    "diagnostics.js",
    "crypto.js",
    "storage.js",
    "terminal.js",
    "updates.js",
    "i18n.js",
    "qr.js",
    "sw.js",
    ...["home", "session", "chat", "settings", "workspaces", "details"].map(
      (n) => `screens/${n}.js`,
    ),
  ]);
  const number = (n, min, max) => Number.isInteger(n) && n >= min && n <= max;
  const id = () => {
    try {
      return Array.from(crypto.getRandomValues(new Uint8Array(16)), (n) =>
        n.toString(16).padStart(2, "0"),
      ).join("");
    } catch {
      return null;
    }
  };
  const page = id(),
    dnt = () =>
      [navigator.doNotTrack, globalThis.doNotTrack].some(
        (v) => v === "1" || v === "yes",
      );
  let consent = false;
  try {
    consent = localStorage.getItem(KEY) === "on";
  } catch {}
  let queue = [],
    recent = [],
    seen = new Map(),
    timer = null,
    inflight = null,
    retryNotBefore = 0,
    epoch = 0;
  const enabled = () => consent && !dnt();
  const build =
    /^\/m\/([0-9a-f]{16})\//.exec(location.pathname)?.[1] || "unversioned";
  const ua = navigator.userAgent || "",
    os = /Android/i.test(ua)
      ? "android"
      : /iPhone|iPad|iPod/i.test(ua) ||
          (/Macintosh/i.test(ua) && navigator.maxTouchPoints > 1)
        ? "ios"
        : /Mac/i.test(ua)
          ? "macos"
          : /Windows/i.test(ua)
            ? "windows"
            : /Linux/i.test(ua)
              ? "linux"
              : "other";
  const browser = /Edg/i.test(ua)
    ? "edge"
    : /Firefox|FxiOS/i.test(ua)
      ? "firefox"
      : /Chrome|CriOS/i.test(ua)
        ? "chrome"
        : /Safari/i.test(ua)
          ? "safari"
          : "other";
  function fields(input = {}) {
    const out = {};
    for (const [key, set] of [
      ["phase", phases],
      ["route", routes],
      ["reason", reasons],
      ["error_name", new Set([...names, "Unknown"])],
      ["source_file", files],
    ])
      if (set.has(input[key])) out[key] = input[key];
    for (const [key, min, max] of [
      ["elapsed_ms", 0, 300000],
      ["retry_count", 0, 10000],
      ["ws_code", 1000, 4999],
      ["http_status", 100, 599],
      ["line", 0, 1000000],
      ["column", 0, 1000000],
    ])
      if (number(input[key], min, max)) out[key] = input[key];
    for (const key of ["attempt_id", "page_id"])
      if (/^[0-9a-f]{32}$/.test(input[key])) out[key] = input[key];
    if (Array.isArray(input.stack_frames))
      out.stack_frames = input.stack_frames
        .slice(0, 8)
        .filter(
          (f) =>
            f &&
            files.has(f.source_file) &&
            number(f.line, 0, 1000000) &&
            number(f.column, 0, 1000000),
        )
        .map((f) => ({
          source_file: f.source_file,
          line: f.line,
          column: f.column,
        }));
    return out;
  }
  // Rebuild disk input using the same allowlists; never trust stored event objects.
  function canonical(event) {
    if (
      !event ||
      !codes.has(event.error_code) ||
      !/^[0-9a-f]{32}$/.test(event.event_id)
    )
      return null;
    const input = event.diagnostics || {},
      diagnostics = fields(input);
    if (!/^[0-9a-f]{32}$/.test(diagnostics.page_id || "")) return null;
    if (typeof input.online === "boolean") diagnostics.online = input.online;
    if (
      ["visible", "hidden", "prerender", "unknown"].includes(input.visibility)
    )
      diagnostics.visibility = input.visibility;
    if (
      ["safari", "chrome", "firefox", "edge", "other"].includes(input.browser)
    )
      diagnostics.browser = input.browser;
    if (Array.isArray(input.breadcrumbs))
      diagnostics.breadcrumbs = input.breadcrumbs.slice(-12).map(crumb);
    const clean = {
      event_id: event.event_id,
      process: "krill-mobile",
      kind: "operation_error",
      error_code: event.error_code,
      release: "web",
      build_id: /^[0-9a-f]{16}$/.test(event.build_id)
        ? event.build_id
        : "unversioned",
      os: ["ios", "android", "macos", "windows", "linux", "other"].includes(
        event.os,
      )
        ? event.os
        : "other",
      arch: "other",
      source_loc: null,
      hang_bucket_ms: null,
      native: null,
      diagnostics,
    };
    return new TextEncoder().encode(JSON.stringify(clean)).length <= 4096
      ? clean
      : null;
  }
  function persist() {
    try {
      if (enabled() && queue.length)
        localStorage.setItem(
          OUTBOX,
          JSON.stringify({
            schema_version: 1,
            items: queue,
            retry_at: retryNotBefore,
          }),
        );
      else localStorage.removeItem(OUTBOX);
    } catch {}
  }
  function restore() {
    try {
      if (enabled()) {
        const raw = localStorage.getItem(OUTBOX);
        if (raw && raw.length <= 160000) {
          const saved = JSON.parse(raw);
          if (number(saved.retry_at, 0, Date.now() + 1800000))
            retryNotBefore = saved.retry_at;
          if (saved.schema_version === 1 && Array.isArray(saved.items))
            queue = saved.items
              .slice(-32)
              .filter(
                (q) =>
                  q &&
                  number(q.at, 0, Date.now()) &&
                  Date.now() - q.at < 1800000 &&
                  number(q.tries, 0, 3),
              )
              .map((q) => ({
                event: canonical(q.event),
                at: q.at,
                tries: q.tries,
              }))
              .filter((q) => q.event);
        }
      }
    } catch {}
    persist();
    schedule();
  }
  function crumb(input) {
    const f = fields(input),
      out = {
        phase: f.phase || "idle",
        reason: f.reason || "unknown",
        elapsed_ms: f.elapsed_ms || 0,
      };
    for (const k of ["ws_code", "http_status"])
      if (f[k] !== undefined) out[k] = f[k];
    return out;
  }
  function clear() {
    epoch++;
    clearTimeout(timer);
    timer = null;
    inflight?.abort();
    inflight = null;
    queue = [];
    retryNotBefore = 0;
    seen.clear();
    persist();
  }
  function setEnabled(value) {
    clear();
    consent = value === true && !dnt();
    try {
      localStorage.setItem(KEY, consent ? "on" : "off");
    } catch {
      // Removing the key preserves default-off even when replacing "on" with
      // the longer "off" value would exceed the browser's storage quota.
      if (!consent)
        try {
          localStorage.removeItem(KEY);
        } catch {}
    }
    return enabled();
  }
  function schedule(delay = 0) {
    if (!enabled() || timer !== null || inflight || !queue.length) return;
    // Expire old reports even when the server asks for a longer cooldown.
    const expires = Math.max(
      0,
      Math.min(...queue.map((q) => q.at + 1800000)) - Date.now(),
    );
    const wait = Math.min(
      expires,
      Math.max(delay, retryNotBefore - Date.now(), 0),
    );
    timer = setTimeout(() => {
      timer = null;
      void flush();
    }, wait);
  }
  async function flush() {
    if (!enabled()) {
      clear();
      return;
    }
    if (inflight) return;
    queue = queue
      .filter((q) => Date.now() - q.at < 1800000 && q.tries < 4)
      .map((q) => ({ ...q, event: canonical(q.event) }))
      .filter((q) => q.event);
    persist();
    if (!queue.length) return;
    if (Date.now() < retryNotBefore) {
      schedule();
      return;
    }
    const batch = queue.slice(0, 16),
      controller = new AbortController(),
      version = epoch;
    inflight = controller;
    const timeout = setTimeout(() => controller.abort(), 8000);
    for (const item of batch) item.tries++;
    persist();
    try {
      const response = await fetch(ENDPOINT, {
        method: "POST",
        mode: "cors",
        credentials: "omit",
        referrerPolicy: "no-referrer",
        cache: "no-store",
        redirect: "error",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          schema_version: 1,
          events: batch.map((q) => q.event),
        }),
        signal: controller.signal,
      });
      if (version !== epoch || !enabled()) return;
      const retryAfter = response.headers?.get?.("Retry-After")?.trim();
      if (
        typeof retryAfter === "string" &&
        /^\d{1,5}$/.test(retryAfter) &&
        Number(retryAfter) <= 86400
      )
        retryNotBefore = Math.max(
          retryNotBefore,
          Date.now() + Math.min(1800000, Number(retryAfter) * 1000),
        );
      if (!response.ok) throw Error();
      const ack = await response.json();
      if (version !== epoch || !enabled()) return;
      if (
        ack.schema_version === 1 &&
        Array.isArray(ack.accepted_ids) &&
        Array.isArray(ack.duplicate_ids)
      ) {
        const ids = new Set([...ack.accepted_ids, ...ack.duplicate_ids]);
        queue = queue.filter((q) => !ids.has(q.event.event_id));
      }
    } catch {
      /* Collector failures are intentionally never reported. */
    } finally {
      clearTimeout(timeout);
      if (version === epoch) {
        inflight = null;
        persist();
        if (!enabled()) clear();
        else
          schedule(
            Math.min(60000, 1000 * 2 ** Math.max(...batch.map((q) => q.tries))),
          );
      }
    }
  }
  function report(code, input = {}) {
    try {
      if (!codes.has(code)) return;
      const clean = fields(input);
      recent.push({ code, ...clean });
      recent = recent.slice(-32);
      if (!enabled() || !page) return;
      if (inflight?.signal.aborted) return;
      const now = Date.now();
      for (const [key, at] of seen) if (now - at > 60000) seen.delete(key);
      const fingerprint = JSON.stringify([
        code,
        clean.attempt_id,
        clean.phase,
        clean.route,
        clean.reason,
        clean.ws_code,
        clean.http_status,
        clean.error_name,
        clean.source_file,
        clean.line,
      ]);
      if (seen.has(fingerprint)) return;
      const eventId = id();
      if (!eventId) return;
      seen.set(fingerprint, now);
      if (seen.size > 128) seen.delete(seen.keys().next().value);
      const diagnostics = {
        ...clean,
        page_id: page,
        online: navigator.onLine !== false,
        visibility: ["visible", "hidden", "prerender"].includes(
          document.visibilityState,
        )
          ? document.visibilityState
          : "unknown",
        browser,
      };
      if (Array.isArray(input.breadcrumbs))
        diagnostics.breadcrumbs = input.breadcrumbs.slice(-12).map(crumb);
      const event = {
        event_id: eventId,
        process: "krill-mobile",
        kind: "operation_error",
        error_code: code,
        release: "web",
        build_id: build,
        os,
        arch: "other",
        source_loc: null,
        hang_bucket_ms: null,
        native: null,
        diagnostics,
      };
      if (new TextEncoder().encode(JSON.stringify(event)).length > 4096) return;
      queue.push({ event, at: now, tries: 0 });
      queue = queue.slice(-32);
      persist();
      schedule();
    } catch {
      /* Diagnostics must never interfere with the application. */
    }
  }
  function attempt(retry = 0) {
    const started = Date.now(),
      attemptId = id();
    let state = { phase: "socket_open", route: "unknown" },
      breadcrumbs = [];
    const metadata = (extra) => ({
      ...state,
      attempt_id: attemptId,
      elapsed_ms: Math.min(300000, Math.max(0, Date.now() - started)),
      retry_count: Math.min(10000, Math.max(0, retry)),
      ...extra,
    });
    return {
      mark(phase, extra = {}) {
        state = { ...state, ...fields(extra), phase };
        breadcrumbs.push(crumb(metadata(extra)));
        breadcrumbs = breadcrumbs.slice(-12);
      },
      report(code, extra = {}) {
        report(code, { ...metadata(extra), breadcrumbs });
      },
    };
  }
  function assetFile(value) {
    try {
      const u = new URL(value, location.href),
        base = /^\/m\/[0-9a-f]{16}\//.exec(location.pathname)?.[0] || "/";
      if (u.origin !== location.origin || !u.pathname.startsWith(base)) return;
      return u.pathname.slice(base.length) || "index.html";
    } catch {}
  }
  function source(value) {
    const file = assetFile(value);
    if (files.has(file)) return file;
  }
  const resources = new Set([
    "app.css",
    "manifest.json",
    "icon.svg",
    "icon-192.png",
    "icon-512.png",
    "icon-maskable-512.png",
    "apple-touch-icon.png",
  ]);
  const exception = (error) => {
    const out = { error_name: names.has(error?.name) ? error.name : "Unknown" },
      frames = [];
    if (typeof error?.stack === "string")
      for (const line of error.stack.slice(0, 16000).split("\n").slice(0, 32)) {
        const match = /(https?:\/\/[^\s)]+):(\d+):(\d+)\)?$/.exec(line.trim());
        if (!match) continue;
        const file = source(match[1]),
          row = Number(match[2]),
          column = Number(match[3]);
        if (file && number(row, 0, 1000000) && number(column, 0, 1000000))
          frames.push({ source_file: file, line: row, column });
        if (frames.length === 8) break;
      }
    if (frames.length) out.stack_frames = frames;
    return out;
  };
  M.diagnostics = {
    enabled,
    setEnabled,
    get dnt() {
      return dnt();
    },
    attempt,
    report,
    exception,
    source,
    get recent() {
      return recent.map((e) => JSON.parse(JSON.stringify(e)));
    },
  };
  globalThis.addEventListener?.(
    "error",
    (event) => {
      try {
        if (event.target && event.target !== globalThis) {
          const asset = assetFile(event.target.src || event.target.href),
            file = files.has(asset) ? asset : undefined;
          if (file || resources.has(asset))
            report("mobile_asset_failed", {
              phase: "resource",
              reason: "resource_error",
              source_file: file,
            });
          return;
        }
        report("mobile_runtime_error", {
          phase: "runtime",
          reason: "runtime_error",
          ...exception(event.error),
          source_file: source(event.filename),
          line: event.lineno,
          column: event.colno,
        });
      } catch {}
    },
    true,
  );
  globalThis.addEventListener?.("unhandledrejection", (event) => {
    try {
      report("mobile_unhandled_rejection", {
        phase: "runtime",
        reason: "unhandled_rejection",
        ...exception(event.reason),
      });
    } catch {}
  });
  globalThis.addEventListener?.("online", () => schedule());
  globalThis.addEventListener?.("storage", (event) => {
    if (event.key !== KEY && event.key !== null) return;
    let value = false;
    try {
      value = localStorage.getItem(KEY) === "on";
    } catch {}
    if (!value) {
      consent = false;
      clear();
    } else if (!consent) {
      clear();
      consent = true;
    }
  });
  restore();
})();
