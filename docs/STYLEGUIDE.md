# Krill UI 디자인 시스템 & 스타일 가이드 (STYLEGUIDE)

이 문서는 Krill(`crates/krill-app`)의 **시각 디자인·컴포넌트 선택·화면 전환·UX 규범**을 정의한다. 시스템 아키텍처는 [docs/ARCHITECTURE.md](ARCHITECTURE.md), 코드 규칙은 [docs/CONVENTIONS.md](CONVENTIONS.md)를 따른다.

근거 및 출처: Orca 레포지토리(`.ref/orca/docs/STYLEGUIDE.md`, `.ref/orca/src/renderer/src/assets/main.css`, `.ref/orca/src/renderer/src/components/ui/*`, `.ref/orca/src/shared/terminal-themes/defaults.ts`, Copyright (c) 2026 Lovecast Inc., MIT License)의 검증된 디자인 토큰·컴포넌트 문법·UX 원칙을 순수 Rust(`gpui` + `gpui-component`) 체계로 이식했다.

**Orca 패리티의 범위**: 레이아웃·치수·단축키·컴포넌트 문법은 Orca와 맞추는 것이 의도된 정책이다(Orca에서 넘어오는 사용자에게 익숙함이 곧 기능이다, `orca_parity_tokens` 테스트가 치수를 고정한다). **색·브랜드·카피 톤은 Krill 고유다** — 무채색 크롬의 심해 틴트, `preserve` 토큰, 코랄 브랜드 마크, 3분절 스피너, 떼 미터는 Orca에 없다.

---

## 1. 개요 및 시각 정체성 (Overview)

Krill은 여러 Git 워크트리와 폴더에 걸쳐 AI 코딩 에이전트를 끊김 없이 병렬 실행하는 macOS 데스크톱 앱이다. 앱이 죽거나 업데이트돼도 세션은 살아남는다. 시각적 정체성은 **심해의 고요 위 하나의 살아 있는 점**이다 — 크롬은 푸른 기 도는 심해 뉴트럴로 물러나고, 코랄은 Krill 자신(로고)에만, 상태색은 의미(에이전트 작업/질문/차단/완료, 파괴적 동작, Git 변경 장식)에만 쓰며, 세션 보존은 경고가 아닌 `preserve` 톤으로 보여 준다.

앱 화면의 대부분은 터미널 격자, Diff 뷰, 코드 에디터가 차지하므로 Krill 자체의 크롬은 뒤로 물러나 콘텐츠를 감싸는 액자 역할을 해야 한다.

확신이 서지 않을 때의 기본 원칙:
1. 유채색을 찾기 전에 **`muted` / `accent` / `border`**를 먼저 쓴다.
2. 16진수 색(`rgb(...)`, `hex(...)`)을 하드코딩하기 전에 **`crate::ui::palette(cx)`의 시맨틱 토큰**을 쓴다.
3. 커스텀 레이아웃을 새로 짜기 전에 **`crate::ui` 공용 프리미티브**와 **`gpui-component` 기본 위젯**을 먼저 쓴다.

---

## 2. 단일 진실 공급원 (Source of Truth)

| 영역 | Krill 정본 위치 (`crates/krill-app/src/`) | Orca 참조 원본 (`.ref/orca/`) |
|---|---|---|
| 색 토큰·타이포그래피·간격·반경·크롬 상수 | [`ui/theme.rs`](../crates/krill-app/src/ui/theme.rs) (`ui::*` 재노출) | `src/renderer/src/assets/main.css` (`:root`, `.dark`), `src/shared/terminal-themes/defaults.ts` |
| 공용 레이아웃·컴포넌트 프리미티브 | [`ui/primitives.rs`](../crates/krill-app/src/ui/primitives.rs) (`ui::*` 재노출) | `src/renderer/src/components/ui/*`, `components/settings/SettingsFormControls.tsx`, `components/ShortcutKeyCombo.tsx` |
| 공식 AI 프로바이더·에이전트·Lucide 아이콘 | [`ui/icons.rs`](../crates/krill-app/src/ui/icons.rs) | `src/renderer/src/components/status-bar/icons.tsx`, `src/renderer/src/lib/agent-icon-glyphs.tsx` |
| 에이전트 상태 글리프·스피너·워크트리 요약·계정 배지 | [`ui/status.rs`](../crates/krill-app/src/ui/status.rs) | `AgentStateDot.tsx`, `AgentWorkingSpinner.tsx`, `worktree-card-compact-agents.tsx` |
| UI 디자인 규칙·UX 리뷰 기준 | 이 문서 (`docs/STYLEGUIDE.md`) | `docs/STYLEGUIDE.md` |

- **금지**: `crates/krill-app/src/ui/`와 터미널 셀 렌더러(`src/terminal.rs`) 바깥에서 `gpui::rgb(...)`, `gpui::hsla(...)`, `gpui::black()`, `gpui::white()`, `gpui::transparent_black()`, `gpui::red()` / `green()` / `blue()` / `yellow()`, `ui::hex(...)`, `ui::hexa(...)`, 비수식 `hex(0x...)` / `hexa(0x...)`(임포트 우회)를 직접 호출하지 않는다. 그림자는 `.shadow_lg()`만 허용하고 `.shadow_xs()` / `.shadow_sm()` / `.shadow_md()` / `.shadow_xl()` / `.shadow_2xl()`을 쓰지 않는다.
- 새 시맨틱 색이나 치수가 필요하면 먼저 [`ui/theme.rs`](../crates/krill-app/src/ui/theme.rs)의 `Palette`(`LIGHT`와 `DARK` 양쪽 모두) 또는 상수에 추가하고 이 문서에 기록한 뒤 사용한다.

---

## 3. 색 역할 및 시맨틱 토큰 (`Palette`)

모든 표면(surface) 토큰은 그 위에서 명도 대비를 만족하는 전경(`*_foreground`) 토큰과 쌍을 이룬다. 항상 쌍으로 함께 사용한다.

### 3.1 `Palette` 시맨틱 토큰 표 (`crate::ui::palette(cx)`)

| 토큰 필드 | Light (`LIGHT`) | Dark (`DARK`) | 용도 (Use it for) | 금지 용도 (Don't use it for) |
|---|---|---|---|---|
| `background` / `foreground` | `#ffffff` / `#0a0a0a` | `#0b0d13` / `#fafafa` | 앱 기본 캔버스, 기본 본문 텍스트, 활성 탭 배경 | 카드, 팝오버, 사이드바(전용 토큰 사용) |
| `editor_surface` | `#ffffff` | `#1e1e1e` | 코드 에디터·마크다운 편집기 표면(다크 모드에서 캔버스보다 살짝 밝은 VS Code식 표면) | 일반 앱 크롬 |
| `card` / `card_foreground` | `#ffffff` / `#0a0a0a` | `#161a24` / `#fafafa` | 캔버스 위에 얹힌 설정 행·패널·그룹 상자(`ui::card`) | 기본 캔버스 자체 |
| `popover` / `popover_foreground` | `#ffffff` / `#0a0a0a` | `#161a24` / `#fafafa` | 떠 있는 메뉴, 드롭다운, Jump 팔레트, 중앙 모달 대화상자 | 인라인 고정 UI |
| `primary` / `primary_foreground` | `#171717` / `#fafafa` | `#e5e5e5` / `#171717` | 한 흐름에서 단 하나의 긍정 주 동작 버튼(저장, 생성, 확인) | 장식용 강조, 호버 배경, 보조 동작 |
| `secondary` / `secondary_foreground` | `#f3f4f7` / `#171717` | `#1f2430` / `#fafafa` | `primary` 옆에 놓이는 낮은 강조의 보조 버튼·보조 칩 | 주 긍정 동작 |
| `muted` / `muted_foreground` | `#f3f4f7` / `#646b78` | `#1f2430` / `#9aa3b5` | 보조 설명, 캡션, 플레이스홀더, 비활성 탭 글자, 키캡·코드 블록 배경 | 본문 텍스트, 주 동작 버튼 |
| `accent` / `accent_foreground` | `#eef0f4` / `#171717` | `#2a3040` / `#fafafa` | 고스트 버튼·목록 행·팔레트 항목의 호버/선택 배경 | 단색 채움 버튼(`secondary` 사용) |
| `destructive` / `destructive_foreground` | `#e40014` / `#fcf3f3` | `#ff6568` / `#fafafa` | 삭제·초기화·강제 종료 등 되돌릴 수 없는 파괴적 동작, 오류 배너/차단 상태 | 취소(Cancel)·닫기(Close) 버튼 |
| `border` | `#e4e7ec` | `rgba(255,255,255,0.08)` | 모든 1px 구분선: 카드 테두리, 섹션 구분선, 입력칸 외곽선 | 강한 포커스 강조(`ring` 사용) |
| `input` | `#e4e7ec` | `rgba(255,255,255,0.15)` | 입력 컨트롤(`Input`) 테두리·배경 | 폼 필드 바깥의 장식 |
| `ring` | `#9aa3b5` | `#5b6478` | 키보드 포커스 링, 활성 칸 강조 테두리 | 상시 고정 테두리 |
| `sidebar` / `sidebar_foreground` | `#f6f7f9` / `#0a0a0a` | `#12151d` / `#fafafa` | 좌측 사이드바·통합 타이틀바·탭 바 크롬 표면과 기본 글자 | 메인 캔버스나 모달 내부 |
| `sidebar_primary` / `sidebar_primary_foreground` | `#171717` / `#fafafa` | `#1447e6` / `#fafafa` | 사이드바 주요 강조점, Git 이름 변경(Renamed/Copied) 배지 | 일반 본문 |
| `sidebar_accent` / `sidebar_accent_foreground` | `#e9ecf1` / `#171717` | `#222836` / `#fafafa` | 사이드바 내비게이션·세션 행 호버/선택 배경 | 사이드바 바깥의 패널 |
| `sidebar_border` / `sidebar_ring` | `#e4e7ec` / `#9aa3b5` | `rgba(255,255,255,0.07)` / `#4b5366` | 사이드바 내부 구분선 및 포커스 링 | 분할선(`split_divider` 사용) |
| `worktree_sidebar` / `worktree_sidebar_foreground` | `#f1f3f6` / `#0a0a0a` | `#232836` / `#fafafa` | 워크트리 카드/그룹 표면(다크 모드에서 `#0b0d13` 캔버스와 구분되는 리프트 표면) | 일반 설정 카드 |
| `worktree_sidebar_accent` / `worktree_sidebar_border` | `#e6e9ef` / `#e4e7ec` | `#2e3445` / `rgba(255,255,255,0.07)` | 워크트리 행 호버·상태 요약 알약 배경 및 테두리 | 사이드바 외부 |
| `success` | `#15803d` | `#86efac` | 에이전트 완료(`Done`), Git 추가(`A`/`??`), 정상 연결 상태 | 일반 장식 |
| `warning` / `warning_foreground` | `#ca8a04` / `#fafafa` | `#eab308` / `#171717` | 에이전트 작업 중(`Working` 노란 스피너), 주의 배너, Git 수정(`M`) | 질문 대기(`agent_question` 사용) |
| `agent_question` | `#ea580c` | `#f97316` | 에이전트가 사용자에게 질문/입력 대기 중(`Waiting`, 주황색 — 작업 중 노란색과 한눈에 구분) | 일반 경고 |
| `preserve` | `#0369a1` | `#7dd3fc` | **Krill 고유.** 세션이 안전하게 보존·이어지는 중: 데몬 재연결 중(세션은 krill-holder가 보관), 탭 밖 분리 세션. 배경 틴트는 `.opacity(0.12)`, 테두리 `.opacity(0.3)` | 에이전트 상태(작업/질문/차단/완료), 조치가 필요한 경고·실패(`warning`·`destructive` 유지), 정상 연결(`success`) |
| `split_divider` / `split_divider_strong` | `#868690` / `#71717a` | `#71717a` / `#a1a1aa` | 터미널 분할선 기본 상태(`card` 대비 3:1 이상) 및 호버/드래그 강조 상태 | 일반 카드 테두리(`border` 사용) |

### 3.1.1 심해 틴트와 대비 기준

- 무채색 크롬(배경·사이드바·카드·팝오버·muted·accent·테두리·포커스 링)은 Orca의 순수 회색 대신 **Krill 심해 틴트**(남색 기, 색상각 약 220°)를 쓴다. 상태색(`success`·`warning`·`agent_question`·`destructive`), 탭 9색, Git 색, `primary`·`sidebar_primary`는 Orca 값 그대로다.
- **대비 회귀 테스트 (`text_tokens_meet_wcag_aa_on_every_surface`)**: 양 팔레트에서 `foreground`·`muted_foreground`가 `background`·`sidebar`·`card`·`popover`·`muted`·`accent`·`sidebar_accent`·`worktree_sidebar` 위에서 WCAG 대비 4.5:1 이상이고, `preserve`가 `background` 위 4.5:1 이상인지 계산해 단언한다.
- 최종 값 계산 결과(WCAG 상대휘도 공식, 2026-10-19):
  - 다크 `muted_foreground` `#9aa3b5`: background 7.66 · sidebar 7.20 · card/popover 6.86 · muted 6.12 · sidebar_accent 5.81 · worktree_sidebar 5.80 · accent 5.19.
  - 라이트 `muted_foreground` `#646b78`: background/card/popover 5.36 · sidebar 5.00 · muted 4.88 · worktree_sidebar 4.82 · accent 4.70 · sidebar_accent 4.53. (초안 `#666d7a`는 sidebar_accent 위 4.40이라 한 단계 어둡게 정했다.)
  - `foreground`: 다크 12.61(accent)~18.61(background), 라이트 16.72(sidebar_accent)~19.80(background).
  - `preserve`: 다크 `#7dd3fc` background 위 11.65, 라이트 `#0369a1` 흰 바탕 위 5.93.

### 3.2 특수 상태 상수 및 헬퍼 함수

- **`AGENT_UNREAD`** (`#f59e0b`, 호박색 `amber-500`): 미확인 완료 알림 벨·워크트리 미확인 점(`ui::status::unread_dot`).
- **`TRANSPARENT`** / **`ui::transparent()`**: 완전 투명(유휴 행 투명 테두리·겹침 마스크 등). `gpui::transparent_black()` 직접 호출 대신 사용한다.
- **`MODAL_BACKDROP`** (`rgba(0, 0, 0, 0.55)`): 창 중앙 모달 대화상자의 어두운 배경 스크림(`ui::modal_backdrop`).
- **에이전트 상태 색 (`ui::agent_state_color(state, cx)`)**:
  - `AgentState::Working` → `p.warning` (노랑 회전 스피너)
  - `AgentState::Waiting` → `p.agent_question` (주황 질문 아이콘)
  - `AgentState::Blocked` → `p.destructive` (빨강 승인/차단 질문 아이콘)
  - `AgentState::Done` → `p.success` (초록 체크 아이콘)
  - `AgentState::Exited | AgentState::Unknown` → `p.muted_foreground` (흐린 점)
- **Git 상태 색 (`ui::git_status_color(status, cx)`)**:
  - 추가/미추적 (`A`, `??`) → `p.success`
  - 수정 (`M`) → `p.warning`
  - 삭제/충돌 (`D`, `U`) → `p.destructive`
  - 이름 변경/복사 (`R`, `C`) → `p.sidebar_primary`
  - 그 외 → `p.muted_foreground`
  - **규칙**: `git_status_color`는 오직 Git 변경 상태 표시에만 쓴다.

### 3.3 목록 행(List Rows): 유휴·호버·선택·활성 규칙

사이드바 워크트리/세션 행, 설정 좌측 내비게이션, Jump 팔레트, 파일 트리 등 모든 목록 행은 아래 규칙을 따른다.

- **유휴(Idle)**: 투명 배경(부모 표면 색 그대로).
- **호버(Hover)**: 일반 패널/팝오버에서는 `p.accent`, 사이드바에서는 `p.sidebar_accent`(워크트리 묶음 내부는 `p.worktree_sidebar_accent`).
- **키보드 선택 / 현재 활성 행(Selected / Active)**: `p.accent` 또는 `p.sidebar_accent`를 사용한다. 임의의 `#ededed`나 `#333333` 같은 생 색상을 발명하지 않는다.
- **색상 틴트(Color Mixing)**: 강조 배경이나 옅은 테두리가 필요할 때는 새 hex를 만들지 않고 시맨틱 토큰에 `.opacity(...)`를 입힌다(예: 오류 배너 배경 `p.destructive.opacity(0.1)`, 테두리 `p.destructive.opacity(0.3)`).

### 3.4 터미널 팔레트 (`TERMINAL_DARK` / `TERMINAL_LIGHT`)

- 다크 모드 기본: `TERMINAL_DARK` = **`Krill Deep`** (배경 `#10131b`, 전경 `#ffffff`, 커서 코랄 `#ff5238`, ANSI 16색은 Orca `Ghostty Default Style Dark` 그대로). 데몬이 앱 테마를 받기 전 색 질의(OSC 10/11/12/4)에 답하는 `krill_proto::theme::DEFAULT`와 같은 값이다 — 두 값이 어긋나면 에이전트가 실제와 다른 배경을 보고 색을 고른다. 배경이 어두워 OSC 11 응답은 계속 다크로 판정된다.
- `TERMINAL_GHOSTTY_DARK`: Orca `Ghostty Default Style Dark`(배경 `#282c34`, 전경·커서 `#ffffff`). 내장 테마 목록 `TERMINAL_THEMES`에 선택지로 남긴다.
- 라이트 모드: `TERMINAL_LIGHT` (Orca `Builtin Tango Light`, 배경 `#ffffff`, 전경 `#000000`).
- 터미널 칸 여백은 셀 배경과 일치하도록 `ui::terminal_surface(theme)` 또는 투명도 설정(`50..=100%`)이 반영된 `ui::terminal_surface_alpha(theme, opacity_pct)`를 쓴다.

---

## 4. 타이포그래피 (Typography)

- **기본 UI 글꼴 (`FONT_UI`)**: `".SystemUIFont"` (`gpui-component`가 macOS에서 SF Pro `.AppleSystemUIFont`로 해석). 사이드바·탭 바·패널·대화상자·상태 바·설정 창 등 모든 UI 크롬에 사용한다.
- **고정폭 글꼴 (`FONT_MONO`)**: `"Menlo"`. 터미널 격자, 코드 에디터, Diff hunk, 파일 경로, SHA 해시, 단축키 키캡(`ui::keycap`), 수치 배지에만 사용한다.
- **글자 크기 단계 (Type Scale)**: 창의 `rem_size`(16px × UI 줌 배율)를 따라 비례 확대/축소되도록 반드시 아래 `Rems` 상수를 사용한다. UI 크롬에서 임의 픽셀 크기(`.text_size(px(..))`)를 직접 쓰지 않는다.

| 상수 | 기준 크기 (줌 100%) | Orca 대응 | 용도 |
|---|---|---|---|
| `TEXT_2XS` | `10px` (`0.625rem`) | `text-3xs` / `text-[10px]` | 배지, 카운터(`+N`), 미세 캡션, SHA 해시, 상태 알약 |
| `TEXT_XS` | `11px` (`0.6875rem`) | `text-2xs` / `text-[11px]` | 사이드바 섹션 머리글, 폼 설명문(`muted_foreground`), 키캡, 상태 바 |
| `TEXT_SM` | `12px` (`0.75rem`) | `text-xs` / `12px` | 설정 행 제목, 탭 라벨, 파일 경로, 부가 텍스트, 기본 고정폭 크기 |
| `TEXT_MD` | `13px` (`0.8125rem`) | `13px` | 사이드바 항목, 밀도 높은 목록 행, 앱 기본 글자 크기(`theme.font_size`) |
| `TEXT_LG` | `14px` (`0.875rem`) | `text-sm` / `14px` | 모달 대화상자 제목, 온보딩·관리자 페이지 주요 제목 |

---

## 5. 간격·모서리 반경·창 크롬·입체감 (Spacing, Radius, Chrome & Elevation)

### 5.1 간격 (`SPACE_*`, 4px 격자)

- `SPACE_1` = `4px`, `SPACE_2` = `8px`, `SPACE_3` = `12px`, `SPACE_4` = `16px`.

### 5.2 모서리 반경 (`RADIUS_*`, Orca `--radius: 10px` 기준)

| 상수 | 값 | 용도 |
|---|---|---|
| `RADIUS_XS` | `4px` | 단축키 키캡(`ui::keycap`), 상태 요약 칩, 인라인 코드 상자, Jump 팔레트 행 |
| `RADIUS_SM` | `6px` | 기본 버튼, 입력칸(`theme.radius`), 인라인 오류 배너(`ui::error_banner`), 드래그 탭 프리뷰 |
| `RADIUS_MD` | `8px` | 설정 카드·행(`ui::card`, `ui::row`) |
| `RADIUS_LG` | `10px` | 중앙 모달 대화상자(`worktree::Dialog`), Jump 팔레트 외곽(`theme.radius_lg`) |
| `RADIUS_XL` | `14px` | 대형 히어로/온보딩 카드 표면 |
| `rounded_full()` | `9999px` | 상태 점(`STATE_DOT`, `unread_dot`), 상태 알약 배지(`ui::badge`) |

### 5.3 창 크롬 및 레이아웃 정본 치수

- **상단 타이틀바 / 탭 바**: `TITLEBAR_HEIGHT = 36px`, `TAB_BAR_HEIGHT = 36px` (1단 통합 상단 바).
- **macOS 신호등 여백**: `TRAFFIC_LIGHT_INSET = (12px, 10px)`, 신호등 우측 첫 컨트롤 여백 `TRAFFIC_LIGHT_PAD = 78px` (이 영역 안에는 클릭 타깃을 두지 않는다).
- **탭 규칙**: `TAB_MAX_WIDTH = 220px`, 비활성 탭 아이콘·상태 점 투명도 `TAB_INACTIVE_OPACITY = 0.7`, 상태 점 지름 `STATE_DOT = 6px`.
- **사이드바·패널·상태바**:
  - `SIDEBAR_DEFAULT_WIDTH = 240pt`, `SIDEBAR_HEADER_HEIGHT = 32px`, `SIDEBAR_EMPTY_PY = 24px`
  - `SIDEBAR_ROW_HEIGHT = 32px` (사이드바 세션 행), `PANEL_ROW_HEIGHT = 26px` (우측 패널 파일·검색 행), `PANEL_TAB_HEIGHT = 24px` (우측 패널 탭), `PANE_HEADER_HEIGHT = 26px` (분할 창 헤더)
  - `PANEL_DEFAULT_WIDTH = 280pt`, `STATUS_BAR_HEIGHT = 24px`, `DIVIDER_HIT_PX = 5pt` (시각적 선은 1pt)
  - 상태 요약 겹친 아이콘: `STATUS_GROUP_ICON = 15px`, 겹침 음수 마진 `STATUS_GROUP_OVERLAP = -3px` (`ui::status::summary_element` 전용)
- **오버레이·보조 창·설정 레이아웃**:
  - 중앙 모달 대화상자 너비: `DIALOG_WIDTH = 512px` (창 폭이 좁으면 양옆 16px 여백 유지), 설정 내 넓은 모달 너비 `SETTINGS_MODAL_WIDTH = 640px`
  - 설정 화면 좌측 탐색 열 고정 너비: `SETTINGS_NAV_WIDTH = 248px`, 우측 본문 열 최대 너비 `SETTINGS_CONTENT_MAX_WIDTH = 1120px` (창 너비에 맞춰 유동 확장하되 초대형 모니터에서 과도한 행 길이 방지)
  - 토글 스위치(`ui::switch`) 치수: 트랙 `SWITCH_WIDTH = 36px` × `SWITCH_HEIGHT = 20px`, 썸 지름 `SWITCH_THUMB = 14px`
  - 상단 커맨드/Jump 팔레트 너비: `COMMAND_PALETTE_WIDTH = 520px`, 세션 히스토리 `AGENT_HISTORY_WIDTH = 520px` (`MAX_HEIGHT = 360px`)
  - 팝아웃 기본 크기: `FLOATING_SIZE = 640×400`, 대시보드 `DASHBOARD_SIZE = 760×480`

### 5.4 입체감과 그림자 (Elevation & Shadows)

Orca와 동일하게 그림자는 꼭 필요한 곳에만 3단계로 제한한다. 4번째 단계를 발명하지 않는다.

1. **Inset hairline (기본)**: `border_1()` + `border_color(p.border)`. 거의 모든 카드·패널·구분선이 이 단계에 놓인다.
2. **Subtle lift**: `bg(p.card)` + `border_1()` + `border_color(p.border)` (`ui::card`). 캔버스 위에 얹힌 설정 행·그룹 카드.
3. **Floating**: `bg(p.popover)` + `border_1()` + `border_color(p.border)` + `.shadow_lg()`. 편집기/터미널 표면을 벗어나는 팝오버, 드롭다운, Jump 팔레트, 그리고 `MODAL_BACKDROP`(`ui::modal_backdrop`) 위에 뜨는 중앙 모달 대화상자에만 사용한다. `.shadow_lg()` 외의 그림자 단계(`.shadow_xs()` / `.shadow_sm()` / `.shadow_md()` / `.shadow_xl()` / `.shadow_2xl()`)는 쓰지 않는다.

---

## 6. 컴포넌트 및 프리미티브 (`crate::ui` & `gpui-component`)

### 6.1 버튼 변형 우선순위 (`gpui_component::button::Button`)

| 변형 | 메서드 | 용도 |
|---|---|---|
| **Primary** | `ui::primary(id, label)` (`.small().primary()`) | 한 화면/대화상자 흐름 또는 카드 헤더의 대표 일괄 액션(생성, 저장, 승인, 전체 설치, 모두 자동 설정) 또는 세그먼트 토글의 활성 항목 |
| **Outline / Secondary** | `ui::secondary(id, label)` (`.small().outline()`) | 주 동작 옆의 보조 동작, 개별 행 액션(스킬 설치, 연결 테스트, 폴더 열기, 가져오기, 복사) 등 채움 버튼이 무거울 때 |
| **Ghost** | `ui::ghost(id, label)` (`.small().ghost()`) | 아이콘 전용 툴바 버튼, 목록 행 부가 액션(초기화·해제), 비활성 세그먼트 항목, **취소(Cancel)·닫기(Close)·건너뛰기(Skip)** |
| **Danger** | `.danger()` / `ui::danger_confirm(...)` | 삭제, 기록 지우기, 신뢰되지 않은 스크립트 강제 실행 등 되돌릴 수 없는 파괴적 동작. **절대 취소/닫기에 쓰지 않는다.** |

- 버튼 크기는 주변 행 높이에 맞춘다(24~28px 툴바·상태바에는 `.xsmall()`, 설정 행에는 `.small()`, 모달 하단 주 동작에는 기본 크기).

### 6.2 화면 전환 및 오버레이 선택 기준 (Picking the Right Surface)

| 원하는 UX | 선택할 패턴 | 쓰면 안 되는 것 |
|---|---|---|
| **관리자·설정 페이지(`SettingsView`)** | **인앱 페이지 전환**: 메인 `Shell` 창 안에서 터미널 영역을 설정 페이지로 전환(좌측 상단 뒤로 가기 화살표 또는 `Esc`로 즉시 복귀) | 별도 OS 새 창(`cx.open_window`) 띄우기 |
| **집중 입력·결정이 필요한 짧은 생성 흐름** (새 워크트리 `worktree::Dialog`) | **창 중앙 모달 (`ui::modal_backdrop` + `DIALOG_WIDTH`)**: `MODAL_BACKDROP` 스크림, `Esc`·바깥 클릭 닫기 | 사이드바 안에 좁게 인라인으로 구겨 넣기 |
| **빠른 퍼지 검색·세션 이동 (`jump::Palette`)** | **상단 플로팅 팔레트 (`COMMAND_PALETTE_WIDTH`)**: 상단 80px 오프셋, `Up`/`Down`/`Enter`/`Escape` 캡처 | 전체 화면 모달 |
| **클릭/우클릭 액션 메뉴** | `gpui_component::menu::{PopupMenu, DropdownMenu, ContextMenuExt}` | 수제 팝오버 목록 |
| **아이콘 전용 버튼·축약 배지의 이름 설명** | `.tooltip(...)` | 본문에 꼭 필요한 경고나 상호작용 컨트롤을 툴팁에 숨기기 |
| **지속적인 인라인 상태 표시** | 인라인 텍스트 + `ui::badge(p, tone, text)` | 사라지는 알림 |

### 6.3 폼 및 설정 행 해부도 (`crate::ui::primitives`)

설정 탭과 폼 화면은 [`ui/primitives.rs`](../crates/krill-app/src/ui/primitives.rs)의 2열 문법을 표준으로 삼는다.

- **섹션 머리글**: `ui::section_title(p, id, title)` — `TEXT_SM` + `SEMIBOLD` + `p.muted_foreground` + 하단 1px 구분선.
- **2열 설정 행**: `ui::row(p)` 안에 왼쪽 `ui::label(p, title, desc)`과 오른쪽 `ui::controls()`를 배치한다.
  - **불변식**: `ui::card`와 `ui::row`는 항상 `w_full().min_w_0()`을 가져 부모 열을 채우면서 내부 긴 텍스트·자식 요소 때문에 부모 폭 밖으로 넘치지 않게 한다. `ui::label`은 반드시 `flex_1().min_w_0()`을 갖고, 오른쪽 컨트롤 묶음(`ui::controls`)은 `flex_none()`을 갖는다. `justify_between` 행 안에서 `flex_1()` 없이 `min_w_0()`만 주면 gpui flex 레이아웃에서 텍스트 열 폭이 0px로 무너져 한 글자씩 세로 줄바꿈되는 결함이 발생하며(OWE-651), 반대로 `flex_1().min_w_0()` 없이 긴 설명문을 넣으면 우측 버튼이 카드 테두리 밖으로 밀려 나간다(OWE-793).
- **불리언 토글 스위치**: `ui::switch(p, id, checked, disabled, on_toggle)` — 켜기/끄기 불리언 설정은 2버튼 나열 대신 단일 알약 트랙(`SWITCH_WIDTH` × `SWITCH_HEIGHT`)과 원형 썸(`SWITCH_THUMB`) 스위치로 일관되게 표현한다.
- **세그먼트 컨트롤**: `ui::segmented_control(p, items)` + `ui::segment(id, label, active)` — 2~4개 선택지(테마·언어·커서 모양·캐시 TTL·임계값 프리셋 등)는 개별 버튼으로 흩어 놓지 않고 테두리 있는 트랙 안에 묶는다.
- **모달 입력 필드 라벨**: `ui::field_label(name, optional_suffix, p)` — `<Input>` 바로 위에 `gap_1`로 쌓는다.
- **인라인 오류 배너**: `ui::error_banner(p, message)` — 모달이나 폼 안에서 실패 사유를 사용자가 읽고 재시도할 수 있게 고정 표시한다.
- **단축키 칩**: `ui::keycap(p, keys)` — 단축키 표시가 필요한 곳에서는 개별 스타일을 만들지 않고 반드시 `ui::keycap`을 쓴다. 실제 플랫폼 바인딩(`⌘`, `⇧`, `⌥`, `⌃`)과 표기가 정확히 일치해야 한다.

### 6.4 아이콘 및 에이전트 상태 인디케이터 (`ui::icons`, `ui::status`)

- **브랜드 아이콘 (`ui::icons`)**: Claude, Codex(OpenAI), Gemini, Antigravity, OpenCode, Cursor 및 Krill 코랄 로고는 외부 이미지 디코더나 네트워크 없이 인라인 벡터(`ui::icons::agent_brand_icon`, `ui::icons::provider_icon`, `ui::icons::agent_menu_icon`, `ui::icons::provider_menu_icon`, `ui::icons::krill_brand_icon`)로 즉시 그린다.
- **Krill 브랜드 마크 (`ui::icons::krill_brand_icon(size_px, p)`, `ui::icons::brand_mark_tile(size_px)`)**:
  - 마크는 코랄 크릴 하나로 통일한다(데스크톱·메뉴 막대 모노·모바일·웹). 사용처: 사이드바 헤더("Krill" 앞, `ICON_MD`), 빈 작업 공간 히어로(`brand_mark_tile`, 남색 `#08090e` 둥근 상자 `RADIUS_LG` 안 코랄 마크), 메뉴 막대(모노 크릴), 모바일 PWA·웹 랜딩(같은 크릴 SVG).
  - 색: 다크 바탕 `KRILL_CORAL` `#ff5238`, 라이트 바탕 `KRILL_CORAL_LIGHT` `#d63a24`(흰 바탕 4.68:1; `#ff5238`은 3.09:1). `krill_brand_icon`이 팔레트 바탕에 맞춰 고르고(`ui::icons::krill_coral(p)`), `brand_mark_tile`은 바탕이 늘 남색이라 `#ff5238`을 쓴다.
  - **코랄 규칙**: 코랄은 Krill 자신(브랜드 마크)에만 쓴다. 상태 의미(오류·경고·작업 중)에 쓰지 않는다 — `destructive`·탭 Red와 색상각이 9~13°밖에 차이 나지 않아 색약 사용자가 혼동한다. 버튼 채움·포커스 링·선택 배경에도 쓰지 않는다. 예외는 터미널 `Krill Deep` 커서 하나다.
- **UI 아이콘**: `gpui-component`의 Lucide 아이콘(`IconName`)과 `ui/icons.rs`의 Lucide SVG 상수를 사용한다. 다른 아이콘 세트를 섞지 않는다.
- **상태 글리프 (`ui::status::status_glyph`)**:
  - 모든 작업 중(`Working`) 스피너는 `Animation::repeat_synced()`(1초 12스텝)를 공유해 탭과 사이드바 전체에서 같은 위상으로 회전한다.
  - 작업 중 스피너 모양은 Orca 3/4 링 대신 **120° 간격 3분절 아크**(크릴 등갑 모티프, `RING_SVG`)다.
  - **접근성 감속(`cx.reduce_motion()`)**: 움직임 감속이 켜져 있으면 멈춘 분절 아크가 고장처럼 보이지 않도록 닫힌 정적 링(`RING_FULL_SVG`)으로 대체한다.
- **워크트리 상태 요약 (`ui::status::summary_element`)**: 주의를 요하는 순서(`Waiting` → `Blocked` → `Working` → `Done` → `Exited` → `Unknown`)로 최대 3개 그룹(`MAX_GROUPS`), 그룹당 최대 3개 에이전트 아이콘(`MAX_GROUP_ICONS`)을 겹쳐 그리고 나머지는 `+N`으로 축약한다.
- **떼(swarm) 미터 (`ui::status::swarm_meter(states, max, cx)`)**: 세션마다 3px 상태색 점(`agent_state_color`, 유휴는 50% 투명)을 요약과 같은 우선순위로 최대 `SWARM_MAX`(12)개 그리고, 넘치면 `+N`(`TEXT_XS`, `muted_foreground`)을 붙인다. 점 `max`개 칸과 `+N` 칸을 늘 예약해 세션 수가 바뀌어도 폭이 흔들리지 않는다(§7.3). 순서·넘침 계산은 `ui::status::swarm_dots`.

### 6.5 프로바이더 아이콘 (Provider & Agent Icons — OWE-875)

프로바이더(`ProviderKind`: Claude, Codex, Antigravity, OpenCode)나 에이전트(`claude`, `codex`, `agy` / `antigravity`, `opencode`, `cursor` / `cursor-agent`, 커스텀 에이전트)가 표기되는 모든 화면 지점은 단색 회색 아이콘이나 텍스트 단독 표기 대신 반드시 [`ui/icons.rs`](../crates/krill-app/src/ui/icons.rs)의 **색상 있는 브랜드 아이콘**과 **토큰화된 아이콘 크기(`ICON_*`)**를 사용한다. `ProviderKind::Gemini`는 제거된 wire tombstone(계정·사용량 화면에 미표시)이라 빈 박스로 두고, 미분류·제거된 에이전트 ID는 앰버 문자 폴백으로 그린다.

1. **브랜드 색상 상수 (라이트·다크 모드 공통 시인성 보장)**:
   - **Claude**: `CLAUDE_COLOR` (`#D97757`, 테라코타 스파클)
   - **Codex (OpenAI)**: `CODEX_COLOR` (`#10A37F`, 에메랄드 블로섬)
   - **Antigravity**: `ANTIGRAVITY_BLUE` (`#4285F4`) + `ANTIGRAVITY_CORAL` (`#EA4335`, 2레이어 A-링)
   - **OpenCode**: `OPENCODE_COLOR` (`#06B6D4`, 시안 프레임) + `OPENCODE_CORE_COLOR` (`#67E8F9`, 코어 블록)
   - **Cursor**: `CURSOR_COLOR` (`#6366F1`, 인디고 큐브) + `CURSOR_HIGHLIGHT_COLOR` (`#A5B4FC`, 상단 면 하이라이트)
   - **커스텀 / 미분류 에이전트**: `CUSTOM_AGENT_COLOR` (`#F59E0B`, 앰버 봇 폴백)
2. **아이콘 크기 토큰 (`crate::ui::ICON_*`)**:
   - `ICON_XS` (`12px`): 고밀도 인라인 배지 — 사이드바 세션 행(`session_row`), 관리자 계정 행 프로바이더 배지, AI Vault / Jump 에이전트 필터 칩
   - `ICON_SM` (`13px`): 기본 행·탭 아이콘 — 상단 탭 바 탭, 워크트리 카드 하단 에이전트 알약 및 요약 칩(`summary_element`), 다른 계정으로 이어가기 모달, Jump 팔레트 세션/Vault 행, AI Vault 검색 결과 행, 온보딩 점검 행(`agent.*`, `hook.*`)
   - `ICON_MD` (`14px`): 카드·팝오버 헤더 — 상태바 프로바이더 사용량 팝오버 헤더, 탭 드래그 프리뷰, 최근 탭 전환기(`Ctrl+Tab`), 에이전트 대시보드 카드, 예약 실행(Schedules) 카드, 메뉴/드롭다운 항목(`provider_menu_icon`, `agent_menu_icon`)
   - `ICON_LG` (`16px`): 섹션 머리글 — 관리자 > 계정(`Accounts`) 프로바이더 섹션 헤더
   - `ICON_XL` (`18px`): 프로필 아바타 — 관리자 > 에이전트(`Agents`) 프로필 카드(32×32px 타일 내부)
3. **렌더링 헬퍼 선택 및 텍스트 병기 원칙**:
   - 일반 `div` 자식에는 다중 레이어 브랜드 색상이 적용되는 `ui::icons::provider_icon(provider, size_px, fallback)` / `ui::icons::agent_brand_icon(agent_id, size_px, fallback)`을 사용한다.
   - `gpui_component`의 `Button::icon`, `PopupMenuItem::icon`, `PopupMenu::submenu_with_icon`처럼 `Icon` 타입을 요구하는 위젯에는 `.text_color(brand_color)`가 입혀진 `ui::icons::provider_menu_icon(provider)` / `ui::icons::agent_menu_icon(agent_id)`을 사용해 버튼·메뉴 전경색에 덮어쓰이지 않게 한다.
   - **텍스트 유지/대체 기준**: 탭·사이드바 세션 행·워크트리 요약 알약처럼 세션/워크트리 제목이 함께 표시되는 고밀도 영역은 색상 아이콘만으로 에이전트를 식별하고, 메뉴 항목·설정 섹션 헤더·필터 칩·온보딩 선택지처럼 명확한 이름 식별이나 접근성이 필요한 곳은 `[색상 아이콘 + 텍스트]`를 함께 표기한다.
   - 제3자 상표·아이콘 출처 고지는 [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)(`docs/licensing/generate_notices.py`)에 유지한다.

---

## 7. 필수 UX 원칙 (Mandatory UX Rules)

독립적으로 작업할 때 가장 틀리기 쉬운 규칙들이다. 모든 UI 변경에 예외 없이 적용한다.

### 7.1 미구현 기능 표시 금지 및 문구 과장 금지 (No Unimplemented Chrome or Overclaiming Copy)

- **있는 기능만 표기한다**: Orca의 UI/UX를 참조하더라도 **Krill에 실제로 구현되어 동작하는 기능만 화면에 그린다.** 아직 없는 기능(예: 아티팩트, 스킬 마켓, 작업/칸반 보드, 미구현 자동화 탭, 동작하지 않는 플레이스홀더 버튼/단축키 칩)을 껍데기만 복사해 노출하는 것은 엄격히 금지한다.
- **조건부 노출**: 프로비저닝 설정이 켜져야만 성립하는 '다른 계정으로 이어가기'(`ui::status::continue_chip_visible`)나 데몬 기능 협상(`FEAT_*`)이 필요한 컨트롤은 해당 기능이 실제로 활성화된 상태에서만 노출한다.
- **문구 정직성**: 코드가 실제 상태나 결과 데이터를 확인하기 전에는 "보호됨", "검증됨", "삭제됨", "적용 완료" 같은 결과 동사를 쓰지 않는다. 진행 중일 때는 중립적인 과정 문구를 쓰고, 실제 결과가 도착했을 때만 결과 문구를 표시한다.

### 7.1.1 카피 톤 (Krill Voice)

- **차분하고 사실만**: 감탄·과장·불안을 부르는 말("치명적", "위험!", "완벽하게")을 쓰지 않는다. 지금 무엇이 어떤 상태인지 한 문장으로 말한다.
- **보존·이어짐 어휘**: 세션이 안전한 상황(데몬 재연결 중, 앱 재시작·업데이트, 탭 밖 분리 세션)은 "끊김"·"오류"가 아니라 "보존", "이어짐", "계속 실행 중"으로 말하고 `preserve` 톤으로 그린다. 사용자 조치가 필요한 실패(재연결 실패, 멈춤)만 `warning`·`destructive`를 쓴다.
- **결과 동사 규칙**: "다시 연결했습니다", "세션 {n}개에 다시 연결했습니다" 같은 결과 문구는 실제 결과(세션 목록 도착 등)를 확인한 뒤에만 보인다(§7.1). 확인 전에는 "다시 연결하는 중…"처럼 과정 문구를 쓴다.
- **프로세스 이름은 정확히**: PTY와 세션은 **krill-holder**가 소유하고, **krilld**는 세션 데몬(질의 응답·저장·상태)이다. "세션은 krilld에서 실행됩니다"처럼 쓰지 않는다. 사용자 문구에서는 프로세스 이름을 빼고 "세션은 계속 실행됩니다"로 쓰거나, 구조 설명이 필요할 때만 "세션은 krill-holder가 보관합니다"라고 쓴다.

### 7.2 인앱 페이지 전환 vs 모달 대화상자

- **관리자·설정 페이지는 인앱 전환**: `⌘,` 또는 사이드바 설정 버튼을 누르면 새 창을 띄우지 않고 메인 창 안에서 페이지가 전환되어야 한다.
- **모달은 짧은 결정·생성에만**: 새 워크트리 생성이나 파괴적 충돌 확인처럼 현재 맥락 위에서 즉시 입력/결정하고 닫는 흐름만 `ui::modal_backdrop` 중앙 모달을 쓴다.

### 7.3 체감 소요 시간에 맞춘 피드백과 공간 사전 예약 (In-Flight Feedback & Pre-Reserved Footprint)

| 소요 시간 | UI 피드백 규칙 |
|---|---|
| `0 ~ 100ms` | 아무 스피너/문구 전환도 보여 주지 않는다(찰나의 깜박임은 글리치로 보인다). |
| `100ms ~ 1s` | 중복 제출 방지를 위해 컨트롤 비활성화(`disabled`)만 즉시 걸어 둔다. |
| `1s ~ 3s` | 비활성화 + 스피너 또는 상태 문구 전환(예: "생성 중…"). |
| `3s 이상 / 다단계` | 현재 단계 라벨(예: 워크트리 백그라운드 준비 상태 + 취소 버튼)을 표시한다. |

- **공간 사전 예약 (Pre-reserve footprint)**: 상태가 바뀔 때 컨트롤의 크기가 변해 주변 레이아웃이 흔들리면 안 된다. 특히 2단계 삭제 확인 버튼(`ui::danger_confirm`)은 기본 문구와 확인 문구를 같은 1×1 grid 칸에 겹쳐 그려 폭을 고정함으로써, 무장 상태에서 옆 버튼("이름 변경" 등) 자리를 침범해 오클릭을 유발하는 사고(OWE-704)를 원천 차단한다.

### 7.4 인접(Sibling) 컴포넌트 일관성

- 같은 도메인이나 인접한 순간에 보이는 컴포넌트(예: 사이드바 세션 행 ↔ 탭 바의 탭 ↔ 상단 Jump 팔레트 항목, 또는 상태바 계정 스위처 ↔ 관리자 페이지 계정 탭)는 같은 아이콘, 같은 상태 점/스피너(`ui::status`), 같은 라벨 규칙(`sidebar::session_label`), 같은 단축키 표기를 공유해야 한다.

### 7.5 취소·복귀 경로 과부하 금지 (Don't Overload the Back-Out Path)

- `danger`(`destructive`)는 데이터를 잃거나 되돌릴 수 없는 동작 전용이다.
- **취소(Cancel), 닫기(Close), 건너뛰기(Skip)는 파괴적 동작이 아니다.** 사용자를 안전하게 이전 상태로 되돌리는 버튼이므로 조용한 `.ghost()` 버튼을 쓰고, 빨간색이나 불필요한 단축키 칩(`Esc` 배지 등)을 붙이지 않는다. 시각적 무게는 긍정 주 동작(`.primary()`)에 집중시킨다.

---

## 8. 화면 UX 리뷰 루브릭 및 결정 우선순위

UI 화면·스크린샷·PR을 검토할 때 아래 기준으로 판단한다.

1. **점진적 노출 (Progressive disclosure)**: 고빈도 동작은 바로 보이는 곳에 두고, 저빈도 동작은 컨텍스트 메뉴나 고급 섹션으로 옮긴다.
2. **동작 위계 (Action hierarchy)**: 주 동작(`.primary()`)은 위치와 스타일로 즉시 구분되어야 하며 보조 동작과 경쟁하지 않는다.
3. **클릭 수와 기본 포커스 (Click count & Default focus)**: 대화상자나 팔레트가 열릴 때 사용자가 바로 타이핑하거나 `Enter`로 제출할 수 있도록 입력칸에 포커스를 둔다. `Esc`는 언제나 안전하게 닫는다.
4. **정렬과 밀도 (Alignment & Density)**: 텍스트·라벨은 왼쪽 정렬, 수치·카운터·단축키·우측 컨트롤은 오른쪽 정렬한다. 카드 안에 또 카드를 중첩하지 않는다.
5. **접근성 및 테스트 가능성**: 모든 상호작용 요소에 안정적인 `id`, `.debug_selector(...)`, 필요 시 `.accessibility_label(...)`을 부여하고 `cx.reduce_motion()`을 존중한다.

### 이 가이드에 명시되지 않은 경우 (When This Guide Is Silent)

1. `crates/krill-app/src/` 안의 가장 가까운 인접(sibling) 화면 코드를 확인하고 그 패턴을 따른다.
2. [`crates/krill-app/src/ui/`](../crates/krill-app/src/ui/mod.rs)와 `gpui-component`에 이미 준비된 프리미티브가 있는지 확인한다.
3. 색·치수·시각 문법 질문이면 [`ui/theme.rs`](../crates/krill-app/src/ui/theme.rs)(라이트·다크 모두)에 시맨틱 토큰으로 추가한다.
4. 위 세 단계로도 해결되지 않거나 새 기능 노출 여부가 모호하면 임의로 발명하지 않고 사용자에게 먼저 묻는다.

---

## 9. 기계적 강제 (Automated Enforcement)

이 디자인 시스템은 문서에만 머물지 않고 `scripts/check.sh`(`cargo nextest run -p krill-app`)에서 자동으로 검증된다([`crates/krill-app/src/ui/theme.rs`](../crates/krill-app/src/ui/theme.rs) 테스트 모듈):

1. **생 색상 및 임의 픽셀 글자 크기 차단 (`no_raw_colors_or_arbitrary_font_sizes_outside_ui_module`)**: `crates/krill-app/src/` 아래의 모든 `.rs` 파일(`src/ui/` 및 터미널 셀 렌더러 `src/terminal.rs` 제외)을 스캔하여 `gpui::rgb(`, `gpui::hsla(`, `gpui::black(`, `gpui::white(`, `gpui::transparent_black(`, `gpui::red(` / `green(` / `blue(` / `yellow(`, `ui::hex(`, `ui::hexa(`, 비수식 `hex(0x` / `hexa(0x`, 비허용 그림자 단계(`.shadow_xs(` / `.shadow_sm(` / `.shadow_md(` / `.shadow_xl(` / `.shadow_2xl(`), `.text_size(px(` 호출이 발견되면 테스트가 즉시 실패한다.
2. **문서-코드 토큰 동기화 검증 (`styleguide_doc_stays_in_sync_with_palette_and_tokens`)**: `Palette`의 모든 시맨틱 색 필드와 타이포그래피·반경·간격·프리미티브 식별자가 이 문서(`docs/STYLEGUIDE.md`)에 빠짐없이 문서화되어 있는지 검사한다.
3. **Orca 패리티 토큰 회귀 검사 (`orca_parity_tokens`, `palettes_distinguish_light_and_dark_surfaces`)**: 라이트·다크 대비와 주요 치수 상수가 흔들리지 않도록 보장한다.
4. **대비 회귀 검사 (`text_tokens_meet_wcag_aa_on_every_surface`)**: §3.1.1의 글자·표면 조합이 WCAG 4.5:1 미만으로 떨어지면 실패한다.
5. **터미널 기본 테마 일치 (`krill_deep_terminal_matches_daemon_default`)**: 앱 `TERMINAL_DARK`와 데몬 `krill_proto::theme::DEFAULT`가 어긋나면 실패한다.
