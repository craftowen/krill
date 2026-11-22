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

  const rpcHost = (ctx, hostId, req) => (hostId && ctx.rpcForHost ? ctx.rpcForHost(hostId, req) : ctx.rpc(req));
  const refreshHost = (ctx, hostId) => (hostId && ctx.refreshHost ? ctx.refreshHost(hostId) : ctx.refresh?.());
  const openBotDetail = (ctx, botId, hostId) => {
    if (hostId && ctx.activateHost) ctx.activateHost(hostId);
    ctx.go(`/bots/${encodeURIComponent(botId)}`);
  };

  async function openBotSession(ctx, t, botId, prompt, hostId) {
    const res = await rpcHost(ctx, hostId, {
      action: "bots.chat.open",
      bot_id: botId,
      ...(prompt ? { prompt } : {}),
    });
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

  function accessSection(ctx, t, requests, bots, ready, reload, multiHost) {
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
    const { items, bots, showBot, ready, onReload, cardMap, multiHost, hideEmpty } = opts;
    const wrap = el("div", undefined, "bots-items-list");
    if (!items.length) {
      if (!hideEmpty) wrap.append(el("p", t("No items match this filter."), "empty"));
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
      if (targetHosts.length > 1) {
        for (const h of targetHosts) {
          const opt = el("option", h.name);
          opt.value = h.id;
          hostSelect.append(opt);
        }
        hostSelect.value = pickedHostId;
        hostSelect.addEventListener("change", () => {
          pickedHostId = hostSelect.value || null;
        });
      }

      const nameInput = el("input");
      nameInput.type = "text";
      nameInput.placeholder = t("Bot name");
      nameInput.setAttribute("aria-label", t("Bot name"));

      const agentSelect = el("select");
      agentSelect.setAttribute("aria-label", t("Agent"));
      const rawAgents = agentsFor(pickedHostId);
      const agentChoices = rawAgents.length ? rawAgents : [{ id: "claude", name: "Claude Code" }];
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
    let botsCursor = null;
    let botsCursorHost = null;
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
    let loading = false;
    let loaded = false;
    let failure = null;
    let loadSeq = 0;
    let lastHostSig = "";

    const hostSig = () => botHosts().map((h) => h.id ?? "local").sort().join(",");

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
      activeGroupHost = hostId || null;
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
        botsCursor = null;
        botsCursorHost = null;
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
        const firstCursor = perHost.find((r) => r.cursor);
        botsCursor = firstCursor?.cursor || null;
        botsCursorHost = firstCursor?.host?.id || null;
        items = perHost.flatMap((r) => r.items);
        requests = perHost.flatMap((r) => r.requests);
        statuses = perHost.flatMap((r) => r.statuses);
        groups = perHost.flatMap((r) => r.groups);
        loaded = true;
        if (activeGroupId && groups.some((g) => g.id === activeGroupId && g._hostId === activeGroupHost)) {
          await loadGroup(activeGroupId, activeThreadId, activeGroupHost).catch(() => {});
        }
        for (const b of bots) {
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
      const res = await rpcHost(ctx, botsCursorHost, { action: "bots.list", limit: 50, cursor: botsCursor });
      if (ctx.signal.aborted) return;
      const hostName = botHosts().find((h) => h.id === botsCursorHost)?.name || "";
      const seen = new Set(bots.map((b) => `${b._hostId || ""}:${b.id}`));
      for (const b of res?.bots || []) {
        const k = `${botsCursorHost || ""}:${b.id}`;
        if (!seen.has(k)) bots.push({ ...b, _hostId: botsCursorHost, _hostName: hostName });
      }
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
          const isActive = activeGroupId === g.id && activeGroupHost === g._hostId;
          const row = button(
            "",
            () => {
              if (isActive) {
                activeGroupId = null;
                activeGroupHost = null;
                groupView = null;
                draw();
              } else {
                return loadGroup(g.id, null, g._hostId);
              }
            },
            `bots-group-row${isActive ? " active" : ""}`,
          );
          const main = el("span", undefined, "bots-row-main");
          const sub = [(g.bot_ids || []).map((id) => botDisplayName(bots, id)).join(", "), multiHost() && g._hostName ? g._hostName : null].filter(Boolean).join(" · ");
          main.append(el("strong", g.name), el("small", sub, "muted"));
          row.append(main, ctx.icon(isActive ? "chevronDown" : "chevronRight"));
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
            () => loadGroup(activeGroupId, th.thread_id, activeGroupHost),
            th.thread_id === activeThreadId ? "filter-chip active" : "filter-chip",
          );
          tb.disabled = !ready();
          threadBar.append(tb);
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
        threadBar.append(newThread);
        top.append(threadBar);

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

        const draftKey = `bot-group:${activeGroupHost || ""}:${activeGroupId}`;
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
          await rpcHost(ctx, activeGroupHost, {
            action: "bots.groups.send",
            group_id: activeGroupId,
            ...(activeThreadId ? { thread_id: activeThreadId } : {}),
            text,
            reply_mode: modeSelect.value,
          });
          if (ctx.signal.aborted) return;
          textarea.value = "";
          ctx.state.drafts?.delete?.(draftKey);
          await loadGroup(activeGroupId, activeThreadId, activeGroupHost);
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
            const aNeeds = items.some((i) => i.owner_bot_id === a.id && i._hostId === a._hostId && i.status === "needs_you") ? 0 : 1;
            const bNeeds = items.some((i) => i.owner_bot_id === b.id && i._hostId === b._hostId && i.status === "needs_you") ? 0 : 1;
            if (aNeeds !== bNeeds) return aNeeds - bNeeds;
            return Number(!!a.paused) - Number(!!b.paused);
          });
          const group = el("div", undefined, "inset-group");
          const showDev = multiHost();
          for (const bot of sortedBots) {
            const openItems = items.filter((i) => i.owner_bot_id === bot.id && i._hostId === bot._hostId && i.status === "needs_you").length;
            const botErrors = statuses.filter((s) => s.bot_id === bot.id && s._hostId === bot._hostId && s.enabled && s.state === "error").length;
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
          if (botsCursor) {
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
        nodes.push(accessSection(ctx, t, requests, bots, ready, () => load(false), multiHost()));
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
    let itemsCursor = null;
    let itemsCursorHost = null;
    let requests = [];
    let suggestions = [];
    let loading = false;
    let failure = null;
    let loadSeq = 0;
    let lastHostSig = "";
    const cardMap = new Map();
    const orchEntries = new Map();

    const hostSig = () => botHosts().map((h) => h.id ?? "local").sort().join(",");

    async function load(reset = true) {
      const targets = botHosts();
      lastHostSig = hostSig();
      if (!targets.length) {
        bots = [];
        items = [];
        itemsCursor = null;
        itemsCursorHost = null;
        requests = [];
        suggestions = [];
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
        const perHost = await Promise.all(
          targets.map(async (h) => {
            const [botsRes, itemsRes, accessRes, sugRes] = await Promise.all([
              rpcHost(ctx, h.id, { action: "bots.list", limit: 100 }),
              rpcHost(ctx, h.id, {
                action: "bots.items",
                include_closed: includeClosed,
                include_snoozed: includeSnoozed,
                limit: 25,
              }),
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
        const firstCursor = perHost.find((r) => r.cursor);
        itemsCursor = firstCursor?.cursor || null;
        itemsCursorHost = firstCursor?.host?.id || null;
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
      if (!itemsCursor || !ready()) return;
      const res = await rpcHost(ctx, itemsCursorHost, {
        action: "bots.items",
        include_closed: filter === "closed",
        include_snoozed: filter === "snoozed",
        limit: 25,
        cursor: itemsCursor,
      });
      if (ctx.signal.aborted) return;
      const hostName = botHosts().find((h) => h.id === itemsCursorHost)?.name || "";
      const seen = new Set(items.map((x) => `${x._hostId || ""}:${x.uid}`));
      for (const item of res?.items || []) {
        const k = `${itemsCursorHost || ""}:${item.uid}`;
        if (!seen.has(k)) items.push({ ...item, _hostId: itemsCursorHost, _hostName: hostName });
      }
      itemsCursor = res?.next_cursor || null;
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
      const sec = el("section", undefined, "bots-section todo-orch-section");
      const head = el("div", undefined, "home-section-heading");
      head.append(el("h2", t("Confirmation requests"), "section-title"), el("small", String(pending.length), "muted"));
      const wrap = el("div", undefined, "bots-items-list");
      const showDev = multiHost();
      const liveKeys = new Set();

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
        wrap.append(e.card);
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
          await refreshHost(ctx, e.host.id)?.catch?.(() => {});
        } catch (err) {
          e.message = `${t("Could not send reply")}: ${safeMsg(err, t)}`;
        } finally {
          e.pending = false;
          if (!ctx.signal.aborted) draw();
        }
      }

      for (const [k, e] of orchEntries) if (!liveKeys.has(k) && !e.pending) orchEntries.delete(k);
      sec.append(head, wrap);
      return { count: pending.length, node: sec };
    }

    function draw() {
      if (ctx.signal.aborted) return;
      const hosts = filteredHosts();
      const hasBotsCap = botHosts().length > 0;
      refreshBtn.disabled = loading || !hosts.some((h) => h.connection?.phase === "ready");
      const nodes = [];
      if (failure) nodes.push(el("p", failure, "account-error"));

      const waitingSec = renderWaitingSessionsSection(hosts);
      const orchSec = renderOrchestrationSection(hosts);
      const pendingAccess = requests.filter((r) => r.status === "pending");
      const visibleItems = filter === "needs_you" ? items.filter((i) => i.status === "needs_you") : items;

      if (waitingSec.node) nodes.push(waitingSec.node);
      if (orchSec.node) nodes.push(orchSec.node);
      if (pendingAccess.length) {
        nodes.push(accessSection(ctx, t, requests, bots, ready, () => load(true), multiHost()));
      }
      const sugNode = renderSuggestionsSection(ctx, t, suggestions, bots, ready, () => load(true), true, multiHost());
      if (sugNode) nodes.push(sugNode);

      const totalEverything = waitingSec.count + orchSec.count + pendingAccess.length + suggestions.length + visibleItems.length;

      if (hasBotsCap) {
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

        if (filter === "needs_you" && totalEverything === 0 && !loading) {
          const clearCard = el("div", undefined, "todo-clear-card inset-group");
          const clearTitle = el("div", undefined, "todo-clear-title");
          clearTitle.append(ctx.icon("check"), el("strong", t("All caught up")));
          clearCard.append(clearTitle, el("p", t("Your agents are working autonomously."), "muted"));
          itemSec.append(clearCard);
        } else {
          itemSec.append(
            renderItemsList(ctx, t, {
              items: visibleItems,
              bots,
              showBot: true,
              ready,
              onReload: () => load(true),
              cardMap,
              multiHost: multiHost(),
              hideEmpty: filter === "needs_you" && totalEverything > 0,
            }),
          );
        }
        if (itemsCursor) {
          const more = button(t("Load more"), loadMore, "secondary");
          more.disabled = !ready();
          itemSec.append(more);
        }
        nodes.push(itemSec);
      } else if (totalEverything === 0) {
        const clearCard = el("div", undefined, "todo-clear-card inset-group");
        const clearTitle = el("div", undefined, "todo-clear-title");
        clearTitle.append(ctx.icon("check"), el("strong", t("All caught up")));
        clearCard.append(clearTitle, el("p", t("Your agents are working autonomously."), "muted"));
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
