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
    if (status === "working" || status === "connected") return "working";
    if (status === "done") return "connected";
    if (status === "error") return "blocked";
    return "unknown";
  };

  const statusLabel = (status, t) => {
    if (status === "needs_you") return t("Needs You");
    if (status === "working") return t("Working");
    if (status === "planned") return t("Planned");
    if (status === "done") return t("Done");
    if (status === "canceled") return t("Canceled");
    if (status === "connected") return t("Connected");
    if (status === "connecting") return t("Connecting…");
    if (status === "disconnected") return t("Disconnected");
    if (status === "disabled") return t("Disabled");
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

  const latestDecisions = (item) => {
    const events = Array.isArray(item?.events) ? item.events : [];
    for (let i = events.length - 1; i >= 0; i--) {
      if (Array.isArray(events[i]?.decisions) && events[i].decisions.length) {
        return { event: events[i], decisions: events[i].decisions };
      }
    }
    return null;
  };

  const botDisplayName = (bots, botId) => bots.find((b) => b.id === botId)?.name || botId;

  function renderAvatar(el, bot, avatarInfo) {
    const badge = el("span", undefined, "bot-avatar");
    if (isValidColor(bot?.avatar_color)) badge.style.backgroundColor = bot.avatar_color;
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
  const openBotDetail = (ctx, botId, hostId) => {
    if (hostId && ctx.activateHost) ctx.activateHost(hostId);
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

  async function openBotSession(ctx, t, botId, prompt, hostId) {
    let res;
    try {
      res = await rpcHost(ctx, hostId, {
        action: "bots.chat.open",
        bot_id: botId,
        ...(prompt ? { prompt } : {}),
      });
    } catch (error) {
      const refusal = readBotRefusal(error);
      if (refusal) throw Object.assign(Error(mobileBotRefusalText(refusal, t)), { data: error?.data });
      throw error;
    }
    const sid = Number(res?.session_id);
    if (!Number.isSafeInteger(sid) || sid <= 0) throw Error(t("Could not open bot chat"));
    if (hostId && ctx.activateHost) ctx.activateHost(hostId);
    try {
      await refreshHost(ctx, hostId);
    } catch {
      /* ignore background refresh hiccup; fallback routing handles missing card */
    }
    if (ctx.signal.aborted) return;
    const session = ctx.state.sessions?.get?.(sid);
    const group =
      (session && ctx.state.groups?.get?.(session.groupKey)) ||
      (session ? { key: session.groupKey, sessions: [session] } : null);
    if (group) {
      const key = ctx.sessionKey?.(sid);
      if (key && !Object.hasOwn(ctx.state.preferences?.sessionViewModes || {}, key)) {
        ctx.savePreferences?.({
          sessionViewModes: { ...ctx.state.preferences?.sessionViewModes, [key]: "chat" },
        });
      }
      ctx.go(ctx.groupRoute(group, sid));
    } else {
      ctx.go("/projects");
    }
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
        countEl.hidden = !requests.length;
        countEl.textContent = requests.length ? String(requests.length) : "";

        const pending = requests.filter((r) => r.status === "pending");
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

  function renderItemsList(ctx, t, opts) {
    const { el, button } = ctx;
    const { items, bots, showBot, ready, onReload, cardMap, multiHost, hideEmpty, container } = opts;
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
        const card = el("article", undefined, "bot-item-card");
        card.dataset.item = item.uid;
        const header = el("div", undefined, "bot-item-head");
        const headLeft = el("div", undefined, "bot-title-line");
        const badge = el("span", undefined, "bot-status-pill");
        const hostBadge = el("span", undefined, "device-badge");
        headLeft.append(badge, hostBadge);
        const botBtn = button(
          "",
          () => openBotDetail(ctx, entry.current.owner_bot_id, entry.current._hostId),
          "text-button bot-owner-chip",
        );
        header.append(headLeft, botBtn);
        const title = el("strong", undefined, "bot-item-title");
        const desc = el("p", undefined, "bot-item-desc muted");
        const doneMeans = el("p", undefined, "bot-item-done muted");
        const links = el("div", undefined, "bot-item-links");
        const decisionsBox = el("div", undefined, "bot-item-decisions");
        const form = el("form", undefined, "bot-item-form");
        const input = el("textarea");
        input.rows = 2;
        input.placeholder = t("Add a comment or answer…");
        input.setAttribute("aria-label", t("Comment"));
        input.value = ctx.state.drafts?.get?.(draftKey) || "";
        input.addEventListener("input", () => ctx.state.drafts?.set?.(draftKey, input.value));
        const controls = el("div", undefined, "bot-item-controls");
        const submit = button(t("Comment"), () => entry.onComment(), "secondary small");
        const doneBtn = button(t("Done"), () => entry.onDone(), "primary small");
        const snoozeBtn = button(t("Snooze 4h"), () => entry.onSnooze(), "ghost small");
        const cancelBtn = button(t("Cancel item"), () => entry.onCancel(), "ghost small");
        const linkBtn = button(t("Add link"), () => entry.onLink(), "ghost small");
        controls.append(submit, doneBtn, snoozeBtn, linkBtn, cancelBtn);
        form.append(input, controls);
        card.append(header, title, desc, doneMeans, links, decisionsBox, form);
        entry = {
          card,
          badge,
          hostBadge,
          botBtn,
          title,
          desc,
          doneMeans,
          links,
          decisionsBox,
          form,
          input,
          submit,
          doneBtn,
          snoozeBtn,
          cancelBtn,
          linkBtn,
          busy: false,
          current: item,
        };
        cardMap.set(cardKey, entry);

        const mutate = async (patch) => {
          if (entry.busy || !ready()) return;
          entry.busy = true;
          syncItem();
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
            await onReload();
          } finally {
            entry.busy = false;
            syncItem();
          }
        };

        entry.onComment = () => {
          const text = input.value.trim();
          if (!text) throw Error(t("Comment cannot be empty"));
          return mutate({ comment: text });
        };
        form.addEventListener("submit", (e) => {
          e.preventDefault();
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
            urlInput.type = "url";
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
              await onReload();
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
      function syncItem() {
        const cur = entry.current;
        const disabled = entry.busy || !ready();
        entry.badge.textContent = statusLabel(cur.status, t);
        entry.badge.className = `bot-status-pill ${statusTone(cur.status)}`;
        entry.hostBadge.hidden = !multiHost || !cur._hostName;
        if (!entry.hostBadge.hidden) entry.hostBadge.textContent = cur._hostName;
        entry.botBtn.hidden = !showBot;
        if (showBot) entry.botBtn.textContent = botDisplayName(bots, cur.owner_bot_id);
        entry.title.textContent = cur.title || "";
        entry.desc.hidden = !cur.description;
        entry.desc.textContent = cur.description || "";
        entry.doneMeans.hidden = !cur.done_means;
        entry.doneMeans.textContent = cur.done_means ? `${t("Done means")}: ${cur.done_means}` : "";

        const linkNodes = [];
        for (const link of cur.links || []) {
          const [kind, val] = Object.entries(link || {})[0] || [];
          if (!val) continue;
          const href = kind === "url" || kind === "pr" ? safeHref(val) : null;
          if (href) {
            const a = el("a", `${kind.toUpperCase()}: ${val}`, "bot-link-chip");
            a.href = href;
            a.target = "_blank";
            a.rel = "noopener noreferrer";
            linkNodes.push(a);
          } else {
            linkNodes.push(el("span", `${kind}: ${val}`, "bot-link-chip muted"));
          }
        }
        entry.links.replaceChildren(...linkNodes);
        entry.links.hidden = !linkNodes.length;

        const decisionNodes = [];
        const latest = cur.status === "needs_you" ? latestDecisions(cur) : null;
        if (latest) {
          for (const d of latest.decisions) {
            const group = el("div", undefined, "bot-decision-box");
            group.append(el("p", d.question, "bot-decision-q"));
            const btns = el("div", undefined, "bot-decision-options");
            (d.options || []).forEach((opt, idx) => {
              const cls = d.recommended === idx ? "primary small" : "secondary small";
              const b = button(opt, async () => {
                if (entry.busy || !ready()) return;
                entry.busy = true;
                syncItem();
                try {
                  await rpcHost(ctx, cur._hostId, {
                    action: "bots.item.answer",
                    item_id: cur.uid,
                    option_index: idx,
                    answer_text: opt,
                  });
                  ctx.toast?.(t("Answer sent"));
                  await onReload();
                } finally {
                  entry.busy = false;
                  syncItem();
                }
              }, cls);
              b.disabled = disabled;
              btns.append(b);
            });
            group.append(btns);
            decisionNodes.push(group);
          }
        }
        entry.decisionsBox.replaceChildren(...decisionNodes);
        entry.decisionsBox.hidden = !decisionNodes.length;

        const isClosed = cur.status === "done" || cur.status === "canceled";
        const isSnoozed = Number(cur.snoozed_until_ms || 0) > Date.now();
        const snoozeLabel = t(isSnoozed ? "Unsnooze" : "Snooze 4h");
        entry.snoozeBtn.textContent = snoozeLabel;
        entry.snoozeBtn.setAttribute("aria-label", snoozeLabel);
        entry.submit.disabled = disabled;
        entry.doneBtn.disabled = disabled || isClosed;
        entry.snoozeBtn.disabled = disabled || isClosed;
        entry.cancelBtn.disabled = disabled || isClosed;
        entry.linkBtn.disabled = disabled;
        entry.input.disabled = disabled;
      }

      syncItem();
      nodes.push(entry.card);
    }
    sync(wrap, nodes);
    return wrap;
  }

  function renderSuggestionsSection(ctx, t, suggestions, bots, ready, reload, showBot, multiHost) {
    const { el, button } = ctx;
    if (!suggestions.length) return null;
    const sec = el("section", undefined, "bots-section");
    const head = el("div", undefined, "home-section-heading");
    head.append(el("h2", t("Suggestions"), "section-title"), el("small", String(suggestions.length), "muted"));
    const group = el("div", undefined, "inset-group");
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
            const sess = ctx.state.sessions?.get?.(Number(res.session_id));
            const grp = sess && ctx.state.groups?.get?.(sess.groupKey);
            if (grp) {
              ctx.go(ctx.groupRoute(grp, Number(res.session_id)));
              return;
            }
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
        return ctx.state.connection?.phase === "ready" && !ctx.state.cached
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
    let items = [];
    let requests = [];
    let statuses = [];
    let groups = [];
    let activeGroupId = null;
    let activeGroupHost = null;
    let activeThreadId = null;
    let groupView = null;
    const avatars = new Map();
    const recentRows = new Map();
    const recentSec = el("section", undefined, "bots-section bots-recent-section");
    const recentHead = el("div", undefined, "home-section-heading");
    recentHead.append(el("h2", t("Recent sessions"), "section-title"), button(t("View all"), () => ctx.go("/projects", true), "text-button bots-section-action"));
    const recentBox = el("div", undefined, "recent-session-list inset-group");
    recentSec.append(recentHead, recentBox);
    const accessCtrl = createAccessSection(ctx, t);

    // Persistent group chat section & composer so background draw() preserves focus, draft, and reply mode.
    const groupSec = el("section", undefined, "bots-section");
    const groupHead = el("div", undefined, "home-section-heading");
    const groupTitleEl = el("h2", t("Group chats"), "section-title");
    const createGrpBtn = button(`+ ${t("New group")}`, () => openCreateGroupSheet(ctx, t, bots, () => load(true), botHosts()), "filter-chip");
    groupHead.append(groupTitleEl, createGrpBtn);
    const groupListBox = el("div", undefined, "inset-group");
    const groupPanel = el("div", undefined, "bot-group-panel");
    const groupTop = el("div", undefined, "bot-group-toolbar");
    const threadBar = el("div", undefined, "bot-decision-options");
    const groupMsgBox = el("div", undefined, "bot-group-messages");
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
    groupSendForm.addEventListener("submit", (e) => {
      e.preventDefault();
      ctx.act(submitGroup);
    });
    groupSendForm.append(groupMentions, groupTextarea, groupControls);
    groupPanel.append(groupTop, groupMsgBox, groupSendForm);
    groupSec.append(groupHead, groupListBox);

    let loading = false;
    let loaded = false;
    let failure = null;
    let loadSeq = 0;
    let lastHostSig = "";

    const hostSig = () => botHosts().map((h) => h.id ?? "local").sort().join(",");

    const fetchAvatars = (list) => {
      for (const b of list) {
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
        const perHost = await Promise.all(
          targets.map(async (h) => {
            const [botsRes, itemsRes, accessRes, statusRes, groupRes] = await Promise.all([
              rpcHost(ctx, h.id, { action: "bots.list", limit: 50 }),
              rpcHost(ctx, h.id, { action: "bots.items", include_closed: false, limit: 100 }),
              rpcHost(ctx, h.id, { action: "bots.access.list", limit: 50 }),
              rpcHost(ctx, h.id, { action: "bots.connectors.status" }),
              rpcHost(ctx, h.id, { action: "bots.groups.list", limit: 50 }),
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
            };
          }),
        );
        if (ctx.signal.aborted || seq !== loadSeq) return;
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
        loaded = true;
        if (activeGroupId && groups.some((g) => g.id === activeGroupId && (g._hostId ?? null) === (activeGroupHost ?? null))) {
          await loadGroup(activeGroupId, activeThreadId, activeGroupHost).catch(() => {});
        }
        fetchAvatars(bots);
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

      if (loading && !bots.length && !isLegacyOnly) {
        const loadingBox = el("div", undefined, "account-placeholder");
        loadingBox.append(el("span", undefined, "spinner"), el("p", t("Loading bots…")));
        nodes.push(loadingBox);
      } else {
        const botsSec = el("section", undefined, "bots-section");
        const botsHead = el("div", undefined, "home-section-heading");
        botsHead.append(el("h2", t("Your bots"), "section-title"), el("small", String(bots.length), "muted"));
        botsSec.append(botsHead);

        if (isLegacyOnly) {
          const legacyBox = el("div", undefined, "bots-empty-card inset-group");
          legacyBox.append(el("p", t("Update Krill desktop to use Bots."), "empty"));
          botsSec.append(legacyBox);
        } else if (!bots.length) {
          const emptyBox = el("div", undefined, "bots-empty-card inset-group");
          emptyBox.append(
            el("p", t("No bots configured yet."), "empty"),
            button(t("Create Bot"), () => openCreateBotSheet(ctx, t, () => load(true), botHosts()), "primary"),
          );
          botsSec.append(emptyBox);
        } else {
          const sortedBots = bots.slice().sort((a, b) => {
            const aNeeds = items.some((i) => i.owner_bot_id === a.id && (i._hostId ?? null) === (a._hostId ?? null) && i.status === "needs_you") ? 0 : 1;
            const bNeeds = items.some((i) => i.owner_bot_id === b.id && (i._hostId ?? null) === (b._hostId ?? null) && i.status === "needs_you") ? 0 : 1;
            if (aNeeds !== bNeeds) return aNeeds - bNeeds;
            return Number(!!a.paused) - Number(!!b.paused);
          });
          const group = el("div", undefined, "inset-group");
          const showDev = multiHost();
          for (const bot of sortedBots) {
            const openItems = items.filter((i) => i.owner_bot_id === bot.id && (i._hostId ?? null) === (bot._hostId ?? null) && i.status === "needs_you").length;
            const botErrors = statuses.filter((s) => s.bot_id === bot.id && (s._hostId ?? null) === (bot._hostId ?? null) && s.enabled && s.state === "error").length;
            const avKey = bot._hostId ? `${bot._hostId}:${bot.id}` : bot.id;
            const row = el("div", undefined, "bot-card-row");
            const tap = button("", () => openBotDetail(ctx, bot.id, bot._hostId), "bot-card-main");
            tap.setAttribute("aria-label", bot.name);
            const avatar = renderAvatar(el, bot, avatars.get(avKey));
            const copy = el("span", undefined, "bots-row-main");
            const titleLine = el("span", undefined, "bot-title-line");
            titleLine.append(
              el("strong", bot.name),
              el("span", t(bot.paused ? "Paused" : "Active"), `bot-status-pill ${bot.paused ? "unknown" : "connected"}`),
            );
            if (openItems > 0) titleLine.append(el("span", t("{0} needs you", openItems), "bot-status-pill waiting"));
            if (botErrors > 0) titleLine.append(el("span", t("Connector error"), "bot-status-pill blocked"));
            if (showDev && bot._hostName) titleLine.append(el("span", bot._hostName, "device-badge"));
            copy.append(titleLine);
            if (bot.description) copy.append(el("small", bot.description, "muted"));
            tap.append(avatar, copy, ctx.icon("chevronRight"));

            const chatBtn = button(
              t("Open Chat"),
              () => openBotSession(ctx, t, bot.id, undefined, bot._hostId),
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

      // Bottom section: Recent project sessions (max 3 + "View all" -> Projects)
      const recentNode = renderRecentSessionsSection(hosts);
      if (recentNode) nodes.push(recentNode);

      if (!isLegacyOnly && ready()) {
        nodes.push(accessCtrl.update({ requests, bots, ready, reload: () => load(false), multiHost: multiHost(), targetHosts: botHosts() }));
        nodes.push(renderGroupPanel());
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
    const screen = el("section", undefined, "details bots bot-detail screen");
    const header = ctx.header(botId, "/bots");
    const chatBtn = button(t("Open Chat"), () => openBotSession(ctx, t, botId), "primary small");
    const refreshBtn = button(t("Refresh"), () => load(), "icon-button refresh-button", "activity");
    header.append(chatBtn, refreshBtn);
    const list = el("div", undefined, "detail-list bots-list");
    screen.append(header, list);
    root.append(screen);

    let bot = null;
    let items = [];
    let itemsCursor = null;
    let statuses = [];
    let suggestions = [];
    let requests = [];
    let upcoming = null;
    let avatarInfo = null;
    let includeClosed = false;
    let includeSnoozed = false;
    let soulLoaded = false;
    let soulText = "";
    let memoryText = "";
    let allowMemoryDelete = false;
    let loading = false;
    let failure = null;
    let loadSeq = 0;
    const cardMap = new Map();
    const h1 = header.querySelector("h1");

    // Persistent sections/inputs in Bot Detail so background draw() preserves focus, cursor, scroll, and drafts.
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

    const replySec = el("section", undefined, "bots-section");
    replySec.append(el("h2", t("External reply"), "section-title"));
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
      await ctx.rpc({ action: "bots.reply", bot_id: botId, text });
      if (ctx.signal.aborted) return;
      replyInput.value = "";
      ctx.state.drafts?.delete?.(replyDraftKey);
      ctx.toast?.(t("Reply posted"));
    };
    const replyBtn = button(t("Send reply"), submitReply, "secondary small");
    replyForm.addEventListener("submit", (e) => {
      e.preventDefault();
      ctx.act(submitReply);
    });
    replyForm.append(replyInput, replyBtn);
    replyBox.append(replyForm);
    replySec.append(replyBox);

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
    const loadDocsBtn = button(
      t("Load"),
      async () => {
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
        draw();
      },
      "filter-chip",
    );
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
        const [botsRes, itemsRes, statusRes, sugRes, accRes, upRes] = await Promise.all([
          ctx.rpc({ action: "bots.list", limit: 100 }),
          ctx.rpc({
            action: "bots.items",
            bot_id: botId,
            include_closed: includeClosed,
            include_snoozed: includeSnoozed,
            limit: 25,
          }),
          ctx.rpc({ action: "bots.connectors.status", bot_id: botId }),
          ctx.rpc({ action: "bots.suggestions.list", bot_id: botId, include_dismissed: false }),
          ctx.rpc({ action: "bots.access.list", bot_id: botId, limit: 50 }),
          ctx.rpc({ action: "bots.upcoming", bot_id: botId }).catch(() => null),
        ]);
        if (ctx.signal.aborted || seq !== loadSeq) return;
        bot = (botsRes?.bots || []).find((b) => b.id === botId) || null;
        if (h1 && bot?.name) h1.textContent = bot.name;
        items = itemsRes?.items || [];
        itemsCursor = itemsRes?.next_cursor || null;
        statuses = statusRes?.statuses || [];
        suggestions = sugRes?.suggestions || [];
        requests = accRes?.requests || [];
        upcoming = upRes && typeof upRes === "object" ? upRes : null;
        if (bot?.avatar && !avatarInfo) {
          rpcHost(ctx, null, { action: "bots.avatar.read", bot_id: botId })
            .then((av) => {
              if (!ctx.signal.aborted && av?.data_base64) {
                avatarInfo = av;
                draw();
              }
            })
            .catch(() => {});
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
              const items = Array.isArray(res?.models) ? res.models : [];
              modelSelect.replaceChildren();
              const d = el("option", t("Default"));
              d.value = "";
              modelSelect.append(d);
              const ids = new Set([""]);
              for (const m of items) {
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
          await refreshHost(ctx, null)?.catch?.(() => {});
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

    function draw() {
      if (ctx.signal.aborted) return;
      chatBtn.disabled = !ready() || !!bot?.paused;
      refreshBtn.disabled = loading || !ready();
      const nodes = [];
      if (!ready()) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
      if (failure) nodes.push(el("p", failure, "account-error"));

      if (bot) {
        const hero = el("div", undefined, "inset-group bot-detail-hero");
        const top = el("div", undefined, "bots-row");
        const left = el("div", undefined, "bot-card-main");
        const meta = el("div", undefined, "bots-row-main");
        meta.append(
          el("strong", bot.name),
          el("small", [M.agentName?.(bot.agent_id) || bot.agent_id, bot.model, bot.paused ? t("Paused") : t("Active")].filter(Boolean).join(" · "), "muted"),
        );
        if (bot.description) meta.append(el("small", bot.description, "muted"));
        left.append(renderAvatar(el, bot, avatarInfo), meta);
        const actions = el("div", undefined, "bots-row-actions");
        const agentBtn = button(t("Agent"), openEditAgentSheet, "secondary small");
        agentBtn.disabled = !ready();
        const lookBtn = button(
          t("Look Around"),
          async () => {
            const res = await ctx.rpc({ action: "bots.look_around", bot_id: botId });
            ctx.toast?.(t("{0} suggestions ready", (res?.suggestions || []).length));
            await load();
          },
          "secondary small",
        );
        lookBtn.disabled = !ready() || !!bot.paused;
        actions.append(agentBtn, lookBtn);
        top.append(left, actions);
        hero.append(top);
        nodes.push(hero);
      }

      // Failed automation run cards (`e1779f39`): surface why a scheduled run could not start.
      const failedRuns = (upcoming?.schedules || []).filter((s) => s?.last_refusal || s?.last_error);
      if (failedRuns.length) {
        const autoSec = el("section", undefined, "bots-section");
        autoSec.append(el("h2", t("Automations"), "section-title"));
        const group = el("div", undefined, "inset-group");
        for (const run of failedRuns) {
          const cfg = (bot?.schedules || []).find((s) => s.id === run.id);
          const row = el("div", undefined, "bots-row bot-automation-run-row");
          const main = el("div", undefined, "bots-row-main");
          const errText = run.last_refusal
            ? mobileBotRefusalText(run.last_refusal, t)
            : String(run.last_error || "");
          main.append(
            el("strong", cfg?.name || run.id),
            el("small", errText, "account-error"),
          );
          row.append(main, ctx.dot("blocked"));
          group.append(row);
        }
        autoSec.append(group);
        nodes.push(autoSec);
      }

      // Connector health summary
      if (statuses.length) {
        const connSec = el("section", undefined, "bots-section");
        connSec.append(el("h2", t("Connectors"), "section-title"));
        const group = el("div", undefined, "inset-group");
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
          const dot = ctx.dot(statusTone(st.state));
          row.append(main, dot);
          group.append(row);
        }
        connSec.append(group);
        nodes.push(connSec);
      }

      const sugNode = renderSuggestionsSection(ctx, t, suggestions, bot ? [bot] : [], ready, load, false);
      if (sugNode) nodes.push(sugNode);

      // Items inbox (persistent section & wrap to keep textarea focus)
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
      });
      moreItemsBtn.disabled = !ready();
      sync(itemSec, itemsCursor ? [itemHead, itemFilters, itemsWrap, moreItemsBtn] : [itemHead, itemFilters, itemsWrap]);
      nodes.push(itemSec);

      if (requests.some((r) => r.status === "pending")) {
        nodes.push(accessCtrl.update({ requests, bots: bot ? [bot] : [], ready, reload: load, multiHost: false }));
      }

      // Quick Reply (persistent form to keep focus & draft)
      replyInput.disabled = replyBtn.disabled = !ready();
      nodes.push(replySec);

      // Soul & Memory (persistent form to keep focus, cursor, and scroll)
      const docsBtnLabel = t(soulLoaded ? "Reload" : "Load");
      if (loadDocsBtn.textContent !== docsBtnLabel) loadDocsBtn.textContent = docsBtnLabel;
      loadDocsBtn.setAttribute("aria-label", docsBtnLabel);
      loadDocsBtn.disabled = !ready();
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

      sync(list, nodes);
    }

    load();
    return { update: draw };
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
        (h) => h.connection?.phase === "ready" && !h.cached && (h.snapshot?.capabilities?.bots || h.id === null),
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

    const itemSec = el("section", undefined, "bots-section");
    const itemSecTitle = el("h2", t("Needs You & Active Items"), "section-title");
    const filterBar = el("div", undefined, "bot-decision-options");
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
    const itemsWrap = el("div", undefined, "bots-items-list");
    const clearCard = el("div", undefined, "todo-clear-card inset-group");
    const clearTitle = el("div", undefined, "todo-clear-title");
    clearTitle.append(ctx.icon("check"), el("strong", t("All caught up")));
    clearCard.append(clearTitle, el("p", t("Your agents are working autonomously."), "muted"));
    const moreBtn = button(t("Load more"), loadMore, "secondary");
    itemSec.append(itemSecTitle, filterBar, itemsWrap);

    const hostSig = () => botHosts().map((h) => h.id ?? "local").sort().join(",");

    const itemRequestParams = (reqFilter, cursor) => ({
      action: "bots.items",
      include_closed: reqFilter === "closed",
      include_snoozed: reqFilter === "snoozed",
      ...(reqFilter === "needs_you" ? { status: "needs_you" } : {}),
      limit: 25,
      ...(cursor ? { cursor } : {}),
    });

    async function fetchBotItems(hostId, reqFilter, cursor) {
      const hKey = hostId ?? "local";
      const req = itemRequestParams(reqFilter, cursor);
      if (req.status && noStatusFilterHosts.has(hKey)) delete req.status;
      try {
        return await rpcHost(ctx, hostId, req);
      } catch (err) {
        if (req.status && err?.definitive) {
          noStatusFilterHosts.add(hKey);
          const fallbackReq = { ...req };
          delete fallbackReq.status;
          return await rpcHost(ctx, hostId, fallbackReq);
        }
        throw err;
      }
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
        draw();
        return;
      }
      const seq = ++loadSeq;
      const reqFilter = filter;
      loading = true;
      failure = null;
      draw();
      try {
        const perHost = await Promise.all(
          targets.map(async (h) => {
            const [botsRes, itemsRes, accessRes, sugRes] = await Promise.all([
              rpcHost(ctx, h.id, { action: "bots.list", limit: 100 }),
              fetchBotItems(h.id, reqFilter, null),
              rpcHost(ctx, h.id, { action: "bots.access.list", limit: 50 }),
              rpcHost(ctx, h.id, { action: "bots.suggestions.list", include_dismissed: false }),
            ]);
            const tag = (arr) => (h.id ? (arr || []).map((x) => ({ ...x, _hostId: h.id, _hostName: h.name })) : (arr || []));
            return {
              host: h,
              bots: tag(botsRes?.bots),
              items: tag(itemsRes?.items),
              cursor: itemsRes?.next_cursor || null,
              requests: tag(accessRes?.requests),
              suggestions: tag(sugRes?.suggestions),
            };
          }),
        );
        if (ctx.signal.aborted || seq !== loadSeq) return;
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

    function draw() {
      if (ctx.signal.aborted) return;
      const hosts = filteredHosts();
      const hasBotsCap = botHosts().length > 0;
      refreshBtn.disabled = loading || !hosts.some((h) => h.connection?.phase === "ready");
      const nodes = [];
      if (failure) nodes.push(el("p", failure, "account-error"));

      const waitingSec = renderWaitingSessionsSection(hosts);
      const orchSecRes = renderOrchestrationSection(hosts);
      const pendingAccess = requests.filter((r) => r.status === "pending");
      const now = Date.now();
      const visibleItems =
        filter === "needs_you"
          ? items.filter((i) => i.status === "needs_you")
          : filter === "closed"
            ? items.filter((i) => i.status === "done" || i.status === "canceled")
            : filter === "snoozed"
              ? items.filter((i) => Number(i.snoozed_until_ms || 0) > now)
              : items;

      if (waitingSec.node) nodes.push(waitingSec.node);
      if (orchSecRes.node) nodes.push(orchSecRes.node);
      if (pendingAccess.length) {
        nodes.push(accessCtrl.update({ requests, bots, ready, reload: () => load(true), multiHost: multiHost(), targetHosts: botHosts() }));
      }
      const sugNode = renderSuggestionsSection(ctx, t, suggestions, bots, ready, () => load(true), true, multiHost());
      if (sugNode) nodes.push(sugNode);

      const totalEverything = waitingSec.count + orchSecRes.count + pendingAccess.length + suggestions.length + visibleItems.length;

      if (hasBotsCap) {
        for (const btn of filterButtons) {
          btn.className = filter === btn.dataset.filter ? "filter-chip active" : "filter-chip";
          btn.disabled = !ready();
        }
        const secNodes = [itemSecTitle, filterBar];
        if (filter === "needs_you" && totalEverything === 0 && !loading) {
          secNodes.push(clearCard);
        } else {
          renderItemsList(ctx, t, {
            items: visibleItems,
            bots,
            showBot: true,
            ready,
            onReload: () => load(true),
            cardMap,
            multiHost: multiHost(),
            hideEmpty: filter === "needs_you" && totalEverything > 0,
            container: itemsWrap,
          });
          secNodes.push(itemsWrap);
        }
        if (itemsCursors.size > 0) {
          moreBtn.disabled = !ready();
          secNodes.push(moreBtn);
        }
        sync(itemSec, secNodes);
        nodes.push(itemSec);
      } else if (totalEverything === 0) {
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
