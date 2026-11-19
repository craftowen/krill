//! Krill 기본 아티팩트 디자인 시스템(KADS) 스캐폴딩(`krill artifacts init`).
//!
//! 네트워크·공개 게이트 없이 단일 완결형 HTML(5개 아키타입) 또는 Markdown 골격을 생성한다.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use krill_proto::artifacts::{Archetype, ArtifactTheme, InitOutcome};

use super::{Error, Opts};

const KADS_BASE_CSS: &str = r#"  :root {
    color-scheme: light dark;
    --ka-bg: light-dark(#f7f4ee, #1a1b26);
    --ka-surface: light-dark(#fbf9f4, #24283b);
    --ka-muted-bg: light-dark(#ece6da, #292e42);
    --ka-fg: light-dark(#1f1d1a, #e0e6ff);
    --ka-muted: light-dark(#5f5a50, #b4beea);
    --ka-border: light-dark(#e2dccf, #3b4261);
    --ka-accent: light-dark(#0369a1, #7dcfff);
    --ka-accent-tint: light-dark(rgba(3, 105, 161, 0.10), rgba(125, 207, 255, 0.14));
    --ka-pos: light-dark(#1b7240, #9ece6a);
    --ka-warn: light-dark(#905403, #e0af68);
    --ka-neg: light-dark(#c62828, #f7768e);
    --ka-viz-1: light-dark(#0369a1, #7dcfff);
    --ka-viz-2: light-dark(#c2410c, #ff9e64);
    --ka-viz-3: light-dark(#1f7a45, #9ece6a);
    --ka-viz-4: light-dark(#6d28d9, #bb9af7);
    --ka-viz-5: light-dark(#a16207, #e0af68);
    --ka-viz-6: light-dark(#be185d, #f7768e);
    --ka-font-sans: "Pretendard Variable", Pretendard, -apple-system, BlinkMacSystemFont, "Apple SD Gothic Neo", "Segoe UI", system-ui, sans-serif;
    --ka-font-mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
    --ka-body-w: 68ch;
    --ka-note-w: 220px;
    --ka-r-xs: 4px;
    --ka-r-sm: 6px;
    --ka-r-md: 8px;
  }
  html[data-ka-theme="light"] {
    --ka-bg: light-dark(#ffffff, #0b0d13);
    --ka-surface: light-dark(#ffffff, #161a24);
    --ka-muted-bg: light-dark(#f3f4f7, #1f2430);
    --ka-fg: light-dark(#0a0a0a, #fafafa);
    --ka-muted: light-dark(#646b78, #9aa3b5);
    --ka-border: light-dark(#e4e7ec, rgba(255, 255, 255, 0.10));
    --ka-accent: light-dark(#0369a1, #7dd3fc);
  }
  html:has(#ka-mode-light:checked) { color-scheme: light; }
  html:has(#ka-mode-dark:checked) { color-scheme: dark; }
  *, *::before, *::after { box-sizing: border-box; }
  html {
    background: var(--ka-bg);
    color: var(--ka-fg);
    font-family: var(--ka-font-sans);
    font-size: 16px;
    line-height: 1.68;
    -webkit-font-smoothing: antialiased;
    text-rendering: optimizeLegibility;
  }
  body { margin: 0; padding: 0 0 72px; word-break: keep-all; overflow-wrap: break-word; }
  .ka-topbar {
    position: sticky; top: 0; z-index: 10;
    background: var(--ka-bg); border-bottom: 1px solid var(--ka-border); padding: 10px 24px;
  }
  .ka-topbar-inner {
    max-width: 1040px; margin: 0 auto;
    display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap;
  }
  .ka-meta-label { font-size: 0.8125rem; color: var(--ka-muted); font-weight: 600; }
  .ka-mode-switch {
    display: inline-flex; gap: 2px; padding: 2px;
    border: 1px solid var(--ka-border); border-radius: var(--ka-r-sm); background: var(--ka-surface);
  }
  .ka-mode-switch input { position: absolute; opacity: 0; pointer-events: none; }
  .ka-mode-switch label {
    font-size: 0.75rem; padding: 4px 10px; border-radius: var(--ka-r-xs);
    cursor: pointer; color: var(--ka-muted); user-select: none;
  }
  .ka-mode-switch input:checked + label {
    background: var(--ka-muted-bg); color: var(--ka-fg); font-weight: 600;
  }
  .ka-mode-switch input:focus-visible + label, button:focus-visible, summary:focus-visible {
    outline: 2px solid var(--ka-accent); outline-offset: 2px;
  }
  main.ka-shell { max-width: 1040px; margin: 0 auto; padding: 36px 24px 0; }
  h1 { font-size: clamp(1.6rem, 2.6vw, 2.05rem); line-height: 1.28; letter-spacing: -0.02em; margin: 0 0 12px; }
  h2 { font-size: 1.28rem; line-height: 1.35; margin: 0 0 12px; letter-spacing: -0.01em; }
  h3 { font-size: 1.02rem; line-height: 1.4; margin: 20px 0 8px; }
  p { margin: 0 0 14px; }
  a { color: var(--ka-accent); text-underline-offset: 3px; }
  code { font-family: var(--ka-font-mono); font-size: 0.86em; background: var(--ka-muted-bg); padding: 2px 5px; border-radius: var(--ka-r-xs); }
  pre {
    background: var(--ka-surface); border: 1px solid var(--ka-border); border-radius: var(--ka-r-md);
    padding: 14px 16px; overflow-x: auto; font-family: var(--ka-font-mono); font-size: 0.82rem; line-height: 1.55;
  }
  pre code { background: transparent; padding: 0; }
  .ka-bluf {
    background: var(--ka-surface); border: 1px solid var(--ka-border); border-left: 3px solid var(--ka-accent);
    border-radius: var(--ka-r-md); padding: 16px 18px; max-width: var(--ka-body-w); margin: 0 0 28px;
  }
  .ka-bluf-label { display: block; font-size: 0.8125rem; font-weight: 700; color: var(--ka-accent); margin-bottom: 4px; }
  .ka-sec { margin-top: 40px; padding-top: 24px; border-top: 1px solid var(--ka-border); }
  .ka-editorial {
    display: grid; grid-template-columns: minmax(0, var(--ka-body-w)) minmax(180px, var(--ka-note-w));
    column-gap: 32px; align-items: start;
  }
  .ka-editorial > * { grid-column: 1; }
  .ka-editorial > aside.ka-sidenote {
    grid-column: 2; font-size: 0.8125rem; line-height: 1.5; color: var(--ka-muted);
    border-left: 2px solid var(--ka-border); padding-left: 12px;
  }
  .ka-wide { grid-column: 1 / -1; margin: 16px 0; }
  .ka-kpi-strip { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin: 0 0 28px; }
  .ka-kpi { background: var(--ka-surface); border: 1px solid var(--ka-border); border-radius: var(--ka-r-md); padding: 14px 16px; }
  .ka-kpi-label { font-size: 0.78rem; color: var(--ka-muted); }
  .ka-kpi-val { font-size: 1.4rem; font-weight: 700; font-variant-numeric: tabular-nums; margin-top: 4px; }
  .ka-kpi-ctx { font-size: 0.78rem; color: var(--ka-muted); margin-top: 6px; font-variant-numeric: tabular-nums; }
  .ka-pos { color: var(--ka-pos); font-weight: 600; }
  .ka-warn { color: var(--ka-warn); font-weight: 600; }
  .ka-neg { color: var(--ka-neg); font-weight: 600; }
  .ka-table-wrap { overflow-x: auto; border: 1px solid var(--ka-border); border-radius: var(--ka-r-md); background: var(--ka-surface); }
  table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
  th, td { padding: 10px 12px; border-bottom: 1px solid var(--ka-border); text-align: left; vertical-align: top; }
  th { font-size: 0.78rem; font-weight: 600; color: var(--ka-muted); background: var(--ka-muted-bg); }
  tr:last-child td { border-bottom: none; }
  td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; font-family: var(--ka-font-mono); white-space: nowrap; }
  .ka-panel { background: var(--ka-surface); border: 1px solid var(--ka-border); border-radius: var(--ka-r-md); padding: 16px 18px; }
  .ka-btn {
    background: var(--ka-bg); color: var(--ka-fg); border: 1px solid var(--ka-border);
    border-radius: var(--ka-r-sm); padding: 6px 12px; font: inherit; font-size: 0.8125rem; cursor: pointer;
  }
  .ka-btn[aria-selected="true"], .ka-btn[aria-pressed="true"] {
    background: var(--ka-accent-tint); border-color: var(--ka-accent); color: var(--ka-accent); font-weight: 600;
  }
  details.ka-det { background: var(--ka-surface); border: 1px solid var(--ka-border); border-radius: var(--ka-r-sm); padding: 0 14px; margin-bottom: 8px; }
  details.ka-det summary { padding: 10px 0; cursor: pointer; font-weight: 600; font-size: 0.875rem; }
  @media (max-width: 900px) {
    .ka-editorial { grid-template-columns: 1fr; }
    .ka-editorial > aside.ka-sidenote { grid-column: 1; margin: 6px 0 14px; background: var(--ka-surface); padding: 10px 12px; border-radius: var(--ka-r-xs); }
  }
  @media print {
    :root { color-scheme: light; }
    .ka-topbar, .no-print { display: none !important; }
    body { padding: 0; font-size: 10.5pt; }
    main.ka-shell { max-width: 100%; padding: 0; }
    pre, .ka-panel, .ka-table-wrap { break-inside: avoid; }
  }"#;

pub(super) fn init(file: &str, opts: &Opts) -> Result<String, Error> {
    let path = Path::new(file);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(ext.as_str(), "html" | "htm" | "md" | "markdown") {
        return Err(Error::coded(
            "unsupported",
            format!("{file}: only .html, .htm, .md, .markdown files can be initialized"),
        ));
    }
    let arch = opts.archetype.unwrap_or_default();
    let chosen_theme = opts.theme.unwrap_or_else(|| arch.default_theme());
    let resolved_title = opts
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| default_title_from_path(path), str::to_owned);

    let body = if matches!(ext.as_str(), "md" | "markdown") {
        render_markdown(&resolved_title, arch)
    } else {
        render_html(&resolved_title, arch, chosen_theme)
    };

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut open_opts = OpenOptions::new();
    open_opts.write(true);
    if opts.force {
        open_opts.create(true).truncate(true);
    } else {
        open_opts.create_new(true);
    }
    let mut out = open_opts.open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            Error::coded(
                "already_exists",
                format!("{file} already exists; pass --force to overwrite"),
            )
        } else {
            Error::from(e)
        }
    })?;
    out.write_all(body.as_bytes())?;
    out.sync_all()?;
    drop(out);

    let full_path = super::canonical(file)?;
    let outcome = InitOutcome {
        path: full_path.clone(),
        archetype: arch,
        theme: chosen_theme,
        byte_size: u64::try_from(body.len()).unwrap_or(u64::MAX),
    };
    if opts.json {
        serde_json::to_string(&outcome).map_err(|e| Error::Fail(e.to_string()))
    } else {
        Ok(full_path)
    }
}

fn default_title_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.replace(['-', '_'], " ").trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Untitled Artifact".into())
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_markdown(title: &str, arch: Archetype) -> String {
    if arch == Archetype::Brief {
        format!(
            "# {title}\n\n\
             **핵심 결론:** 독자가 첫 문장에서 결정 사항과 수치 변화를 파악할 수 있도록 한 문장 결론을 적는다.\n\n\
             ## 주요 지표 변화\n\n\
             | 지표 | 현재값 | 기준 대비 변화 | 비고 |\n\
             |---|---:|---:|---|\n\
             | 응답 지연 (p99) | 28 ms | -41.7% (▼ 20 ms) | 실측 기준 명시 |\n\n\
             ## 다음 실행 항목\n\n\
             - [ ] 담당자와 완료 기준이 드러나는 동사형 할 일을 적는다.\n"
        )
    } else {
        format!(
            "# {title}\n\n\
             **요약:** 이 문서가 답하는 질문과 검증된 결론을 2~3문장으로 먼저 제시한다.\n\n\
             ## 1. 결론형 소제목을 적어 독자가 제목만 읽어도 논지를 따라가게 한다\n\n\
             사실과 측정 수치를 중심으로 서술하고, 정량 근거 없는 추상 수식절은 생략한다.\n\n\
             | 비교 항목 | 대안 A | 대안 B | 판정 근거 |\n\
             |---|---:|---:|---|\n\
             | 상주 메모리 | 42 MB | 96 MB | 동일 워크로드 실측 |\n"
        )
    }
}

fn render_html(title: &str, arch: Archetype, theme: ArtifactTheme) -> String {
    let safe_title = escape_html(title);
    let theme_attr = match theme {
        ArtifactTheme::Paper => "",
        ArtifactTheme::Light => " data-ka-theme=\"light\"",
    };
    let body = match arch {
        Archetype::Deck => return render_deck_html(&safe_title, theme_attr),
        Archetype::Report => render_report_body(&safe_title),
        Archetype::Brief => render_brief_body(&safe_title),
        Archetype::Explorer => render_explorer_body(&safe_title),
        Archetype::Variants => render_variants_body(&safe_title),
    };
    format!(
        "<!doctype html>\n\
         <html lang=\"ko\"{theme_attr}>\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{safe_title}</title>\n\
         <style>\n\
         {KADS_BASE_CSS}\n\
         </style>\n\
         </head>\n\
         <body>\n\
         <div class=\"ka-topbar\">\n\
           <div class=\"ka-topbar-inner\">\n\
             <span class=\"ka-meta-label\">{safe_title}</span>\n\
             <div class=\"ka-mode-switch\" role=\"radiogroup\" aria-label=\"테마 선택\">\n\
               <input type=\"radio\" name=\"ka-mode\" id=\"ka-mode-auto\" checked>\n\
               <label for=\"ka-mode-auto\">시스템</label>\n\
               <input type=\"radio\" name=\"ka-mode\" id=\"ka-mode-light\">\n\
               <label for=\"ka-mode-light\">라이트</label>\n\
               <input type=\"radio\" name=\"ka-mode\" id=\"ka-mode-dark\">\n\
               <label for=\"ka-mode-dark\">다크</label>\n\
             </div>\n\
           </div>\n\
         </div>\n\
         <main class=\"ka-shell\">\n\
         {body}\n\
         </main>\n\
         </body>\n\
         </html>\n"
    )
}

fn render_report_body(safe_title: &str) -> String {
    format!(
        "  <header style=\"max-width:var(--ka-body-w);margin-bottom:28px\">\n\
             <h1>{safe_title}</h1>\n\
             <p style=\"color:var(--ka-muted);margin:0\">작성 기준과 측정 환경을 한 줄로 명시한다.</p>\n\
           </header>\n\
           <div class=\"ka-bluf\">\n\
             <span class=\"ka-bluf-label\">핵심 결론 (BLUF)</span>\n\
             <p style=\"margin:0\">독자가 첫 문단에서 핵심 판단과 정량 근거를 바로 확인하도록 결론을 먼저 적는다.</p>\n\
           </div>\n\
           <section class=\"ka-sec\">\n\
             <div class=\"ka-editorial\">\n\
               <h2>1. 결론이 드러나는 문장형 제목을 적는다</h2>\n\
               <p>본문은 68ch 폭 안에서 사실과 수치 중심으로 서술한다. 보충 출처나 예외 조건은 우측 여백 주석으로 분리해 읽기 흐름을 지킨다.</p>\n\
               <aside class=\"ka-sidenote\">여백 주석: 측정 조건, 원전 출처, 용어 정의를 본문 문단 옆에 배치한다.</aside>\n\
               <div class=\"ka-wide ka-table-wrap\">\n\
                 <table>\n\
                   <thead><tr><th>항목</th><th class=\"num\">변경 전</th><th class=\"num\">변경 후</th><th class=\"num\">변화량</th></tr></thead>\n\
                   <tbody>\n\
                     <tr><td>예시 지표</td><td class=\"num\">120.0 ms</td><td class=\"num\">48.0 ms</td><td class=\"num ka-pos\">-60.0% ▼</td></tr>\n\
                   </tbody>\n\
                 </table>\n\
               </div>\n\
             </div>\n\
           </section>"
    )
}

fn render_brief_body(safe_title: &str) -> String {
    format!(
        "  <header style=\"max-width:var(--ka-body-w);margin-bottom:24px\">\n\
             <h1>{safe_title}</h1>\n\
           </header>\n\
           <div class=\"ka-bluf\">\n\
             <span class=\"ka-bluf-label\">한 줄 요약</span>\n\
             <p style=\"margin:0\">의사결정자가 30초 안에 파악해야 할 핵심 결론과 요청 사항을 적는다.</p>\n\
           </div>\n\
           <div class=\"ka-kpi-strip\">\n\
             <div class=\"ka-kpi\">\n\
               <div class=\"ka-kpi-label\">핵심 지표 A</div>\n\
               <div class=\"ka-kpi-val\">42.0 MB</div>\n\
               <div class=\"ka-kpi-ctx\"><span class=\"ka-pos\">▼ -30.0%</span> vs 목표 상한 60 MB</div>\n\
             </div>\n\
             <div class=\"ka-kpi\">\n\
               <div class=\"ka-kpi-label\">핵심 지표 B</div>\n\
               <div class=\"ka-kpi-val\">99.4%</div>\n\
               <div class=\"ka-kpi-ctx\"><span class=\"ka-pos\">▲ +1.2%p</span> vs 지난 주</div>\n\
             </div>\n\
           </div>\n\
           <section class=\"ka-sec\">\n\
             <h2>결정 및 후속 실행 항목</h2>\n\
             <div class=\"ka-table-wrap\">\n\
               <table>\n\
                 <thead><tr><th>할 일</th><th>담당</th><th class=\"num\">기한</th></tr></thead>\n\
                 <tbody><tr><td>구체적인 동사로 시작하는 실행 항목을 적는다</td><td>담당자</td><td class=\"num\">2026-11-20</td></tr></tbody>\n\
               </table>\n\
             </div>\n\
           </section>"
    )
}

fn render_explorer_body(safe_title: &str) -> String {
    format!(
        "  <header style=\"margin-bottom:24px\">\n\
             <h1>{safe_title}</h1>\n\
             <p style=\"color:var(--ka-muted);max-width:var(--ka-body-w)\">정적 데이터 스냅샷을 내장해 네트워크 요청이나 스토리지 접근 없이 즉시 필터링·재계산한다.</p>\n\
           </header>\n\
           <div class=\"ka-panel\">\n\
             <div style=\"display:flex;flex-wrap:wrap;gap:12px;align-items:center;justify-content:space-between;margin-bottom:16px\">\n\
               <label for=\"ka-limit\" style=\"font-size:0.875rem\">상한 기준선: <output id=\"ka-limit-out\" style=\"font-family:var(--ka-font-mono);font-weight:700\">60</output> ms\n\
                 <input id=\"ka-limit\" type=\"range\" min=\"10\" max=\"120\" value=\"60\" style=\"vertical-align:middle;margin-left:8px\">\n\
               </label>\n\
               <span id=\"ka-summary\" style=\"font-size:0.8125rem;color:var(--ka-muted)\"></span>\n\
             </div>\n\
             <div id=\"ka-chart\" style=\"display:flex;flex-direction:column;gap:8px\"></div>\n\
           </div>\n\
           <script id=\"artifact-data\" type=\"application/json\">\n\
             [{{\"name\":\"단계 A\",\"ms\":24}},{{\"name\":\"단계 B\",\"ms\":52}},{{\"name\":\"단계 C\",\"ms\":84}}]\n\
           </script>\n\
           <script>\n\
           (function () {{\n\
             var rows = JSON.parse(document.getElementById('artifact-data').textContent);\n\
             var max = rows.reduce(function (m, r) {{ return Math.max(m, r.ms); }}, 1);\n\
             var input = document.getElementById('ka-limit');\n\
             var out = document.getElementById('ka-limit-out');\n\
             var summary = document.getElementById('ka-summary');\n\
             var chart = document.getElementById('ka-chart');\n\
             function render() {{\n\
               var limit = Number(input.value);\n\
               out.textContent = String(limit);\n\
               var over = 0;\n\
               chart.innerHTML = '';\n\
               rows.forEach(function (r) {{\n\
                 if (r.ms > limit) over += 1;\n\
                 var line = document.createElement('div');\n\
                 line.style.cssText = 'display:grid;grid-template-columns:140px 1fr 80px;gap:10px;align-items:center;font-size:0.85rem';\n\
                 var label = document.createElement('span');\n\
                 label.textContent = r.name;\n\
                 var track = document.createElement('div');\n\
                 track.style.cssText = 'height:16px;background:var(--ka-muted-bg);border-radius:3px;overflow:hidden';\n\
                 var bar = document.createElement('div');\n\
                 bar.style.height = '100%';\n\
                 bar.style.width = ((r.ms / max) * 100).toFixed(1) + '%';\n\
                 bar.style.background = r.ms > limit ? 'var(--ka-neg)' : 'var(--ka-viz-1)';\n\
                 track.appendChild(bar);\n\
                 var val = document.createElement('span');\n\
                 val.style.cssText = 'text-align:right;font-family:var(--ka-font-mono);font-variant-numeric:tabular-nums';\n\
                 val.textContent = r.ms + ' ms';\n\
                 line.appendChild(label); line.appendChild(track); line.appendChild(val);\n\
                 chart.appendChild(line);\n\
               }});\n\
               summary.textContent = '기준 초과: ' + over + '건 / 전체 ' + rows.length + '건';\n\
             }}\n\
             input.addEventListener('input', render);\n\
             render();\n\
           }})();\n\
           </script>"
    )
}

fn render_variants_body(safe_title: &str) -> String {
    format!(
        "  <header style=\"margin-bottom:20px\">\n\
             <h1>{safe_title}</h1>\n\
             <p style=\"color:var(--ka-muted);max-width:var(--ka-body-w)\">동일한 데이터 모델을 공유하는 서로 다른 정보 구조 시안을 눌러 비교하고 하나를 확정한다.</p>\n\
           </header>\n\
           <div role=\"tablist\" aria-label=\"시안 선택\" style=\"display:flex;gap:8px;flex-wrap:wrap;margin-bottom:14px\">\n\
             <button type=\"button\" class=\"ka-btn\" role=\"tab\" aria-selected=\"true\" data-variant=\"a\">시안 A: 고밀도 표</button>\n\
             <button type=\"button\" class=\"ka-btn\" role=\"tab\" aria-selected=\"false\" data-variant=\"b\">시안 B: 상태 보드</button>\n\
           </div>\n\
           <div class=\"ka-panel\" id=\"ka-stage\"></div>\n\
           <section class=\"ka-sec\">\n\
             <h2>시안 비교표</h2>\n\
             <div class=\"ka-table-wrap\">\n\
               <table>\n\
                 <thead><tr><th>시안</th><th>잘 맞는 상황</th><th>약점</th><th>구현 비용</th></tr></thead>\n\
                 <tbody>\n\
                   <tr><td>시안 A</td><td>수십 건을 한 화면에서 대조할 때</td><td>모바일 폭에서 열 압축 필요</td><td>작음</td></tr>\n\
                   <tr><td>시안 B</td><td>진행 단계별 병목을 바로 볼 때</td><td>정밀 수치 비교에는 불리</td><td>중간</td></tr>\n\
                 </tbody>\n\
               </table>\n\
             </div>\n\
           </section>\n\
           <script>\n\
           (function () {{\n\
             var items = [\n\
               {{ title: '항목 1', status: '진행 중', owner: '팀 A' }},\n\
               {{ title: '항목 2', status: '검토 대기', owner: '팀 B' }}\n\
             ];\n\
             var stage = document.getElementById('ka-stage');\n\
             var tabs = Array.prototype.slice.call(document.querySelectorAll('[role=\"tab\"]'));\n\
             function show(key) {{\n\
               tabs.forEach(function (t) {{ t.setAttribute('aria-selected', String(t.getAttribute('data-variant') === key)); }});\n\
               if (key === 'a') {{\n\
                 stage.innerHTML = '<table><thead><tr><th>제목</th><th>상태</th><th>담당</th></tr></thead><tbody>' +\n\
                   items.map(function (i) {{ return '<tr><td>' + i.title + '</td><td>' + i.status + '</td><td>' + i.owner + '</td></tr>'; }}).join('') +\n\
                   '</tbody></table>';\n\
               }} else {{\n\
                 stage.innerHTML = '<div style=\"display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:12px\">' +\n\
                   items.map(function (i) {{ return '<div style=\"border:1px solid var(--ka-border);border-radius:6px;padding:12px\"><strong>' + i.status + '</strong><p style=\"margin:6px 0 0\">' + i.title + ' (' + i.owner + ')</p></div>'; }}).join('') +\n\
                   '</div>';\n\
               }}\n\
             }}\n\
             tabs.forEach(function (t) {{\n\
               t.addEventListener('click', function () {{ show(t.getAttribute('data-variant')); }});\n\
             }});\n\
             show('a');\n\
           }})();\n\
           </script>"
    )
}

fn render_deck_html(safe_title: &str, theme_attr: &str) -> String {
    format!(
        "<!doctype html>\n\
         <html lang=\"ko\"{theme_attr}>\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{safe_title}</title>\n\
         <style>\n\
         {KADS_BASE_CSS}\n\
           body {{ padding: 0; overflow: hidden; }}\n\
           .ka-deck {{ height: 100vh; display: grid; grid-template-rows: 1fr auto; }}\n\
           section.ka-slide {{ display: none; max-width: 1120px; width: 100%; margin: 0 auto; padding: 56px 64px; align-self: center; }}\n\
           section.ka-slide.active {{ display: block; }}\n\
           section.ka-slide h1 {{ font-size: clamp(1.8rem, 3.2vw, 2.5rem); margin-bottom: 24px; }}\n\
           section.ka-slide li {{ font-size: 1.25rem; line-height: 1.6; margin-bottom: 10px; }}\n\
           .ka-deck-bar {{ border-top: 1px solid var(--ka-border); padding: 8px 24px; display: flex; justify-content: space-between; align-items: center; font-size: 0.8rem; color: var(--ka-muted); font-variant-numeric: tabular-nums; }}\n\
           @media print {{\n\
             @page {{ size: 16in 9in; margin: 0; }}\n\
             body {{ overflow: visible; }}\n\
             .ka-deck {{ height: auto; display: block; }}\n\
             section.ka-slide {{ display: block !important; page-break-after: always; min-height: 9in; }}\n\
             .ka-deck-bar {{ display: none !important; }}\n\
           }}\n\
         </style>\n\
         </head>\n\
         <body>\n\
         <div class=\"ka-deck\">\n\
           <section class=\"ka-slide active\">\n\
             <h1>{safe_title}</h1>\n\
             <p style=\"font-size:1.3rem;color:var(--ka-muted)\">발표의 핵심 결론 한 문장을 표지 부제로 둔다.</p>\n\
           </section>\n\
           <section class=\"ka-slide\">\n\
             <h1>슬라이드마다 주제가 아니라 검증된 결론 문장을 제목으로 쓴다</h1>\n\
             <ul>\n\
               <li>한 슬라이드에는 하나의 메시지와 최대 4~5개 근거만 둔다.</li>\n\
               <li>모든 수치에는 비교 기준과 변화 방향(▲/▼)을 함께 표기한다.</li>\n\
             </ul>\n\
           </section>\n\
           <footer class=\"ka-deck-bar no-print\">\n\
             <span>방향키(←/→) 또는 Space로 이동</span>\n\
             <span id=\"ka-slide-num\">1 / 2</span>\n\
           </footer>\n\
         </div>\n\
         <script>\n\
         (function () {{\n\
           var slides = Array.prototype.slice.call(document.querySelectorAll('section.ka-slide'));\n\
           var num = document.getElementById('ka-slide-num');\n\
           var idx = 0;\n\
           function go(n) {{\n\
             idx = Math.max(0, Math.min(slides.length - 1, n));\n\
             slides.forEach(function (s, i) {{ s.classList.toggle('active', i === idx); }});\n\
             num.textContent = (idx + 1) + ' / ' + slides.length;\n\
           }}\n\
           document.addEventListener('keydown', function (e) {{\n\
             if (e.key === 'ArrowRight' || e.key === ' ') {{ e.preventDefault(); go(idx + 1); }}\n\
             else if (e.key === 'ArrowLeft') {{ e.preventDefault(); go(idx - 1); }}\n\
             else if (e.key === 'Home') {{ go(0); }}\n\
             else if (e.key === 'End') {{ go(slides.length - 1); }}\n\
           }});\n\
         }})();\n\
         </script>\n\
         </body>\n\
         </html>\n"
    )
}
