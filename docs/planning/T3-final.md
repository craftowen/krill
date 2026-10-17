**결론: ① UI는 gpui(`gpui-pre =0.3.9` + `gpui-component 0.7.3`, vendor 고정)로 가되 첫 스파이크(한글 IME)가 실패하면 objc2 AppKit + 커스텀 Metal로 전환한다. ② 터미널 상태는 데몬 안의 `alacritty_terminal 0.26.0`(핫 셀 스크롤백 1,000줄)과 `lz4_flex` 압축 원본 바이트 로그(콜드)로 나눈다. 셀 1만 줄 × 10개는 실측 320MB라 금지한다. ③ 메모리 예산의 가장 큰 단일 항목은 창 drawable(IOSurface)이다. 같은 조건에서 다시 재 보니 gpui도 iced와 비슷한 76MB였다. 그래서 앱 ≤ 120MB, 데몬 ≤ 60MB, 합계 ≤ 180MB(상한 200MB)로 1차 보고서의 "앱 ≤ 90MB"를 현실적인 값으로 고친다.**

작성: T3 2차(검증) 워커 / 2026-10-14 / 기준: `.ref/orca` HEAD a0c46b40, Zed `c403bc6af644cd0ab60c90045ac6dc3436218809`, winit `f0a83481c233c714d061356305f89bace0106f39`(= 2026-10-12 master), 이 맥미니(Apple Silicon, macOS 27, DELL U2725QE 5K, "UI Looks like" 2560×1440 @2x)

읽는 법: "실측"은 이 맥미니에서 이번에 다시 잰 값이다. "추정"은 계산이나 정황 근거다. 근거가 `.ref/orca` 경로이면 `.ref/orca` 기준 상대경로이고, 렌더러 파일은 `src/renderer/src/` 아래다.

---

## 0. 이 트랙이 답할 질문과 성공 기준

| # | 질문 | 성공 기준(관찰 가능) |
|---|---|---|
| Q1 | 어떤 Rust GUI로 앱을 만드나 | 세 조건을 모두 만족한다. ① 한글 2벌식 조합 글자가 터미널 커서 셀에 인라인으로 보이고, 확정할 때 자모 유실·중복·순서 뒤바뀜이 없다. ② 터미널 10개를 띄운 1600×1000pt 창에서 앱 phys_footprint ≤ 120MB. ③ 상용 배포에 라이선스 문제가 없다 |
| Q2 | 터미널 VT 상태를 어느 크레이트가 맡나 | Claude Code·Codex TUI가 깨지지 않는다(DEC 2026, kitty keyboard, OSC 8, SGR 1006, DECSCUSR). 순수 Rust이고 crates.io로 배포되며, 핫 상태 기준 터미널 1개당 ≤ 5MB |
| Q3 | 스크롤백을 어디에 어떤 형태로 두나 | 1만 줄 이상을 보존하면서 10세션 합계 ≤ 60MB이고, UI를 재시작해도 놓친 출력을 다시 볼 수 있다 |
| Q4 | Orca의 어떤 화면을 언제 재현하나 | 화면별 난이도와 마일스톤 배정 |
| Q5 | 메모리·시작 시간·렌더 성능 목표와 측정법 | 숫자로 된 예산표와 재현 가능한 측정 절차, 스파이크별 합격선 |

다른 트랙과 겹치는 부분은 짧게만 다룬다. 프로토콜·재접속·데몬 수명은 T2, 기능 범위는 T1, 에이전트 상태·Git·배포는 T4가 맡는다.

---

## 1. 이전 조사 정정 (주장 / 실제 / 근거)

1차 보고서(`.ref/drafts/T3-A.md`)의 주장을 하나씩 원본과 대조했다. 대조 수단은 crates.io API, `gh api`, 고정 커밋 소스, 로컬 크레이트 소스, 재측정이다. **틀린 결론을 낳은 정정**을 먼저 싣고, 그다음 수치·줄 번호 정정을 싣는다. 여기 없는 주장(크레이트 버전·날짜·라이선스, GitHub 이슈 상태, Zed 줄 번호 대부분, alacritty 메모리 수치, 압축률)은 모두 맞았다.

### 1.1 결론에 영향을 준 정정

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| C1 | gpui footprint가 가장 낮다(gpui-pre hello 45MB). 이것이 gpui 권고 근거 중 하나 | **비교 조건이 달랐다.** 45MB는 800×600 창에 한 줄만 띄운 값이다(IOSurface 15MB). 다른 프레임워크와 같은 조건(1400×900pt, 텍스트 50줄)으로 다시 재면 gpui-pre 0.3.9는 **76–77MB**(IOSurface 39MB, 2회)다. iced 0.14는 71–76MB(IOSurface 39MB), gpui 0.2.2는 95–106MB(IOSurface 59MB), eframe 0.36.2는 107MB(IOSurface 59MB)다. **메모리는 gpui를 고를 근거가 아니다.** 근거는 IME와 터미널 선례뿐이다 | 실측. `/tmp/guiresearch/bench/gp/src/main.rs:11`(`size(px(800.), px(600.0))`). 동일 조건 재빌드는 `/tmp/t3v-me/gp2` |
| C2 | 앱 소계 ≤ 90MB | 1600×1000pt 창의 drawable 한 장이 3200×2000×4B ≈ 25.6MB다. gpui 유휴 상태에서 2장, 부하 시 최대 3장이므로 51–77MB다. 여기에 비-IOSurface 기본 약 37MB(실측 76MB − 39MB)를 더하면 **90MB는 hello 수준에서 이미 빠듯하다.** 앱 목표를 ≤ 120MB로 올린다(§3.6) | 계산과 실측 |
| C3 | "gpui에서 `maximumDrawableCount`를 조절할 수 있는지 미확인" | gpui가 **`set_maximum_drawable_count(3)`으로 하드코딩**한다. 유휴 시 drawable을 해제하는 코드는 없다. 창이 가려지면(occlusion) CVDisplayLink만 멈춘다. 2로 줄이려면 vendor 패치(한 줄)가 필요하다 | Zed `crates/gpui_apple/src/metal_renderer.rs:183`, gpui-0.2.2 `src/platform/mac/metal_renderer.rs:146`, gpui-pre-apple 0.3.9 `metal_renderer.rs:183`. occlusion은 `crates/gpui_macos/src/window.rs:2993-3006` |
| C4 | truecolor TUI 프레임 파싱이 23MB/s여서 데몬 CPU 병목 후보 | **282–349MB/s**(120×50, 셀마다 `38;2;r;g;b`, 2026 감싸기 유무·청크 1K/4K/64K 모든 변형). 병목이 아니다. 23MB/s는 debug 빌드 등 다른 조건에서 나온 값으로 추정한다 | 실측 `/tmp/t3v-meas/m/src/bin/tp.rs`, `tp2.rs` |
| C5 | Zed 방식(`terminal_element.rs`)을 렌더러로 참고·재사용 | Zed의 `terminal`·`terminal_view` 크레이트는 **GPL-3.0-or-later**다. gpui·gpui_macos만 Apache-2.0이다. **코드 차용은 금지하고, 구조 아이디어만 참고(클린룸)한다.** 1차 보고서에는 이 구분이 없었다 | `crates/terminal_view/Cargo.toml` `license = "GPL-3.0-or-later"`, `crates/terminal/Cargo.toml` 동일, `crates/gpui/Cargo.toml` `Apache-2.0` |
| C6 | gpui는 AccessKit(macOS 어댑터)을 지원 | **부분.** Zed main과 gpui-pre 0.3.9에는 있다(`accesskit_macos::SubclassingAdapter::for_window`). 하지만 모듈 이름이 `_accessibility`(밑줄 접두사)라 실험 단계로 보인다. crates.io 공식 `gpui 0.2.2`에는 없다. Zed의 Windows 스크린리더 이슈 #41138은 아직 open이다 | Zed `crates/gpui_macos/src/window.rs:2376`, gpui-pre-macos 0.3.9 `Cargo.toml:63-67`, https://github.com/zed-industries/zed/issues/41138 |

### 1.2 수치·줄 번호·표현 정정

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| C7 | gpui 0.2.2 hello footprint 124MB | 95–106MB(RSS 97MB, IOSurface 59MB는 맞음) | 실측(기존 바이너리 `/tmp/t3mem/hgpui` 재실행) |
| C8 | eframe 0.36.2 footprint 87MB | 107MB(RSS 111–115MB, IOSurface 59MB) | 실측 |
| C9 | `gpui_macos`·`gpui_platform`은 crates.io 404 | `gpui_platform`은 404가 맞다. `gpui_macos`는 **이름만 같은 서드파티 크레이트**(0.1.1, owner gstohl, "macOS design system for GPUI Kit")로 존재한다. Zed 공식 플랫폼 크레이트가 아니라는 취지는 맞다 | https://crates.io/api/v1/crates/gpui_macos |
| C10 | `gpui-pre`는 Zed 스냅샷 재게시본 | 맞다. 설명이 "gpui-pre snapshot of zed@93d7456"이고 메타데이터가 `zed-rev = 93d745668805…`다. **Zed 커밋 c403bc6과는 다른 스냅샷**이므로 줄 번호를 서로 섞어 인용하면 안 된다 | https://crates.io/api/v1/crates/gpui-pre |
| C11 | dioxus-native 0.7.10, blitz 0.2.1(0.3 beta) | 안정판은 맞다. 프리릴리스가 빠졌다: dioxus-native 0.8.0-alpha.1(2026-07-31), blitz 0.3.0-beta.3(2026-10-12) | crates.io API |
| C12 | libghostty-vt는 "Zig 0.16 필요" | `libghostty-vt` 자체에는 build.rs가 없다. 의존하는 `libghostty-vt-sys`의 build.rs가 ghostty를 `git clone`(커밋 a887df42)한 뒤 `zig`를 실행한다. Zig와 네트워크가 필요하다는 결론은 맞다. **"0.16"이라는 버전은 확인하지 못했다** | 크레이트 tarball `/tmp/t3v-meta/x/` |
| C13 | Zed는 `Arc<FairMutex<Term>>`를 쓰고 alacritty는 포크 0.26.1-dev(PTY signal mask 패치) | 별칭 `AlacrittyTermLock = FairMutex<AlacrittyTerm>`(`crates/terminal/src/alacritty.rs:52`)을 거쳐 `term: Arc<AlacrittyTermLock>`(`terminal.rs:1528`)로 쓴다. 의존성은 `zed-industries/alacritty` rev `4c129667…`(`Cargo.toml:532`), 버전 0.26.1-dev, vte 0.15.0이다. **포크의 패치 내용(signal mask)은 확인하지 못했다** | Zed c403bc6 |
| C14 | 블록 문자를 서브셀 사각형으로 직접 칠함 | 일부만 그렇다. 블록 요소(U+2580–2590, 2594, 2595), 4분할, 섹스턴트(U+1FB00–1FB3B), 음영 ░▒▓만 사각형으로 칠한다. **박스 드로잉 U+2500–257F는 글리프로 그린다** | `crates/terminal_view/src/terminal_element.rs:721-743, 755-817` |
| C15 | gpui NSTextInputClient "자체 완전 구현" | 플랫폼 계층은 모든 셀렉터를 등록한다(`window.rs:236-295`). 하지만 `validAttributesForMarkedText`는 빈 배열 스텁이고(L3366), Zed 터미널의 `TerminalInputHandler`는 `text_for_range`·`character_index_for_point`에 `None`, `selected_text_range`에 `0..0`을 돌려준다. **터미널 위젯에서는 이 셋이 사실상 스텁이다.** 단, 그래도 한글 조합에는 문제가 없다(§3.3) | Zed `crates/gpui_macos/src/window.rs:3366, 3384, 3482, 3577`, `terminal_element.rs:1764` 이후 |
| C16 | winit `characterIndexForPoint`는 스텁(NSNotFound 계열) | 스텁은 맞다. 다만 반환값이 **`0`**이다 | `winit-appkit/src/view.rs:360-363` |
| C17 | makepad NSTextInputClient `macos_delegates.rs` L1281–1315 | 기본 브랜치 `dev` 기준 메서드 본문은 맞다. `add_protocol`은 L1460, `doCommandBySelector`는 L1320이다. `main` 브랜치에서는 L1194–1233이다 | makepad dev `71c9ddc`, main `c6497c7` |
| C18 | 2MiB sync 버퍼는 쓰기 전까지 할당되지 않는다 | vte 0.15.0의 `SyncState::default()`가 `Vec::with_capacity(SYNC_BUFFER_SIZE)`로 **2MiB를 미리 예약한다.** 예약은 가상 메모리라 건드리기 전에는 footprint에 잡히지 않는다(빈 Term 10개 +5.2MB 실측과 일치). 하지만 한 번 크게 채워진 페이지는 상주로 남을 수 있다. 세션당 최대 2MiB, 10세션이면 최대 20MB가 잡힐 수 있다(추정, S2에서 측정) | `vte-0.15.0/src/ansi.rs:36, 39, 263` |
| C19 | Orca IME 코드 비테스트 1,580줄 | 1,580줄에 테스트 픽스처 `terminal-ime-linux-pane-replay-fixture.ts`(153줄)가 섞여 있다. **프로덕션 코드는 1,427줄**이다. iPadOS 한글 파일(316 + 53줄)과 `lib/ime-*` 파일은 따로 있다 | `components/terminal-pane/terminal-ime-*` `wc -l` |
| C20 | `terminal-ime-deferred-newline.ts` 주석 :18–20 | 상수는 :7이 맞다. 주석 "`한` Enter `글` … `한글\n`"은 **:19–21**이다 | 해당 파일 |
| C21 | composition-tracker: 10초 지난 조합은 폐기 | 10초(`…STALE_COMPOSITION_EXPIRY_MS = 10_000`, :14)는 조합 자체를 버리는 값이 아니다. **Space·숫자 후보 키 가드가 영구히 걸려 있지 않도록 만료시키는 값**이다. 250ms 가드(:18, Sogou/fcitx 주석 :15–17)는 맞다. :98–102는 이벤트 리스너 등록이다 | `terminal-ime-composition-tracker.ts:11-18, 98-102` |
| C22 | `terminal-ime-anchor.ts`는 에이전트 프롬프트 위치에 앵커 | 프롬프트 감지는 **Cursor Agent 전용**이다(`CURSOR_AGENT_HEADER`). 일반 경로는 `terminal-ime-candidate-anchor.ts`로, 조합 시작 시 OS 후보창을 입력 중인 셀에 맞춘다 | `lib/pane-manager/terminal-ime-anchor.ts:8-13`, `terminal-ime-candidate-anchor.ts:13-20` |
| C23 | `macos-press-and-hold-default.json`(용도 미확인) | 소스는 `src/main/macos-press-and-hold-default.ts`이고 `.json`은 userData 기록 파일 이름이다. 용도: 앱 도메인에 `ApplePressAndHoldEnabled=false`를 **한 번만** 써서 키를 누르고 있을 때 악센트 팝업 대신 키 반복이 되게 한다(vim `j` 홀드). 사용자가 다시 켜면 덮어쓰지 않는다 | `src/main/macos-press-and-hold-default.ts:7-31` |
| C24 | `pane-terminal-output-queue-registry.ts:80-87`이 DEC 2026을 처리 | :80–88은 큐 상태 필드이고 :82 주석이 2026 교대 스트림 정지 문제(#8754)를 설명한다. 실제 처리는 backlog를 버릴 때 `\x1b[?2026l`을 주입하는 **:111–118**이다 | 해당 파일 |
| C25 | `AppWorkspaceShell.tsx:115-245`에 리사이즈 핸들 | 115–245는 3열 레이아웃 JSX다. 핸들은 `components/sidebar/index.tsx:39-42`(클래스)와 `:243-249`(렌더)에 있다. :114–116은 너비 CSS 변수다 | 해당 파일 |
| C26 | `pane-manager.ts:73-305` | `class PaneManager`는 :73–**361**(파일 끝)이다. `TerminalPane.tsx`는 7줄짜리 래퍼다. 분할 트리(:126–138)와 최소 50px(`pane-divider-drag.ts:16`)는 맞다 | 해당 파일 |
| C27 | 에이전트 상태는 working/waiting/done/blocked 4개 | `AgentDotState`는 11개다(working, monitoring, blocked, waiting, interrupted, failed, done, idle, unverifiable, unconfirmed, permission). 워크트리 상태(`WorktreeStatus`)는 별도이며 waiting/blocked가 없다. 우선순위는 permission > working > monitoring > active > inactive다. 4상태는 Minikrill M1용 단순화로 남기되, 그것이 **의도적 축소**라는 점을 명시한다 | `components/AgentStateDot.tsx:22-46`, `lib/worktree-status.ts:15-24, 47-74` |
| C28 | 명령 팔레트 Cmd+J, Quick Open Cmd+P | `Mod+P`는 "Go to File"(`worktree.quickOpen`)이다. 워크트리 전환(`worktree.palette`)은 mac에서 Mod+J, 그 외에서 Mod+Shift+J다 | `src/shared/keybindings/definitions-core-1.ts:8-13, 35-45` |
| C29 | native-chat 787개 파일, 받아쓰기 sherpa-onnx는 renderer | 787개는 테스트 포함이고 비테스트는 423개다. sherpa-onnx는 **main 프로세스**(`src/main/speech/stt-worker.ts`)에서 돈다 | 파일 수 집계 |
| C30 | 상호작용 출력용 256KB 예약 | 단위는 바이트가 아니라 **문자**다(`PTY_RENDERER_INTERACTIVE_RESERVE_CHARS = 256 * 1024`, :8). 그 밖에 PTY당 high-water 512K(:5), 전체 8M(:6), ACK 침묵 10초(:11)가 있다 | `src/main/ipc/pty/delivery/constants.ts:1-17` |
| C31 | iced 기본 text_input은 over-the-spot 오버레이 | 대체로 맞다. iced 0.14에 Input method 지원(#2777)과 `window::Preedit`이 들어갔다. preedit은 위젯 근처 별도 오버레이로 그려진다 | https://github.com/iced-rs/iced/blob/master/CHANGELOG.md (0.14.0 항목, #2777, #2790, #2793) |
| C32 | Orca 실측: Main 412/376, Renderer 777/681–686, GPU 133/1,146–1,166(IOSurface 891), 데몬 135/165, PTY 17개 | 측정 오류가 아니라 시점 차이다. 22:01 재측정(앱 4일째 실행): Main 428/388, Renderer 772/**805**, GPU 125/**1,534(IOSurface 1,245)**, 데몬 139/167, **PTY 21개**. 부모 PID가 1인 고아 데몬 2개(56MB, 66MB footprint)가 추가로 있었다. 핵심 4개 합계는 RSS 1,464MB / footprint 2,894MB, 보조 포함 시 RSS 약 1,661MB / footprint 약 3,153MB다. **GPU 프로세스 footprint가 시간에 따라 커진다** | 실측(`ps`, `footprint -p`) |
| C33 | 순수 Rust zstd 인코더는 미성숙(미확인) | 확인했다. `ruzstd` 0.9.1은 `Fastest`(약 레벨 1, 28.9%, 141–161MB/s)만 동작하고 Default/Better/Best는 `unimplemented!()`다. lz4_flex(31.0%, 650–663MB/s)보다 조금 작고 4배 느리다 | ruzstd-0.9.1 `src/encoding/frame_compressor.rs:203`, 실측 |

---

## 2. 확정 결정 사항

| # | 결정 | 권고안 | 대안 | 근거 | 되돌리기 비용 |
|---|---|---|---|---|---|
| D1 | UI 프레임워크 | **gpui**(`gpui-pre =0.3.9` + `gpui-component =0.7.3`), `cargo vendor` 고정. Apache-2.0 | ① objc2-app-kit 0.3.2 + 커스텀 Metal(가장 네이티브, 코드량 최대) ② iced 0.14(winit, 메모리 동급) ③ makepad | winit 계열(iced·egui·Slint·Xilem·floem·Blitz)은 macOS NSTextInputClient의 `selectedRange`·`attributedSubstring`·`characterIndexForPoint`가 스텁이다(`winit-appkit/src/view.rs:257-261, 349-363`). 한글 open 이슈도 쌓여 있다: winit #3095(자모 분리), #4524(한자), #4626(panic). 같은 winit 기반인 Alacritty에도 #8079(한글 IME에서 스페이스 2번), #6942(CJK 첫 특수문자 무시)가 열려 있다. gpui는 AppKit 계층을 직접 구현했고 Zed 터미널이 같은 문제를 실전에서 풀고 있다. **메모리는 근거가 아니다(C1)** | 높음. 위젯·레이아웃 코드 전부. 그래서 S1을 첫 주에 둔다. 터미널 그리드 렌더러와 IME 상태기계는 프레임워크 중립 모듈로 분리해 손실을 줄인다 |
| D2 | VT 상태 엔진 | **`alacritty_terminal 0.26.0`**(crates.io, Apache-2.0), 데몬에 세션당 1개 | ① libghostty-vt(8B 셀, 압축·스냅샷 API 내장, 하지만 Zig + C API WIP) ② termwiz/wezterm-term(wezterm-term은 crates.io에 없음) | 순수 Rust. OSC 8, DECSCUSR, kitty keyboard, 2026, SGR 1006, reflow를 모두 소스로 확인했다(§3.4). Zed가 실사용 중이다. 파싱 135–191MB/s(실측) | 중간. 데몬 내부에만 갇히면 교체 범위는 데몬의 "세션 상태" 모듈 하나다 |
| D3 | 스크롤백 모델 | **핫: 셀 그리드 1,000줄 / 콜드: PTY 원본 바이트를 64KB 블록 단위로 `lz4_flex` 압축해 메모리 링(세션당 ≤ 2MB) + 디스크 스풀** | 셀 1만 줄(Orca 5,000, Zed·Alacritty 10,000) | 실측: 120열 1만 줄 셀은 32.1MB/세션, 1,000줄은 3.6MB. 원본 16MB는 lz4 31%로 약 4.97MB이고 1만 줄(약 760KB 원본)은 약 240KB다. 원본을 보관하므로 "놓친 출력"을 바이트 단위로 재생할 수 있다 | 낮음. 핫 줄 수는 설정값이다 |
| D4 | UI가 받는 데이터 | **보이는 영역 스냅샷 + 변경 행(dirty row) 셀 델타.** 스크롤백은 요청할 때 페이지로 받는다(프로토콜 세부는 T2) | UI도 원본 바이트를 받아 자체 alacritty Term을 유지(Orca 방식: 데몬 headless xterm + 렌더러 xterm) | UI 쪽 Term 미러를 두면 앱에 세션당 3.6MB 이상(1,000줄 기준, 10개 36MB)이 중복된다. 앱 예산(§3.6)을 넘는다. 대신 선택·복사·검색 범위가 화면 밖이면 데몬 왕복이 필요하다 | 중간. 프로토콜 변경. S5에서 결정한다 |
| D5 | 터미널 렌더 방식 | **gpui 텍스트 시스템 재사용:** 스타일이 같은 연속 셀을 run으로 묶고, 셀 폭 강제(`shape_line`의 force_width), 배경 사각형 병합, vsync당 한 번 그리기. 구현은 클린룸으로 한다(Zed 터미널 코드는 GPL, C5) | 셀마다 글리프 쿼드를 그리는 커스텀 Metal 셰이더(Alacritty·Ghostty 방식) | 코드량이 가장 적다. S3에서 프레임 p99 > 16.7ms이면 커스텀 셰이더로 전환한다 | 중간. 렌더러 모듈 한 개 |
| D6 | 한글 IME | preedit는 UI 상태로만 두고 커서 셀 위 오버레이로 그린다. PTY에는 확정 텍스트만 쓴다. 포커스를 잃거나 탭을 바꾸면 **확정**한다. 후보창은 커서 셀에 붙인다 | – | Orca의 IME 대응 코드 1,427줄에서 정리한 실패 패턴과 Zed의 구현(`terminal_element.rs:1683-1725` 오버레이) | 낮음 |
| D7 | 글리프 아틀라스 필터 | gpui vendor 패치로 단색 아틀라스 샘플러를 **nearest**로 바꾸는 것을 S1에서 비교 검증한다 | 업스트림 기본(bilinear) 유지 | Zed #57777(open): bilinear 때문에 한글처럼 획이 촘촘한 글리프의 가장자리가 번진다. Ghostty·Kitty·Alacritty는 nearest를 쓴다 | 낮음(한 줄 패치) |
| D8 | drawable 수 | vendor 패치로 `maximumDrawableCount` 3 → **2**. 지연과 프레임 드롭은 S3에서 확인한다 | 3 유지 | 1600×1000pt 창에서 장당 25.6MB, 5K 전체 화면에서 장당 59MB(계산) | 낮음(한 줄 패치) |
| D9 | 이미지 프로토콜 | 미지원(P3) | libghostty 전환 시 kitty graphics | Orca도 Image addon이 선택 사항이다(`pane-inline-images.ts:59-70`). 에이전트 CLI에는 필요 없다 | 낮음 |
| D10 | 창 구조 | M1은 단일 창. 대시보드 팝아웃은 M3에서 1개만 | 다중 창 자유 | 창마다 drawable 수십 MB. Orca도 메인 창 1개와 팝아웃 최대 1개다(`src/main/window/dashboard-popout-window.ts:24-26`) | 낮음(gpui는 다중 창 지원) |
| D11 | 범위 밖 | 파일 에디터, 브라우저 패널, 네이티브 채팅, 자체 STT | – | 각각 Orca에서 수백 개 파일 규모(native-chat 비테스트 423개)이고 메모리 목표와 충돌한다. macOS 기본 받아쓰기는 IME 경로(`insertText`)로 자연스럽게 들어온다 | – |
| D12 | 메모리 지표 | **phys_footprint**(`footprint -p`, 활동 모니터 "메모리")로 통일한다. 에이전트 프로세스는 예산에서 제외한다 | RSS | RSS에는 공유 프레임워크가 섞이고 IOSurface가 빠진다. Orca GPU 프로세스는 RSS 125MB, footprint 1,534MB였다(실측). claude/codex 프로세스 20개 평균 RSS는 340MB였다(실측). 이것이 예산을 쓰면 안 된다 | – |

---

## 3. 상세 설계·요구사항

### 3.1 Rust GUI 후보 비교 (2026-10-14 crates.io·GitHub 확인)

| 후보 | crates.io 최신 | 라이선스 | macOS 렌더 | 텍스트 | macOS 한글 IME 경로 | 접근성 | 다중 창 | 성숙도·위험 | hello footprint(1400×900pt, 실측) |
|---|---|---|---|---|---|---|---|---|---|
| **gpui** | 공식 `gpui` 0.2.2(2025-10-22, 이후 없음). **`gpui-pre` 0.3.9**(2026-10-12, owner huacnlee 1인, zed@93d7456 스냅샷) | Apache-2.0 | Metal 직접 | CoreText | **자체 AppKit NSTextInputClient**(`gpui_macos/src/window.rs:236-295`). 한글 회귀 #13536은 2025-03 수정. open: 한자 #46319, nonactivating panel #62661, Google 일본어 크래시 #64687 | Zed main·gpui-pre: `accesskit_macos`(실험 단계). 0.2.2: 없음 | O | Zed ★91.7k, pre-1.0, 공식 배포 정체 | gpui-pre 76–77MB(IOS 39) / gpui 0.2.2 95–106MB(IOS 59) |
| gpui-component | 0.7.3(2026-10-12), `gpui-pre =0.3.9` 고정 | Apache-2.0 | – | – | gpui 위 | gpui 위 | – | repo `longbridge/gpui-kit` ★17.3k | – |
| iced | 0.14.0(2025-12-07), README "experimental" | MIT | wgpu | cosmic-text | winit + 0.14 Input method(#2777) | 업스트림 없음 | O | Halloy 등 | 71–76MB(IOS 39) |
| egui/eframe | 0.36.2(2026-09-08) | MIT OR Apache-2.0 | wgpu/glow | 자체 | winit. 한자 후보창 #7974 open | AccessKit | O(viewports) | 즉시 모드 | 107MB(IOS 59) |
| Slint | 1.18.1(2026-09-21) | GPL-3.0-only OR Royalty-free-2.0 OR Software-3.0 | femtovg/Skia/wgpu | 자체 | winit. 조합 중 포커스 잃으면 마지막 글자 유실 #10861 open | AccessKit | O | DSL 필수, 라이선스 조건 확인 필요 | 미측정(1차 서브조사 76MB, 조건 미확인) |
| makepad | `makepad-widgets` 1.0.0(2025-05-13), 기본 브랜치 dev는 2026-10-12에도 활동 | MIT OR Apache-2.0 | Metal 직접 | 자체 | 자체 NSTextInputClient(dev `macos_delegates.rs:1281-1315`) | **없음**(#196 open since 2023) | O | 배포 정체, 생태계 작음 | 미측정 |
| Xilem/Masonry | 0.4.0(2025-10-29), vello ^0.6(최신 0.11.0) | Apache-2.0 | Vello(wgpu) | parley | winit 0.30 | AccessKit | O | 자칭 experimental | 미측정 |
| floem | 0.2.0(2024-11-14), main 커밋 2026-09-24 | MIT | wgpu | parley | winit 계열. IME 이슈 #1024 open | 미확인 | 미확인 | Lapce가 git 의존 | 미측정 |
| Dioxus native(Blitz) | dioxus-native 0.7.10 / 0.8.0-alpha.1, blitz 0.2.1 / 0.3.0-beta.3 | MIT OR Apache-2.0 | Vello | parley | winit. 커서 2칸 이동 #894 open | AccessKit(옵션) | 미확인 | 알파·베타 | 미측정 |
| (대안) objc2-app-kit | 0.3.2(2025-10-04) | MIT | 직접 Metal | CoreText 직접 | **NSTextInputClient 직접 구현**(가장 완전) | NSAccessibility 직접 | O | 위젯이 없어 코드량 최대 | 미측정 |

해석:
- 같은 창 크기에서는 **IOSurface(drawable 장수 × 창 크기)가 footprint 차이의 대부분**이다. iced와 gpui-pre는 유휴 시 2장(39MB), gpui 0.2.2와 eframe은 3장(59MB)이었다. 비-IOSurface 부분은 프레임워크 모두 약 32–48MB로 비슷하다.
- 그래서 프레임워크 선택은 메모리가 아니라 **IME 경로**(winit 계열이냐 자체 AppKit이냐)와 **터미널 선례**로 정한다.
- gpui-pre 1400×900 실측 피크는 109MB였다(창 생성 직후 3장 할당으로 추정).

### 3.2 gpui 배포·공급망

- 공식 `gpui`는 crates.io에서 1년째 0.2.2다. 플랫폼 분리 이후의 코드는 `gpui-pre`(개인 1인 소유 재게시)나 Zed git rev로만 얻을 수 있다.
- 결정: `gpui-pre =0.3.9`, `gpui-pre-platform =0.3.9`, `gpui-component =0.7.3`을 정확히 고정하고 `cargo vendor`로 저장소에 사본을 둔다. D7·D8 패치는 vendor 패치로 관리한다(`[patch]`). 업그레이드는 분기마다 한 번, S1 회귀 체크리스트를 통과할 때만 한다.
- 빌드: gpui-pre hello release 빌드는 처음 3분 22초였고 바이너리는 8.5MB였다(실측). 1차 측정에서는 `runtime_shaders` feature를 썼다. 이 머신에는 Xcode 없이 CommandLineTools만 있어 `xcrun metal`로 셰이더를 미리 컴파일할 수 없다. 출시 빌드는 full Xcode 러너에서 프리컴파일 셰이더로 만든다(콜드 시작 단축, 추정).
- 라이선스: gpui·gpui_macos·gpui-component는 Apache-2.0이므로 NOTICE 고지만 하면 된다. **Zed의 `terminal`·`terminal_view`·`editor` 등은 GPL-3.0-or-later이므로 코드를 복사하지 않는다**(C5). Orca(MIT)에서 로직을 차용하면 저작권 고지를 붙인다.

### 3.3 한글 IME 설계 (터미널 위젯 기준)

**AppKit 경로 (gpui):**
- 키가 들어오면 IME 입력 소스가 활성인지 확인하고(`is_ime_input_source_active`, `gpui_macos/src/window.rs:2621`), 활성이면 키바인딩보다 먼저 `inputContext handleEvent:`로 보낸다.
- AppKit이 `setMarkedText:`(L3455) → `insertText:`(L3437) → `firstRectForCharacterRange:`(L3393)를 부른다. gpui는 이를 앱의 `InputHandler`(`replace_and_mark_text_in_range`, `replace_text_in_range`, `bounds_for_range`)로 넘긴다.
- 터미널 위젯은 이 `InputHandler`만 구현하면 된다. `text_for_range`가 `None`이어도 한글 2벌식 조합은 marked text만으로 동작한다(Zed가 같은 구성). 단 재변환(이미 확정한 글자를 다시 한자로 바꾸기)은 동작하지 않는다(추정, S1에서 기록).

**설계 규칙:**
1. **preedit는 PTY로 보내지 않는다.** `replace_and_mark_text_in_range`의 텍스트는 UI 상태로만 저장한다. 커서 셀 위치에 셀 폭으로 정렬해 그리고 밑줄을 친다. 아래 셀은 배경으로 덮고 커서는 숨긴다.
2. **확정 텍스트만 PTY에 UTF-8로 쓴다.** kitty keyboard flag bit 3(모든 키를 escape로)이 켜진 세션에서는 확정 텍스트를 CSI-u로 다시 인코딩해야 하는지 S4에서 확인한다. Orca는 그렇게 한다(`terminal-ime-kitty-commit-encoding.ts:59-71, 115-119`).
3. **조합 중 Enter.** Orca는 브라우저에서 `한`⏎`글`이 `한글\n`으로 뒤바뀌는 버그 때문에 200ms 지연 개행을 둔다(`terminal-ime-deferred-newline.ts:7, 19-21`). 네이티브에서는 AppKit이 `insertText:` 다음에 `doCommandBySelector:`를 부를 것으로 기대한다. 이 순서는 **미확인**이라 S1의 핵심 케이스다. 순서가 보장되지 않으면 IME 상태기계에 "확정 대기 중 개행 보류"를 넣는다.
4. **후보창 위치.** `bounds_for_range`는 커서 셀 사각형에 `cell_width × range.start`를 더해 돌려준다(Zed 방식 `terminal_element.rs:1851-1863`). Claude Code처럼 하드웨어 커서를 숨기는 TUI에서는 마지막으로 보인 커서 위치를 쓴다. 에이전트별 프롬프트 감지(Orca의 Cursor Agent 전용 앵커, C22)는 하지 않는다(YAGNI). 문제가 보고되면 추가한다.
5. **포커스 이탈·탭 전환·창 닫기 시 확정.** 조합 중인 글자는 `unmarkText` 경로로 확정해 PTY로 보낸다(Slint #10861처럼 버리면 안 된다). 창을 닫을 때 marked text가 남아 있으면 panic하는 버그(winit #4626)를 회귀 테스트에 넣는다.
6. **키 반복.** Orca처럼 앱 도메인의 `ApplePressAndHoldEnabled`를 최초 1회만 false로 기록하고, 사용자가 바꾼 값은 존중한다(C23).
7. **입력 소스 전환 직후 첫 자모**(winit #3095), **스페이스 중복**(Alacritty #8079), **CJK 모드에서 첫 특수문자 무시**(Alacritty #6942), **한자 키**(Zed #46319)를 S1 회귀 케이스로 고정한다.
8. **NFD.** macOS 파일명처럼 NFD(초성·중성·종성 자모 분리)로 오는 출력은 alacritty가 첫 자모에 폭 2를 주고 나머지를 zerowidth로 붙인다(1차 실측). 렌더러는 셀의 `zerowidth` 문자까지 한 클러스터로 셰이핑해야 음절로 합쳐 보인다. Zed도 같은 문제를 겪은 이력이 있다(#26036, closed).

### 3.4 터미널 에뮬레이션 크레이트 비교

| 항목 | alacritty_terminal 0.26.0 (2026-04-06) | termwiz 0.23.3 / wezterm-term | vt100 0.16.2 (2025-07-12) | libghostty-vt 0.2.2 (2026-09-28) |
|---|---|---|---|---|
| 라이선스 | Apache-2.0 | MIT | MIT | MIT OR Apache-2.0 |
| 배포·유지 | crates.io, Zed 사용(포크 rev 고정) | termwiz만 crates.io(2025-03-20). wezterm-term은 crates.io에 없음(포크 `tattoy-wezterm-term` 0.1.0-fork.5만 있음) | 마지막 커밋 2025-07-12 | 비공식 바인딩(Uzaaft/libghostty-rs ★394). `-sys` build.rs가 git clone + zig 빌드. 헤더에 "incomplete, work-in-progress API"(`include/ghostty/vt.h:10`) |
| Cell 크기 | **24B**(실측). `Cell { c, fg, bg, flags, extra: Option<Arc<CellExtra>> }`(`src/term/cell.rs:134-140`). Row 헤더 32B | 24B(실측). `Line` 136B | 32B(실측) | **8B**: `packed struct(u64)`(ghostty `src/terminal/page.zig:2137`). 스타일은 페이지별 style map으로 분리 |
| 스크롤백 | `Vec<Row>` 링, 무압축. 120열 1만 줄 +32.1MB, 1,000줄 +3.6MB(실측) | 밀려난 줄 압축 | `VecDeque<Row>` 무압축 | mmap 페이지. LZ4 페이지 압축 모듈 있음(`src/terminal/compress.zig`, `compress/lz4.zig`) |
| 폭 계산 | `unicode-width` 0.2(문자 단위 `c.width()`, `term/mod.rs:1064`) | grapheme 단위 | 문자 단위 | grapheme(mode 2027) |
| 한글 폭 | 2 + spacer | 2 | 2 | 2 |
| VS16 / ZWJ | 별도 처리 없음. ❤️(U+2764 U+FE0F)는 1칸, ZWJ 가족 이모지는 구성 이모지마다 2칸씩 나뉨(`term/mod.rs:1061-1084`) | ❤️ 2칸, ZWJ 2칸 | alacritty와 같음 | grapheme 지원 |
| OSC 8 | O(vte `ansi.rs:1407-1418`, `term/mod.rs:1874`) | O | X | O |
| SGR 마우스 1006 | O(vte `ansi.rs:911`) | O | O | O |
| DEC 2026 | O. vte 0.15.0 Processor가 2MiB 버퍼·150ms 타임아웃으로 처리(`vte/src/ansi.rs:36, 39`) | O | X | O |
| DECSCUSR | O(vte `ansi.rs:1711-1727`) | O | X | O |
| kitty keyboard | O(`KeyboardModes`, vte `ansi.rs:739, 1748-1759`) | O | X | O |
| 리사이즈 reflow | O. 기본 화면만(`term/mod.rs:677`, `grid/resize.rs`) | O | X | O |
| 이미지 | X(#910 open since 2017) | sixel·kitty·iTerm2 | X | kitty |
| 파싱 처리량(실측) | 컬러 diff 135–191MB/s, truecolor TUI 282–349MB/s | 미측정 | 미측정(1차 서브조사 25MB/s, 조건 미확인) | 미측정(Zig 없음) |

**결정 D2의 보완 요구:**
- **이모지 폭.** alacritty의 ❤️ 1칸·ZWJ 분리는 xterm.js + Orca `orca-11-zwj` 프로바이더(`src/shared/terminal-unicode-provider.ts:12, 55-70`)와 다르다. 무엇이 "맞는지"는 에이전트 CLI가 가정하는 폭에 달렸다. S4에서 Claude Code·Codex 출력의 커서 정렬을 비교하고, 어긋나면 래퍼에서 VS16(U+FE0F)과 ZWJ 클러스터 폭을 보정한다. Zed도 포크로 쓰므로 alacritty 포크(rev 고정)는 허용한다.
- **모호폭(Ambiguous) = 1** 고정. 박스 드로잉 U+2500대가 Ambiguous라 2로 잡으면 TUI가 깨진다.
- **2026 sync.** vte Processor가 타임아웃을 처리하지만, 데몬은 PTY를 읽지 않는 동안에도 타임아웃이 지나면 화면을 확정해야 한다. 데몬 이벤트 루프에서 `sync_timeout()` 데드라인을 폴링한다(S4).
- **sync 버퍼 상주.** C18에 따라 세션당 최대 2MiB가 상주할 수 있다. S2에서 Claude Code 10개 실행 후 측정한다.

### 3.5 Zed는 터미널을 어떻게 그리나 (구조 참고만, GPL 코드라 복사 금지)

근거는 Zed `c403bc6`이며 모든 줄 번호를 확인했다.
- **상태**: alacritty 포크 Term을 `Arc<FairMutex<…>>`로 들고(`crates/terminal/src/terminal.rs:1528`), `sync()`(L2498)에서 화면 내용을 `last_content`로 복사한다(L2506). 스크롤백은 기본 10,000줄, 최대 100,000줄이다(L891–892).
- **레이아웃** `layout_grid`(`crates/terminal_view/src/terminal_element.rs:438`): 셀을 줄 단위로 순회하며 wide spacer는 건너뛴다. 기본색이 아닌 배경은 같은 색끼리 이어 붙이고 `merge_background_regions`(L360)로 여러 줄을 사각형 하나로 합친다. 스타일이 같고 열이 연속인 글자는 `BatchedTextRun`(L97)으로 묶는다.
- **그리기** `BatchedTextRun::paint`(L149): `shape_line(text, size, style, Some(cell_width))`(L163–168)로 글리프를 셀 격자에 강제 정렬한다.
- **블록 문자**: 블록 요소·섹스턴트·음영은 사각형으로 칠하고 박스 드로잉은 글리프로 그린다(C14).
- **IME**: `TerminalInputHandler`(L1764)가 marked text를 받는다. paint(L1573)가 L1596에서 읽어 L1683–1725에서 오버레이로 그린다. `bounds_for_range`(L1851)는 후보창 위치를 계산한다.

Minikrill은 이 구조(run 배치, 배경 병합, force_width)를 자체 코드로 새로 작성한다. 다른 점은 "그리드를 UI가 가진 Term에서 읽지 않고 데몬이 보낸 화면 델타에서 읽는다"는 것이다(D4).

### 3.6 메모리 예산 (phys_footprint, 터미널 10개, 120×40 패널, 창 1개 1600×1000pt @2x, 에이전트 프로세스 제외)

| 항목 | 예산 | 근거 | 상태 |
|---|---|---|---|
| 앱: 프레임워크 기본(비-IOSurface) | 35–45MB | gpui-pre 76MB − IOSurface 39MB = 37MB(실측) | 실측 기반 |
| 앱: drawable | 51MB(2장) / 77MB(3장) | 3200×2000×4B = 25.6MB/장. D8 패치로 2장 | 계산. 1400×900에서 39MB(2장) 실측과 일치 |
| 앱: 글리프 아틀라스 | 2–8MB | gpui 아틀라스는 1024² 페이지 단위로 추가된다. 단색 `A8Unorm` 1MiB/페이지, 컬러(이모지) `BGRA8Unorm` 4MiB/페이지(Zed `crates/gpui_apple/src/metal_atlas.rs:124-185`). 한글 음절은 쓰인 것만 래스터한다. 서브픽셀 변형 4개(`SUBPIXEL_VARIANTS_X = 4`, Zed #57777 본문) | 추정 |
| 앱: 터미널 화면 상태(보이는 영역만) | < 3MB | 10 × 200×50 셀 × 24B = 2.4MB | 계산 |
| 앱: UI 상태(트리, 탭, 셰이핑 캐시) | 5–10MB | – | 추정 |
| **앱 소계** | **≤ 120MB**(2장 기준 약 95–115MB) | | 목표 |
| 데몬: 기본(런타임, IPC) | 5–8MB | – | 추정 |
| 데몬: 핫 Term 1,000줄 × 10 | 36MB | 120열 3.6MB/세션(실측) | 실측 |
| 데몬: 콜드 로그 메모리 링 | ≤ 2MB × 10 = 20MB 상한 | 1만 줄 ≈ 240KB 압축(lz4 31% 실측 비율로 계산) | 계산 |
| 데몬: sync 버퍼·PTY 읽기 버퍼 | 0–2.5MB × 10 | 빈 Term 10개 +5.2MB(실측). sync 버퍼 상주 여부는 미측정(C18) | 일부 실측 |
| **데몬 소계** | **≤ 60MB** | 링 상한까지 다 차고 sync 버퍼까지 상주하면 넘을 수 있다. 링 상한을 1MB로 줄이는 것이 1순위 조정 수단이다 | 목표 |
| **합계** | **≤ 180MB 목표, 상한 200MB** | | |
| (금지) 셀 스크롤백 1만 줄 × 10 | 321MB(120열) / 512MB(200열) | 실측 | 채택 금지 |
| (참고) 5K 전체 화면 | drawable 장당 59MB → 2장 118MB / 3장 177MB | 5120×2880×4B | 계산. 전체 화면에서는 예산 초과를 문서화하고, 측정 기준 창은 1600×1000pt로 고정한다 |
| (참고) Orca 같은 시점 | 핵심 4개 프로세스 footprint 2,894MB | 실측(C32) | 비교용 |

### 3.7 측정 계획

- **도구**: `footprint -p <pid>`(카테고리별: IOSurface, MALLOC 등), in-process `proc_pid_rusage(RUSAGE_INFO_V4).ri_phys_footprint`(데몬 자체 계측), `vmmap --summary`. 할당 추적은 Instruments(Allocations, Metal System Trace)로 한다.
- **고정 조건**: 창 1600×1000pt, 외장 5K @2x와 내장 Retina 두 가지, release 빌드, 프리컴파일 셰이더.
- **시나리오**(스크립트 `mk-bench`로 재현):
  1. 기동 후 유휴 10초.
  2. 터미널 10개에서 `claude` 실행 후 유휴 60초.
  3. 터미널 10개에 각각 16MB 컬러 출력(`git log -p --color=always | head -c 16000000`)을 `cat`.
  4. 한글·이모지 혼합 대량 출력(NFD 파일명 포함).
  5. 1시간 소크(에이전트 실작업)에서 1분 간격 footprint 샘플링.
- **합격선**: 시나리오 3 후 앱 ≤ 120MB, 데몬 ≤ 60MB. 소크 1시간 증가율 ≤ 5MB/h.
- **CI**: 데몬은 헤드리스라 macOS CI 러너에서 in-process footprint로 회귀를 감시한다(시나리오 3 대체: 바이트 주입). 앱은 GPU 창이 필요해 셀프호스트 러너나 릴리스 전 수동 측정으로 한다(러너 확보는 T4).

### 3.8 시작 시간과 렌더 성능 목표

| 항목 | 목표 | 근거·측정 |
|---|---|---|
| 콜드 실행 → 첫 프레임 | ≤ 300ms | gpui 0.2.2 hello의 `main` 진입부터 첫 render까지: 1차 339ms(콜드)·150–156ms(웜), 이번 재측정 114–260ms. 모두 런타임 셰이더 컴파일 포함. 프리컴파일 셰이더 전제. 측정: `main` 진입 → 첫 render 타임스탬프와 dyld 포함 `time` 비교 |
| 앱 재시작 → 터미널 10개 마지막 화면 표시 | ≤ 500ms | 보이는 영역 스냅샷만 전송(10 × 200×50 셀, 2.4MB 미만, 계산). T2와 공동 측정 |
| 재연결 상태 표시 | 연결이 끊긴 지 ≤ 1s 안에 터미널마다 "재연결 중" 표시 | 사용자 최대 불만이 "연결 끊김"이다. 조용히 멈춘 화면이 가장 나쁘다(§3.9) |
| 대량 출력(`cat` 100MB, `yes`) 중 프레임 | 60fps 유지, 프레임 p99 ≤ 16.7ms | 데몬이 파싱하고 UI는 vsync당 최신 델타만 그린다. 청크마다 그리지 않는다. Metal System Trace로 측정 |
| 데몬 파싱 처리량 | ≥ 100MB/s/세션 | 실측 135–349MB/s |
| 키 입력 → 화면(로컬) | p95 ≤ 16ms | 입력 에코 델타를 대량 출력 델타보다 먼저 보낸다. Orca는 상호작용 출력용으로 256K **문자**를 예약한다(`src/main/ipc/pty/delivery/constants.ts:8`) |
| 유휴 CPU | 앱 0%(애니메이션이 없으면 프레임 0), 데몬 < 0.5% | gpui는 창이 가려지면 display link를 멈춘다(`window.rs:2993-3006`). 스피너는 저주파 타이머로 그린다(Orca는 CSS `steps()` 트릭, `assets/main.css:1793-1795`, #12359 타이핑 지연 회피) |
| 백프레셔 | UI가 느려도 데몬 메모리 상한 유지 | 데몬은 최신 화면만 유지하고 중간 프레임은 버린다. 원본은 콜드 로그에 남는다 |

### 3.9 Orca 화면 목록과 네이티브 구현 난이도

Orca 렌더러는 비테스트 `.ts/.tsx` 파일이 7,161개다(전체 11,996개). 스택은 React 19.2, zustand 5, radix + shadcn(new-york-v4), Tailwind 4.2, `@tanstack/react-virtual` 3, `@dnd-kit`, monaco-editor 0.55.1, `@xterm/xterm` 6.1.0-beta.304(WebGL addon), Electron 43.7.5다. 난이도: 하(1–3일) / 중(1–2주) / 상(2주 이상, 추정).

| 화면 | Orca 근거 | 핵심 동작 | 난이도 | 마일스톤 |
|---|---|---|---|---|
| 앱 셸(좌 사이드바 / 중앙 / 우 패널, 타이틀바) | `app-shell/AppWorkspaceShell.tsx:115-245`, 리사이즈 `components/sidebar/index.tsx:39-42, 243-249` | 3열과 리사이즈 핸들 | 하(gpui-component dock·resizable) | M1 |
| **터미널 패널 + 분할** | `lib/pane-manager/pane-manager.ts:73-361`, 트리 `src/shared/terminal-tab-types.ts:126-138`, 최소 50px `pane-divider-drag.ts:16` | 이진 분할 트리(leaf / split dir first second ratio) | **상**(그리드 렌더, IME, 선택·복사, 스크롤, 링크) | M1 |
| **연결 상태 표시(재연결 중 / 복원됨 / 데몬 버전 불일치)** | `components/terminal-pane/TerminalErrorToast.tsx:150, 173, 299, 325`, `SessionRestoredBanner.tsx`(`--- session restored ---`), `NewerDaemonHoldBanner.tsx`, 끊긴 동안 입력: `pty-preconnect-input-buffer.ts` | 터미널마다 상태 배너, 데몬 재시작 버튼, 끊긴 동안의 입력 보류 | 중(UI는 쉽고 상태 정의는 T2) | **M1**(1차 보고서 누락) |
| 터미널 탭 바 | `components/tab-bar/TabBar.tsx`, `TerminalTabLeadingIcon.tsx:80` | 드래그 정렬, 상태 점, 우클릭 메뉴 | 중 | M1 |
| 터미널 우클릭 메뉴·링크·파일 경로·드래그 앤 드롭 | `terminal-pane/TerminalContextMenu.tsx`, `terminal-file-link-target.ts`, `terminal-link-handlers.ts`, `terminal-drop-handler.ts`, `terminal-drop-shell.ts`(셸 이스케이프) | 복사/붙여넣기, Cmd+클릭 링크, 파일 끌어 놓으면 경로를 이스케이프해 붙여넣기 | 중 | M1(복사·붙여넣기·OSC 8·드롭), M2(파일 경로 감지) |
| **좌 사이드바 워크트리 트리** | `components/sidebar/WorktreeList.tsx:75`, 그룹핑 `worktree-list/grouping/row-types.ts:8`(none/workspace-status/repo/pr-status) | 가상 리스트, 그룹, 핀, 드래그 | 중상 | M1 기본, M2 그룹·드래그 |
| 워크트리 카드·배지 | `WorktreeCardStatusSlot.tsx:51-71`(미읽음 점), `WorktreeCardAgents.tsx` | 미읽음, PR·CI, 에이전트 하위 행 | 중 | M2 |
| **에이전트 상태 점** | `components/AgentStateDot.tsx:22-46`(11상태), `lib/worktree-status.ts:15-24, 47-74` | M1은 working / waiting(입력 대기) / done / error 4상태로 축소(C27) | 하 | M1 |
| 워크트리 생성·저장소 추가 대화상자 | `components/NewWorkspaceComposerModal.tsx`, `components/new-workspace/*`, `components/sidebar/AddRepoDialog.tsx` | 이름·브랜치·에이전트 선택 | 중(IME 입력 필드 필요) | M2(로직은 T4) |
| 알림(OS 알림, Dock 배지, 토스트) | `src/main/ipc/native-notification-delivery.ts:23`, `hooks/useUnreadDockBadge.ts`, sonner `App.tsx:104` | 입력 대기·완료 알림 | 하~중(UNUserNotificationCenter는 서명된 번들 필요) | M2 |
| 터미널 검색 | `components/TerminalSearch.tsx` | 이전/다음, 하이라이트 | 중(데몬 측 검색, 콜드 로그 포함) | M2 |
| 워크트리 팔레트(mac Cmd+J) / Go to File(Cmd+P) | `components/WorktreeJumpPalette.tsx`, `definitions-core-1.ts:8-13, 35-45` | 퍼지 검색 | 하~중(IME 입력 필드) | M2(팔레트만. Go to File은 범위 밖) |
| Git 변경 패널·커밋 | `components/right-sidebar/source-control/*`(ai, commit, listing, notes, panel, review, sync) | 스테이징·커밋 | 중 | M2(로직 T4) |
| **diff 뷰** | `components/editor/DiffViewer.tsx`, `DiffSectionBody.tsx:5`(Monaco DiffEditor) | side-by-side/inline, 문법 강조, 큰 diff | **상**(가상 스크롤 + tree-sitter 0.27.1 또는 syntect 5.3.0 + 단어 diff) | M2 읽기 전용, M3 코멘트 |
| 설정 | `components/settings/Settings.tsx`(`*Pane.tsx` 39개), `settings/TerminalPane.tsx`, `ShortcutsPane.tsx` | 폰트, 스크롤백, 테마, 알림, 단축키, Option-as-Meta(Orca 기본 `macOptionIsMeta: false`) | 중 | M2 최소, M3 확장 |
| 상태 바 | `components/status-bar/StatusBar.tsx` | 리소스·사용량 | 하 | M3 |
| 플로팅 터미널 | `components/floating-terminal/FloatingTerminalPanelSurface.tsx`(Mod+Alt+A, `definitions-core-1.ts:275`) | 창 안 오버레이 | 중 | M3 |
| 에이전트 대시보드 팝아웃 | `components/dashboard-popout/AgentKanbanBoard.tsx`, 단일 팝아웃 `src/main/window/dashboard-popout-window.ts:24-26` | 칸반 + 축소 터미널 미리보기 | 중상 | M3 |
| 온보딩 | `components/onboarding/OnboardingFlow.tsx:7-11` | 에이전트·테마·알림 단계 | 하 | M3 |
| 파일 에디터 / 브라우저 / 네이티브 채팅 / STT | `components/editor/EditorPanel.tsx`, `browser-pane/*`, `native-chat/`(비테스트 423개), `src/main/speech/stt-worker.ts` | – | 상~최상 | **범위 밖**(D11) |

### 3.10 상용 품질 요구 (1차 보고서에 없던 관점)

- **보안**
  - OSC 52 클립보드 쓰기는 기본 "묻기"로 둔다(T1 이슈 14와 맞춤).
  - OSC 8 링크는 Cmd+클릭으로만 열고, 열기 전에 실제 URL을 툴팁으로 보여준다.
  - 창 제목(OSC 0/2)에 제어 문자가 들어오지 않게 거른다.
  - 여러 줄 붙여넣기는 bracketed paste를 쓰고, bracketed paste를 안 쓰는 프로그램에는 경고한다.
  - 비밀번호 프롬프트용 Secure Keyboard Entry(`EnableSecureEventInput`)를 메뉴 토글로 제공한다(P2).
- **안정성**: 렌더러는 데몬이 보낸 셀 델타를 신뢰하지 않는다. 범위 밖 좌표와 잘못된 UTF-8은 버리고 panic하지 않는다. Zed가 CJK 문자 경계에서 panic했던 이력(#49860, #27599, #53479, 모두 closed)이 있으므로, 한글·이모지 바이트열 퍼징을 데몬 파서와 렌더러 셰이핑 경로 모두에 둔다.
- **접근성**: M2에서 사이드바·탭·버튼의 이름과 역할을 노출한다(gpui-pre의 AccessKit, 실험 단계). 터미널 본문 읽기(VoiceOver)는 P3이다.
- **폰트**: 기본 폰트 체인은 SF Mono → Menlo, 한글은 시스템 폴백(Apple SD Gothic Neo)을 쓴다. 폴백 폰트의 메트릭 차이는 셀 폭 강제 정렬(force_width)로 흡수한다. 한글 굵게가 폴백에서 빠지는 문제(Zed #51460, closed)를 S1에서 확인한다.
- **비용**: 모든 의존성은 crates.io와 vendor 사본으로 오프라인 빌드가 가능해야 한다. Zig·C 툴체인 의존(libghostty-vt, zstd)은 도입하지 않는다.

---

## 4. 위험과 대응

| 위험 | 가능성/영향 | 대응 |
|---|---|---|
| gpui 공식 배포 정체, `gpui-pre`가 개인 1인 소유 | 중/상: 업데이트 중단, API 파손 | `=` 버전 고정, `cargo vendor` 사본, 자체 미러. 터미널 렌더러와 IME 상태기계를 gpui `InputHandler`·페인트 API에만 의존하는 모듈로 분리. 분기별 업그레이드 창구 |
| S1에서 gpui 한글 IME가 실패 | 중/상: D1 재결정 | 1주 안에 패치가 불가능하면 objc2-app-kit + 커스텀 Metal(대안 ①)로 S1을 다시 한다. 결정 규칙을 미리 합의해 둔다 |
| drawable 메모리 | 상/중: 전체 화면 5K에서 2장 118MB, 3장 177MB | D8 패치(3→2). 예산 측정 창 크기를 고정. 창이 가려지거나 최소화되면 drawable 해제 패치를 검토(S2) |
| 글리프 bilinear 블러로 한글 품질 저하(Zed #57777 open) | 중/중: 한국 사용자 첫인상 | D7 패치를 S1에서 Terminal.app·Ghostty와 스크린샷 비교 |
| alacritty 이모지 폭이 에이전트 CLI 가정과 다름 | 중/중: 커서 어긋남 | S4에서 측정하고, 필요하면 래퍼나 포크에서 VS16·ZWJ 폭 보정 |
| vte sync 버퍼(세션당 2MiB 예약)가 상주로 남음 | 중/하: 데몬 +20MB | S2에서 측정. 상주로 남으면 sync 종료 후 `shrink_to`를 넣는 포크 패치나 세션 유휴 시 Processor 재생성 |
| 콜드 로그 재생 페이지가 원래 화면과 다름(리사이즈 이력) | 중/중 | 리사이즈 이벤트를 로그에 함께 기록해 재생 |
| D5 run 배치 렌더러가 대량 출력에서 느림 | 하/중 | S3에서 조기 판정. 셀 쿼드 셰이더로 전환 |
| Zed GPL 코드가 섞여 들어옴 | 하/상: 라이선스 위반 | 기여 규칙에 "Zed `terminal*`·`editor` 코드 복사 금지" 명시. 리뷰 체크리스트 |
| 메모리 기준 혼동(RSS vs footprint) | 중/중: 잘못된 합격 판정 | 모든 문서·CI를 phys_footprint로 통일(D12) |
| 사용자가 끊김을 인지하지 못함 | 중/상: 최대 불만 재발 | 연결 상태 배너를 M1 P0로 승격(§3.9). 끊긴 동안 입력은 보류하고 버리지 않으며, 보류 중임을 화면에 표시 |

---

## 5. 검증 스파이크

순서는 **S1 → S2 → S3·S4(병행) → S5 → S6 → S7 → S8**이다. S1이 실패하면 D1 결정 규칙에 따라 대안으로 S1을 다시 한다.

| ID | 목적 | 방법 | 합격 기준 | 기간(추정) |
|---|---|---|---|---|
| **S1** | gpui 터미널 위젯에서 한글 IME와 한글 렌더 품질 검증 | gpui-pre 0.3.9 창에 `alacritty_terminal` Term을 직결한 최소 그리드 위젯. run 배치 렌더와 `InputHandler` 구현. macOS 기본 2벌식·3벌식, 수동 입력과 화면 녹화 | 다음 10개 케이스를 모두 통과(⑤는 기록만): ① "한글 " 입력 ② "안녕하세요⏎"에서 유실·중복·순서 뒤바뀜 없음(`한`⏎`글` 재정렬 없음, `insertText`→`doCommandBySelector` 순서 로그 첨부) ③ 영↔한 전환 직후 첫 자모 분리 없음(winit #3095) ④ 한글 모드 스페이스 1회 = 공백 1개(Alacritty #8079), 첫 특수문자 유실 없음(#6942) ⑤ 한자 키 후보창 위치(Zed #46319, 결과만 기록) ⑥ 조합 중 포커스 이탈·탭 전환·창 닫기 시 확정, 유실·panic 없음(Slint #10861, winit #4626) ⑦ 조합 중 Cmd+C/V/T가 조합을 깨지 않음 ⑧ Claude Code 입력창에서 ①② 동작 ⑨ 받아쓰기·이모지 피커(Ctrl+Cmd+Space) 입력 ⑩ preedit 밑줄 오버레이가 커서 셀에 표시되고, nearest vs bilinear 아틀라스 스크린샷 비교에서 한글 획 선명도가 Terminal.app 수준 | 1–1.5주 |
| S2 | 메모리 기준선과 drawable·sync 버퍼 | S1 위젯에 터미널 10개. §3.7 시나리오 1·2·3. D8 패치 전후 비교 | 시나리오 3 후 앱 ≤ 120MB, 데몬 ≤ 60MB. drawable 2장 패치 후 프레임 드롭 없음. sync 버퍼 상주량 기록 | 3일 |
| S3 | 대량 출력 렌더 성능 | `cat` 100MB, `yes`, truecolor TUI 재생. Instruments Metal System Trace | 프레임 p99 ≤ 16.7ms, 입력 에코 p95 ≤ 16ms, 유휴 시 프레임 0 | 1주 |
| S4 | 에이전트 TUI 호환 | Claude Code·Codex 각 30분 실사용. Orca(xterm.js)와 같은 장면 스크린샷 비교 | 화면 깨짐 0, 2026 sync 깜빡임 없음, Shift+Enter 등 kitty keyboard 동작, 확정 텍스트 kitty 인코딩 필요 여부 결론, 한글·이모지 폭 정렬 일치 또는 보정 목록 | 1주 |
| S5 | 데몬 핫/콜드 스크롤백과 화면 델타 | 1만 줄 이상 출력 후 UI 재시작, 핫 1,000줄 너머 스크롤 | 마지막 화면 복원 ≤ 500ms. 콜드 재생 페이지가 원본과 셀 단위 일치. 10세션 데몬 ≤ 60MB. D4(델타 vs UI 미러) 확정 | 1주(T2 공동) |
| S6 | 분할·탭·사이드바 골격 | gpui-component dock·tabs·가상 리스트 | 분할 4개, 탭 10개, 사이드바 500행이 60fps로 스크롤. 리사이즈 시 reflow 정상 | 1주 |
| S7 | 접근성 최소선 | gpui-pre AccessKit | VoiceOver가 사이드바·탭·버튼 이름을 읽음 | 2일 |
| S8 | diff 뷰 프로토타입 | 가상 스크롤 + tree-sitter 또는 syntect | 5,000줄 diff 60fps, 앱 메모리 +20MB 이하 | 1주(M2 직전) |

---

## 6. 미확인 항목

| 항목 | 이유 |
|---|---|
| 모든 프레임워크에서의 실제 한글 IME 동작 | 수동 입력이 필요해 자동화하지 못했다. S1에서 처음 확인한다 |
| AppKit의 `insertText:` → `doCommandBySelector:insertNewline:` 순서 보장 | 문서 근거를 찾지 못했다. S1 ②에서 로그로 확인한다 |
| Slint·makepad·Xilem·floem·Blitz의 동일 조건 footprint | 이번에 다시 빌드하지 않았다. Slint 76MB는 1차 서브조사 값이고 창 크기 조건을 확인하지 못했다 |
| floem·Blitz의 다중 창, floem 접근성 | 소스를 확인하지 않았다 |
| libghostty-vt 실측(셀 8B 외 메모리, 속도)과 필요한 Zig 버전 | Zig가 설치돼 있지 않다. 셀 8B는 소스(`page.zig:2137`)로 확인했다 |
| Zed alacritty 포크(rev 4c129667)의 패치 내용 | 포크 diff를 열어 보지 않았다 |
| vte sync 버퍼가 실제 Claude Code 사용에서 얼마나 상주하는지 | 실제 에이전트 출력으로 측정하지 않았다(S2) |
| Claude Code·Codex가 가정하는 이모지·모호폭 폭 | S4에서 측정한다 |
| 실제 Claude Code 출력의 압축률 | 샘플은 git diff 컬러 출력으로 대체했다. 참고로 샘플 13.8MB 지점에 리터럴 `ESC[?1049h`가 있어 마지막 2.2MB는 alt screen에서 파싱됐다. 스크롤백이 이미 가득 찬 뒤라 메모리 수치에는 영향이 없다 |
| gpui-pre 1400×900 피크 109MB의 원인 | 창 생성 직후 drawable 3장 할당으로 추정했고 확인하지 않았다 |
| 앱 메모리 회귀를 CI에서 자동 측정할 GPU 가능 macOS 러너 | T4 영역이며 확보 여부를 모른다 |
| gpui에서 가려진 창의 drawable을 해제하는 패치의 안전성 | 코드만 읽었고 시도하지 않았다(S2) |

---

## 7. Minikrill 이슈 후보

마일스톤 정의(T1과 맞춤): **M1 코어**(스파이크 + 데몬 + 단일 창 + 세션 생존) / **M2 병렬**(워크트리·상태·알림·diff) / **M3 출시 준비**(상용 품질) / **M4 v1.x** / **M5 확장**(이미지 등) / **M6 엔진 재평가**.

| # | 제목 | 마일스톤 | 우선순위 | 완료 조건 | 선행 |
|---|---|---|---|---|---|
| 1 | [S1] gpui 터미널 위젯 한글 IME·렌더 품질 스파이크 | M1 | **P0** | §5 S1 10개 케이스 결과표(통과/실패, 녹화)가 저장소에 기록되고, D1이 확정되거나 대안으로 전환 결정됨 | – |
| 2 | gpui-pre·gpui-component 버전 고정, vendor 사본, `[patch]` 체계 | M1 | **P0** | `Cargo.lock` `=` 고정, 네트워크 없이 `cargo build --offline` 성공, D7·D8 패치가 vendor 패치로 적용됨, 업그레이드 절차 1쪽 | – |
| 3 | [S2] 앱·데몬 메모리 기준선과 drawable 2장 패치 | M1 | **P0** | §3.7 시나리오 1·2·3 footprint 표 기록. 앱 ≤ 120MB, 데몬 ≤ 60MB이거나 초과 원인과 조치가 문서화됨 | 1, 2 |
| 4 | 데몬 VT 엔진: alacritty_terminal 핫 Term(1,000줄) + 2026 sync 데드라인 폴링 | M1 | **P0** | 10세션에 16MB씩 넣은 뒤 데몬 footprint ≤ 60MB(로그 첨부). 종료 신호 없는 sync 스트림이 150ms 뒤 화면에 반영되는 테스트 통과 | – |
| 5 | 콜드 스크롤백: lz4_flex 압축 원본 로그(메모리 링 + 디스크)와 페이지 재생 | M1 | P1 | 1만 줄 너머 스크롤 시 원본과 셀 단위 일치 테스트 통과, 세션당 메모리 링 ≤ 2MB, 리사이즈 이벤트 재생 포함 | 4 |
| 6 | [S5] 화면 스냅샷 + dirty row 델타 전송(UI는 보이는 영역만) | M1 | **P0** | 앱 재시작 후 10세션 마지막 화면 ≤ 500ms. 앱이 스크롤백을 보관하지 않음을 footprint로 확인(T2 공동) | 4 |
| 7 | 터미널 렌더러(클린룸): run 배치, 배경 병합, force_width, vsync 합치기 | M1 | **P0** | [S3] `cat` 100MB 중 프레임 p99 ≤ 16.7ms(Instruments 기록), 유휴 시 프레임 0. Zed GPL 코드 미포함 확인(리뷰 체크) | 1 |
| 8 | IME 상태기계: preedit 오버레이, 확정만 PTY, 포커스 이탈 시 확정, 후보창 앵커, press-and-hold 기본값 | M1 | **P0** | 이슈 1의 회귀 케이스가 릴리스 체크리스트에 들어가고 통과 | 1 |
| 9 | [S4] 에이전트 TUI 호환: kitty keyboard(확정 텍스트 인코딩 포함), OSC 8, SGR 1006, DECSCUSR | M1 | **P0** | Claude Code·Codex 각 30분 사용 시 깨짐 0, Shift+Enter 동작, 링크 Cmd+클릭 동작 | 4, 7 |
| 10 | 터미널별 연결 상태 배너(재연결 중 / 복원됨 / 데몬 버전 불일치)와 끊긴 동안 입력 보류 | M1 | **P0** | 데몬을 SIGKILL하면 1초 안에 모든 터미널에 "재연결 중" 표시. 재연결 후 "복원됨" 표시. 끊긴 동안 친 입력은 유실 없이 보내지거나 보류 상태가 화면에 표시됨(T2 공동) | 6 |
| 11 | 앱 셸(3열) + 분할 트리 + 탭 바 + 레이아웃 복원 | M1 | **P0** | [S6] 분할 4개·탭 10개, 비율 드래그(최소 50px), 앱 재시작 시 레이아웃 복원 | 2 |
| 12 | 터미널 기본 상호작용: 선택·복사·붙여넣기(bracketed), 우클릭 메뉴, 파일 드롭(셸 이스케이프), OSC 52 묻기 | M1 | P1 | 수동 QA 체크리스트 통과. 공백·따옴표가 든 경로 드롭이 셸에서 그대로 열림 | 7 |
| 13 | 사이드바 워크트리 트리(가상 리스트) + 에이전트 상태 점(4상태) | M1 | P1 | 500행 60fps 스크롤, 상태 4종 표시, 유휴 시 프레임 0(로직은 T4) | 11 |
| 14 | 메모리·성능 회귀 스크립트 `mk-bench`(시나리오 1~5) | M1 | P1 | 한 번 실행으로 footprint 표 생성, 데몬 시나리오는 CI에서 자동 실행 | 3 |
| 15 | 이모지·ZWJ·VS16 폭 보정 | M2 | P2 | S4 결과의 불일치 목록이 0이 되는 스냅샷 테스트 | 9 |
| 16 | 한글·이모지 바이트열 퍼징(데몬 파서 + 렌더 셰이핑) | M2 | P1 | 1시간 퍼징 panic 0 | 4, 7 |
| 17 | OS 알림과 Dock 배지 | M2 | P1 | 입력 대기 전환 후 5초 안에 알림, 클릭 시 해당 터미널로 포커스(상태 감지는 T4) | 13 |
| 18 | 터미널 검색(데몬 측, 콜드 로그 포함) | M2 | P2 | 1만 줄 이상 이력에서 다음/이전 이동과 하이라이트 | 5 |
| 19 | 워크트리 팔레트(mac Cmd+J) | M2 | P2 | 한글 IME로 퍼지 검색 입력, 1,000개 항목 응답 ≤ 50ms | 8, 13 |
| 20 | [S8] diff 뷰(읽기 전용, 가상 스크롤, 문법 강조) | M2 | P1 | 5,000줄 diff 60fps, 앱 +20MB 이하 | 11 |
| 21 | Git 변경 패널·커밋 UI | M2 | P1 | 스테이징·언스테이징·커밋이 UI에서 끝남(로직 T4) | 20 |
| 22 | 워크트리 생성·저장소 추가 대화상자 | M2 | P1 | 한글 이름 입력 가능, 생성 후 사이드바 반영(로직 T4) | 13 |
| 23 | 설정 최소판(폰트, 스크롤백 줄 수, 테마, Option-as-Meta, 알림, 단축키) | M2 | P2 | 재시작 없이 반영되고 디스크에 저장 | 11 |
| 24 | [S7] 접근성 최소선(AccessKit 이름·역할) | M2 | P2 | VoiceOver가 사이드바·탭·주요 버튼 이름을 읽음 | 11 |
| 25 | 보안 강화: OSC 8 URL 미리보기, 제목 제어문자 필터, 여러 줄 붙여넣기 경고, Secure Keyboard Entry 토글 | M3 | P2 | 각 항목 테스트 또는 QA 체크 통과 | 12 |
| 26 | 상태 바·플로팅 터미널·온보딩 | M3 | P3 | 각 화면 동작과 유휴 CPU 0% 유지 | 11 |
| 27 | 대시보드 팝아웃(칸반 + 축소 미리보기) | M3 | P3 | 두 번째 창을 띄울 때 앱 footprint 증가 ≤ 40MB | 3, 11 |
| 28 | diff 인라인 코멘트 | M4 | P3 | 코멘트 작성·표시·저장 | 20 |
| 29 | 이미지 프로토콜(kitty graphics) | M5 | P3 | 이슈 30의 재평가 결과에 따라 진행 여부 결정 | 30 |
| 30 | libghostty-vt 재평가 | M6(2027 상반기) | P3 | C API 안정 태그, Zig 없이 빌드 가능 여부, 셀·스크롤백 메모리 실측 비교 메모 | – |

---

### 부록: 이번 검증의 재현 방법

- 크레이트 메타데이터: `curl -A ... https://crates.io/api/v1/crates/<name>`(`/owners`, `/<ver>/dependencies` 포함), 이슈: `gh issue view -R <repo> <N> --json state,title,closedAt`. 크레이트 tarball은 `/tmp/t3v-meta/x/`에 풀었다(실행하지 않음).
- 소스 줄 번호: `gh api -H "Accept: application/vnd.github.raw" "repos/zed-industries/zed/contents/<path>?ref=c403bc6af644cd0ab60c90045ac6dc3436218809"` + `grep -n`. 사본은 `/tmp/t3v-src/`에 있다.
- 메모리·처리량: `/tmp/t3v-meas/`(`sz/` Cell 크기, `m/` Term 메모리, `m/src/bin/tp*.rs` 처리량, `cz`·`rz9/` 압축). in-process `proc_pid_rusage(RUSAGE_INFO_V4)`와 `ps -o rss`.
- GUI footprint: 각 hello 바이너리를 6–7초 실행한 뒤 `footprint -p <pid>`. gpui-pre 동일 조건 재빌드는 `/tmp/t3v-me/gp2`(1400×900pt, 50줄, `runtime_shaders`). 실행한 프로세스는 모두 종료했다.
- Orca: `ps -axo pid,ppid,rss,command | grep -i orca`, `footprint -p <pid>`, 데몬 자식 `login` 프로세스 수.
