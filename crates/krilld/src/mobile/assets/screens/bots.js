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

  const formatText = (ctx) => (s, ...v) =>
    v.reduce((out, val, i) => out.replaceAll(`{${i}}`, String(val)), ctx.t?.(s, ...v) ?? s);

  const safeMsg = (e, t) => {
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

  async function openBotSession(ctx, t, botId, prompt) {
    const res = await ctx.rpc({
      action: "bots.chat.open",
      bot_id: botId,
      ...(prompt ? { prompt } : {}),
    });
    const sid = Number(res?.session_id);
    if (!Number.isSafeInteger(sid) || sid <= 0) throw Error(t("Could not open bot chat"));
    try {
      await ctx.refresh?.();
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
      ctx.go("/host/local");
    }
  }

  function accessSection(ctx, t, requests, bots, ready, reload) {
    const { el, button } = ctx;
    const section = el("section", undefined, "bots-section");
    const head = el("div", undefined, "home-section-heading");
    head.append(el("h2", t("Access & pairing"), "section-title"));
    if (requests.length) head.append(el("small", String(requests.length), "muted"));
    const box = el("div", undefined, "inset-group bots-access-card");
    const pending = requests.filter((r) => r.status === "pending");

    for (const req of pending) {
      const row = el("div", undefined, "bots-row");
      const copy = el("div", undefined, "bots-row-main");
      copy.append(
        el("strong", req.sender_name || req.sender_id || t("Sender")),
        el("small", `${req.source} · ${botDisplayName(bots, req.bot_id)}`, "muted"),
      );
      const actions = el("div", undefined, "bots-row-actions");
      const decide = async (approve) => {
        if (!ready()) return;
        await ctx.rpc({ action: "bots.access.decide", token: req.token, approve });
        ctx.toast?.(approve ? t("Access approved") : t("Access declined"));
        await reload();
      };
      const allow = button(t("Approve"), () => decide(true), "primary small");
      const deny = button(t("Deny"), () => decide(false), "ghost small");
      allow.disabled = deny.disabled = !ready();
      actions.append(allow, deny);
      row.append(copy, actions);
      box.append(row);
    }

    const redeemForm = el("form", undefined, "bots-redeem-form");
    const codeInput = el("input");
    codeInput.type = "text";
    codeInput.placeholder = t("Enter pairing code");
    codeInput.setAttribute("aria-label", t("Enter pairing code"));
    const submitRedeem = async () => {
      if (!ready()) return;
      const code = codeInput.value.trim();
      if (!code) throw Error(t("Enter pairing code"));
      await ctx.rpc({ action: "bots.access.redeem", code });
      if (ctx.signal.aborted) return;
      codeInput.value = "";
      ctx.toast?.(t("Pairing code redeemed"));
      await reload();
    };
    const redeemBtn = button(t("Redeem"), submitRedeem, "secondary small");
    redeemForm.addEventListener("submit", (e) => {
      e.preventDefault();
      ctx.act(submitRedeem);
    });
    codeInput.disabled = redeemBtn.disabled = !ready();
    redeemForm.append(codeInput, redeemBtn);
    box.append(redeemForm);
    section.append(head, box);
    return section;
  }

  function renderItemsList(ctx, t, opts) {
    const { el, button } = ctx;
    const { items, bots, showBot, ready, onReload, cardMap } = opts;
    const wrap = el("div", undefined, "bots-items-list");
    if (!items.length) {
      wrap.append(el("p", t("No items match this filter."), "empty"));
      return wrap;
    }

    const nodes = [];
    for (const item of items) {
      let entry = cardMap.get(item.uid);
      const draftKey = `bot-item:${item.uid}`;
      if (!entry) {
        const card = el("article", undefined, "bot-item-card");
        card.dataset.item = item.uid;
        const header = el("div", undefined, "bot-item-head");
        const badge = el("span", undefined, "bot-status-pill");
        const botBtn = button(
          "",
          () => ctx.go(`/bots/${encodeURIComponent(item.owner_bot_id)}`),
          "text-button bot-owner-chip",
        );
        header.append(badge, botBtn);
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
        cardMap.set(item.uid, entry);

        const mutate = async (patch) => {
          if (entry.busy || !ready()) return;
          entry.busy = true;
          syncItem();
          try {
            await ctx.rpc({ action: "bots.item.update", item_id: entry.current.uid, ...patch });
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
              await ctx.rpc({ action: "bots.item.link", item_id: entry.current.uid, link });
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
                  await ctx.rpc({
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

  function renderSuggestionsSection(ctx, t, suggestions, bots, ready, reload, showBot) {
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
        el("small", [showBot ? botDisplayName(bots, s.bot_id) : null, s.why].filter(Boolean).join(" · "), "muted"),
      );
      if (s.ask) main.append(el("small", s.ask, "muted"));
      const actions = el("div", undefined, "bots-row-actions");
      const startBtn = button(
        t("Start"),
        async () => {
          const res = await ctx.rpc({
            action: "bots.suggestions.start",
            bot_id: s.bot_id,
            suggestion_id: s.id,
          });
          if (ctx.signal.aborted) return;
          if (Number.isSafeInteger(Number(res?.session_id)) && Number(res.session_id) > 0) {
            await ctx.refresh?.().catch(() => {});
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
          await ctx.rpc({
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

  function openCreateBotSheet(ctx, t, reload) {
    const { el, button } = ctx;
    const agents = ctx.state.snapshot?.agents || [];
    ctx.sheet(t("New Bot"), [], t("Create an autonomous bot with its own workspace and soul."), (body) => {
      const nameInput = el("input");
      nameInput.type = "text";
      nameInput.placeholder = t("Bot name");
      nameInput.setAttribute("aria-label", t("Bot name"));

      const agentSelect = el("select");
      agentSelect.setAttribute("aria-label", t("Agent"));
      const agentChoices = agents.length ? agents : [{ id: "claude", name: "Claude Code" }];
      for (const a of agentChoices) {
        const opt = el("option", a.name || a.id);
        opt.value = a.id;
        agentSelect.append(opt);
      }
      agentSelect.value = agentChoices[0].id;

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
      ctx.rpc({ action: "bots.templates" })
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
        await ctx.rpc({
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
          await ctx.refresh?.();
        } catch {}
        await reload();
      };

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

  function openCreateGroupSheet(ctx, t, bots, reload) {
    const { el, button } = ctx;
    const selected = new Set(bots.slice(0, 2).map((b) => b.id));
    ctx.sheet(t("New Group Chat"), [], t("Coordinate multiple bots in shared threads."), (body) => {
      const nameInput = el("input");
      nameInput.type = "text";
      nameInput.placeholder = t("Group name");
      nameInput.setAttribute("aria-label", t("Group name"));

      const descInput = el("input");
      descInput.type = "text";
      descInput.placeholder = t("Description (optional)");
      descInput.setAttribute("aria-label", t("Description"));

      const picks = el("div", undefined, "bot-decision-options");
      for (const b of bots) {
        const chip = button(b.name, () => {
          if (selected.has(b.id)) selected.delete(b.id);
          else selected.add(b.id);
          chip.setAttribute("aria-pressed", String(selected.has(b.id)));
          chip.className = selected.has(b.id) ? "filter-chip active" : "filter-chip";
        }, selected.has(b.id) ? "filter-chip active" : "filter-chip");
        chip.setAttribute("aria-pressed", String(selected.has(b.id)));
        picks.append(chip);
      }

      const submit = async () => {
        const name = nameInput.value.trim();
        if (!name) throw Error(t("Group name is required"));
        if (!selected.size) throw Error(t("Select at least one bot"));
        await ctx.rpc({
          action: "bots.groups.create",
          name,
          bot_ids: [...selected],
          description: descInput.value.trim(),
        });
        ctx.closeSheet?.();
        ctx.toast?.(t("Group created"));
        await reload();
      };

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
    const ready = () => ctx.state.connection?.phase === "ready" && !ctx.state.cached;

    if (botId) {
      return mountBotDetail(root, ctx, t, botId, ready);
    }

    const screen = el("section", undefined, "details bots screen");
    const header = ctx.header("Bots", "/");
    const upNextBtn = button(t("Up Next"), () => ctx.go("/up-next"), "filter-chip");
    const newBtn = button(`+ ${t("New Bot")}`, () => openCreateBotSheet(ctx, t, () => load(true)), "primary small");
    const refreshBtn = button(t("Refresh"), () => load(true), "icon-button refresh-button", "activity");
    header.append(upNextBtn, newBtn, refreshBtn);
    const list = el("div", undefined, "detail-list bots-list");
    screen.append(header, list);
    root.append(screen);

    let bots = [];
    let botsCursor = null;
    let items = [];
    let requests = [];
    let statuses = [];
    let groups = [];
    let activeGroupId = null;
    let activeThreadId = null;
    let groupView = null;
    const avatars = new Map();
    let loading = false;
    let failure = null;
    let loadSeq = 0;
    let lastPhase = ctx.state.connection?.phase;

    async function loadGroup(groupId, threadId) {
      if (!ready() || !groupId) return;
      const res = await ctx.rpc({
        action: "bots.groups.read",
        group_id: groupId,
        ...(threadId ? { thread_id: threadId } : {}),
        limit: 60,
      });
      if (ctx.signal.aborted) return;
      groupView = res?.group || null;
      activeGroupId = groupView?.group?.id || groupId;
      activeThreadId =
        threadId ||
        groupView?.threads?.[0]?.thread_id ||
        groupView?.messages?.at?.(-1)?.thread_id ||
        null;
      draw();
    }

    async function load(reset = false) {
      if (!ready()) {
        draw();
        return;
      }
      const seq = ++loadSeq;
      loading = true;
      failure = null;
      draw();
      try {
        const [botsRes, itemsRes, accessRes, statusRes, groupRes] = await Promise.all([
          ctx.rpc({ action: "bots.list", limit: 50 }),
          ctx.rpc({ action: "bots.items", include_closed: false, limit: 100 }),
          ctx.rpc({ action: "bots.access.list", limit: 50 }),
          ctx.rpc({ action: "bots.connectors.status" }),
          ctx.rpc({ action: "bots.groups.list", limit: 50 }),
        ]);
        if (ctx.signal.aborted || seq !== loadSeq) return;
        bots = botsRes?.bots || [];
        botsCursor = botsRes?.next_cursor || null;
        items = itemsRes?.items || [];
        requests = accessRes?.requests || [];
        statuses = statusRes?.statuses || [];
        groups = groupRes?.groups || [];
        if (activeGroupId && groups.some((g) => g.id === activeGroupId)) {
          await loadGroup(activeGroupId, activeThreadId).catch(() => {});
        }
        // Best-effort lazy avatar fetch for bots that have one
        for (const b of bots) {
          if (b.avatar && !avatars.has(b.id)) {
            avatars.set(b.id, null);
            ctx.rpc({ action: "bots.avatar.read", bot_id: b.id })
              .then((av) => {
                if (!ctx.signal.aborted && av) {
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

    async function loadMoreBots() {
      if (!botsCursor || !ready()) return;
      const res = await ctx.rpc({ action: "bots.list", limit: 50, cursor: botsCursor });
      if (ctx.signal.aborted) return;
      const seen = new Set(bots.map((b) => b.id));
      for (const b of res?.bots || []) if (!seen.has(b.id)) bots.push(b);
      botsCursor = res?.next_cursor || null;
      draw();
    }

    function renderGroupPanel() {
      const sec = el("section", undefined, "bots-section");
      const head = el("div", undefined, "home-section-heading");
      head.append(el("h2", t("Group chats"), "section-title"));
      const createGrp = button(`+ ${t("New group")}`, () => openCreateGroupSheet(ctx, t, bots, () => load(true)), "filter-chip");
      createGrp.disabled = !ready() || !bots.length;
      head.append(createGrp);
      sec.append(head);

      const box = el("div", undefined, "inset-group");
      if (!groups.length) {
        box.append(el("p", t("No group chats yet."), "muted empty"));
      } else {
        for (const g of groups) {
          const row = button(
            "",
            () => {
              if (activeGroupId === g.id) {
                activeGroupId = null;
                groupView = null;
                draw();
              } else {
                return loadGroup(g.id, null);
              }
            },
            `bots-group-row${activeGroupId === g.id ? " active" : ""}`,
          );
          const main = el("span", undefined, "bots-row-main");
          main.append(
            el("strong", g.name),
            el("small", (g.bot_ids || []).map((id) => botDisplayName(bots, id)).join(", "), "muted"),
          );
          row.append(main, ctx.icon(activeGroupId === g.id ? "chevronDown" : "chevronRight"));
          box.append(row);
        }
      }
      sec.append(box);

      if (activeGroupId && groupView) {
        const panel = el("div", undefined, "bot-group-panel");
        const top = el("div", undefined, "bot-group-toolbar");
        const threadBar = el("div", undefined, "bot-decision-options");
        for (const th of groupView.threads || []) {
          const tb = button(
            th.title || t("Thread"),
            () => loadGroup(activeGroupId, th.thread_id),
            th.thread_id === activeThreadId ? "filter-chip active" : "filter-chip",
          );
          tb.disabled = !ready();
          threadBar.append(tb);
        }
        const newThread = button(
          `+ ${t("Thread")}`,
          async () => {
            const prevIds = new Set((groupView?.threads || []).map((th) => th.thread_id));
            const res = await ctx.rpc({ action: "bots.groups.new_thread", group_id: activeGroupId });
            if (res?.group) {
              groupView = res.group;
              const created = (groupView.threads || []).find((th) => !prevIds.has(th.thread_id));
              activeThreadId = created?.thread_id || groupView.threads?.[0]?.thread_id || null;
              draw();
            } else {
              await loadGroup(activeGroupId, null);
            }
          },
          "filter-chip",
        );
        newThread.disabled = !ready();
        threadBar.append(newThread);
        top.append(threadBar);

        if (groupView.active_run) {
          const stopBtn = button(
            t("Stop"),
            async () => {
              await ctx.rpc({
                action: "bots.groups.stop",
                group_id: activeGroupId,
                ...(activeThreadId ? { thread_id: activeThreadId } : {}),
              });
              await loadGroup(activeGroupId, activeThreadId);
            },
            "ghost small",
          );
          stopBtn.disabled = !ready();
          top.append(stopBtn);
        }
        panel.append(top);

        const msgBox = el("div", undefined, "bot-group-messages");
        const msgs = groupView.messages || [];
        if (!msgs.length) {
          msgBox.append(el("p", t("No messages yet."), "muted"));
        } else {
          for (const msg of msgs) {
            const item = el("div", undefined, "bot-group-msg");
            item.append(
              el("strong", `${msg.sender_name || msg.sender_id}${msg.in_progress ? ` · ${t("Working")}` : ""}`),
              el("p", msg.body || ""),
            );
            msgBox.append(item);
          }
        }
        panel.append(msgBox);

        const draftKey = `bot-group:${activeGroupId}`;
        const sendForm = el("form", undefined, "bot-item-form");
        const mentions = el("div", undefined, "bot-decision-options");
        const textarea = el("textarea");
        textarea.rows = 2;
        textarea.placeholder = t("Message group (@mention bots)…");
        textarea.setAttribute("aria-label", t("Group message"));
        textarea.value = ctx.state.drafts?.get?.(draftKey) || "";
        textarea.addEventListener("input", () => ctx.state.drafts?.set?.(draftKey, textarea.value));

        for (const bid of groupView.group?.bot_ids || []) {
          const b = bots.find((x) => x.id === bid);
          const slug = b?.slug || bid;
          const chip = button(`@${slug}`, () => {
            textarea.value = `${textarea.value ? `${textarea.value.trimEnd()} ` : ""}@${slug} `;
            ctx.state.drafts?.set?.(draftKey, textarea.value);
            textarea.focus();
          }, "filter-chip");
          mentions.append(chip);
        }

        const modeSelect = el("select");
        modeSelect.setAttribute("aria-label", t("Reply mode"));
        for (const [val, lbl] of [
          ["mentions", t("Mentioned bots")],
          ["all", t("All bots")],
        ]) {
          const opt = el("option", lbl);
          opt.value = val;
          modeSelect.append(opt);
        }
        modeSelect.value = "mentions";
        const submitGroup = async () => {
          if (!ready()) return;
          const text = textarea.value.trim();
          if (!text) throw Error(t("Enter a message"));
          await ctx.rpc({
            action: "bots.groups.send",
            group_id: activeGroupId,
            ...(activeThreadId ? { thread_id: activeThreadId } : {}),
            text,
            reply_mode: modeSelect.value,
          });
          if (ctx.signal.aborted) return;
          textarea.value = "";
          ctx.state.drafts?.delete?.(draftKey);
          await loadGroup(activeGroupId, activeThreadId);
        };
        const sendBtn = button(t("Send"), submitGroup, "primary small");
        const controls = el("div", undefined, "bot-item-controls");
        controls.append(modeSelect, sendBtn);
        sendForm.addEventListener("submit", (e) => {
          e.preventDefault();
          ctx.act(submitGroup);
        });
        textarea.disabled = sendBtn.disabled = modeSelect.disabled = !ready();
        sendForm.append(mentions, textarea, controls);
        panel.append(sendForm);
        sec.append(panel);
      }
      return sec;
    }

    function draw() {
      if (ctx.signal.aborted) return;
      const needsCount = items.filter((i) => i.status === "needs_you").length;
      const pendingAccessCount = requests.filter((r) => r.status === "pending").length;
      const totalUpNext = needsCount + pendingAccessCount;
      upNextBtn.textContent = totalUpNext > 0 ? `${t("Up Next")} (${totalUpNext})` : t("Up Next");
      newBtn.disabled = !ready();
      refreshBtn.disabled = loading || !ready();

      const nodes = [];
      if (!ready()) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
      if (failure) nodes.push(el("p", failure, "account-error"));

      if (loading && !bots.length) {
        const loadingBox = el("div", undefined, "account-placeholder");
        loadingBox.append(el("span", undefined, "spinner"), el("p", t("Loading bots…")));
        nodes.push(loadingBox);
        sync(list, nodes);
        return;
      }

      if (totalUpNext > 0) {
        const upNextRow = ctx.linkRow(
          "Up Next",
          t("{0} items or requests waiting for you", totalUpNext),
          "/up-next",
          "tasks",
        );
        const cardWrap = el("div", undefined, "inset-group");
        cardWrap.append(upNextRow);
        nodes.push(cardWrap);
      }

      const botsSec = el("section", undefined, "bots-section");
      const botsHead = el("div", undefined, "home-section-heading");
      botsHead.append(el("h2", t("Your bots"), "section-title"), el("small", String(bots.length), "muted"));
      botsSec.append(botsHead);

      if (!bots.length) {
        const emptyBox = el("div", undefined, "bots-empty-card inset-group");
        emptyBox.append(
          el("p", t("No bots configured yet."), "empty"),
          button(t("Create Bot"), () => openCreateBotSheet(ctx, t, () => load(true)), "primary"),
        );
        botsSec.append(emptyBox);
      } else {
        const group = el("div", undefined, "inset-group");
        for (const bot of bots) {
          const openItems = items.filter((i) => i.owner_bot_id === bot.id && i.status === "needs_you").length;
          const botErrors = statuses.filter((s) => s.bot_id === bot.id && s.enabled && s.state === "error").length;
          const row = el("div", undefined, "bot-card-row");
          const tap = button(
            "",
            () => ctx.go(`/bots/${encodeURIComponent(bot.id)}`),
            "bot-card-main",
          );
          tap.setAttribute("aria-label", bot.name);
          const avatar = renderAvatar(el, bot, avatars.get(bot.id));
          const copy = el("span", undefined, "bots-row-main");
          const titleLine = el("span", undefined, "bot-title-line");
          titleLine.append(
            el("strong", bot.name),
            el(
              "span",
              t(bot.paused ? "Paused" : "Active"),
              `bot-status-pill ${bot.paused ? "unknown" : "connected"}`,
            ),
          );
          if (openItems > 0) {
            titleLine.append(el("span", t("{0} needs you", openItems), "bot-status-pill waiting"));
          }
          if (botErrors > 0) {
            titleLine.append(el("span", t("Connector error"), "bot-status-pill blocked"));
          }
          copy.append(titleLine);
          if (bot.description) copy.append(el("small", bot.description, "muted"));
          tap.append(avatar, copy, ctx.icon("chevronRight"));

          const chatBtn = button(
            t("Open Chat"),
            () => openBotSession(ctx, t, bot.id),
            "secondary small bot-chat-btn",
          );
          chatBtn.disabled = !ready() || !!bot.paused;
          if (bot.paused) chatBtn.title = t("Paused bots cannot start chat");
          row.append(tap, chatBtn);
          group.append(row);
        }
        botsSec.append(group);
        if (botsCursor) {
          const more = button(t("Load more"), loadMoreBots, "secondary");
          more.disabled = !ready();
          botsSec.append(more);
        }
      }
      nodes.push(botsSec);
      nodes.push(accessSection(ctx, t, requests, bots, ready, () => load(false)));
      nodes.push(renderGroupPanel());

      sync(list, nodes);
    }

    load();
    return {
      update() {
        const curPhase = ctx.state.connection?.phase;
        if (curPhase !== lastPhase) {
          lastPhase = curPhase;
          if (ready()) load(false);
          else draw();
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
        const [botsRes, itemsRes, statusRes, sugRes, accRes] = await Promise.all([
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
        ]);
        if (ctx.signal.aborted || seq !== loadSeq) return;
        bot = (botsRes?.bots || []).find((b) => b.id === botId) || null;
        if (h1 && bot?.name) h1.textContent = bot.name;
        items = itemsRes?.items || [];
        itemsCursor = itemsRes?.next_cursor || null;
        statuses = statusRes?.statuses || [];
        suggestions = sugRes?.suggestions || [];
        requests = accRes?.requests || [];
        if (bot?.avatar && !avatarInfo) {
          ctx.rpc({ action: "bots.avatar.read", bot_id: botId })
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
          el("small", [bot.agent_id, bot.model, bot.paused ? t("Paused") : t("Active")].filter(Boolean).join(" · "), "muted"),
        );
        if (bot.description) meta.append(el("small", bot.description, "muted"));
        left.append(renderAvatar(el, bot, avatarInfo), meta);
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
        top.append(left, lookBtn);
        hero.append(top);
        nodes.push(hero);
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
              [statusLabel(st.state, t), st.account_label, st.error_category].filter(Boolean).join(" · "),
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

      // Items inbox
      const itemSec = el("section", undefined, "bots-section");
      const itemHead = el("div", undefined, "home-section-heading");
      itemHead.append(el("h2", t("Items"), "section-title"));
      const addBtn = button(`+ ${t("New Item")}`, openCreateItemSheet, "filter-chip");
      addBtn.disabled = !ready();
      itemHead.append(addBtn);
      itemSec.append(itemHead);

      const filters = el("div", undefined, "bot-decision-options");
      const closedBtn = button(t("Include closed"), () => {
        includeClosed = !includeClosed;
        return load();
      }, includeClosed ? "filter-chip active" : "filter-chip");
      const snoozeBtn = button(t("Include snoozed"), () => {
        includeSnoozed = !includeSnoozed;
        return load();
      }, includeSnoozed ? "filter-chip active" : "filter-chip");
      closedBtn.disabled = snoozeBtn.disabled = !ready();
      filters.append(closedBtn, snoozeBtn);
      itemSec.append(filters);
      itemSec.append(renderItemsList(ctx, t, { items, bots: bot ? [bot] : [], showBot: false, ready, onReload: load, cardMap }));
      if (itemsCursor) {
        const more = button(t("Load more"), loadMoreItems, "secondary");
        more.disabled = !ready();
        itemSec.append(more);
      }
      nodes.push(itemSec);

      if (requests.some((r) => r.status === "pending")) {
        nodes.push(accessSection(ctx, t, requests, bot ? [bot] : [], ready, load));
      }

      // Quick Reply (fail-closed if no external conversation link)
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
      replyInput.disabled = replyBtn.disabled = !ready();
      replyForm.append(replyInput, replyBtn);
      replyBox.append(replyForm);
      replySec.append(replyBox);
      nodes.push(replySec);

      // Soul & Memory
      const docSec = el("section", undefined, "bots-section");
      const docHead = el("div", undefined, "home-section-heading");
      docHead.append(el("h2", t("Soul & Memory"), "section-title"));
      const loadDocsBtn = button(
        t(soulLoaded ? "Reload" : "Load"),
        async () => {
          const [sRes, mRes] = await Promise.all([
            ctx.rpc({ action: "bots.soul.get", bot_id: botId }),
            ctx.rpc({ action: "bots.memory.get", bot_id: botId }),
          ]);
          if (ctx.signal.aborted) return;
          soulText = sRes?.content || "";
          memoryText = mRes?.content || "";
          soulLoaded = true;
          draw();
        },
        "filter-chip",
      );
      loadDocsBtn.disabled = !ready();
      docHead.append(loadDocsBtn);
      docSec.append(docHead);

      if (soulLoaded) {
        const docCard = el("div", undefined, "inset-group bot-docs-card");
        const soulArea = el("textarea");
        soulArea.rows = 4;
        soulArea.value = soulText;
        soulArea.setAttribute("aria-label", t("Soul"));
        soulArea.addEventListener("input", () => (soulText = soulArea.value));
        const saveSoul = button(
          t("Save Soul"),
          async () => {
            await ctx.rpc({ action: "bots.soul.set", bot_id: botId, content: soulText });
            ctx.toast?.(t("Soul saved"));
          },
          "secondary small",
        );

        const memArea = el("textarea");
        memArea.rows = 4;
        memArea.value = memoryText;
        memArea.setAttribute("aria-label", t("Memory"));
        memArea.addEventListener("input", () => (memoryText = memArea.value));
        const delToggle = button(
          t(allowMemoryDelete ? "Deletions allowed" : "Protect facts"),
          () => {
            allowMemoryDelete = !allowMemoryDelete;
            draw();
          },
          allowMemoryDelete ? "filter-chip active" : "filter-chip",
        );
        const saveMem = button(
          t("Save Memory"),
          async () => {
            const res = await ctx.rpc({
              action: "bots.memory.update",
              bot_id: botId,
              content: memTextOrCurrent(),
              allow_deletions: allowMemoryDelete,
            });
            if (ctx.signal.aborted) return;
            memoryText = res?.content ?? memoryText;
            const restored = (res?.restored_facts || []).length;
            ctx.toast?.(restored ? t("Memory saved ({0} facts protected)", restored) : t("Memory saved"));
            draw();
          },
          "secondary small",
        );
        const memTextOrCurrent = () => memArea.value;
        soulArea.disabled = saveSoul.disabled = memArea.disabled = saveMem.disabled = !ready();
        docCard.append(
          el("label", "SOUL.md", "sheet-field-label"),
          soulArea,
          saveSoul,
          el("label", "MEMORY.md", "sheet-field-label"),
          memArea,
          delToggle,
          saveMem,
        );
        docSec.append(docCard);
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
    const ready = () => ctx.state.connection?.phase === "ready" && !ctx.state.cached;

    const screen = el("section", undefined, "details bots up-next screen");
    const header = ctx.header("Up Next", "/");
    const botsBtn = button(t("All Bots"), () => ctx.go("/bots"), "filter-chip");
    const refreshBtn = button(t("Refresh"), () => load(true), "icon-button refresh-button", "activity");
    header.append(botsBtn, refreshBtn);
    const list = el("div", undefined, "detail-list bots-list");
    screen.append(header, list);
    root.append(screen);

    let filter = "needs_you";
    let bots = [];
    let items = [];
    let itemsCursor = null;
    let requests = [];
    let suggestions = [];
    let loading = false;
    let failure = null;
    let loadSeq = 0;
    const cardMap = new Map();

    async function load(reset = true) {
      if (!ready()) {
        draw();
        return;
      }
      const seq = ++loadSeq;
      loading = true;
      failure = null;
      draw();
      try {
        const includeClosed = filter === "closed";
        const includeSnoozed = filter === "snoozed";
        const [botsRes, itemsRes, accessRes, sugRes] = await Promise.all([
          ctx.rpc({ action: "bots.list", limit: 100 }),
          ctx.rpc({
            action: "bots.items",
            include_closed: includeClosed,
            include_snoozed: includeSnoozed,
            limit: 25,
          }),
          ctx.rpc({ action: "bots.access.list", limit: 50 }),
          ctx.rpc({ action: "bots.suggestions.list", include_dismissed: false }),
        ]);
        if (ctx.signal.aborted || seq !== loadSeq) return;
        bots = botsRes?.bots || [];
        items = itemsRes?.items || [];
        itemsCursor = itemsRes?.next_cursor || null;
        requests = accessRes?.requests || [];
        suggestions = sugRes?.suggestions || [];
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
      if (!itemsCursor || !ready()) return;
      const res = await ctx.rpc({
        action: "bots.items",
        include_closed: filter === "closed",
        include_snoozed: filter === "snoozed",
        limit: 25,
        cursor: itemsCursor,
      });
      if (ctx.signal.aborted) return;
      const seen = new Set(items.map((x) => x.uid));
      for (const item of res?.items || []) if (!seen.has(item.uid)) items.push(item);
      itemsCursor = res?.next_cursor || null;
      draw();
    }

    function draw() {
      if (ctx.signal.aborted) return;
      refreshBtn.disabled = loading || !ready();
      const nodes = [];
      if (!ready()) nodes.push(el("p", t("Reconnect to manage bots."), "muted bots-offline-hint"));
      if (failure) nodes.push(el("p", failure, "account-error"));

      if (requests.some((r) => r.status === "pending")) {
        nodes.push(accessSection(ctx, t, requests, bots, ready, () => load(true)));
      }
      const sugNode = renderSuggestionsSection(ctx, t, suggestions, bots, ready, () => load(true), true);
      if (sugNode) nodes.push(sugNode);

      const itemSec = el("section", undefined, "bots-section");
      itemSec.append(el("h2", t("Needs You & Active Items"), "section-title"));
      const filterBar = el("div", undefined, "bot-decision-options");
      for (const [id, lbl] of [
        ["needs_you", t("Needs You")],
        ["open", t("All Open")],
        ["snoozed", t("Snoozed")],
        ["closed", t("Closed")],
      ]) {
        const btn = button(lbl, () => {
          filter = id;
          return load(true);
        }, filter === id ? "filter-chip active" : "filter-chip");
        btn.disabled = !ready();
        filterBar.append(btn);
      }
      itemSec.append(filterBar);

      const visibleItems =
        filter === "needs_you" ? items.filter((i) => i.status === "needs_you") : items;
      itemSec.append(
        renderItemsList(ctx, t, {
          items: visibleItems,
          bots,
          showBot: true,
          ready,
          onReload: () => load(true),
          cardMap,
        }),
      );
      if (itemsCursor) {
        const more = button(t("Load more"), loadMore, "secondary");
        more.disabled = !ready();
        itemSec.append(more);
      }
      nodes.push(itemSec);
      sync(list, nodes);
    }

    load(true);
    return { update: draw };
  };
})();
