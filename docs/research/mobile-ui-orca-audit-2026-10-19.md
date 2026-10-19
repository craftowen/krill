# Krill 모바일 PWA ↔ Orca 모바일 UI/UX 대조 감사

- 작성: 2026-10-19, 읽기 전용 감사(코드·문서 수정·커밋 없음)
- 대상(Krill): 브랜치 `craftowen/mobile-speed` = `571f4ba4`, `crates/krilld/src/mobile/assets/`(아래 `K/`), `crates/krilld/src/mobile/pwa_rpc.rs`
- 기준(Orca): `/tmp/orca-ref/orca/mobile/`(아래 `M/`, React Native, MIT, 커밋 `2d0fa93`)
- 규범: `AGENTS.md`, `docs/STYLEGUIDE.md`, 기존 명세 `docs/research/orca-mobile-ui-spec.md`(아래 "명세")
- Krill 자산은 축약(minified) 파일이라 `파일:줄`의 한 줄에 여러 규칙이 들어 있다. 식별을 돕도록 선택자·함수명을 같이 적었다.

## 0. 결론

1. **골격은 Orca를 충실히 따랐다.** 토큰, 홈 카드, 워크스페이스 목록, 128px 탭, 20개 특수키(순서·바이트·반복 400/45ms), direct/buffered 입력, Fit to Phone 아이콘, 핀치 폰트 프리셋, 액션 시트가 모두 같다. 상태 색은 Krill STYLEGUIDE를 따르며 이는 **의도된 차이**로 올바르다.
2. **실렌더에서 바로 보이는 결함이 5개 있다.** ① buffered 입력줄 레이아웃 깨짐: 전역 `form` 규칙이 새어 들어와 송신 버튼이 아래 줄로 떨어진다(P0). ② 오류 토스트가 첫 연결 뒤로는 사라지지 않는다. ③ 같은 탭에서 페어링 URL을 붙여넣으면 해시만 바뀌어 페어링이 시작되지 않고, 비밀 fragment가 주소창에 남는다. ④ 터미널 탭에 상태 표시가 없다. ⑤ 터미널 글자를 선택·복사할 수 없다.
3. **Orca에도 없는 개선 중 효과가 가장 큰 것은 "Needs You 우선" 흐름이다.** 홈 대기 요약, 탭 상태 점, 승인 전 맥락 3줄 표시가 여기에 든다. 서버가 이미 보내는 `lines` 필드는 지금 UI 어디에서도 쓰이지 않는다.

판정 분포(§2.2–2.8 요소 85행, 스크립트로 집계): 일치 35 · 부분 28 · 누락 9 · 의도적 차이 11 · Krill 고유 2. 라우트 대응 16행은 §2.1에 따로 정리했다.

## 1. 실제 렌더 비교 방법과 산출물

- 격리 빌드: `CARGO_TARGET_DIR=/tmp/mx/target-ui cargo build -p krilld -p krill -p krill-holder`가 성공했다(32초).
- 격리 데몬: `KRILL_STATE_DIR=/tmp/mx/uiaudit/state`에서 holder→krilld를 기동했다. state 디렉터리가 0755면 krilld 소켓 bind가 `PermissionDenied`로 조용히 실패하므로 0700으로 바꿔 해결했다. 이것도 실사용자 진단성 측면의 관찰이다. 기본 로그 위치는 공유 `~/Library/Logs/Krill`이다.
- 게이트웨이: `krill mobile pair --address 127.0.0.1:52987 --json`(LAN 모드)로 열고 loopback URL로 접속했다. 운영 52931과 운영 krilld는 건드리지 않았다.
- 시나리오: 데모 git repo(`feature/mobile-ui`, 변경 2파일)에 세션 2개, 폴더 세션 1개를 만들었다. `krill hook claude PermissionRequest`로 Waiting 상태를, `krill hook codex UserPromptSubmit`로 Working 상태를 만들었다. krilld를 kill -9 해 끊김과 자동 재연결도 확인했다.
- 브라우저: agent-browser 헤드리스, `set device "iPhone 14"`(390×844, dpr 3, iOS UA로 실측 확인). 데스크톱 GUI 입력은 쓰지 않았다.
- 종료: 내가 띄운 holder·krilld·세션 셸 3개·브라우저 세션을 모두 종료했다. 52987 리스너가 닫힌 것도 확인했다.
- **Orca는 RN이라 실행하지 않았고 소스로만 대조했다**(명세 §5와 같다).
- **미확인**: 실제 iOS 키보드·IME·safe-area, 실기 Safari 렌더, Claude Code 권한 프롬프트에 `y⏎`가 맞는지(§3 P1-5).

스크린샷(`/tmp/mx/uiaudit/shots/`, 1170×2532):

| 파일 | 화면 |
|---|---|
| 01-home-unpaired | 페어링 전 홈 |
| 02-pair-confirm | 페어링 확인 |
| 03/04-home-connecting/connected | 연결 직후 홈(연결이 빨라 03과 04가 같다) |
| 05-workspaces | 워크스페이스 목록(상태 Unknown) |
| 06-workspace-options-sheet | 필터 시트 |
| 07/08-workspaces-search(-empty) | 검색 / 결과 없음 |
| 09-session-terminal | 세션·터미널(80×24, 가로 넘침) |
| 10-session-fit-to-phone | Fit to Phone(48×24) |
| 11-session-buffered | **buffered 입력줄 깨짐** |
| 12-session-actions-sheet | 터미널 액션 시트 |
| 13-session-history | 스크롤백 상단 |
| 14/15-git-list/diff | Source Control 목록·diff |
| 16-accounts | 계정 사용량(Unknown이 움직이는 막대로 보임) |
| 17-orchestration-empty | 오케스트레이션 빈 상태 |
| 18-settings / -terminal / -about / diagnostics | 설정·터미널 설정·About·연결 로그 |
| 19-workspaces-states | Waiting(주황) / Working(노랑 링) 행 |
| 20/21-session-waiting(-actions) | 대기 탭(탭에 표시 없음) / Approve 시트 |
| 22-home-with-waiting | 대기 세션이 있어도 홈에 신호 없음 |
| 23/24-session-disconnected/reconnected | krilld 강제 종료 → 약 6초 안에 자동 재연결 |

## 2. 화면 목록과 대응표

### 2.1 Orca 라우트 전부 → Krill 대응

| Orca 라우트(`M/app/…`) | Krill 대응 | 판정 | 근거 |
|---|---|---|---|
| `index.tsx` 홈·호스트 목록 | `#/` `screens/home.js` | 부분 | §2.2 |
| `pair.tsx`, `pair-confirm.tsx` | `#/pair-confirm` `app.js:29` | 부분 | §2.7 |
| `pair-scan.tsx`(카메라·코드 붙여넣기) | 시트의 URL 붙여넣기 `app.js:25` | 의도적 차이 | 명세 §3.2: 의존성 없는 QR 디코더가 없어 OS 카메라로 링크를 연다 |
| `mobile-onboarding.tsx`, `notification-opt-in.tsx`, `notifications.tsx` | 없음 | 의도적 차이 | 명세 §3.2: 푸시 preference API가 없고, 미구현 기능은 UI에 표기 금지 |
| `settings.tsx` | `#/settings` `screens/settings.js:3` | 부분 | §2.7 |
| `terminal-settings.tsx` | `#/settings/terminal` `settings.js:5` | 부분 | 순서 바꾸기·custom shortcut·autocorrect 없음 |
| `native-chat-settings`, `browser-settings`, `voice-settings` | 없음 | 의도적 차이 | 해당 RPC 없음(명세 §3.2) |
| `troubleshoot.tsx` → `connection-log.tsx` | `#/diagnostics` `settings.js:6` | 부분 | 레벨 글리프·상대시간·Copy report 없음 |
| `about.tsx` | `#/settings/about` `settings.js:4` | 일치(범위 축소) | 외부 링크 없음(확인된 항목만) |
| `h/[hostId]/index.tsx` 워크스페이스 목록 | `#/host/local` `screens/workspaces.js` | 부분 | §2.3 |
| `h/[hostId]/accounts.tsx` | `#/accounts` `details.js:2` | 부분 | §2.6 |
| `h/[hostId]/tasks.tsx` | `#/orchestration` `details.js:4` | 의도적 차이 | Provider Tasks와 Krill 오케스트레이션은 다른 기능(명세 §3.2) |
| `h/[hostId]/edit.tsx` 호스트 편집 | 없음 | 의도적 차이 | 단일 origin 데몬(명세 §3.1) |
| `h/[hostId]/session/[worktreeId].tsx` | `#/host/local/group/<key>/s/<id>` `session.js` | 부분 | §2.4, §2.5 |
| `source-control/[worktreeId]` | `#/git/<id>` `details.js:3` | 부분 | 읽기 전용, 파일별 상태·± 없음 |
| `review/`, `pr/`, `history/`, `agent-history/`, `files/`, `files/preview/` | 없음 | 누락(백엔드 없음) | 명세 §3.2에서 제외. UI에 노출되지 않음을 실렌더로 확인 |

### 2.2 홈

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 상단 바(로고 + 이름 17, Settings 36/r18) | `M/src/home/MobileHomeTopBar.tsx` | `K/app.css:6 .home-topbar,.brand`, `home.js:3` | 일치 |
| `Welcome back` 24/800/−0.3 | `MobileHomeListHeader.tsx` | `app.css:6 .home-list>h1` | 일치 |
| 통계 3칸 | `MobileHomeListHeader.tsx:24`(stats가 있을 때만) | 없음 | 의도적 차이(집계 RPC 없음, 명세 §3.2) |
| 호스트 카드(46 아이콘/r13, 15/600, 점 8, r14) | `components/MobileHostCard.tsx` | `app.css:6 .host-card…`, `home.js:4` | 일치 |
| 카드 ⋮ 시트(Connect/Reconnect/Disconnect/Diagnostics/Edit/Remove) | `host-list-action-sheet-actions.ts` | `home.js:4`: Open workspaces / Reconnect 2개 | 부분(실재 기능만, 타당) |
| **호스트 0개 빈 상태**: `Connect your desktop` 히어로만, `Welcome back`·카드 없음 | `MobileHomeScreen.tsx:107-113` | `home.js:3-5`: 미페어링에도 `Welcome back` + "This Mac / Not connected" 카드 + 안내 | **부분**(첫 방문에 "Welcome back"과 가짜 호스트 카드, 01번 캡처) |
| 3단계 안내(제목 + 설명, 중앙 히어로 버튼) | `MobileHomeEmptyState.tsx` | `home.js:5`: 제목만, 설명 없음, 버튼 좌측 정렬 | 부분 |
| Resume 카드 / Quick Actions / 계정 카드 | `MobileHomeListFooter.tsx`, `MobileHomeResumeCard.tsx` | Accounts·Orchestration 링크 행 `home.js:5` | 부분(Resume 없음) |
| 주의 필요 신호 | Resume 카드·알림 | 없음. 대기 세션이 있어도 홈 변화 없음(22번 캡처) | 누락 |

### 2.3 워크스페이스 목록

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 헤더(뒤로 32, 이름 15/600 + 점 gap8, descriptor, Reconnect 칩) | `host-screen/host-screen-header.tsx` | `workspaces.js:2`: 점이 헤더 **오른쪽 끝**, descriptor·Reconnect 없음 | 부분 |
| 툴바(Options 칩 r12, Accounts/Tasks/Search 16) | `host-screen-primary-styles.ts` | `app.css:7 .workspace-toolbar,.filter-chip`, `workspaces.js:3` | 일치(Tasks→Orchestration) |
| 필터 시트 현재 선택 표시 | 활성 칩 + 배지 | 칩만 활성. 시트에 체크 없음(06번 캡처) | 부분 |
| 검색(42, r6, 16px 입력) | `components/MobileSearchField.tsx` | `app.css:7 .workspace-search`, 브라우저 기본 X | 일치(clear 칩만 다름) |
| 그룹 헤더 11/600/uppercase + 수 | `host-workspace-list.tsx` | `app.css:7 .repo-group>h2` | 일치 |
| 행(세로10·좌우16, 투명 border-left 2, separator 왼쪽40) | `components/WorktreeListRow.tsx` | `app.css:7 .workspace-row` | 일치 |
| 행 우측 터미널 수 | 있음 | 있음 + chevron | 일치(chevron 추가) |
| **에이전트 활동 줄(inline agents)** | `WorktreeListRow.tsx:189-198` | 없음. 서버는 `lines`(마지막 3줄)를 매번 보냄 `pwa_rpc.rs:361-370`, UI 미사용 `workspaces.js:7` | 누락 |
| 미읽음 Bell, PR·repo 칩, lineage | `WorktreeListRow.tsx:100-125` | 없음 | 의도적 차이(데이터 없음, 명세 §3.2) |
| 행 long press 400ms + haptic | `WorktreeListRow.tsx:5,88` | 행 long press 없음(탭에만 있음) | 누락(경미) |
| FAB New workspace | `components/NewWorkspaceFab.tsx` | 없음 | 의도적 차이(워크트리 생성 RPC 없음) |
| Pull-to-refresh | `host-workspace-list.tsx:189` | 없음(2.5초 폴링만) | 누락 |
| 빈 상태 3종 문구 | `worktree/host-workspace-list-states.tsx:39-40` | `workspaces.js:8` 같은 문구 | 일치 |
| 로딩 spinner | `host-workspace-list-states.tsx:18` | 스냅샷 전에는 헤더 "Krill" + 상태 문장 `app.js:30` | 부분 |

### 2.4 세션 화면 chrome

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 헤더(뒤로 36/r18, 제목 14/600, 요약 12, 우측 36 아이콘) | `session/MobileSessionHeader.tsx` | `app.css:5 .page-header`(52 높이, 제목 15), `session.js:5-7` | 일치(±1) |
| 요약 줄 StatusDot + tap reconnect | `MobileSessionHeader.tsx:98-105` | 텍스트 "Connected"만. 점·tap 없음 | 부분 |
| 탭 128/36/13, 활성 밑줄 | `mobile-session-frame-styles.ts` | `app.css:8 .session-tab` | 일치 |
| 탭 아이콘 = 감지된 agent 브랜드 아이콘 | `MobileSessionHeader.tsx:188-193` | 항상 `>_` 아이콘, 라벨 `claude`/`Terminal N` `session.js:18` | 부분 |
| **탭별 상태(대기 탭 식별)** | agent 아이콘 + 목록 점 | 없음(20번 캡처: 대기 탭이 일반 탭과 같음) | 누락 |
| `+` 탭(우측 고정 40×36) | `MobileSessionHeader.tsx:217` | `session.js:8`(`capabilities.session_create`일 때만) | 일치 |
| 탭 long press → 액션 시트 | 400ms | `session.js:18 longPress`, contextmenu | 일치 |
| 더보기 시트 | Agent History / Checks | Approve·Deny(대기 시) / Git / Send prompt / Handoff / Close `session.js:10` | 의도적 차이(Krill 기능) |
| 활성 탭 자동 스크롤 | 있음 | `scrollIntoView` `session.js:18` | 일치 |

### 2.5 터미널 화면

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 렌더러 | xterm WebView | delta 모델 + canvas `terminal.js:164-171` | 의도적 차이(명세 §2.1, 질의 응답자 krilld 단독) |
| 테마 Tokyonight | `terminal-webview-html/theme.ts` | `terminal.js:166-170` 같은 팔레트 | 일치 |
| 폰트 13px, 프리셋 [0.5…2], 핀치 snap | `document/text-scaling.ts`, `terminal-text-scales.ts` | `terminal.js:38,90-93,134` | 일치 |
| 특수키 20개 순서·바이트 | `terminal/terminal-key-definitions.ts` | `terminal.js:39-45` | 일치 |
| 반복키 즉시 → 400ms → 45ms | `terminal-accessory-repeat.ts` | `terminal.js:96-102` | 일치 |
| 키 행 순서 Dismiss → Fit → Direct → (Paste) → 20 → custom → + | `MobileSessionCommandDock.tsx:95,136-138` | Dismiss → Fit → Direct → 20 `terminal.js:72-75` | 부분(Paste·custom·+ 없음) |
| 키 칩(bgRaised r6 최소 36, 12 mono) | `mobile-session-command-input-styles.ts` | `app.css:8 .accessory-key` | 일치 |
| Dismiss keyboard 고정 버튼(키보드 열림 시) | `MobileSessionCommandDock.tsx:95` | `terminal.js:72,130`(inset > 80) | 일치 |
| Direct 기본, 세션별 모드 유지 | `mobile-terminal-direct-input-default.md` | `terminal.js:58-59,124` | 일치(localStorage 영속은 Krill 추가) |
| Live input 바 문구 | `Live input` / `Tap to show keyboard` | `terminal.js:76` 같다 | 일치 |
| buffered 입력줄(필드 34 + 송신 34 한 줄) | `mobile-session-command-input-styles.ts` | **깨짐**: 송신 버튼이 둘째 줄 왼쪽으로 떨어짐(11번 캡처). 원인: 전역 `form{display:flex;flex-direction:column}` `app.css:9`가 `.buffered-command`(`app.css:8`, flex-direction 미지정)에 적용 | **누락(버그)** |
| IME 조합 중 미전송·commit 후 키 순서 | live-input hooks | `terminal.js:106-115` generation 검사 | 일치(실기 미확인) |
| Fit to Phone(Monitor↔Smartphone 아이콘, ACK 후 확정) | `MobileSessionCommandDock.tsx:136-138` | `terminal.js:159` | 일치 |
| Phone fit 시 rows도 맞춤 | `session/mobile-terminal-viewport-resubscribe.ts`(PHONE 40×50 테스트) | cols만 바꾸고 rows는 유지 `terminal.js:160` → 하단 약 180px가 빈 띠(10번 캡처) | 부분 |
| 기본(비 fit) 상태 가로 넘침 | 데스크톱 모드는 의도적으로 데스크톱 크기 | 80열 = 640px / 뷰포트 390 → 가로 스크롤(09번) | 일치(정책상 같다) |
| 스크롤백 | xterm 5000행 + 스크롤 indicator `document-style.ts:58-69` | hot/cold 100행 페이지, 상단 30px에서 자동 로드, 최대 1000행 `terminal.js:85,162-163` | 의도적 차이(명세 §2.1) |
| 스크롤백 조작 UI | 없음(스크롤만) | `80 × 24` 상태줄 + `Load earlier output` 버튼이 터미널 아래 상시 58px 차지 `terminal.js:71,146` | 부분(디버그성 chrome) |
| **long press 텍스트 선택·Copy** | `terminal/document/selection-overlay.ts`, `selection-menu-buttons.ts:7` | 없음(canvas라 선택 불가, `aria-label`에 텍스트만) `terminal.js:171` | 누락 |
| TUI 마우스 휠 라우팅 | `surface-touch-gestures.ts` | 없음 | 의도적 차이(명세 §2.1) |
| 지연 로드 `Terminal is taking longer…` + Retry | `MobileSessionActiveContent.tsx` | 15초 timeout 문구 + 지수 백오프 자동 재시도 `terminal.js:138`, `session.js:16`. 수동 Retry 없음 | 부분 |
| 끊김 시 표현 | StatusDot amber, 재연결 | 상단 배너 + 상태 "Disconnected". 기존 화면이 그대로 밝고 키가 활성처럼 보임(23번). 약 6초 안에 자동 재연결(24번) | 부분 |
| 입력 불확실 시 재전송 금지 문구 | — | `terminal.js:174` | Krill 고유(좋음) |
| 사진 첨부(tap 사진, long press 350ms 파일) | dock 사진 버튼 | `terminal.js:80-82`, 축소·압축 `app.js:26` | 일치 |
| 마이크·dictation | 있음 | 없음 | 의도적 차이(API 없음) |

### 2.6 보조 화면(계정·Git·오케스트레이션)

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 계정 카드·5h/7d 사용량·전환 | `home/MobileHomeAccountUsageCards.tsx`, `accounts.tsx` | `details.js:2`, `app.css:9 .account-card` | 부분 |
| 사용량 미상 표현 | null을 0%로 오인하지 않음 | 라벨은 "Unknown"이지만 값 없는 `<progress>`가 **움직이는 불확정 막대**로 그려져 로딩처럼 보임(16번) `details.js:2` | 부분(버그) |
| Source Control 목록·diff | `source-control/…`(staged/unstaged, 파일별 상태) | 단일 목록 + 하단 `pre` diff, 색 `app.css:12` `details.js:3` | 부분(명세 §3.3 D 전) |
| diff 색 토큰 | `chatDiffAdded/Removed`, `diffAddedBg` | 같은 토큰 `app.css:12` | 일치 |
| 오케스트레이션 Needs You(gate/question) | (Orca 없음) | `details.js:4` 카드·버튼·답장 폼 | Krill 고유 |

### 2.7 시트·설정·페어링·로그

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 액션 시트(scrim 0.5, 상단 r16, 핸들 36×4, 그룹 bgPanel r12, 행 아이콘 16, 행 사이 hairline) | `components/ActionSheetModal.tsx`, `bottom-drawer-styles.ts:14,51` | `<dialog>` scrim 0.55, r20, 핸들 같음, **아이콘·그룹 상자·구분선 없음**, 별도 Cancel 버튼 `app.css:10`, `app.js:22` | 부분 |
| 시트 열릴 때 첫 항목에 포커스 링 | 없음 | `showModal()` 자동 포커스 + `:focus-visible`이 첫 항목을 "선택됨"처럼 강조(12·21번) | 부분 |
| destructive 빨강 | statusRed | `--status-blocked`(#ff6568 = Krill destructive) `app.css:10` | 의도적 차이(STYLEGUIDE:53) |
| 설정 그룹(bgPanel r12 상자, 행 사이 hairline 좌우12) | `settings/mobile-settings-menu.tsx:90` | 행마다 각진 패널 + border-bottom, 그룹 라운드 없음 `app.css:9 .setting-link` | 부분 |
| 설정 헤더 20/700 | 같은 파일 | 공통 page-header 15/600 | 부분 |
| 터미널 설정: Text size / 입력 모드 / 키 표시 | `app/terminal-settings.tsx` | `settings.js:5` 셀렉트 2 + 체크박스 20 | 부분 |
| 키 순서 끌어 바꾸기·custom shortcut | `components/TerminalShortcutSettings.tsx:231`(DragReorderList) | 코드는 `orderedKeyIds`를 지원 `terminal.js:74`. 설정 UI 없음 | 누락 |
| 페어링 확인(`Pair with this desktop?` 18/600 중앙, Pair r6, Cancel 텍스트) | `app/pair-confirm.tsx:153,226-272` | 좌측 정렬, 22px 제목, 빈 상태 문구 재사용, Cancel이 테두리 버튼 `app.js:29` | 부분 |
| 연결 로그(글리프 •✓!✕, +초 상대시간, mono 12) | `components/ConnectionLog.tsx:22-25` | 시각 + 메시지만, `toLocaleTimeString`이라 영어 UI 안에 "오전 8:40:10"이 섞임 `settings.js:6` | 부분 |

### 2.8 토큰·타이포·상태 표시·접근성

| 항목 | Orca | Krill | 판정 |
|---|---|---|---|
| 크롬 색(bgBase #111, panel #1a1a1a, raised #242424, border #2a2a2a) | `M/src/theme/mobile-theme.ts` | `app.css:3` 원문 그대로 | 일치. 단 Krill 데스크톱 Palette(#0a0a0a / card #171717 / border rgba 0.08, STYLEGUIDE §3.1)와는 다르다. 명세 §4.1은 "Krill 의미 토큰과 맞춤"을 요구했으므로 **결정 필요** |
| 간격 4/8/12/16/24, 반경 row 6 / card 14 | 같은 파일 | `app.css:3` | 일치 |
| 타이포 18/14/12, 입력 16px(Safari 줌 방지) | 같은 파일, `text-input-font-size.web.ts` | `app.css:3-4` | 일치 |
| Working 노랑 회전 링 1000ms linear | `AgentStateDot.tsx:22`, `use-working-ring-rotation.ts` | `app.css:5 .status-dot.working` | 일치 |
| Waiting 색 | 빨강 `#ef4444` `AgentStateDot.tsx:16` | 주황 `#f97316` `app.css:3` | 의도적 차이(STYLEGUIDE:65,75 agent_question) |
| Blocked / Done 색 | `#ef4444` / `#10b981` | `#ff6568` / `#86efac` | 의도적 차이(STYLEGUIDE:53,63) |
| 점 크기 | AgentStateDot 6(10 래퍼) | 8 `app.css:5` | 부분 |
| 그룹 rollup 우선순위 | — | Blocked > Waiting > Working… `app.js:16` vs 데스크톱 Waiting → Blocked → Working(STYLEGUIDE:208) | 부분(데스크톱과 어긋남) |
| 모션 감소 | 정적 glyph | `prefers-reduced-motion`이면 애니메이션 전부 끔 `app.css:10` | 일치 |
| 320px·가로 모드 | — | `@media(max-width:340px)`, landscape `app.css:10` | 일치 |
| 아이콘 버튼 이름 | accessibilityLabel 539곳 | 모든 `button()`에 `aria-label` `app.js:13`, tablist/tab/aria-selected `session.js:7,18`, dialog focus trap | 일치 |
| 상태 점 접근성 이름 | `No recent update` 등 문장 | 원시 enum(`Unknown`, `connected`)을 `aria-label`로 씀 `app.js:15`. 홈 카드 이름이 "This Mac **Unknown** Not connected"로 읽힘(스냅샷 실측) | 부분 |
| 터치 영역 | 키 최소 36×~28 | 키 28, 툴바 아이콘 28×28 `app.css:7-8`. 둘 다 44 미만 | 일치(둘 다 작음) |
| canvas 스크린리더 | xterm a11y | 매 draw마다 화면 전체를 `aria-label`로 갱신 `terminal.js:171` | 부분 |
| 오류 알림 | Alert/인라인 | `#notice role=alert`. **첫 연결 뒤 발생한 오류는 자동으로 사라지지 않음**(`app.js:11`에서 숨기는 곳은 첫 ready 때 `app.js:35`와 사진 성공뿐). `pointer-events:none`이라 화면 하단을 계속 가림 | 누락(버그) |

## 3. 발견된 결함과 개선 제안

### P0

| # | 문제 | 근거 | 제안 | 효과 | 난이도 |
|---|---|---|---|---|---|
| P0-1 | buffered 입력줄 레이아웃이 깨짐(송신 버튼이 둘째 줄로 떨어지고 dock 높이가 커짐) | 11번 캡처, `app.css:9 form{…flex-direction:column;gap:12px}`가 `app.css:8 .buffered-command`에 적용 | `.buffered-command{flex-direction:row;gap:8px}`를 추가하거나 전역 `form` 규칙을 `.detail-list form, .sheet form`으로 한정. 회귀 방지용 레이아웃 스냅샷 1장 | 설정에서 고를 수 있는 입력 모드 하나가 실제로 쓸 수 있게 됨 | 하 |

### P1

| # | 문제 / 제안 | 근거 | 효과 | 난이도 |
|---|---|---|---|---|
| P1-1 | **오류 토스트가 영구히 남음**. 자동 숨김(4–6초) + 탭해서 닫기, 연결 회복 시 연결 관련 오류 정리 | `app.js:11,35` | 한 번의 RPC 오류가 이후 내내 터미널 하단을 가리는 문제 해소 | 하 |
| P1-2 | **같은 탭 페어링 실패**: 시트에서 같은 origin URL을 `location.assign`하면 fragment만 달라 same-document 이동이 되어 bootstrap이 다시 돌지 않음. hashchange는 홈으로 렌더하고 `#pair=…&key=…`가 주소창·history에 남음(02번 전 단계 실측: `location.href`에 key가 남은 채 홈 표시) | `app.js:25`, `client.js:9-12`, `app.js:27,36` | 시트에서 `bootstrap` 해석을 직접 호출해 메모리로 옮기고 `replaceState`로 fragment 제거 후 `#/pair-confirm`. 또는 hashchange에서 `pair=` 감지. 페어링 실패 해소와 비밀 노출 제거(보안 위생) | 하–중 |
| P1-3 | **탭별 상태 표시 없음**. 탭 아이콘 자리에 상태 점(Working 링 / Waiting 주황 / Blocked 빨강), 가능하면 agent 브랜드 아이콘 | 20번 캡처, `session.js:18` vs `M/…/MobileSessionHeader.tsx:188-193` | 여러 탭 중 어느 탭이 나를 기다리는지 즉시 알 수 있음 | 하 |
| P1-4 | **홈 "Needs You" 요약**(Orca에도 없는 Krill 고유). 대기·차단 세션과 오케스트레이션 gate/question 수를 홈 최상단 카드로 보이고 tap하면 해당 세션으로 이동. 워크스페이스 툴바 "Needs attention" 필터에 수 배지 | 22번 캡처, `home.js:5`, `workspaces.js:3` | 앱을 여는 주된 이유(승인 대기 처리)까지 탭 수 감소 | 중 |
| P1-5 | **승인 전 맥락 표시 + Approve 의미 검증**. Approve는 PTY에 `y⏎`를 보낸다. Claude Code처럼 번호·화살표 선택형 프롬프트에서 `y`가 맞는지는 **미확인**. 시트에 마지막 3줄(`lines`, 이미 서버가 보냄)을 보이고, 에이전트별 응답 바이트를 확인하거나 "Enter / 1 / Esc" 같은 실제 키를 노출 | `pwa_rpc.rs:146-151`, `session.js:10-11` | 엉뚱한 입력으로 프롬프트를 망가뜨릴 위험 감소, 화면을 열지 않고 판단 가능 | 중(실기 검증 필요) |
| P1-6 | **터미널 텍스트 선택·복사 없음**. long press → 행 단위 선택 → Copy. 최소안은 "현재 화면 복사" 시트 항목(이미 `aria-label`에 텍스트가 있음) | `terminal.js:171` vs `M/…/selection-overlay.ts` | 에러 메시지·URL·명령을 폰에서 옮길 수 있게 됨 | 최소안 하 / 정식 중 |
| P1-7 | **워크스페이스 행에 활동 미리보기**. 에이전트 세션의 마지막 1줄을 12px mono muted로 표시(Orca inline agents 대응). 안 쓸 거면 서버의 `lines` 계산을 없앨지 속도 워커와 함께 결정 | `pwa_rpc.rs:361-370`(매 스냅샷 캡처), `workspaces.js:7` 미사용, `M/…/WorktreeListRow.tsx:189` | 목록에서 진행 상황을 파악해 불필요한 진입이 줄어듦 | 하 |

### P2

| # | 문제 / 제안 | 근거 | 효과 | 난이도 |
|---|---|---|---|---|
| P2-1 | 헤더 "뒤로"가 `history.back()`이 아니라 새 해시를 push함 → 뒤로 버튼 뒤 iOS 스와이프 백·브라우저 뒤로가 방금 떠난 화면으로 돌아감 | `app.js:14,18` | 이전 항목이 기대 경로면 `history.back()` 사용 | 내비게이션 일관성 | 하 |
| P2-2 | Fit to Phone이 rows는 그대로(24) → 하단 빈 띠. 표시 영역 높이로 rows도 계산 | 10번 캡처, `terminal.js:160` vs Orca phone-fit(cols·rows) | 폰 화면을 꽉 채움 | 하(정책상 desktop 크기 반환 확인 필요) |
| P2-3 | `80 × 24` 상태줄 + `Load earlier output` 버튼이 상시 약 58px 차지. 상단 도달 시 자동 로드가 이미 있으므로 평소에는 숨기고 상태는 오류·로딩 때만 오버레이 | `terminal.js:71,85,146`, 09번 | 터미널 가시 영역 약 3행 증가 | 하 |
| P2-4 | 끊김 상태 표현: 터미널을 흐리게 하고, 키·입력을 비활성으로 보이며, "Reconnecting…" + 수동 Retry. 배너 문구 "Pending actions were not resent"는 보낼 게 없었을 때 불안감을 줌 → 실제 pending이 있을 때만 | 23번, `client.js:57`, `app.js:35` | 오래된 화면을 현재로 오인하는 일 방지 | 하 |
| P2-5 | 사용량 Unknown이 움직이는 progress로 보임 → 값 없으면 progress를 숨기거나 `value=0` + 회색 | 16번, `details.js:2` | 로딩 중이라는 오해 제거 | 하 |
| P2-6 | 미페어링 홈을 Orca 빈 상태로: `Welcome back`·가짜 카드를 없애고 `Connect your desktop` 히어로 + 단계 설명 + 중앙 버튼 | 01번, `home.js:3-5` vs `MobileHomeScreen.tsx:107-113` | 첫 인상·안내 명확 | 하 |
| P2-7 | Pull-to-refresh(또는 목록 상단 "방금 갱신" 표시)와 오프라인 시 마지막 스냅샷에 "오래됨" 라벨 | `M/…/host-workspace-list.tsx:189`, 명세 §4.3 | 신선도 신뢰. 속도 체감은 다른 워커와 함께 | 중 |
| P2-8 | 대기 수를 `document.title`에 반영하고, 설치된 PWA(HTTPS·고정 출처)에서는 `navigator.setAppBadge`(iOS 16.4+) | Orca에 없음 | 앱 밖에서도 대기 인지(알림 없이) | 하–중(secure context에서만) |
| P2-9 | 상태 점 접근성 이름을 사람이 읽는 문장으로("Waiting for you", "Working", "Not connected") | `app.js:15`, 홈 카드 실측 이름 | VoiceOver 품질 | 하 |
| P2-10 | rollup 순서를 데스크톱(Waiting → Blocked → Working → Done → Exited → Unknown)과 맞춤 | `app.js:16` vs STYLEGUIDE:208 | 데스크톱·모바일 같은 판단 | 하 |

### P3

| # | 제안 | 근거 | 난이도 |
|---|---|---|---|
| P3-1 | 액션 시트를 Orca 그룹형으로: bgPanel r12 상자, 행 아이콘 16, hairline. 첫 항목 자동 포커스 링 제거(`:focus-visible`만 남기고 초기 포커스는 시트 컨테이너로) | `app.css:10`, `app.js:22` | 하 |
| P3-2 | Workspace Options 시트에 현재 선택 체크 표시 | `workspaces.js:3` | 하 |
| P3-3 | 설정 그룹을 r12 둥근 상자 + 헤더 20/700로 | `app.css:9` vs `mobile-settings-menu.tsx:90` | 하 |
| P3-4 | 키 순서 바꾸기 UI(위·아래 버튼이면 충분. 저장 경로 `orderedKeyIds`는 이미 있음) | `terminal.js:74`, `settings.js:5` | 하–중 |
| P3-5 | 연결 로그에 레벨 글리프(• ✓ ! ✕), 첫 이벤트 기준 +초, 시간 형식 고정(영어 UI와 로캘 혼용 해소), "Copy log" | `settings.js:6` vs `ConnectionLog.tsx:22-25` | 하 |
| P3-6 | 페어링 확인 화면을 `Pair with this desktop?` 중앙 정렬, Cancel 텍스트 버튼으로 | `app.js:29` vs `pair-confirm.tsx:153` | 하 |
| P3-7 | 워크스페이스 헤더 점을 이름 바로 옆으로, 세션 헤더 요약에 점 + tap reconnect | `workspaces.js:2`, `session.js:5` | 하 |
| P3-8 | 크롬 팔레트 결정: Orca 모바일 값(#111 계열)을 유지할지 Krill Palette(#0a0a0a / #171717 / rgba 0.08)로 맞출지 메인테이너가 결정. 상태 색은 이미 Krill을 따름 | `app.css:3`, STYLEGUIDE §3.1 | 하(결정이 관건) |
| P3-9 | 빠른 프롬프트: 최근 보낸 프롬프트 3개 칩(메모리만, localStorage 금지 원칙 유지) | `session.js:12` | 하 |
| P3-10 | 터미널 내 A−/A+ 빠른 글자 크기(핀치를 모르는 사용자용) | `terminal.js:134` | 하 |

### 범위 밖 메모(속도 워커 담당, UX 관점만)

- 스냅샷 RPC가 세션마다 `lines` 캡처와 git 조회를 한다(`pwa_rpc.rs:355-370`). UX로 쓰지 않는 필드라면 P1-7에서 쓸지 뺄지 함께 정한다.
- 첫 진입에 스냅샷이 오기 전 화면은 "Krill" 헤더와 문장 한 줄뿐이다(`app.js:30`). 스켈레톤 행을 두면 같은 대기 시간에도 체감이 낫다.
- Orca에 있는 haptic은 iOS Safari에 `navigator.vibrate`가 없어 PWA에서 재현할 수 없다(N/A).

## 4. 남은 확인 사항

- 실기 iPhone Safari: 키보드 inset(`terminal.js:130`), 한글 IME commit 순서, safe-area, 홈 화면 PWA 모드.
- Claude Code, Codex 각 권한 프롬프트에 대한 `Approve (Yes)` = `y⏎`의 실제 동작(P1-5).
- P3-8 팔레트 방향은 메인테이너 결정 사항이다.
