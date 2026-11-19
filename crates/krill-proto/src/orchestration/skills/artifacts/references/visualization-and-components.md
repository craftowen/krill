# Data Visualization & Sandbox-Safe Dynamic Components

This reference governs how charts, diagrams, tables, and interactive widgets are
built for Krill HTML artifacts (`share.krill.page`, max 10 MiB, `Origin: null` CSP
sandbox).

---

## 1. Perceptual Hierarchy & Chart Selection (Cleveland-McGill / Munzner / VisEval)

Humans decode quantitative differences most accurately from **position along a
common baseline**, followed by **length**, **slope**, and much less accurately from
**area**, **angle**, or **color saturation**. LLM chart benchmarks (`VisEval`) show
that models frequently fail on aggregation accuracy, sorting, label clipping, and
axis/legend clarity even when code runs without syntax errors.

| Analytical Task | Primary Idiom | Avoid (High Perceptual Error) | Required Details |
|---|---|---|---|
| Compare magnitudes across categories | **Sorted horizontal bar chart** or **table with inline proportion bars** | Pie/donut (>4 slices), unsorted vertical bars with rotated labels | Sort by value (unless natural order exists); place exact numbers at bar ends |
| Show change or trend over time | **Line chart** (or `uPlot` / `Lightweight Charts` for dense series) | 3D area, dual Y-axes with different scales | Direct end-of-line series labels when `<= 4` series; annotate key inflection points |
| Compare distributions | **Histogram**, **box/quantile plot**, or **strip/dot plot** | Bar chart of means hiding variance | Show sample count `n` and median/IQR |
| Compare across 2+ dimensions/cohorts | **Small multiples** sharing fixed axes | Radar/spider chart, >5-segment stacked bars | Lock Y-axis domain across panels so slopes compare honestly |
| Show architecture, sequence, or flow | **Clean inline SVG** (`<= 10` nodes) or `Cytoscape.js` / `Mermaid` (complex) | ASCII boxes in HTML reports, 729 KiB `Mermaid` for a 4-box flow | Reference `var(--ka-fg)`, `var(--ka-surface)`, `var(--ka-border)`, `var(--ka-accent)` |
| Summarize a single headline metric | **KPI card** with baseline delta (`+14.2% ▲ vs last week`) | Standalone big number without context, semicircle gauge | Never encode positive/negative with green/red color alone |

**Coordinate honesty rule**: Never hand-write guessed `<path d="M...">` data curves
or arbitrary bar pixel widths. Compute widths/heights proportionally from the data
(`(value / max) * 100%` in JS or exact ratios) or render through a chart library.

---

## 2. Dynamic Component Selection Matrix (Measured `cdn.jsdelivr.net` Payloads)

All CDN URLs below return `Access-Control-Allow-Origin: *` and run inside
`share.krill.page`'s `Content-Security-Policy: sandbox allow-scripts ...` (`Origin:
null`). **Never touch `localStorage`, `sessionStorage`, `indexedDB`, or
`document.cookie`** without a `try { ... } catch {}` fallback.

### Tier 0 — Zero-Dependency Browser Standards (`0 KiB`, Always First Choice)

- **Exclusive accordions**: `<details name="group-id"><summary>...</summary>...</details>`
- **Accessible modals**: `<dialog id="d">...</dialog>` (`d.showModal()` works in
  `Origin: null` regardless of `allow-modals`)
- **Light-dismiss popovers**: `<button popovertarget="p">Info</button><div id="p" popover>...</div>`
- **Inline data-ink bar tables**: HTML `<table>` with `tabular-nums` + a percentage
  fill `<div>` inside metric cells

### Tier 1 — Ultra-Light Reactivity & Fast Charts (`< 75 KiB` gzipped)

- **Stateful calculators & multi-variant UI (`7.0 KiB` gzip total)**:
  Load **all three** UMD scripts (`preact.umd.js` alone does not include `useState`):
  ```html
  <script src="https://cdn.jsdelivr.net/npm/preact@10.26.4/dist/preact.umd.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/preact@10.26.4/hooks/dist/hooks.umd.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/htm@3.1.1/dist/htm.umd.js"></script>
  <script>
    const { h, render } = window.preact;
    const { useState, useMemo } = window.preactHooks;
    const html = window.htm.bind(h);
  </script>
  ```
- **Declarative markup filters & toggles (`15.8 KiB` gzip)**:
  ```html
  <script defer src="https://cdn.jsdelivr.net/npm/alpinejs@3.14.8/dist/cdn.min.js"></script>
  ```
- **Dense time-series & benchmark telemetry (`21.9 KiB` gzip)**:
  ```html
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/uplot@1.6.31/dist/uPlot.min.css">
  <script src="https://cdn.jsdelivr.net/npm/uplot@1.6.31/dist/uPlot.iife.min.js"></script>
  ```
- **Financial / candlestick / crosshair time-series (`53.6 KiB` gzip)**:
  ```html
  <script src="https://cdn.jsdelivr.net/npm/lightweight-charts@5.0.0/dist/lightweight-charts.standalone.production.js"></script>
  ```
- **General-purpose responsive charts (`68.5 KiB` gzip)**:
  ```html
  <script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.8/dist/chart.umd.min.js"></script>
  ```

### Tier 2 — Rich Statistical Plots, Data Grids, Math & Graphs (`75–330 KiB` gzipped)

- **Equations (`78.0 KiB` gzip; use native `<math>` MathML for 1–2 simple formulas)**:
  ```html
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/katex@0.16.21/dist/katex.min.css">
  <script src="https://cdn.jsdelivr.net/npm/katex@0.16.21/dist/katex.min.js"></script>
  ```
- **Interactive data grid for 50–10,000 rows (`102.2 KiB` gzip — 3.6x lighter than AG Grid)**:
  ```html
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/tabulator-tables@6.3.1/dist/css/tabulator_simple.min.css">
  <script src="https://cdn.jsdelivr.net/npm/tabulator-tables@6.3.1/dist/js/tabulator.min.js"></script>
  ```
- **Interactive network / dependency DAG exploration (`125.9 KiB` gzip)**:
  ```html
  <script src="https://cdn.jsdelivr.net/npm/cytoscape@3.31.0/dist/cytoscape.min.js"></script>
  ```
- **Faceted statistical graphics & small multiples (`157.7 KiB` gzip)**:
  ```html
  <script src="https://cdn.jsdelivr.net/npm/d3@7.9.0/dist/d3.min.js"></script>
  <script src="https://cdn.jsdelivr.net/npm/@observablehq/plot@0.6.17/dist/plot.umd.min.js"></script>
  ```
- **Sankey, heatmaps, treemaps & linked brush dashboards (`327.7 KiB` gzip)**:
  ```html
  <script src="https://cdn.jsdelivr.net/npm/echarts@5.6.0/dist/echarts.min.js"></script>
  ```
- **Complex multi-diagram specs (`Mermaid 11.4.1`, `728.5 KiB` gzip — conditional only)**:
  Only load `https://cdn.jsdelivr.net/npm/mermaid@11.4.1/dist/mermaid.min.js` when a
  document contains `>= 3` complex sequence/state/ER diagrams. For 1–2 architecture
  or flow diagrams, write clean inline `<svg viewBox="...">` styled with `var(--ka-*)`.

---

## 3. Data-Snapshot & Theme-Sync Pattern

1. **Embed static JSON snapshots** (Observable Framework pattern) so pages render
   instantly and still display readable tables if a CDN is unreachable:
   ```html
   <script id="artifact-data" type="application/json">
     [{"step":"parse","ms":4.2},{"step":"compress","ms":1.8}]
   </script>
   ```
2. **Sync Canvas charts (`Chart.js`, `uPlot`, `ECharts`) with KADS dark/light mode**:
   Read computed colors from `getComputedStyle(document.documentElement)` (or pass
   explicit light/dark palettes on `matchMedia('(prefers-color-scheme: dark)')` and
   theme radio `change` events) so chart axes and labels never turn invisible on
   dark surfaces.
