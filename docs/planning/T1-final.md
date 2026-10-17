# T1 최종 — Orca 기능 전수 인벤토리 · 경쟁 제품 · Minikrill v1 범위 (2차 검증본)

**결론(가장 중요한 결정 3가지):**
1. **v1 범위는 다섯 가지로 고정한다.** 데몬이 PTY를 소유해 앱이 죽어도 세션이 살아 있게 하는 것, 놓친 출력을 바이트 단위로 재생하는 것, 워크트리별 에이전트 병렬 실행, 에이전트 상태와 알림, `mk` CLI(+`attach`)다. 내장 브라우저·에디터·이슈 트래커·모바일·클라우드·음성·computer-use는 모두 v1에서 뺀다(Orca 비테스트 TS 약 229만 줄 중 이 기능군이 대부분이다).
2. **포지셔닝을 "앱을 꺼도 에이전트가 산다"에서 한 단계 좁혀야 한다.** 이 기능은 이미 여러 제품에 있다. Orca·Superset(Electron 데몬), Jean(분리 프로세스), tmux 계열이 그렇고, **Claude Code 자체도 supervisor 기반 백그라운드 세션(`claude agents`, `claude daemon`)을 제공한다.** 1차 보고서가 "미확인"으로 둔 부분인데 2차 조사에서 확인했다. 그래서 Minikrill의 차별점은 다음 네 가지를 **수치로 증명**하는 것이다.
   - 어떤 TUI든(Codex·Gemini·OpenCode·셸·dev 서버) 에이전트 종류와 무관하게 PTY 수준에서 유지한다.
   - 놓친 출력을 바이트 단위로 재생한다.
   - 네이티브로 앱+데몬 150MB 이하(터미널 10개 기준)를 지킨다.
   - 업데이트 뒤 고아 데몬이 0개다.
3. **라이선스는 코어 전체를 `MIT OR Apache-2.0`으로 무료 공개한다.** 유료화는 출시 3개월 뒤 지표를 보고 원격·다중 머신 기능에 한해 재검토한다. 경쟁자 상당수(Orca·Claude Squad·Agent Deck·Emdash·Jean)가 무료 OSS다. 데몬이 사용자 셸과 자격 증명을 다루므로 소스를 공개하는 것이 곧 신뢰를 얻는 수단이다.

- 작성: T1 2차(검증) 워커, 2026-10-14. 1차 보고서 `.ref/drafts/T1-A.md` 전문을 원본과 대조했다.
- 근거 표기:
  - `D/` = `.ref/orca/docs/site/content/docs/`
  - `S/` = `.ref/orca/src/`
  - 그 밖의 경로는 `.ref/orca/` 기준 상대경로
  - 웹 근거는 URL로 단다.
  - 측정하지 못한 수치에는 **(추정)** 을 붙인다.
- 검증 방법:
  - 로컬: `sed -n`/`grep`으로 인용 줄을 직접 열었고, `find … | xargs cat | wc -l`로 줄 수를 다시 셌다.
  - 이 맥미니에서 `ps`·`footprint`로 메모리를 다시 측정했다.
  - GitHub·crates.io: `gh api`와 crates.io API로 조회했다.
  - 경쟁 제품: 공식 사이트·저장소·문서를 조사 보조 에이전트 2개가 1차 출처로 대조했다. 그중 핵심 주장(Claude Code agent view, Jean detached, Superset 메모리 공개, cmux 세션 복원)은 이 워커가 원문을 직접 다시 열어 확인했다.
- 다른 트랙과 겹치는 내용(데몬 프로토콜=T2, 렌더링·IME=T3, 에이전트 감지·Git·배포=T4)은 짧게 짚고, 해당 트랙을 표시했다.

---

## 1. 이전 조사 정정 (주장 vs 실제 vs 근거)

1차 보고서의 문서·소스 인용(`D/…mdx:줄`)은 **대부분 정확했다.** 확인한 것:
- 세션 복원·워크트리·터미널·에이전트·설정·알림·SSH·모바일·클라우드 README 인용
- 기능별 디렉터리 파일 수와 줄 수(16개 디렉터리 전부 ±1 이내)
- 키 바인딩 91개, 에이전트 표시 이름 47개, hibernation 안전 조건 8개
- sherpa-onnx 1.12.37(`package.json:203,341-345`), `LICENSE:3`

틀렸거나 보강이 필요한 항목은 아래와 같다.

### 1.1 Orca 소스·문서

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| 1 | 문서 사이트 57페이지 | **58개** `.mdx` | `find D -name '*.mdx' \| wc -l` |
| 2 | `docs/audits/`는 약 45개 디렉터리이고 대부분 `*-retention` | **46개 중 `*-retention`은 21개**다. 이름에 lifetime·retirement·budget·late-exit 등이 붙은 자원 수명 관련 항목까지 합치면 약 30개다. "대부분 retention"은 과장이지만 "메모리·자원 보유 문제가 반복된다"는 결론은 유지된다 | `ls docs/audits` |
| 3 | 데몬 소켓 교훈은 `S/main/daemon/AGENTS.md:15-17` | 핵심 문장("Seven review rounds … twenty-three defects")은 **16-17행** | `grep -n` |
| 4 | Jump Palette에 Cmd-1~6 있음 | 문서에 근거 없음. 실제는 "빈 쿼리일 때 최근 세션 최대 6개(needs-you 우선)" | `D/model/quick-open.mdx:28` |
| 5 | `orca status` 근거 `D/cli/overview.mdx:22-25` | 22-25행은 skills 설치 예시다. `orca status`는 **33행** | `D/cli/overview.mdx:33` |
| 6 | 링크는 Cmd-클릭으로 연다(Orca 기본) | Orca 기본은 **일반 클릭 → 액션 팝오버**이고, 팝오버를 끄면 modifier-click이다 | `D/terminal.mdx:25-29` |
| 7 | (누락) | **`S/main/runtime/`이 Orca에서 가장 큰 단일 디렉터리다.** 비테스트 1,478파일, 약 238K줄. 1차 인벤토리에 전혀 없었다 | `find S/main/runtime …` |
| 8 | (누락) 그 밖의 기능 | sparse checkout 프리셋(`S/renderer/src/components/sparse/`), 워크스페이스 디스크 사용량(Space, `WorkspaceSpacePage.tsx`·Resource Manager "workspace disk scans" `D/settings.mdx:28`), 숨긴 터미널 parking(`use-terminal-parking-pass.ts`), 에이전트 CLI 설치 감지 preflight(`S/main/preflight/`), 진단 번들 업로드(`S/main/observability/`), macOS 키체인·암호화 API 키 저장(`S/main/macos-keychain/`, `S/main/credentials/`), 로그아웃 시 데몬 퇴역(`S/main/daemon/macos-login-session-death-watch.ts:32-36`) | 각 경로 |
| 9 | 앱 소스 버전 | 소스 `package.json` version은 **1.4.214**, 설치된 앱은 1.4.224다. 소스와 바이너리 버전이 다르므로, 소스 근거는 "HEAD a0c46b40 기준"으로 읽어야 한다 | `package.json:3` |

### 1.2 GitHub 수치·이슈

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| 10 | 열린 이슈 8,282개 | `open_issues_count` 8,288은 **PR을 포함한 값**이다. 실제로는 **열린 이슈 4,219 + 열린 PR 4,069**다. "이슈가 많다"는 결론은 유지된다 | `gh api search/issues` (2026-10-14) |
| 11 | 별 92,099 | 92,108(조회 시점 차이) | `gh api repos/stablyai/orca` |
| 12 | #19116: 탭 10개에 GPU 메모리 약 4GB | 맞지만 **`orca serve` 모드의 브라우저 탭** 이슈다. 데스크톱 일반 사례로 인용하면 안 된다 | 이슈 제목 "serve mode: GPU process memory grows…" |
| 13 | 이슈 상태 | #9138·#12728(Windows)·#15241·#20707·#14878·#24288·#26887·#22804 모두 **OPEN** 확인. #5049는 CLOSED | `gh api …/issues/N` |

### 1.3 이 맥미니 실측

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| 14 | 이 맥에서 Orca RSS 합계 약 1,617MB(Renderer 740.7 / Main 411.7 / 데몬 135.9MB) | 재측정(약 4시간 뒤): **Renderer 768.0 / Main 434.3 / 데몬 v43 138.5(PTY 19개) / GPU 127.4 / scanner 79.6MB.** 일시적인 CLI 프로세스(84.8MB)를 빼면 **약 1,674MB**다. footprint는 775 / 389 / 171MB다. 브리프의 "Renderer 665 / Main 428"과 함께 보면 **Renderer는 시점에 따라 665~768MB로 움직인다** | `ps -axo rss`, `footprint <pid>` |
| 15 | 구버전 데몬 v40·v41이 PTY 0개로 남아 있음 → #9138 재현 | v40(16.5MB, 7일째)·v41(21.1MB, 5일째)이 **자식 프로세스 0개로 여전히 남아 있다**(재측정). 다만 #9138 본문은 세션을 가진 구세대 데몬이 쌓여 370개 프로세스와 swap 25GB에 이른 사례다. 이 맥은 **"빈 구세대 데몬이 퇴역하지 않는다"는 약한 형태**다. "같은 근본 원인(세대 정리 실패), 다른 심각도"로 고친다 | `pgrep -P`, 소켓 경로 `daemon-v40.sock`·`daemon-v41.sock`, #9138 본문 |
| 16 | 이 맥의 `claude` 16개: 4,378MB, 평균 274MB | 재측정 **19개: 7,041MB, 평균 371MB**(시점 차이). "에이전트 자체가 앱보다 훨씬 많은 메모리를 쓴다"는 결론이 더 강해진다 | `ps -ax -o rss,comm` |

### 1.4 경쟁 제품

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| 17 | **Claude Code 1st-party의 세션 지속 동작은 미확인** | **확인됨.** Claude Code CLI의 agent view(research preview)는 별도 **supervisor 프로세스**가 백그라운드 세션을 돌린다. 셸이나 터미널을 닫아도 계속 돌고, 자동 업데이트와 잠자기를 넘어 유지된다. 재부팅·종료하면 멈추고, 대화 기록에서 재개된다. 붙어 있지 않은 유휴 세션은 약 1시간 뒤 프로세스가 정리된다(Ctrl+T로 고정하면 유지). `claude agents --json`(state·pid·waitingFor), `claude attach`, `claude logs`, `claude daemon status`가 있다 | https://code.claude.com/docs/en/agent-view.md (158-160·788·822-824행) |
| 18 | Jean: 세션 유지 미확인 | **유지된다.** `jean-core/src/chat/detached.rs` 주석 원문: "spawning Claude CLI as a fully detached process that survives Jean quitting". 출력은 JSONL 파일에 쓰고 앱이 그 파일을 tail한다. 즉 PTY 방식이 아니다 | https://github.com/coollabsio/jean (해당 파일) |
| 19 | Emdash: 세션 유지 미확인 | **tmux 옵션**(`project.tmuxByDefault`)으로 유지한다(opt-in). 원격 `workspace-server` 데몬도 있다 | https://github.com/generalaction/emdash (agents/architecture/settings.md, `packages/core/src/services/pty/api/tmux.ts`) |
| 20 | Superset: 공식 메모리 수치 없음 | **공개했다.** 2026-07-19 changelog에 터미널 16개 테스트 결과가 있다. Renderer RSS 1,127→838MB, JS heap 581→286MB. 숨긴 터미널은 최근 N개만 메모리에 두고 나머지는 디스크로 내린다(기본 12개, 측정은 4개 설정). 현재는 `packages/pty-daemon`이 분리돼 있고, 데몬을 업그레이드해도 세션이 살아남는다(fd 넘겨주기) | https://github.com/superset-sh/superset/blob/main/apps/marketing/content/changelog/2026-07-19-performance-memory-and-load.mdx |
| 21 | Superset Pro $20/인·월 | 월결제 $20, **연결제 시 월 $15**, Enterprise는 별도 견적 | https://superset.sh/pricing |
| 22 | cmux: 무료 + Founder's Edition | Free / **Pro $40~50 / Max $160~200 / Team $60** / Enterprise(주로 Cloud VM). Founder's Edition은 별도 후원형이다. tmux 외에 **`cmux local-zellij`** 도 있다 | https://cmux.com/pricing, https://github.com/manaflow-ai/cmux README 272-277행 |
| 23 | Conductor가 Tauri라는 것은 "반쯤 미확인" | 창업자 스레드와 함께, **보조 에이전트가 0.90.2 앱 번들을 직접 검사**했다. 결과는 `tauri_runtime_wry` 문자열, arm64 단일 바이너리, `conductor-runtime` 안의 `@anthropic-ai/claude-agent-sdk`·`codex-sdk` 문자열이다. Tauri + SDK 래핑으로 본다(이 워커가 재검사하지는 않았다) | https://threadreaderapp.com/thread/1945870105109246401.html, https://formulae.brew.sh/cask/conductor |
| 24 | zellij 장시간 1.2GB 누수 | 이슈 본문은 **"more than 2GB"**다. v0.30.0, 2022-08 등록, **2023-08 종료**된 오래된 사례다 | https://github.com/zellij-org/zellij/issues/1625 |
| 25 | Terragon 종료일 미확인 | **2026-01-16**(OSS 스냅샷 공지) | https://github.com/terragon-labs/terragon-oss |
| 26 | Warp 라이선스 근거는 helpnetsecurity 기사 | 사실 자체는 맞다. 기사에는 클라이언트 AGPL만 나오므로, UI 프레임워크(`warpui_core`, `warpui`) MIT 근거는 **저장소 README**로 바꾼다 | https://github.com/warpdotdev/warp |
| 27 | Codex 앱 worktree 근거는 helpnetsecurity 기사 | 기사에는 worktree라는 단어가 없다. 공식 문서로 바꾼다. 로컬 작업은 "Prevent sleep while running"을 안내할 뿐, 앱 종료 후 유지 여부는 문서에 없다 | https://learn.chatgpt.com/codex/environments/git-worktrees, https://learn.chatgpt.com/codex/long-running-work |
| 28 | (누락) | 별 수: Agent Deck 1.08k, Vibe Kanban 28.4k(마지막 릴리스 v0.1.44, 2026-04-24), tmux 49.9k, zellij 35.7k | `gh api` |

---

## 2. 확정 결정 사항

| # | 결정 | 권고안 | 대안 | 근거 | 되돌리기 비용 |
|---|---|---|---|---|---|
| D1 | v1 범위 | §3.2의 **Must만** 넣는다. 출시 이슈는 §7의 P0·P1이다. 묶음은 "터미널 + 데몬 세션 유지·재생 + 워크트리 + 상태·알림 + CLI" | Orca 기능을 넓게 복제 | Orca 비테스트 TS 약 229만 줄(§3.1.2), 열린 이슈 4,219개. 이 범주의 종료·방치 사례: Vibe Kanban 2026-04, Crystal 2026-02, Terragon 2026-01-16 | 낮음. 범위는 나중에 넓힐 수 있다 |
| D2 | 핵심 가치 문장 | "에이전트 종류와 무관한 **PTY 수준 지속 + 놓친 출력 바이트 재생 + 네이티브 ≤150MB**" | "앱을 꺼도 에이전트가 산다"만 내세우기 | 그 기능은 Claude Code supervisor·Orca·Superset·Jean·tmux 계열이 이미 제공한다(§1.4 #17~#20) | 낮음(마케팅 문구) |
| D3 | 내장 에디터·브라우저 | **넣지 않는다.** 대신 "Open in"(VS Code·Cursor·Zed·Finder)과 링크를 시스템 브라우저로 여는 것으로 처리한다 | WKWebView 경량 브라우저(v2 검토) | `S/main/browser` 338파일 43.5K줄, 에디터 Monaco. 순수 Rust 원칙과 메모리 목표에 맞지 않는다 | 중간. 나중에 넣으면 UI 구조를 확장해야 한다 |
| D4 | 상태 감지 위치 | **hook 수신기와 상태 store를 데몬에 둔다.** 앱은 구독만 한다. OSC 타이틀은 보조 신호다. Claude는 `claude agents --json`도 선택적 신호원으로 쓴다 | 앱이 수신(Orca 방식, `endpoint.env`를 매번 다시 읽어 우회 `D/agents/hooks-memory.mdx:25-27`) | 앱이 재시작돼도 상태가 끊기지 않아야 한다. Orca `AGENTS.md`의 "Agent Status" 절(108행) | 높음. 데몬 프로토콜에 들어간다(→ T2·T4) |
| D5 | CLI | `mk` 단일 바이너리: `ls / new / send / read / wait / attach / doctor`. **`attach`로 일반 터미널(Ghostty·Terminal.app)에서도 같은 세션에 붙는다** | GUI만 | Orca CLI가 에이전트 조작의 표준 경로다(`D/cli/overview.mdx`). Claude Code도 `claude attach`로 같은 UX를 1st-party로 제공한다 | 중간 |
| D6 | 권한 기본값 | **에이전트 자체 기본값(Manual).** YOLO는 전역 토글을 명시적으로 켜야 적용 | Orca처럼 YOLO가 기본 | Orca는 새 실행에 `--dangerously-skip-permissions` 등을 미리 넣는다(`D/agents/supported.mdx:19`, `D/model/agents-sessions.mdx:63`). 워크트리는 보안 샌드박스가 아니다 | 낮음 |
| D7 | 라이선스 | `MIT OR Apache-2.0`, 무료. 후원(GitHub Sponsors) | Open-core / FSL·ELv2 / 비공개 유료(§3.4.3) | §3.4.3 | 높음. 공개 뒤에는 라이선스를 되돌리기 어렵다 |
| D8 | 플랫폼 | macOS Apple Silicon 우선. x86_64는 빌드만 | 크로스플랫폼 동시 지원 | 사용자 환경. Orca `AGENTS.md`에 Windows 언급 12회와 별도 절("Cross-Platform Support" 77행) | 중간 |
| D9 | 모바일 대응 | 알림 웹훅(ntfy·Telegram·Discord URL)만, opt-in | 모바일 앱 + 클라우드 relay | Orca `cloud/`는 director/cell relay, IAM fence broker, APNs/FCM push gateway, PostgreSQL을 운영한다(`cloud/README.md:1-57`). 1인 개발자의 운영비와 보안 책임을 넘는다 | 낮음 |
| D10 | 원격 | v2: Rust 정적 데몬 바이너리를 원격에 복사하고, SSH로 소켓을 포워딩하고, `mk attach --host`로 붙는다 | Orca식 relay(원격 `node-pty` 컴파일 필요) | Orca는 원격에 make·C++·python3이 없으면 원격 터미널이 실패한다(`D/ssh.mdx:85-96`) | 낮음(v2) |
| D11 | 세션 히스토리·hibernation | v1.x. 요청할 때만 스캔한다(상주 프로세스 없음). hibernation은 Orca의 안전 조건 8가지를 그대로 따른다 | 상주 scanner(Orca 실측 79.6MB, `--max-old-space-size=384`) | 이 맥의 `claude` 19개가 7.0GB를 쓴다(§1 #16) | 낮음 |
| D12 | 1st-party 공존 | Claude Code 백그라운드 세션(`claude agents`)과 **경쟁하지 않고 공존**한다. `claude agents --json`을 읽어 상태를 보여 주는 통합은 Should | 무시 | `claude agents --json`은 "외부에서 세션 상태를 읽는 지원 인터페이스"로 문서화돼 있다(agent-view.md) | 낮음 |
| D13 | 텔레메트리 | **v1은 텔레메트리 없음.** 로컬 크래시 로그와 사용자가 직접 보내는 진단 번들만 둔다 | Orca식 익명 opt-out(`D/telemetry.mdx:10-14`) | 1인 개발 운영비를 줄이고, "계정·클라우드 없음"이라는 신뢰 메시지를 지킨다 | 낮음(나중에 opt-in으로 추가 가능) |

---

## 3. 상세 설계·요구사항

### 3.1 기준선: Orca

#### 3.1.1 메모리 실측 (2026-10-14, 이 맥미니, macOS 27.0.1, RAM 32GB, Orca 1.4.224, 실행 4일째)

| 프로세스(역할) | RSS | phys_footprint (peak) | 비고 |
|---|---:|---:|---|
| Renderer | 768.0MB | 775MB (798) | React UI 전체. 브리프 시점에는 665MB |
| Main | 434.3MB | 389MB (518) | Electron 메인 |
| 터미널 데몬 v43 | 138.5MB | 171MB (257) | 자식 19개(PTY) |
| GPU process | 127.4MB | — | |
| session-scanner (`--max-old-space-size=384`) | 79.6MB | — | 에이전트 transcript 스캔용 상주 프로세스 |
| NetworkService | 32.5MB | — | |
| 기타 Helper(파일 감시 등) | 28.5MB | — | |
| computer-use 사이드카 + 헬퍼 | 12.8 + 12.6MB | — | |
| **구세대 데몬 v41 / v40** | **21.1 / 16.5MB** | — | **자식 0개, 5일·7일째 잔존** |
| **합계(일시 CLI 프로세스 제외)** | **약 1,674MB** | 주요 3개 합 1,335MB | 브리프의 "약 1.6GB"와 일치 |

- PTY 하나에 붙는 로그인 셸(`login … zsh -l`)은 약 7.4~10MB다. 셸과 에이전트는 위 합계에 들어 있지 않다.
- 같은 시점 이 맥의 `claude` 19개는 합계 7,041MB(평균 371MB)다. **시스템 전체로 보면 에이전트가 앱보다 4배 이상 메모리를 쓴다.** 그래서 앱+데몬 목표(≤150MB)와 별개로 hibernation(유휴 에이전트 재우기)이 v1.x에서 효과가 크다.
- 번들 크기: `/Applications/Orca.app` = 642MB(`du -sh`).
- 스택: Electron 43.7.5, `node-pty ^1.1.0`, `@xterm/xterm 6.1.0-beta.304`(`package.json:275,197,268`).

#### 3.1.2 코드 규모(HEAD a0c46b40, shallow clone, 비테스트 TS/TSX, fixture 포함이라 대략값)

| 디렉터리 | 줄 수 |
|---|---:|
| `src/main` | 899K |
| `src/renderer` | 927K |
| `src/shared` | 203K |
| `mobile/src` | 196K |
| `cloud` | 41K |
| `src/cli` | 27K |
| **합계** | **약 2.29M** |

- 테스트 파일은 13,244개이고, 테스트를 포함한 전체 TS는 약 5.33M줄이다.
- 기능별 무게(비테스트 파일 수 / 줄 수):

| 기능 | 경로 | 파일 / 줄 |
|---|---|---:|
| 런타임 코어 | `S/main/runtime` | 1,478 / 238K |
| native-chat | `S/main/native-chat` | 402 / 53K |
| 내장 브라우저 | `S/main/browser` | 338 / 43.5K |
| SSH | `S/main/ssh` | 291 / 40.5K |
| 데몬 | `S/main/daemon` | 289 / 36K |
| 영속화 | `S/main/persistence` | 254 / 32K |
| 세션 히스토리 | `S/main/ai-vault` | 162 / 21K |
| skills | `S/main/skills` | 103 / 15K |
| agent-hooks | `S/main/agent-hooks` | 106 / 13K |
| rate-limits | `S/main/rate-limits` | 81 / 10.9K |
| ACP | `S/main/acp` | 64 / 10.2K |

- GitHub(2026-10-14):
  - 별 92,108 / 포크 5,884
  - 열린 이슈 4,219 + 열린 PR 4,069
  - 2026-03-17 생성, MIT("Copyright (c) 2026 Lovecast Inc.", `LICENSE:3`)
  - 정식 릴리스가 거의 매일 나온다: v1.4.220(10-04) → v1.4.224(10-10)
- **시사점: 기능 폭으로는 1인 개발이 따라잡을 수 없다. 범위를 줄이는 것 자체가 제품 전략이다.**

#### 3.1.3 "연결 끊김"과 메모리 문제는 Orca에서도 계속 열려 있다

| 이슈 | 내용 | 상태 | Minikrill 시사점 |
|---|---|---|---|
| [#9138](https://github.com/stablyai/orca/issues/9138) | 업데이트 뒤에도 구세대 데몬이 살아 있어 보이지 않는 세션이 쌓임(swap 25GB, 프로세스 370개) | OPEN. 이 맥에서는 약한 형태(빈 데몬 잔존) 관측 | 세대 교체·퇴역을 P0 테스트로 둔다 |
| [#12728](https://github.com/stablyai/orca/issues/12728) | 데몬 private 메모리가 300~400MB로 늘고 해제되지 않음(Windows) | OPEN | 24시간 메모리 증가 지표 |
| [#15241](https://github.com/stablyai/orca/issues/15241) | 에러를 계속 출력하는 pane이 있으면 Renderer 메모리·CPU가 끝없이 증가 | OPEN | 출력 폭주 격리 지표 |
| [#20707](https://github.com/stablyai/orca/issues/20707) | 런타임 연결이 끊기면 UI가 클릭되지 않고 스스로 복구되지 않음 | OPEN | 앱↔데몬 재연결은 자동이어야 하고, 상태가 보여야 한다 |
| [#22804](https://github.com/stablyai/orca/issues/22804) | 상태 이름 바꾸기를 동기로 처리해 터미널이 멈춤 | OPEN | 상태 처리는 PTY 경로와 분리한다 |
| [#24288](https://github.com/stablyai/orca/issues/24288) | Orca 터미널 안의 Claude Code가 세션을 잃고 재로그인을 요구 | OPEN | 환경변수·키체인 접근을 보존한다(→ T4) |
| [#14878](https://github.com/stablyai/orca/issues/14878), [#26887](https://github.com/stablyai/orca/issues/26887) | 원격 연결이 자주 끊기고, 재연결 뒤 화면이 멈춘 것처럼 보임 | OPEN | v2 원격 설계 때 참고 |
| `mobile/issue-5049-unresponsive-session-findings.md` | 재연결 루프가 12회 시도 후 영구 정지해, 끊긴 게 아니라 멈춘 것처럼 보임 | CLOSED(#5049) | **재연결은 포기하지 않는다. 끊긴 상태는 명시적으로 보여 준다** |

- 데몬 소켓 소유권 문제만으로 "검토 7회, 결함 23건"이 나왔다(`S/main/daemon/AGENTS.md:16-17`). 교훈은 "자기가 만들지 않은 이름은 지우지 않는다"는 것이다(→ T2 설계 원칙).

### 3.2 Orca 기능 전수 인벤토리와 v1 분류

분류 기준:
- **Must**: v1.0 출시 조건이다. 없으면 "세션 안 끊김"이나 "병렬 에이전트 운용"이 성립하지 않거나, 상용 품질에 꼭 필요하다.
- **Should**: v1.x(출시 후 1~3개월).
- **Later**: v2 이후 재검토.
- **Won't**: 포지셔닝("초경량·터미널 우선")과 정면으로 부딪히거나, 1인 운영비를 감당할 수 없다.
- 사용자 가정: 1인 개발자이며 에이전트 5~20개를 병렬로 돌린다. 1순위 가치는 가벼움과 세션 안 끊김이다.

#### 3.2.1 워크스페이스·레포 관리

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| A1 | 레포 추가, base ref 자동 감지 | `D/first-session.mdx:10-14`, `S/main/git/` | **Must** | 모든 흐름의 시작점 |
| A2 | 워크트리 생성(백그라운드 fetch + `git worktree add`, 진행 표시·취소·재시도) | `D/model/worktrees.mdx:25-27`, `S/main/git/worktree-add.ts:207` | **Must** | 병렬 에이전트의 격리 단위. Orca도 git CLI를 호출한다. Minikrill도 `git` CLI를 호출한다(gix 0.89는 worktree add 지원이 미확인) |
| A3 | start-from 선택(base / 로컬 브랜치 / 커밋 SHA / 원격 브랜치) | `D/model/worktrees.mdx:29-36` | **Must** | 입력창 하나로 처리 |
| A4 | 워크트리 삭제(브랜치 함께 삭제, 확인 대화상자), 아카이브·sleep | `D/model/worktrees.mdx:15,113` | 삭제 **Must** / 아카이브 Should | 정리가 안 되면 병렬 사용이 무너진다 |
| A4b | 보존 브랜치 검토(병합 안 된 브랜치) | `D/model/worktrees.mdx:125-127` | Should | v1.0은 "git이 거부하면 경고하고 브랜치는 남김"만 |
| A5 | 공유 경로: Settings Shared Paths, `orca.yaml worktree.sharedDirectories`(링크), `.worktreeinclude`(APFS clone 복사) | `D/model/worktrees.mdx:38-65`, `D/model/orca-yaml.mdx:117-142` | Should | `.env`·`node_modules` 문제를 해결해 주는 체감 가치가 크다. Orca는 복사 상한을 2GiB·5만 항목으로 둔다(`orca-yaml.mdx:140`) |
| A6 | `orca.yaml` setup/archive 스크립트, `defaultTabs`, `setupAgentStartupPolicy`, `issueCommand` | `D/model/orca-yaml.mdx:12-46,75-115` | Should(setup + `.worktreeinclude`만 호환) | 이전 장벽을 낮춘다. Orca는 `CONDUCTOR_ROOT_PATH`·`GHOSTX_ROOT_PATH` 호환 별칭까지 준다(`orca-yaml.mdx:98`) |
| A7 | 자동 이름(해양생물), 이모지 shortcode 이름 | `D/first-session.mdx:18`, `D/model/worktrees.mdx:79-81` | 자동 이름 **Must** / 이모지 Won't | 자동 이름은 몇 줄이면 된다 |
| A8 | 브랜치 이름 직접 지정, 부모 워크스페이스 중첩 | `D/model/worktrees.mdx:83-91,115-117` | 지정 **Must** / 중첩 Later | 중첩은 orchestration과 함께 |
| A9 | 사이드바: 프로젝트별 그룹, 필터(sleeping·CLI 생성·detached 등), 핀, 재정렬, 다중 선택, 인라인 이름 변경 | `D/model/worktrees.mdx:93-119,127` | 그룹·핀·이름 변경 **Must** / 나머지 Later | 세션 10~20개를 한눈에 보는 핵심 화면 |
| A10 | Resource Manager(CPU·메모리·세션·데몬 제어·디스크 스캔), 워크스페이스 정리 | `D/settings.mdx:28`, `D/model/worktrees.mdx:121-123`, `S/main/memory/` | **Must(경량판)** | "가벼움"을 직접 보여 주는 화면. 세션별 메모리, 데몬 재시작, 세션 kill |
| A11 | 멀티레포 프로젝트 그룹, 폴더 워크스페이스 | `D/model/worktrees.mdx:129-135`, `S/main/project-groups/` | 비-git 폴더 세션 **Must** / 그룹 Later | git이 아닌 폴더에서도 에이전트를 띄울 수 있어야 한다 |
| A12 | 외부 워크트리(`git worktree add`, Claude Code·GSD) 표시·숨김 | `D/model/worktrees.mdx:137-148` | Should | Claude Code가 만든 워크트리와 공존해야 한다 |
| A13 | Quick Open(Cmd-P, 파일 검색) | `D/model/quick-open.mdx:10-12` | Won't(v1) | 에디터가 없다 |
| A14 | Jump Palette(Cmd-J): 워크트리·탭·최근 세션(needs-you 우선, 최대 6개), 없으면 생성 행 | `D/model/quick-open.mdx:20-39` | **Must** | 세션이 많아지면 이동이 병목이다 |
| A15 | 새 탭 omnibox(탭·파일·URL·에이전트) | `D/model/quick-open.mdx:14-18` | Won't | 브라우저·에디터를 전제로 한 기능 |
| A16 | **(추가)** sparse checkout 프리셋 | `S/renderer/src/components/sparse/` | Later | 거대 모노레포 사용자용 |
| A17 | **(추가)** 워크스페이스 디스크 사용량(Space) | `S/renderer/src/components/workspace-space/`, `D/settings.mdx:28` | Should(A10에 포함) | 워크트리가 쌓이면 디스크가 문제가 된다 |

#### 3.2.2 터미널

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| B1 | **PTY 데몬 분리**: Cmd-Q·자동 업데이트·앱 크래시에도 에이전트가 살고 warm reattach | `D/model/session-restore.mdx:14,24-32`, `S/main/daemon/` | **Must(P0)** | 제품의 존재 이유(→ T2) |
| B2 | 스크롤백 복원(**앱이 닫혀 있던 동안의 출력 포함**), 탭·분할·포커스 복원 | `D/model/session-restore.mdx:10-16` | **Must(P0)** | "놓친 출력까지 이어진다"가 바로 이것 |
| B3 | 재부팅·데몬 사망 뒤 cold restore(레이아웃 + 마지막 스크롤백) | `D/model/session-restore.mdx:18-22,34-36` | **Must** | 프로세스는 잃더라도 화면과 맥락은 남아야 한다 |
| B3b | cold restore 뒤 에이전트 resume(`claude --resume <id>`, `codex resume`) | `D/agents/hibernation.mdx:46-52`, `D/agents/session-history.mdx` | **Must** | 재부팅 뒤 클릭 한 번으로 대화를 잇는 것까지가 "안 끊김"이다 |
| B3c | **(추가)** 로그아웃 시 데몬 퇴역 | `S/main/daemon/macos-login-session-death-watch.ts:32-36` | **Must(정책)** | 로그아웃하면 PAM 컨텍스트가 죽어 세션을 살릴 수 없다. "로그아웃·재부팅은 생존 범위 밖, cold restore 대상"이라고 명시한다(→ T2) |
| B4 | 탭·분할(드래그, 중첩, 경계 위치 저장) | `D/model/tabs-panes-splits.mdx:15-53` | **Must** | v1.0은 단축키 분할(Cmd-\, Cmd-Shift-\, `D/terminal.mdx:119-120`)과 위치 저장. 드래그는 Should |
| B5 | 스크롤백 검색(Cmd-F, regex·대소문자) | `D/terminal.mdx:19-21` | **Must** | 기본기 |
| B6 | 링크 액션(URL·파일 경로·OSC 8), 팝오버 | `D/terminal.mdx:23-29` | 열기 **Must** / 팝오버 Later | Minikrill은 Cmd-클릭으로 바로 연다(Ghostty·iTerm 관례) |
| B7 | OSC 52 클립보드 쓰기(기본 허용) | `D/terminal.mdx:13-17` | **Must** | tmux·nvim·원격 복사에 필요하다. **쓰기만 허용하고 읽기(질의)는 막는다**(§3.6) |
| B8 | **kitty 키보드 프로토콜**(Shift+Enter 등) | `D/terminal.mdx:122-124` | **Must** | Claude Code에서 Shift+Enter 줄바꿈(→ T3) |
| B9 | 테마, Ghostty 설정 가져오기, Warp 테마 가져오기 | `D/terminal.mdx:35-47`, `S/main/ghostty/`(6파일 792줄) | 테마 **Must** / Ghostty Should / Warp Later | Ghostty 사용자층과 겹치고 구현이 싸다 |
| B10 | Copy Context(최근 출력 일부를 transcript로 복사) | `D/terminal.mdx:31-33` | Should | 에이전트 간 맥락 전달용 |
| B11 | 셸 설정(로그인 셸 기본, 사용자 지정 셸·인자), 환경 보정 | `D/terminal.mdx:49-64`, `S/main/pty/`(conda·codex-home 등 30파일) | **Must** | PATH·nvm·mise 문제는 지원 문의 1순위가 된다 |
| B12 | Windows 셸·WSL·관리자 모드 | `D/terminal.mdx:66-112` | Won't | macOS 우선 |
| B13 | 플로팅 터미널 / Floating Workspace | `D/terminal.mdx:128-135`, `D/settings.mdx:152-156` | Later | 비-git 폴더 세션(A11)으로 대체 |
| B14 | Quick Commands(명령·프롬프트 저장) | `D/terminal.mdx:137-143` | Should | 싸고 반복 작업에 유용 |
| B15 | **한글 IME 조합 입력**(Minikrill 고유 요구, Orca 문서에는 일본어 JIS 키 옵션만 있음 `D/terminal.mdx:126`) | — | **Must** | 사용자가 한국어 사용자. 네이티브 렌더러에서 IME는 흔한 함정(→ T3) |
| B16 | 파일·이미지를 터미널에 드래그해 경로 붙여넣기 | `D/editing/file-explorer.mdx:7-12` | **Must** | Claude Code에 이미지를 넘기는 기본 동선 |
| B17 | **(추가)** 숨긴 터미널 parking(보이지 않는 터미널의 렌더 상태를 해제·직렬화) | `S/renderer/src/components/use-terminal-parking-pass.ts`. Superset은 기본 12개만 메모리에 유지 | **Must(설계 원칙)** | 메모리 목표의 핵심. **앱은 보이는 터미널만 grid를 들고, 나머지는 데몬의 바이트 버퍼에서 다시 그린다**(→ T3) |

#### 3.2.3 에이전트 실행·상태

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| C1 | 에이전트 런처: 표시 이름 47종(`S/shared/tui-agent-display-names.ts`, `claude-agent-teams` 실행 모드 포함), 문서 표 40종(`D/agents/supported.mdx:27-66`), 기본 에이전트, 인자 편집 | `S/shared/tui-agent-config.ts:27,356` | **Must(축소)** | 프리셋은 Claude Code·Codex·OpenCode·Gemini·Cursor 5종 + 사용자 정의 명령 |
| C1b | **(추가)** 에이전트 CLI 설치 감지(preflight) | `S/main/preflight/agent-detection.ts`, `codex-cli-installation.ts` | **Must** | 미설치·PATH 문제를 첫 실행에서 알려 준다. 로그인 셸 환경으로 감지한다 |
| C2 | 권한 기본값: Orca는 YOLO가 기본, 전역 Yolo/Manual 전환 | `D/agents/supported.mdx:17-24`, `D/model/agents-sessions.mdx:61-65` | **Must** | D6: 기본은 Manual |
| C3 | **에이전트 상태 감지**(working / waiting / done / blocked / idle), OSC 타이틀 + hooks | `D/model/agents-sessions.mdx:10-21`, `S/main/agent-hooks/`(106파일 13K줄) | **Must(P0)** | 병렬 운용의 핵심 신호(→ T4) |
| C4 | 훅 엔드포인트가 앱 재시작 뒤에도 유지 | `D/agents/hooks-memory.mdx:25-27` | **Must** | D4: 수신기를 데몬에 둔다 |
| C5 | Restart 칩(같은 cwd·인자로 재실행) | `D/model/agents-sessions.mdx:67-69` | **Must** | 싸고 체감이 크다 |
| C6 | 에이전트 세션 히스토리(디스크 transcript 스캔, Resume) | `D/agents/session-history.mdx`, `S/main/ai-vault/`(162파일 21K줄) | Should | Claude·Codex만, 요청할 때 스캔 |
| C7 | Hibernation(유휴 에이전트 정지 → 열면 자동 resume) | `D/agents/hibernation.mdx:15-40`(조건 8개, 기본 30분, 범위 1분~24시간) | Should(v1.x 상위) | 시스템 RAM은 대부분 에이전트가 쓴다. Claude Code supervisor도 약 1시간 뒤 유휴 프로세스를 정리한다(agent-view.md:822) |
| C8 | Agent Dashboard(칸반: Needs You / Working / Done / Idle) | `D/model/agents-sessions.mdx:23-59` | "Needs You" 목록 **Must** / 칸반 Later | 사이드바 상단의 대기 목록으로 충분 |
| C9 | 서브에이전트·Agent Teams를 자식 행으로 표시 | `D/model/agents-sessions.mdx:57`, `D/agents/supported.mdx:28` | Later | |
| C10 | Claude·Codex 다계정 핫스왑, 키체인 자격 증명 | `D/agents/codex-hot-swap.mdx`, `S/main/claude-accounts/`, `S/main/macos-keychain/` | Later | 자격 증명을 다루는 위험한 표면 |
| C11 | 사용량·레이트리밋(5시간·주간 창, 80% 경고, 로컬 파일만 읽음) | `D/agents/usage-tracking.mdx:7-24`, `S/main/rate-limits/`(81파일 10.9K줄) | Should | Claude·Codex 로컬 파일 경량판 |
| C12 | Stats·추정 비용 | `D/agents/usage-tracking.mdx`, `S/main/stats/` | Later | |
| C13 | 다른 에이전트로 넘기기(handoff) | `D/agents/codex.mdx` | Later | B10으로 대체 가능 |
| C14 | Native Chat UI(PTY 위 transcript + composer) | `D/agents/native-chat.mdx`, `S/main/native-chat/`(402파일 53K줄) | Won't | 터미널이 원본이다. 가장 비싼 기능 |
| C15 | ACP(Agent Client Protocol) | `S/main/acp/`(64파일 10K줄) | Later | Zed 쪽 표준. v2에서 재검토 |
| C16 | Claude Agent Teams 네이티브 pane | `D/agents/supported.mdx:28` | Later | |
| C17 | **Keep computer awake**(On / Agent / Off) | `D/settings.mdx:65`, `S/main/agent-awake-service.ts:1`(Electron `powerSaveBlocker`) | **Must** | 맥이 잠들면 밤샘 병렬 작업이 끊긴다. Rust에서는 `IOPMAssertion`(예: `keepawake` 0.6.1) |
| C18 | 대화 이름 AI 생성, 첫 작업 후 브랜치 자동 이름 변경 | `S/main/agent-hooks/first-work-*.ts`, `S/main/startup/branch-rename-hook.ts` | Later | |
| C19 | 워크스페이스 신뢰 사전 처리(Claude 폴더 신뢰 프롬프트) | `S/main/agent-trust-presets.ts`, `S/shared/tui-agent-config.ts:34`(`preflightTrust`) | Should | 새 워크트리에서 신뢰 프롬프트에 막히지 않게(→ T4) |
| C20 | **(추가)** Claude Code `claude agents --json` 연동 | https://code.claude.com/docs/en/agent-view.md | Should | 1st-party 백그라운드 세션까지 사이드바에 표시. 지원 인터페이스로 문서화돼 있다 |

#### 3.2.4 알림·인박스

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| D1 | 에이전트 완료·입력 대기 알림(시스템·소리·칩) | `D/notifications.mdx:7-9` | **Must** | 여러 에이전트를 걸어 두고 자리를 비우는 사용 패턴 |
| D2 | 읽지 않음 벨 + Dock 배지 | `D/notifications.mdx:11-15`, `S/main/dock/unread-badge.ts:10-11` | **Must** | 구현이 싸다 |
| D3 | 읽지 않음으로 표시 | `D/notifications.mdx:17-19` | Should | |
| D4 | 카테고리별 사용자 지정 사운드 | `D/notifications.mdx:25-29` | Later | |
| D5 | Agents 활동 피드 | `D/activity.mdx` | Later | |
| D6 | 메뉴바(트레이) 아이콘 + 주의 상태 | `S/main/tray/system-tray.ts`, `tray-attention-icon.ts` | Should | 창을 닫아도 상태가 보인다 |
| D7 | PR 체크 실패·업데이트 알림 | `D/settings.mdx:97-102` | Later | |
| D8 | **(Minikrill 신규)** 알림 웹훅(ntfy·Telegram·Discord·Slack) | Orca는 모바일 + push gateway로 해결(`cloud/README.md:27-57`) | Should | 모바일 앱과 클라우드 없이 휴대폰 알림. 운영비 0 |

#### 3.2.5 Diff·리뷰·Git·이슈 연동

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| E1 | Diff 뷰어(통합 diff, 이미지 diff, 3-way 병합 UI) | `D/review/diff-viewer.mdx:7-30` | Should(텍스트 diff만) | 워크트리 대 base 읽기 전용 |
| E2 | Annotate AI Diff(줄 주석을 모아 에이전트에 일괄 전송) | `D/review/annotate-ai-diff.mdx` | Should(v1.x 상위) | 리뷰 루프의 차별 기능 |
| E3 | Attribution(AI가 쓴 줄 표시) | `D/review/attribution.mdx:7-12` | Won't | 도구 호출 추적이 필요하다 |
| E4 | Source Control 패널(stage·commit·push·force-with-lease·conflict) | `D/review/commit-push.mdx` | Later. **변경 파일 수·ahead/behind 칩만 Should** | 1인 개발자는 에이전트나 터미널로 커밋한다 |
| E5 | AI commit 메시지·PR 본문·Fix with AI | `D/review/commit-push.mdx`, `S/main/text-generation/` | Later | |
| E6 | 호스팅 리뷰: GitHub PR·체크·auto-merge, GitLab·Bitbucket·Azure·Gitea | `D/review/github.mdx`, `S/main/{github,gitlab,bitbucket,azure-devops,gitea,source-control}/` | Later. **PR 상태 칩(`gh`)만 Should** | API 레이트리밋 관리 필요(`AGENTS.md` "GitHub CLI Usage" 142행) |
| E7 | 이슈 드로어: GitHub Issues·Projects, Linear, Jira, GitLab | `D/review/{github,linear,jira}.mdx`, renderer `task-page-*` 수백 파일 | Won't(v1) | 표면이 가장 넓은 기능군 |
| E8 | 워크트리 코멘트·상태 체크포인트(에이전트가 CLI로 갱신) | `D/cli/worktree-checkpoints.mdx:7-31` | Should | 싸다 |
| E9 | 깨진 체크 고치기 | `D/review/github.mdx` | Later | |

#### 3.2.6 에디터·뷰어

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| F1 | Monaco 에디터 | `D/editing/monaco.mdx` | Won't | 순수 Rust로 다시 만들 수 없는 규모 |
| F2 | 리치 마크다운 에디터 | `D/editing/markdown.mdx` | Won't | |
| F3 | 파일 탐색기 | `D/editing/file-explorer.mdx` | Later | 경로 붙여넣기(B16)만 Must |
| F4 | 뷰어: HTML·Mermaid·PDF·이미지·CSV·Jupyter | `D/editing/viewers.mdx`, `S/main/notebook/` | Won't | |
| F5 | "Open in" 메뉴(VS Code·Cursor·Zed·Finder, SSH면 Remote-SSH) | `D/settings.mdx:18` | **Must** | 에디터가 없는 대신 반드시 있어야 한다 |

#### 3.2.7 내장 브라우저

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| G1 | 워크트리별 Chromium 브라우저(주소창·devtools·다운로드) | `D/browser/overview.mdx:7,46-48`, `S/main/browser/`(338파일 43.5K줄) | Won't | 초경량 원칙과 정면 충돌 |
| G2 | Design Mode(요소 → HTML·CSS·스크린샷을 에이전트에) | `D/browser/design-mode.mdx` | Won't | 에이전트 쪽 브라우저 MCP로 대체 |
| G3 | 브라우저 프로필·쿠키 가져오기·패스키 | `D/browser/profiles.mdx` | Won't | |
| G4 | 브라우저 자동화 CLI(`orca goto/snapshot/click/fill`) | `D/cli/overview.mdx:92-96` | Won't | Playwright·agent-browser가 대신한다 |
| G5 | 로컬 포트·localhost URL 감지 | `S/main/ports/`(17파일 2.4K줄) | Later(v1.x 후보) | 감지한 URL을 클릭 목록으로 |

#### 3.2.8 CLI·orchestration·자동화·skills

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| H1 | `orca` CLI: `status`, `worktree ps/create/rm/set`, `terminal list/read/send/wait/create/split` | `D/cli/overview.mdx:33,56-59`, `S/cli/`(27K줄), 핸들러 약 60개 | **Must(축소판 `mk`)** | **`mk attach`로 "안 끊김"을 UI 밖으로 확장한다** |
| H2 | `terminal wait --for tui-idle` | `D/cli/overview.mdx:57`, `D/model/agents-sessions.mdx:78-80` | **Must(`mk wait --for idle`)** | 스크립트·에이전트 자동화의 기본. C3 신호를 그대로 노출한다 |
| H3 | Orchestration: Run / Task / Dispatch / worker_done / ask / decision gate | `D/cli/orchestration.mdx:23-199` | Later(M6 상위) | **사용자가 지금 이 기능을 실제로 쓰고 있다**(이 작업 자체가 dispatch다). v1은 그 기반 프리미티브(send/read/wait + 이벤트 스트림)만 제공 |
| H4 | Skills: hybrid stub, 백그라운드 업데이트, 공유 링크 | `D/cli/skills.mdx:31-100`, `S/main/skills/`(103파일 15K줄) | `mk` 사용법 skill 하나만 Should / 나머지 Won't | 공유 링크는 클라우드가 필요하다 |
| H5 | 예약 자동화(cron·RRULE, precheck, 세션 재사용, missed-run grace) | `D/cli/automations.mdx:7-156`, `S/main/automations/` | Later(M6) | 데몬이 상주하므로 자연스럽게 얹을 수 있다 |
| H6 | CLI로 파일·diff 열기 | `D/cli/overview.mdx:64-72` | Later | |
| H7 | MCP 서버 등록 UI | `D/cli/skills.mdx:170-172` | Won't | 에이전트가 각자 관리한다 |
| H8 | `orca serve`(헤드리스 런타임) | `D/remote-servers.mdx:123-160`, `S/main/server/` | Later | Minikrill 데몬은 원래 헤드리스라 v2 원격의 기반이 된다 |

#### 3.2.9 원격·SSH

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| I1 | SSH 워크트리(relay, 재연결 replay, 포트 포워딩, FIDO2) | `D/ssh.mdx`, `S/main/ssh/`(291파일 40.5K줄) | Later(M6 핵심) | 정적 바이너리 데몬 복사 방식이 훨씬 단순하다(D10) |
| I2 | Remote Orca Server 페어링(Tailscale, 토큰 회수) | `D/remote-servers.mdx` | Later | |
| I3 | Cloud VM / 워크스페이스별 환경 레시피 | `D/ways-to-run.mdx:72-98` | Won't | |
| I4 | WSL | `S/main/wsl/` | Won't | |

#### 3.2.10 모바일·클라우드·계정

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| J1 | 모바일 동반 앱(상태·스크롤백·답장·소스컨트롤·계정·push) | `D/mobile.mdx:17-34`, `mobile/`(196K줄) | Later | v1은 웹훅(D8)으로 대체 |
| J2 | Orca Relay(director/cell, fence broker) | `cloud/README.md:1-30` | Won't | 운영비와 보안 책임 |
| J3 | Push gateway(APNs/FCM, PostgreSQL) | `cloud/README.md:27-57` | Won't | D8로 대체 |
| J4 | Artifacts(공개 공유 링크, Orca 계정 필요, 기본 꺼짐) | `D/settings.mdx:80-86`, `S/main/artifacts/` | Won't | |
| J5 | Orca 계정·프로필 | `S/main/orca-profiles/`(48파일 6.3K줄) | Won't | 계정 없는 로컬 앱이 오히려 장점 |

#### 3.2.11 음성·컴퓨터유즈·기타·운영

| # | 기능 | 근거 | 분류 | 이유 |
|---|---|---|---|---|
| K1 | 음성 받아쓰기(sherpa-onnx 1.12.37: Parakeet·Zipformer KO·SenseVoice·Whisper, OpenAI 클라우드) | `D/settings.mdx:104-118`, `package.json:203,341-345`, `S/main/speech/`(24파일 2.9K줄) | Won't | macOS 시스템 받아쓰기로 충분하고, 모델 용량이 크다 |
| K2 | Computer use(접근성 트리·클릭·스크린샷) | `D/cli/computer-use.mdx`, `native/computer-use-macos/` | Won't | 범위 밖 |
| K3 | iOS 시뮬레이터·Android 에뮬레이터 제어 | `D/cli/overview.mdx:107-120`, `S/main/emulator/` | Won't | |
| K4 | 펫 오버레이 | `S/renderer/src/components/pet/` | Won't | |
| K5 | GitHub 별 요청 넛지 | `S/main/star-nag/` | Won't | 상용 품질에 해가 된다 |
| K6 | 플러그인·마켓플레이스(실험) | `D/settings.mdx:158-163`, `S/main/plugins/`(73파일 7.8K줄) | Later | |
| K7 | 다국어 UI(en·zh·ko·ja·es) | `D/settings.mdx:31`, `S/main/i18n/` | Should(ko·en) | 문자열은 처음부터 키로 관리한다 |
| K8 | 크래시 리포트(crashpad·breadcrumb), 진단 번들 업로드, 텔레메트리(익명, opt-out) | `S/main/crash-reporting/`(34파일 5K줄), `S/main/observability/`, `D/telemetry.mdx:10-14` | 로컬 크래시 로그 + 수동 진단 번들 **Must** / 텔레메트리 Won't(v1, D13) | 상용 품질에는 크래시 원인 파악이 필수 |
| K9 | 자동 업데이트(RC·perf 채널, 로컬 빌드 선택) | `D/settings.mdx:10-16`, `S/main/updater/`, `S/main/local-builds/` | **Must(단일 채널)** | **업데이트 중에도 세션이 유지되고, 구세대 데몬이 남지 않아야 한다**(#9138) |
| K10 | 키 바인딩: 91개 액션(25+28+31+7), 전부 재지정 가능, `~/.orca/keybindings.json` | `S/shared/keybindings/definitions-core-{1..4}.ts`, `S/main/keybindings/keybinding-file.ts:28` | **Must(기본값 + 설정 파일)** | 재지정 UI는 Later |
| K11 | 검색 가능한 설정 화면(Cmd-,) | `D/settings.mdx:5` | **Must(경량)** | TOML 파일 + 최소 설정 UI |
| K12 | 메인 스레드 hang watchdog | `S/main/hang-watchdog/` | Should | Rust에서는 watchdog 스레드 하나 |
| K13 | 온보딩·기능 투어·feature wall·setup guide | renderer `onboarding/`, `contextual-tours/`, `feature-wall/`, `setup-guide/` | Later(첫 실행 화면 하나만 Must) | |
| K14 | 외관: 라이트·다크, UI 줌, 앱 아이콘 변형 | `D/settings.mdx:30` | 테마·줌 **Must** / 아이콘 Won't | |
| K15 | 네트워크 프록시 | `S/main/network/` | Later | |
| K16 | `orca status` / 진단 | `D/cli/overview.mdx:33`, `S/main/diagnostics/` | **Must(`mk doctor`)** | 지원 비용을 줄인다 |
| K17 | 로컬 영속화(SQLite, 원자적 스냅샷) | `S/main/persistence/`(254파일 32K줄), `S/main/sqlite/` | **Must** | 형식은 T2에서 결정 |
| K18 | **(추가)** 접근성(VoiceOver) | Orca는 Chromium a11y를 그대로 얻는다. 네이티브 Rust UI는 따로 구현해야 한다(`accesskit` 0.25.1, `accesskit_macos` 0.27.1) | Should(사이드바·버튼 최소) | 상용 품질 기본기. 터미널 본문 a11y는 Later(→ T3) |

#### 3.2.12 집계

| 분류 | 항목 수(혼합 행은 중복 집계) | 대표 기능 |
|---|---:|---|
| Must | 약 44 | 데몬 세션 유지, 놓친 출력 재생, cold restore + resume, 워크트리, 런처 + preflight, 상태·알림·awake, Jump Palette, `mk` CLI·attach·wait, 업데이트, 크래시 로그·doctor, 설정·키 바인딩, parking 원칙 |
| Should | 약 27 | 공유 경로·setup 호환, 세션 히스토리, hibernation, 사용량, 텍스트 diff + 주석, 웹훅, 메뉴바, `claude agents` 연동, ko/en, 최소 a11y |
| Later | 약 33 | orchestration, SSH 원격, 자동화, PR 연동, 모바일, 플러그인, sparse |
| Won't | 약 30 | 브라우저, 에디터, 뷰어, 이슈 트래커, 클라우드·계정·artifacts, 음성, computer-use, 펫, 텔레메트리(v1) |

### 3.3 경쟁·유사 제품 (2026-10-14 확인)

| 제품 | 스택 | 메모리 공개 | 라이선스·가격 | 앱 종료·크래시 후 에이전트 유지 | 상태 |
|---|---|---|---|---|---|
| **Orca** | Electron 43 + Node 데몬 | 없음. **실측 약 1.67GB**(PTY 19개, §3.1.1) | MIT, 무료 | **된다**(데몬). 재부팅하면 소실, 레이아웃·스크롤백은 복원 | v1.4.224(10-10), 92.1k |
| **Superset** | Electron 41 + xterm.js 6 + `node-pty` + Node `pty-daemon`(세션별 headless xterm) | **있음**: 16터미널 Renderer RSS 1,127→838MB | ELv2(소스 공개, OSI 아님). Free / Pro $20(월결제)·$15(연결제) / Enterprise | **된다**(데몬, 업그레이드 시 fd 넘겨주기). 데몬이 죽거나 재부팅하면 읽기 전용 이전 출력 + 새 셸 | desktop-v1.37.0(10-08), 15.2k |
| **cmux** | Swift/AppKit + libghostty, macOS 전용 | "low memory" 주장만 | GPL-3.0+(서버 일부 BUSL-1.1). Free / Pro $40~50 / Max $160~200 / Team $60(주로 Cloud VM) + 후원형 Founder's Edition | **기본은 안 된다**("does not checkpoint arbitrary live process state"). `cmux local-tmux`·`local-zellij`를 켜야 된다. 에이전트는 hook으로 저장한 세션 ID로 resume | v0.65.1(10-12), 28.2k |
| **Conductor** | Tauri(Rust) + Claude Agent SDK·Codex SDK 래핑, arm64 전용 | 없음 | 비공개. Free / Pro $50 / Teams $60 / Enterprise | 로컬은 확인불가. Cloud 워크스페이스는 "앱을 닫아도 계속" | 0.90.0(10-02, iOS), brew 0.90.2 |
| **Claude Code(CLI agent view)** | 1st-party, supervisor 데몬 | — | 구독에 포함 | **된다**(백그라운드 세션, 셸을 닫아도 유지, 자동 업데이트 넘어 유지). 재부팅하면 정지 후 대화 재개. 유휴 세션은 약 1시간 뒤 정리. Claude만 해당 | research preview |
| **Claude 데스크톱 / Codex 앱** | 1st-party | — | 구독에 포함 | Claude: Cloud·SSH 세션은 유지, 로컬은 문서 언급 없음. Codex: 로컬은 문서 언급 없음 | 둘 다 자체 worktree 병렬 지원 |
| **Jean** | Tauri v2 + Rust + React + xterm.js | 없음 | Apache-2.0 | **된다**(Claude CLI를 분리 프로세스로 실행하고 JSONL을 tail). PTY 방식이 아니다 | v1.0.10(10-13), 1.3k |
| **Emdash** | Electron 40, node-pty, xterm.js 6, SQLite | 없음 | Apache-2.0 | tmux 옵션(`tmuxByDefault`)일 때 된다 | v1.2.8(10-10), 6.0k |
| **Claude Squad** | Go + bubbletea TUI + tmux + worktree | 없음 | AGPL-3.0 | **된다**(tmux, 종료 시 세션을 죽이지 않음) | v1.0.20(08-20) 이후 정체, 8.6k |
| **Agent Deck** | Go TUI + tmux | MCP 소켓 풀링으로 MCP 메모리 85~90% 절감(앱 전체 수치는 아님) | MIT | **된다**(tmux) | v1.16.28(10-10), 1.08k |
| **Nimbalyst**(구 Crystal) | Electron, iOS 보조 앱 | 없음 | MIT. 개인 무료, Teams $20 | 미확인 | v0.80.6(10-09), 1.9k. Crystal은 2026-02 중단 |
| **Vibe Kanban** | Rust 백엔드 + 웹 UI(`npx`) | 없음 | Apache-2.0 | 미확인 | 2026-04-10 회사 종료, 커뮤니티 유지. 마지막 릴리스 v0.1.44(04-24), 28.4k |
| **Zed 에이전트 패널** | Rust/GPUI, 외부 에이전트는 ACP 별도 프로세스 | 없음 | GPL·Apache. $0 / Pro $10 / Business 좌석당 $30 | 문서에 프로세스 유지 언급 없음. "Import Threads"로 이어 쓰기 | v1.23.3(10-12), 91.7k |
| **Warp** | Rust(클라이언트 AGPL, `warpui` MIT, 서버 비공개) | 없음 | Free / Build $20 / Max $200 / Business $50 | 창·탭·pane·최근 블록만 복원(SQLite) | 65.5k |
| **tmux / zellij** | C / Rust | 블로그 측정 tmux 약 3.2MB, zellij 약 14.7MB(단일 블로그, 신뢰도 낮음) | ISC / MIT | tmux: **된다**(서버가 살아 있는 한). zellij 복원은 명령을 다시 실행("Press ENTER to run") | tmux 3.8(09-09) 49.9k, zellij v0.45.1(08-28) 35.7k |
| Sculptor / opcode / Uzi / Terragon | Electron+Python / Tauri / Go / TS | — | MIT / AGPL / MIT / Apache(스냅샷) | — | Sculptor 활발(v0.48.0). opcode는 2025-10 이후 사실상 정지, Uzi는 2025-06 이후 정지. Terragon은 **2026-01-16 종료** |

출처 URL:

| 제품 | URL |
|---|---|
| Orca | https://github.com/stablyai/orca , `D/model/session-restore.mdx` |
| Superset | https://superset.sh/pricing , https://github.com/superset-sh/superset (`apps/marketing/content/blog/terminal-daemon-deep-dive.mdx`, `apps/marketing/content/changelog/2026-07-19-performance-memory-and-load.mdx`, `packages/pty-daemon`) |
| cmux | https://cmux.com/pricing , https://github.com/manaflow-ai/cmux , https://cmux.com/docs/session-restore |
| Conductor | https://www.conductor.build/pricing , https://www.conductor.build/changelog , https://threadreaderapp.com/thread/1945870105109246401.html , https://formulae.brew.sh/cask/conductor |
| Claude Code | https://code.claude.com/docs/en/agent-view.md , https://code.claude.com/docs/en/desktop.md , https://code.claude.com/docs/en/worktrees.md |
| Codex 앱 | https://learn.chatgpt.com/codex/environments/git-worktrees , https://learn.chatgpt.com/codex/long-running-work |
| Jean | https://github.com/coollabsio/jean |
| Emdash | https://github.com/generalaction/emdash |
| Claude Squad | https://github.com/smtg-ai/claude-squad |
| Agent Deck | https://github.com/asheshgoplani/agent-deck |
| Nimbalyst / Crystal | https://github.com/Nimbalyst/nimbalyst , https://nimbalyst.com/pricing , https://github.com/stravu/crystal |
| Vibe Kanban | https://www.vibekanban.com/blog/shutdown , https://github.com/BloopAI/vibe-kanban |
| Zed | https://zed.dev/docs/ai/external-agents , https://zed.dev/pricing |
| Warp | https://github.com/warpdotdev/warp , https://www.warp.dev/pricing , https://docs.warp.dev/terminal/sessions/session-restoration |
| tmux / zellij | https://www.commandinline.com/tmux-vs-zellij-comparison/ , https://zellij.dev/documentation/session-resurrection.html , https://github.com/zellij-org/zellij/issues/1625 |
| 기타 | https://github.com/imbue-ai/sculptor , https://github.com/winfunc/opcode , https://github.com/devflowinc/uzi , https://github.com/terragon-labs/terragon-oss |

#### 3.3.1 해석: 시장 지도 (1차 보고서에서 수정)

- **축 1, 세션 지속성의 "층"**:
  - (a) **PTY 수준, 에이전트 무관**: Orca·Superset 데몬, tmux 계열(Claude Squad·Agent Deck·Emdash 옵션·cmux 옵션)
  - (b) **에이전트 수준, 벤더 한정**: Claude Code supervisor(Claude만), Jean(Claude CLI 분리 실행·JSONL), Conductor Cloud
  - (c) **레이아웃만 복원**: cmux 기본값, Warp, zellij
- **축 2, 무게**: TUI·네이티브(tmux 계열, cmux, Zed, Warp) < Tauri(Conductor·Jean) < Electron(Orca·Superset·Emdash·Nimbalyst)
- **빈자리**:
  - (a) 층을 GUI와 함께 기본값으로 제공하는 **네이티브** 제품은 확인 범위 안에 없다. cmux는 네이티브지만 opt-in tmux에 의존한다.
  - "놓친 출력 바이트 재생"을 정량으로 보장한다고 밝힌 제품도 없다. Superset은 디스크 cold restore와 읽기 전용 출력이고, tmux는 히스토리 한도 안에서만 보장한다.
- 1차 보고서의 "빈자리 = 네이티브 + 기본 데몬 + 놓친 출력 재생" 판단은 **유지**한다.
  - 다만 Claude Code만 쓰는 사용자에게는 1st-party agent view가 "끊김" 불만 상당 부분을 이미 해결한다.
  - 그래서 Minikrill의 1차 타깃은 **"여러 벤더를 섞어 쓰거나, 에이전트 외 장기 프로세스(dev 서버·테스트 watch)까지 함께 돌리는 사람"** 으로 좁힌다.
- Superset 블로그가 밝힌 **tmux를 뺀 이유**(추가 의존성, Windows, xterm과의 조작 충돌, 블로그 48-52행)는 Minikrill이 자체 데몬을 만드는 근거와 같다(→ T2).
- **Superset의 "숨긴 터미널은 최근 N개만 메모리에" 전략(기본 12개)은 Electron에서도 Renderer를 26% 줄였다.** Minikrill은 이를 처음부터 설계 원칙(B17)으로 둔다.

### 3.4 포지셔닝·타깃·가격/라이선스

#### 3.4.1 포지셔닝 문장(수정안)

> **"어떤 에이전트든 끊기지 않는, 메모리 1/10의 네이티브 에이전트 터미널."**
> tmux의 PTY 지속성 + Orca/Conductor의 워크트리·상태 UX를 순수 Rust 단일 앱으로. 앱을 꺼도, 죽어도, 업데이트해도 Claude Code·Codex·Gemini·dev 서버는 계속 돌고, 다시 열면 놓친 출력이 바이트 단위로 그대로다.

- 증명 수치 세 가지(README 첫 화면):
  1. 앱+데몬 ≤ 150MB(터미널 10개). Orca 실측은 약 1.67GB다.
  2. 앱 SIGKILL 1,000회에서 세션 손실 0.
  3. 업데이트 뒤 고아 데몬 0.
- 피해야 할 포지션:
  - "Orca 대체 IDE": 기능 폭으로 경쟁하면 진다.
  - "Claude Code 세션 매니저": 1st-party와 정면으로 겹친다(D12).

#### 3.4.2 타깃

| 구분 | 내용 |
|---|---|
| 1차 | macOS(Apple Silicon)에서 **여러 벤더 에이전트**(Claude Code·Codex·Gemini 등)를 동시에 5~20개 돌리는 1인 개발자·파워유저. 지금 tmux를 쓰거나, Orca·Superset을 쓰다 메모리·끊김 때문에 불만인 사람 |
| 2차 | 맥미니·홈서버에 에이전트를 상시로 돌리고 노트북에서 붙는 사람(M6 원격 attach) |
| 비타깃 | 노코드 사용자, 내장 브라우저·에디터가 필요한 IDE형 사용자, Windows 사용자(v1), Claude Code만 쓰고 agent view에 만족하는 사람 |

#### 3.4.3 가격·라이선스

| 안 | 내용 | 장점 | 단점 |
|---|---|---|---|
| **A(권고)**: 코어 OSS, 무료 | `MIT OR Apache-2.0`(Rust 생태계 관례) + GitHub Sponsors. 원하면 cmux식 후원형 "Supporter 에디션"(아이콘·이름만) | 경쟁 OSS(Orca·Claude Squad·Agent Deck·Emdash·Jean)와 같은 조건이라 채택이 빠르다. 운영비 0. Orca(MIT) 차용 시 고지 의무와 충돌 없음 | 직접 수익이 작다 |
| B: Open-core | 코어 OSS + Pro(원격 attach·다중 머신·모바일 웹, 월 $5~10 수준, 추정) | 수익 경로가 분명하다 | v1에는 팔 Pro 기능이 없다. 경계 유지보수 부담 |
| C: Source-available | FSL-1.1 / ELv2(Superset 방식) | 무임 상용 재배포를 막는다 | "오픈소스"라고 내세울 수 없고 기여가 줄어든다 |
| D: 비공개 유료 | 일회성 $29~49 등 | 단순하다 | 무료 대안이 강하다. 데몬이 PTY를 소유하는 제품이라 신뢰를 얻기 어렵다 |

- 근거: 유료 경쟁자(Conductor $50, Superset $15~20, cmux 유료 플랜)는 모두 **클라우드·팀·모바일**로 돈을 받는다. v1 Minikrill에는 그런 기능이 없다.
- 재검토 시점: 출시 3개월 뒤 주간 활성 사용자 1,000명 또는 GitHub 별 5k(둘 다 **추정 기준값**). 이때 M6 원격·다중 머신을 B안으로 낼지 정한다.
- 저작권 표기:
  - Orca 코드를 일부라도 옮기면 `THIRD_PARTY_NOTICES.md`에 "Copyright (c) 2026 Lovecast Inc." MIT 고지 전문을 넣는다(`LICENSE:3`).
  - 기능만 참고한 경우 법적 표기 의무는 없다. README에 영감 출처를 밝히기를 권고한다.
  - Superset(ELv2)·cmux(GPL/BUSL)·Claude Squad(AGPL) 코드는 **차용 금지**다. 라이선스가 Minikrill과 맞지 않는다.

### 3.5 성공 지표 (수치 목표 + 측정 방법)

| 지표 | v1.0 목표 | 실패 기준 | 측정 방법 | Orca 기준선 |
|---|---|---|---|---|
| 앱+데몬 메모리, 터미널 10개(각 스크롤백 10k줄, 4개는 출력 진행 중) | **≤ 150MB** | 200MB(브리프 상한) | `footprint` phys_footprint 합(앱+데몬, 셸·에이전트 제외), 1시간 부하 뒤 | 주요 3개 footprint 1,335MB / RSS 약 1,674MB(PTY 19개) |
| 데몬 메모리 | 유휴 ≤ 15MB, 세션당 ≤ 3MB + 링 버퍼 **(추정 예산)** | 50MB @10세션 | 위와 같음 | 171MB @19 PTY |
| 24시간 메모리 증가(10세션, 연속 출력) | ≤ 5MB | 20MB | 1시간 간격 샘플링 | #12728·#15241 |
| 콜드 스타트 → 첫 프레임 | ≤ 300ms **(추정 목표)** | 800ms | 프로세스 생성부터 첫 present까지 os_signpost | 미측정 |
| 앱 재시작 → 세션 10개 재부착 + 마지막 화면 | ≤ 500ms | 1.5s | 위와 같음 | 미측정 |
| **앱 크래시·SIGKILL·업데이트 시 세션 생존율** | **100%**(1,000회 자동 반복에서 손실 0) | 99.9% | CI 장애 주입: 앱 SIGKILL → PTY pid 생존 + 출력 연속성 확인 | 문서상 지원(`session-restore.mdx:24-32`) |
| **놓친 출력 재생 정확도** | 링 버퍼(세션당 기본 4MB, 추정) 안에서는 **바이트 단위 100%**. 넘치면 "N바이트 생략" 표시 | 표시 없는 손실 0 | 앱을 끈 동안 `seq 1 1000000` 출력 → 재시작 뒤 해시 비교 | — |
| 업데이트 뒤 고아 데몬 | **0** | 0 | 업데이트 E2E 뒤 `pgrep` | **이 맥에서 2개 실측**(v40·v41) |
| 앱↔데몬 연결 끊김 후 자동 복구 | 5초 안, 사용자 조작 없이. 끊긴 동안은 배너 표시 | 30초 | 데몬 소켓 강제 종료·재시작 테스트 | #20707(복구 안 됨) |
| 데몬 크래시율 | < 1회 / 1,000 세션·일 | 1회 / 100 | 로컬 크래시 로그(사용자가 제출) | 미측정 |
| 키 입력 → 에코 지연(로컬) | p99 < 16ms **(목표)** | 33ms | typometer 계열 | 미측정 |
| 출력 폭주 격리 | 한 세션에서 `cat` 1GB를 해도 다른 세션 입력 지연 p99 < 33ms | 100ms | 부하 테스트 | #22804 |
| 번들 크기 | ≤ 30MB **(추정 목표)** | 60MB | `du -sh` | 642MB |
| 재부착 뒤 상태 표시 일관성 | 1초 안에 모든 세션 상태 정확 | 3초 | hook 이벤트 replay 테스트 | — |

### 3.6 1차 보고서가 빠뜨린 관점

- **보안**
  - 데몬 소켓은 사용자 전용 디렉터리에 0600으로 두고, 연결마다 토큰으로 인증한다. Orca DaemonSpawner도 토큰 인증과 PID 파일 + launch nonce를 쓴다(→ T2).
  - hook 수신기도 같은 토큰을 요구해야 한다. 같은 사용자의 다른 프로세스가 상태를 위조할 수 있기 때문이다.
  - OSC 52는 **쓰기만 허용하고 읽기(클립보드 질의)는 막는다.** 원격 출력이 클립보드를 읽어 가는 것을 방지한다.
  - 놓친 출력 버퍼에는 비밀(토큰·키)이 섞일 수 있다. 디스크에 쓰면 0600 권한, 세션을 닫으면 삭제, "디스크 저장 끄기" 옵션을 둔다.
  - 웹훅에는 제목·상태만 보내고 출력 본문은 보내지 않는다(opt-in).
  - YOLO는 기본 꺼짐이다(D6).
- **개인정보**: v1은 텔레메트리 없음(D13). 크래시 로그는 로컬에만 두고, 사용자가 직접 진단 번들을 만들어 제출한다.
- **운영(1인 개발)**
  - 서명·공증과 업데이트 서버(GitHub Releases + appcast)만 운영한다. 클라우드는 운영하지 않는다.
  - 데몬 세대 교체·퇴역 규칙을 릴리스 체크리스트의 P0 테스트로 둔다(#9138).
  - 업데이트용 크레이트 선택은 T4 몫이다. 참고로 `self_update` 1.3.0(MIT, 2026-09-02 갱신)이 있다.
- **성능·메모리**: 보이는 터미널만 grid를 유지하고(B17), 데몬은 원시 바이트 링 버퍼만 보관한다(→ T2·T3). 출력 폭주 세션의 렌더링은 프레임 단위로 합친다.
- **UX(상용 품질)**
  - 앱↔데몬 연결 상태를 항상 보여 준다. 재연결은 포기하지 않는다(Orca #5049 교훈).
  - 첫 실행 preflight(C1b)와 `mk doctor`로 문의를 줄인다.
  - 한글 IME와 kitty 키보드는 출시 차단 조건이다.
  - 최소 VoiceOver(K18).
- **1st-party 의존 위험**: `claude agents --json`은 문서상 지원 인터페이스지만 research preview다. `~/.claude/jobs/` 파일은 "stable interface가 아님"이라고 명시돼 있다(agent-view.md). 그래서 **파일을 직접 읽지 않고 CLI JSON만 쓴다.**
- **v1 Must 기능 관련 크레이트(crates.io 조회, 2026-10-14; 최종 선택은 T2~T4)**

| 크레이트 | 최신 | 갱신일 | 라이선스 | 비고 |
|---|---|---|---|---|
| `portable-pty` | 0.9.0 | 2025-02-11 | MIT | wezterm 산하. 갱신이 1년 반 넘게 없다(→ T2 검토) |
| `alacritty_terminal` | 0.26.0 | 2026-04-06 | Apache-2.0 | |
| `libghostty-vt` | 0.2.2 | 2026-09-28 | MIT OR Apache-2.0 | 0.x, 서드파티 바인딩 |
| `git2` / `gix` | 0.21.0 / 0.89.0 | 2026-10-06 / 10-08 | MIT OR Apache-2.0 | 워크트리 생성은 `git` CLI 권고(A2) |
| `keepawake` | 0.6.1 | 2026-08-20 | MIT | C17 |
| `tray-icon` | 0.26.1 | 2026-10-07 | MIT OR Apache-2.0 | D6 |
| `notify-rust` / `mac-notification-sys` | 4.18.2 / 0.6.15 | 2026-10-07 / 06-16 | MIT/Apache-2.0 | macOS 알림 API 선택은 T4 |
| `objc2-user-notifications` | 0.3.2 | 2025-10-04 | Zlib OR Apache-2.0 OR MIT | UNUserNotificationCenter 직접 호출 대안 |
| `rusqlite` | 0.40.2 | 2026-08-08 | MIT | K17 후보 |
| `toml` | 1.1.8 | 2026-10-09 | MIT OR Apache-2.0 | K11 |
| `accesskit` / `accesskit_macos` | 0.25.1 / 0.27.1 | 2026-09-25 | MIT OR Apache-2.0 | K18 |
| `iced` / `gpui` / `winit` | 0.14.0 / 0.2.2 / 0.30.13 | 2025-12-07 / 2025-10-22 / 2026-09-04 | MIT / Apache-2.0 / Apache-2.0 | UI 프레임워크 선택은 T3 |

---

## 4. 위험과 대응

| 위험 | 영향 | 가능성 | 대응 |
|---|---|---|---|
| **1st-party가 지속성을 흡수**(Claude Code supervisor가 이미 있음, Codex도 따라올 수 있음) | 존재 이유가 약해진다 | 높음(이미 진행 중) | 벤더 무관 PTY 지속, dev 서버까지 포함, 바이트 재생, 네이티브 메모리로 차별화. `claude agents --json` 연동으로 공존(D12). 분기마다 1st-party 변경을 재점검 |
| cmux가 프로세스 유지를 기본값으로 전환 | 네이티브 경쟁자와 정면 충돌 | 중간 | 외부 의존 없는 자체 데몬 + 메모리·생존율 수치 공개로 차별화. cmux 릴리스 노트 감시 |
| Superset(Electron)이 메모리 최적화를 계속함(Renderer −26% 사례) | "1/10" 메시지가 약해진다 | 중간 | 경쟁 앱과 같은 조건(터미널 10개)으로 측정한 공개 벤치를 README에 둔다 |
| 범위 확장 유혹 | 1인 번아웃, 품질 저하 | 높음 | §3.2 분류를 이슈 라벨로 고정. Won't를 바꾸려면 결정 기록을 남긴다 |
| 데몬 소켓·수명 관리의 경쟁 조건 | "입력은 받는데 실행 안 됨" 같은 최악의 버그(`S/main/daemon/AGENTS.md:14-17`) | 중간 | T2에서 "자기가 만들지 않은 이름은 지우지 않는다", link-then-rename 원칙 채택 |
| 업데이트 시 구세대 데몬 잔존 | 메모리 누적, 보이지 않는 세션(#9138, 이 맥에서 실측) | 중간 | 세대 교체 프로토콜 + "빈 구세대 데몬은 즉시 퇴역" + E2E 테스트(이슈 #4) |
| 네이티브 렌더러의 IME·kitty 키보드·폰트·a11y | 한국어 입력이 안 되면 즉시 이탈 | 중간 | T3에서 v1 Must 테스트 매트릭스로 지정. 출시 차단 조건 |
| 메모리 목표 미달(GPU·폰트 아틀라스 기본 비용) | 핵심 마케팅 수치가 무너진다 | 중간 | M1 스파이크에서 렌더러 후보별로 실측. parking 원칙(B17) |
| `portable-pty` 정체(2025-02 이후 갱신 없음) | macOS 변화에 대응 지연 | 낮음~중간 | T2에서 직접 `openpty`/`posix_spawn` 구현과 비교 |
| 라이선스 오염(Orca·Superset·cmux 코드 무심코 복사) | 고지 누락, ELv2·GPL 위반 | 낮음 | 기능만 참고한다는 원칙 + PR 템플릿 확인란 + `THIRD_PARTY_NOTICES.md` |
| 이 범주의 높은 종료율 | 사용자 신뢰 하락 | 중간 | OSS 공개로 "버려져도 쓸 수 있다"를 보장. 범위를 작게 유지 |
| 놓친 출력 버퍼 속 비밀 유출 | 보안 사고 | 낮음 | 0600, 종료 시 삭제, 디스크 저장 끄기 옵션(§3.6) |

---

## 5. 검증 스파이크

| # | 목적 | 방법 | 합격 기준 |
|---|---|---|---|
| S1 | 메모리 예산 확인 | 렌더러 후보(T3)별로 터미널 10개(스크롤백 10k줄, 4개 출력 중)를 띄우고 `footprint` 합을 1시간 측정 | 앱+데몬 ≤ 150MB인 구성 1개 이상 |
| S2 | 앱 SIGKILL 생존 | 최소 데몬 + 클라이언트로 `kill -9` 1,000회 루프 | PTY pid 생존 100%, 출력 해시 일치 |
| S3 | 놓친 출력 바이트 재생 | 앱을 끈 동안 `seq 1 1000000`·컬러 TUI(Claude Code) 출력 → 재부착 | 링 버퍼 안에서는 해시 일치. 넘치면 생략 배너 표시. TUI 화면이 깨지지 않음 |
| S4 | 데몬 세대 교체 | v(n) 데몬 + 세션 5개 → v(n+1) 앱 설치 → 재시작 | 세션 유지 + 업데이트 뒤 `pgrep` 데몬 1개(빈 구세대 0) |
| S5 | 한글 IME + kitty 키보드 | Claude Code·Codex 안에서 한글 조합, Shift+Enter, Ctrl+Enter | Ghostty와 동작이 같다(수동 체크리스트 20항목) |
| S6 | 상태 감지가 앱 재시작을 넘어 유지되는지 | 데몬 hook 수신기 + Claude/Codex hooks. working 중에 앱 재시작 | 재부착 1초 안에 상태가 정확하다 |
| S7 | `claude agents --json` 연동 가능성 | Claude Code agent view 세션 3개 실행 → JSON 폴링 | state·pid·waitingFor를 사이드바에 반영. 폴링 CPU < 1% |
| S8 | 1st-party와의 실제 차이 측정 | 같은 작업(Claude 3 + Codex 2 + dev 서버 1)을 Orca·Superset·cmux(local-tmux)·Minikrill 프로토타입에서 실행 | 메모리·재부착 시간·놓친 출력 보존 비교표 작성(README 벤치의 원천) |

---

## 6. 미확인 항목

| 항목 | 확인하지 못한 이유 |
|---|---|
| Conductor·Nimbalyst·Vibe Kanban·Zed·Warp가 앱 종료 뒤 **로컬 에이전트 프로세스**를 유지하는지 | 공식 문서에 언급이 없고, 이 맥에 설치돼 있지 않다 |
| Claude 데스크톱 **로컬** 세션과 Codex 앱 로컬 세션의 앱 종료 후 동작 | 문서에 언급이 없다. Cloud만 "앱을 닫아도 계속"이라고 적혀 있다 |
| Conductor 번들 검사 결과(Tauri, arm64, SDK 문자열) | 조사 보조 에이전트가 검사한 결과다. 이 워커는 원문 스레드와 brew cask만 대조했고 바이너리를 재검사하지 않았다 |
| 경쟁 제품의 실측 메모리 | 이 맥에 경쟁 앱이 없다. Superset만 자체 공개 수치(Renderer)가 있다. tmux·zellij는 단일 블로그 인용이다 |
| cmux Founder's Edition 가격 | 가격 페이지에 나오지 않는다 |
| Minikrill 메모리 예산(데몬 세션당 3MB, 링 버퍼 4MB, 콜드 스타트 300ms, 번들 30MB) | 전부 **추정**이다. S1·S2로 측정해야 한다 |
| Orca 구세대 데몬(v40·v41)이 빈 상태로 남는 정확한 원인 | `daemon-protocol-cleanup.ts`에 정리 경로가 있지만 왜 실행되지 않았는지는 추적하지 않았다(T2 범위) |
| `gix`의 `git worktree add` 지원 여부 | 문서를 확인하지 않았다. 권고는 `git` CLI 호출이라 결정에는 영향이 없다 |
| 문서에 없고 소스에만 있는 소형 기능 전부 | `S/main` 하위 약 120개 디렉터리를 이름과 일부 파일로 대조했다. `S/main/runtime`(1,478파일)은 내부 구조를 전수로 읽지 않았다 |
| 이 맥의 `claude` 19개 중 Orca 아래에서 도는 수 | 프로세스 트리를 구분하지 않았다 |

---

## 7. Minikrill 이슈 후보

마일스톤 정의:

| 마일스톤 | 내용 |
|---|---|
| **M1** | 스파이크(기술 검증, §5) |
| **M2** | 코어: 데몬 + 단일 창 + 세션 생존·재생 |
| **M3** | 병렬: 워크트리·런처·상태·알림·CLI |
| **M4** | 출시 준비(상용 품질): 업데이트·서명·크래시·설정·라이선스 |
| **M5** | v1.x(출시 후 1~3개월) |
| **M6** | v2: 원격·자동화·orchestration |

| # | 제목 | 마일스톤 | 우선순위 | 완료 조건(관찰 가능) | 선행 이슈 |
|---|---|---|---|---|---|
| 1 | 메모리 예산 스파이크: 터미널 10개 ≤150MB | M1 | P0 | 렌더러 후보별 `footprint` 합계 보고서. 목표 달성 구성 1개 이상(S1) | — |
| 2 | SIGKILL 생존·바이트 재생 프로토타입 | M1 | P0 | S2·S3 합격 기준 충족 보고서 | — |
| 3 | 한글 IME + kitty 키보드 검증 | M1 | P0 | S5 체크리스트 20항목 통과(→ T3) | 1 |
| 4 | 데몬 세대 교체·빈 구세대 데몬 퇴역 설계 검증 | M1 | P0 | S4 통과. 업데이트 뒤 `pgrep` 데몬 1개 | 2 |
| 5 | 앱 크래시·종료에도 PTY를 유지하는 데몬 + 재부착 | M2 | P0 | 앱 SIGKILL 1,000회 자동 테스트에서 세션 손실 0(→ T2) | 2 |
| 6 | 앱이 닫힌 동안의 출력 재생(링 버퍼 + 생략 표시) | M2 | P0 | `seq 1 1000000` 해시 일치. 넘치면 "N바이트 생략" 배너 | 5 |
| 7 | 앱↔데몬 자동 재연결 + 연결 상태 표시 | M2 | P0 | 데몬 소켓을 강제로 끊어도 5초 안에 조작 없이 복구. 끊긴 동안 배너 | 5 |
| 8 | 데몬 소켓 0600 + 토큰 인증(앱·CLI·hook 공통) | M2 | P0 | 다른 사용자·토큰 없는 연결이 거부되는 테스트 통과 | 5 |
| 9 | 탭·분할·레이아웃 저장과 cold restore(재부팅·로그아웃 뒤) | M2 | P0 | 재부팅 뒤 레이아웃과 마지막 스크롤백 복원. 프로세스가 사라진 탭에 "다시 시작" 버튼 | 5 |
| 10 | 숨긴 터미널 parking(보이는 터미널만 grid 유지) | M2 | P0 | 터미널 20개 중 보이는 4개만 grid 메모리 보유를 계측으로 확인. 전환 시 데몬 버퍼에서 100ms 안에 재구성 | 1, 6 |
| 11 | 스크롤백 검색·OSC 52(쓰기만)·링크 Cmd-클릭·파일 드래그 경로 붙여넣기 | M2 | P1 | 기능별 수동 QA 체크리스트 통과. OSC 52 읽기 요청은 무시됨 | 5 |
| 12 | 셸 설정(로그인 셸·사용자 지정 셸) + 환경 보정 | M2 | P1 | nvm/mise PATH가 Terminal.app과 같다 | 5 |
| 13 | 데몬 상주 hook 수신기 + 에이전트 상태(working/waiting/done/blocked) | M3 | P0 | Claude Code·Codex 상태가 1초 안에 사이드바에 반영. 앱 재시작 뒤에도 유지(S6, → T4) | 5, 8 |
| 14 | 워크트리 생성·삭제(base·브랜치·커밋·자동 이름, 백그라운드 진행·취소) | M3 | P0 | UI·CLI로 만든 워크트리에서 에이전트가 실행됨. 삭제 시 디렉터리·브랜치 제거. 병합 안 된 브랜치는 경고 후 남김 | 5 |
| 15 | 에이전트 런처(5종 + 사용자 정의, 인자 편집, 권한 기본 Manual) + 설치 감지 preflight | M3 | P0 | 프리셋이 지정 cwd에서 실행됨. 미설치 CLI는 첫 실행에서 안내. YOLO는 토글을 켰을 때만 적용 | 12, 14 |
| 16 | 완료·입력 대기 알림 + Dock 배지 + 잠자기 방지(On/Agent/Off) | M3 | P0 | working→done 전환 시 macOS 알림. Dock 배지 수 일치. 에이전트가 일하는 동안 `pmset -g assertions`에 assertion 존재 | 13 |
| 17 | `mk` CLI: ls/new/send/read/wait/attach/doctor | M3 | P0 | 일반 터미널에서 `mk attach <id>`로 입출력 가능. `mk send` + `mk wait --for idle`이 스크립트에서 동작 | 5, 13 |
| 18 | 재부팅 뒤 에이전트 resume(`claude --resume`, `codex resume`) + Restart 칩 | M3 | P1 | hook으로 저장한 세션 ID로 클릭 한 번 resume 성공(Claude·Codex). 종료된 에이전트 탭에 Restart 칩 | 9, 13 |
| 19 | 사이드바(프로젝트 그룹·핀·이름 변경·Needs You 목록) + Jump Palette(Cmd-J) | M3 | P1 | 세션 20개에서 Cmd-J 퍼지 검색으로 이동. 입력 대기 세션이 맨 위 | 13, 14 |
| 20 | Open in(VS Code·Cursor·Zed·Finder) + 비-git 폴더 세션 | M3 | P1 | Open in으로 워크트리가 열림. git이 아닌 폴더에서 에이전트 실행 가능 | 14 |
| 21 | Resource Manager 경량판(앱·데몬·세션별 메모리, 디스크, 데몬 재시작, 세션 kill) | M4 | P1 | UI에 메모리가 표시되고 `footprint` 값과 ±10% 안. 데몬 재시작은 확인 대화상자 뒤 동작 | 5 |
| 22 | 자동 업데이트(단일 채널, 서명·공증) + 세대 교체 | M4 | P0 | 구버전에서 업데이트해도 세션 유지. 공증 검증 통과. 고아 데몬 0(→ T4) | 4, 5 |
| 23 | 로컬 크래시 로그 + 진단 번들 + `mk doctor` | M4 | P1 | 강제 panic 뒤 크래시 로그 생성. `mk doctor`가 데몬·소켓·권한·에이전트 설치 상태 출력. 번들에 출력 본문 미포함 | 17 |
| 24 | 설정(TOML) + 최소 설정 UI + 키 바인딩 파일 + 라이트/다크 + UI 줌 | M4 | P1 | 설정 변경이 재시작 없이 반영. 키 바인딩 파일 재정의 동작 | 5 |
| 25 | 장애 주입 CI(SIGKILL 1,000회·출력 폭주·24시간 메모리) | M4 | P0 | CI에서 §3.5 생존율·격리·누수 지표 자동 측정, 실패 시 빌드 차단 | 5, 6, 10 |
| 26 | 라이선스 확정·`THIRD_PARTY_NOTICES.md`·README 포지셔닝·공개 벤치 | M4 | P1 | LICENSE(MIT OR Apache-2.0)와 고지 파일 존재. README 첫 화면에 메모리·생존 수치와 측정 방법 | 25 |
| 27 | 최소 접근성(사이드바·버튼 VoiceOver 라벨) | M4 | P2 | VoiceOver로 세션 목록 탐색과 새 세션 생성 가능 | 19 |
| 28 | `.worktreeinclude` 복사(APFS clonefile) + `orca.yaml` setup 스크립트 호환 | M5 | P2 | Orca 설정이 있는 레포에서 `.env` 복사와 setup이 자동 실행. `MINIKRILL_ROOT_PATH`(+`ORCA_ROOT_PATH`·`CONDUCTOR_ROOT_PATH` 별칭) 제공 | 14 |
| 29 | Hibernation(Orca 안전 조건 8가지) | M5 | P2 | done 상태로 30분 지난 백그라운드 에이전트를 종료하고, 열면 자동 resume. 대기·입력 중 에이전트는 절대 종료하지 않음 | 13, 18 |
| 30 | 에이전트 세션 히스토리(Claude·Codex, 요청 시 스캔) | M5 | P2 | 과거 세션 목록에서 Resume으로 새 탭 재개. 상주 프로세스 없음 | 18 |
| 31 | `claude agents --json` 연동 | M5 | P2 | agent view 백그라운드 세션이 사이드바에 상태와 함께 표시(S7) | 13 |
| 32 | 사용량·레이트리밋 표시(Claude·Codex 로컬 파일) | M5 | P2 | 상태바에 5시간·주간 창 사용률, 80%에서 경고 | 13 |
| 33 | 텍스트 diff 뷰어 + 줄 주석 일괄 전송 | M5 | P2 | 워크트리 대 base diff에 주석을 달고 "에이전트에 보내기"로 프롬프트 1개 주입 | 14 |
| 34 | 알림 웹훅(ntfy·Telegram·Discord, 본문 제외) | M5 | P2 | 설정 URL로 완료 이벤트 POST, 재시도 1회. 출력 본문 미포함 | 16 |
| 35 | 메뉴바 아이콘(전체 상태 요약) | M5 | P2 | 창을 닫아도 메뉴바에 working·대기 수 표시 | 13 |
| 36 | Quick Commands + Copy Context | M5 | P3 | 저장한 명령·프롬프트를 클릭 한 번으로 주입. 최근 출력 N줄 복사 | 5 |
| 37 | 워크트리 코멘트·상태(`mk set --comment`) + 외부 워크트리 감지 | M5 | P3 | 에이전트가 CLI로 쓴 코멘트가 사이드바에 표시. `.claude/worktrees`가 숨김 카드로 보임 | 17, 19 |
| 38 | ko/en 다국어 | M5 | P3 | 언어 전환 시 모든 UI 문자열이 바뀌고, 하드코딩 문자열 0(lint) | 24 |
| 39 | 원격 attach(데몬 바이너리 복사 + SSH 소켓 포워딩) | M6 | P3 | 원격 맥미니의 세션을 `mk attach --host`와 앱으로 조작. 네트워크가 끊겼다 붙어도 세션 유지 | 7, 8, 17 |
| 40 | 경량 orchestration(send/check/ask, worker_done) | M6 | P3 | 코디네이터 세션이 워커 3개에 작업을 배정하고 완료 메시지를 수집 | 17 |
| 41 | 예약 자동화(데몬 스케줄러) | M6 | P3 | cron 표현식 시각에 프롬프트 실행, 실행 기록 저장 | 17 |
