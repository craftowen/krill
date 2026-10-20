# Orca 모바일 UI/UX 명세와 Krill PWA 재현 설계

출처: Orca MIT 소스, Copyright (c) 2026 Lovecast Inc. 원본 `LICENSE`의 MIT 고지를 유지한다. 조사 대상은 `/tmp/orca-ref/orca`, 커밋 `2d0fa9368f6b13c593a3fd16f96b6282bab8619c`이다. Krill 근거는 조사 시작 커밋 `3694c21516cbe5a4ee9cee1db1ab64ca1e65c07a` 기준이며 병렬 워커가 모바일 구현을 바꾸면 아래 줄 번호는 해당 커밋에서 확인한다. 작성일: 2026-10-18.

이 문서는 구현 코드 변경이 없는 조사·설계 기록이다. `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/CONVENTIONS.md`, `docs/STYLEGUIDE.md`가 우선한다. **Orca의 원본 명세와 Krill의 구현 제안을 구분한다.** 원본의 색·치수는 복원 근거이지 Krill 디자인 시스템을 덮어쓰는 승인이 아니다. Krill 구현 때 `krill-app/src/ui/theme.rs` 의미 토큰과 STYLEGUIDE를 CSS 변수로 대응시키고, 불일치 시 Krill 규범을 따른다. 미구현 버튼·메뉴·통계·탭은 비활성 플레이스홀더로도 노출하지 않는다.

표기: `M/` = `/tmp/orca-ref/orca/mobile/`, `K/` = 이 저장소 루트. 수치는 소스의 RN 논리 단위이며 웹 재현 시 CSS px 대응을 제안한다. RN `StyleSheet.hairlineWidth`는 장치 의존이므로 고정 1px로 단정하지 않는다. 명시된 것 외 기본 글꼴은 RN 플랫폼 기본값이며 웹의 `-apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif` 대응은 **설계 제안**이다. RN 화면을 실행·촬영한 것은 아니며 소스에서 읽었다.

## 1. Orca 모바일 정보 구조와 화면 명세

### 1.1 공통 토큰: 원문 전체

근거: `M/src/theme/mobile-theme.ts`. 아래 블록은 주석까지 원문 그대로이며 화면별 예외 값은 후속 표에 따로 적는다. 특히 `statusGreen`과 에이전트 완료 점의 `#10b981`, `statusAmber`와 작업 스피너의 `#eab308`은 서로 다르다.

```ts
// Orca mobile design tokens — matches desktop graphite/dark palette.
// All screen files should import from here instead of using inline hex values.

export const colors = {
  bgBase: '#111111',
  bgPanel: '#1a1a1a',
  bgRaised: '#242424',
  borderSubtle: '#2a2a2a',
  editorSurface: '#1e1e1e',

  textPrimary: '#e0e0e0',
  textSecondary: '#a1a1a1',
  textMuted: '#8c8c8c',

  // Crisp near-white surface for the single primary action on a screen (the
  // worktree FAB). Brighter than textPrimary so it reads as a solid button, not
  // disabled chrome, while staying monochrome (STYLEGUIDE: color is for state).
  surfaceBright: '#f5f5f5',

  accentBlue: '#3b82f6',
  // Text/icon color on a filled accent (accentBlue) button, where the muted
  // textPrimary would lack contrast against the saturated fill.
  onAccent: '#ffffff',

  statusGreen: '#22c55e',
  statusAmber: '#f59e0b',
  statusRed: '#ef4444',
  // Merge CTA fill + its on-fill text, mirroring the desktop ChecksPanel's
  // bg-green-600 "Squash and merge" button (green-600 / white).
  mergeGreen: '#16a34a',
  onMergeGreen: '#ffffff',
  // Merged-PR purple, mirroring the desktop ReviewIcon's purple-400/70 tone.
  statusPurple: '#a78bfa',
  gitDecorationAdded: '#81b88b',
  gitDecorationDeleted: '#c74e39',
  diffAddedBg: 'rgba(129, 184, 139, 0.1)',
  diffDeletedBg: 'rgba(199, 78, 57, 0.11)',
  // Desktop's `--diff-*-gutter`: a tinted row's line-number column.
  diffAddedGutter: 'rgba(129, 184, 139, 0.3)',
  diffDeletedGutter: 'rgba(199, 78, 57, 0.32)',
  // Desktop's `--chat-diff-*`: diff text in the chat, mixed toward the foreground so it reads on a
  // tinted row where the git decoration colours would not.
  chatDiffAdded: '#a5ccac',
  chatDiffRemoved: '#d68273',

  syntaxComment: '#6a9955',
  syntaxKeyword: '#569cd6',
  syntaxString: '#ce9178',
  syntaxNumber: '#b5cea8',
  syntaxType: '#4ec9b0',
  syntaxFunction: '#dcdcaa',
  syntaxVariable: '#9cdcfe',
  syntaxMeta: '#c586c0',

  // Terminal WebView background (Tokyonight) — separate from app chrome
  terminalBg: '#1a1b26'
} as const

/** On your own message's fill (`colors.textPrimary`), where the dark palette's light text would
 *  vanish; its body text is `colors.bgBase`. */
export const onBrightColors = {
  secondary: '#4a4a4a',
  link: '#1e40af',
  codeBackground: '#c8c8c8'
} as const

export const spacing = {
  xs: 4,
  sm: 8,
  md: 12,
  lg: 16,
  xl: 24
} as const

export const radii = {
  row: 6,
  card: 14,
  button: 6,
  input: 6,
  camera: 8
} as const

export const typography = {
  titleSize: 18,
  bodySize: 14,
  metaSize: 12,
  monoFamily: 'monospace' as const
} as const
```

### 1.2 화면과 전환

`app/index.tsx`의 홈에서 호스트를 선택하면 `/h/[hostId]` (`src/host-screen/HostScreen.tsx`) → 워크스페이스 선택 → `/h/[hostId]/session/[worktreeId]` (`src/session/MobileSessionRouteScreen.tsx`)로 간다. **워크스페이스와 터미널 탭은 다른 계층**이다. 세션 화면의 뒤로 가기는 워크스페이스 목록, 목록의 뒤로 가기는 홈이다. 탭 선택은 같은 워크스페이스 안에서 활성 탭만 바꾼다.

```text
/ 홈·호스트 목록
 ├─ 설정 → /settings
 │   ├─ /terminal-settings, /native-chat-settings, /browser-settings, /voice-settings
 │   ├─ /notifications, /troubleshoot → /connection-log?hostId=…
 │   └─ /about (+ 외부 Privacy Policy / Support)
 ├─ Pair Desktop → /pair-scan (카메라 또는 코드 붙여넣기)
 ├─ /pair?code=… → /pair-confirm → 페어링 → 온보딩 목적지 → 홈/호스트
 └─ /h/[hostId] 워크스페이스 목록
     ├─ /accounts, /tasks, /edit (같은 호스트 경로 아래)
     └─ /session/[worktreeId] → 터미널/agent-session/브라우저/markdown/file 탭
         ├─ 파일 패널, sourceControl 패널, PR 패널
         ├─ /source-control/[worktreeId], /review/[worktreeId], /pr/[worktreeId]
         └─ /history/[worktreeId], /agent-history/[worktreeId]
```

경로 근거: `M/app/**/*.tsx`, 설정 항목 `src/settings/mobile-settings-menu-items.ts`, 세션 헤더 `src/session/MobileSessionHeader.tsx`. `.web.tsx` 라우트에는 웹 셸 래퍼도 있으므로 RN 화면을 PWA 라우트에 그대로 이식하는 것으로 보지 않는다. 넓은 화면의 host sidebar/master-detail과 `MobileSessionContentRow`의 dock column은 별도 레이아웃이며 이 명세의 기준은 390×844 세로 폰이다. 모든 화면은 safe area를 확보한다. 홈·목록에 공통 하단 탭 바는 없다. 터미널에서만 하단 command dock이 고정된다.

### 1.3 홈 / 호스트 목록

근거: `src/home/MobileHomeTopBar.tsx`, `MobileHomeListHeader.tsx`, `MobileHomeHostList.tsx`, `MobileHomeListFooter.tsx`, `MobileHomeEmptyState.tsx`, `src/components/MobileHostCard.tsx`, `MobileHomeQuickActions.tsx`.

| 영역 | RN 원본 레이아웃·표현 |
|---|---|
| 상단 | 가로 padding 16, 위 8, 아래 12. 왼쪽 로고 18 + 오른쪽 여백 8 + `Orca` 17/700. 우측 Settings 아이콘 18, 터치 영역 36×36/r18, 눌림 bgRaised |
| 본문 | FlatList 좌우 16, 카드 간격 8, 하단 24 + bottomInset. `Welcome back` 24/800, 자간 −0.3, 위 4/아래 12 |
| 통계 | 데이터가 있을 때만 Agents spawned / Agent time / PRs created, 3등분, gap10, 아래16. 카드 rgba(26,26,26,0.6), borderSubtle 1, r10, 세로8/가로12. 값18/700/자간−0.3, 설명11/500/textMuted/위2 |
| 섹션 | `Desktops` 11/600/textMuted, uppercase, 자간0.6, 아래8, 좌우4 |
| 호스트 카드 | bgPanel/borderSubtle 1/r14/overflow hidden, 가로 행. 주 터치 영역 왼쪽12·세로12, 아이콘 박스46×46/r13/bgRaised/우측14, Monitor20. 이름15/600/행20, descriptor12/행16. 메타 위3/gap6, 점8 + 상태12/textSecondary. 연결될 때만 워크스페이스 요약12/textMuted/위2. 추가 안내11/행15/위4. 이름·descriptor·메타는 한 줄 말줄임 |
| 카드 우측 | MoreVertical18, 영역40×40/r6/좌우4. 주 영역 long press 400ms와 같은 액션 시트로 진입 |
| 하단 콘텐츠 | 조건부 Resume 카드, Tasks 카드(지원 provider 칩), Quick Actions의 Pair Desktop / New Workspace, 연결된 호스트의 계정 사용량. 이것은 **현재 RN 홈**이며 mock의 바닥 Settings와 다르다 |

연결 예외 문구: `Pairing invalid`, `Pairing temporarily unavailable`, `Tap to re-pair with your desktop`, `Unlock your phone, then tap to retry`. 연락 불가·릴레이 미설정이면 `Update desktop Orca and sign in to connect from anywhere`. 실제 상태 라벨은 `transport/connection-health.ts`의 `verdictDisplayLabel`이 결정한다. error verdict는 statusRed, 임시 자격 증명 불가는 statusAmber, 연결되지 않으면 이름도 textSecondary다.

호스트가 없으면 `Connect your desktop`(22/700/아래10), 설명 `Pair with Orca on your computer to check on your agents, jump into any terminal, and drive work from your phone.`(15/행22/아래32). 중앙 hero 좌우32·아래40, `Pair Desktop` 버튼 textPrimary 배경/bgBase 텍스트/15/700/r14/가로28·세로14/gap10. 아래 `How it works`와 3단계: `Open Orca desktop`, `Scan the code`, `You're connected`. 단계 영역 좌우24, 행 세로16/gap14, 숫자 박스28×28/r8/border1/rgba(255,255,255,0.04), 제목14/600·아래3, 설명12/행17, 행 사이 borderSubtle 1. 가로 화면에서도 전체 스크롤 가능하다. Krill에는 원문의 E2EE 보장 문구를 그대로 쓰기 전에 실제 인증 완료 상태를 연결한다.

### 1.4 호스트 → 워크스페이스/프로젝트 목록

근거: `src/host-screen/host-screen-header.tsx`, `host-screen-primary-styles.ts`, `host-screen-secondary-styles.ts`, `host-workspace-list.tsx`, `src/components/WorktreeListRow.tsx`, `NewWorkspaceFab.tsx`.

| 영역 | RN 원본 |
|---|---|
| 헤더 chrome | bgPanel, 하단 borderSubtle 1. statusBar 최소34/위4/좌우16, 뒤로 영역32×32/우측4(chevron22). 호스트 이름15/600 + 점8/gap8. descriptor12/행16/왼쪽16. 조건부 Reconnect: border1/r6/세로4·가로8, 글자12/600 |
| 툴바 | 좌우12·세로6/gap8, 하단 border1. Workspace Options 칩(표시는 `workspace-options-copy.ts`에서 공급): r12/border1/가로10·세로4/gap4/12 textSecondary. 활성 bgRaised/border textSecondary/textPrimary + 필터 수 배지. 우측 Accounts / Tasks / Search 아이콘16, 패딩4. 검색은 별도 줄 좌우12·세로8/bgPanel/hairline |
| 프로젝트 그룹 | sectionHeader 좌우16/위12·아래4. 이름11/600/uppercase/textMuted/자간0.5, 수11/왼쪽4. separator 1px, 왼쪽40/오른쪽16 |
| 워크스페이스 행 | 세로10·좌우16, 왼쪽 투명 border2를 미리 확보. desktop 활성 행 bgPanel + 왼쪽 textSecondary, 눌림 bgRaised. 상태 열 폭20/위6/우측8/gap4, unread Bell10 amber/위2 |
| 행 본문 | 제목14/600(미읽음700)/textPrimary, 우측여백8, 아이콘·제목 gap8. 메타 위2/gap4, repo11/textSecondary/최대100, branch 또는 folder path11/monospace/textMuted/flex1. `refs/heads/` 제거. 우측 liveTerminalCount12/textMuted/최소16/위3/우측 정렬 |
| 배지 | repo icon chip16×16/r4/hairline/bgRaised. PR 아이콘10 + 번호10/gap3, Folder·Child 배지도 가로5·세로1/r4/bgRaised. Child 깊이별 왼쪽18 추가. lineage 접기 버튼 gap4/위4/가로8·세로4/r6/11/600 |
| FAB | 폰 전용 New workspace: 48×48/r24, 오른쪽16/아래24+bottomInset, surfaceBright, Plus24/bgBase/stroke2.75. 눌림 textPrimary, disabled opacity0.5. 목록은 FAB 아래 마지막 행이 가리지 않게 padding 예약 |

카드의 repo/PR/meta/inline agents는 `cardDetails` 설정에 따라 조건부다. 프로젝트로 묶을 때 중복 repo 이름을 생략한다. 일반 shell 출력 tail은 보조 에이전트 활동 줄에 표시하지 않는다. 워크스페이스 tap은 세션, agent 행 tap은 해당 pane, long press 400ms는 컨텍스트 액션(중간 세기 haptic)이다. 소스에서 이 목록의 행 스와이프 삭제는 확인되지 않았다. 따라서 이를 추가 재현 대상으로 삼지 않는다.

검색 필드(`components/MobileSearchField.tsx`)는 최소높이42/bgRaised/border1/r6/gap8/왼쪽12·오른쪽4, iOS 세로8·그 외6, focus border textMuted, disabled opacity0.55다. clear 터치 영역 최소36×36 안에24×24/r12/textMuted 칩과 X12/surfaceBright를 둔다. 입력은 native14/web16, placeholder textSecondary, selection accentBlue다.

목록 상태 근거 `src/worktree/host-workspace-list-states.tsx`: 로딩은 중앙 small spinner/textSecondary, 빈 상태14/textSecondary에 `No workspaces`, `No matching workspaces`, `No workspaces match filters`. 실패는 `Could not load workspaces from this host`와 `worktree.ps failed (${catalogError}) — retrying automatically`(12/textMuted/위4/좌우16/중앙). 화면 controller 자체 error는 statusRed/14 중앙 표시다.

### 1.5 워크스페이스 → 세션 탭과 터미널 chrome

근거: `src/session/MobileSessionHeader.tsx`, `mobile-session-frame-styles.ts`, `MobileSessionActiveContent.tsx`, `MobileSessionHeaderMoreActionsSheet.tsx`.

헤더는 bgPanel/하단border1, safe top 다음 topBar 최소44·좌우8·세로4. 뒤로 영역36×36/r18/우측4, chevron22/textSecondary/stroke2.2. 제목14/600 한 줄, 연결 요약12/textSecondary/위2, 점과 간격8. 재연결이 필요한 경우 요약 줄을 눌러 reconnect한다. 우측 파일·Git·더보기 영역36×36/r6/왼쪽4, 활성·눌림 bgRaised. Folder/floating workspace에서는 실제 지원 여부에 따라 일부 아이콘을 숨긴다. 더보기에는 조건부 `Agent History`(힌트 `Browse and resume agent sessions`), `Checks`(힌트 `Open pull request checks`)가 있다.

탭은 상단border1, 가로 스크롤, 내부 좌우8, 각 **폭128/최대128/최소높이36/좌우8·세로8**, 하단border2 투명; 활성은 textSecondary 밑줄·textPrimary 글자, 비활성 textSecondary. 글자13, 아이콘13/gap4, 한 줄 말줄임. terminal에는 감지된 agent 아이콘, agent-session에는 agent 아이콘, browser/markdown/file에는 각각 Globe/FileText/File. 활성 탭을 가시 범위로 스크롤한다. 우측 고정 `+` 영역40×36/Plus16와 Quick Commands를 스크롤 밖에 둔다. 생성 중·연결 해제 시 disabled opacity0.45. 탭 long press 400ms는 키보드 dismiss 후 탭 액션 시트다. 탭 바 `keyboardShouldPersistTaps="handled"`로 키보드가 떠 있을 때 첫 탭을 삼키지 않는다.

본문 `flex:1; minHeight:0`, terminalFrame은 relative/overflow hidden, 터미널 pane은 absolute fill, 비활성은 opacity0으로 보존한다. 로딩 중앙 small spinner, 빈 상태 `No tabs in this session`(14/textSecondary/아래16) + `Create Tab`/`Creating...`(버튼 bgRaised/border1/r6/가로24·세로10). 지연 터미널은 `Loading terminal` 또는 제목; 복구 대기 길어지면 `Terminal is taking longer than expected` + `Retry`. 오류는 13/statusRed/아래8. Krill에서 생성 RPC 없이 Create Tab을 복사하면 안 된다.

### 1.6 상태 점의 정확한 의미

근거: `src/components/StatusDot.tsx`, `AgentStateDot.tsx`, `AgentSpinner.tsx`. 에이전트 상세 상태와 워크스페이스 요약 상태, 네트워크 상태는 서로 다른 컴포넌트다.

| 컴포넌트 | 상태 → 원본 색·형태 |
|---|---|
| StatusDot | 8×8/r4. connected→statusGreen, connecting/handshaking/reconnecting→statusAmber, disconnected→textMuted, auth-failed→statusRed. verdict unreachable/auth-failed은 빨강, warning 또는 `…`로 끝나는 normal 라벨은 amber가 우선 |
| AgentStateDot | wrapper10×10, dot6×6/r3. working→`#eab308` 6×6/r3/border1.5/윗변투명 회전 링. done→`#10b981`, blocked/waiting/failed→`#ef4444`, interrupted→textMuted, unconfirmed→statusAmber, idle→`rgba(115,115,115,0.4)` |
| 상세 예외 | monitoring→Activity10/`#eab308`, unverifiable→8×8/r4/border1.5 dashed/statusAmber, 접근성 `No recent update` |
| AgentSpinner | wrapper12×12, dot8×8/r4. working→`#eab308` border2 회전 링, active/done→`#10b981`, permission→`#ef4444`, inactive→`rgba(115,115,115,0.4)`. monitoring→Activity12 |

작업 링 회전은 `components/use-working-ring-rotation.ts`의1000ms/linear/무한 반복이며 비활성화 시0도로 돌아간다.

`needs-input`라는 한 개의 점 상태를 원본 enum으로 발명하지 않는다. waiting/blocked/permission 계열이 주의를 나타낸다. Krill은 §3 상태 매핑을 거쳐 자체 `ui::status`와 같은 의미로 표시한다.

### 1.7 액션 시트 / 설정 / 페어링 / 알림 / 연결 로그

| 화면 | 구조·정확한 표현·동작 및 근거 |
|---|---|
| 액션 시트 | `components/ActionSheetModal.tsx`, `bottom-drawer-styles.ts`: 하단 정렬, 검정 rgba(0,0,0,0.5) scrim; drawer bgBase/상단모서리16/좌우12. 핸들36×4/r2/textMuted/opacity0.4, hit area 위8·아래12. actionGroup bgPanel/r12, 행 세로12·가로14/gap10, icon16/textSecondary, 텍스트14/500, destructive만 statusRed. 행 사이 hairline/좌우12. 제목13/500/textMuted, 설명12/위2, 힌트12/위2. disabled opacity0.58. 바깥 탭·아래로 drag dismiss, 닫힘150ms. 다음 모달은 `onAfterClose` 뒤 연다 |
| 호스트 시트 | `host-list-action-sheet-actions.ts`: Connect 또는 Reconnect → 조건부 Disconnect → Network diagnostics → Edit host → Remove 순서. Remove는 destructive. 실제 연결 이력·상태에 따라 Connect/Reconnect 선택 |
| 설정 | `settings/mobile-settings-menu.tsx`, `mobile-settings-menu-items.ts`: 좌우16/위safeTop+8, 헤더 뒤로36/r18/우측8 + Settings20/700/아래24. ScrollView 아래safeBottom+16. section bgPanel/r12, 행 세로12·가로14/gap10/14·500, icon16, ChevronRight16. 행 사이 hairline/좌우12. 메뉴 순서 Terminal / Chat UI / Browser / Voice / Notifications / Troubleshooting / About. Privacy Policy / Support는 12 간격 별도 그룹(외부 링크). 모두 앱 내부 라우팅 |
| 터미널 설정 | `app/terminal-settings.tsx`: WHEN YOU LEAVE THE APP, TEXT SIZE, KEYBOARD INPUT 섹션, Text size 선택, Autocomplete & autocorrect, 기본 액세서리 키의 표시·정렬과 custom shortcut 관리(`components/TerminalShortcutSettings.tsx`). 호스트의 자동 desktop 복귀 설정은 Krill의 현재 기능으로 간주하지 않는다 |
| 페어링 링크 | `app/pair.tsx`: code가 있으면 `/pair-confirm`으로 replace. 없으면 중앙 spinner, 최종 `Missing pairing code`(14/statusRed/행20/아래24) + `Back to home` |
| 스캔 | `app/pair-scan.tsx`, `src/pair-scan-styles.ts`: 전체padding16/위safeTop+8/아래safeBottom+8, 뒤로36/r18/아래8. 세 단계 `Open Orca on your computer` → `Go to Settings → Mobile` → `Scan the QR code`, gap8/아래16/왼쪽7, 번호22/r11/bgRaised/12·700. 카메라 flex1/r8/overflow hidden, 가운데 정사각 reticle=min(round(min(width,height)×0.62),360). 모서리28×28/두께2.5/r6/rgba(255,255,255,0.7). 하단 `Or paste pairing code`(14/500/위12·세로8) |
| 스캔 상태 | 권한 미확인 spinner; 미허용 `Pair with desktop` + Continue, 재요청 불가 `Camera Access Disabled` + Open Settings; 모두 Paste code instead 제공. 붙여넣기 시 카메라 unmount, 같은 flex1 placeholder bgPanel/r8로 위치 유지. 잘못된 QR: `Not a valid Orca QR code`; 잘못된 코드: `Not a valid pairing code — copy it from your computer and paste again`. 연결 중 `Connecting…` + Pairing log. 25초 시한 뒤 `Couldn't connect within 25s — see log below for where it stalled`; 기타 `Pairing failed: …`; Try Again/Paste code instead |
| 확인 | `app/pair-confirm.tsx`: 제목 `Pair with this desktop?`18/600/중앙/아래8, 설명14/행20/아래24/최대520, 액션 열 최대360. Pair(textPrimary 배경/bgBase/14·600/세로10·가로24/r6), Cancel(textSecondary/14·500). 내용 중앙/가로8/아래48. 연결 중 spinner + Connecting…/위16 + log. 취소는 홈 replace; route detach는 진행 중 시도 dispose |
| 알림 | `settings/notification-settings-screen.tsx`: 설정과 같은20/700 헤더, 전체padding16/위safeTop+8/아래safeBottom+24. bgPanel/r12 카드 안 `Enable notifications`14/500, Switch false bgRaised / true textSecondary / thumb textPrimary. 설명12/textMuted/행18/가로14·아래12. denied면 `Notifications are disabled in system settings.` + Open Settings. saving 또는 denied일 때 switch disable. 오류 `Could not load notification settings. Try again.`, `Could not save notification settings. Try again.`, `Could not open system settings. Try again.` |
| 연결 로그 | `app/connection-log.tsx`, `diagnostics/connection-diagnostics-view.tsx`, `components/ConnectionLog.tsx`: 여러 호스트일 때 host chips, 연결 상태·재시도·경로·진단 동작 및 로그. 로그 카드 bgPanel/r14/hairline/세로8·가로12, bounded 높이≤240/scroll≤200 또는 fillAvailableHeight. title12/mono/uppercase/자간1/아래4. 행 gap8, 시간 폭52/12 mono/textMuted/위1, glyph 폭12. 본문12 mono/행16, detail11/행14/textMuted/위1/최대2줄. 행 간6. info `•` textSecondary, success `✓` green, warn `!` amber, error `✕` red. 첫 이벤트 기준 +초(10 미만 소수2/100 미만 소수1/이상 반올림). 새 항목시 scrollToEnd. entries=0이면 로그 컴포넌트 자체를 숨김 |

연결 진단 페이지의 추가 치수(`diagnostics/connection-diagnostics-screen-styles.ts`): 전체padding16/위safeTop+8, topRow 아래16, 뒤로36/r18/우측8, 제목 `Network diagnostics`20/700. host chips는 wrap/gap8/아래12, 세로6·가로12/r16/bgRaised, 활성 bgPanel/border1, 글자12/최대160/활성600. 상태줄12/textSecondary/아래8, Copy report 버튼 가로12·세로6/r8/gap6/12·600. 진단 카드r10/hairline/padding12/아래12, 제목12·600/아래4, 본문12/행18, 다음 단계 textSecondary/위4. 실제 빈 화면 문구는 `No connection events yet. Events appear as the app dials this host.` 또는 `No paired hosts.`(12/textMuted/행18)다. Copy 성공은 `Copied`, relay에만 Send diagnostics to Orca / Sending… / Diagnostics sent / Retry sending을 제공한다. **Krill에서는 외부 Orca 진단 전송 버튼을 제외**한다.

페어링 설명은 Orca 프로토콜 전용이다. Krill에서는 `orca://pair`를 해석하지 않고 기존 Krill URL(ticket/query + secret/fragment)을 그대로 사용한다. 알림 설정 화면은 알림 받은 목록이 아니다. Orca의 작업 완료/입력 대기 알림과 시스템 권한 흐름을 Krill의 웹훅 딥링크와 혼동하지 않는다.

## 2. 터미널 화면 상세

### 2.1 렌더링·스크롤·폰트

Orca native는 `src/terminal/TerminalWebView.tsx`의 WebView 안에 xterm 문서를 만들고, web은 `TerminalWebView.web.tsx`에서 같은 문서 요소를 scoped DOM으로 사용한다. 실제 출력 엔진은 `document/terminal-init.ts`: 처음80×24, cols/rows 초기화 및 snapshot/후속 바이트 write, xterm + WebGL addon(복구 경로 있음) + Unicode11 addon. `scrollback:5000`, `convertEol:false`, 글자 weight300/bold500, cursor blink false/style bar/inactive block. theme은 host 테마가 덮을 수 있으며 기본 Tokyonight는 아래와 같다(`terminal-webview-html/theme.ts`). 이것은 앱 chrome의 bgBase와 다르다.

| 항목 | 기본값 |
|---|---|
| 배경/전경/커서 | `#1a1b26` / `#c0caf5` / `#c0caf5` (cursorAccent 배경과 같음) |
| 선택 배경/전경 | `#33467c` / `#c0caf5` |
| ANSI 0–7 | `#15161e`, `#f7768e`, `#9ece6a`, `#e0af68`, `#7aa2f7`, `#bb9af7`, `#7dcfff`, `#a9b1d6` |
| ANSI 8–15 | `#414868`, `#f7768e`, `#9ece6a`, `#e0af68`, `#7aa2f7`, `#bb9af7`, `#7dcfff`, `#c0caf5` |

폰트 근거 `document/text-scaling.ts`: 기본13px, 최소6px, `max(6,round(13×scale))`; presets `[0.5,0.75,1,1.25,1.5,2]`(`terminal-text-scales.ts`). iOS는 `ui-monospace`, 그 외 `"SF Mono"` 선두, 이후 `"Menlo", "Monaco", "Cascadia Mono", "Consolas", "DejaVu Sans Mono", "Liberation Mono", "Symbols Nerd Font Mono", monospace`. 핀치 중 CSS scale/pan으로 따라가고 끝날 때 가장 가까운 preset으로 snap → 실제 fontSize 변경·그리드 refit → 호스트 viewport 갱신, 선택 haptic, preference 저장(`MobileSessionActiveContent.tsx`). 텍스트 크기 변경을 단순 브라우저 페이지 확대와 혼동하지 않는다. 표시 모드 전환은 desktop/phone 아이콘이며 실제 PTY viewport 연동이 포함된다.

스크롤 근거 `document/surface-touch-gestures.ts`, `mouse-input-encoding.ts`: 세로 한 손가락 이동은 normal buffer 스크롤(관성 포함), 마우스를 받는 TUI 또는 alternate screen에서는 적절한 wheel/키 입력으로 라우팅한다. 가로는 출력이 viewport 폭보다 클 때 pan. 핀치는 두 손가락 중점 기준. long press는 선택 모드 → 단어 선택·핸들·Copy(`selection-state-and-eviction.ts`, `selection-range.ts`, `selection-menu-buttons.ts`). 키보드를 여는 tap과 텍스트 선택 제스처를 분리한다. URL/file tap도 전용 처리 경로가 있다.

`terminal-webview-html/document-style.ts`: native 문서 전체 overflow hidden, xterm 자체 scrollbar 숨김. 별도 scroll-indicator 오른쪽3/위아래4/폭3, visible opacity0.72/120ms, thumb 최소24/r999/textSecondary. 선택 overlay는 viewport 좌표에 고정(z10), 핸들 터치44×44, 원14×14/테두리2, Copy 메뉴 r8/13·600/버튼가로16·세로10. xterm 출력 자체의 CSS를 PWA 전역 스타일로 주입하지 않는다.

치명적 로드 실패 overlay(`terminal-webview-engine-error-state.tsx`): terminalBg 배경, 중앙/gap12/padding24, `Terminal failed to load`16/700, 상세13/행18/최대4줄, `Reload`(최소높이36/가로14/r6/surfaceBright, 글자14/700). RN renderer crash 또는 메시지 오류를 묵살하지 않는다.

**Krill 렌더링 설계:** xterm을 도입하지 않는다. `K/crates/krilld/src/mobile/assets/terminal.js:18`의 화면 delta 모델과 `:73`의 canvas를 유지하고 chrome만 바꾼다. 질의 응답은 계속 krilld 단독 소유다. Orca의 5,000행 client scrollback·query reply bridge를 복제하면 Krill 규범과 어긋난다. Krill은 현재 hot/cold history RPC를 100행 단위로 가져오고 client에 최대1,000행을 보존한다(`terminal.js:67`). font preset·선택·일반 스크롤의 로컬 UI는 추가 구현 가능하지만 TUI mouse gesture는 negotiated term modes와 인코더가 필요하다. 현재 canvas가 TUI mouse를 지원한다고 표시하지 않는다.

### 2.2 direct input과 buffered input

근거: `M/mobile-terminal-direct-input-default.md`, `src/session/MobileSessionCommandDock.tsx`, `src/terminal/use-buffered-terminal-drafts.ts`, live-input 관련 hook들.

- 처음 발견한 terminal handle마다 direct input 기본값을 딱 한 번 적용한다. `liveInputTerminalHandles`와 default 적용 완료 set을 분리한다. 사용자가 buffered로 바꾼 뒤 snapshot이 새로 와도 되돌리지 않는다. `terminal.list`만 수명 정리 근거로 삼고, 지연될 수 있는 session tab snapshot으로 set을 제거하지 않는다. 워크스페이스 route reset은 default tracking을 초기화한다. 앱 재시작 간 mode 영속화는 원본 요구사항이 아니다.
- direct mode는 숨겨진 TextInput(absolute/opacity0/1×1)을 통해 키 입력을 PTY로 전달한다. 화면 진입만으로 자동 focus하지 않으며, terminal 또는 `Show keyboard for live terminal input` 영역 tap으로 focus한다. autocapitalize none/autocorrect false/spellcheck false/smartInsertDelete false/autocomplete off/blurOnSubmit false. IME 조합 중 문자열을 매 이벤트 전송하지 말고 commit된 텍스트와 제어키를 순서대로 보낸다.
- live bar: 아이콘 Keyboard16 + 제목 `Live input`(12/600/textPrimary), 보조 `Tap to show keyboard` 또는 capture text(12/mono/textSecondary, 앞부분 말줄임). dictation 중에는 Listening / Processing / Starting mic 및 Tap mic to stop / Transcribing on desktop / Preparing microphone. 이미지 전송 중 Uploading image to host. Krill에서는 음성 처리 API가 없으므로 해당 상태·마이크 버튼을 숨긴다.
- buffered mode는 보이는 command field, placeholder `Type a command…`, Enter 또는 송신 버튼으로 보낸다. draft는 terminal별로 유지하고 offline에도 편집한다. 송신에 성공하기 전 내용을 지우지 않는다. 원본은 autocomplete preference를 반영하며 direct mode와 별개다. RN input font14, web sibling `platform/text-input-font-size.web.ts`는 Safari focus zoom을 막도록 **16 이상**이다. viewport의 사용자 확대 기능을 잠그지 않는다.

하단 dock의 정확한 치수(`mobile-session-command-input-styles.ts`): z20, bottom safe inset, `translateY:-keyboardLift`. 액세서리 줄 bgPanel/윗border1, 가로 scroll 내부 좌우8·세로4/gap4. 키 bgRaised/r6/최소폭36/가로10·세로4/12 monospace/textSecondary, pressed borderSubtle, mode 활성 textPrimary 배경/bgBase 아이콘, disabled opacity0.35. 입력 줄 bgPanel/윗border1/최소높이46/좌우12·세로6. buffered field 높이34/r6/좌우12/우측8/bgRaised. live focus target 최소높이34/border1/r6/가로10/gap8. 송신·사진·마이크34×34/r17, 사진·마이크 우측8. 사진 tap은 photo library, long press350ms는 파일 선택, 마이크는 hold 또는 toggle 설정이다.

### 2.3 액세서리 키의 정확한 목록과 순서

근거: `src/terminal/terminal-key-definitions.ts`, `terminal-accessory-layout.ts`, `terminal-accessory-repeat.ts`. **Ctrl 단독 latch는 기본 목록에 없다. Ctrl 조합별 버튼이다.** 기본값은 모든 builtin을 아래 순서대로 표시한다. 사용자 설정은 순서·가시성을 바꿀 수 있다.

| 순서 | 라벨 | 전송 bytes(escaped) | 누르고 반복 |
|---|---|---|---|
| 1 | Esc | `\x1b` | 아니오 |
| 2 | Tab | `\t` | 아니오 |
| 3 | Enter | `\r` | 아니오 |
| 4 | Shift+Tab | `\x1b[Z` | 아니오 |
| 5 | Space | ` ` | 아니오 |
| 6 | ⌫ | `\x7f` | 예 |
| 7 | Del | `\x1b[3~` | 예 |
| 8 | ↑ | `\x1b[A` | 예 |
| 9 | ↓ | `\x1b[B` | 예 |
| 10 | ← | `\x1b[D` | 예 |
| 11 | → | `\x1b[C` | 예 |
| 12 | Ctrl+C | `\x03` | 아니오 |
| 13 | Ctrl+D | `\x04` | 아니오 |
| 14 | Ctrl+L | `\x0c` | 아니오 |
| 15 | Ctrl+Z | `\x1a` | 아니오 |
| 16 | Ctrl+R | `\x12` | 아니오 |
| 17 | Ctrl+A | `\x01` | 아니오 |
| 18 | Ctrl+E | `\x05` | 아니오 |
| 19 | Ctrl+W | `\x17` | 아니오 |
| 20 | Ctrl+U | `\x15` | 아니오 |

행 전체 순서는 키보드가 열린 경우 scroll 밖에 고정된 Dismiss keyboard 영역(최소36×28/r6/왼쪽8·세로4) → scroll 안 display mode(Monitor/Smartphone14) → direct toggle(ChevronsRight14) → 조건부 Paste → 위 builtin 20개 → custom keys → Add custom shortcut(Plus14)다. custom key long press400ms는 삭제 타깃을 열며 border1을 추가한다. 반복 키는 press-in 즉시 송신, 400ms 후 반복 시작, 이후 성공한 송신마다45ms, release/타깃 변경 시 취소한다. builtin 뒤 Enter가 한 번 더 붙는 순서가 아니다. Insert/Home/End/PgUp/PgDn/F1–F12는 custom shortcut picker의 선택지이지 기본 액세서리 행이 아니다.

키보드 처리: accessory `keyboardShouldPersistTaps="always"`, repeatable key의 long press가 held press를 끊지 않게 처리. 키보드를 닫는 고정 버튼은 scroll·사용자 숨김 대상에서 제외한다. 커맨드 입력줄·터미널 영역·safe bottom을 keyboardLift 기준으로 함께 배치하고, tab 전환·sheet·route 이탈 시 늦게 끝난 IME commit이 이전 terminal에 전송되지 않도록 타깃 generation을 확인한다. Krill의 기존 `visualViewport` inset 계산(`terminal.js:39`)을 유일한 웹 키보드 기준으로 재사용하되 viewport scale≠1의 사용자 줌을 키보드로 오판하지 않게 검증한다.

## 3. Orca → Krill 기능 매핑과 노출 기준

### 3.1 개념과 현재 wire

Orca host ≈ **현재 브라우저 origin이 연결한 Krill 데몬/Mac 한 대**. Krill desktop의 SSH/iroh 호스트 기능이 있다고 해서 현재 PWA가 여러 Mac의 호스트 카탈로그를 제공하는 것은 아니다. 현재 PWA 인증 저장소는 한 origin의 `minikrill-mobile-v2` 한 개(`assets/client.js:12`), WS도 같은 origin `/ws`다(`:71`). 초기 화면은 한 데몬 카드로 표현할 수 있으나 hostname/OS는 실제 RPC가 없으므로 `location.host`를 식별자로 쓰고 macOS 버전 등을 추측해 표시하지 않는다. 다른 origin의 기기 비밀을 복사하는 host manager는 별도 기능이다.

Orca worktree/project ≈ Krill **repo/worktree 경로로 묶은 세션 그룹**, Orca terminal tab ≈ Krill **SessionId 하나**다. `SidebarData`는 `sessions[{session,cwd:Vec<u8>}]`, `repositories[{repo:Vec<u8>,worktrees:Vec<Vec<u8>>}]`를 이미 제공한다(`crates/krill-proto/src/control.rs:455`). desktop은 cwd가 속하는 worktree 중 가장 깊은 경로를 고른다(`crates/krill-app/src/sidebar/ipc.rs:619`). 별도로 상태/PR 그룹 키도 있다(`sidebar/options.rs:424`). 즉 단순 cwd basename이나 branch만으로 project/worktree ID를 만들면 같은 이름끼리 충돌한다.

현재 JSON `snapshot`은 List+States+Sidebar를 합치지만 `repositories`를 버리고 cwd 문자열만 가져온다(`pwa_rpc.rs:294`). 최대64세션, 각 카드 `{id,state,agent,cwd,lines,exited,branch,added,deleted}`, `lines`는 비어 있지 않은 마지막3줄/각500문자다(`:307`, `:325`, `:334`). 따라서 **현재 RPC만으로 가능한 기본안은 정확히 같은 cwd 문자열끼리 묶기**이며, 하위 디렉터리까지 같은 worktree로 묶는 desktop 동등안은 §3.3 확장 후다. cwd 빈 값은 `unknown:${sessionId}`로 각각 분리하고 전부 하나의 프로젝트라고 주장하지 않는다.

JSON RPC는 `MobileRequest::Workflow(String)`(wire kind0x45, enum index7) → `MobileReply::Workflow(String)`(0x65, index6) 안에 들어간다(`krill-proto/src/mobile.rs:20`, `:38`, `:66`; 클라이언트 `client.js:23`). JSON은 `{ok:true,data:…}` / `{ok:false,error:…}`(`pwa_rpc.rs:88`). 특수키의 raw input은 Action enum이 아니라 **별도 live WS의 binary Input** 경로다. `ws.rs`는 WS 프레이밍이며 터미널 UI state 머신이 아니다.

### 3.2 화면·요소별 매핑 표

판정의 “없음”은 **조사한 모바일 데이터/RPC 계약에 해당 기능이 없음**을 뜻한다. desktop IPC까지 없는지와 구별한다. “소폭 추가”는 설계 제안이며 구현 완료 판정이 아니다. 완성 전에는 UI를 숨긴다. 전체 경로는 `K/` 기준이다. 표의 축약 경로는 `pwa_rpc.rs` = `crates/krilld/src/mobile/pwa_rpc.rs`, `assets/…` = `crates/krilld/src/mobile/assets/…`, `control.rs` = `crates/krill-proto/src/control.rs`, `mobile/gateway.rs` = `crates/krilld/src/mobile/gateway.rs`, `registry.rs` = `crates/krilld/src/registry.rs`다. 모든 `파일:줄`은 위 조사 시작 커밋 기준이다.

| Orca 화면/요소 | Krill 기존 근거(파일:줄) | 분류와 재현 범위 |
|---|---|---|
| 홈 호스트 카드/연결 점 | `crates/krilld/src/mobile/assets/client.js:71`, `:73`, `:79`; `terminal.js:45` | **재현 대상**: 현재 origin 한 데몬·실제 WS 상태. 여러 데스크톱 카탈로그·자동 발견은 **제외**. 친숙한 이름·OS는 §3.3 metadata 후 |
| 호스트 연결/해제/재연결 시트 | `assets/terminal.js:44`, `:62`; `assets/client.js:73` | **재현 대상**: 클라이언트 socket close/reopen을 실제 구현한 항목만. 현재 RPC socket은 disconnect 뒤 자동 복구 없음. “자동 재연결”을 기존 기능으로 표시하지 않음 |
| 홈 전체 agents/time/PR 통계 | `pwa_rpc.rs:334`, `:336`에 해당 집계 없음 | **제외**: 세션 개수를 lifetime Agents spawned로 표시 금지 |
| repo/cwd 목록·세션 수·에이전트 상태 | `pwa_rpc.rs:302`, `:307`, `:334`; `krill-proto/src/control.rs:455` | **재현 대상**: 같은 cwd 그룹·실제 반환된 세션 수. **소폭 추가**: repositories와 worktree group 식별자 노출.64개 제한에서 전체 Mac 총계라고 표기 금지 |
| 워크스페이스 제목·설명·핀·미읽음·lineage·repo icon 설정·desktop 활성 강조 | `control.rs:463`에는 session/cwd만, `pwa_rpc.rs:334`에는 관련 필드 없음 | **제외**: Orca 고유 메타. 로컬 active route 강조만 재현 가능하며 desktop active라고 부르지 않음 |
| 상태 working/idle/needs input | `krill-proto/src/control.rs:386`, `:419`; `pwa_rpc.rs:308` | **재현 대상**: Working→작업 링, Waiting/Blocked→주의, Done→완료, Exited 또는 exited→종료, Unknown→알 수 없음. Orca idle/interrupted/monitoring을 없는 값으로 합성하지 않음. source/confidence는 §3.3 추가 시 추정 표시 가능 |
| 워크스페이스 세션 탭 | `pwa_rpc.rs:334`; `assets/terminal.js:44` | **재현 대상**: 그룹에 속한 session id별 terminal 탭, agent 라벨/브랜드. native agent chat session 탭은 **제외** |
| 새 terminal/agent 탭·닫기·이름 변경 | `krill-proto/src/control.rs:145` Create/Close는 desktop IPC에 있음, `pwa_rpc.rs:42` Action에는 없음 | **소폭 추가**: 알려진 cwd의 Login shell 생성·Close bridge는 가능. 자유 agent launch/이름 변경은 별도 계약 전 **제외**. 초기안에서 `+`·Close 숨김 |
| 출력·색·커서·화면 갱신 | `assets/terminal.js:18`, `:48`, `:52`, `:73` | **재현 대상**: 기존 screen snapshot/delta canvas. xterm/클라이언트 VT 해석으로 교체 금지 |
| hot/cold 스크롤백 | `assets/terminal.js:28`, `:67`, `:68` | **재현 대상**: top 도달/load history 및 bounded rows. 원본의5,000행·TUI mouse-wheel은 **제외** |
| direct input·특수키·Ctrl 조합 | `assets/terminal.js:63`, `:54`; `krill-proto/src/input.rs`의 Input/ACK | **재현 대상**: 이미 raw byte 입력 있음. 숨은 입력기·IME commit UI는 프런트 구현, backend 추가 아님. 원본20키로 확장 가능 |
| buffered command bar | `pwa_rpc.rs:44`, `:108`; `assets/client.js:58` | **재현 대상**: Prompt는 text 뒤 `\r` 추가. direct input은 Prompt로 보내면 안 됨 |
| 폰/desktop viewport | `assets/terminal.js:48`, `:64`, `:65`; `crates/krilld/src/registry.rs:460` | **재현 대상**: 기본 owns_size:false, 명시 Fit to Phone 후 Resize. 폰트 크기 변경만으로 desktop PTY 크기 소유권을 빼앗지 않음 |
| 텍스트 크기·키 순서·custom shortcuts·autocorrect 설정 | `assets/terminal.js:29`, `:73`은 현재 고정 값 | **재현 대상**: local UI로 새 구현할 수 있음. 기능을 구현한 설정만 노출. 원본 폰트 preset은 canvas cell metrics도 함께 바꿔야 함 |
| 승인/질문·Needs You | `pwa_rpc.rs:48`, `:115`, `:147`, `:153`, `:338` | **재현 대상**: Approve(value)는 PTY에 문자열+CR 보내는 기능, typed permission API라고 과장 금지. GateReply/QuestionReply는 store의 실제 id/options를 사용 |
| 계정과 사용량·계정 전환 | `pwa_rpc.rs:57`, `:135`, `:231`; `assets/client.js:52` | **재현 대상**: AccountsSnapshot의 실제 값. null usage를0%로 오인 표시하지 않음. Orca의 로그인/삭제/세밀 정책 설정은 해당 모바일 Action 없어 **제외** |
| Git branch / +− / 변경 파일 / diff | `pwa_rpc.rs:357`, `:379`, `:393`; `assets/client.js:53` | **재현 대상**: readonly branch/files/patch. staged·unstaged 분리 행은 현재 응답에 없으므로 **소폭 추가** 전 단일 목록 |
| Git commit/push/PR·Checks·merge·review comments | `pwa_rpc.rs:75` Git은 조회만; `krill-proto/src/control.rs:207` desktop GitEdit는 별도 | **제외**: GitEdit IPC가 있어도 현재 모바일 bridge는 없음. PR 연결/Checks는 이 JSON 계약에 없음 |
| Handoff 액션 | `pwa_rpc.rs:71`, `:162` | **재현 대상**: codex/claude만, agent 동작 결과 후 완료 표시. Orca 화면을 복사할 때 Krill 고유 시트 항목으로 배치 가능 |
| 사진·카메라 첨부 | `pwa_rpc.rs:52`, `:122`, `:414`; `assets/client.js:63` | **재현 대상**: JPEG/PNG/WebP 서버상한512KiB. 기존 browser는20MiB 입력 상한·최대변1600 축소·JPEG 압축. 일반 파일 첨부·음성 dictation은 **제외** |
| Tasks(provider 이슈 목록) | `pwa_rpc.rs:338` orchestration만 제공 | **제외**: GitHub/Linear/Jira Tasks와 Krill orchestration Task는 별개. 필요 시 Krill “오케스트레이션” 화면으로 실제 runs/tasks/gates/questions만 보여줌 |
| 파일 탐색·markdown 편집·browser screencast·native chat | `pwa_rpc.rs:42` 전체 Action 계약 | **제외**: 대응되는 모바일 파일/브라우저/chat RPC 없음. read-only Git diff로 파일 explorer를 대체했다고 표기 금지 |
| QR URL 접속·페어링 확인·코드 붙여넣기 | `assets/client.js:12`, `:75`, `:79`; `krill-proto/src/mobile.rs:21` | **재현 대상**: 기존 Krill handshake 및 인증 성공 후 저장. confirm UI는 side effect 시작 전에 끼울 수 있음. 앱 내 QR 카메라 스캐너는 **제외**(의존성 없는 QR decoder 없음); 휴대폰 기본 카메라로 QR URL 열기 안내 |
| 기기 목록·회수·연결 모드 | `krill-proto/src/mobile.rs:20`, `:52`, `:106`; `mobile/gateway.rs:150`, `:187`; `registry.rs:475` | **재현 대상**: raw MobileRequest가 이미 존재(Workflow Action과 별개). admin UI를 만들 때 binary encoder/권한 처리까지 구현한 항목만. 현재 Workflow-only wrapper로 이 요청을 보냈다고 가정 금지 |
| 연결 로그·간단 설정/About | `assets/client.js:70`, `:73`, `:75`; `terminal.js:60` | **재현 대상**: 클라이언트에서 관측한 connecting/authenticated/disconnected/error의 로컬 bounded log 및 로컬 터미널 설정. Orca relay trace·host test·remote runtime 진단은 **제외** |
| 알림 enable·푸시 등록·전달 테스트 | `crates/krilld/src/webhook.rs:57`; `mobile/gateway.rs:119` | **제외**: 기존은 daemon webhook과 `/?session=` 딥링크, APNs/Web Push preference API 아님. webhook 링크 진입은 **재현 대상**, Notifications switch를 동작 없이 복사 금지 |
| 앱 업데이트 카드·onboarding 음성/알림·외부 Orca 지원 링크 | `pwa_rpc.rs:42`에 대응 없음 | **제외**. Krill 제품 정보/지원 주소는 확인된 항목만 사용 |

### 3.3 소폭 backend 확장 계약 제안

아래는 **미구현 제안**이다. UI 재현의 필수 전제는 A(정확한 repo/worktree 그룹)이며, 나머지는 후속 선택이다. 기존 postcard DTO 필드·순서·opcode를 변경하지 않고 Workflow JSON 응답의 추가 optional 필드/Action으로 bridge하는 방향이다. 새 protocol bit가 필요하면 ARCHITECTURE 등록과 지휘자 할당을 먼저 따른다.

**A. snapshot 그룹·상태 근거 확장** (`pwa_rpc.rs` 소유 워커):

```json
{
  "sessions": [{
    "id": 42, "state": "Waiting", "agent": "codex", "cwd": "/repo/wt/sub",
    "lines": [], "exited": false, "branch": "feature", "added": 2, "deleted": 1,
    "repo_key": "<opaque>", "worktree_key": "<opaque>",
    "group_label": "wt", "group_path": "/repo/wt",
    "state_source": "Hook", "state_confidence": "High", "state_since_unix_ns": "0"
  }],
  "repositories": [{"key":"<opaque>","label":"repo","path":"/repo","worktree_keys":["<opaque>"]}],
  "truncated": false,
  "capabilities": {"snapshot_groups":true,"session_create":false,"session_close":false}
}
```

`repo_key/worktree_key`는 OS path bytes를 충돌 없이 식별하는 opaque 문자열(예: base64url, 비밀 아님), display path는 별도 lossy UTF-8이다. cwd의 worktree 소속은 desktop과 동일하게 path component 기준 최장 접두 경로로 산정한다(`sidebar/ipc.rs:619`); 문자열 `startsWith('/repo/a')`로 `/repo/abc`를 묶지 않는다. 알려진 repo 밖에서는 cwd bytes별 그룹, cwd 미상은 세션별 그룹. `repositories`는 기존 Sidebar 응답을 bridge하며 empty worktree도 사실에 따라 구분한다. `truncated`는64세션 제한 적용 전 길이로 계산한다. 상태 필드들은 기존 AgentStatus에서 가져오며 timestamp는 JS 정밀도 보존을 위해 문자열, unknown은 null/source·0 timestamp로 정의한다. 오래된 서버에서 필드가 없으면 exact cwd fallback·Unknown 근거 처리. capabilities가 없으면 현재 검증된 RPC 세트만 쓰고 확장 버튼을 숨긴다.

**B. 현재 데몬 메타**: `snapshot.host?:{display_name:string|null, platform:string|null}`; 실제 서버에서 읽은 이름/플랫폼만 반환, 없으면 origin 표시. Orca식 remote catalog는 추가하지 않는다. 진짜 daemon UUID가 없으면 임의의 안정적 UUID라고 주장하지 않는다.

**C. 알려진 그룹 안 terminal 생성·닫기**: 새 JSON 요청 `{"action":"create_terminal","cwd_session":42}` → source session의 cwd를 서버에서 조회 → `Request::Create`의 `Program::Login` → `{"session":43}`. 임의 프로그램 문자열·임의 파일 경로를 폰에서 받아 실행하는 계약은 여기 포함하지 않는다. `{"action":"close_terminal","session":43}` → `Request::Close` → 실제 성공 뒤 `{"closed":true}`. 권한·feature 협상·중복 요청·in-flight 타깃을 검증하고 종료의 영향은 확인 시트에서 설명한다. 요청만 보냈는데 탭을 지우지 않는다. 기존 관련 IPC 근거 `krill-proto/src/control.rs:118`, `:145`; 모바일 Action 추가 전까지 UI는 숨긴다. close는 세션의 실행을 끝내는 동작이며 단순 viewer 닫기와 다르다.

**D. Git 행 세분화(선택)**: `git` 응답에 `file_details?:[{path,status,staged:{added,deleted}|null,unstaged:{added,deleted}|null}]` 추가; `path` 인자는 기존 검증 유지. 현재 `files:string[]`는 보존한다. 값이 없으면 단일 변경 목록으로 표시하며 staged/unstaged 탭은 숨긴다. PR/Checks/commit RPC는 이 소폭 확장 범위가 아니다.

## 4. 의존성 없는 바닐라 JS PWA 구현 설계

이 절은 **후속 구현 제안**이다. 이 조사에서는 아래 파일을 수정하지 않는다. `crypto.js`, `client.js`, `terminal.js`가 분리되어 있어도 현재 HTTP 서버가 제공하는 것은 `app.js` 한 개라는 점을 먼저 해결해야 한다. 현재 `app.js`는 crypto/terminal/client 코드를 포함한 단일 산출물이고, `pwa.rs:4`의 ASSETS는 `/`, `/app.js`, `/app.css`, `/manifest.json`, `/icon.svg` 다섯 경로만 제공한다. 화면 모듈을 디스크에 쓰는 것만으로는 브라우저에 도달하지 않는다.

### 4.1 파일 경계와 로딩

제안 구조(모두 `crates/krilld/src/mobile/` 아래):

```text
assets/
  index.html             # 앱 root, viewport-fit=cover, 외부 CSS/script 태그
  app.css                # 의미 CSS 토큰과 공통/화면 스타일 전체
  crypto.js              # 기존 MkCrypto 그대로, 인증/암호 primitive
  client.js              # 연결·인증·직렬 RPC·bounded 진단 이벤트, DOM 의존 제거
  terminal.js            # MkTerminalCodec/MkLiveTerminal, canvas/input/history/size
  screens/home.js        # origin 데몬 카드, 빈 페어링 안내
  screens/workspaces.js  # exact cwd 또는 확장 group별 목록·검색·필터
  screens/session.js     # 헤더·탭·입력 dock·terminal mount
  screens/details.js     # 계정/Git/오케스트레이션 실제 데이터 화면
  screens/settings.js    # 로컬 설정·페어링 확인·클라이언트 진단
  app.js                 # 앱 셸·상태 store·hash router·화면 등록/생명주기
pwa.rs                   # 위 HTTP 경로와 gzip 정적 임베딩 명시
```

네트워크/CDN/런타임 package loader 없이 각 script를 동일 origin에서 `defer`로 순서대로 로딩한다: crypto → client → terminal → screens → app. namespace는 `globalThis.KrillMobile` 한 개 아래 `transport`, `screens`, `store`로 제한한다. 현재 terminal codec의 `MkCrypto` 전역/lexical 참조도 이 순서를 보장한다. ESM·bundle tool 도입은 필수가 아니다. 모든 새 경로에 MIME·plain/gzip pair를 `pwa.rs`에 등록하고 CSP는 유지한다(`pwa.rs:60`). `innerHTML`, inline onclick, 외부 폰트 요청 없이 `createElement`, `textContent`, `addEventListener`를 사용한다. 원본 UI 아이콘은 Krill의 기존 Lucide/브랜드 벡터를 출처와 함께 필요한 것만 로컬로 가져온다.

`app.css`에 의미 토큰을 단일 선언하고 각 모듈은 class만 부여한다. 예: `--surface-base`, `--surface-panel`, `--surface-raised`, `--border`, `--foreground`, `--muted-foreground`, `--status-working`, `--status-waiting`, `--radius-card`, `--space-md`, `--font-body`. 원본 토큰값은 §1의 비교표로 유지하되 실제 값은 Krill STYLEGUIDE의 해당 의미 토큰과 맞춘다. 입력 font 최소16, 일반 body14/meta12/헤더18–20 계층을 CSS 변수로 분리한다. status-working을 원본의 노랑으로 그대로 사용할지 Krill 상태 규범에 맞출지는 `ui::status`가 최종 기준이다. 뷰마다 색 hex를 중복 하드코딩하지 않는다.

### 4.2 해시 라우팅과 인증 순서

```text
#/                              홈(origin 데몬 카드)
#/host/local                    그룹/워크스페이스 목록
#/host/local/group/<key>         그룹 첫 세션을 고르는 기본 경로
#/host/local/group/<key>/s/<id>  세션 탭 활성화
#/accounts                      실제 계정 사용량·전환
#/orchestration                 runs/tasks/gates/questions
#/git/<id>                      변경 파일 목록과 읽기 전용 diff
#/settings                      구현한 로컬 설정 항목만
#/settings/terminal              폰트·키 구성 등 구현분
#/diagnostics                   현재 클라이언트 이벤트 로그
#/pair-confirm                  Krill 링크를 받은 경우의 확인 화면
```

**기존 `#key=`와 hash router 충돌을 해결해야 한다.** 첫 script 초기화에서 query `pair`, fragment `key`를 라우터보다 먼저 읽어 메모리에만 보관하고, 민감 fragment를 즉시 `replaceState`로 제거한다(`client.js:12`의 기존 보안 동작 유지). 인증 전에는 hashchange가 연결을 시작하지 않게 한다. 확인 화면 Pair를 명시적으로 누른 뒤 기존 handshake를 시작하고 credential은 인증 성공 응답 뒤에 저장한다. 취소 시 메모리 ticket/secret을 버리고 홈 안내로 돌아간다. `/?session=42` 웹훅 링크는 인증을 마친 뒤 snapshot에서 session42의 그룹을 찾아 세션 경로로 replace한다. 없는 세션은 “세션을 찾을 수 없음”을 표시하고 실제 그룹 목록으로 돌아가는 버튼을 제공한다. 인증·키 material을 hash route/DOM/log에 쓰지 않는다.

기본안의 key는 exact cwd를 충돌 없이 인코딩한 로컬 식별자, 확장안은 opaque worktree_key다. label/basename을 ID로 쓰지 않는다. `encodeURIComponent`/`decodeURIComponent` 실패를 처리하며 session은 안전한 정수 검증 후 사용한다. 현재 JSON id가 Number이므로 안전 범위 밖 u64는 조용히 반올림하지 말고 호환 오류로 처리한다. 장기적으로 문자열 ID 계약은 별도 변경이다.

브라우저 back/forward는 hashchange로 작동한다. `renderRoute()`는 이전 화면의 cleanup을 먼저 실행하고 `AbortController`로 DOM listener를 해제한다. 세션 이동 시 이전 terminal의 입력 generation을 폐기한다. 탭 선택 시 헤더와 본문을 patch하고 동일 terminal의 연결/캔버스를 매 polling마다 재생성하지 않는다. settings는 같은 main root의 페이지 전환이며 새 창을 열지 않는다.

### 4.3 상태 저장·연결·송신 규칙

```js
// 계약 스케치: 구현 코드가 아님
state = {
  connection: { phase, error, generation },
  snapshot: null,
  route: { screen, groupKey, sessionId },
  groups: new Map(), sessions: new Map(),
  drafts: new Map(),                 // sessionId → buffered text
  directHandles: new Set(), defaultedHandles: new Set(),
  ui: { search: '', filter: 'all', activeSheet: null },
  preferences: { textScale: 1, orderedKeyIds: [], visibleKeyIds: [] }
};
// Screen.render(root, state, { rpc, navigate, openSheet, terminal }) → cleanup
```

서버 state와 UI state를 나눈다. snapshot 교체가 draft/IME/focus/scroll position을 덮지 않게 key별 DOM을 갱신한다. 그룹 rollup은 Waiting/Blocked를 우선하고 Working/Done/Exited/Unknown을 Krill 상태 규칙으로 요약한다. unknown을 idle이라고 표시하지 않는다. 마지막 snapshot은 offline 화면용 stale 데이터로 보존하되 이전 상태가 실시간인 듯 표시하지 않는다. 인증 비밀 외에 터미널 내용·프롬프트를 localStorage에 자동 저장하지 않는다. localStorage에는 기존 credential key와 별도의 `krill-mobile-ui-v1` 로컬 설정만 저장하며 direct mode default tracking과 draft는 메모리에 둔다. Orca 원본과 동일하게 수동 mode opt-out이 refresh에서 유지되어야 한다.

`client.js`의 RPC 직렬화(`:22`)를 유지해 응답을 요청에 정확히 대응시킨다. polling 기준은 기존2,500ms(`:57`), 중복 refresh를 합치고 연결 세대가 달라지면 이전 응답을 버린다. 문서 숨김 시 불필요 refresh를 멈추고 복귀 시 한 번 갱신하는 것은 후속 제안이다. retry는 조회에만 적용하며 Prompt/Approve/Photo/Handoff/GateReply/QuestionReply 및 raw input을 결과 불확실 상태에서 자동 재송신하지 않는다. 기존 terminal은 Input ACK 전 연결이 끊기면 `Input delivery is uncertain; input will not be resent.`를 표시한다(`terminal.js:60`). 새 router도 이 보장을 유지한다.

터미널 WS는 렌더링 전용 delta+raw Input; workflow WS는 snapshot/actions다. 각 연결의 challenge/channel nonce를 공유하지 않는다. 기본 attach의 owns_size=false를 유지하고 `Fit to Phone` 전환의 ACK 성공 뒤에만 UI 활성 상태를 확정한다. 폰트 확대는 canvas의 글꼴·셀 폭/높이·커서·history 행 높이까지 함께 변경하고, size 소유권이 없는 상태에서는 화면을 수평 스크롤/로컬 확대할 뿐 PTY resize를 자동 송신하지 않는다. 원본의 phone mode reflow는 Krill 명시 resize 소유 정책에 맞춰 제한한다.

로컬 연결 로그는 `{at,level,message,detail}`를 작은 상한 배열로 보존하는 UI 기능을 제안한다. key/token/credential/터미널 전문을 기록하지 않는다. 현재 로깅되지 않은 relay·LAN 진단 성공 단계를 꾸며 넣지 않는다. 재연결 구현 뒤에만 reconnect action을 제공하고 실패 시 snapshot을 지우지 않고 입력을 잠근다.

### 4.4 화면별 DOM 스케치

아래는 §3 “재현 대상”만 포함한 기본안이다. `+`, 파일 탐색, 음성, Tasks provider, Notifications switch는 확장 전 숨긴다. aria-label과 stable data-testid는 아이콘·상태에도 붙인다.

```html
<!-- index.html: 100dvh column, viewport-fit=cover -->
<div id="mobile-app">
  <div id="connection-banner" role="status"></div>
  <main id="route-root"></main>
  <div id="sheet-root"></div>
  <div id="notice" role="alert" hidden></div>
</div>

<!-- home -->
<section class="home screen">
  <header class="home-topbar"><a class="brand">Krill</a><button aria-label="설정"></button></header>
  <div class="home-list">
    <h1>Welcome back</h1><h2 class="eyebrow">Mac</h2>
    <article class="host-card">
      <button class="host-main"><span class="host-icon"></span>
        <span class="host-identity"><strong></strong><span class="host-status"></span></span>
      </button><button aria-label="연결 동작"></button>
    </article>
    <!-- accounts/orchestration links only when transport is ready -->
  </div>
</section>

<!-- workspaces: header 고정, list만 스크롤 -->
<section class="workspaces screen">
  <header class="host-header"><button aria-label="뒤로"></button><h1></h1><span role="status"></span></header>
  <div class="workspace-toolbar"><button aria-label="목록 옵션"></button><button aria-label="검색"></button></div>
  <div class="workspace-search" hidden><input type="search" aria-label="워크스페이스 검색"></div>
  <div class="workspace-list">
    <section class="repo-group"><h2></h2>
      <button class="workspace-row" data-group=""><span class="status-glyph"></span>
        <span class="workspace-main"><strong></strong><small class="mono"></small></span>
        <span class="terminal-count"></span>
      </button>
    </section>
  </div>
</section>

<!-- session: 헤더/탭, 줄어들 수 있는 viewport, 키보드 위 dock -->
<section class="session screen">
  <header class="session-header"><button aria-label="워크스페이스로"></button>
    <div class="session-title"><h1></h1><span role="status"></span></div><button aria-label="Git 변경"></button>
  </header>
  <nav class="session-tabs" aria-label="세션"><div role="tablist" class="tab-scroll"></div></nav>
  <div class="terminal-frame"><div id="terminal-scroll"><pre id="terminal-history"></pre>
    <canvas id="terminal-grid" role="img"></canvas></div><div id="terminal-status" role="status"></div></div>
  <footer class="command-dock">
    <div class="accessory-row"><button aria-label="키보드 닫기" hidden></button><div id="keybar" role="toolbar"></div></div>
    <div class="input-row"><button class="live-focus-target"></button><textarea class="live-capture" aria-label="터미널 직접 입력"></textarea>
      <form class="buffered-command" hidden><input placeholder="Type a command…"><button type="submit" aria-label="보내기"></button></form>
      <button aria-label="사진 첨부"></button></div>
  </footer>
</section>
```

`terminal.js`의 현재 고정 DOM id(terminal-panel/title/status, fit-phone, load-history, history-status, keybar 등)는 mount 시 참조를 주입받도록 바꾸거나 숨은 호환 요소까지 명시적으로 유지해야 한다. 일부 id를 삭제한 뒤 constructor가 null에 listener를 붙이는 상태로 두지 않는다. 명세 스케치는 기본 구조이며 기존 renderer API adapter가 필요한 부분이다.

| 보조 화면 | DOM 구조와 노출 |
|---|---|
| 계정 | `section.screen > header(back,h1) + div.account-list > article(provider,label,email,usage progress,label,switch-button)`; percent 미상은 “알 수 없음”, 전환 중 버튼 비활성 |
| 오케스트레이션 | `header + section.needs-you(gate/question) + article.run > progress + ul.tasks`; gate는 실제 options 버튼, question은 label+textarea+submit. 응답 후 해당 id 갱신 |
| Git | `header(back,branch,+/−) + ul.changed-files + pre.patch`; 파일 tap RPC 결과는 textContent, 돌아오면 목록 스크롤 복원. 파일별 스타일은 optional file_details 전에는 분리하지 않음 |
| 설정 | `header(back,h1) + section.settings-group > a.setting-row(icon,label,chevron)`; terminal 로컬 설정, diagnostics, 검증된 제품 정보만. 토글은 실제 boolean일 때만 |
| 페어링 확인/빈 상태 | `header(back) + div.centered(title,body,Pair,Cancel,error,log)`; 권한 없는 카메라 UI 대신 기본 카메라로 QR 링크 여는 안내 + URL 붙여넣기. 검증 후 기존 Krill URL로 동작 |
| 진단 | `header(back,h1,status) + ol.connection-log > li(time,glyph,message,detail)`; 내용 없으면 “아직 연결 기록이 없습니다”라는 **Krill 제안 문구**. 원본은 entries=0에서 컴포넌트를 숨김 |
| 액션 시트 | `dialog.sheet > handle + heading + ul > li > button(icon,label,hint)`; showModal focus trap, Escape/backdrop/close 복귀, 확인 후만 destructive 실행. body scroll lock과 keyboard dismiss를 순서대로 처리 |

390px 기준 탭은128px, header/dock은 flex-none, terminal은 min-height:0/flex1, 두 스크롤 영역(탭·키 행)은 overscroll을 페이지로 전파하지 않는다. body 전체를 스크롤시켜 입력줄이 밀려 올라가게 하지 않는다. safe area는 `env(safe-area-inset-*)`, 높이는 `100dvh`+visualViewport 관측값으로 보정한다. motion 감소 시 스피너를 정적 상태 glyph로 대체한다. 모든 icon button에 이름, 오류 role alert, 연결/ACK 결과 role status를 둔다.

### 4.5 단계별 구현 순서와 파일 소유권(4커밋)

단계 사이에 **동일 파일 소유권을 겹치지 않는다**. 공통 CSS/쉘 계약을 이 명세로 먼저 고정한다. 아직 요청되지 않은 backend 확장 A–D는 이 프런트 계획에 몰래 포함하지 않는다. 기본 exact-cwd안부터 완성하고 backend 확장은 별도 커밋·별도 소유로 배정한다.

| 단계 | 단독 소유 파일(다른 단계 수정 금지) | 완료 조건/독립 커밋 |
|---|---|---|
| 1. 시각 토큰·레이아웃 | `assets/app.css` | §1 치수·Krill 의미 토큰, safe area, 화면별 layout/sheet/키보드 CSS. 기존 class도 최종 통합 전까지 유지. viewport390×844·320폭·가로 모드에서 overflow 점검. 커밋 예: `모바일 화면 토큰과 레이아웃을 정리한다` |
| 2. 전송/화면 모듈 | `assets/client.js`, 새 `assets/screens/home.js`, `workspaces.js`, `session.js`, `details.js`, `settings.js` | client는 연결·RPC API를 export하고 화면 렌더와 분리. 화면 모듈은 namespace에 등록, null metadata/빈 목록/지원 기능만 렌더. session 모듈은 terminal API를 호출하며 codec 구현을 소유하지 않음. 커밋 예: `모바일 화면과 데이터 연결 모듈을 나눈다` |
| 3. terminal adapter | `assets/terminal.js` | 고정 id 의존 제거·mount/unmount 계약, 기존 delta/ACK/history 보존, direct/IME/20키·폰트 변경·명시 resize. §4.3 생성 API와 합의한 adapter를 export. 커밋 예: `모바일 터미널 입력과 화면 생명주기를 연결한다` |
| 4. shell·라우팅·서빙 통합 | `assets/app.js`, `assets/index.html`, `pwa.rs`, 모든 변경·신규 asset의 `.gz` | app.js를 셸/라우터로 교체, 기존 합본에서 crypto/client/terminal 중복 실행 제거. 새 script 경로를 ASSETS에 추가, gzip 생성·임베딩·150KiB 압축예산 확인. 실제 origin에서 인증→목록→탭→입력→계정/Git/설정→복귀 확인. 커밋 예: `모바일 셸 라우팅과 정적 모듈 서빙을 통합한다` |

`assets/crypto.js`, manifest/icon은 이 UI 변경에서 그대로 사용한다. 단계1–3은 소스 단위로 독립 리뷰·커밋 가능하며 **실제 번들/서빙 전환은 단계4에서 한 번에** 수행한다. 전환 전 기존 합본 app.js가 계속 실행되므로 source만 바꿔 놓고 런타임 반영 완료라고 보고하지 않는다. 단계4에서 deterministic gzip(`gzip.compress(...,mtime=0)`)을 만들고 각 plain과 압축 해제 결과 일치 확인. 다른 워커가 동일 파일을 소유한 경우 실제 배정 전 지휘자가 경계를 재조정한다. 하네스 `scripts/` 수정은 이 계획의 소유권에 넣지 않는다.

필수 검증: 전체 `scripts/check.sh` 0, `pwa.rs` 내 embed route/gzip budget, 실제 LAN HTTP에서도 기존 JS 암호화가 동작하는지 확인(secure-context 전용 API로 교체 금지). meaningful UI 검증은 ①새로운 terminal default direct/사용자 opt-out 유지 ②한글 IME 조합과 special key 순서 ③disconnect 도중 입력 재전송 없음 ④탭/뒤로/설정 복귀 후 잘못된 세션 입력 없음 ⑤Fit to Phone ACK 실패 시 원상 복귀 ⑥hot→cold history의 스크롤 앵커 유지 ⑦지원하지 않는 메뉴가 DOM에도 없는지 ⑧키보드가 열린 상태에서 탭·키 첫 tap 정상 처리다. 서버 PTY 소유권은 건드리지 않는다.

## 5. Orca 원본 화면 참고 이미지와 재현의 한계

헤드리스 Chromium/agent-browser의 독립 세션에서 `set viewport 390 844` 후 로컬 원본 HTML을 열어 **viewport screenshot**을 저장했다. RN 런타임·iOS status bar를 촬영한 자료가 아니다. 원본 HTML이나 RN 소스는 수정하지 않았다.

| 원본 | 저장 이미지 | 확인한 화면 |
|---|---|---|
| `M/mock-homepage.html` | [mock-homepage-390x844.png](/tmp/mk-mobile/orca-shots/mock-homepage-390x844.png) | 기본 Current 상태. 위쪽 mock 선택기(Current/Task Entry/Empty State), Orca/Welcome back, 통계3개, Desktops 카드, Pair another desktop, 바닥 Settings |
| `M/mock-tasks.html` | [mock-tasks-390x844.png](/tmp/mk-mobile/orca-shots/mock-tasks-390x844.png) | Tasks 헤더, GitHub / Issues / Open 칩, 검색 `is:issue is:open`, issue 두 행(Open/Closed) |

두 mock은 시각 탐색용 파일이다. 특히 홈 mock의 위쪽 상태 선택기·아래 Settings·예시 통계 수치는 실제 RN 화면의 픽셀 기준이나 Krill 기능 요구사항이 아니다. **RN 소스의 현재 홈은 위쪽 Settings 버튼과 조건부 통계·Resume/Tasks/Quick Actions/계정 카드**가 기준이다. Tasks mock의 GitHub 이슈 목록은 Krill orchestration과 다르므로 Krill PWA에 복사하지 않는다. RN 폰트 rasterization, 실제 기기 safe area/키보드 높이, WebView GPU 렌더 차이는 소스만으로 확정할 수 없으며 기기 검증이 필요하다(픽셀 일치는 미측정).

이 문서의 산출물은 이 파일과 `/tmp/mk-mobile/orca-shots/`의 참고 PNG뿐이다. 이후 구현 워커는 §3 기본안 또는 명시적 backend 확장안을 선택하고, 각 화면에서 데이터가 없는 기능을 먼저 제거한 다음 §1·§2 치수에 맞춰 구현한다.

### 5.1 원문 토큰 인용의 MIT 고지

Orca 원본 `LICENSE` 전문(위 토큰 인용에 적용):

```text
MIT License

Copyright (c) 2026 Lovecast Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

```

### 5.2 내장 QR 디코더(jsQR)의 Apache-2.0 고지

`crates/krilld/src/mobile/assets/qr.js`는 [jsQR](https://github.com/cozmo/jsQR) 1.4.0(npm `jsqr@1.4.0`, `dist/jsQR.js`)을 수정한 사본이다. 라이선스는 Apache License 2.0이다. 수정 사항: Shift-JIS(Kanji) 변환표를 빈 객체로 바꿨다(페어링 QR은 byte 모드라 쓰지 않는다). esbuild 0.28로 축소했다. 파일 머리에 출처·라이선스·변경 고지를 남겼다. PWA는 이 파일을 QR 스캔이 필요할 때만 불러온다(BarcodeDetector가 없는 iOS Safari 등). jsQR 패키지에는 NOTICE 파일이 없다.

원본 `LICENSE` 전문:

```text
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS

   APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "{}"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

   Copyright {yyyy} {name of copyright owner}

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.

```
