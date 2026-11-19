# Krill Artifact Design System (KADS)

KADS gives every HTML deliverable produced in Krill a cohesive, editorial visual
identity grounded in Krill's desktop `STYLEGUIDE.md` palettes while staying
zero-build, print-ready, responsive down to `360px`, and crash-free inside
`share.krill.page`'s `Origin: null` sandbox.

Core rule: **keep the token & layout system fixed; vary the structure and content to
fit the subject.** Do not invent ad-hoc hex values or decorative SaaS card chrome.

---

## 1. Semantic Color Tokens (`light-dark()`)

All foreground/surface combinations below are verified against WCAG 2.1 AA
(`>= 4.5:1` for text and direct series labels).

- **Editorial palette (`paper`, default for `report`, `brief`, `deck`)**: pairs
  warm off-white `Krill Paper` (`#f7f4ee`) with `Krill Tokyo Night` (`#1a1b26`) and
  Krill's `preserve` ocean blue accent (`#0369a1` / `#7dcfff`).
- **Studio palette (`light`, default for `explorer`, `variants`)**: pairs crisp
  neutral `Krill Light` (`#ffffff` / `#f3f4f7`) with `Krill Deep Sea` (`#0b0d13` /
  `#161a24`).

Do **not** register `--ka-*` tokens with `@property { syntax: "<color>"; }`:
unregistered custom properties let `light-dark()` re-evaluate dynamically when a
descendant or `html:has(#ka-theme-dark:checked)` overrides `color-scheme`.

```css
:root {
  color-scheme: light dark;

  /* Editorial default: Krill Paper (Light) + Tokyo Night (Dark) */
  --ka-bg: light-dark(#f7f4ee, #1a1b26);
  --ka-surface: light-dark(#fbf9f4, #24283b);
  --ka-muted-bg: light-dark(#ece6da, #292e42);
  --ka-fg: light-dark(#1f1d1a, #e0e6ff);
  --ka-muted: light-dark(#5f5a50, #b4beea);
  --ka-border: light-dark(#e2dccf, #3b4261);
  --ka-accent: light-dark(#0369a1, #7dcfff);
  --ka-accent-tint: light-dark(rgba(3, 105, 161, 0.10), rgba(125, 207, 255, 0.14));

  /* Semantic status & delta tokens (always pair with +/- or arrows) */
  --ka-pos: light-dark(#1b7240, #9ece6a);
  --ka-warn: light-dark(#905403, #e0af68);
  --ka-neg: light-dark(#c62828, #f7768e);

  /* 6-series colorblind-aware data visualization palette (>= 4.68:1 on surface) */
  --ka-viz-1: light-dark(#0369a1, #7dcfff);
  --ka-viz-2: light-dark(#c2410c, #ff9e64);
  --ka-viz-3: light-dark(#1f7a45, #9ece6a);
  --ka-viz-4: light-dark(#6d28d9, #bb9af7);
  --ka-viz-5: light-dark(#a16207, #e0af68);
  --ka-viz-6: light-dark(#be185d, #f7768e);

  /* Typography & rhythm */
  --ka-font-sans: "Pretendard Variable", Pretendard, -apple-system, BlinkMacSystemFont,
    "Apple SD Gothic Neo", "Segoe UI", system-ui, sans-serif;
  --ka-font-mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  --ka-body-w: 68ch;
  --ka-note-w: 220px;
  --ka-r-xs: 4px;
  --ka-r-sm: 6px;
  --ka-r-md: 8px;
}

/* Optional Studio override (--theme light) */
html[data-ka-theme="light"] {
  --ka-bg: light-dark(#ffffff, #0b0d13);
  --ka-surface: light-dark(#ffffff, #161a24);
  --ka-muted-bg: light-dark(#f3f4f7, #1f2430);
  --ka-fg: light-dark(#0a0a0a, #fafafa);
  --ka-muted: light-dark(#646b78, #9aa3b5);
  --ka-border: light-dark(#e4e7ec, rgba(255, 255, 255, 0.10));
  --ka-accent: light-dark(#0369a1, #7dd3fc);
}

/* Zero-JS, zero-localStorage theme switch (safe in Origin: null sandbox) */
html:has(#ka-mode-light:checked) { color-scheme: light; }
html:has(#ka-mode-dark:checked) { color-scheme: dark; }
```

---

## 2. Typography, Spacing, and Discipline

- **Measure & Korean line breaking**: constrain body prose to `max-width: 68ch`
  (`line-height: 1.65–1.72`) so eyes track comfortably. Set `word-break: keep-all;
  overflow-wrap: break-word;` on Korean prose so words never split mid-syllable.
- **Heading hierarchy**: use `h1` once for the document Action Title, `h2` for major
  sections, and `h3` for subsections. Following Tufte's rule, if you reach for `h4`
  or `h5`, flatten the hierarchy or convert the subsection into a table.
- **Discrete spacing grid**: use multiples of `4px` (`4, 8, 12, 16, 24, 32, 48`).
- **Numeric alignment**: apply `font-variant-numeric: tabular-nums` and right
  alignment (`text-align: right`) to every table metric, timestamp, and KPI figure.
- **Restraint**: reserve `--ka-accent` for the primary active control, links, BLUF
  left border, and the single focal series in a chart. Keep chrome quiet so data
  and prose lead.

---

## 3. Five Standard Archetypes

Scaffold any of these directly with `krill artifacts init <file.html> --archetype <name>`:

1. **`report` (Editorial Technical Report — Tufte/Distill hybrid)**
   - Grid: `grid-template-columns: minmax(0, 68ch) minmax(180px, 220px); column-gap: 32px;`.
   - Prose sits in column 1; concise citations, caveats, and definitions sit in
     `<aside class="ka-sidenote">` in column 2 adjacent to the paragraph they
     annotate (collapsing cleanly inline below `900px`).
   - Data tables and charts span both columns via `.ka-wide { grid-column: 1 / -1; }`.
2. **`brief` (1-Page Executive Brief / Recap)**
   - Single centered container (`max-width: 880px`).
   - Top metadata line -> 1-sentence BLUF callout -> 2-to-4 KPI cards (each with
     baseline delta) -> structured comparison table / flow / checklist. Keep bullets
     to at most 2 lines each for fast mobile scanning.
3. **`explorer` (Interactive Data App / Explorable Explanation)**
   - Embed precomputed data inside `<script id="artifact-data" type="application/json">`
     so initial render needs zero network fetches.
   - Sticky filter/slider toolbar at the top -> responsive chart stage -> sortable
     detail table. Every slider/input updates dependent numbers immediately.
4. **`variants` (Decision Mockups A–E)**
   - Top bar with problem statement + `role="tablist"` tabs (`A`..`E`, keyboard
     ArrowLeft/ArrowRight supported).
   - Each variant demonstrates a genuinely different information architecture
     (e.g., dense table vs kanban vs split master-detail vs action queue) over the
     **same shared JavaScript array**, followed by a brief yellow/tinted trade-off
     note and a bottom comparison matrix (`Best for`, `Weakness`, `Effort`).
5. **`deck` (16:9 Presentation Slides)**
   - Fixed `1280x720` logical canvas scaled via CSS transform/viewport fit, instant
     slide transitions (no dizzying animations), keyboard navigation (`ArrowRight`,
     `Space`, `ArrowLeft`, `Home`, `End`, `F` fullscreen), progress bar, and
     `@media print` `@page { size: 16in 9in; margin: 0; }` 1-slide-per-page export.

---

## 4. Mandatory Print & Mobile Resilience

Every HTML artifact must include:
- `<meta name="viewport" content="width=device-width, initial-scale=1">`
- Responsive collapse at `@media (max-width: 900px)` and horizontal scroll wrappers
  (`.ka-table-wrap { overflow-x: auto; }`) so `360px` mobile screens never clip.
- `@media print` rules forcing `:root { color-scheme: light; }`, hiding interactive
  chrome (`.no-print { display: none !important; }`), and setting `break-inside: avoid`
  on `<pre>`, `<figure>`, and `.ka-table-wrap`.
