// Orca mobile conversation/tool disclosure reference: MIT, Copyright (c) 2026 Lovecast Inc.
(() => {
  const M = (globalThis.KrillMobile ||= {});
  M.screens ||= {};
  function cleanRpcError(err, t) {
    const raw = String(err?.message || err || "").trim();
    const tr = typeof t === "function" ? t : (s) => s;
    if (!raw)
      return tr("Request failed. Check the desktop connection and try again.");
    if (
      /os error|Broken pipe|Connection refused|Connection reset|unexpected control reply|unexpected daemon reply|unexpected daemon response|invalid mobile authentication or frame|\bbad request\b|Session \d+:|Usage API|\bHTTP\s*\d{3}\b|\/Users\/|\/home\/|\/var\/|\/tmp\/|\.sock\b/i.test(
        raw,
      )
    ) {
      return tr("Request failed. Check the desktop connection and try again.");
    }
    return tr(raw);
  }
  const EFFORT_BARS = new Map([
    ["none", 0],
    ["minimal", 1],
    ["low", 1],
    ["medium", 2],
    ["high", 3],
    ["xhigh", 4],
    ["max", 5],
    ["ultra", 5],
  ]);
  const EFFORT_ORDER = new Map([
    ["none", 0],
    ["off", 0],
    ["minimal", 1],
    ["low", 2],
    ["medium", 3],
    ["high", 4],
    ["xhigh", 5],
    ["max", 6],
    ["ultra", 7],
  ]);
  function sortEfforts(efforts) {
    const list = Array.isArray(efforts) ? efforts : [],
      ranked = list
        .filter((e) => EFFORT_ORDER.has(String(e || "").toLowerCase()))
        .sort(
          (a, b) =>
            EFFORT_ORDER.get(String(a).toLowerCase()) -
            EFFORT_ORDER.get(String(b).toLowerCase()),
        );
    let next = 0;
    return list.map((e) =>
      EFFORT_ORDER.has(String(e || "").toLowerCase()) ? ranked[next++] : e,
    );
  }
  const EFFORT_LABELS = new Map([
    ["none", "None"],
    ["minimal", "Minimal"],
    ["low", "Low"],
    ["medium", "Medium"],
    ["high", "High"],
    ["xhigh", "Extra high"],
    ["max", "Max"],
    ["ultra", "Ultra"],
  ]);
  const fullModel = (text) => {
    const name = shortModel(text);
    return name.charAt(0).toUpperCase() + name.slice(1);
  };
  function shortModel(text) {
    const name = text
        .toLowerCase()
        .replace(/\s*[([].*$/, "")
        .replace(/-\d{8}$/, ""),
      [, family, major, minor] =
        /^claude-(.+?)-(\d+)(?:-(\d+))?$/.exec(name) || [];
    if (family) return `${family} ${major}${minor ? `.${minor}` : ""}`;
    return name.replace(/^claude[- ]/, "");
  }
  // Build inline formatting with text nodes/elements only; transcript HTML never becomes markup.
  function renderInline(el, parent, text, depth = 0) {
    if (depth >= 8) {
      parent.append(el("span", text));
      return;
    }
    const tokens =
      /\\([\\`*_[\]])|(`+)(.*?)\2(?!`)|(\*\*|__|\*)(?=\S)(.+?\S|\S)\4|\[([^\]\n]+)\]\(([^)\s]+)\)/g;
    let end = 0;
    for (const match of text.matchAll(tokens)) {
      if (match.index > end)
        parent.append(el("span", text.slice(end, match.index)));
      const [, escaped, ticks, code, mark, body, label, href] = match;
      if (escaped) parent.append(el("span", escaped));
      else if (ticks) parent.append(el("code", code, "chat-inline-code"));
      else if (mark) {
        const node = el(mark.length === 2 ? "strong" : "em");
        renderInline(el, node, body, depth + 1);
        parent.append(node);
      } else if (/^(https?:\/\/|mailto:)/i.test(href)) {
        const node = el("a");
        node.setAttribute("href", href);
        node.setAttribute("target", "_blank");
        node.setAttribute("rel", "noopener noreferrer");
        renderInline(el, node, label, depth + 1);
        parent.append(node);
      } else parent.append(el("span", match[0]));
      end = match.index + match[0].length;
    }
    if (end < text.length) parent.append(el("span", text.slice(end)));
  }
  const SVG_NS = "http://www.w3.org/2000/svg";
  const CHAT_SVG_SHAPES = {
    stop: [{ tag: "rect", x: "6", y: "6", width: "12", height: "12", rx: "2" }],
    textWrap: [
      { tag: "path", d: "m16 16-3 3 3 3" },
      { tag: "path", d: "M3 12h14.5a1 1 0 0 1 0 7H13" },
      { tag: "path", d: "M3 19h6" },
      { tag: "path", d: "M3 5h18" },
    ],
    brain: [
      { tag: "path", d: "M12 18V5" },
      { tag: "path", d: "M15 13a4.17 4.17 0 0 1-3-4 4.17 4.17 0 0 1-3 4" },
      { tag: "path", d: "M17.598 6.5A3 3 0 1 0 12 5a3 3 0 1 0-5.598 1.5" },
      { tag: "path", d: "M17.997 5.125a4 4 0 0 1 2.526 5.77" },
      { tag: "path", d: "M18 18a4 4 0 0 0 2-7.464" },
      { tag: "path", d: "M19.967 17.483A4 4 0 1 1 12 18a4 4 0 1 1-7.967-.517" },
      { tag: "path", d: "M6 18a4 4 0 0 1-2-7.464" },
      { tag: "path", d: "M6.003 5.125a4 4 0 0 0-2.526 5.77" },
    ],
    eye: [
      {
        tag: "path",
        d: "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0",
      },
      { tag: "circle", cx: "12", cy: "12", r: "3" },
    ],
    search: [
      { tag: "path", d: "m21 21-4.34-4.34" },
      { tag: "circle", cx: "11", cy: "11", r: "8" },
    ],
    folder: [
      {
        tag: "path",
        d: "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z",
      },
    ],
    "square-terminal": [
      { tag: "path", d: "m7 11 2-2-2-2" },
      { tag: "path", d: "M11 13h4" },
      { tag: "rect", width: "18", height: "18", x: "3", y: "3", rx: "2", ry: "2" },
    ],
    pencil: [
      {
        tag: "path",
        d: "M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z",
      },
      { tag: "path", d: "m15 5 4 4" },
    ],
    globe: [
      { tag: "circle", cx: "12", cy: "12", r: "10" },
      { tag: "path", d: "M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20" },
      { tag: "path", d: "M2 12h20" },
    ],
    plug: [
      { tag: "path", d: "M12 22v-5" },
      { tag: "path", d: "M15 8V2" },
      {
        tag: "path",
        d: "M17 8a1 1 0 0 1 1 1v4a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1z",
      },
      { tag: "path", d: "M9 8V2" },
    ],
    bot: [
      { tag: "path", d: "M12 8V4H8" },
      { tag: "rect", width: "16", height: "12", x: "4", y: "8", rx: "2" },
      { tag: "path", d: "M2 14h2" },
      { tag: "path", d: "M20 14h2" },
      { tag: "path", d: "M15 13v2" },
      { tag: "path", d: "M9 13v2" },
    ],
    "list-checks": [
      { tag: "path", d: "M13 5h8" },
      { tag: "path", d: "M13 12h8" },
      { tag: "path", d: "M13 19h8" },
      { tag: "path", d: "m3 17 2 2 4-4" },
      { tag: "path", d: "m3 7 2 2 4-4" },
    ],
    wrench: [
      {
        tag: "path",
        d: "M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.106-3.105c.32-.322.863-.22.983.218a6 6 0 0 1-8.259 7.057l-7.91 7.91a1 1 0 0 1-2.999-3l7.91-7.91a6 6 0 0 1 7.057-8.259c.438.12.54.662.219.984z",
      },
    ],
    chevronRight: [{ tag: "path", d: "m9 18 6-6-6-6" }],
    check: [{ tag: "path", d: "M20 6 9 17l-5-5" }],
  };
  function svgIcon(name) {
    const doc = globalThis.document,
      svg = doc?.createElementNS?.(SVG_NS, "svg");
    if (!svg) return null;
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    if (name === "stop") {
      svg.setAttribute("fill", "currentColor");
    } else {
      svg.setAttribute("fill", "none");
      svg.setAttribute("stroke", "currentColor");
      svg.setAttribute("stroke-width", "1.8");
      svg.setAttribute("stroke-linecap", "round");
      svg.setAttribute("stroke-linejoin", "round");
    }
    for (const shape of CHAT_SVG_SHAPES[name] || CHAT_SVG_SHAPES.wrench) {
      const node = doc.createElementNS(SVG_NS, shape.tag);
      for (const [k, v] of Object.entries(shape))
        if (k !== "tag") node.setAttribute(k, v);
      svg.append(node);
    }
    return svg;
  }
  const CODE_COPY_FEEDBACK_MS = 1500,
    USER_FOLD_MAX_LINES = 8,
    USER_FOLD_MAX_LENGTH = 600;
  function userMessageFolds(text) {
    const raw = String(text || "");
    return (
      raw.length > USER_FOLD_MAX_LENGTH ||
      raw.split(/\r?\n/).length > USER_FOLD_MAX_LINES
    );
  }
  const CODE_LANG_LABELS = new Map([
    ["ts", "TypeScript"],
    ["typescript", "TypeScript"],
    ["tsx", "TSX"],
    ["js", "JavaScript"],
    ["javascript", "JavaScript"],
    ["mjs", "JavaScript"],
    ["cjs", "JavaScript"],
    ["jsx", "JSX"],
    ["py", "Python"],
    ["python", "Python"],
    ["rs", "Rust"],
    ["rust", "Rust"],
    ["sh", "Bash"],
    ["bash", "Bash"],
    ["shell", "Shell"],
    ["zsh", "Zsh"],
    ["yml", "YAML"],
    ["yaml", "YAML"],
    ["md", "Markdown"],
    ["markdown", "Markdown"],
    ["json", "JSON"],
    ["jsonc", "JSONC"],
    ["toml", "TOML"],
    ["sql", "SQL"],
    ["css", "CSS"],
    ["scss", "SCSS"],
    ["html", "HTML"],
    ["xml", "XML"],
    ["go", "Go"],
    ["golang", "Go"],
    ["rb", "Ruby"],
    ["ruby", "Ruby"],
    ["c", "C"],
    ["cpp", "C++"],
    ["c++", "C++"],
    ["cxx", "C++"],
    ["cs", "C#"],
    ["csharp", "C#"],
    ["java", "Java"],
    ["kt", "Kotlin"],
    ["kotlin", "Kotlin"],
    ["swift", "Swift"],
    ["php", "PHP"],
    ["diff", "Diff"],
    ["graphql", "GraphQL"],
    ["gql", "GraphQL"],
    ["mermaid", "Mermaid"],
    ["ps1", "PowerShell"],
    ["powershell", "PowerShell"],
    ["dockerfile", "Dockerfile"],
    ["docker", "Dockerfile"],
    ["txt", "Plain text"],
    ["text", "Plain text"],
    ["plaintext", "Plain text"],
  ]);
  function fenceLang(info) {
    const token = String(info || "")
      .replace(/[\u0000-\u001f\u007f-\u009f]/g, "")
      .trim()
      .split(/\s+/)[0];
    if (!token) return "Code";
    return CODE_LANG_LABELS.get(token.toLowerCase()) || token;
  }
  function fenceLine(line) {
    const match = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    return match
      ? { char: match[1][0], length: match[1].length, info: match[2] }
      : null;
  }
  function parseRichBlocks(text) {
    const raw = String(text || "").replace(/\r\n?/g, "\n"),
      lines = raw.split("\n"),
      out = [];
    let buffer = [],
      opening = null,
      blockIndex = 0;
    for (const line of lines) {
      const fence = fenceLine(line);
      if (fence) {
        if (opening) {
          if (
            fence.char === opening.char &&
            fence.length >= opening.length &&
            !fence.info.trim()
          ) {
            out.push({
              kind: "code",
              lang: opening.lang,
              code: buffer.join("\n"),
              blockIndex: blockIndex++,
            });
            buffer = [];
            opening = null;
            continue;
          }
        } else if (fence.char !== "`" || !fence.info.includes("`")) {
          if (buffer.length) {
            out.push({ kind: "prose", text: buffer.join("\n") });
            buffer = [];
          }
          opening = {
            char: fence.char,
            length: fence.length,
            lang: fenceLang(fence.info),
          };
          continue;
        }
      }
      buffer.push(line);
    }
    if (opening) {
      out.push({
        kind: "code",
        lang: opening.lang,
        code: buffer.join("\n"),
        blockIndex: blockIndex++,
      });
    } else if (buffer.length) {
      out.push({ kind: "prose", text: buffer.join("\n") });
    }
    return out;
  }
  async function writeClipboardText(el, t, text) {
    // 클립보드 권한이 거부되면(일부 웹뷰) 선택 복사로 한 번 더 시도한다.
    let copied = false;
    if (globalThis.isSecureContext && globalThis.navigator?.clipboard)
      copied = await globalThis.navigator.clipboard.writeText(text).then(
        () => true,
        () => false,
      );
    if (!copied) {
      const doc = globalThis.document,
        area = el("textarea");
      area.value = text;
      area.setAttribute("readonly", "");
      area.className = "copy-buffer";
      doc?.body?.append(area);
      area.select?.();
      const ok = !!doc?.execCommand?.("copy");
      area.remove?.();
      if (!ok) throw Error(t("Copy is not available in this browser"));
    }
  }
  function renderRichText(el, text, richOpts) {
    const translate = richOpts?.t || ((s) => s),
      wrap = el("div", undefined, "chat-md"),
      blocks = parseRichBlocks(text);
    const rich = (tag, value, cls) => {
      const node = el(tag, undefined, cls);
      renderInline(el, node, value);
      return node;
    };
    const cells = (line) =>
      line
        .trim()
        .replace(/^\|/, "")
        .replace(/\|$/, "")
        .split("|")
        .map((s) => s.trim());
    const tableLine = (line) => /^\s*\|.*\|\s*$/.test(line),
      separator = (line) => cells(line).every((s) => /^:?-{3,}:?$/.test(s));
    for (const block of blocks) {
      if (block.kind === "code") {
        const codeWrap = el("div", undefined, "chat-code-wrap"),
          head = el("div", undefined, "chat-code-head"),
          langEl = el(
            "span",
            block.lang === "Code" ? translate("Code") : block.lang,
            "chat-code-lang",
          ),
          actions = el("div", undefined, "chat-code-actions"),
          pre = el("pre", undefined, "chat-code");
        pre.append(el("code", block.code));
        if (richOpts?.renderCodeWrap) {
          const wrapBtn = richOpts.renderCodeWrap(block, pre);
          if (wrapBtn) actions.append(wrapBtn);
        }
        if (richOpts?.renderCodeCopy) {
          const copyBtn = richOpts.renderCodeCopy(block);
          if (copyBtn) actions.append(copyBtn);
        }
        head.append(langEl, actions);
        codeWrap.append(head, pre);
        wrap.append(codeWrap);
        continue;
      }
      const lines = block.text.split("\n");
      let list = null,
        quote = null;
      for (let n = 0; n < lines.length; n++) {
        const line = lines[n].trimEnd(),
          trimmed = line.trim();
        if (!trimmed) {
          list = quote = null;
          continue;
        }
        if (tableLine(line)) {
          list = quote = null;
          const table = el("table", undefined, "chat-table"),
            scroll = el("div", undefined, "chat-code"),
            header = tableLine(lines[n + 1] || "") && separator(lines[n + 1]);
          let first = true;
          while (n < lines.length && tableLine(lines[n])) {
            const row = el("tr");
            if (!separator(lines[n])) {
              for (const value of cells(lines[n]))
                row.append(rich(first && header ? "th" : "td", value));
              table.append(row);
              first = false;
            }
            n++;
          }
          n--;
          scroll.append(table);
          wrap.append(scroll);
          continue;
        }
        const heading = trimmed.match(/^(#{1,3})\s+(.*)$/),
          bullet = trimmed.match(/^(?:([-*])|([0-9]+)[.)])\s+(.*)$/),
          quoted = trimmed.match(/^>\s?(.*)$/);
        if (quoted) {
          list = null;
          if (!quote) {
            quote = el("blockquote", undefined, "chat-quote");
            wrap.append(quote);
          }
          quote.append(rich("p", quoted[1], "chat-p"));
        } else if (bullet) {
          quote = null;
          const tag = bullet[2] ? "ol" : "ul";
          if (!list || list.dataset.listKind !== tag) {
            list = el(tag, undefined, tag === "ol" ? "chat-ol" : undefined);
            list.dataset.listKind = tag;
            if (bullet[2]) list.setAttribute("start", bullet[2]);
            wrap.append(list);
          }
          list.append(rich("li", bullet[3], "chat-li"));
        } else {
          list = quote = null;
          wrap.append(
            rich(
              heading ? "h" + (heading[1].length + 1) : "p",
              heading ? heading[2] : line,
              heading ? "chat-h" : "chat-p",
            ),
          );
        }
      }
    }
    return wrap;
  }
  // Plain-language step names let non-developers skim what the agent did without opening tool payloads.
  function toolStep(name) {
    const n = String(name || "");
    return /^(read|view|notebookread|read_file)$/i.test(n)
      ? "Read {0} file(s)"
      : /edit|write|patch/i.test(n)
        ? "Changed {0} file(s)"
        : /grep|glob|search|find|^ls$/i.test(n)
          ? "Searched {0} time(s)"
          : /bash|shell|exec|command/i.test(n)
            ? "Ran {0} command(s)"
            : "Did {0} other step(s)";
  }
  function firstNonEmptyLine(text) {
    return (
      String(text || "")
        .split("\n")
        .map((line) => line.trim())
        .find(Boolean) || ""
    );
  }
  function toolDetail(item) {
    return (
      firstNonEmptyLine(item.title) ||
      firstNonEmptyLine(item.file_path) ||
      firstNonEmptyLine(item.text)
    );
  }
  const TOOL_CATEGORY_ICONS = {
    read: "eye",
    search: "search",
    listFiles: "folder",
    unknown: "square-terminal",
    fileChange: "pencil",
    webSearch: "globe",
    mcpToolCall: "plug",
    subAgentActivity: "bot",
    todoList: "list-checks",
    other: "wrench",
  };
  function toolCategory(name) {
    const n = String(name || "")
      .trim()
      .toLowerCase();
    if (!n) return "other";
    if (n.startsWith("mcp__")) return "mcpToolCall";
    if (/^(read|view|notebookread|read_file)$/.test(n)) return "read";
    if (
      /^(bash|sh|shell|exec|local_shell|command|run_command)$/.test(n) ||
      /command/.test(n)
    )
      return "unknown";
    if (/edit|write|patch|diff/.test(n)) return "fileChange";
    if (/webfetch|web_search|websearch/.test(n)) return "webSearch";
    if (/^(grep|glob|search|find)$/.test(n) || /search/.test(n))
      return /web/.test(n) ? "webSearch" : "search";
    if (/^(ls|list|list_dir|list_files)$/.test(n)) return "listFiles";
    if (/^(task|agent|subagent)$/.test(n)) return "subAgentActivity";
    if (/^(todowrite|update_plan)$/.test(n)) return "todoList";
    return "other";
  }
  function toolLineLabel(item) {
    const cat = toolCategory(item.tool_name),
      icon = TOOL_CATEGORY_ICONS[cat] || "wrench",
      rawPath = String(item.file_path || "").trim(),
      baseName = rawPath
        ? rawPath.split(/[\\/]/).filter(Boolean).pop() || rawPath
        : "",
      title = firstNonEmptyLine(item.title),
      lineText = firstNonEmptyLine(item.text);
    let verb = "",
      target = "";
    if (cat === "read") {
      verb = "Read";
      target = baseName || title || lineText;
    } else if (cat === "fileChange") {
      verb = "Edited";
      target = baseName || title || lineText;
    } else if (cat === "search") {
      verb = "Searched";
      target = title || rawPath || lineText;
    } else if (cat === "listFiles") {
      verb = "Listed";
      target = title || rawPath || lineText;
    } else if (cat === "unknown") {
      verb = "Ran";
      target = title || lineText;
    } else if (cat === "webSearch") {
      verb = /fetch/i.test(item.tool_name || "") ? "Fetched" : "Searched the web";
      target = title || lineText;
    } else if (cat === "subAgentActivity") {
      verb = "Subagent";
      target = title || lineText;
    } else if (cat === "todoList") {
      verb = "Updated the plan";
      target = title || lineText;
    } else if (cat === "mcpToolCall") {
      verb = item.tool_name || "Used";
      target = title || lineText;
    } else {
      verb = item.tool_name || "Used";
      target = title || rawPath || lineText;
    }
    return { icon, verb, target };
  }
  const MAX_PREAMBLE_CHARS = 160;
  function isWorkItemKind(kind) {
    return (
      kind === "tool_call" || kind === "tool_result" || kind === "reasoning"
    );
  }
  function isTurnBoundaryKind(kind) {
    return kind === "user" || kind === "question" || kind === "permission";
  }
  function isFoldablePreamble(text) {
    const trimmed = String(text || "").trim();
    if (
      !trimmed ||
      Array.from(trimmed).length > MAX_PREAMBLE_CHARS ||
      trimmed.includes("\n") ||
      trimmed.includes("\r") ||
      trimmed.includes("```")
    )
      return false;
    return !/^(?:#{1,6} |[-*>] |\d+\. )/.test(trimmed);
  }
  function isToolItemKind(kind) {
    return kind === "tool_call" || kind === "tool_result";
  }
  function computeFoldableNotes(items) {
    const foldable = new Array(items.length).fill(false);
    let seenToolInTurn = false;
    for (let idx = items.length - 1; idx >= 0; idx--) {
      const kind = items[idx]?.kind || "assistant";
      if (isTurnBoundaryKind(kind)) seenToolInTurn = false;
      else if (isToolItemKind(kind)) seenToolInTurn = true;
      else if (
        kind === "assistant" &&
        seenToolInTurn &&
        isFoldablePreamble(items[idx]?.text)
      )
        foldable[idx] = true;
    }
    return foldable;
  }
  function workRunEnd(items, start, foldable) {
    let tools = 0,
      thoughts = 0,
      idx = start;
    while (
      idx < items.length &&
      (isWorkItemKind(items[idx]?.kind) || foldable?.[idx])
    ) {
      const kind = items[idx]?.kind;
      if (kind === "reasoning") thoughts++;
      else if (isWorkItemKind(kind)) tools++;
      idx++;
    }
    return tools >= 1 || thoughts >= 2 ? idx : 0;
  }
  function parseItemAtMs(at) {
    if (at === undefined || at === null || at === "") return null;
    if (typeof at === "number")
      return Number.isFinite(at) && at > 0 ? at : null;
    const raw = String(at).trim();
    if (!raw) return null;
    const ms = /^\d+$/.test(raw)
      ? Number(raw) < 1e11
        ? Number(raw) * 1000
        : Number(raw)
      : Date.parse(raw);
    return Number.isFinite(ms) && ms > 0 ? ms : null;
  }
  function formatElapsedSecs(secs) {
    if (!Number.isFinite(secs) || secs < 1) return "";
    const whole = Math.floor(secs);
    if (whole < 60) return `${whole}s`;
    const mins = Math.floor(whole / 60),
      rem = String(whole % 60).padStart(2, "0");
    return `${mins}m ${rem}s`;
  }
  const cacheLimit = 8,
    cacheByteLimit = 8 * 1024 * 1024,
    pageLimit = 2048,
    itemByteLimit = 2 * 1024 * 1024;
  const isEntryPinned = (e) =>
      e.mounts > 0 ||
      e.sending ||
      e.stopping ||
      e.modelPending ||
      e.questionSending ||
      e.questionDraft?.changed ||
      e.questionDraft?.selected?.length ||
      e.photo ||
      e.live,
    pinned = (state, id, e) => isEntryPinned(e) || !!state.drafts?.get(id);
  function pruneCache(state) {
    const cache = state.chatCache;
    if (!cache) return;
    let bytes = 0;
    for (const e of cache.values()) bytes += e.retainedBytes || 0;
    for (const [id, e] of cache) {
      if (cache.size <= cacheLimit && bytes <= cacheByteLimit) break;
      if (!pinned(state, id, e)) {
        cache.delete(id);
        bytes -= e.retainedBytes || 0;
      }
    }
  }
  function forgetSession(state, id) {
    const e = state.chatCache?.get(id);
    if (e && !pinned(state, id, e)) state.chatCache.delete(id);
  }
  function mountChat(root, opts) {
    const { ctx, sessionId, onSwitchTerm, agentIcon } = opts,
      agentName = (a) => M.agentName?.(a) || a;
    const { el, button, state } = ctx,
      drafts = state.drafts || (state.drafts = new Map()),
      t = ctx.t || ((s) => s),
      itemLimit = 1000;
    const cacheMap = state.chatCache || (state.chatCache = new Map());
    const entry = cacheMap.get(sessionId) || {
      offset: 0,
      items: [],
      agent: "",
      bound: false,
    };
    entry.pages ||= [];
    entry.photoVersion ||= 0;
    entry.mounts = (entry.mounts || 0) + 1;
    cacheMap.delete(sessionId);
    cacheMap.set(sessionId, entry);
    pruneCache(state);
    const shell = el("div", undefined, "chat-view"),
      feed = el("div", undefined, "chat-feed"),
      composer = el("form", undefined, "chat-composer");
    const statusRow = el("div", undefined, "chat-status-row"),
      statusLine = el("div", undefined, "chat-status"),
      statusBadge = el("span", "IDLE", "badge"),
      statusText = el("span", "", "chat-live-text");
    statusLine.setAttribute("role", "status");
    statusLine.setAttribute("aria-live", "polite");
    statusLine.append(statusBadge, statusText);
    const history = el("div", undefined, "chat-actions chat-history");
    const earlierBtn = button(
      t("Load earlier messages"),
      () => run(loadEarlier),
      "ghost small",
    );
    function returnToLatest() {
      if (!active()) return;
      followLatest();
      renderItems();
      pollNow();
    }
    const latestBtn = button(t("Back to latest"), returnToLatest, "ghost small");
    history.append(earlierBtn, latestBtn);
    const pollStatus = el("div", undefined, "chat-actions"),
      pollText = el("span", undefined, "muted");
    pollStatus.setAttribute("role", "status");
    const retryBtn = button(
      t("Retry"),
      () => (failedEarlier ? run(loadEarlier) : pollNow()),
      "ghost small",
    );
    pollStatus.append(pollText, retryBtn);
    pollStatus.hidden = true;
    const input = el("textarea", undefined, "chat-input");
    input.rows = 1;
    input.placeholder = t("Message agent (Bracketed Paste)…");
    input.setAttribute("aria-label", t("Chat prompt"));
    let sizedValue = null,
      sizedWidth = -1;
    function sizeInput(force = false) {
      if (!input.style) return;
      const width = input.clientWidth || 0;
      if (!force && sizedValue === input.value && sizedWidth === width) return;
      const prev = input.style.height;
      sizedValue = input.value;
      sizedWidth = width;
      input.style.height = "auto";
      if (input.scrollHeight)
        input.style.height =
          Math.min(140, Math.max(28, input.scrollHeight)) + "px";
      if (input.style.height !== prev) {
        if (
          globalThis.document?.activeElement === input &&
          input.scrollHeight > (input.clientHeight || 0)
        )
          input.scrollTop = input.scrollHeight;
        pinLatest();
      }
    }
    input.value = drafts.get(sessionId) || "";
    function insertText(text) {
      if (!active()) return false;
      const clean = String(text ?? "")
        .replace(/\r\n?/g, "\n")
        .replace(/^\n+|\n+$/g, "");
      if (!clean) return false;
      const value = input.value,
        start =
          typeof input.selectionStart === "number"
            ? input.selectionStart
            : value.length,
        end =
          typeof input.selectionEnd === "number" ? input.selectionEnd : start;
      input.value = value.slice(0, start) + clean + value.slice(end);
      input.selectionStart = input.selectionEnd = start + clean.length;
      entry.draftVersion = (entry.draftVersion || 0) + 1;
      drafts.set(sessionId, input.value);
      sizeInput();
      closeSuggestions();
      input.focus?.();
      return true;
    }

    let composing = false,
      disposed = false,
      pollTimer = null,
      inFlight = false,
      retryWithoutTail = false,
      failures = 0,
      lastSuccess = entry.lastSuccess || 0,
      earlierPending = false,
      visible = opts.visible !== false,
      following = true;
    entry.initialized ??= entry.offset > 0;
    entry.loaded ??= entry.initialized;
    let earlierEpoch = 0,
      earlierQueued = false,
      autoBlocked = null,
      failedEarlier = false,
      prefetchedEarlier = null,
      fillPages = 0;
    let resetHistory = !!entry.latestRecovery,
      refreshLatest =
        resetHistory ||
        (!!entry.initialized && Date.now() - lastSuccess > 3000);
    // Only the reader moving up stops following; pins and late layout growth never do.
    let lastTop = 0,
      lastHeight = 0,
      lastViewport = feed.clientHeight;
    function syncScroll() {
      lastTop = feed.scrollTop;
      lastHeight = feed.scrollHeight;
      lastViewport = feed.clientHeight;
    }
    feed.addEventListener("scroll", () => {
      const top = feed.scrollTop,
        gap = feed.scrollHeight - top - feed.clientHeight;
      // Layout clamps and our own pins/prepends are not a new request to read backwards.
      const upward =
        top < lastTop - 1 &&
        lastHeight === feed.scrollHeight &&
        lastViewport === feed.clientHeight;
      syncScroll();
      // A reader back at the newest row resumes live updates. A trimmed window keeps its place.
      if (gap <= 1 && entry.live && !earlierPending && entry.items.at(-1)?.id === entry.live.items.at(-1)?.id) {
        returnToLatest();
        return;
      }
      if (!canReadEarlier()) return;
      if (gap <= 1) following = true;
      else if (upward) following = false;
      else if (gap < 60) following = true;
      syncLiveCardVisibility();
      const threshold = historyThreshold();
      if (top > threshold) {
        autoBlocked = null;
        earlierQueued = false;
      } else if (upward && !following && !earlierPending) {
        earlierQueued = true;
        drainEarlier();
      }
    });
    let touchStartY = null, touchHistory = false, wheelAt = -Infinity;
    function earlierGesture() {
      if (!capability("chat_tail") || !canReadEarlier() || feed.scrollHeight > feed.clientHeight + 1) return;
      following = false; syncLiveCardVisibility(); fillPages = 0; autoBlocked = null; earlierQueued = true; drainEarlier();
    }
    feed.addEventListener("touchstart", e => {touchStartY = e.touches?.[0]?.clientY ?? null; touchHistory = false;}, {passive:true});
    feed.addEventListener("touchmove", e => {if (touchStartY !== null && !touchHistory && (e.touches?.[0]?.clientY ?? touchStartY) - touchStartY > 12) {touchHistory = true; earlierGesture();}}, {passive:true});
    feed.addEventListener("touchend", () => touchStartY = null, {passive:true});
    feed.addEventListener("touchcancel", () => touchStartY = null, {passive:true});
    feed.addEventListener("wheel", e => {const now = Date.now(), continuing = now - wheelAt < 250; wheelAt = now; if (e.deltaY < 0 && !continuing) earlierGesture();}, {passive:true});
    function followLatest() {
      if (failedEarlier) requestLatest();
      earlierEpoch++;
      prefetchedEarlier = null;
      fillPages = 0;
      earlierQueued = false;
      failedEarlier = false;
      autoBlocked = null;
      pollStatus.hidden = true;
      if (entry.live && !resetHistory) {
        entry.items = entry.live.items;
        entry.offset = entry.live.offset;
        entry.live = null;
      }
      following = true;
      syncLiveCardVisibility();
    }
    function pinLatest() {
      if (visible && following && !entry.live)
        feed.scrollTop = feed.scrollHeight || 0;
      syncScroll();
    }
    function pinLatestSoon() {
      pinLatest();
      globalThis.requestAnimationFrame?.(() => {
        if (!disposed) pinLatest();
      });
    }
    // Late fonts, disclosures, a shown pane or keyboard resize re-pin a following reader.
    const resize = globalThis.ResizeObserver
      ? new ResizeObserver(() => pinLatest())
      : null;
    resize?.observe(feed);
    const unsubscribeKeyboard = globalThis.MkKeyboard?.subscribe?.(() => {
      if (!disposed && visible) {
        sizeInput(true);
        pinLatest();
      }
    });
    const cards = new Map(),
      noteCards = new Map(),
      toolRuns = new Map(),
      copiedCode = new Map(),
      codeButtonsByCard = new Map();
    function clearCopiedCode() {
      for (const state of copiedCode.values()) clearTimeout(state.timer);
      copiedCode.clear();
      codeButtonsByCard.clear();
    }
    let tailRun = null,
      renderedCount = 0,
      renderedItems = null,
      optimisticCard = null,
      optimisticShown = "";
    function renderWorkNote(item) {
      const trimmed = String(item.text || "").trim(),
        note = el("div", undefined, "chat-card chat-work-note"),
        badge = el("span", t("Note"), "badge"),
        textNode = el("span", trimmed, "chat-work-note-text");
      note.append(badge, textNode);
      note._text = trimmed;
      note._textNode = textNode;
      return note;
    }
    function runCategoryIcon(targetRun) {
      if (!targetRun.tools && targetRun.thoughts) return "brain";
      if (targetRun.categories?.size === 1) {
        const [only] = targetRun.categories;
        return TOOL_CATEGORY_ICONS[only] || "wrench";
      }
      return "wrench";
    }
    function syncRunSummary(targetRun, isTail) {
      if (!targetRun) return;
      const sessState = state.sessions?.get(sessionId)?.state,
        isActive = Boolean(
          isTail &&
            !entry.live &&
            targetRun.endIndex === entry.items.length &&
            sessState === "Working",
        );
      if (isActive) targetRun.details.dataset.active = "true";
      else delete targetRun.details.dataset.active;
      const progressCount =
        targetRun.totalTools > 0
          ? `${targetRun.completedTools}/${targetRun.totalTools}`
          : targetRun.thoughts > 0
            ? `${targetRun.thoughts}/${targetRun.thoughts}`
            : "";
      if (isActive && progressCount)
        targetRun.details.dataset.progress = progressCount;
      else delete targetRun.details.dataset.progress;
      let elapsedBadge = "";
      if (targetRun.minAtMs !== null) {
        const endMs = isActive ? Date.now() : targetRun.maxAtMs;
        if (endMs !== null && endMs >= targetRun.minAtMs)
          elapsedBadge = formatElapsedSecs((endMs - targetRun.minAtMs) / 1000);
      }
      if (elapsedBadge) targetRun.details.dataset.elapsed = elapsedBadge;
      else delete targetRun.details.dataset.elapsed;
      const statusPart = targetRun.errors
          ? t("Tool error")
          : !targetRun.count && targetRun.tools
            ? t("Tool activity")
            : "",
        stepParts = [
          ...[...targetRun.steps].map(([step, n]) => t(step, n)),
          !targetRun.tools && targetRun.thoughts
            ? t("Thought {0} time(s)", targetRun.thoughts)
            : !targetRun.tools && !targetRun.thoughts && targetRun.notes
              ? t("Tool activity")
              : "",
        ].filter(Boolean);
      let mainParts;
      if (isActive) {
        const pendingCall = targetRun.pending.at(-1)?.call,
          inFlightDetail = pendingCall
            ? toolDetail(pendingCall) ||
              pendingCall.tool_name ||
              t("Tool call")
            : "",
          currentDetail =
            inFlightDetail ||
            targetRun.activeDetail ||
            targetRun.detail ||
            targetRun.thoughtDetail ||
            targetRun.noteDetail;
        mainParts = [
          t("Working"),
          progressCount,
          statusPart,
          currentDetail,
          ...stepParts,
        ].filter(Boolean);
      } else {
        const finalDetail = targetRun.count
          ? targetRun.detail
          : targetRun.detail ||
            targetRun.thoughtDetail ||
            targetRun.noteDetail;
        mainParts = [statusPart, ...stepParts, finalDetail].filter(Boolean);
      }
      const mainText = mainParts.join(" · "),
        summaryNodes = [],
        iconWrap = el("span", undefined, "chat-tool-summary-icon"),
        glyph = svgIcon(runCategoryIcon(targetRun));
      iconWrap.setAttribute("aria-hidden", "true");
      if (glyph) iconWrap.append(glyph);
      summaryNodes.push(iconWrap);
      if (mainText)
        summaryNodes.push(el("span", mainText, "chat-tool-summary-text"));
      if (elapsedBadge) {
        if (mainText)
          summaryNodes.push(el("span", " · ", "chat-tool-summary-sep"));
        summaryNodes.push(
          el("span", elapsedBadge, "chat-tool-summary-elapsed"),
        );
      }
      const caretWrap = el("span", undefined, "chat-tool-summary-caret"),
        caretGlyph = svgIcon("chevronRight");
      caretWrap.setAttribute("aria-hidden", "true");
      if (caretGlyph) caretWrap.append(caretGlyph);
      summaryNodes.push(caretWrap);
      targetRun.summary.replaceChildren(...summaryNodes);
    }
    function syncTailRunSummary() {
      if (tailRun) syncRunSummary(tailRun, true);
    }
    function syncDraft() {
      if (!ownsEntry() || entry.sending) return;
      const current = drafts.get(sessionId) || "";
      if (input.value === current) return;
      entry.draftVersion = (entry.draftVersion || 0) + 1;
      input.value
        ? drafts.set(sessionId, input.value)
        : drafts.delete(sessionId);
    }
    input.addEventListener("input", () => {
      entry.draftVersion = (entry.draftVersion || 0) + 1;
      input.value
        ? drafts.set(sessionId, input.value)
        : drafts.delete(sessionId);
      sizeInput();
      suggest();
    });
    input.addEventListener("change", syncDraft);
    input.addEventListener("blur", syncDraft);
    input.addEventListener("focus", () => {
      if (
        !entry.live &&
        (following ||
          (feed.scrollHeight || 0) -
            (feed.scrollTop || 0) -
            (feed.clientHeight || 0) <
            120)
      ) {
        following = true;
        pinLatestSoon();
      }
    });
    input.addEventListener("compositionstart", () => {
      composing = true;
      closeSuggestions();
    });
    input.addEventListener("compositionend", () => {
      composing = false;
      suggest();
    });
    const attachment = el("div", undefined, "chat-actions");
    attachment.setAttribute("role", "status");
    const photoPicker = el("input");
    photoPicker.type = "file";
    photoPicker.accept = "image/*";
    photoPicker.hidden = true;
    photoPicker.addEventListener("change", () => {
      const f = photoPicker.files?.[0];
      photoPicker.value = "";
      if (f) preparePhoto(f);
    });
    const attachBtn = button(
      t("Photo"),
      () => {
        if (entry.photo || entry.sending) return;
        photoPicker.value = "";
        if (M.updates?.pickFile) M.updates.pickFile(photoPicker);
        else photoPicker.click?.();
      },
      "icon-button composer-icon",
      "plus",
    );
    const sendBtn = el("button", undefined, "primary composer-send");
    sendBtn.dataset.mode = "send";
    sendBtn.type = "submit";
    sendBtn.setAttribute("aria-label", t("Send"));
    if (ctx.icon) sendBtn.append(ctx.icon("arrowUp"));
    else sendBtn.textContent = t("Send");
    const modelBtn = button(t("Model"), () => chooseModel(), "composer-pill"),
      modelName = el("span", undefined, "composer-model-name"),
      modelEffort = el("span", undefined, "composer-model-effort"),
      effortBars = Array.from({ length: 5 }, () => el("span")),
      contextMeter = el("span", undefined, "composer-context");
    modelEffort.append(...effortBars);
    modelBtn.replaceChildren(modelName, modelEffort);
    contextMeter.setAttribute("role", "img");
    const suggestions = el("div", undefined, "chat-composer-suggestions"),
      choiceList = el("div", undefined, "chat-composer-options");
    choiceList.id = `chat-composer-options-${sessionId}`;
    suggestions.hidden = true;
    choiceList.setAttribute("role", "listbox");
    choiceList.setAttribute("aria-label", t("Commands, skills and files"));
    const box = el("div", undefined, "chat-composer-box");
    box.append(suggestions, input, sendBtn);
    statusRow.append(attachBtn, modelBtn, contextMeter, statusLine);
    composer.append(attachment, box, statusRow, photoPicker);
    // Like Orca, the send tap takes focus so the IME commits the last syllable first.
    for (const row of [attachment, statusRow])
      globalThis.MkKeyboard?.keepFocus(row, ctx.signal);
    // Read-only discovery is scoped to this mount, host cache and current provider/workspace.
    function chatAgent() {
      return (
        M.sessionAgent?.({
          ...state.sessions?.get(sessionId),
          ...(entry.agent ? { agent: entry.agent } : {}),
        }) ||
        entry.agent ||
        ""
      );
    }
    function ownsEntry() {
      return (
        state.chatCache === cacheMap &&
        state.drafts === drafts &&
        cacheMap.get(sessionId) === entry &&
        state.sessions?.has(sessionId)
      );
    }
    function active() {
      if (disposed || ctx.signal?.aborted || !ownsEntry()) return false;
      syncReadScope();
      return true;
    }
    // Snapshot identity changes clear private paint immediately; transient read failures do not.
    let pollQueued = false,
      appliedTimer = null;
    function syncReadScope() {
      const sess = state.sessions.get(sessionId),
        key = JSON.stringify([
          ctx.sessionKey?.(sessionId) ?? sessionId,
          sess.agent,
          sess.cwd,
          sess.groupKey,
          sess.provider,
          sess.account_id,
          sess.profile_id,
          sess.command_context,
        ]);
      if (entry.readScope === key) return;
      const changed = entry.readScope !== undefined;
      entry.readScope = key;
      if (!changed) return;
      earlierEpoch++;
      earlierQueued = false;
      failedEarlier = false;
      autoBlocked = null;
      resetHistory = false;
      refreshLatest = true;
      entry.latestRecovery = false;
      clearTranscript();
      entry.offset = 0;
      entry.generation = "";
      entry.initialized = false;
      entry.loaded = false;
      entry.progress = null;
      lastSuccess = entry.lastSuccess = 0;
      failures = 0;
      pollStatus.hidden = true;
      following = true;
      renderItems();
    }
    function requestLatest() {
      refreshLatest = true;
      resetHistory = true;
      entry.latestRecovery = true;
      latestBtn.hidden = false;
      history.hidden = false;
    }
    function scope() {
      const sess = state.sessions?.get(sessionId),
        key = JSON.stringify([
          chatAgent(),
          sess?.agent || "",
          sess?.foreground || "",
          sess?.cwd || "",
          sess?.groupKey || "",
          sess?.provider || "",
          sess?.account_id || "",
          sess?.profile_id || "",
          sess?.command_context ?? "",
        ]);
      // A scope epoch also fences A → B → A account/profile switches while a read/change is pending.
      if (entry.composerScope !== key) {
        entry.composerScope = key;
        entry.composerEpoch = (entry.composerEpoch || 0) + 1;
        entry.model = null;
        entry.composerStatus = null;
        entry.composerStatusAt = 0;
        entry.composerProbes = 0;
        statusFlight = null;
        discovery.clear();
        discovering.clear();
        closeSuggestions();
      }
      return `${entry.composerEpoch}:${key}`;
    }
    function capability(name) {
      return state.snapshot?.capabilities?.[name] === true;
    }
    const discovery = new Map(),
      discovering = new Map();
    let suggestVersion = 0,
      suggestTimer = null,
      picker = null,
      choices = [],
      choiceIndex = 0;
    function closeSuggestions() {
      suggestVersion++;
      clearTimeout(suggestTimer);
      suggestTimer = null;
      picker = null;
      choices = [];
      delete suggestions.dataset.pending;
      suggestions.hidden = true;
      input.setAttribute("aria-expanded", "false");
      input.setAttribute("aria-activedescendant", "");
    }
    function paintChoices(items, message) {
      choices = items;
      choiceIndex = 0;
      suggestions.replaceChildren();
      choiceList.replaceChildren();
      suggestions.hidden = false;
      const head = el("div", undefined, "chat-actions"),
        close = button(
          t(picker?.kind === "models" ? "Close" : "Close suggestions"),
          () => {
            closeSuggestions();
            input.focus?.();
          },
          "ghost small",
        );
      close.type = "button";
      head.append(
        el(
          "span",
          t(
            picker?.kind === "models"
              ? "Model"
              : picker?.kind === "files"
                ? "Workspace files"
                : "Commands and skills",
          ),
          "muted",
        ),
        close,
      );
      suggestions.append(head);
      input.setAttribute("aria-expanded", "true");
      if (message) suggestions.append(el("p", message, "muted"));
      let row = null,
        group;
      for (const [index, item] of items.entries()) {
        if (item.group !== group) {
          group = item.group;
          row = el("div", undefined, "chat-composer-chips");
          if (group === "efforts")
            choiceList.append(el("span", t("Effort"), "muted chat-composer-section"));
          choiceList.append(row);
        }
        const node = button(
          "",
          () => selectChoice(index),
          group ? "ghost chat-composer-chip" : "ghost chat-composer-option",
        );
        if (item.current) node.setAttribute("aria-current", "true");
        node.type = "button";
        node.tabIndex = -1;
        node.id = `chat-option-${sessionId}-${index}`;
        node.setAttribute("role", "option");
        node.append(el("span", item.label, "chat-composer-option-label"));
        if (item.detail) node.append(el("span", item.detail, "muted"));
        node.addEventListener("pointerdown", (event) => event.preventDefault());
        (group ? row : choiceList).append(node);
        item.node = node;
      }
      suggestions.append(choiceList);
      markChoice();
    }
    function markChoice() {
      for (const [i, item] of choices.entries())
        item.node.setAttribute("aria-selected", String(i === choiceIndex));
      input.setAttribute(
        "aria-activedescendant",
        choices[choiceIndex]?.node.id || "",
      );
      choices[choiceIndex]?.node.scrollIntoView?.({ block: "nearest" });
    }
    async function discover(action, query = "") {
      const context = scope(),
        key = `${context}\n${action}\n${query}`,
        cached = discovery.get(key);
      if (
        action !== "composer_models" &&
        cached &&
        Date.now() - cached.at < 30000
      )
        return cached.data;
      let request = discovering.get(key);
      if (!request) {
        request = ctx.rpc({
          action,
          session: sessionId,
          ...(action === "mention_files" ? { query } : {}),
        });
        discovering.set(key, request);
      }
      let data;
      try {
        data = await request;
      } finally {
        if (discovering.get(key) === request) discovering.delete(key);
      }
      if (active() && scope() === context && action !== "composer_models") {
        discovery.set(key, { at: Date.now(), data });
        while (discovery.size > 20)
          discovery.delete(discovery.keys().next().value);
      }
      return data;
    }
    function trigger() {
      const end =
        typeof input.selectionStart === "number"
          ? input.selectionStart
          : input.value.length;
      if (typeof input.selectionEnd === "number" && input.selectionEnd !== end)
        return null;
      const before = input.value.slice(0, end),
        slash = before.match(/^\/([^\s]*)$/),
        mention = before.match(/(?:^|\s)@([^\s@]*)$/);
      if (slash) return { kind: "commands", query: slash[1], start: 0, end };
      if (mention)
        return {
          kind: "files",
          query: mention[1],
          start: end - mention[1].length - 1,
          end,
        };
      return null;
    }
    function suggest() {
      closeSuggestions();
      if (!active() || composing || !visible) return;
      const token = trigger();
      if (!token) return;
      const action =
        token.kind === "commands" ? "command_catalog" : "mention_files";
      if (!capability(action)) return;
      const context = scope(),
        version = suggestVersion,
        draft = input.value;
      picker = { ...token, scope: context };
      paintChoices([], t("Searching…"));
      const load = async () => {
        try {
          if (
            !active() ||
            version !== suggestVersion ||
            context !== scope() ||
            draft !== input.value
          )
            return;
          const data = await discover(
            action,
            token.kind === "files" ? token.query : "",
          );
          if (
            !active() ||
            version !== suggestVersion ||
            context !== scope() ||
            draft !== input.value
          )
            return;
          let items;
          if (token.kind === "commands") {
            const query = token.query.toLocaleLowerCase();
            items = (data?.items || [])
              .filter(
                (item) =>
                  typeof item.text === "string" &&
                  !/[\r\n\x00-\x1f\x7f]/.test(item.text) &&
                  `${item.name} ${item.text} ${item.description}`
                    .toLocaleLowerCase()
                    .includes(query),
              )
              .slice(0, 80)
              .map((item) => ({
                label: item.text,
                detail: [t(item.kind || "Command"), item.description]
                  .filter(Boolean)
                  .join(" · "),
                text: item.text,
              }));
          } else
            items = (data?.items || [])
              .filter(
                (item) =>
                  typeof item.path === "string" &&
                  item.path &&
                  !/^(?:[\/\\]|[a-z]:)/i.test(item.path) &&
                  !item.path.split(/[\/\\]/).includes("..") &&
                  !/[\x00-\x1f\x7f]/.test(item.path),
              )
              .slice(0, 80)
              .map((item) => ({
                label: item.path,
                detail: t("Insert file path as text"),
                text: JSON.stringify(item.path),
              }));
          paintChoices(
            items,
            !items.length
              ? t("No matches")
              : data.truncated
                ? t("More results available. Keep typing.")
                : undefined,
          );
        } catch (error) {
          if (active() && version === suggestVersion && context === scope())
            paintChoices([], t("Search unavailable. Try again."));
        }
      };
      if (token.kind === "files") suggestTimer = setTimeout(load, 150);
      else load();
    }
    function selectChoice(index) {
      const choice = choices[index],
        token = picker;
      if (!choice || !token || !active() || composing) return;
      if (token.scope !== scope()) {
        closeSuggestions();
        return;
      }
      if (token.kind === "models") {
        if (choice.group === "efforts") {
          applyModel(null, choice.id); return;
        }
        const effort = entry.composerStatus?.effort;
        applyModel(choice, choice.efforts?.length === 1 ? choice.efforts[0] : choice.efforts?.includes(effort) ? effort : undefined); return;
      }
      const current = trigger();
      if (
        !current ||
        current.start !== token.start ||
        current.end !== token.end ||
        current.kind !== token.kind ||
        current.query !== token.query
      ) {
        closeSuggestions();
        return;
      }
      if (choice.text === "/model" && capability("composer_models")) {
        openModelCommand(token.start, token.end);
        return;
      }
      input.selectionStart = token.start;
      input.selectionEnd = token.end;
      insertText(choice.text + " ");
    }
    input.setAttribute("aria-controls", choiceList.id);
    input.setAttribute("aria-haspopup", "listbox");
    input.setAttribute("aria-autocomplete", "list");
    input.setAttribute("aria-expanded", "false");
    input.addEventListener("click", () => {
      if (picker?.kind !== "models") suggest();
    });
    input.addEventListener("keydown", (event) => {
      if (
        composing ||
        event.isComposing ||
        event.keyCode === 229 ||
        suggestions.hidden
      )
        return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closeSuggestions();
      } else if (
        (event.key === "ArrowDown" || event.key === "ArrowUp") &&
        choices.length
      ) {
        event.preventDefault();
        event.stopPropagation();
        choiceIndex =
          (choiceIndex + (event.key === "ArrowDown" ? 1 : choices.length - 1)) %
          choices.length;
        markChoice();
      } else if (event.key === "Enter") {
        event.preventDefault();
        event.stopPropagation();
        if (choices.length) selectChoice(choiceIndex);
      }
    });
    function openModelCommand(start, end) {
      input.value = input.value.slice(0, start) + input.value.slice(end);
      input.selectionStart = input.selectionEnd = start;
      entry.draftVersion = (entry.draftVersion || 0) + 1;
      input.value
        ? drafts.set(sessionId, input.value)
        : drafts.delete(sessionId);
      sizeInput();
      chooseModel();
    }
    async function chooseModel() {
      if (!active() || entry.modelPending) return;
      if (picker?.kind === "models") {
        closeSuggestions();
        return;
      }
      closeSuggestions();
      if (!capability("composer_models")) return;
      const context = scope(),
        version = suggestVersion,
        warm =
          entry.modelChoices?.scope === context ? entry.modelChoices.items : [];
      entry.model = null;
      syncAgent();
      picker = { kind: "models", scope: context };
      paintChoices(warm, warm.length ? undefined : t("Loading…"));
      try {
        const [, data] = await Promise.all([
          refreshStatus(true).catch(() => {}),
          discover("composer_models"),
        ]);
        if (!active() || version !== suggestVersion || scope() !== context)
          return;
        if (
          data?.supported &&
          capability("chat_composer") &&
          !entry.composerStatus?.effort_supported &&
          !entry.composerStatus?.models?.length
        ) {
          await refreshStatus(true).catch(() => {});
          if (!active() || version !== suggestVersion || scope() !== context)
            return;
        }
        const models = data?.supported
          ? (data.items || data.models || []).filter(
              (item) => typeof item.id === "string" && item.id,
            )
          : [];
        const status = entry.composerStatus,
          agent = chatAgent(),
          current = models.find((item) => item.id === data.current);
        entry.model = current
          ? {
              agent,
              scope: context,
              id: current.id,
              label: current.label || current.name || current.id,
              effort: status?.effort || null,
              statusBefore: status?.model,
              statusEffortBefore: status?.effort,
            }
          : null;
        syncAgent();
        const effortSupported = Boolean(
            status?.effort_supported ||
              (data?.supported &&
                models.some(
                  (m) => Array.isArray(m.efforts) && m.efforts.length,
                )),
          ),
          activeModelId = data?.current || status?.model;
        const items = models.map((item) => {
          const isCurrent =
              item.id === data.current ||
              matchesModel(agent, item.id, status?.model),
            label = t(item.label || item.name || item.id);
          return {
            id: item.id,
            group: "models",
            current: isCurrent,
            label:
              isCurrent && agent === "claude" && status?.model
                ? fullModel(status.model)
                : label,
            efforts: effortSupported
              ? (Array.isArray(item.efforts)
                  ? item.efforts
                  : status?.models?.find((model) => model.id === item.id)
                      ?.efforts) || []
              : [],
          };
        });
        const efforts = sortEfforts(
          models.find((model) => model.id === activeModelId)?.efforts ||
            status?.models?.find((model) => model.id === activeModelId)
              ?.efforts ||
            status?.efforts ||
            [],
        );
        if (effortSupported && efforts.length > 1)
          for (const effort of efforts)
            items.push({
              id: effort,
              group: "efforts",
              current: effort === status?.effort,
              label: t(EFFORT_LABELS.get(effort) || effort),
            });
        entry.modelChoices = { scope: context, items };
        paintChoices(
          items,
          models.length
            ? data?.note
              ? t(data.note)
              : undefined
            : t(
                data?.reason ||
                  "Model selection is unavailable for this session.",
              ),
        );
      } catch (error) {
        if (active() && version === suggestVersion && scope() === context)
          paintChoices(
            [],
            t("Model selection is unavailable for this session."),
          );
      }
    }
    async function applyModel(choice, effort) {
      if (
        entry.modelPending ||
        entry.sending ||
        !active() ||
        state.connection?.phase !== "ready"
      )
        return;
      const context = scope(),
        version = suggestVersion,
        operation = { scope: context };
      entry.modelPending = operation;
      if (picker?.kind === "models" && !suggestions.hidden) {
        suggestions.dataset.pending = "true";
        const chosen =
          choice ||
          choices.find((item) => item.group === "efforts" && item.id === effort);
        if (chosen?.node) chosen.node.dataset.pending = "true";
        for (const item of choices) if (item.node) item.node.disabled = true;
        for (const child of Array.from(suggestions.children || []))
          if (child.tagName === "P" || child.tag === "p") child.remove();
        suggestions.insertBefore(
          el("p", t("Applying…"), "muted chat-picker-status"),
          choiceList,
        );
      }
      updateComposer();
      try {
        const result = await ctx.rpc({
          action: choice ? "set_model" : "set_effort",
          session: sessionId,
          ...(choice ? {model:choice.id} : {}),
          ...(effort ? {effort} : {}),
        });
        if (
          !ownsEntry() ||
          entry.modelPending !== operation ||
          scope() !== context
        )
          return;
        discovery.clear();
        if (result?.applied === true) {
          const id =
              typeof result.model === "string" ? result.model : choice?.id,
            nextEffort =
              result.effort ||
              effort ||
              entry.model?.effort ||
              entry.composerStatus?.effort ||
              null;
          if (id)
            entry.model = {
              agent: chatAgent(),
              scope: context,
              id,
              label: result.label || (id === choice?.id ? choice.label : id),
              effort: nextEffort,
              statusBefore: entry.composerStatus?.model,
              statusEffortBefore: entry.composerStatus?.effort,
            };
          else if (result.effort && entry.model?.scope === context)
            entry.model.effort = result.effort;
          entry.composerStatusAt = 0;
          entry.composerProbes = 0;
          if (result.effort && entry.composerStatus)
            entry.composerStatus.effort = result.effort;
          clearTimeout(appliedTimer);
          modelBtn.dataset.applied = "true";
          appliedTimer = setTimeout(() => {
            delete modelBtn.dataset.applied;
          }, 1500);
          if (active() && version === suggestVersion) closeSuggestions();
        } else if (active() && version === suggestVersion)
          paintChoices(
            [],
            t(result?.reason || "Model change was not confirmed."),
          );
      } catch (error) {
        if (
          active() &&
          entry.modelPending === operation &&
          scope() === context &&
          version === suggestVersion
        )
          paintChoices([], t("Model change was not confirmed."));
      } finally {
        delete suggestions.dataset.pending;
        if (entry.modelPending === operation) {
          entry.modelPending = null;
          if (ownsEntry()) entry.notifyComposer?.();
        }
      }
    }
    let statusFlight = null;
    async function refreshStatus(force = false) {
      if (!active() || !capability("chat_composer") || state.cached || state.connection?.phase !== "ready") return;
      const binding = scope(), generation = entry.generation, owner = ctx.transport, linkEpoch = state.connection?.generation;
      if (statusFlight?.binding === binding) return statusFlight.promise;
      const hasLiveMeta = Boolean(entry.composerStatus?.model || entry.composerStatus?.supported),
        ttl = hasLiveMeta || (entry.composerProbes || 0) >= 10 ? 15000 : 1500;
      if (!force && (!isVisible() || Date.now() - (entry.composerStatusAt || 0) < ttl)) return;
      const flight = {binding};
      const current = () => statusFlight === flight && active() && scope() === binding && entry.generation === generation && owner === ctx.transport && linkEpoch === state.connection?.generation;
      flight.promise = Promise.resolve().then(async () => {
        try {
          const data = await ctx.rpc({action:"chat_composer", session:sessionId});
          if (!current() || data?.agent !== chatAgent() || data.generation && data.generation !== generation) return;
          if (entry.model && data.model && data.model !== entry.model.statusBefore && !matchesModel(chatAgent(), entry.model.id, data.model)) entry.model = null;
          else if (entry.model && data.effort && data.effort !== entry.model.statusEffortBefore) entry.model.effort = data.effort;
          entry.composerProbes = data?.model || data?.supported ? 0 : (entry.composerProbes || 0) + 1;
          entry.composerStatus = data; entry.composerStatusAt = Date.now(); syncAgent();
        } catch (error) {
          if (current()) {entry.composerStatus = null; entry.composerStatusAt = Date.now(); syncAgent(); if (force) throw error;}
        } finally {if (statusFlight === flight) statusFlight = null; if (active() && scope() === binding && entry.generation !== generation) refreshStatus();}
      });
      statusFlight = flight; return flight.promise;
    }
    function matchesModel(agent, selected, observed) {
      return selected === observed || agent === "claude" && ["opus","sonnet","haiku","fable"].includes(selected) && String(observed || "").startsWith(`claude-${selected}-`);
    }
    function syncAgent() {
      const context = scope(),
        agent = chatAgent(),
        status = entry.composerStatus?.agent === agent ? entry.composerStatus : null,
        currentEntryModel = entry.model?.agent === agent && entry.model.scope === context ? entry.model : null,
        currentModel = currentEntryModel?.label || null,
        observedModel = currentModel || status?.model_label || status?.model,
        observedEffort = currentEntryModel?.effort || status?.effort,
        hasStatus = capability("chat_composer"),
        modelText = observedModel || t(hasStatus ? "Model unknown" : "Model"),
        label = hasStatus ? `${modelText} · ${observedEffort || t("Effort unknown")}` : modelText,
        filledBars = EFFORT_BARS.get(observedEffort);
      shell.dataset.composerStatus = String(hasStatus);
      if ((modelBtn.getAttribute ? modelBtn.getAttribute("aria-label") : modelBtn.attrs?.["aria-label"]) !== label) modelBtn.setAttribute("aria-label", label);
      modelName.textContent = observedModel ? shortModel(observedModel) : modelText;
      modelEffort.hidden = filledBars === undefined;
      effortBars.forEach((bar, index) => {
        bar.dataset.on = String(index < filledBars);
      });
      modelBtn.title = label;
      const percent = !entry.model || matchesModel(agent, entry.model.id, status?.model) ? status?.context?.percent : undefined, known = typeof percent === "number" && Number.isFinite(percent);
      contextMeter.hidden = !known;
      if (known) {
        const usage = Math.round(Math.max(0,Math.min(100,percent))), description = t("Context used {0}%", usage);
        contextMeter.style.background = `conic-gradient(var(--foreground) ${usage}%, var(--border) 0)`;
        contextMeter.title = description; contextMeter.setAttribute("aria-label",description);
      }
      const controllable = ["claude", "codex", "agy"].includes(agent);
      modelBtn.hidden = !agent || !capability("composer_models") || (!controllable && !!status && !status.supported && !status.model && !status.model_label && !currentModel);
      modelBtn.disabled =
        !!entry.modelPending ||
        entry.sending ||
        state.connection?.phase !== "ready";
      const text = liveQuestion ? t("Answer the current question above") : agent
        ? t("Message {0}", agentName(agent))
        : t("Message agent (Bracketed Paste)…");
      if (input.placeholder !== text) input.placeholder = text;
    }
    function copyText(text, node) {
      run(async () => {
        await writeClipboardText(el, t, text);
        if (!node) return;
        if (ctx.icon && String(node.className || "").includes("chat-copy")) {
          node.replaceChildren(ctx.icon("check"));
          node.setAttribute("aria-label", t("Copied"));
          node.dataset.copied = "true";
          setTimeout(() => {
            node.replaceChildren(ctx.icon("copy"));
            node.setAttribute("aria-label", t("Copy"));
            delete node.dataset.copied;
          }, CODE_COPY_FEEDBACK_MS);
        } else {
          const prev = node.textContent;
          node.textContent = t("Copied");
          node.dataset.copied = "true";
          setTimeout(() => {
            node.textContent = prev;
            delete node.dataset.copied;
          }, CODE_COPY_FEEDBACK_MS);
        }
      });
    }
    function setCodeCopyState(btn, copied) {
      if (!btn) return;
      const label = t(copied ? "Copied" : "Copy");
      btn.setAttribute("aria-label", label);
      if (copied) {
        btn.dataset.copied = "true";
        const nodes = [],
          checkIcon = ctx.icon ? ctx.icon("check") : svgIcon("check");
        if (checkIcon) nodes.push(checkIcon);
        nodes.push(el("span", label, "chat-code-copy-label"));
        btn.replaceChildren(...nodes);
      } else {
        delete btn.dataset.copied;
        btn.replaceChildren();
        btn.textContent = label;
      }
    }
    function copyCodeBlock(target, code) {
      const id = `${target.key}\0${target.blockIndex}`;
      run(async () => {
        await writeClipboardText(el, t, code);
        if (disposed) return;
        const prev = copiedCode.get(id);
        if (prev) clearTimeout(prev.timer);
        const liveBtn = () =>
          codeButtonsByCard.get(target.key)?.[target.blockIndex] || target.btn;
        setCodeCopyState(liveBtn(), true);
        const timer = setTimeout(() => {
          copiedCode.delete(id);
          if (!disposed) setCodeCopyState(liveBtn(), false);
        }, CODE_COPY_FEEDBACK_MS);
        copiedCode.set(id, { timer });
      });
    }
    function setCodeWrapState(btn, pre, wrapped) {
      const label = t(wrapped ? "Disable line wrap" : "Wrap lines");
      btn.setAttribute("aria-pressed", String(wrapped));
      btn.setAttribute("aria-label", label);
      btn.title = label;
      if (pre) {
        if (wrapped) pre.dataset.wrapped = "true";
        else delete pre.dataset.wrapped;
      }
    }
    function richOptsFor(key) {
      return {
        t,
        renderCodeWrap: (block, pre) => {
          const wrapKey = `code-wrap:${key}:${block.blockIndex}`,
            wrapBtn = button("", () => {
              const next = !entry.disclosures?.get(wrapKey);
              entry.disclosures ||= new Map();
              if (next) entry.disclosures.set(wrapKey, true);
              else entry.disclosures.delete(wrapKey);
              setCodeWrapState(wrapBtn, pre, next);
            }, "chat-code-wrap-btn");
          wrapBtn.type = "button";
          wrapBtn.setAttribute("type", "button");
          const glyph = svgIcon("textWrap");
          if (glyph) wrapBtn.append(glyph);
          setCodeWrapState(
            wrapBtn,
            pre,
            Boolean(entry.disclosures?.get(wrapKey)),
          );
          return wrapBtn;
        },
        renderCodeCopy: (block) => {
          const copyBtn = button(
            t("Copy"),
            () =>
              copyCodeBlock(
                { key, blockIndex: block.blockIndex, btn: copyBtn },
                block.code,
              ),
            "chat-code-copy",
          );
          copyBtn.type = "button";
          copyBtn.setAttribute("type", "button");
          let list = codeButtonsByCard.get(key);
          if (!list) {
            list = [];
            codeButtonsByCard.set(key, list);
          }
          list[block.blockIndex] = copyBtn;
          setCodeCopyState(copyBtn, copiedCode.has(`${key}\0${block.blockIndex}`));
          return copyBtn;
        },
      };
    }
    // 오늘은 시각만, 다른 날은 날짜와 시각을 보인다(Orca 메시지 아래 표기).
    function formatTime(at) {
      if (!at) return "";
      const date = new Date(/^\d+$/.test(at) ? Number(at) : at);
      if (Number.isNaN(date.getTime())) return "";
      const lang = M.i18n?.language,
        today = new Date().toDateString() === date.toDateString();
      return date.toLocaleString(
        lang,
        today
          ? { hour: "numeric", minute: "2-digit" }
          : {
              month: "numeric",
              day: "numeric",
              hour: "numeric",
              minute: "2-digit",
            },
      );
    }
    function messageMeta(item) {
      const meta = el("footer", undefined, "chat-meta"),
        copy = button(
          t("Copy"),
          () => copyText(item.text, copy),
          "icon-button chat-copy",
          "copy",
        );
      meta.append(copy);
      const when = formatTime(item.at);
      if (when) {
        const time = el("time", when, "chat-time");
        time.setAttribute(
          "datetime",
          new Date(
            /^\d+$/.test(item.at) ? Number(item.at) : item.at,
          ).toISOString(),
        );
        meta.append(time);
      }
      return meta;
    }
    // act owns product-level error presentation; local operations retain their own visible status.
    function run(fn) {
      if (ctx.act) ctx.act(fn);
      else
        Promise.resolve()
          .then(fn)
          .catch((e) => ctx.error?.(Error(cleanRpcError(e, t))));
    }
    function syncStopLatch() {
      const sess = state.sessions?.get(sessionId);
      if (
        entry.stopping &&
        (sess?.state !== "Working" ||
          (entry.stopStamp &&
            sess?.state_since_unix_ns &&
            sess.state_since_unix_ns !== entry.stopStamp))
      ) {
        entry.stopping = false;
        entry.stopStamp = null;
      }
    }
    function isStopMode() {
      syncStopLatch();
      return (
        capability("chat_interrupt") &&
        state.sessions?.get(sessionId)?.state === "Working" &&
        !entry.sending
      );
    }
    const stopDisabled = () =>
      !!entry.stopping ||
      !!entry.modelPending ||
      !!liveQuestion ||
      state.connection?.phase !== "ready";
    function updateComposer(sent) {
      updateStatus();
      let cleared = false;
      // Completion belongs to the shared draft version, including a newly mounted composer.
      if (
        sent &&
        (entry.draftVersion || 0) === sent.version &&
        input.value === sent.text
      ) {
        input.value = "";
        cleared = true;
      }
      if (liveQuestion) syncDraft();
      sizeInput();
      syncOptimistic();
      if (cleared) pinLatestSoon();
      input.disabled = !!liveQuestion;
      modelBtn.disabled =
        !!liveQuestion ||
        !!entry.modelPending ||
        entry.sending ||
        state.connection?.phase !== "ready";
      const stopMode = isStopMode(),
        wantMode = stopMode ? "stop" : "send";
      if (sendBtn.dataset.mode !== wantMode) {
        sendBtn.dataset.mode = wantMode;
        sendBtn.type = stopMode ? "button" : "submit";
        const label = t(stopMode ? "Stop" : "Send");
        sendBtn.setAttribute("aria-label", label);
        const glyph = ctx.icon
          ? stopMode
            ? svgIcon("stop")
            : ctx.icon("arrowUp")
          : null;
        if (glyph) sendBtn.replaceChildren(glyph);
        else sendBtn.textContent = label;
      }
      sendBtn.disabled = stopMode
        ? stopDisabled()
        : !!entry.modelPending ||
          !!liveQuestion ||
          entry.sending ||
          entry.photo?.status === "pending" ||
          entry.photo?.status === "error";
      attachBtn.disabled = !!liveQuestion || entry.sending || !!entry.photo;
      attachment.replaceChildren();
      const photo = entry.photo;
      if (!photo) return;
      if (photo.status === "ready" && photo.prepared?.data) {
        const img = el("img", undefined, "chat-photo-thumb"),
          src = `data:${photo.prepared.mime || "image/jpeg"};base64,${photo.prepared.data}`;
        img.alt = "";
        img.src = src;
        img.setAttribute("alt", "");
        img.setAttribute("src", src);
        attachment.append(img);
      }
      const text =
        photo.status === "pending"
          ? t("Preparing photo…")
          : photo.status === "error"
            ? t("Photo preparation failed. Retry or remove it.")
            : t("Photo ready. It will be sent with this message.");
      attachment.append(
        el(
          "span",
          `${photo.name}: ${text}`,
          photo.status === "error" ? "danger" : "muted",
        ),
      );
      if (photo.status === "error")
        attachment.append(
          button(
            t("Retry photo"),
            () => preparePhoto(photo.file),
            "ghost small",
          ),
        );
      attachment.append(
        button(
          t("Remove photo"),
          () => {
            if (entry.sending) return;
            entry.photoVersion++;
            entry.photo = null;
            updateComposer();
          },
          "ghost small",
        ),
      );
    }
    async function preparePhoto(file) {
      const version = ++entry.photoVersion;
      entry.photo = {
        file,
        name: file.name || t("Photo"),
        status: "pending",
        version,
      };
      updateComposer();
      try {
        const prepared = await ctx.photo?.(sessionId, file, { prepare: true });
        if (entry.photoVersion !== version) return;
        if (!prepared?.mime || !prepared?.data)
          throw new Error(t("Photo preparation unavailable"));
        entry.photo = { ...entry.photo, status: "ready", prepared };
      } catch (error) {
        if (entry.photoVersion === version)
          entry.photo = {
            ...entry.photo,
            status: "error",
            error: cleanRpcError(error, t),
          };
      }
      entry.notifyComposer?.();
    }
    sendBtn.addEventListener("click", (e) => {
      if (!isStopMode()) return;
      e.preventDefault();
      if (e.detail > 1 || !active() || sendBtn.disabled || stopDisabled())
        return;
      entry.stopping = true;
      entry.stopStamp =
        state.sessions?.get(sessionId)?.state_since_unix_ns || null;
      updateComposer();
      run(async () => {
        const res = await ctx.rpc({ action: "interrupt", session: sessionId });
        if (res?.interrupted === false && ownsEntry()) {
          await ctx.refresh?.();
        } else if (!disposed) {
          await pollNow();
          await ctx.refresh?.();
        }
      });
    });
    composer.addEventListener("submit", (e) => {
      e.preventDefault();
      if (
        !active() ||
        isStopMode() ||
        liveQuestion ||
        entry.modelPending ||
        !suggestions.hidden ||
        entry.sending ||
        entry.photo?.status === "pending" ||
        entry.photo?.status === "error"
      )
        return;
      const text = input.value,
        photo = entry.photo,
        version = entry.draftVersion || 0;
      if (text.trim() === "/model" && !photo && capability("composer_models")) {
        openModelCommand(0, text.length);
        return;
      }
      if (!text.trim() && !photo) return;
      const hadLive = !!entry.live;
      followLatest();
      entry.progress = {
        phase: "sending",
        text,
        photo: !!photo,
        at: Date.now(),
        canConfirm: entry.loaded && entry.bound,
        generation: entry.generation,
        lastState: state.sessions?.get(sessionId)?.state,
        stamp: state.sessions?.get(sessionId)?.state_since_unix_ns,
        ids: new Set(entry.items.map((item) => item.id)),
      };
      entry.sending = true;
      closeSuggestions();
      if (hadLive) renderItems();
      updateComposer();
      pinLatestSoon();
      run(async () => {
        try {
          if (!active()) return;
          await ctx.rpc({
            action: "prompt",
            session: sessionId,
            text,
            ...(photo ? { photo: photo.prepared } : {}),
          });
          entry.progress.phase = "accepted";
          entry.notifyComposer?.();
          // A later mount or edit owns the current draft; an older RPC must never erase it.
          if (
            (entry.draftVersion || 0) === version &&
            drafts.get(sessionId) === text
          ) {
            drafts.delete(sessionId);
            entry.notifyComposer?.({ version, text });
          }
          if (photo && entry.photo === photo) entry.photo = null;
          if (!disposed) {
            await pollNow();
            await ctx.refresh?.();
          }
        } catch (error) {
          const message = cleanRpcError(error, t);
          if (entry.progress.phase === "sending") {
            entry.progress.phase = "error";
            entry.progress.error = message;
          }
          throw Error(message);
        } finally {
          entry.sending = false;
          entry.notifyComposer?.();
        }
      });
    });
    const liveCard = el("section", undefined, "chat-live-question"),
      liveCopy = el("div", undefined, "live-question-copy"),
      liveTitle = el("strong"),
      liveOpen = button(
        t("Answer question"),
        () => openQuestion(),
        "secondary live-question-open",
      );
    liveCard.hidden = true;
    liveCard.setAttribute("aria-label", t("Current question"));
    liveCopy.append(
      el("small", t("Current question"), "live-question-label"),
      liveTitle,
    );
    liveCard.append(liveCopy, liveOpen);
    shell.append(history, pollStatus, feed, liveCard, composer);
    root.replaceChildren(shell);
    // Only a complete live terminal screen supplies an answer target. Historical cards stay inert.
    let liveQuestion = null,
      activeQuestionKey = null,
      replyPanel = null;
    const isWriteInLabel = (label) =>
      /^(?:other(?:\b|[ (])|type something|write[- ]?in|직접 입력|기타)/i.test(
        label || "",
      );
    const compactQuestionText = (value) =>
      String(value || "").replace(/[\s`]+/g, "");
    function matchesLiveQuestion(item, question) {
      if (
        !item ||
        item.kind !== "question" ||
        !question ||
        Boolean(item.multi_select) !== Boolean(question.multiple) ||
        !Array.isArray(item.options) ||
        item.options.length < 2 ||
        !Array.isArray(question.choices) ||
        question.choices.length < item.options.length
      )
        return false;
      return item.options.every((opt, index) => {
        const choice = compactQuestionText(question.choices[index]?.label),
          label = compactQuestionText(opt?.label),
          withDesc = opt?.description
            ? compactQuestionText(`${opt.label} ${opt.description}`)
            : label;
        return Boolean(label) && (choice === label || choice === withDesc);
      });
    }
    function computeActiveQuestionKey() {
      if (!liveQuestion || !entry.items.length) return null;
      for (let idx = entry.items.length - 1; idx >= 0; idx--) {
        const item = entry.items[idx];
        if (item.kind === "question") {
          if (matchesLiveQuestion(item, liveQuestion))
            return itemKey(item, idx);
          continue;
        }
        if (item.kind === "reasoning") continue;
        break;
      }
      return null;
    }
    function syncLiveCardVisibility() {
      if (!liveCard) return;
      liveCard.hidden =
        !liveQuestion || Boolean(activeQuestionKey && following && !entry.live);
    }
    function syncQuestionCards() {
      activeQuestionKey = computeActiveQuestionKey();
      for (const card of cards.values()) card._syncQuestion?.();
    }
    const draftFor = (question) => {
      if (entry.questionDraft?.identity !== question.identity)
        entry.questionDraft = {
          identity: question.identity,
          selected: question.multiple
            ? question.choices.flatMap((choice, i) =>
                choice.checked ? [i] : [],
              )
            : [],
        };
      return entry.questionDraft;
    };
    const panelOpen = (panel) =>
      panel &&
      (!("isConnected" in panel.root) || panel.root.isConnected) &&
      panel.root.closest?.("dialog")?.open !== false;
    function setQuestion(question, status = {}) {
      if (disposed) return;
      if (!question && status.settled) entry.questionDraft = null;
      liveQuestion = question;
      const prevActive = activeQuestionKey;
      syncQuestionCards();
      renderLiveQuestion();
      if (activeQuestionKey && activeQuestionKey !== prevActive)
        pinLatestSoon();
      if (panelOpen(replyPanel)) renderReplyPanel(replyPanel);
    }
    function renderLiveQuestion() {
      syncLiveCardVisibility();
      updateComposer();
      syncAgent();
      if (!liveQuestion) {
        updateStatus();
        return;
      }
      const question = liveQuestion,
        draft = draftFor(question);
      liveTitle.textContent = question.title || t("Choose an answer");
      liveOpen.textContent = t(
        draft.selected.length ? "Continue answering" : "Answer question",
      );
      liveOpen.setAttribute("aria-label", liveOpen.textContent);
      liveOpen.disabled = !question.enabled || entry.questionSending;
      updateStatus();
    }
    function openQuestion() {
      const question = opts.getQuestion?.() || liveQuestion;
      if (!question || !question.enabled || disposed) return;
      liveQuestion = question;
      draftFor(question);
      ctx.sheet(t("Answer question"), [], undefined, (body) => {
        const root = el("section", undefined, "question-reply-panel"),
          list = el("div", undefined, "question-reply-options"),
          heading = el("div", undefined, "question-reply-heading"),
          title = el("h3", question.title || t("Choose an answer")),
          kind = el(
            "p",
            t(
              question.multiple
                ? "Select one or more answers"
                : "Select one answer",
            ),
            "muted",
          ),
          status = el("p", undefined, "question-reply-status"),
          footer = el("div", undefined, "question-reply-footer");
        status.setAttribute("role", "status");
        heading.append(title, kind);
        const close = button(
          t("Close"),
          () => ctx.closeSheet?.(),
          "icon-button question-reply-close",
          "close",
        );
        heading.append(close);
        const send = button(
            t("Send answer"),
            () => submitQuestion(panel),
            "primary question-reply-send",
          ),
          write = button(
            t("Write an answer in Term"),
            () => {
              ctx.closeSheet?.();
              onSwitchTerm?.();
            },
            "ghost question-reply-write",
          );
        const panel = {
          root,
          list,
          status,
          send,
          footer,
          question,
          buttons: [],
          error: "",
        };
        replyPanel = panel;
        const draft = draftFor(question);
        list.setAttribute("role", question.multiple ? "group" : "radiogroup");
        list.setAttribute(
          "aria-label",
          question.title || t("Choose an answer"),
        );
        question.choices.forEach((choice, index) => {
          const row = button(
            "",
            () => {
              if (
                !liveQuestion?.enabled ||
                entry.questionSending ||
                liveQuestion.identity !== question.identity
              )
                return;
              const selected = draft.selected;
              draft.changed = true;
              draft.selected = question.multiple
                ? selected.includes(index)
                  ? selected.filter((i) => i !== index)
                  : [...selected, index]
                : [index];
              panel.error = "";
              renderReplyPanel(panel);
              renderLiveQuestion();
            },
            "question-reply-choice",
          );
          row.setAttribute("aria-label", choice.label);
          row.setAttribute("role", question.multiple ? "checkbox" : "radio");
          const mark = el("span", undefined, "question-reply-check"),
            label = el("span", choice.label, "question-reply-label");
          mark.setAttribute("aria-hidden", "true");
          row.append(mark, label);
          panel.buttons.push({ row, mark, index });
          list.append(row);
          row.addEventListener("keydown", (event) => {
            if (
              question.multiple ||
              !["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(
                event.key,
              )
            )
              return;
            event.preventDefault();
            const offset = ["ArrowDown", "ArrowRight"].includes(event.key)
                ? 1
                : panel.buttons.length - 1,
              target = panel.buttons[(index + offset) % panel.buttons.length];
            target.row.click?.();
            target.row.focus?.();
          });
        });
        footer.append(status, send, write);
        root.append(heading, list, footer);
        body.append(root);
        renderReplyPanel(panel);
      });
    }
    function renderReplyPanel(panel) {
      const draft = entry.questionDraft,
        matching = liveQuestion?.identity === panel.question.identity,
        enabled =
          matching &&
          liveQuestion.enabled &&
          !entry.questionSending &&
          !disposed,
        selected = matching
          ? draftFor(liveQuestion).selected
          : draft?.identity === panel.question.identity
            ? draft.selected
            : [];
      for (const { row, mark, index } of panel.buttons) {
        const checked = selected.includes(index);
        row.setAttribute("aria-checked", String(checked));
        row.dataset.selected = String(checked);
        row.disabled = !enabled;
        mark.textContent = checked ? "✓" : "";
      }
      panel.send.disabled =
        !enabled || (!selected.length && !panel.question.multiple);
      panel.send.textContent = t(
        entry.questionSending ? "Sending answer…" : "Send answer",
      );
      panel.send.setAttribute("aria-label", panel.send.textContent);
      panel.status.textContent =
        panel.error ||
        (!matching
          ? t(
              "The question changed. Review the current question before sending.",
            )
          : !enabled && !entry.questionSending
            ? t(
                "Waiting for the current question. Check Term if this continues.",
              )
            : selected.length
              ? t(
                  "Selected {0}. Nothing is sent until you press Send answer.",
                  selected.length,
                )
              : t(
                  panel.question.multiple
                    ? "No options selected. Send answer to continue without selections."
                    : "Select an answer. It will not be sent immediately.",
                ));
      panel.status.dataset.error = String(!!panel.error || !matching);
    }
    async function submitQuestion(panel) {
      if (!panelOpen(panel) || entry.questionSending || disposed) return;
      const question = opts.getQuestion?.() || liveQuestion,
        draft = entry.questionDraft;
      if (
        !question?.enabled ||
        question.identity !== panel.question.identity ||
        draft?.identity !== question.identity ||
        (!draft.selected.length && !question.multiple)
      ) {
        panel.error = t(
          "The question changed. Review the current question before sending.",
        );
        renderReplyPanel(panel);
        return;
      }
      const selected = draft.selected.slice();
      entry.questionSending = true;
      panel.error = "";
      renderReplyPanel(panel);
      renderLiveQuestion();
      try {
        await opts.onAnswerQuestion(question.token, selected);
        if (disposed || ctx.signal?.aborted) return;
        if (entry.questionDraft === draft) entry.questionDraft = null;
        if (panelOpen(panel)) ctx.closeSheet?.();
        ctx.toast?.(t("Answer sent"));
        // Native write-in choices open the agent's own text prompt; do not inject unbound chat text.
        if (
          selected.some((i) =>
            /^(?:other(?:\b|[ (])|type something|write[- ]?in|직접 입력|기타)/i.test(
              question.choices[i]?.label || "",
            ),
          )
        )
          onSwitchTerm?.();
      } catch (error) {
        if (!disposed && panelOpen(panel)) {
          panel.error = cleanRpcError(error, t);
          renderReplyPanel(panel);
        }
      } finally {
        entry.questionSending = false;
        if (!disposed) {
          renderLiveQuestion();
          if (panelOpen(panel)) renderReplyPanel(panel);
        }
      }
    }
    function itemKey(item, idx) {
      return `${entry.generation || ""}:${item.id || `${item._pageStart || 0}:${idx}:${item.tool_id || ""}:${item.text}`}`;
    }
    function readerAnchor() {
      if (following || !(feed.clientHeight > 0)) return null;
      const top = feed.getBoundingClientRect?.().top,
        node = Array.from(feed.children || []).find((card) =>
          card.getBoundingClientRect
            ? card.getBoundingClientRect().bottom > top
            : (card.offsetTop || 0) + (card.clientHeight || 0) > feed.scrollTop,
        );
      return node
        ? {
            node,
            top: node.getBoundingClientRect
              ? node.getBoundingClientRect().top
              : (node.offsetTop || 0) - feed.scrollTop,
          }
        : null;
    }
    function restoreReader(anchor) {
      if (!following && anchor && anchor.node.parentElement) {
        const top = anchor.node.getBoundingClientRect
          ? anchor.node.getBoundingClientRect().top
          : (anchor.node.offsetTop || 0) - feed.scrollTop;
        feed.scrollTop += top - anchor.top;
        lastTop = feed.scrollTop;
      }
    }
    function optimisticText() {
      const p = entry.progress;
      if (
        !p ||
        p.rendered ||
        entry.live ||
        (p.phase !== "sending" && p.phase !== "accepted") ||
        (p.generation && entry.generation && p.generation !== entry.generation)
      )
        return "";
      if (p.phase !== "sending" && state.sessions?.get(sessionId)?.exited)
        return "";
      if (
        entry.items.some(
          (item) =>
            !p.ids.has(item.id) &&
            (item.kind === "user" || (!p.canConfirm && item.kind === "assistant")),
        )
      ) {
        p.rendered = true;
        return "";
      }
      return p.text || (p.photo ? t("Photo") : "");
    }
    function syncOptimistic() {
      const text = optimisticText(),
        phase = entry.progress?.phase || "";
      if (!text) {
        if (optimisticCard) {
          optimisticCard.remove();
          resize?.unobserve(optimisticCard);
          optimisticCard = null;
          optimisticShown = "";
          if (
            !entry.items.length &&
            entry.loaded &&
            !Array.from(feed.children || []).some(
              (c) => c.className === "chat-empty",
            )
          ) {
            const empty = el("div", undefined, "chat-empty");
            empty.append(el("p", t("Enter a new message"), "muted"));
            feed.append(empty);
          }
        }
        return;
      }
      for (const child of Array.from(feed.children || []))
        if (child.className === "chat-empty") child.remove();
      if (!optimisticCard || optimisticShown !== text) {
        const next = renderCard(
          { kind: "user", text, at: entry.progress?.at },
          "optimistic",
        );
        next.className = "chat-card chat-user chat-optimistic";
        next.dataset.chatOptimistic = "true";
        if (optimisticCard?.parentElement === feed) {
          feed.insertBefore(next, optimisticCard);
          optimisticCard.remove();
          resize?.unobserve(optimisticCard);
        } else feed.append(next);
        resize?.observe(next);
        optimisticCard = next;
        optimisticShown = text;
      } else if (
        optimisticCard.parentElement !== feed ||
        feed.children?.[feed.children.length - 1] !== optimisticCard
      ) {
        feed.append(optimisticCard);
      }
      optimisticCard.dataset.phase = phase;
      const meta = optimisticCard.children?.[1];
      if (meta) {
        const metaChildren = Array.from(meta.children || []),
          timeEl = metaChildren.find((n) => n.className === "chat-time");
        let statusEl = metaChildren.find(
          (n) => n.className === "chat-optimistic-status",
        );
        if (phase === "sending") {
          if (timeEl) timeEl.hidden = true;
          if (!statusEl) {
            statusEl = el("span", t("Sending…"), "chat-optimistic-status");
            meta.append(statusEl);
          }
          statusEl.hidden = false;
        } else {
          if (statusEl) statusEl.remove();
          if (timeEl) timeEl.hidden = false;
        }
      }
    }
    function renderItems(appendFrom) {
      const anchor = readerAnchor(),
        foldable = computeFoldableNotes(entry.items);
      let prevPromotedToRun = false;
      if (appendFrom > 0) {
        if (
          !tailRun &&
          entry.items[appendFrom - 1]?.kind === "reasoning" &&
          (isWorkItemKind(entry.items[appendFrom]?.kind) ||
            foldable[appendFrom])
        )
          prevPromotedToRun = true;
        for (let i = appendFrom - 1; !prevPromotedToRun && i >= 0; i--) {
          const prevKind = entry.items[i]?.kind || "assistant";
          if (isTurnBoundaryKind(prevKind)) break;
          if (foldable[i] && cards.has(itemKey(entry.items[i], i))) {
            prevPromotedToRun = true;
            break;
          }
        }
      }
      const incremental =
        appendFrom > 0 &&
        renderedItems === entry.items &&
        renderedCount === appendFrom &&
        !prevPromotedToRun;
      const wanted = new Set(),
        wantedNotes = new Set(),
        wantedRuns = new Set(),
        nodes = [];
      let run = incremental ? tailRun : null,
        runEnd = 0;
      // Retain the actual question/details nodes so selection, write-in, focus and expansion survive deltas.
      for (
        let idx = incremental ? appendFrom : 0;
        idx < entry.items.length;
        idx++
      ) {
        const item = entry.items[idx],
          key = itemKey(item, idx),
          isFoldedNote = Boolean(foldable[idx]);
        let card = null;
        if (!isFoldedNote) {
          wanted.add(key);
          card = cards.get(key);
          if (!card) {
            card = renderCard(item, key);
            card.dataset.chatId = key;
            cards.set(key, card);
          } else if (!sameItem(card._item, item)) {
            const next = renderCard(item, key);
            card.replaceChildren(...Array.from(next.children));
            card._syncQuestion = next._syncQuestion;
          }
          card._item = item;
        }
        if (!run && idx >= runEnd)
          runEnd = workRunEnd(entry.items, idx, foldable);
        if (run ? isWorkItemKind(item.kind) || isFoldedNote : idx < runEnd) {
          if (!run) {
            let group = toolRuns.get(key);
            if (!group && !incremental) {
              for (let m = idx; m < runEnd && !group; m++) {
                const memberKey = itemKey(entry.items[m], m);
                for (const [oldKey, oldGroup] of toolRuns) {
                  if (
                    !wantedRuns.has(oldKey) &&
                    oldGroup.memberKeys?.has(memberKey)
                  ) {
                    const discloseKey = "run:" + key;
                    toolRuns.delete(oldKey);
                    toolRuns.set(key, oldGroup);
                    if (
                      oldGroup.discloseKey &&
                      oldGroup.discloseKey !== discloseKey &&
                      entry.disclosures?.has(oldGroup.discloseKey)
                    ) {
                      entry.disclosures.set(
                        discloseKey,
                        entry.disclosures.get(oldGroup.discloseKey),
                      );
                      entry.disclosures.delete(oldGroup.discloseKey);
                    }
                    oldGroup.discloseKey = discloseKey;
                    group = oldGroup;
                    break;
                  }
                }
              }
            }
            if (!group) {
              const details = el("details", undefined, "chat-tool-run"),
                summary = el("summary", undefined, "chat-tool-summary"),
                body = el("div", undefined, "chat-tool-items");
              group = {
                details,
                summary,
                body,
                pairs: new Map(),
                memberKeys: new Set(),
                discloseKey: "run:" + key,
              };
              rememberDetails(details, () => group.discloseKey);
              details.append(summary, body);
              toolRuns.set(key, group);
            } else {
              group.memberKeys.clear();
            }
            const nodeSet = new Set(),
              activePairs = new Set();
            group.activeNodes = nodeSet;
            group.activePairs = activePairs;
            run = {
              ...group,
              key,
              nodes: [],
              nodeSet,
              activePairs,
              steps: new Map(),
              categories: new Set(),
              calls: new Map(),
              pending: [],
              pairs: group.pairs,
              memberKeys: group.memberKeys,
              count: 0,
              tools: 0,
              totalTools: 0,
              completedTools: 0,
              thoughts: 0,
              notes: 0,
              errors: 0,
              detail: "",
              thoughtDetail: "",
              noteDetail: "",
              activeDetail: "",
              minAtMs: null,
              maxAtMs: null,
              endIndex: idx,
            };
            wantedRuns.add(key);
            nodes.push(run.details);
          }
          run.memberKeys.add(key);
          run.endIndex = idx + 1;
          const atMs = parseItemAtMs(item.at);
          if (atMs !== null) {
            if (run.minAtMs === null || atMs < run.minAtMs) run.minAtMs = atMs;
            if (run.maxAtMs === null || atMs > run.maxAtMs) run.maxAtMs = atMs;
          }
          let node = card;
          if (isFoldedNote) {
            wantedNotes.add(key);
            let noteNode = noteCards.get(key);
            const trimmed = String(item.text || "").trim();
            if (!noteNode) {
              noteNode = renderWorkNote(item);
              noteNode.dataset.chatId = key;
              noteCards.set(key, noteNode);
            } else if (noteNode._text !== trimmed) {
              noteNode._text = trimmed;
              if (noteNode._textNode) noteNode._textNode.textContent = trimmed;
            }
            run.notes++;
            if (trimmed) {
              run.noteDetail = trimmed;
              run.activeDetail = trimmed;
            }
            node = noteNode;
          } else if (item.kind === "reasoning") {
            run.thoughts++;
            const thoughtLine = firstNonEmptyLine(item.text);
            if (!run.count)
              run.thoughtDetail = thoughtLine || run.thoughtDetail;
            run.activeDetail = thoughtLine || t("Reasoning");
          } else {
            run.tools++;
            // Pair results by the wire tool ID, with ordered fallback for older transcripts.
            const callId = item.call_id || item.tool_use_id || item.tool_id;
            let pair;
            if (item.kind === "tool_call") {
              pair = run.pairs.get(key) || {
                node: el("div", undefined, "chat-tool-pair"),
                call: item,
                callCard: card,
              };
              pair.call = item;
              pair.callCard = card;
              run.pairs.set(key, pair);
              run.activePairs.add(key);
              run.pending.push(pair);
              if (callId) run.calls.set(String(callId), pair);
              run.count++;
              run.totalTools++;
              const detail = toolDetail(item);
              run.detail = detail || run.detail;
              run.activeDetail = detail || item.tool_name || t("Tool call");
              const step = toolStep(item.tool_name);
              run.steps.set(step, (run.steps.get(step) || 0) + 1);
              run.categories.add(toolCategory(item.tool_name));
            } else {
              pair = callId ? run.calls.get(String(callId)) : run.pending[0];
              if (pair) {
                const index = run.pending.indexOf(pair);
                if (index >= 0) run.pending.splice(index, 1);
                run.completedTools++;
                if (item.is_error && pair.callCard?.children?.[0])
                  pair.callCard.children[0].dataset.error = "true";
                const summary = card.children?.[0]?.children?.[0];
                if (summary)
                  summary.textContent = [
                    t(item.is_error ? "Tool error" : "Tool output"),
                    pair.call.tool_name,
                    pair.call.title || pair.call.file_path,
                  ]
                    .filter(Boolean)
                    .join(" · ");
              } else {
                run.totalTools++;
                run.completedTools++;
                const orphanDetail =
                  toolDetail(item) || firstNonEmptyLine(item.text);
                run.detail = orphanDetail || run.detail;
                run.activeDetail = orphanDetail || t("Tool activity");
              }
            }
            node = pair?.node || card;
            if (pair) {
              const at =
                pair.node.children?.[item.kind === "tool_call" ? 0 : 1];
              if (at !== card) pair.node.insertBefore(card, at || null);
            }
            if (item.is_error) run.errors++;
          }
          if (!run.nodeSet.has(node)) {
            run.nodeSet.add(node);
            run.nodes.push(node);
            const at = run.body.children?.[run.nodes.length - 1];
            if (at !== node) run.body.insertBefore(node, at || null);
          }
        } else {
          if (run) syncRunSummary(run, false);
          run = null;
          runEnd = 0;
          nodes.push(card);
        }
      }
      for (const [idx, node] of nodes.entries()) {
        const at = incremental
          ? optimisticCard?.parentElement === feed
            ? optimisticCard
            : null
          : feed.children?.[idx];
        if (at !== node) {
          feed.insertBefore(node, at || null);
          resize?.observe(node);
        }
      }
      for (const [key, group] of toolRuns)
        if (!incremental) {
          if (!wantedRuns.has(key)) {
            group.details.remove();
            resize?.unobserve(group.details);
            toolRuns.delete(key);
          } else {
            for (const [callKey, pair] of group.pairs)
              if (!group.activePairs?.has(callKey)) {
                pair.node.remove();
                group.pairs.delete(callKey);
              }
            for (const child of Array.from(group.body.children || []))
              if (!group.activeNodes?.has(child)) child.remove();
          }
        }
      for (const [key, card] of cards)
        if (!incremental && !wanted.has(key)) {
          card.remove();
          resize?.unobserve(card);
          cards.delete(key);
          codeButtonsByCard.delete(key);
        }
      for (const [key, noteNode] of noteCards)
        if (!incremental && !wantedNotes.has(key)) {
          noteNode.remove();
          noteCards.delete(key);
        }
      if (!incremental) {
        for (const child of Array.from(feed.children || []))
          if (child.className === "chat-empty") child.remove();
        for (const dKey of entry.disclosures?.keys() || []) {
          const keep =
            cards.has(dKey) ||
            (dKey.startsWith("run:") && toolRuns.has(dKey.slice(4))) ||
            (dKey.startsWith("past:") && cards.has(dKey.slice(5))) ||
            (dKey.startsWith("user:") && cards.has(dKey.slice(5))) ||
            (dKey.startsWith("code-wrap:") &&
              cards.has(dKey.slice(10).replace(/:\d+$/, "")));
          if (!keep) entry.disclosures.delete(dKey);
        }
      }
      tailRun = run;
      syncTailRunSummary();
      renderedCount = entry.items.length;
      renderedItems = entry.items;
      if (!entry.items.length && entry.loaded && !optimisticText()) {
        const empty = el("div", undefined, "chat-empty");
        empty.append(el("p", t("Enter a new message"), "muted"));
        feed.append(empty);
      }
      const first = entry.items[0];
      const start = first?._pageStart || 0;
      earlierBtn.hidden =
        !(entry.pages.findIndex((page) => page.start === start) > 0) &&
        !first?._clipped &&
        !(first && start > 0);
      earlierBtn.disabled = earlierPending;
      latestBtn.hidden =
        !entry.live &&
        !earlierPending &&
        !failedEarlier &&
        !entry.latestRecovery;
      history.hidden = earlierBtn.hidden && latestBtn.hidden;
      syncQuestionCards();
      syncLiveCardVisibility();
      syncOptimistic();
      pinLatest();
      restoreReader(anchor);
    }
    function rememberDetails(node, key) {
      const resolve = () => (typeof key === "function" ? key() : key);
      node.open = entry.disclosures?.get(resolve()) || false;
      node.addEventListener("toggle", () => {
        const current = resolve();
        entry.disclosures ||= new Map();
        if (node.open) entry.disclosures.set(current, true);
        else entry.disclosures.delete(current);
      });
    }
    function lazyDetails(details, build) {
      if (details.open) build();
      else
        details.addEventListener("toggle", () => {
          if (details.open && !details.dataset.loaded) {
            details.dataset.loaded = "true";
            build();
          }
        });
      if (details.open) details.dataset.loaded = "true";
    }
    function renderCard(item, key) {
      const kind = item.kind || "assistant",
        card = el("article", undefined, `chat-card chat-${kind}`),
        richOpts = richOptsFor(key);
      codeButtonsByCard.set(key, []);
      if (kind === "user") {
        const bubble = el("div", undefined, "chat-user-bubble"),
          body = renderRichText(el, item.text, richOpts);
        if (userMessageFolds(item.text)) {
          const foldKey = `user:${key}`,
            foldWrap = el("div", undefined, "chat-user-fold"),
            toggleBtn = button(
              "",
              () => {
                const nextOpen = !entry.disclosures?.get(foldKey);
                entry.disclosures ||= new Map();
                if (nextOpen) entry.disclosures.set(foldKey, true);
                else entry.disclosures.delete(foldKey);
                syncFold();
              },
              "ghost small chat-user-fold-toggle",
            ),
            syncFold = () => {
              const open = Boolean(entry.disclosures?.get(foldKey)),
                label = t(open ? "Show less" : "Show full message");
              foldWrap.dataset.folded = String(!open);
              toggleBtn.textContent = label;
              toggleBtn.setAttribute("aria-expanded", String(open));
              toggleBtn.setAttribute("aria-label", label);
            };
          foldWrap.append(body);
          syncFold();
          bubble.append(foldWrap, toggleBtn);
        } else {
          bubble.append(body);
        }
        card.append(bubble, messageMeta(item));
      } else if (kind === "assistant") {
        card.append(renderRichText(el, item.text, richOpts), messageMeta(item));
      } else if (kind === "reasoning") {
        const details = el("details", undefined, "chat-reasoning"),
          summary = el("summary", undefined, "chat-reasoning-summary"),
          iconWrap = el("span", undefined, "chat-reasoning-icon"),
          brain = svgIcon("brain"),
          headline = el(
            "span",
            t(item.status === "completed" ? "Thought" : "Reasoning"),
            "chat-reasoning-headline",
          ),
          caretWrap = el("span", undefined, "chat-reasoning-caret"),
          caret = svgIcon("chevronRight");
        iconWrap.setAttribute("aria-hidden", "true");
        if (brain) iconWrap.append(brain);
        caretWrap.setAttribute("aria-hidden", "true");
        if (caret) caretWrap.append(caret);
        summary.append(iconWrap, headline, caretWrap);
        rememberDetails(details, key);
        details.append(summary);
        lazyDetails(details, () => {
          const body = el("div", undefined, "chat-reasoning-body");
          body.append(renderRichText(el, item.text, richOpts));
          details.append(body);
        });
        card.append(details);
      } else if (kind === "tool_result") {
        const details = el("details", undefined, "chat-tool-result");
        rememberDetails(details, key);
        details.append(
          el("summary", t(item.is_error ? "Tool error" : "Tool output")),
        );
        lazyDetails(details, () =>
          details.append(el("pre", item.text, "chat-code")),
        );
        card.append(details);
      } else if (kind === "tool_call") {
        const details = el("details", undefined, "chat-tool-call"),
          head = el("summary", undefined, "chat-tool-head"),
          line = toolLineLabel(item),
          iconWrap = el("span", undefined, "chat-tool-icon"),
          glyph = svgIcon(line.icon);
        if (item.is_error) details.dataset.error = "true";
        iconWrap.setAttribute("aria-hidden", "true");
        if (glyph) iconWrap.append(glyph);
        head.append(iconWrap);
        if (line.verb) head.append(el("span", t(line.verb), "chat-tool-verb"));
        if (line.target)
          head.append(el("span", line.target, "chat-tool-title"));
        else if (!line.verb)
          head.append(
            el("span", item.tool_name || t("Tool call"), "chat-tool-title"),
          );
        const hasDetail = Boolean(
          item.file_path || (item.text && item.text !== item.file_path),
        );
        if (hasDetail) {
          const caretWrap = el("span", undefined, "chat-tool-caret"),
            caret = svgIcon("chevronRight");
          caretWrap.setAttribute("aria-hidden", "true");
          if (caret) caretWrap.append(caret);
          head.append(caretWrap);
        }
        rememberDetails(details, key);
        details.append(head);
        if (item.file_path)
          details.append(
            button(
              t("Current diff"),
              () => openDiff(item.file_path),
              "ghost small",
            ),
          );
        if (item.text && item.text !== item.file_path)
          lazyDetails(details, () =>
            details.append(el("pre", item.text, "chat-tool-body")),
          );
        card.append(details);
      } else if (kind === "question") renderQuestionCard(card, item, key);
      else if (kind === "permission") {
        card.append(
          el(
            "header",
            `${t("Permission")} • ${item.title || item.tool_name || t("Approval")}`,
            "chat-role",
          ),
          el("p", item.text, "chat-p"),
          el(
            "p",
            t("Historical request. Confirm the current request in Term."),
            "muted",
          ),
          button(t("Confirm in Term"), () => onSwitchTerm?.(), "ghost small"),
        );
      }
      return card;
    }
    function renderQuestionCard(card, item, key) {
      const history = el("details", undefined, "chat-past-question"),
        summary = el("summary"),
        body = el("div", undefined, "chat-past-question-body");
      let wasLive = false,
        ignoreToggle = false,
        cardError = "",
        sendingIndex = -1;
      history.open = entry.disclosures?.get("past:" + key) || false;
      history.addEventListener("toggle", () => {
        if (ignoreToggle || activeQuestionKey === key) return;
        entry.disclosures ||= new Map();
        if (history.open) entry.disclosures.set("past:" + key, true);
        else entry.disclosures.delete("past:" + key);
      });
      history.append(summary, body);
      card.append(history);
      body.append(el("p", item.text, "chat-p"));
      const list = el("div", undefined, "chat-question-options"),
        selected = new Set(),
        buttons = [];
      const custom = el("input");
      custom.type = "text";
      custom.value = "";
      custom.placeholder = t("Other (write-in)…");
      custom.setAttribute("aria-label", t("Other answer notes"));
      const statusNote = el(
        "p",
        t(
          "These are answer notes only. Confirm the current question and enter your answer in Term.",
        ),
        "muted",
      );
      const currentLive = () => {
        if (activeQuestionKey !== key) return null;
        const q = opts.getQuestion?.() || liveQuestion;
        return matchesLiveQuestion(item, q) ? q : null;
      };
      const draftAnswer = () =>
        [
          ...[...selected].map((i) => item.options?.[i]?.label).filter(Boolean),
          String(custom.value || "").trim(),
        ]
          .filter(Boolean)
          .join(", ");
      const copyBtn = button(
        t("Copy answer"),
        () => {
          const ans = draftAnswer();
          if (ans) copyText(ans, copyBtn);
        },
        "secondary small",
      );
      const sendAnswerBtn = button(
        t("Send answer"),
        () => {
          const question = currentLive();
          if (question)
            sendFromCard(question, draftFor(question).selected.slice());
        },
        "primary small",
      );
      const termBtn = button(
        t("Confirm in Term"),
        () => onSwitchTerm?.(),
        "ghost small",
      );
      async function sendFromCard(question, picks) {
        if (
          !question?.enabled ||
          entry.questionSending ||
          disposed ||
          (!picks.length && !question.multiple)
        )
          return;
        const draft = entry.questionDraft;
        entry.questionSending = true;
        sendingIndex = !question.multiple && picks.length === 1 ? picks[0] : -1;
        cardError = "";
        syncQuestionCards();
        renderLiveQuestion();
        if (panelOpen(replyPanel)) renderReplyPanel(replyPanel);
        try {
          await opts.onAnswerQuestion(question.token, picks);
          if (disposed || ctx.signal?.aborted) return;
          if (draft && entry.questionDraft === draft) entry.questionDraft = null;
          if (panelOpen(replyPanel)) ctx.closeSheet?.();
          ctx.toast?.(t("Answer sent"));
          if (picks.some((i) => isWriteInLabel(question.choices[i]?.label)))
            onSwitchTerm?.();
        } catch (error) {
          if (!disposed) cardError = cleanRpcError(error, t);
        } finally {
          entry.questionSending = false;
          sendingIndex = -1;
          if (!disposed) {
            syncQuestionCards();
            renderLiveQuestion();
            if (panelOpen(replyPanel)) renderReplyPanel(replyPanel);
          }
        }
      }
      function syncCard() {
        const question = activeQuestionKey === key ? liveQuestion : null,
          isLive = Boolean(question);
        card.dataset.liveQuestion = String(isLive);
        const prefix = t(isLive ? "Current question" : "Past question");
        summary.textContent = item.title ? `${prefix} • ${item.title}` : prefix;
        if (isLive && !history.open) {
          ignoreToggle = true;
          history.open = true;
          Promise.resolve().then(() => {
            ignoreToggle = false;
          });
        } else if (!isLive && wasLive) {
          const saved = entry.disclosures?.get("past:" + key) || false;
          if (history.open !== saved) {
            ignoreToggle = true;
            history.open = saved;
            Promise.resolve().then(() => {
              ignoreToggle = false;
            });
          }
        }
        wasLive = isLive;
        if (isLive) {
          const draft = draftFor(question),
            picks = question.multiple
              ? draft.selected
              : sendingIndex >= 0
                ? [sendingIndex]
                : [],
            enabled = question.enabled && !entry.questionSending && !disposed;
          buttons.forEach(({ button: b, baseLabel }, i) => {
            const active = picks.includes(i);
            b.textContent = question.multiple
              ? `${active ? "☑ " : "☐ "}${baseLabel}`
              : baseLabel;
            b.className = `${active ? "primary" : "ghost"} small chat-opt`;
            b.setAttribute("aria-pressed", String(active));
            b.disabled = !enabled;
          });
          custom.hidden = true;
          copyBtn.hidden = true;
          copyBtn.disabled = true;
          sendAnswerBtn.hidden = !question.multiple;
          sendAnswerBtn.disabled = !enabled;
          sendAnswerBtn.textContent = t(
            entry.questionSending ? "Sending answer…" : "Send answer",
          );
          sendAnswerBtn.setAttribute("aria-label", sendAnswerBtn.textContent);
          termBtn.textContent = t("Write an answer in Term");
          termBtn.setAttribute("aria-label", termBtn.textContent);
          const note =
            cardError ||
            (entry.questionSending
              ? t("Sending answer…")
              : !question.enabled
                ? t(
                    "Waiting for the current question. Check Term if this continues.",
                  )
                : question.multiple
                  ? t("Select answers, then submit")
                  : "");
          statusNote.textContent = note;
          statusNote.hidden = !note;
          statusNote.dataset.error = String(Boolean(cardError));
        } else {
          cardError = "";
          buttons.forEach(({ button: b, baseLabel }, i) => {
            const active = selected.has(i);
            b.textContent = baseLabel;
            b.className = `${active ? "primary" : "ghost"} small chat-opt`;
            b.setAttribute("aria-pressed", String(active));
            b.disabled = false;
          });
          custom.hidden = false;
          const has = Boolean(draftAnswer());
          copyBtn.hidden = !has;
          copyBtn.disabled = !has;
          sendAnswerBtn.hidden = true;
          sendAnswerBtn.disabled = true;
          termBtn.textContent = t("Confirm in Term");
          termBtn.setAttribute("aria-label", termBtn.textContent);
          statusNote.textContent = t(
            "These are answer notes only. Confirm the current question and enter your answer in Term.",
          );
          statusNote.hidden = false;
          statusNote.dataset.error = "false";
        }
      }
      for (const [index, opt] of (item.options || []).entries()) {
        const baseLabel = opt.description
          ? `${opt.label} — ${opt.description}`
          : opt.label;
        const b = button(
          baseLabel,
          () => {
            const question = currentLive();
            if (question) {
              if (!question.enabled || entry.questionSending || disposed)
                return;
              cardError = "";
              if (question.multiple) {
                const draft = draftFor(question),
                  cur = draft.selected;
                draft.changed = true;
                draft.selected = cur.includes(index)
                  ? cur.filter((i) => i !== index)
                  : [...cur, index];
                syncCard();
                renderLiveQuestion();
                if (panelOpen(replyPanel)) renderReplyPanel(replyPanel);
              } else {
                sendFromCard(question, [index]);
              }
              return;
            }
            if (!item.multi_select) selected.clear();
            if (selected.has(index)) selected.delete(index);
            else selected.add(index);
            syncCard();
          },
          "ghost small chat-opt",
        );
        b.setAttribute("aria-pressed", "false");
        buttons.push({ button: b, baseLabel });
        list.append(b);
      }
      custom.addEventListener("input", syncCard);
      const actions = el("div", undefined, "chat-actions");
      actions.append(sendAnswerBtn, termBtn, copyBtn);
      body.append(list, custom, statusNote, actions);
      card._syncQuestion = syncCard;
      syncCard();
    }
    function openDiff(filePath) {
      run(async () => {
        const normalizePrefix = (p) =>
          String(p || "").replace(/^\/private(?=\/(?:tmp|var)(?:\/|$))/, "");
        const sess = state.sessions?.get(sessionId),
          cwd = normalizePrefix(sess?.cwd).replace(/\/+$/, ""),
          path = normalizePrefix(filePath);
        const relPath =
          cwd && path.startsWith(cwd + "/")
            ? path.slice(cwd.length + 1)
            : filePath;
        const data = await ctx.rpc({
          action: "git",
          session: sessionId,
          path: relPath,
        });
        if (disposed || ctx.signal?.aborted) return;
        ctx.sheet?.(
          t("Current diff") + `: ${filePath}`,
          [],
          undefined,
          (body) => {
            const message =
              data?.diff_status === "outside_scope"
                ? t("This file is outside the current workspace diff.")
                : t(
                    "No current changes for this file. This is a historical tool record; its earlier diff is not stored here.",
                  );
            body.append(
              el(
                "pre",
                data?.patch || message,
                data?.patch ? "chat-code" : "muted",
              ),
            );
          },
        );
      });
    }
    function updateStatus() {
      const sess = state.sessions?.get(sessionId),
        progress = entry.progress;
      const currentItems = entry.live?.items || entry.items;
      if (progress?.phase === "accepted" && sess?.state === "Working")
        progress.confirmed = true;
      if (
        progress?.canConfirm &&
        !progress.confirmed &&
        progress.generation === entry.generation &&
        currentItems.some(
          (item) =>
            item.kind === "user" &&
            !progress.ids.has(item.id) &&
            item.text === progress.text,
        )
      )
        progress.confirmed = true;
      let status = sess?.state || "Unknown",
        message = sess?.lines?.at(-1) || "";
      if (state.connection?.phase !== "ready") {
        status = "Offline";
        message = t("Offline. Reconnect to update activity.");
      } else if (progress) {
        if (sess?.state !== progress.lastState) {
          progress.stateChanged = true;
          progress.lastState = sess?.state;
        }
        const changed =
          progress.stateChanged ||
          (sess?.state_since_unix_ns &&
            sess.state_since_unix_ns !== progress.stamp);
        if (progress.phase === "sending") {
          status = "Sending";
          message = t("Sending…");
        } else if (progress.phase === "error") {
          status = "Blocked";
          message = t(
            "Send not confirmed. Draft kept; check activity before retrying.",
          );
        } else if (sess?.exited) {
          status = "Exited";
          message = t("Session ended.");
        } else if (
          changed &&
          ["Waiting", "Blocked", "Done"].includes(sess?.state)
        )
          message = t(
            sess.state === "Done"
              ? "Completed"
              : sess.state === "Waiting"
                ? "Waiting for input"
                : "Agent blocked",
          );
        else if (sess?.state === "Working") {
          status = "Working";
          message = t("Working");
        } else {
          status = "Accepted";
          message = t("Accepted. Awaiting agent response…");
        }
        if (
          progress.phase !== "sending" &&
          progress.phase !== "error" &&
          !["Waiting", "Blocked", "Done", "Exited"].includes(status)
        ) {
          const latest =
            progress.generation === entry.generation
              ? currentItems.findLast(
                  (item) => !progress.ids.has(item.id) && item.kind !== "user",
                )
              : null;
          if (latest) {
            if (latest.kind === "tool_call") {
              status = "Working";
              message = [
                t("Tool activity"),
                latest.tool_name,
                latest.title || latest.file_path,
              ]
                .filter(Boolean)
                .join(" · ");
            } else if (latest.kind === "tool_result" && latest.is_error) {
              status = "Blocked";
              message = t("Tool error");
            } else if (
              latest.kind === "question" ||
              latest.kind === "permission"
            ) {
              status = "Waiting";
              message = t("Waiting for input");
            } else if (status !== "Working") message = t("Response received");
          }
        }
      }
      const unconfirmed = progress && !progress.confirmed,
        // Online, the status above already names the send phase.
        receipt = !unconfirmed || status !== "Offline"
          ? ""
          : t(
              progress.phase === "sending"
                ? "Sending…"
                : progress.phase === "error"
                  ? "Send not confirmed"
                  : "Accepted. Awaiting agent response…",
            ),
        draft = unconfirmed ? progress.text || t("Photo") : "",
        badge = t(status),
        bare = (text) => text.replace(/[….]+$/, "");
      statusLine.hidden =
        !liveQuestion && !progress &&
        state.connection?.phase === "ready" &&
        !["Waiting", "Blocked", "Working"].includes(status);
      statusBadge.textContent = badge;
      statusBadge.dataset.state = status;
      statusText.textContent = [
        receipt,
        bare(message) === bare(badge) ? "" : message,
        draft.length > 240 ? draft.slice(0, 240) + "…" : draft,
      ]
        .filter(Boolean)
        .join(" · ");
      statusLine.title = statusText.textContent;
      syncTailRunSummary();
      syncAgent();
    }
    function normalizeItems(res, start) {
      return (res.items || []).map((item, index) => ({
        ...item,
        id: item.id || `${start}:${index}`,
        _pageStart: start,
        _retainedBytes: JSON.stringify(item).length * 2,
      }));
    }
    function clearTranscript(clearSessionState = true) {
      prefetchedEarlier = null; fillPages = 0;
      entry.composerStatus = null; entry.composerStatusAt = 0; entry.composerProbes = 0; statusFlight = null;
      entry.items = [];
      entry.retainedBytes = 0;
      if (clearSessionState) entry.disclosures?.clear();
      entry.pages = [];
      entry.live = null;
      entry.agent = "";
      entry.bound = false;
      cards.clear();
      noteCards.clear();
      codeButtonsByCard.clear();
      if (clearSessionState) clearCopiedCode();
      toolRuns.clear();
      tailRun = null;
      optimisticCard = null;
      optimisticShown = "";
      feed.replaceChildren();
      resize?.disconnect();
      resize?.observe(feed);
    }
    // Pages are chronological links. Cursor numbers may decrease between live pages.
    function rememberPage(start, next) {
      if (
        !Number.isSafeInteger(start) ||
        !Number.isSafeInteger(next) ||
        start < 0 ||
        next < 0 ||
        start === next
      )
        return;
      const pages = entry.pages;
      if (!pages.some((p) => p.start === start)) {
        const after = pages.findIndex((p) => p.start === next),
          before = pages.findIndex((p) => p.next === start);
        pages.splice(
          after >= 0 ? after : before >= 0 ? before + 1 : pages.length,
          0,
          { start, next },
        );
      }
      while (pages.length > pageLimit) {
        const first = entry.items[0]?._pageStart ?? start,
          index = pages.findIndex((p) => p.start === first);
        if (index > 0) pages.shift();
        else pages.pop();
      }
    }

    function trimItems(fromStart) {
      let bytes = entry.items.reduce(
          (n, item) => n + (item._retainedBytes || 0),
          0,
        ),
        count = 0;
      while (
        entry.items.length - count > 1 &&
        (entry.items.length - count > itemLimit || bytes > itemByteLimit)
      ) {
        const item =
          entry.items[fromStart ? count : entry.items.length - 1 - count];
        bytes -= item._retainedBytes || 0;
        count++;
      }
      entry.retainedBytes =
        bytes +
        (entry.live?.items.reduce(
          (n, item) => n + (item._retainedBytes || 0),
          0,
        ) || 0);
      if (!count) return false;
      if (fromStart) {
        const removed = entry.items.splice(0, count),
          first = entry.items[0];
        // Re-read this page only when it actually contained an item that was clipped.
        if (first && removed.at(-1)?._pageStart === first._pageStart)
          first._clipped = true;
      } else entry.items.length -= count;
      return true;
    }
    function showPollError(error) {
      const anchor = readerAnchor();
      failures++;
      pollStatus.hidden = false;
      const when = lastSuccess
        ? new Date(lastSuccess).toTimeString().slice(0, 8)
        : t("never");
      const detail = error ? cleanRpcError(error, t) : "";
      updateStatus();
      pollText.textContent = `${t("Chat history update failed")} (${failures}). ${t("Last updated")}: ${when}. ${t("Retry or check Term.")}${detail ? ` ${detail}` : ""}`;
      restoreReader(anchor);
    }
    function sameItem(a, b) {
      if (!a || !b) return false;
      if (
        (b.id !== undefined && a.id !== b.id) ||
        a.kind !== b.kind ||
        a.text !== b.text ||
        a.at !== b.at ||
        a.title !== b.title ||
        a.tool_name !== b.tool_name ||
        a.tool_id !== b.tool_id ||
        a.file_path !== b.file_path ||
        a.is_error !== b.is_error ||
        a.status !== b.status ||
        a.multi_select !== b.multi_select
      )
        return false;
      const ao = a.options,
        bo = b.options;
      if (!!ao !== !!bo) return false;
      if (ao) {
        if (ao.length !== bo.length) return false;
        for (let i = 0; i < ao.length; i++)
          if (
            ao[i]?.label !== bo[i]?.label ||
            ao[i]?.description !== bo[i]?.description
          )
            return false;
      }
      return true;
    }
    async function pollNow(initial = false) {
      // 숨은 Chat도 첫 내용을 받을 때까지는 받아 둔다(실패 2회까지). 연결 전에 마운트돼 첫 프리페치를 놓쳐도 ready 뒤 update()가 다시 받아, Chat을 누른 첫 프레임에 내용이 있다.
      if (
        !active() ||
        state.cached ||
        (!visible && !initial && (entry.loaded || failures >= 2)) ||
        globalThis.document?.hidden ||
        globalThis.document?.visibilityState === "hidden" ||
        inFlight ||
        earlierPending ||
        (entry.live && !resetHistory) ||
        state.connection?.phase !== "ready"
      )
        return;
      const epoch = earlierEpoch,
        prefetch = !visible,
        owner = ctx.transport,
        linkEpoch = state.connection?.generation;
      const current = () =>
        active() &&
        epoch === earlierEpoch &&
        owner === ctx.transport &&
        linkEpoch === state.connection?.generation &&
        !state.cached &&
        (visible || prefetch) &&
        !globalThis.document?.hidden &&
        globalThis.document?.visibilityState !== "hidden" &&
        state.connection?.phase === "ready";
      inFlight = true;
      try {
        // v0.1.37 이하 데몬은 latest를 모르는 필드로 거부한다. chat_tail이 없는 데몬이 그렇게 거부하면 처음부터 이어 읽는다(빈 화면으로 남기지 않는다).
        if (entry.noTail && state.snapshot?.capabilities?.chat_tail === true)
          entry.noTail = false;
        const start = entry.offset;
        let latest = !entry.initialized || refreshLatest,
          res = await ctx.rpc({
            action: "chat_poll",
            session: sessionId,
            offset: latest ? 0 : start,
            ...(latest && !entry.noTail ? { latest: true } : {}),
          });
        if (!current() || earlierPending || (entry.live && !resetHistory))
          return;
        if (!res)
          throw new Error(t("Transcript changed. Return to latest and retry."));
        let changed =
          entry.generation &&
          res.generation &&
          entry.generation !== res.generation;
        // Rebinding also opens at the tail. Keep the current paint until that window is ready.
        if (!latest && !entry.noTail && (res.reset || changed || !res.bound)) {
          requestLatest();
          res = await ctx.rpc({
            action: "chat_poll",
            session: sessionId,
            offset: 0,
            latest: true,
          });
          if (!current() || earlierPending || (entry.live && !resetHistory))
            return;
          if (!res)
            throw new Error(
              t("Transcript changed. Return to latest and retry."),
            );
          latest = true;
          changed = true;
        }
        if (!res.bound && (entry.items.length || entry.latestRecovery)) {
          requestLatest();
          renderItems();
          throw new Error(t("Transcript changed. Return to latest and retry."));
        }
        const rItems = res.items || [],
          sameTail =
            latest &&
            !resetHistory &&
            !changed &&
            !res.reset &&
            entry.generation === res.generation &&
            entry.items.length === rItems.length &&
            rItems.every((item, index) => sameItem(entry.items[index], item));
        const reset = Boolean(res.reset || changed || (latest && resetHistory)),
          metaChanged =
            entry.agent !== (res.agent || "") || entry.bound !== !!res.bound;
        const hadItems = entry.items.length,
          wasLoaded = entry.loaded,
          anchor =
            (!sameTail && (rItems.length || metaChanged)) || !pollStatus.hidden
              ? readerAnchor()
              : null;
        if (res.reset && entry.progress && entry.progress.phase !== "sending")
          entry.progress.rendered = true;
        const generationChanged = Boolean(
          !res.bound ||
            (entry.generation &&
              res.generation &&
              entry.generation !== res.generation),
        );
        if (reset) {
          clearTranscript(generationChanged);
          following = true;
        }
        entry.generation = res.generation || entry.generation;
        entry.agent = res.agent || "";
        entry.bound = !!res.bound;
        entry.initialized = entry.bound;
        entry.loaded = true;
        refreshLatest = false;
        resetHistory = false;
        entry.latestRecovery = false;
        const effectiveStart = res.page_start ?? (res.reset ? 0 : start);
        const incoming = normalizeItems(res, effectiveStart),
          before = entry.items.length;
        let reconciled = false;
        if (incoming.length) {
          if (latest && !reset && !sameTail) {
            const incomingIds = new Set(incoming.map((item) => item.id)),
              overlap = entry.items.findIndex((old) => incomingIds.has(old.id)),
              next =
                overlap >= 0
                  ? [...entry.items.slice(0, overlap), ...incoming]
                  : incoming;
            reconciled =
              next.length !== entry.items.length ||
              next.some((item, i) => !sameItem(entry.items[i], item));
            if (reconciled) entry.items = next;
          } else if (!sameTail) {
            const seen = new Set(entry.items.map((item) => item.id));
            entry.items.push(...incoming.filter((item) => !seen.has(item.id)));
          }
          rememberPage(effectiveStart, res.next_offset);
        }
        const trimmed = incoming.length && trimItems(true);
        if (typeof res.next_offset === "number") entry.offset = res.next_offset;
        lastSuccess = entry.lastSuccess = Date.now();
        failures = 0;
        pollStatus.hidden = true;
        if (
          reconciled ||
          entry.items.length !== before ||
          trimmed ||
          metaChanged ||
          !wasLoaded ||
          (reset && (hadItems || !renderedItems))
        )
          renderItems(!reset && !trimmed && !reconciled ? before : undefined);
        updateStatus();
        restoreReader(anchor);
        pruneCache(state);
        refreshStatus();
      } catch (error) {
        if (!current()) return;
        if (
          !entry.noTail &&
          state.snapshot &&
          !state.cached &&
          state.snapshot.capabilities?.chat_tail !== true &&
          /invalid mobile authentication or frame/i.test(
            String(error?.message || error),
          )
        ) {
          entry.noTail = true;
          retryWithoutTail = true;
          return;
        }
        if (refreshLatest && entry.items.length) requestLatest();
        showPollError(error);
      } finally {
        inFlight = false;
        if (earlierQueued) drainEarlier();
        prefetchEarlier();
        fillHistory();
        if (retryWithoutTail) {
          retryWithoutTail = false;
          pollNow(initial);
        } else if (pollQueued) {
          pollQueued = false;
          if (canReadEarlier() && !entry.loaded) pollNow();
        }
      }
    }
    function canReadEarlier() {
      return (
        active() &&
        isVisible() &&
        !state.cached &&
        state.connection?.phase === "ready"
      );
    }
    function earlierPage() {
      const first = entry.items[0];
      if (!first) return null;
      const page = first._clipped
        ? entry.pages.find((page) => page.start === first._pageStart)
        : entry.pages[
            entry.pages.findIndex((page) => page.start === first._pageStart) - 1
          ];
      const start = page?.start,
        end = page?.next ?? first._pageStart;
      return start === undefined && !end
        ? null
        : {
            first,
            start,
            end,
            key: `${entry.generation}:${start}:${end}:${first.id}`,
          };
    }
    function historyThreshold() {return capability("chat_tail") ? Math.max(600, feed.clientHeight * 1.5) : 120;}
    function prefetchEarlier() {
      if (!capability("chat_tail") || !canReadEarlier() || earlierPending) return null;
      const page = earlierPage();
      if (!page) {prefetchedEarlier = null; return null;}
      if (autoBlocked === page.key) return null;
      if (prefetchedEarlier?.key === page.key) return prefetchedEarlier;
      if (prefetchedEarlier && !prefetchedEarlier.settled) return null;
      const prepared = {key:page.key, promise:Promise.resolve().then(() => ctx.rpc({action:"chat_poll",session:sessionId,offset:page.start??0,end_offset:page.end,...page.start===undefined?{latest:true}:{}}))};
      prepared.promise.then(() => prepared.settled = true, () => prepared.settled = true);
      prefetchedEarlier = prepared; return prepared;
    }
    function fillHistory() {
      if (!capability("chat_tail") || !canReadEarlier() || inFlight || earlierPending || !earlierPage() || autoBlocked) return;
      if (feed.scrollHeight <= feed.clientHeight + 1 && fillPages < 4) {earlierQueued = true; drainEarlier();}
    }
    function drainEarlier() {
      if (!earlierQueued || inFlight) return;
      earlierQueued = false;
      const short = capability("chat_tail") && feed.scrollHeight <= feed.clientHeight + 1;
      if (canReadEarlier() && (!following || short && fillPages < 4) && feed.scrollTop <= historyThreshold())
        loadEarlier(true, short && following);
    }
    async function loadEarlier(automatic = false, filling = false) {
      if (!canReadEarlier() || earlierPending || inFlight) return;
      const page = earlierPage();
      if (!page || (automatic && autoBlocked === page.key)) return;
      const { first, end } = page;
      const prepared = prefetchedEarlier?.key === page.key ? prefetchedEarlier : prefetchEarlier();
      let { start } = page;
      const epoch = ++earlierEpoch,
        generation = entry.generation,
        owner = ctx.transport,
        linkEpoch = state.connection?.generation;
      const current = () =>
        canReadEarlier() &&
        epoch === earlierEpoch &&
        owner === ctx.transport &&
        linkEpoch === state.connection?.generation;
      earlierQueued = false;
      earlierPending = true;
      failedEarlier = false;
      if (!filling) following = false;
      if (feed.scrollHeight <= feed.clientHeight + 1) fillPages++;
      renderItems();
      let loaded = false;
      try {
        const res = await (prepared?.key === page.key ? prepared.promise : ctx.rpc({
          action: "chat_poll",
          session: sessionId,
          offset: start ?? 0,
          end_offset: end,
          ...(start === undefined ? { latest: true } : {}),
        }));
        if (!current()) return;
        const responseStart = res?.page_start ?? start;
        if (
          !res ||
          !res.bound ||
          res.next_offset !== end ||
          res.reset ||
          res.generation !== generation ||
          entry.generation !== generation ||
          !Number.isSafeInteger(responseStart) ||
          responseStart < 0 ||
          responseStart === end ||
          (start !== undefined && responseStart !== start)
        )
          throw new Error(t("Transcript changed. Return to latest and retry."));
        // Capture the viewport after any scrolling during the RPC, before changing chrome/items.
        if (!filling) following = false;
        const anchor = readerAnchor();
        start = responseStart;
        rememberPage(start, res.next_offset);
        const items = normalizeItems(res, start),
          index = items.findIndex((item) => item.id === first.id);
        const prefix = index >= 0 ? items.slice(0, index) : items;
        const previous = entry.items,
          live = entry.live;
        if (!filling && !live)
          entry.live = { items: previous.slice(), offset: entry.offset };
        const seen = new Set(previous.map((item) => item.id));
        entry.items = [
          ...prefix.filter((item) => {
            if (seen.has(item.id)) return false;
            seen.add(item.id);
            return true;
          }),
          ...previous,
        ];
        trimItems(filling);
        const anchorGroup = [...toolRuns.values()].find(
            (group) => group.details === anchor?.node,
          ),
          anchorKey = anchor?.node.dataset.chatId;
        // If the reader moved past the retained window, defer the prepend rather than evict their card.
        if (
          anchorKey
            ? !entry.items.some(
                (item, index) => itemKey(item, index) === anchorKey,
              )
            : anchorGroup &&
              !entry.items.some((item, index) =>
                anchorGroup.memberKeys?.has(itemKey(item, index)),
              )
        ) {
          entry.items = previous;
          entry.live = live;
          trimItems(false);
          return;
        }
        if (first._clipped && entry.items[0] !== first) delete first._clipped;
        loaded = entry.items[0] !== first;
        if (filling && !loaded) fillPages = 4;
        failures = 0;
        pollStatus.hidden = true;
        autoBlocked = null;
        renderItems();
        restoreReader(anchor);
      } catch (error) {
        if (current()) {
          autoBlocked = page.key;
          if (!filling) {failedEarlier = true; showPollError(error);}
        }
      } finally {
        earlierPending = false;
        if (epoch === earlierEpoch && (loaded || !filling)) prefetchedEarlier = null;
        if (active()) {
          renderItems();
          if (epoch === earlierEpoch && loaded) {prefetchEarlier(); fillHistory();}
          if (epoch !== earlierEpoch && canReadEarlier() && !entry.live)
            pollNow();
        }
      }
    }
    function isVisible() {
      return (
        visible &&
        !globalThis.document?.hidden &&
        globalThis.document?.visibilityState !== "hidden"
      );
    }
    function schedule() {
      clearTimeout(pollTimer);
      pollTimer = null;
      if (!disposed && isVisible())
        pollTimer = setTimeout(async () => {
          await pollNow();
          schedule();
        }, 1500);
    }
    function visibilityChanged() {
      if (!isVisible()) {
        earlierEpoch++;
        prefetchedEarlier = null;
        earlierQueued = false;
      }
      schedule();
      syncTailRunSummary();
      if (isVisible()) {
        if (Date.now() - lastSuccess > 3000) refreshLatest = true;
        pinLatest();
        pollNow();
        prefetchEarlier(); refreshStatus();
      }
    }
    globalThis.document?.addEventListener?.(
      "visibilitychange",
      visibilityChanged,
    );
    entry.notifyComposer = updateComposer;
    active();
    renderItems();
    updateStatus();
    updateComposer();
    pollNow(true);
    refreshStatus();
    schedule();
    return {
      insertText,
      pollNow,
      setQuestion,
      openQuestion,
      setVisible(next) {
        const changed = visible !== !!next;
        visible = !!next;
        if (changed) visibilityChanged();
      },
      update() {
        if (!canReadEarlier()) {
          earlierEpoch++;
          earlierQueued = false;
          pollQueued = false;
        } else if (inFlight && !entry.loaded && !entry.live) {
          pollQueued = true;
        }
        if (picker && picker.scope !== scope()) closeSuggestions();
        updateStatus();
        updateComposer();
        pollNow(); refreshStatus();
      },
      unmount() {
        if (disposed) return;
        syncDraft();
        earlierEpoch++;
        earlierQueued = false;
        pollQueued = false;
        closeSuggestions();
        discovery.clear();
        discovering.clear();
        disposed = true;
        if (panelOpen(replyPanel)) ctx.closeSheet?.();
        entry.mounts = Math.max(0, (entry.mounts || 1) - 1);
        globalThis.document?.removeEventListener?.(
          "visibilitychange",
          visibilityChanged,
        );
        unsubscribeKeyboard?.();
        resize?.disconnect();
        pruneCache(state);
        clearCopiedCode();
        clearTimeout(pollTimer);
        clearTimeout(appliedTimer);
        pollTimer = null;
        if (entry.notifyComposer === updateComposer)
          entry.notifyComposer = null;
      },
    };
  }
  M.chat = {
    mount: mountChat,
    pruneCache,
    forgetSession,
    isEntryPinned,
    shortModel,
    sortEfforts,
    fenceLang,
    parseRichBlocks,
    userMessageFolds,
  };
  M.screens.chat = (root, ctx) => {
    const session = ctx.state.sessions.get(ctx.route.sessionId);
    if (!session) {
      root.append(
        ctx.el("p", (ctx.t || ((s) => s))("Session not found"), "empty"),
      );
      return {};
    }
    const handle = mountChat(root, {
      ctx,
      sessionId: session.id,
      onSwitchTerm: () =>
        ctx.go(
          ctx.groupRoute
            ? ctx.groupRoute(
                ctx.state?.groups?.get(session.groupKey),
                session.id,
              )
            : `/host/local/group/${encodeURIComponent(session.groupKey || "default")}/s/${session.id}`,
        ),
    });
    return { update: () => handle.update(), cleanup: () => handle.unmount() };
  };
})();
