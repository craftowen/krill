(() => {
  const M = (globalThis.KrillMobile ||= {}),
    C = MkCrypto,
    decode = new TextDecoder(),
    FEATURES = (1n << 64n) - 1n,
    v = (value) => {
      let n = BigInt(value),
        out = [];
      do {
        let b = Number(n & 127n);
        ((n >>= 7n), out.push(b | (n ? 128 : 0)));
      } while (n);
      return Uint8Array.from(out);
    },
    text = (s) => {
      const bytes = C.enc.encode(s);
      return C.concat(v(bytes.length), bytes);
    },
    frame = (kind, body) => {
      const h = new Uint8Array(5);
      return (
        new DataView(h.buffer).setUint32(0, body.length + 1, !0),
        (h[4] = kind),
        C.concat(h, body)
      );
    },
    reader = (bytes) => {
      let at = 0;
      return {
        u() {
          let n = 0n,
            shift = 0n;
          for (let i = 0; i < 10; i++) {
            if (at >= bytes.length) throw Error("Truncated response");
            const b = bytes[at++];
            if (((n |= BigInt(b & 127) << shift), !(b & 128))) return n;
            shift += 7n;
          }
          throw Error("Invalid response");
        },
        text() {
          const n = Number(this.u());
          if (n > bytes.length - at) throw Error("Truncated response");
          const s = decode.decode(bytes.slice(at, at + n));
          return ((at += n), s);
        },
      };
    },
    relayAccount = (M.relayAccount = {
      required: !1,
      accountMismatch: !1,
      signIn() {
        const path = location.pathname + location.search;
        rememberRelayReturn();
        location.assign("/auth?return_to=" + encodeURIComponent(path));
      },
      async logout() {
        rememberRelayReturn();
        const session = await fetch("/auth/session", {
          credentials: "same-origin",
          cache: "no-store",
        });
        if (session.status === 401) {
          ((relayAccount.required = !0), location.reload());
          return;
        }
        if (!session.ok) throw Error("Could not sign out. Try again.");
        const data = await session.json();
        if (
          !(
            await fetch("/auth/logout", {
              method: "POST",
              credentials: "same-origin",
              cache: "no-store",
              headers: {
                "Content-Type": "application/json",
                "x-krill-csrf": data.csrf_token,
              },
              body: "{}",
            })
          ).ok
        )
          throw Error("Could not sign out. Try again.");
        ((relayAccount.required = !0), location.reload());
      },
    }),
    relayFetch = async (url, { isCurrent, ...options } = {}) => {
      const response = await fetch(url, {
        ...options,
        credentials: "same-origin",
      });
      return (
        (response.status === 401 || response.status === 403) &&
          !options.signal?.aborted &&
          (!isCurrent || isCurrent()) &&
          ((relayAccount.required = !0),
          (relayAccount.accountMismatch = response.status === 403),
          globalThis.dispatchEvent?.(new Event("krill-relay-auth-required"))),
        response
      );
    },
    AUTH = /^([0-9a-f]{32,128})\.([0-9a-f]{64})(?:\.([0-9a-f]{32}))?$/,
    handoff = (c) => {
      const token = [c?.device_id, c?.credential, c?.rid]
        .filter(Boolean)
        .join(".");
      return AUTH.test(token) ? token : null;
    };
  function clearRelayReturn() {
    relayAccount.fragment = null;
    try {
      globalThis.sessionStorage?.removeItem("krill-relay-return");
    } catch {}
  }
  function rememberRelayReturn() {
    const path = location.pathname + location.search;
    try {
      if (relayAccount.fragment && !globalThis.sessionStorage)
        throw Error("No session storage");
      globalThis.sessionStorage?.setItem(
        "krill-relay-return",
        JSON.stringify({
          path,
          fragment: relayAccount.fragment || location.hash,
          at: Date.now(),
        }),
      );
    } catch {
      throw Error(
        "Could not preserve pairing. Allow session storage and try again.",
      );
    }
  }
  function directSocket(value) {
    const origin = new URL(value),
      loopback =
        origin.hostname === "localhost" ||
        origin.hostname === "[::1]" ||
        /^127\./.test(origin.hostname);
    if (loopback && origin.hostname !== location.hostname)
      throw Error(
        "Saved desktop address points to this phone. Scan a Tailscale QR code from desktop Settings → Mobile.",
      );
    if (location.protocol === "https:" && origin.protocol !== "https:")
      throw Error(
        "Open the Tailscale QR code directly to use the private-network backup.",
      );
    return `${origin.protocol === "https:" ? "wss:" : "ws:"}//${origin.host}/ws`;
  }
  function bootstrap() {
    const match = /^\/m\/([0-9a-f]{16})\/$/.exec(location.pathname),
      fixed = !!match,
      hash = match?.[1];
    let returnedFragment = location.hash;
    try {
      const raw = globalThis.sessionStorage?.getItem("krill-relay-return"),
        saved2 = raw && JSON.parse(raw);
      saved2 &&
        saved2.path === location.pathname + location.search &&
        Date.now() - saved2.at < 6e5 &&
        ((returnedFragment = saved2.fragment),
        globalThis.sessionStorage.removeItem("krill-relay-return"));
    } catch {}
    const parameters = new URLSearchParams(location.search),
      fragment = new URLSearchParams(returnedFragment.slice(1)),
      ticket = fragment.get("pair"),
      secret = fragment.get("key"),
      STORE = "krill-mobile-v2",
      HOSTS = "krill-mobile-hosts-v1",
      REMOVED = "krill-mobile-removed-v1:",
      removalKey = (host) =>
        REMOVED +
        C.hex(
          C.sha256(
            C.enc.encode(
              JSON.stringify([host.id, host.device_id, host.credential]),
            ),
          ),
        ),
      removed = (host) => {
        try {
          return !!host && localStorage.getItem(removalKey(host)) === "1";
        } catch {
          return !1;
        }
      },
      validHost = (h) =>
        typeof h?.id == "string" &&
        typeof h.device_id == "string" &&
        typeof h.credential == "string";
    let saved, book;
    try {
      saved = JSON.parse(localStorage.getItem(STORE) || "null");
    } catch {
      saved = null;
    }
    try {
      book = JSON.parse(localStorage.getItem(HOSTS) || "null");
    } catch {}
    const hosts = Array.isArray(book?.hosts)
      ? book.hosts.filter((h) => validHost(h) && !removed(h))
      : [];
    let active = hosts.find((h) => h.id === book?.active) || hosts[0] || null;
    Array.isArray(book?.hosts) && (saved = active);
    const pair =
        ticket && secret && !(fragment.has("install") && saved?.credential)
          ? { ticket, secret }
          : null,
      auth = parameters.get("auth"),
      sensitive = ["pair", "key", "t", "r", "install"],
      consumed = sensitive.some((key) => fragment.has(key));
    for (const key of [...sensitive, "auth"]) parameters.delete(key);
    (history.replaceState(
      {},
      "",
      location.pathname +
        (parameters.size ? "?" + parameters : "") +
        (consumed ? "" : location.hash),
    ),
      (relayAccount.fragment = pair ? returnedFragment : null));
    let credentials = pair ? null : saved,
      directOrigin = location.origin,
      destinationFixed = fixed,
      pairingPending = !!pair;
    const hostId = (c) =>
      c.rid
        ? "relay:" + c.rid
        : c.url
          ? "tunnel:" + c.url
          : "direct:" + (c.origin || location.origin);
    let updateHandler = null,
      updatePending = !1;
    const hostListeners = new Set();
    let storageFailed = !1;
    const persist = () => {
        try {
          const latest = JSON.parse(localStorage.getItem(HOSTS) || "null"),
            live = hosts.filter((h) => !removed(h));
          for (const host of latest?.hosts || [])
            validHost(host) &&
              !removed(host) &&
              !live.some((h) => h.id === host.id) &&
              live.push(host);
          (hosts.splice(0, hosts.length, ...live),
            localStorage.setItem(
              HOSTS,
              JSON.stringify({
                active: active && !removed(active) ? active.id : null,
                hosts,
              }),
            ),
            (storageFailed = !1));
        } catch {
          storageFailed = !0;
        }
      },
      stillSaved = (host) => {
        if (!host || removed(host)) return !1;
        try {
          const latest = JSON.parse(localStorage.getItem(HOSTS) || "null");
          if (Array.isArray(latest?.hosts))
            return latest.hosts.some(
              (h) =>
                h.id === host.id &&
                h.device_id === host.device_id &&
                h.credential === host.credential,
            );
        } catch {}
        return !0;
      };
    if (
      (globalThis.addEventListener?.("storage", (event) => {
        if (pairingPending || (event.key !== null && event.key !== HOSTS))
          return;
        let book2;
        try {
          book2 = JSON.parse(localStorage.getItem(HOSTS) || "null");
        } catch {
          return;
        }
        const next = Array.isArray(book2?.hosts)
          ? book2.hosts.filter((h) => validHost(h) && !removed(h))
          : [];
        (hosts.splice(0, hosts.length, ...next),
          (active =
            hosts.find((h) => h.id === active?.id) ||
            hosts.find((h) => h.id === book2?.active) ||
            hosts[0] ||
            null),
          (saved = credentials = active),
          (rid = active?.rid),
          (url = active?.url),
          (directOrigin = active?.origin || location.origin),
          (destinationFixed = active?.fixed ?? fixed),
          (useRelay = validRid(rid)));
        for (const fn of hostListeners) fn();
      }),
      saved && !active)
    ) {
      ((active = {
        ...saved,
        id: hostId(saved),
        origin: saved.origin || location.origin,
        fixed: saved.fixed ?? fixed,
        name: "Desktop",
      }),
        hosts.push(active));
      try {
        persist();
      } catch {}
    }
    (active &&
      ((directOrigin = active.origin || location.origin),
      (destinationFixed = active.fixed ?? fixed)),
      pair && ((directOrigin = location.origin), (destinationFixed = fixed)));
    const seed = AUTH.exec(auth || "");
    if (!pair && !saved && seed && (!fixed || seed[3])) {
      credentials = {
        device_id: seed[1],
        credential: seed[2],
        ...(fixed && { rid: seed[3], url: null, hash }),
      };
      try {
        localStorage.setItem(STORE, JSON.stringify(credentials));
      } catch {}
    }
    let rid = pair ? fragment.get("r") : credentials?.rid,
      url = pair
        ? fragment.get("t")
          ? "https://" + fragment.get("t")
          : null
        : credentials?.url;
    const validUrl = (value) =>
        typeof value == "string" &&
        /^https:\/\/[a-z0-9-]+\.trycloudflare\.com$/.test(value),
      validRid = (value) =>
        typeof value == "string" && /^[0-9a-f]{32}$/.test(value);
    let useRelay = validRid(rid);
    const relayUrl = () =>
      `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/r/${rid}?v=${hash}`;
    return (
      navigator.serviceWorker?.register("/sw.js").catch(() => {}),
      {
        parameters,
        pair,
        credentials,
        fixed,
        replacing: !!(pair && saved),
        get storageFailed() {
          return storageFailed;
        },
        forHost(id) {
          const found = hosts.find((h) => h.id === id && stillSaved(h));
          if (!found) throw Error("Desktop is no longer saved.");
          const source = { ...found };
          let auth2 = { ...source },
            peerUrl = source.url,
            relay = validRid(source.rid);
          const current = () => hosts.find((h) => h.id === id && stillSaved(h)),
            peerRid = source.rid,
            peerFixed = source.fixed ?? fixed,
            save = (patch) => {
              const host = current();
              if (
                !host ||
                host.device_id !== auth2.device_id ||
                host.credential !== auth2.credential
              )
                return !1;
              if (
                Object.entries(patch).every(
                  ([key, value]) => host[key] === value,
                )
              )
                return !0;
              if (
                (Object.assign(host, patch),
                (auth2 = { ...host }),
                active?.id === id)
              ) {
                active = saved = credentials = host;
                try {
                  localStorage.setItem(STORE, JSON.stringify(host));
                } catch {
                  storageFailed = !0;
                }
              }
              return (persist(), !0);
            };
          return {
            get credentials() {
              return auth2;
            },
            socketUrl() {
              if (!peerFixed)
                return directSocket(source.origin || location.origin);
              if (relay && validRid(peerRid))
                return `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/r/${peerRid}?v=${hash}`;
              if (validUrl(peerUrl))
                return `wss://${peerUrl.slice(8)}/ws?v=${hash}`;
              throw Error("Desktop address is unavailable.");
            },
            authenticated(reply) {
              if (reply.device_id && reply.device_id !== source.device_id)
                throw Error("Desktop identity changed.");
              return (
                save({
                  ...(reply.credential ? { credential: reply.credential } : {}),
                  url: peerUrl,
                  preferred_route: peerFixed
                    ? relay
                      ? "relay"
                      : "tunnel"
                    : "direct",
                }),
                auth2
              );
            },
            describeHost(info) {
              const patch = {};
              (info?.display_name && (patch.name = info.display_name),
                info?.platform && (patch.platform = info.platform),
                save(patch));
            },
            fallback(attempted) {
              peerFixed &&
                attempted === this.socketUrl() &&
                (!relay && validRid(peerRid)
                  ? (relay = !0)
                  : relay && validUrl(peerUrl) && (relay = !1));
            },
            async locate({
              isCurrent = () => !0,
              signal,
              diagnosticsTrace: trace,
            } = {}) {
              if (!peerFixed || !validRid(peerRid)) return !1;
              trace?.mark("discovery", { route: "relay" });
              let timedOut = false;
              const controller = new AbortController(),
                abort = () => controller.abort(),
                timeout = setTimeout(() => {
                  timedOut = true;
                  abort();
                }, 5e3);
              (signal?.addEventListener("abort", abort, { once: !0 }),
                signal?.aborted && abort());
              try {
                const response = await relayFetch(
                  "/r/" + encodeURIComponent(peerRid),
                  { cache: "no-store", signal: controller.signal },
                );
                if (isCurrent() && !signal?.aborted) {
                  trace?.mark("discovery", { http_status: response.status });
                  if (!response.ok)
                    trace?.report("mobile_discovery_failed", {
                      reason:
                        response.status === 401 || response.status === 403
                          ? "auth_required"
                          : "http_error",
                      http_status: response.status,
                    });
                }
                if (
                  !response.ok ||
                  controller.signal.aborted ||
                  !isCurrent() ||
                  !current()
                )
                  return !1;
                const found2 = await response.json();
                if (controller.signal.aborted || !isCurrent()) return !1;
                typeof found2.hash == "string" &&
                  /^[0-9a-f]{16}$/.test(found2.hash) &&
                  (validUrl(found2.url) || found2.url === "" || found2.relay) &&
                  ((peerUrl = validUrl(found2.url) ? found2.url : null),
                  (relay = !peerUrl),
                  save({ url: peerUrl }));
              } catch (error) {
                if (isCurrent() && !signal?.aborted)
                  trace?.report("mobile_discovery_failed", {
                    reason: timedOut ? "timeout" : "unknown",
                    ...M.diagnostics?.exception(error),
                  });
              } finally {
                (clearTimeout(timeout),
                  signal?.removeEventListener("abort", abort));
              }
              return !1;
            },
          };
        },
        onHostsChanged(fn) {
          return (hostListeners.add(fn), () => hostListeners.delete(fn));
        },
        reloadHosts() {
          let latest;
          try {
            latest = JSON.parse(localStorage.getItem(HOSTS) || "null");
          } catch {
            return !1;
          }
          const live = (
            Array.isArray(latest?.hosts) ? latest.hosts : []
          ).filter((h) => validHost(h) && !removed(h));
          hosts.splice(0, hosts.length, ...live);
          const next =
            hosts.find((h) => h.id === active?.id) ||
            hosts.find((h) => h.id === latest?.active) ||
            hosts[0] ||
            null;
          return (
            (active = saved = credentials = next),
            (rid = next?.rid),
            (url = next?.url),
            (directOrigin = next?.origin || location.origin),
            (destinationFixed = next?.fixed ?? fixed),
            (useRelay = validRid(rid)),
            (updatePending = !1),
            !0
          );
        },
        profiles() {
          return hosts
            .filter((h) => !removed(h))
            .map((h) => ({
              id: h.id,
              name: h.alias || h.name || "Desktop",
              platform: h.platform,
              endpoint: h.fixed
                ? h.rid
                  ? "Relay \xB7 " + h.rid.slice(0, 8)
                  : h.url
                : h.origin,
              active: h.id === active?.id,
            }));
        },
        selectHost(id) {
          const host = hosts.find((h) => h.id === id && stillSaved(h));
          if (!host) throw Error("Desktop is no longer saved.");
          clearRelayReturn();
          ((updatePending = !1),
            (pairingPending = !1),
            (active = host),
            (saved = credentials = host),
            (rid = host.rid),
            (url = host.url),
            (directOrigin = host.origin || location.origin),
            (destinationFixed = host.fixed ?? fixed),
            (useRelay = validRid(rid)),
            persist());
          try {
            localStorage.setItem(STORE, JSON.stringify(host));
          } catch {
            storageFailed = !0;
          }
          return credentials;
        },
        renameHost(id, value) {
          const host = hosts.find((h) => h.id === id);
          if (!host) throw Error("Desktop is no longer saved.");
          const name = String(value).trim();
          if (!name || name.length > 80)
            throw Error("Enter a desktop name (1\u201380 characters).");
          const previous = host.alias;
          if (((host.alias = name), persist(), storageFailed))
            throw (
              previous === void 0 ? delete host.alias : (host.alias = previous),
              Error("Could not save desktop name.")
            );
        },
        forgetHost(id) {
          const at = hosts.findIndex((h) => h.id === id);
          if (at < 0) return;
          const selected = active?.id === id,
            remaining = hosts.filter((h) => h.id !== id && !removed(h)),
            next = selected ? remaining[0] || null : active,
            key = removalKey(hosts[at]);
          let oldBook, oldSaved, oldRemoved;
          try {
            ((oldBook = localStorage.getItem(HOSTS)),
              (oldSaved = localStorage.getItem(STORE)),
              (oldRemoved = localStorage.getItem(key)),
              localStorage.setItem(key, "1"),
              localStorage.setItem(
                HOSTS,
                JSON.stringify({ active: next?.id || null, hosts: remaining }),
              ),
              selected &&
                (next
                  ? localStorage.setItem(STORE, JSON.stringify(next))
                  : localStorage.removeItem(STORE)));
          } catch {
            try {
              (oldBook != null
                ? localStorage.setItem(HOSTS, oldBook)
                : localStorage.removeItem(HOSTS),
                oldSaved != null && localStorage.setItem(STORE, oldSaved),
                oldRemoved != null
                  ? localStorage.setItem(key, oldRemoved)
                  : localStorage.removeItem(key));
            } catch {}
            throw (
              (storageFailed = !0),
              Error("Could not remove desktop. Try again.")
            );
          }
          return (
            hosts.splice(at, 1),
            (storageFailed = !1),
            selected &&
              ((updatePending = !1),
              (active = saved = credentials = next),
              (rid = next?.rid),
              (url = next?.url),
              (directOrigin = next?.origin || location.origin),
              (destinationFixed = next?.fixed ?? fixed),
              (useRelay = validRid(rid))),
            next?.id || null
          );
        },
        describeHost(host) {
          if (!stillSaved(active) || !credentials) return;
          let changed = !1;
          for (const [key, value] of [
            ["name", host?.display_name],
            ["platform", host?.platform],
          ])
            value &&
              active[key] !== value &&
              ((active[key] = value), (changed = !0));
          changed && persist();
        },
        restoreSaved() {
          return (
            clearRelayReturn(),
            (pairingPending = !1),
            (updatePending = !1),
            (credentials = saved),
            (directOrigin = active?.origin || location.origin),
            (destinationFixed = active?.fixed ?? fixed),
            (rid = saved?.rid),
            (url = saved?.url),
            (useRelay = validRid(rid)),
            fixed &&
              /^[0-9a-f]{16}$/.test(saved?.hash || "") &&
              saved.hash !== hash &&
              (updateHandler
                ? updateHandler()
                : location.replace("/m/" + saved.hash + "/")),
            credentials
          );
        },
        socketUrl() {
          if (!destinationFixed) return directSocket(directOrigin);
          if (useRelay && validRid(rid)) return relayUrl();
          if (validUrl(url)) return `wss://${url.slice(8)}/ws?v=${hash}`;
          if (validRid(rid)) return relayUrl();
          throw Error(
            "Scan a fresh QR code from desktop Settings \u2192 Mobile.",
          );
        },
        adopt(href) {
          const NOT =
            "Not a Krill pairing code. Scan the QR code in desktop Settings \u2192 Mobile.";
          let u;
          try {
            u = new URL(href);
          } catch {
            throw Error(NOT);
          }
          const f = new URLSearchParams(u.hash.slice(1)),
            ticket2 = f.get("pair"),
            secret2 = f.get("key"),
            version = /^\/m\/([0-9a-f]{16})\/?$/.exec(u.pathname)?.[1];
          if (
            !/^[0-9a-f]+$/.test(ticket2 || "") ||
            !secret2 ||
            !/^https?:$/.test(u.protocol)
          )
            throw Error(NOT);
          if (!fixed)
            return u.origin === location.origin
              ? {
                  ticket: ticket2,
                  secret: secret2,
                  use() {
                    ((relayAccount.fragment = u.hash),
                      (pairingPending = !0),
                      (updatePending = !1),
                      (destinationFixed = !1),
                      (directOrigin = u.origin));
                  },
                }
              : null;
          if (
            (u.protocol === "https:" &&
              !version &&
              !f.has("t") &&
              !f.has("r") &&
              !validUrl(u.origin) &&
              !u.username &&
              !u.password) ||
            (u.origin === location.origin && version && version !== hash)
          )
            return null;
          const t = f.get("t"),
            r = f.get("r"),
            next = t ? "https://" + t : validUrl(u.origin) ? u.origin : null;
          if (!validUrl(next) && !validRid(r))
            throw Error(
              u.protocol === "http:"
                ? "This direct code cannot be opened here. Choose Relay server in desktop Settings \u2192 Mobile, then tap New QR code."
                : NOT,
            );
          return {
            ticket: ticket2,
            secret: secret2,
            use() {
              ((relayAccount.fragment = u.hash),
                (pairingPending = !0),
                (updatePending = !1),
                (destinationFixed = !0),
                (url = validUrl(next) ? next : null),
                (rid = validRid(r) ? r : null),
                (useRelay = validRid(rid)));
            },
          };
        },
        authenticated(auth2) {
          if (
            (clearRelayReturn(),
            (credentials = auth2.credential ? auth2 : credentials),
            destinationFixed &&
              (credentials = {
                ...credentials,
                rid,
                url,
                hash,
                preferred_route: useRelay ? "relay" : "tunnel",
              }),
            !credentials)
          )
            return null;
          const id = hostId({ ...credentials, origin: directOrigin }),
            existing = hosts.find((h) => h.id === id);
          if (
            ((active = {
              ...existing,
              ...credentials,
              id,
              origin: directOrigin,
              fixed: destinationFixed,
              name: existing?.name || "Desktop",
            }),
            existing
              ? (hosts[hosts.indexOf(existing)] = active)
              : hosts.push(active),
            removed(active))
          )
            throw Error("Desktop is no longer saved.");
          ((saved = credentials = active), persist());
          try {
            localStorage.setItem(STORE, JSON.stringify(credentials));
          } catch {
            storageFailed = !0;
          }
          return ((pairingPending = !1), credentials);
        },
        fallback(attempted) {
          if (destinationFixed)
            try {
              attempted === this.socketUrl() &&
                (!useRelay && validRid(rid)
                  ? (useRelay = !0)
                  : useRelay && validUrl(url) && (useRelay = !1));
            } catch {}
        },
        setUpdateHandler(handler) {
          updateHandler = handler;
        },
        get updatePending() {
          return updatePending;
        },
        clearUpdatePending() {
          updatePending = !1;
        },
        updateIdentity() {
          return fixed && destinationFixed && credentials && active
            ? JSON.stringify([active.id, credentials.device_id, rid || url])
            : null;
        },
        async probeUpdate({ signal } = {}) {
          const identity = this.updateIdentity();
          if (!identity || !validRid(rid)) return null;
          const controller = new AbortController(),
            abort = () => controller.abort(),
            timeout = setTimeout(abort, 5e3);
          if (signal?.aborted) return (clearTimeout(timeout), null);
          signal?.addEventListener("abort", abort, { once: !0 });
          try {
            const response = await relayFetch("/r/" + encodeURIComponent(rid), {
              cache: "no-store",
              signal: controller.signal,
            });
            if (
              !response.ok ||
              controller.signal.aborted ||
              identity !== this.updateIdentity()
            )
              return null;
            const found = await response.json();
            return controller.signal.aborted ||
              identity !== this.updateIdentity() ||
              typeof found.hash != "string" ||
              !/^[0-9a-f]{16}$/.test(found.hash) ||
              !(validUrl(found.url) || found.url === "" || found.relay)
              ? null
              : {
                  identity,
                  hash: found.hash,
                  rid,
                  url: validUrl(found.url) ? found.url : null,
                };
          } finally {
            (clearTimeout(timeout),
              signal?.removeEventListener("abort", abort));
          }
        },
        commitUpdate(found) {
          if (
            found.identity !== this.updateIdentity() ||
            found.rid !== rid ||
            !/^[0-9a-f]{16}$/.test(found.hash)
          )
            return !1;
          let oldBook, oldSaved, writtenBook, writtenSaved;
          try {
            ((oldBook = localStorage.getItem(HOSTS)),
              (oldSaved = localStorage.getItem(STORE)));
            const book2 = JSON.parse(oldBook || "null"),
              stored = JSON.parse(oldSaved || "null"),
              latest = book2?.hosts?.find((h) => h.id === active.id);
            if (
              book2?.active !== active.id ||
              latest?.device_id !== credentials.device_id ||
              latest?.credential !== credentials.credential ||
              stored?.device_id !== credentials.device_id ||
              stored?.credential !== credentials.credential
            )
              return !1;
            const next = { ...latest, hash: found.hash, url: found.url, rid },
              nextHosts = book2.hosts.map((h) =>
                h.id === active.id ? next : h,
              ),
              bookText = JSON.stringify({
                ...book2,
                active: next.id,
                hosts: nextHosts,
              }),
              savedText = JSON.stringify(next);
            if (
              localStorage.getItem(HOSTS) !== oldBook ||
              localStorage.getItem(STORE) !== oldSaved
            )
              return !1;
            if (
              ((writtenBook = bookText),
              localStorage.setItem(HOSTS, bookText),
              (writtenSaved = savedText),
              localStorage.setItem(STORE, savedText),
              localStorage.getItem(HOSTS) !== bookText ||
                localStorage.getItem(STORE) !== savedText)
            )
              throw Error("Update destination not saved");
            return (
              hosts.splice(0, hosts.length, ...nextHosts),
              (active = saved = credentials = next),
              (url = next.url),
              (useRelay = validRid(rid)),
              (storageFailed = !1),
              !0
            );
          } catch {
            storageFailed = !0;
            try {
              (oldBook != null &&
                localStorage.getItem(HOSTS) === writtenBook &&
                localStorage.setItem(HOSTS, oldBook),
                oldSaved != null &&
                  localStorage.getItem(STORE) === writtenSaved &&
                  localStorage.setItem(STORE, oldSaved));
            } catch {}
            return !1;
          }
        },
        async locate({
          isCurrent = () => !0,
          signal,
          diagnosticsTrace: trace,
        } = {}) {
          if (!destinationFixed || !validRid(rid)) return !1;
          trace?.mark("discovery", { route: "relay" });
          let timedOut = false;
          const requestRid = rid,
            controller = new AbortController(),
            abort = () => controller.abort(),
            current = () =>
              !controller.signal.aborted &&
              !signal?.aborted &&
              isCurrent() &&
              rid === requestRid;
          if (!current()) return !1;
          signal?.addEventListener("abort", abort, { once: !0 });
          const timeout = setTimeout(() => {
            timedOut = true;
            abort();
          }, 5e3);
          try {
            const response = await relayFetch(
              "/r/" + encodeURIComponent(requestRid),
              { cache: "no-store", signal: controller.signal },
            );
            if (!current()) return !1;
            trace?.mark("discovery", { http_status: response.status });
            if (!response.ok)
              trace?.report("mobile_discovery_failed", {
                reason:
                  response.status === 401 || response.status === 403
                    ? "auth_required"
                    : "http_error",
                http_status: response.status,
              });
            if (response.ok) {
              const found = await response.json();
              if (!current()) return !1;
              if (
                typeof found.hash == "string" &&
                /^[0-9a-f]{16}$/.test(found.hash) &&
                (validUrl(found.url) || found.url === "" || found.relay)
              ) {
                if (
                  (validUrl(found.url)
                    ? ((url = found.url), (useRelay = !1))
                    : ((url = null), (useRelay = !0)),
                  found.hash !== hash && updateHandler)
                )
                  return ((updatePending = !0), updateHandler(), !1);
                if (
                  (credentials &&
                    stillSaved(active) &&
                    ((credentials = {
                      ...credentials,
                      rid,
                      url,
                      hash: found.hash,
                    }),
                    active &&
                      (Object.assign(active, credentials),
                      (saved = active),
                      persist()),
                    localStorage.setItem(STORE, JSON.stringify(credentials))),
                  found.hash !== hash)
                )
                  return (
                    location.replace(
                      "/m/" +
                        found.hash +
                        "/" +
                        location.search +
                        location.hash,
                    ),
                    !0
                  );
              }
            }
          } catch (error) {
            if (isCurrent() && !signal?.aborted)
              trace?.report("mobile_discovery_failed", {
                reason: timedOut ? "timeout" : "unknown",
                ...M.diagnostics?.exception(error),
              });
          } finally {
            (clearTimeout(timeout),
              signal?.removeEventListener("abort", abort));
          }
          return !1;
        },
      }
    );
  }
  M.transport = {
    bootstrap,
    handoff,
    pool({ endpoint, onChange = () => {}, create } = {}) {
      const entries = new Map(),
        cache = M.storage?.create();
      let closed = !1;
      const removeEntry = (entry, forget = !1) => {
          (entry.cache?.close(),
            entry.transport.close(),
            forget && cache?.removeHost(entry.id));
        },
        notify = (entry) => {
          !closed && entries.get(entry.id) === entry && onChange(entry);
        },
        api = {
          sync() {
            if (closed) return;
            const profiles = endpoint.profiles(),
              live = new Set(profiles.map((h) => h.id));
            for (const [id, entry] of entries)
              live.has(id) || (entries.delete(id), removeEntry(entry, !0));
            for (const profile of profiles) {
              const peer = endpoint.forHost(profile.id),
                auth = peer.credentials,
                key = JSON.stringify([auth.device_id, auth.credential]);
              let entry = entries.get(profile.id);
              if (entry?.key === key) {
                entry.profile = profile;
                continue;
              }
              (entry && (entries.delete(profile.id), removeEntry(entry, !0)),
                (entry = {
                  id: profile.id,
                  key,
                  profile,
                  endpoint: peer,
                  snapshot: null,
                  cached: !1,
                  cache: cache?.bind(profile.id, auth),
                  connection: {
                    phase: "connecting",
                    message: "Connecting\u2026",
                  },
                  transport: null,
                }),
                entries.set(profile.id, entry));
              const cached = entry.cache?.peek();
              (cached && ((entry.snapshot = cached), (entry.cached = !0)),
                entry.cache?.hydrate().then((data) => {
                  data &&
                    entries.get(entry.id) === entry &&
                    (!entry.snapshot || entry.cached) &&
                    ((entry.snapshot = data),
                    (entry.cached = !0),
                    notify(entry));
                }),
                (entry.transport = (create || M.transport.create)({
                  endpoint: peer,
                  credentials: auth,
                  warmTerminal: !1,
                  onAuthenticated: () => notify(entry),
                  onSnapshot: (data) => {
                    if (entries.get(entry.id) !== entry) return;
                    const current = peer.credentials,
                      nextKey = JSON.stringify([
                        current.device_id,
                        current.credential,
                      ]);
                    ((!entry.cache || entry.key !== nextKey) &&
                      (entry.cache?.close(),
                      (entry.cache = cache?.bind(entry.id, current)),
                      (entry.key = nextKey)),
                      (entry.snapshot = data),
                      (entry.cached = !1),
                      entry.cache?.write(data),
                      peer.describeHost(data.host),
                      notify(entry));
                  },
                  onStatus: (status) => {
                    ((entry.connection = status),
                      status.blocked &&
                        (entry.cache?.remove(),
                        (entry.cache = null),
                        (entry.snapshot = null),
                        (entry.cached = !1)),
                      notify(entry));
                  },
                  onError: () => notify(entry),
                })),
                entry.transport.connect());
            }
          },
          get: (id) => entries.get(id),
          entries: () => [...entries.values()],
          owns: (transport) =>
            [...entries.values()].some((e) => e.transport === transport),
          resume(stale) {
            for (const entry of entries.values()) entry.transport.resume(stale);
          },
          get updateBusy() {
            return [...entries.values()].some((e) => e.transport.updateBusy);
          },
          close() {
            ((closed = !0), unsubscribe?.());
            for (const entry of entries.values()) removeEntry(entry);
            (entries.clear(), cache?.close());
          },
        },
        unsubscribe = cache?.onInvalidate((host, keep) => {
          const entry = entries.get(host);
          entry &&
            entry.cache?.scope !== keep &&
            (entry.cache?.close(),
            (entry.cache = null),
            (entry.snapshot = null),
            (entry.cached = !1),
            notify(entry));
        });
      return api;
    },
    create(options) {
      let diagnosticsTrace;
      let pendingRpc = 0,
        uncertainMutation = !1;
      const latestReads = new Map(),
        isRead = (request) =>
          ["snapshot", "chat_poll", "git", "accounts"].includes(
            request?.action,
          );
      let credentials = options.credentials,
        pair = options.pair,
        socket,
        channel,
        generation = 0,
        connected = !1,
        connecting = !1,
        stopped = !1,
        failed,
        queue = [],
        waiters = [],
        serial = Promise.resolve(),
        refreshing,
        timer,
        retryTimer,
        connectController,
        warm = null,
        terminalActive = options.warmTerminal !== !1,
        timing = null,
        retryDelay = 1e3,
        last = null,
        authFailures = 0,
        retryNotBefore = 0,
        authBlocked = !1,
        retryCount = 0,
        nextRetryAt = 0;
      const socketUrl = () =>
          options.endpoint ? options.endpoint.socketUrl() : options.url,
        clock = () => globalThis.performance?.now?.() ?? Date.now(),
        mark = (name) => {
          timing && (timing[name] = Math.max(0, clock() - timing.started));
        },
        timings = () =>
          timing
            ? Object.fromEntries(
                Object.entries(timing).filter(([key]) => key !== "started"),
              )
            : null;
      let state = {
        phase: "disconnected",
        message: "",
        generation,
        retryAt: 0,
        blocked: !1,
      };
      const status = (phase, message) => {
        ((state = {
          phase,
          message,
          generation,
          retryAt: Math.max(retryNotBefore, nextRetryAt),
          retryCount,
          blocked: authBlocked,
          timings: timings(),
        }),
          options.onStatus?.(state));
      };
      function deadline(operation, ms, message, current = generation) {
        let timeout;
        return Promise.race([
          operation,
          new Promise((_, reject) => {
            timeout = setTimeout(() => {
              const error = Error(message);
              error.diagnosticReason = "timeout";
              ((error.transport = !0),
                reject(error),
                current === generation && disconnect(error));
            }, ms);
          }),
        ]).finally(() => clearTimeout(timeout));
      }
      const next = () =>
          queue.length
            ? Promise.resolve(queue.shift())
            : failed
              ? Promise.reject(failed)
              : new Promise((resolve, reject) =>
                  waiters.push({ resolve, reject }),
                ),
        send = (bytes) => socket.send(channel.seal(bytes)),
        plain = async () => channel.open(await next());
      function clearWarm(close = !1) {
        if (!warm) return;
        const w = warm;
        if (((warm = null), w.cleanup(), close))
          try {
            w.socket.close();
          } catch {}
      }
      function warmTerminal(force = !1) {
        if (
          (!force && (!terminalActive || options.warmTerminal === !1)) ||
          !connected ||
          !credentials ||
          stopped ||
          warm
        )
          return;
        let ws;
        try {
          ws = new WebSocket(socketUrl());
        } catch {
          return;
        }
        ws.binaryType = "arraybuffer";
        const messages = [],
          auth = credentials,
          onMessage = (e) => {
            if (warm?.socket === ws) {
              if (!warm.channel) {
                try {
                  const challenge = JSON.parse(e.data),
                    nonce = crypto.getRandomValues(new Uint8Array(32));
                  if (challenge.version !== 2)
                    throw Error("Unsupported terminal protocol");
                  const ch = new C.Channel(
                    C.sha256(C.unhex(auth.credential)),
                    C.concat(C.unhex(challenge.challenge), nonce),
                  );
                  (ws.send(
                    JSON.stringify({
                      device_id: auth.device_id,
                      token: null,
                      client_nonce: C.hex(nonce),
                    }),
                  ),
                    ws.send(ch.seal(C.enc.encode('{"label":"Live terminal"}'))),
                    ws.send(
                      ch.seal(
                        frame(
                          2,
                          C.concat(
                            v(1),
                            v(1),
                            v((1n << 56n) | 1n | 4n | 4096n | 8192n),
                            v(0),
                            text("krill-mobile-grid"),
                          ),
                        ),
                      ),
                    ),
                    (warm.channel = ch));
                } catch {
                  clearWarm(!0);
                }
                return;
              }
              messages.push(e.data);
            }
          },
          onEnd = () => {
            (warm?.socket === ws && (warm = null), cleanup());
          },
          cleanup = () => {
            (ws.removeEventListener?.("message", onMessage),
              ws.removeEventListener?.("close", onEnd),
              ws.removeEventListener?.("error", onEnd));
          };
        (ws.addEventListener("message", onMessage),
          ws.addEventListener("close", onEnd),
          ws.addEventListener("error", onEnd),
          (warm = {
            socket: ws,
            credentials: auth,
            channel: null,
            messages,
            cleanup,
          }));
      }
      function retry(delay = retryDelay) {
        if ((clearTimeout(retryTimer), options.endpoint?.updatePending)) {
          nextRetryAt = 0;
          return;
        }
        if (stopped || authBlocked || (!credentials && !pair)) return;
        const wait = Math.max(delay, retryNotBefore - Date.now());
        ((nextRetryAt = Date.now() + wait),
          status(
            "disconnected",
            retryNotBefore > Date.now()
              ? "Authentication retry is cooling down. Wait, or scan a fresh QR code."
              : failed?.message || "Reconnecting\u2026",
          ),
          (retryTimer = setTimeout(connect, wait)),
          (retryDelay = Math.min(retryDelay * 2, 15e3)));
      }
      function disconnect(error) {
        if (failed) return;
        const wasConnected = connected;
        ((failed = error),
          (connected = !1),
          clearTimeout(timer),
          clearWarm(!0));
        for (const w of waiters.splice(0)) w.reject(error);
        (status("disconnected", error.message),
          socket?.close(),
          connecting || retry(wasConnected ? 0 : retryDelay));
      }
      const ask = (request) =>
        send(frame(69, C.concat(v(7), text(JSON.stringify(request)))));
      async function answer(current) {
        const bytes = await plain();
        if (current !== generation)
          throw Error("Disconnected. Pending actions were not resent.");
        if (bytes[4] !== 101) throw Error("Unexpected daemon response");
        const r = reader(bytes.slice(5)),
          kind = Number(r.u());
        if (kind === 4)
          throw Object.assign(Error(r.text()), { definitive: !0 });
        if (kind !== 6) throw Error("Update the desktop for mobile workflows");
        const reply = JSON.parse(r.text());
        if (!reply.ok)
          throw Object.assign(Error(reply.error), { definitive: !0 });
        return reply.data;
      }
      function rpc(request, latest) {
        const current = generation,
          mutation = !isRead(request);
        pendingRpc++;
        let submitted = !1;
        const operation = deadline(
          serial.then(() => {
            if (!connected || current !== generation)
              throw Error("Disconnected. The action was not resent.");
            return (
              latest && ((latest.submitted = !0), (request = latest.request)),
              ask(request),
              (submitted = !0),
              answer(current)
            );
          }),
          15e3,
          "Desktop response timed out. The result may be unknown; pending actions were not resent.",
          current,
        );
        return (
          (serial = operation.catch(() => {})),
          operation
            .catch((error) => {
              if (current === generation)
                diagnosticsTrace?.report("mobile_rpc_failed", {
                  phase: "rpc",
                  reason:
                    error.diagnosticReason ||
                    (error.transport ? "websocket_closed" : "protocol_error"),
                  ...M.diagnostics?.exception(error),
                });
              throw (
                mutation &&
                  submitted &&
                  !error.definitive &&
                  (uncertainMutation = !0),
                error
              );
            })
            .finally(() => pendingRpc--)
        );
      }
      function rpcLatest(key, request) {
        if (!isRead(request))
          return Promise.reject(Error("Only reads can be coalesced"));
        const previous = latestReads.get(key);
        if (previous && !previous.submitted)
          return ((previous.request = request), previous.promise);
        const slot = { request, submitted: !1 };
        return (
          latestReads.set(key, slot),
          (slot.promise = rpc(request, slot).finally(() => {
            latestReads.get(key) === slot && latestReads.delete(key);
          })),
          slot.promise
        );
      }
      function snapshot(current, operation) {
        const pending = operation
          .then((data) => {
            if (current === generation) {
              if (data.unchanged && last && data.etag === last.etag)
                return last;
              for (const s of data.sessions || [])
                if (!Number.isSafeInteger(s.id) || s.id <= 0)
                  throw Error("Unsupported session ID. Update the desktop.");
              return ((last = data), options.onSnapshot?.(data), data);
            }
          })
          .finally(() => {
            refreshing === pending && (refreshing = null);
          });
        return ((refreshing = pending), pending);
      }
      function refresh() {
        return (
          refreshing ||
          snapshot(
            generation,
            rpc({
              action: "snapshot",
              ...(last?.etag && { since: last.etag }),
            }),
          )
        );
      }
      function poll() {
        if ((clearTimeout(timer), !connected || stopped)) return;
        const current = generation;
        timer = setTimeout(
          async () => {
            if (!globalThis.document?.hidden)
              try {
                await refresh();
              } catch (e) {
                connected && current === generation && options.onError?.(e);
              }
            poll();
          },
          M.activeTerminal ? 1e4 : 2500,
        );
      }
      async function connect() {
        if (
          connecting ||
          stopped ||
          connected ||
          authBlocked ||
          options.endpoint?.updatePending
        )
          return;
        if (Date.now() < retryNotBefore) {
          retry();
          return;
        }
        ((connecting = !0), clearTimeout(retryTimer), clearTimeout(timer));
        diagnosticsTrace = M.diagnostics?.attempt(retryCount);
        const trace = diagnosticsTrace;
        const current = ++generation,
          controller = (connectController = new AbortController()),
          isCurrent = () =>
            !stopped && current === generation && !controller.signal.aborted;
        if (
          ((queue = []),
          (waiters = []),
          (failed = null),
          (timing = { started: clock() }),
          status("connecting", "Connecting\u2026"),
          await serial,
          !isCurrent())
        )
          return;
        mark("queueMs");
        let stage = 0,
          established = false,
          attempted,
          quick = !1;
        try {
          if (!pair && !credentials)
            throw Error(
              "Scan a fresh QR code from desktop Settings \u2192 Mobile.",
            );
          const ensureCurrent = () => {
            if (stopped || current !== generation || failed)
              throw (
                failed ||
                Error("Disconnected. Pending actions were not resent.")
              );
          };
          if (
            (await deadline(
              (async () => {
                attempted = socketUrl();
                const target = new URL(attempted);
                timing.route = target.pathname.startsWith("/r/")
                  ? "relay"
                  : target.hostname.endsWith(".trycloudflare.com")
                    ? "tunnel"
                    : "direct";
                const octets = target.hostname.split(".").map(Number);
                if (
                  octets.length === 4 &&
                  octets[0] === 100 &&
                  octets[1] >= 64 &&
                  octets[1] <= 127
                )
                  timing.route = "tailscale";
                trace?.mark("socket_open", { route: timing.route });
                const active = (socket = new WebSocket(attempted));
                ((active.binaryType = "arraybuffer"),
                  active.addEventListener("message", (e) => {
                    if (current !== generation) return;
                    const w = waiters.shift();
                    w ? w.resolve(e.data) : queue.push(e.data);
                  }));
                const end = (e) => {
                  if (current !== generation) return;
                  const diagnosticReason =
                    e?.code === 1008 || e?.code === 4009
                      ? "auth_rejected"
                      : e?.code
                        ? "websocket_closed"
                        : "websocket_error";
                  if (
                    !failed ||
                    e?.code ||
                    diagnosticReason === "auth_rejected"
                  )
                    trace?.report(
                      diagnosticReason === "auth_rejected"
                        ? "mobile_auth_failed"
                        : "mobile_connect_failed",
                      {
                        reason: diagnosticReason,
                        ws_code: e?.code,
                        route: timing.route,
                        phase: !established
                          ? [
                              "socket_open",
                              "challenge",
                              "authentication",
                              "ipc",
                              "snapshot",
                            ][stage]
                          : "ipc",
                      },
                    );
                  const capacity = e?.code === 4009,
                    cooldown = e?.code === 1013,
                    denied = e?.code === 1008 || capacity;
                  (denied && ((authBlocked = !0), clearTimeout(retryTimer)),
                    cooldown && (retryNotBefore = Date.now() + 65e3));
                  const error = Error(
                    capacity
                      ? "Desktop device limit reached. Revoke an unused device in desktop Settings \u2192 Mobile, then scan a new QR code."
                      : cooldown
                        ? "Authentication retry is cooling down. Wait before reconnecting."
                        : denied
                          ? pair
                            ? "This QR code expired or was already used. Tap New QR code on desktop and scan again."
                            : "Desktop rejected authentication. Access may have been revoked; scan a fresh QR code."
                          : "Disconnected. Pending actions were not resent. If desktop access was revoked, scan a fresh QR code.",
                  );
                  error.diagnosticReason = diagnosticReason;
                  error.diagnosticReported = true;
                  if (((error.transport = !denied), denied && failed)) {
                    ((failed = error), status("disconnected", error.message));
                    return;
                  }
                  disconnect(error);
                };
                (active.addEventListener("close", end),
                  active.addEventListener("error", end),
                  await new Promise((resolve, reject) => {
                    (active.addEventListener(
                      "open",
                      () => {
                        ((stage = 1), resolve());
                      },
                      { once: !0 },
                    ),
                      active.addEventListener(
                        "error",
                        () => reject(Error("Could not connect to desktop")),
                        { once: !0 },
                      ),
                      active.addEventListener(
                        "close",
                        () => reject(failed || Error("Disconnected")),
                        { once: !0 },
                      ));
                  }),
                  ensureCurrent(),
                  mark("socketOpenMs"));
                trace?.mark("challenge");
                const challengeBytes = await next();
                ensureCurrent();
                const challenge = JSON.parse(challengeBytes),
                  nonce = crypto.getRandomValues(new Uint8Array(32));
                if (((stage = 2), mark("challengeMs"), challenge.version !== 2))
                  throw Error("Unsupported desktop protocol");
                trace?.mark("authentication");
                const pairing = !!pair,
                  secret = pairing
                    ? C.unbase64(pair.secret)
                    : C.sha256(C.unhex(credentials.credential));
                ((channel = new C.Channel(
                  secret,
                  C.concat(C.unhex(challenge.challenge), nonce),
                )),
                  secret.fill(0),
                  active.send(
                    JSON.stringify({
                      token: pairing ? pair.ticket : null,
                      device_id: pairing ? null : credentials.device_id,
                      client_nonce: C.hex(nonce),
                    }),
                  ),
                  send(
                    C.enc.encode(
                      JSON.stringify({
                        label: options.label || "Mobile browser",
                      }),
                    ),
                  ),
                  send(
                    frame(
                      2,
                      C.concat(
                        v(1),
                        v(1),
                        v(FEATURES),
                        v(0),
                        text("krill-mobile-pwa"),
                      ),
                    ),
                  ),
                  ask({
                    action: "snapshot",
                    ...(last?.etag && { since: last.etag }),
                  }),
                  mark("proofSentMs"));
                const authBytes = await plain();
                ensureCurrent();
                const auth = JSON.parse(decode.decode(authBytes));
                ((stage = 3),
                  mark("authenticatedMs"),
                  (auth.credential || options.endpoint) &&
                    ((credentials = options.endpoint
                      ? options.endpoint.authenticated(auth)
                      : auth),
                    (pair = null),
                    options.onAuthenticated?.(credentials)));
                trace?.mark("ipc");
                const hello = await plain();
                if ((ensureCurrent(), hello[4] !== 1))
                  throw Error("Unexpected daemon response");
                (mark("helloMs"), (connected = !0), warmTerminal());
                trace?.mark("snapshot");
                stage = 4;
                const first = serial.then(() => answer(current));
                serial = first.catch(() => {});
                const pending = snapshot(current, first),
                  resumed = !!last;
                (resumed &&
                  (mark("readyMs"),
                  status("ready", "Encrypted \xB7 connected")),
                  await pending,
                  ensureCurrent(),
                  resumed || mark("readyMs"),
                  (retryDelay = 1e3),
                  (authFailures = 0),
                  (retryCount = 0),
                  (nextRetryAt = 0),
                  (retryNotBefore = 0),
                  warmTerminal(),
                  resumed || status("ready", "Encrypted \xB7 connected"),
                  (established = true),
                  poll());
              })(),
              2e4,
              "Connection timed out. Pending actions were not resent.",
              current,
            ),
            !isCurrent())
          )
            return;
        } catch (e) {
          if (!isCurrent()) return;
          if (!e.diagnosticReported && !failed?.diagnosticReported)
            trace?.report(
              stage === 2 || authBlocked
                ? "mobile_auth_failed"
                : "mobile_connect_failed",
              {
                reason:
                  e.diagnosticReason ||
                  failed?.diagnosticReason ||
                  (navigator.onLine === false
                    ? "network_offline"
                    : "protocol_error"),
                ...M.diagnostics?.exception(e),
              },
            );
          if ((mark("failedMs"), retryCount++, stage < 2 && !authBlocked)) {
            const located = await options.endpoint?.locate({
              isCurrent,
              signal: controller.signal,
              diagnosticsTrace: trace,
            });
            if (!isCurrent()) return;
            if (relayAccount.required)
              ((authBlocked = !0),
                (e = Error("Sign in with Google to use the relay.")));
            else if (located) stopped = !0;
            else
              try {
                (socketUrl() === attempted &&
                  options.endpoint?.fallback?.(attempted),
                  (quick = retryDelay <= 1e3 && socketUrl() !== attempted));
              } catch {}
          }
          (stage === 2 &&
            ((e?.transport || failed?.transport) && !authBlocked
              ? (authFailures++,
                (retryNotBefore = Date.now() + 65e3 * authFailures),
                authFailures > 2 && (authBlocked = !0))
              : (authBlocked = !0)),
            options.onError?.(e),
            disconnect(e),
            status(
              relayAccount.required ? "auth_required" : "disconnected",
              relayAccount.required
                ? relayAccount.accountMismatch
                  ? "This desktop uses a different Google account. Open Settings, sign out of Google, then sign in with the account used on this desktop."
                  : "Sign in with Google to use the relay."
                : failed?.message || e.message,
            ));
        } finally {
          (connectController === controller && (connectController = null),
            controller.abort(),
            current === generation &&
              ((connecting = !1), connected || retry(quick ? 0 : retryDelay)));
        }
      }
      function reconnect(acknowledge = !0) {
        (acknowledge && (uncertainMutation = !1),
          connectController?.abort(),
          disconnect(
            Error("Reconnecting\u2026 Pending actions were not resent."),
          ),
          generation++,
          (connecting = !1),
          (refreshing = null),
          (authBlocked = !1),
          (authFailures = 0),
          (retryDelay = 1e3),
          connect());
      }
      return {
        connect,
        rpc,
        rpcLatest,
        refresh,
        reconnect,
        get updateBusy() {
          return connecting || !!pair || pendingRpc > 0 || uncertainMutation;
        },
        get state() {
          return {
            ...state,
            retryAt: Math.max(retryNotBefore, nextRetryAt),
            retryCount,
            blocked: authBlocked,
            timings: timings(),
          };
        },
        setPair(nextPair) {
          ((pair = nextPair), (credentials = null), (last = null), reconnect());
        },
        get ready() {
          return connected;
        },
        get blocked() {
          return authBlocked;
        },
        get credentials() {
          return credentials;
        },
        setActive(active) {
          ((terminalActive = !!active),
            terminalActive ? warmTerminal() : clearWarm(!0));
        },
        prewarmTerminal() {
          warmTerminal(!0);
        },
        resume(stale) {
          if (!connected) {
            ((retryDelay = 1e3), connect());
            return;
          }
          if (stale) {
            reconnect(!1);
            return;
          }
          (!warm ||
            warm.socket.readyState === 2 ||
            warm.socket.readyState === 3 ||
            warm.socket.closed) &&
            (clearWarm(!0), warmTerminal());
          const current = generation;
          refresh().catch((e) => {
            connected && current === generation && options.onError?.(e);
          });
        },
        terminalConnect() {
          if (!connected || !credentials)
            throw Error(
              connecting ? "Connecting\u2026" : "Reconnect to desktop first",
            );
          let out;
          if (
            warm &&
            warm.socket.readyState !== 2 &&
            warm.socket.readyState !== 3 &&
            !warm.socket.closed
          ) {
            const w = warm;
            ((warm = null),
              w.cleanup(),
              (out = {
                socket: w.socket,
                credentials: w.credentials,
                channel: w.channel,
                messages: w.messages,
              }));
          } else
            (clearWarm(!0),
              (out = { socket: new WebSocket(socketUrl()), credentials }));
          return (warmTerminal(), out);
        },
        close() {
          ((stopped = !0),
            connectController?.abort(),
            clearTimeout(timer),
            clearTimeout(retryTimer),
            clearWarm(!0),
            disconnect(Error("Disconnected")),
            generation++,
            (connecting = !1));
        },
      };
    },
  };
})();
