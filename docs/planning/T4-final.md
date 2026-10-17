# T4 최종: 에이전트·Git 워크트리 통합 + 상용 운영 (2차 검증본)

**결론(가장 중요한 결정 3가지)**
1. **에이전트 상태 수신기와 상태 저장소는 앱이 아니라 데몬에 둔다.**
   - hook 명령은 Rust 단일 바이너리(`krill hook`)로 만든다. 시제품을 실측한 결과 호출당 p50 2.6ms, 최대 RSS 2.2MB였다.
   - Claude Code에는 `--settings`로 세션마다 hook을 주입한다. 사용자 hook과 병합된다는 것을 실측으로 확인했다. 따라서 사용자 전역 설정은 편집하지 않는다.
2. **Git은 git CLI 서브프로세스로 통일한다.**
   - 재측정 결과 status는 60ms, RSS는 15.6MB이고 이 메모리는 일시적이다.
   - gix는 worktree remove를 지원하지 않는다.
   - `krill` CLI는 앱을 거치지 않고 **데몬에 직접 붙는다**. 앱이 꺼져 있으면 Orca CLI는 "Orca is not running"으로 끝나지만, `krill`은 그 상태에서도 동작해야 한다.
   - Orca orchestration과는 호환하지 않는다.
3. **상용 운영은 다음 넷을 M3 출시 게이트로 고정한다.**
   - Developer ID 서명과 공증
   - 자체 최소 업데이터(ed25519 서명 + codesign 검증 + 원자 교체 + 롤백)
   - 데몬 세대 공존과 **세션이 0개인 구 데몬의 자동 은퇴**. Orca는 이 정책이 없어 구 데몬이 쌓이는 것을 실측했다.
   - **macOS 업데이트·크래시 생존 e2e**. Orca에는 Windows용만 있다.

- 작성: 2026-10-14. 기준은 Orca 소스 HEAD `a0c46b40`(`.ref/orca`)와 설치 앱 v1.4.224다.
  - 설치본은 소스보다 구버전이다. launchd 헬퍼 경로가 없고 데몬을 fork 방식으로 띄운다.
- 표기 규칙:
  - `경로:줄`은 `.ref/orca` 기준 상대경로다.
  - **실측**은 이 맥미니에서 직접 잰 값이다(git 2.54.0, Claude Code 2.1.299, codex-cli 0.162.0, cargo 1.98.1).
  - **추정**은 재지 못한 값이다.
- 다른 트랙과 겹치는 부분:
  - 데몬 프로토콜·스크롤백·세대 교체 메커니즘은 T2
  - 렌더링·diff 화면은 T3
  - 기능 목록·잠자기 방지 등은 T1
  - 이 문서에서는 언급만 한다.
- 이름 충돌: T1 초안은 CLI 이름을 `mk`로 썼고, 이 문서는 과제 지시에 따라 `krill`로 쓴다. 통합할 때 하나로 정해야 한다(7장 이슈 #T4-00).

---

## 1. 이전 조사 정정 (주장 vs 실제 vs 근거)

1차 보고서(`.ref/drafts/T4-A.md`)의 주장을 다음 네 갈래로 나눠 대조했다.
- 에이전트 통합 27건
- Git 20건
- CLI·운영 24건
- 크레이트·웹 약 50건

**큰 틀의 결론은 유지된다.** 다만 아래 항목은 사실이 다르거나 근거 위치가 틀렸다. 표에 없는 나머지 주장은 원본과 일치했다. 그 가운데 핵심 수치(스풀 5MB/1024/7일, 30분 만료, 1.5초 quiet window, 4시간 규칙 갱신, fetch 30초/60초, 삭제 동시성 2, setup 120초, gh 페이싱 5분 20회 등)는 원문 줄까지 확인했다.

### 1.1 에이전트 통합

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| A1 | 에이전트 카탈로그 "40종 이상" | 정확히 **45개** | `src/shared/tui-agent.ts:4-48` |
| A2 | 프롬프트 주입 방식 4가지(argv / --prompt / -i / stdin) | **6가지**: `argv`, `flag-prompt`, `flag-prompt-interactive`, `flag-interactive`(-i), `hermes-query`, `stdin-after-start` | `src/shared/tui-agent-config-types.ts:1-7`, 분기 `src/shared/tui-agent-startup.ts:98,115,128,154,166` |
| A3 | 셸 준비 판정은 `ESC]133;A`나 bracketed paste | 세 가지 중 하나: `?2004h`, `?1034h`, `]133;A` | `src/main/line-editor-ready-output-scanner.ts:1` |
| A4 | hook 서버는 "항상 204(fail-open)"로 응답 | POST가 아니면 404, 토큰이 틀리면 **403**, 모르는 경로는 404. 성공하면 204, **본문 파싱 실패만 fail-open 204**. 토큰은 `randomUUID()` | `src/main/agent-hooks/server/server-lifecycle.ts:42,50,56,97,151,171` |
| A5 | `server-runtime-env.ts`가 `ORCA_PANE_KEY`도 주입 | 이 파일이 주입하는 것은 PORT·TOKEN·ENV·VERSION·TRANSPORT·ENDPOINT다. PANE_KEY는 renderer 쪽에서 넣는다 | `src/main/agent-hooks/server/server-runtime-env.ts:13-29`, `src/renderer/.../agent-idle-working-handlers.ts:65` |
| A6 | `curl --connect-timeout 0.5 --max-time 1.5`는 `hook-script.ts`에 있음 | 실제 위치는 `hook-post-command.ts`이고, 변수로 덮어쓸 수 있는 기본값이다 | `src/main/agent-hooks/hook-post-command.ts:15,25` |
| A7 | Codex trusted_hash 근거 `config-toml-trust.ts:142` | 이 줄은 안전 가드에 붙은 주석일 뿐이다. 해시는 **`codex app-server` RPC(`hooks/list`, `config/batchWrite`)로 받는다**. 자체 계산한 값이 어긋난 이슈가 있었다(#7896, #7110, #8699) | `src/main/codex/codex-app-server-client.ts:6-14`, 미러 `codex-hook-user-mirroring.ts:85` |
| A8 | OpenCode는 `OPENCODE_CONFIG_DIR` 오버레이에 플러그인을 둠 | **기본은 사용자 config 디렉터리에 직접 쓴다.** 오버레이는 사용자가 이미 `OPENCODE_CONFIG_DIR`을 지정했을 때만 쓴다 | `src/main/opencode/hook-service.ts:136-142,146-161` |
| A9 | Claude 정규화: Stop만 done | `Stop`과 `StopFailure`가 done이다. 수동 `PostCompact`도 done이다. `PostToolUseFailure`는 working이다 | `src/shared/agent-hook-listener/providers/claude-events.ts:86,124-135` |
| A10 | `cursor-events.ts:24`는 "waiting을 못 줘서 화면 규칙으로 잡는다"는 내용 | 실제 내용은 "beforeShell/MCP를 working으로 처리해 waiting 알림 스팸을 막는다"이다. 승인 메뉴는 `cursor.json` 규칙이 잡고 결과는 **`blocked`**(waiting 아님) | `src/shared/agent-hook-listener/providers/cursor-events.ts:24`, `src/main/runtime/agent-state-rules/cursor.json` |
| A11 | 프로세스 캡처($PPID, 시작 시각)는 일반 기능 | **Claude 전용**이다 | `src/main/claude/hook-script.ts:82`, `src/main/agent-hooks/hook-process-capture.ts:2-22` |
| A12 | 포그라운드 폴링 750ms~15s (`agent-completion-poll-cadence.ts`) | 값은 750/2000/3000/15000ms로 맞다. 위치는 renderer다 | `src/renderer/src/components/terminal-pane/agent-completion-poll-cadence.ts:3-8` |
| A13 | transcript 마지막 메시지를 읽는다 | Claude는 Stop payload의 **`last_assistant_message`를 먼저 쓰고**, 없을 때만 transcript를 뒤에서부터 스캔한다(5회 × 50ms 재시도). **실측**: Claude 2.1.299의 Stop payload에 `last_assistant_message`와 `transcript_path`가 들어 있다 | `src/main/claude/claude-tool-fields.ts:52-60`, `src/main/agent-hooks/server/server-constants.ts:5-6`. 실측 로그는 3.1.2 |
| A14 | (1차가 언급 안 함) Claude 입력 대기 신호 | Orca는 Claude의 **Notification hook을 등록하지 않는다.** waiting은 PermissionRequest와 PreToolUse(AskUserQuestion)로만 판정한다. `idle_prompt`는 "Stop 후 약 60초 뒤에 온다"는 메모만 있다 | `src/main/claude/claude-managed-hook-events.ts:10-78`, `claude-owed-task-notifications.ts:21-26` |
| A15 | AGENTS.md:107-109 | 실제는 108-110이다(섹션 제목 `## Agent Status`가 :108) | `AGENTS.md:108-114` |

### 1.2 Git 워크트리

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| G1 | git·gh·glab CLI만 쓴다 | Bitbucket·Gitea·Azure DevOps는 **HTTP fetch로 직접 호출**한다 | `src/main/bitbucket/client.ts:89`, `src/main/gitea/client.ts:91`, `src/main/azure-devops/azure-devops-api-request.ts:160` |
| G2 | 베이스 결정 순서: meta baseRef → 레포 설정 → origin/HEAD → origin/main (`git-default-base-ref.ts`) | 우선순위는 사용자 요청 → `repo.worktreeBaseRef` → 감지값 순서다(`worktree-create-base.ts`). 감지할 때는 origin/HEAD → origin/main → origin/master → main → master 순서로 찾는다 | `src/main/worktree-create-base.ts:8-29`, `src/shared/git-default-base-ref.ts:3-14` |
| G3 | "미리 준비된 `--no-checkout` 워크트리 **풀**" | 생성 창이 열려 있을 때만 하는 **투기적 사전 준비**다(최대 3개, TTL 5분, `.orca-preparing/`). `--no-checkout --lock` 다음에 **`reset --hard`로 전체 체크아웃**을 한다. 제출하면 `worktree move` + `checkout -b`를 한다. 비용이 없어지는 게 아니라 **앞당겨질** 뿐이다. 실측: `reset --hard`가 2.92초 | `src/main/git/worktree-preparation-add.ts:47-57`, `src/main/git/worktree-create-preparation.ts:73-76,203-229`, `worktree-create-preparation-pool.ts:45-46` |
| G4 | 브랜치는 `branch -d`로만 지운다 | 생성에 실패해 롤백할 때는 `-D`를 쓴다. `-d`가 실패해도 squash-merge로 변경이 없다는 것이 증명되면 강제로 지운다 | `src/main/git/worktree-branch-removal.ts:22-23,70,105-131` |
| G5 | setup 환경 변수는 ROOT/WORKTREE/CONDUCTOR 세 개 | `ORCA_WORKSPACE_NAME`, `GHOSTX_ROOT_PATH`도 준다 | `src/main/setup-hook-env-vars.ts:14-22` |
| G6 | `.worktreeinclude`는 256KB / 1000개 제한 | 256KB는 **`.worktreeinclude` 파일 자체**의 크기 제한이고 1000은 **항목 수**다. glob과 부정 패턴 줄은 건너뛰고, **gitignore된 경로만** 복사한다 | `src/main/git/worktree-include-file.ts:14-19,62,92,138-141` |
| G7 | sharedDirectories는 node_modules 등을 심볼릭 링크로 연결 | 사용자가 설정한 항목만 대상이다(기본값 없음) | `src/main/git/worktree-shared-directories.ts:79-125`, `src/main/ipc/worktree-symlinks.ts:34-45` |
| G8 | git 동시 실행: 일반 2~4, 전체 10 | 일반은 `max(2,min(4,cores-4))`에 여유 2, 네트워크는 3+1, route는 2+1이다. 전체 10이고 **삭제는 이 제한을 우회**한다 | `src/shared/git-admission-state.ts:6-15` |
| G9 | status에 `-z`(1차 설계안) | Orca는 `-z` 없이 스트림을 파싱하다가 제한에 도달하면 멈춘다. `GIT_OPTIONAL_LOCKS=0`을 쓴다 | `src/main/git/source-control/status-read.ts:137-149`, `src/main/git/command-runner/git-process-env.ts:13` |
| G10 | numstat 2분 캐시는 `status-line-stats.ts:29` | 인자는 `-c diff.autoRefreshIndex=false diff -z --numstat -M`이다. 캐시는 다른 파일에 있다 | `status-line-stats.ts:20-31`, `src/shared/git-status-line-stats-cache.ts:47` |
| G11 | (1차 설계안) FSEvents 감시 | Orca는 `@parcel/watcher`를 **크래시가 격리된 자식 프로세스**에서 돌린다. git common dir의 메타 파일(HEAD, index, packed-refs 등)은 얕게 감시하고, 15틱마다 stat으로 놓친 변경을 보완한다 | `src/main/ipc/parcel-watcher-process.ts:1-25`, `worktree-git-common-metadata-files.ts:4-11`, `worktree-git-common-primary-watch.ts:19-25` |
| G12 | 벤치마크 수치 | **재측정해도 일치했다**(새 클론, tracked 35,926개). status 60ms·RSS 15.6MB, add 2.93~3.03초, remove 1.39~1.49초, `--no-checkout` 0.01초. 1차에 없던 값: **`worktree add`를 하는 git 프로세스의 최대 RSS는 약 158MB**(일시적) | 2차 실측 |

### 1.3 CLI

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| C1 | `orca orchestration check` 한 번에 89.7MB | 재현하지 못했다(메시지가 오가는 명령은 실행하지 않음). **`orca --version`과 `check --help`는 최대 RSS 약 71MiB, 0.06초**다. 서버와 왕복하지 않아도 Electron(Node) 기동 비용이 70MB대라는 뜻이다 | 2차 실측 `/usr/bin/time -l` |
| C2 | 앱이 꺼진 상태의 CLI 동작은 미확인 | 일반 명령은 **`Orca is not running. Run 'orca open' first.` 오류로 끝난다.** 앱을 띄우는 것은 `orca open`뿐이다 | `src/cli/cli-error.ts:50,74`, `src/cli/runtime/client.ts:285,296` |
| C3 | orchestration은 SQLite에 저장 | `node:sqlite`(내장)를 쓴다. 명령에는 worker-*, run-*, reply, inbox, coordinator-*도 있다 | `src/main/orchestration/db/orchestration-db.ts:1,29` |
| C4 | tui-idle 판정 순서: hook → blocked 텍스트 → 3초 정적 | **blocked를 먼저** 보고 그다음 hook(ready-strong), 정적 3초 순서다. 기본 5분 타임아웃은 tui-idle에만 걸리고 exit 대기는 무제한이다 | `src/main/runtime/terminal/wait/terminal-wait.ts:100,171`, `orca-runtime-postlude.ts:60,64` |

### 1.4 상용 운영

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| O1 | 워크플로 95개 | **91개** | `ls .github/workflows` |
| O2 | 텔레메트리 이벤트 89종 | **92종**이다. 신규 설치는 `optedIn: true`(opt-out 기본)이고, 배너는 기존 사용자에게만 보인다("new users are covered by install-time disclosure") | `src/shared/telemetry-event-registry.ts:109-216`, `loaded-cohort-migrations.ts:96-102`, `FirstLaunchBanner.tsx:1-2` |
| O3 | 데몬 실행 파일을 "버전별" 경로에 복사 | 복사본 경로는 **헬퍼의 CDHash 기준**이다(`terminal-host-<cdhash>`). launchd 라벨이 **프로필 + 프로토콜** 기준이다. 헬퍼 1개는 약 124MiB이고 앱과 **같은 bundle id(`com.stablyai.orca`)로 서명**해 TCC 권한을 물려받는다. 단, **설치된 1.4.224에는 `Contents/Helpers`도 `daemon-host/`도 없고 fork 방식**이다 | `docs/reference/macos-daemon-tcc-attribution.md:9-53`, 2차 실측 |
| O4 | 구 데몬 v40·v41이 자식 0개로 각각 약 16MB | v40 17.2MB, v41 **21.7MB**, 현재 v43은 139MB에 자식 **19개**다. v36~38은 소켓만 남아 있고 v39·v42는 없다. **데스크톱에는 세션이 0개인 구 데몬을 은퇴시키는 경로가 없다.** `requestIdleDaemonRetirement`는 있지만 orcad managed stop에서만 호출된다. 업데이트 1회마다 약 17~22MB씩 쌓이는 것은 의도라기보다 부작용으로 보인다(관련 주석이나 이슈는 찾지 못함, 추정) | `src/main/daemon/daemon-legacy-adapters.ts:36-49`, `daemon-provider-state.ts:163`, `daemon-router-retirement.ts:45`, `orcad/orcad-daemon-retirement.ts:1-7` |
| O5 | 종료할 때 disconnect만 하고 shutdown은 하지 않음 | 맞다. 단 개발 부모 프로세스를 종료할 때는 예외로 shutdown한다 | `src/main/startup/main-process-quit.ts:260` |
| O6 | 업데이트 확인 백오프는 1시간부터 | 1시간에서 시작해 두 배씩 늘리고 **상한은 6시간**이다. `autoDownload=false`는 updater-state가 아니라 updater-setup에 있다 | `src/main/updater/updater-state.ts:8-11`, `updater-setup.ts:147` |
| O7 | 테스트 파일 약 13,539개, 메모리·성능 e2e 26개 | `*.test.ts*`는 **13,248개**다. 메모리·성능 e2e는 파일명 기준 **16개**만 확인했다. e2e 스펙 498개와 터미널·PTY 148개는 맞다 | 2차 실측 `find` |
| O8 | `macos-updater-tests.yml`이 업데이트 생존을 검증 | **vitest 단위·통합 테스트 8개만** 돌린다. 실제 생존 e2e는 Windows용(`win-update-survival-e2e.yml`)과 `tests/e2e/daemon-*.spec.ts`에 있다 | `.github/workflows/macos-updater-tests.yml:36-48` |
| O9 | 토큰 비교가 `!==` | 데몬은 `!==`, 런타임 RPC는 `===`다. **둘 다 상수 시간 비교가 아니다** | `src/main/daemon/daemon-client-connections.ts:131`, `runtime-rpc-request-admission.ts:135` |
| O10 | OAuth PKCE 근거 `profile-cloud-auth-config.ts:23-26` | 그 줄에는 scope와 client id가 있다. PKCE(S256)는 다른 파일에 있다 | `src/main/orca-profiles/profile-cloud-pkce.ts:143-144` |

### 1.5 크레이트·웹

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| W1 | cargo-dist 0.33.0 (2026-09-11) | 0.33.0은 **GitHub 릴리스**에만 있다. **crates.io 최신 안정판은 0.32.0(2026-05-22)** | https://crates.io/crates/cargo-dist , https://github.com/axodotdev/cargo-dist/releases |
| W2 | mac-notification-sys 0.6.15 MIT | 라이선스는 **MIT/Apache-2.0**이다. **구식 `NSUserNotification`을 쓴다.** notify-rust 4.18.2도 기본 백엔드가 이것이고, `preview-macos-un` 기능을 켜야 UN을 쓴다 | https://github.com/h4llow3En/mac-notification-sys , https://github.com/hoodie/notify-rust |
| W3 | (미확인 1) `claude --settings` hooks가 병합되는지 | **병합된다.** 문서 원문: "Hook entries merge across settings levels rather than replacing each other". 우선순위는 managed > `--settings` > local > project > user. **실측**으로 `--settings` hook과 프로젝트 hook이 둘 다 실행됐다 | https://code.claude.com/docs/en/settings , 3.1.2 |
| W4 | (미확인 4) Gemini Notification이 권한 대기에서 발화하는지 | 발화한다. `notification_type: "ToolPermission"`이고 관찰 전용이다 | https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md |
| W5 | (미확인 9) GHA 공개 레포 무료 여부 | 무료다. "free for standard GitHub-hosted runners in public repositories". Intel 라벨은 `macos-26-intel`도 있다 | https://docs.github.com/en/actions/concepts/billing-and-usage , https://docs.github.com/en/actions/reference/runners/github-hosted-runners |
| W6 | (미확인 2) Codex hook을 `-c`로 세션별 주입 | 문서에는 언급이 없다. 소스에는 `-c` 레이어(`SessionFlags`)를 탐색하는 코드가 있지만 비관리 레이어라 **trust가 필요**하다. **실측**: `codex exec -c 'hooks.Stop=[...]'`에서 hook이 **발화하지 않았다.** 원인이 trust 게이트인지 미지원인지는 구분하지 못했다 | https://github.com/openai/codex/blob/main/codex-rs/hooks/src/engine/discovery.rs , 3.1.2 |
| W7 | Cursor CLI hook 실행 여부 미확인 | 문서는 workspaceOpen에 대해서만 "desktop app and CLI"라고 명시한다. 포럼에는 CLI에서 부분적으로만 동작한다는 보고와 Linux 버그(2026-08-11) 보고가 있다. 권한 요청이나 idle 전용 이벤트는 없다 | https://cursor.com/docs/agent/hooks , https://forum.cursor.com/t/cursor-cli-hooks/148511 |
| W8 | (보충) Claude hook 기본 타임아웃 | 600초다. Orca는 관리 hook에 **10초**를 건다 | https://code.claude.com/docs/en/hooks , `src/main/agent-hooks/installer-utils.ts:49` |

나머지 크레이트의 버전·날짜·라이선스는 모두 맞았다: gix 0.89.0, git2 0.21.0, notify 8.2.0(CC0-1.0), clap 4.6.7, objc2-user-notifications 0.3.2, sentry 0.49.3(`minidump` 기능 공식 지원, `sentry-rust-minidump`는 deprecated), crash-handler 0.8.1, minidumper 0.11.0, posthog-rs 0.27.0, ed25519-dalek 3.0.0(정식 안정판), minisign-verify 0.3.0, keyring 4.2.0, security-framework 3.7.0, tokio 1.53.3(`UnixStream::peer_cred` 있음), cargo-nextest 0.9.148, insta 1.49.0, expectrl 0.9.0, self_update 1.3.0, axoupdater 0.10.2, cargo-packager-updater 0.2.3, velopack 1.2.161, Sparkle 2.10.0, sparkle-updater 0.1.0(개인 Tauri용 프로젝트), apple-codesign 0.29.0(2024-11 이후 릴리스 없음). 가격 정보도 1차와 같다(Conductor, Superset, Warp, Zed, Keygen, Lemon Squeezy, Paddle).

---

## 2. 확정 결정 사항

되돌리기 비용의 뜻은 다음과 같다.
- **낮음**: 모듈 하나만 바꾸면 된다.
- **중간**: 여러 모듈과 설정 형식이 바뀐다.
- **높음**: 배포된 사용자 데이터나 프로토콜, 서명 체계에 영향이 간다.

| # | 결정 | 권고안 | 대안 | 근거 | 되돌리기 비용 |
|---|---|---|---|---|---|
| D1 | 상태 수신 위치 | **데몬**이 수신기와 상태 저장소를 갖는다. 앱은 구독만 한다 | Orca처럼 앱 HTTP 서버 + 디스크 spool | Orca는 앱에 둬서 앱이 재시작되는 동안의 이벤트를 spool과 endpoint 파일 재읽기로 우회한다(`hook-script.ts:83-92`, `agent-hook-spool.ts:15-17`). 데몬은 PTY 주인이므로 같은 수명을 갖는다. 사용자가 가장 불만인 "끊김"을 직접 해결한다 | 중간 |
| D2 | hook 전송 | `krill hook <agent>` Rust 바이너리가 stdin JSON을 유닉스 소켓으로 보낸다. **stdout에 `{}`를 먼저 찍고** 소켓 쓰기 타임아웃은 200ms로 한다 | Claude `type:"http"` hook(프로세스 생성이 없음), bash+curl(Orca) | 실측 p50 2.6ms·p95 3.9ms·RSS 2.2MB(3.1.2). http hook을 쓰면 데몬에 TCP 리스너가 필요해 공격면이 늘어난다 | 낮음 |
| D3 | hook 주입 | Claude는 세션별 `--settings <0600 파일>`로 한다(실측 병합 확인). Codex는 S2 스파이크 결과에 따른다. Cursor·OpenCode·Gemini는 사용자 설정을 **동의 후 편집**하고, 원복 기록과 `KRILL_PANE_ID` 가드를 둔다 | 모든 에이전트의 전역 설정 편집(Orca) | Orca는 `~/.claude/settings.json`에 13개 이벤트를 써 넣는다(실측, `src/main/claude/hook-settings.ts:38-52`). 세션별로 주입하면 사용자 설정을 오염시키지 않고, 앱을 삭제해도 흔적이 없다 | 낮음 |
| D4 | 상태 모델 | `working / waiting / blocked / done / exited / unknown` 여섯 가지에 **근거 출처**(`hook / process / title / screen`)와 **신뢰도**를 함께 기록한다 | Orca 4상태 | Orca의 blocked는 화면 규칙에서 나오고(`blocked-text-layer.ts:12-19`), 프로세스 종료는 별도 완료 근거다(`agent-completion-process-types.ts:10`). 출처를 남겨야 UI에서 "추정"을 구분하고 텔레메트리로 정확도를 잴 수 있다 | 중간(프로토콜) |
| D5 | 보조 감지 | 우선순위: 프로세스(SIGCHLD, `tcgetpgrp`) > OSC 0/2 제목 글리프 > 출력 정지 + 화면 규칙. transcript는 마지막 메시지가 없을 때만 읽는다 | transcript 파일 감시를 주 신호로 사용 | 데몬이 PTY를 쥐고 있어 프로세스 신호를 비용 없이 얻는다. Claude Stop payload에 `last_assistant_message`가 있다(실측) | 낮음 |
| D6 | 세션 재개 | `claude --session-id <uuid>`를 미리 발급한다(실측 동작). Codex는 SessionStart payload의 id를 저장해 `codex resume <id>`로 재개한다 | hook에서 ID를 추출(Orca) | 데몬이 소실(재부팅)되어도 대화를 이어 갈 수 있다. T2 복구 범위 밖을 보완한다 | 낮음 |
| D7 | Git 엔진 | **git CLI만 쓴다**(쓰기·status·diff 전부). `-c core.quotePath=false`, `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`을 붙인다 | gix 읽기 병용 / git2 | 실측 속도가 비슷하다(git 60ms vs gix 44~66ms). gix는 상주 메모리(1차 실측 최대 45.6MB)가 들고 remove를 지원하지 않는다. git2는 C 의존성이 생긴다 | 낮음 |
| D8 | git 작업 위치 | 워크트리 생성·삭제(변경)는 **데몬 작업 큐**에서 한다(앱이 꺼져도 완료, CLI 지원). status·diff 표시용 조회는 **앱**에서 한다 | 전부 데몬 / 전부 앱 | 삭제는 p90 29초다(`worktree-removal.ts:66-67`). 데몬은 가능한 한 얇아야 한다. 파싱 버그로 데몬이 죽으면 모든 세션이 죽는다 | 중간 |
| D9 | 설정 호환 | `krill.toml`을 기본으로 하고 `orca.yaml`의 `scripts.setup/archive`와 `.worktreeinclude`는 읽기 전용으로 호환한다. 환경 변수는 `KRILL_*`와 `ORCA_ROOT_PATH`, `ORCA_WORKTREE_PATH`, `CONDUCTOR_ROOT_PATH`를 함께 준다 | 독자 형식만 | 이주 비용이 줄어든다. Orca도 Conductor 변수를 호환한다(`setup-hook-env-vars.ts:14-22`) | 낮음 |
| D10 | CLI 전송 | Rust 단일 바이너리가 **데몬에 직접** 붙는다. 줄 단위 JSON에 `--json` 스키마 버전을 둔다 | 앱 경유(Orca) | Orca CLI는 기동만으로 약 71MiB(실측)이고, 앱이 꺼져 있으면 실패한다(`cli-error.ts:50`) | 중간(외부 계약) |
| D11 | Orca CLI·orchestration 호환 | 하지 않는다. 명령 모양만 비슷하게 맞춘다 | `orca` 별칭 shim | orchestration은 `node:sqlite` 기반의 큰 하위 제품이고 앱 런타임에 묶여 있다(`orchestration-db.ts:1,29`) | 낮음 |
| D12 | 서명·공증 | Developer ID + hardened runtime + **빈 엔타이틀먼트** + notarytool(App Store Connect API 키) + staple을 macOS 러너에서 한다 | rcodesign으로 Linux에서 서명 | rcodesign은 2024-11 이후 릴리스가 없다. Homebrew는 Gatekeeper 통과를 필수로 요구한다 | 높음(인증서·TCC) |
| D13 | 데몬 헬퍼 신원 | 데몬은 앱 번들 안의 서명된 헬퍼(`.app`)로 두고 **앱과 같은 identifier + 팀**으로 서명한다. 실행 위치와 방식(복사본 + launchd)은 T2가 정한다 | 별도 bundle id | TCC 권한(전체 디스크 접근 등)을 물려받는다(`macos-daemon-tcc-attribution.md:47-53`) | 높음 |
| D14 | 앱 업데이트 | 자체 최소 업데이터: `latest.json` + ed25519(`ed25519-dalek` 3.0.0 또는 `minisign-verify` 0.3.0) + `codesign --verify` + `ditto` + rename 원자 교체 + 이전 번들 보존(롤백) | Velopack(서명·공증 플래그 지원 확인), Sparkle 2.10 FFI | 데몬 세대 오케스트레이션은 어차피 직접 짜야 한다. sparkle-updater 크레이트는 개인 Tauri 프로젝트(0.1.0)다 | 중간 |
| D15 | 데몬 세대 공존 | 새 앱이 기존 데몬과 프로토콜을 협상한다. 새 세션은 새 세대에서 열고, **구 세대는 세션이 0개가 되고 60초 뒤 스스로 종료**한다 | 업데이트할 때 데몬 재시작 | Orca 데스크톱에는 은퇴 경로가 없어 구 데몬이 쌓인다(O4 실측) | 중간 |
| D16 | 크래시 | panic hook으로 로컬 파일에 기록하고 다음 실행 때 감지해 "진단 보내기"를 띄운다. 업로드는 opt-in `sentry` 0.49.3(`minidump` 기능은 앱에만). 데몬은 세션 작업을 격리하고 재기동은 60초에 5회로 제한한다 | 전 프로세스에 minidump | 데몬 메모리 예산. Orca도 로컬 저장만 한다(`crashpad-capture.ts:32-35,71`). 재기동 제한 근거는 `daemon-respawn-throttle.ts:29-30` | 낮음 |
| D17 | 텔레메트리 | **opt-in, 기본 꺼짐**, 이벤트 10개 이하, `DO_NOT_TRACK`과 CI 환경 존중 | Orca식 opt-out(설치 시 고지) | 과제 요구사항. Orca는 opt-out에 92종이다(O2) | 낮음 |
| D18 | 소켓 보안 | 0700 디렉터리 + 0600 소켓 + `peer_cred` uid 검사 + `subtle` 2.6.1 상수 시간 비교 + **pane 범위 토큰** | 단일 토큰(Orca) | 에이전트가 다른 세션에 키 입력을 주입하는 것을 막는다. Orca는 상수 시간 비교도 하지 않는다(O9) | 높음(프로토콜) |
| D19 | 배포 | DMG(GitHub Releases) + 자체 Homebrew tap(`auto_updates true`, `binary krill`, `zap`) | 공식 cask(나중에) | Orca와 같은 구조(`Casks/orca.rb:8,23,34,39`). 공식 cask는 Gatekeeper 통과가 필수다 | 낮음 |
| D20 | 품질 게이트 | 메모리(10터미널 ≤ 200MB)와 생존 e2e(앱 kill -9, 업데이트, 데몬 크래시)를 **macOS nightly + 릴리스 게이트**로 둔다 | 수동 측정 | 핵심 목표 수치와 핵심 불만을 지키는 유일한 장치다. Orca는 macOS 실제 생존 e2e가 없다(O8) | 낮음 |
| D21 | 라이선스·결제 | 1.0 전에 결정한다. 지금은 `capabilities` 구조체 하나와 오프라인 ed25519 라이선스 검증 자리만 둔다 | MIT 무료 / FSL + Keygen + Lemon Squeezy | 경쟁 제품은 Free + Pro $10~50/월이다. 검증 크레이트는 업데이터와 같은 것을 재사용한다 | 낮음 |

---

## 3. 상세 설계·요구사항

### 3.1 에이전트 통합

#### 3.1.1 Orca가 하는 방식(검증 완료)
- **원칙**: 상태는 hook에서 오고 제목으로 추론하지 않는다(`src/shared/agent-status-types.ts:2-3`). 실행 호스트가 상태 저장소 하나를 갖는다(`AGENTS.md:108-110`). 다만 실제로는 다음도 근거로 쓴다.
  - 화면 텍스트 규칙(blocked)
  - 제목(완료)
  - 프로세스 종료
- **hook 경로**: 앱 main의 `node:http`(`127.0.0.1:0`)가 `/hook/<agent>` 28개 경로를 받는다(`server-lifecycle.ts:198`, `source-routing.ts:5-34`).
  - hook 스크립트는 `{}`를 먼저 출력한다. 빈 출력이면 Claude 권한 hook이 fail-closed되기 때문이다(#14818, `hook-script.ts:64-65`).
  - 호출할 때마다 endpoint 파일을 source한다(:83-88). 그 뒤 curl로 보내고, 실패하면 spool에 쌓는다.
  - 관리 hook의 타임아웃은 10초다(`installer-utils.ts:49`).
- **사용자 hook을 보존하는 방법**: 스크립트 파일명으로 Orca 항목만 식별해 교체한다(`installer-utils.ts:72-90,176`). 쓰기는 원자적으로 하고 롤링 `.bak`을 남긴다(:296-342).
  - **Claude 버전에 따라 이벤트 목록을 정한다.** Claude는 모르는 이벤트가 있으면 설정을 거부하기 때문이다(`claude-managed-hook-events.ts:97-116`, `hook-service.ts:99`).
  - 백그라운드 세션(`CLAUDE_JOB_DIR`)과 Devin 안에서는 hook을 건너뛴다(`hook-script.ts:72,77-81`).
- **실측**: 이 맥의 `~/.claude/settings.json`에 Orca hook 13개 이벤트가 들어 있다(SessionStart~SessionEnd).
  - 모든 hook 명령은 `ORCA_PANE_KEY`가 없으면 아무것도 하지 않도록 가드돼 있다. Codex 오버레이 `hooks.json`의 명령도 같은 가드로 시작한다.
- **Codex**: 계정별 `CODEX_HOME`(실측 `~/Library/Application Support/orca/codex-accounts/<id>/home`, 그 안에 별도의 `auth.json`)의 `hooks.json`에 9개 이벤트를 넣는다.
  - `config.toml`의 `[hooks.state."<path>:<event>:0:0"] trusted_hash`로 신뢰를 기록한다(실측). 해시는 `codex app-server` RPC에서 받는다(A7).
  - 전역 `~/.codex/hooks.json`은 비어 있다(실측). `notify`는 쓰지 않는다.
- **보조 신호**:
  - 제목 글리프: `✳`이면 idle, 브라유 문자면 working(`agent-title-glyphs.ts:1`, `agent-title-status.ts:190,245`)
  - 인터럽트 추론: Esc/Ctrl-C 뒤 500ms(`agent-interrupt-intent.ts:5`)
  - 30분 만료(`agent-status-freshness.ts:7`)
  - 화면 규칙 JSON을 4시간마다 릴리스에서 핫 업데이트(`agent-state-rules-live-update.ts:25,44`)
  - 정의만 있고 보내는 쪽이 없는 OSC 9999(`agent-status-osc.ts:5`)
- **에이전트가 보내는 OSC 9 / 777 알림과 BEL은 처리하지 않는다.** OSC 777은 Orca 자체 셸 준비 마커로만 쓴다(`shell-startup-features.ts:45`).
- **중첩 에이전트**: 자식 CLI도 `ORCA_PANE_KEY`를 물려받는다. 그래서 상태는 갱신하되 소유권은 넘기지 않는다(`agent-hook-presence-transition.ts:26`, `agent-status-identity.ts:26,75`).

#### 3.1.2 이번 조사의 실측 스파이크
1. **Claude `--settings` 병합과 `--session-id`**
   - 환경: `/tmp/t4spike/proj`, `ORCA_*` 환경을 지운 `env -i`, `claude -p --model haiku`
   - 프로젝트 `.claude/settings.json`에 SessionStart·Stop hook을 넣고, `--settings` JSON에도 SessionStart·Stop hook을 넣었다.
   - 결과 로그: `flag-SessionStart`, `project-SessionStart`, `project-Stop`, `flag-Stop`. **두 출처가 모두 실행됐다.**
   - Stop payload는 `{"session_id":"b1c57a6f-…"(지정한 UUID 그대로), "transcript_path":"~/.claude/projects/-private-tmp-t4spike-proj/<uuid>.jsonl", "permission_mode":"default", "hook_event_name":"Stop", "last_assistant_message":"ok", "background_tasks":[], …}`였다.
2. **Codex `-c` hook 주입**: `codex exec --skip-git-repo-check -c 'hooks.Stop=[{hooks=[{type="command",command="echo … >> log"}]}]'`로 실행했다. 응답은 정상이었지만 **hook은 실행되지 않았다**(로그 파일이 생기지 않음).
3. **`krill hook` 시제품**: std만 쓴 Rust 바이너리다(`{}` 출력 → stdin 읽기 → `UnixStream` 쓰기, 쓰기 타임아웃 200ms). release + LTO + strip.
   - 바이너리 크기 320KB
   - 최대 RSS 2.2MB
   - 200회 실행: 데몬이 떠 있을 때 p50 2.63ms·p95 3.93ms·최대 5.42ms, 소켓이 없을 때 p50 2.50ms·p95 4.42ms
   - 측정값에는 파이썬 `subprocess` 오버헤드가 포함돼 있다. 참고로 `rg --version`의 최대 RSS는 2.9MB였다.

#### 3.1.3 에이전트 프로필(TOML, 내장 + 사용자 정의)
```toml
[agent.claude]
detect = "claude"                     # PATH 탐색
launch = ["claude"]
expected_process = "claude"           # 포그라운드 프로세스 이름
prompt_mode = "argv"                  # argv | flag:<name> | stdin-after-ready  (Orca 6종을 3종으로 일반화)
hook = { kind = "claude-settings", min_version = "2.1.0" }   # claude-settings | codex-home | opencode-plugin | cursor-hooks | gemini-settings | none
session = { preassign = "--session-id", resume = ["claude", "--resume", "{session_id}"] }
title = { idle = ["✳"], working = ["braille"] }
yolo_args = ["--dangerously-skip-permissions"]   # 프로필별 opt-in일 때만 붙임
```
- **MVP 내장 5종**: Claude Code, Codex, OpenCode, Cursor CLI, Gemini CLI. 나머지 40여 종은 "제목·프로세스만 쓰는 일반 프로필"로 둔다.
- **이벤트 목록은 에이전트 버전으로 게이트한다.** `claude --version`을 캐시하고, 모르는 이벤트 때문에 설정 전체가 거부되는 것을 막는다(Orca 근거 위). 버전 감지에 실패하면 가장 보수적인 이벤트 집합을 쓴다.

#### 3.1.4 감지 계층과 신뢰도

| 계층 | 신호 | 대상 | 신뢰도 |
|---|---|---|---|
| L1 | 공식 hook → `krill hook` → 데몬 | Claude(PermissionRequest, PreToolUse:AskUserQuestion, Stop/StopFailure, UserPromptSubmit, Pre/PostToolUse, SubagentStart/Stop, SessionStart/End), Codex(PermissionRequest, Stop, Interrupt≥0.150, …), OpenCode(`permission.asked`, `session.idle`), Gemini(Notification `ToolPermission`, AfterAgent) | 높음(추정 95% 이상, M2에서 실측해야 함) |
| L2 | 자식 종료(SIGCHLD), 포그라운드 pgid 변화 | 모든 에이전트 | 종료·크래시는 확정 |
| L3 | OSC 0/2 제목 글리프 | Claude 등 | 중간. 버전마다 바뀐다(#13889, #6011 `tui-idle-evidence.ts:30`) |
| L4 | 출력 정지 + 화면 규칙(trust·권한 메뉴 정규식) | Cursor 승인 메뉴, trust 프롬프트(→ blocked) | 낮음~중간 |

결합 규칙은 다음과 같다.
- L1이 이긴다.
- L1이 working인데 L2가 종료를 알리면 `exited`로 바꾼다.
- L1이 없는 에이전트는 L3과 L4를 합쳐 "추정" 배지를 단다.
- 같은 pane 안의 자식 에이전트 이벤트는 `session_id`가 리더와 다르면 리더의 상태를 바꾸지 않는다(Orca 중첩 규칙).
- Claude `Notification`은 `permission_prompt`·`elicitation_dialog`만 waiting 보조 신호로 쓴다. `idle_prompt`는 Stop 후 약 60초 뒤에 오므로(A14) 상태 판정에는 쓰지 않는다.

#### 3.1.5 Orca와 다르게 할 점
1. **수신기는 데몬에 둔다.** spool은 기본적으로 필요 없다. PTY와 상태가 같은 프로세스 수명을 갖기 때문이다.
   - 예외는 T2의 데몬 세대 교체·핸드오버 창이다. 이때를 위해 `krill hook`은 소켓 연결을 50ms 간격으로 3회 재시도한다.
   - 그래도 실패하면 최대 64개 / 1MB 규모의 작은 spool에 남긴다(P2).
2. **세션별 주입**: Claude에는 데몬이 `~/Library/Application Support/Minikrill/agent/<pane>/claude-settings.json`(0600)을 만들어 `--settings <path>`로 넘긴다.
   - JSON을 argv로 넘기면 `ps`에 노출되므로 파일 경로를 쓴다.
   - 사용자 hook과 같은 핸들러는 Claude가 한 번만 실행한다(문서).
3. **전역 편집이 불가피한 에이전트**(Cursor, OpenCode, Gemini, Codex 경로에 따라)는 다음을 지킨다.
   - 온보딩에서 diff를 보여 주고 동의를 받는다.
   - Orca처럼 krill 항목만 파일명으로 식별해 원자적으로 쓰고 `.bak`을 남긴다.
   - 명령 첫 줄에 `[ -n "$KRILL_PANE_ID" ] || exit 0` 가드를 둬서 Minikrill 밖에서는 아무것도 하지 않게 한다.
   - 앱 설정에 "hook 제거" 버튼을 둔다.
4. **`krill hook`은 결정을 내리지 않는다.** PermissionRequest에 대해 allow/deny JSON을 절대 출력하지 않고 항상 `{}`만 출력한다. 데몬이 손상돼도 권한을 자동 승인하는 통로가 되지 않게 하려는 것이다.

#### 3.1.6 알림
- 데몬이 상태 전이 이벤트를 발행하고, **앱이** `UNUserNotificationCenter`로 알림을 띄운다.
  - 크레이트는 `objc2-user-notifications` 0.3.2를 쓴다. `mac-notification-sys`나 notify-rust 기본 백엔드는 deprecated된 NSUserNotification을 쓰므로 쓰지 않는다(W2).
  - Dock 배지도 앱이 단다.
- 완료 알림은 1.5초 quiet window를 둔 뒤 보낸다(Orca `agent-completion-notification-controller.ts:15`). 입력 대기(waiting) 알림은 즉시 보낸다.
- 앱이 꺼져 있을 때 받는 알림은 P2로 둔다. UN API는 서명된 번들이 있어야 하므로, 데몬 헬퍼 `.app`에서 띄울 수 있는지 S5에서 확인한다.
- 잠자기 방지(작업 중 power assertion)는 T1 범위다.

#### 3.1.7 Codex 연결 경로(S2에서 결정)
Codex hook은 trust 게이트가 있고, `-c` 주입은 실측에서 실행되지 않았다. 후보는 두 가지다.
- (a) 사용자 `~/.codex/hooks.json`에 가드된 krill hook을 넣고, **`codex app-server` RPC로 trusted_hash를 등록**한다. Orca가 해시를 자체 계산했다가 어긋난 이슈가 있었으므로 RPC를 쓴다.
- (b) Orca처럼 krill 전용 `CODEX_HOME` 오버레이를 둔다.
  - (b)는 `auth.json`을 따로 두게 되므로 토큰 갱신이 갈라질 위험이 있다. Orca도 계정별 home에 auth 사본을 둔다(실측).
- 권고는 (a)이고, 동의 UI를 함께 둔다.

### 3.2 Git 워크트리

#### 3.2.1 생성
흐름은 다음과 같다(데몬 작업 큐, 레포 단위로 직렬화).
1. **fetch**: 30초 안에 한 적이 있으면 재사용한다. 타임아웃은 60초이고, 오프라인이면 로컬 base로 진행한다(Orca `runtime-remote-fetch-controller.ts:23,25`). fetch할 때 auto-maintenance는 끈다(`-c maintenance.auto=false`, `gc.auto=0`).
2. **base 결정**: 사용자 지정 → 레포 설정 → 감지값(origin/HEAD → origin/main → origin/master → main → master) 순서다(G2).
3. **브랜치 이름**: `<prefix>/<leaf>`이고 prefix 기본값은 git user다(`branch-prefix.ts:14-26`). 이름이 겹치면 `-2`~`-100`을 붙여 본다.
4. `git worktree add --no-track -b <branch> <path> <base>`를 실행하고 `push.autoSetupRemote=true`를 켠다(`worktree-add.ts:207-229,256-260`). 삭제가 진행 중인 경로나 브랜치로는 생성하지 않는다(:201-203).
5. 메타 기록: `branch.<b>.krillBase`
6. `.worktreeinclude`: 리터럴 경로만, gitignore된 파일만, APFS clone으로 복사한다. 실패하면 일반 복사로 한다.
7. 공유 디렉터리: 사용자가 지정한 것만 심볼릭 링크로 연결한다.
8. setup 스크립트: 첫 터미널 탭에 출력을 보여 주며 실행하고 120초 타임아웃을 건다. **내용 해시 기반으로 신뢰를 묻는다.**
9. 에이전트 시작(옵션: setup이 끝날 때까지 대기)

- **경로**: 기본은 `~/Minikrill/worktrees/<repo>/<name>`이고 레포별로 바꿀 수 있다(Orca 기본값은 `~/orca/workspaces/<repo>/<name>`, `src/main/ipc/worktree-logic.ts:118-131`, `src/shared/constants.ts:104-108`).
- **성능**: 36k 파일 레포에서 checkout I/O가 약 3초다(실측). 이 시간은 라이브러리를 바꿔도 줄지 않는다. 그래서 진행률과 취소를 제공한다.
  - Orca식 투기적 사전 준비(G3)는 비용을 앞당길 뿐이고 디스크와 I/O를 더 쓰므로 M4 P2로 미룬다.
- **메모리**: `worktree add`를 하는 git 자식 프로세스의 최대 RSS가 약 158MB다(실측). 상주가 아닌 **일시 피크**이므로 메모리 예산은 "상주 RSS 합계"로 정의하고, 일시 자식 프로세스는 따로 보고한다.

#### 3.2.2 삭제·정리
- 순서: dirty 검사(`status --porcelain -uall`) → archive 스크립트 → `worktree remove`(타임아웃 없이 진행 표시, 동시 2개) → `branch -d`
  - merge되지 않은 브랜치는 남기고 알린다.
  - squash-merge 증명으로 강제 삭제하는 Orca 규칙(G4)은 P2로 둔다.
- locked 워크트리는 삭제를 거부한다(`src/shared/worktree/removal.ts:79-91`).
- 디렉터리가 사라진 등록은 `git worktree prune`으로 정리한다. **prune은 locked 등록을 남기므로** 이후에 따로 검증한다(`local-worktree-removal-recovery.ts:83-90`).
- 같은 레포의 작업은 직렬로 처리해 `packed-refs.lock` 경합을 피한다.

#### 3.2.3 변경 파일·diff 표시(앱 쪽)
- **감시**: `notify` 8.2.0(FSEvents)으로 워크트리를 재귀 감시한다.
  - 제외 대상은 `.git`, `node_modules`, `target`, `dist`, `build`, `.next`, `.venv`다. Orca 목록은 `filesystem-watcher-ignore.ts:5-22`.
  - git common dir의 `HEAD`, `index`, `packed-refs`, `config`, `logs/HEAD`, `worktrees/`는 얕게 감시한다.
  - 이벤트 유실에 대비해 60초 안전 폴링을 하고, 창이 숨겨지면 정지한다.
  - 디바운스는 125~250ms, 최소 간격은 3초다(Orca `useGitStatusPolling.ts:26-37`).
  - 감시기가 패닉해도 앱이 죽지 않게 별도 스레드에서 돌리고 재시작한다(Orca는 자식 프로세스로 격리, G11).
- **status**: `status --porcelain=v2 --branch -z --untracked-files=all`을 스트림으로 파싱하고 1000개에서 멈춘다.
- **numstat**: `-c diff.autoRefreshIndex=false diff -z --numstat -M`, 2분 캐시
- **diff 본문**은 양쪽 전체 내용으로 만든다.
  - index 쪽은 `git show :<path>`, 커밋 쪽은 `git show <oid>:<path>`, 작업 트리 쪽은 파일 읽기로 얻는다.
  - 상한은 10MB, 바이너리 판정은 앞 8KB 안의 NUL 바이트다.
  - 렌더 상한은 120,000줄 또는 600만 자다(Orca `file-diff.ts:32`, `git-show-max-bytes.ts:1`, `binary-buffer.ts:2-12`, `large-diff-render-limit.ts:1-2`).
  - 렌더링 자체는 T3 몫이다.
- **submodule**: `.gitmodules`를 읽어 gitlink로 표시만 한다. `submodule update`는 setup 스크립트에 맡긴다.
- **LFS**: 따로 처리하지 않고 git filter에 맡긴다(Orca도 같음).

#### 3.2.4 PR 연동(gh)
- MVP(M4)에서 하는 것:
  - 상태 배지: `gh pr list --head <b> --state all --limit 1 --json state,statusCheckRollup,reviewDecision,url`
  - "PR 만들기": `gh pr create --web` 또는 `--fill --body-file`
- 요청 예산은 5분에 20회, 최소 간격 10초다(`pr-refresh-pacing.ts:3-7`).
- `gh`가 없거나 로그인하지 않았으면 배지를 숨기고 안내만 한다.
- merge·checks 상세는 P2, GitLab(glab)은 P3이다. Bitbucket·Gitea·Azure는 REST API 토큰 관리가 필요하므로 범위 밖이다.

### 3.3 CLI·자동화(`krill`)

- **바이너리**: 앱 번들의 `Contents/MacOS/krill` 하나에 서브커맨드로 `hook`, `term`, `worktree`, `agent`, `daemon`을 둔다. clap 4.6.7을 쓴다.
  - 호출당 RSS 목표는 10MB 미만이다. 시제품은 2.2MB였고, clap과 serde를 넣으면 늘어난다(추정).
- **접속**: 데몬 유닉스 소켓에 직접 붙는다. 데몬이 없으면 T2의 launchd 기동 경로로 깨운다. 앱은 필요 없다.
- **명령(MVP, M2)**:
```
krill worktree create --repo <path> [--name] [--base <ref>] [--agent <profile>] [--prompt <text>] [--setup run|skip] [--json]
krill worktree list|rm [--force] [--json]
krill term list [--worktree <id>] [--json]
krill term read  <id> [--since <seq>] [--screen | --tail N]      # T2 링버퍼 seq 커서
krill term send  <id> (--text <s> [--enter] | --key ctrl-c|esc)
krill term wait  <id> --for exit|idle|waiting|blocked|pattern:<regex>|quiet:<ms> [--timeout 300s]
krill agent status [--json]
krill daemon status|stop [--kill-sessions]
krill hook <agent>                                                  # 내부용
```
- **계약**:
  - `--json` 출력에 `schema_version`을 넣는다.
  - 종료 코드: 0 성공, 2 타임아웃, 3 blocked·waiting 상태로 끝남, 4 데몬 없음, 5 권한 거부.
- **wait 의미**: Orca의 tui-idle과 같이 **blocked를 먼저 판정**하고, 그다음 hook 증거, 마지막으로 3초 정적을 본다(C4). 타임아웃은 idle·waiting·pattern·quiet에만 걸고 exit는 무제한이다.
  - pattern 대기는 Orca에 없는 기능이다. 정규식은 `regex` 크레이트로 처리해 백트래킹 폭주가 없다.
- **권한**: 에이전트 환경에 주입한 `KRILL_TOKEN`은 **pane 범위 토큰**이다.
  - 허용: 자기 pane의 hook 전송·read·send, 자기 워크트리의 하위 워크트리 생성
  - 다른 pane에 send하려면 사용자가 "감독 권한"을 부여해야 한다(코디네이터 에이전트 용도).
- **Orca 호환**: 하지 않는다(D11). 대신 `krill term wait --for waiting`과 `krill term send`로 "코디네이터가 워커를 감독"하는 최소 패턴을 지원한다.
  - **에이전트용 사용 설명(SKILL.md / AGENTS 스니펫)을 함께 배포**해 에이전트가 CLI를 스스로 익히게 한다. Orca도 `skills/`, `skill-guides/`로 CLI 사용법을 배포한다.
  - orchestration(ask/dispatch/gate)은 M6 P3에서 재평가한다.
- **PATH 설치**: 기본은 `~/.local/bin/krill` 심링크다(관리자 권한 불필요). Homebrew cask의 `binary`로도 설치된다. Minikrill이 띄운 터미널에는 자동으로 PATH에 넣는다.

### 3.4 상용 운영

#### 3.4.1 코드서명·공증
- **Orca 실측**:
  - `Developer ID Application: Lovecast LLC (6CX3WHS9HZ)`, `flags=0x10000(runtime)`, arm64 thin, `Notarization Ticket=stapled`
  - `spctl`: `accepted, source=Notarized Developer ID`
  - 번들 크기 642MB
  - 엔타이틀먼트 9개(JIT·unsigned memory·dyld env 포함, `resources/build/entitlements.mac.plist:5-21`)는 Electron 때문에 필요한 것이다.
  - 개발 채널까지 공증한다. TCC가 팀 ID와 번들 ID에 묶여 있어서다(`config/electron-builder.config.cjs:630-636`).
- **Minikrill**:
  - Apple Developer Program($99/년)에 가입하고 Developer ID Application 인증서를 쓴다.
  - `codesign --options runtime --timestamp`로 서명하고 **엔타이틀먼트는 비운다.**
  - 서명은 안쪽부터 바깥쪽 순서로 한다: `krill` → 데몬 헬퍼 `.app` → 앱. 데몬 헬퍼는 앱과 같은 identifier + 팀으로 서명한다(D13, 검증은 S4).
  - 공증은 `xcrun notarytool submit --key <p8> --key-id --issuer --wait`로 하고 `xcrun stapler staple`을 붙인다. API 키 옵션은 로컬 `notarytool --help`에서 확인했다. DMG도 공증한다.
  - 아키텍처는 arm64를 우선한다. x64가 필요하면 `lipo`로 universal을 만든다. Orca는 Electron 때문에 universal을 거부하지만 Rust 바이너리는 제약이 없다. 크기 증가폭은 추정이다.
  - 개발용 빌드도 고정 인증서(Developer ID 또는 Apple Development)로 서명한다. 그래야 TCC 프롬프트가 반복되지 않는다.

#### 3.4.2 자동 업데이트와 데몬 무중단
- **Orca 방식**:
  - electron-updater 6.8.10이 GitHub `releases/latest/download`에서 업데이트를 받는다(`updater-setup.ts:164`).
  - 24시간마다 확인하고, 실패하면 1시간부터 6시간까지 백오프한다.
  - `autoDownload=false`, 채널 5종(`release-channel.ts:3`), `nudge.json` 롤아웃 플래그를 쓴다.
  - 업데이트 직전 정리에 2.5초를 준다(`updater-state.ts:14-15`).
  - 데몬은 끄지 않는다(O5).
- **Minikrill 업데이터(D14)**:
  1. `latest.json`에 다음을 담는다: `version`, `url`, `sha256`, `ed25519_sig`, `min_daemon_protocol`, `channel`, `rollout_pct`
  2. 다운로드 → ed25519 서명 검증 → `codesign --verify --strict --deep -R "=<designated requirement>"`
  3. `ditto`로 `.app.staging`에 풀고 같은 볼륨에서 rename으로 교체한다. 이전 번들은 `Minikrill.app.prev`로 1개 보존한다.
  4. 재실행 후 30초 안에 앱이 정상 신호를 보내지 않으면 **롤백 대화상자**를 띄운다. Orca에는 이런 장치가 없다(미확인 범위).
  5. **데몬은 업데이터가 건드리지 않는다.** 새 앱이 기존 데몬 세대와 협상하고 새 세션은 새 세대에서 연다. 구 세대는 세션 0 + 60초 뒤에 종료한다(D15).
     - 데몬 헬퍼 복사본과 launchd 라벨 체계는 T2가 정한다. Orca 방식은 CDHash별 복사본에 프로필+프로토콜 라벨이고, `lsof +D`로 사용 중이 아님을 확인한 뒤 정리한다.
  6. 채널은 stable, beta 두 개로 시작한다.
- **대안**:
  - Velopack 1.2.161: `--signAppIdentity`, `--notaryProfile` 지원을 확인했다. 델타가 필요해지면 검토한다.
  - Sparkle 2.10.0(FFI): 검증된 UX와 권한 상승 설치가 필요하면 검토한다.
  - self_update 1.3.0: 단일 바이너리용이라 .app 교체에는 부족하다.
  - cargo-packager-updater: 15개월째 릴리스가 없어 제외한다.
  - dist(cargo-dist, crates.io 0.32.0 / GitHub 0.33.0) + axoupdater 0.10.2: CLI 배포용이다.
- 근거: 데몬 분리 때문에 업데이트 오케스트레이션은 직접 짜야 한다. 남는 다운로드·검증·교체는 수백 줄 수준이다(추정).

#### 3.4.3 크래시 리포트·로그
- **Orca**:
  - Sentry를 쓰지 않는다.
  - Electron crashReporter를 `uploadToServer:false`로 두고 로컬 minidump만 남긴다(128MiB / 64개).
  - 사용자가 보내는 진단 번들은 4MiB이고 리댁션을 거친다(`crash-reporting/crashpad-capture.ts:32-35,71`, `observability/redactor.ts`).
  - 로그는 `userData/logs` 아래에 둔다. main trace가 10MB × 10개, daemon.log가 5MB × 2개다. **실측 합계는 97MB**다.
- **Minikrill**:
  - **panic hook**: 메시지·백트레이스·버전·OS를 `logs/crash-<ts>.txt`에 쓴다. 다음 실행 때 발견하면 "진단 보내기" 대화상자를 띄운다.
  - **데몬**: `panic = "unwind"`를 유지하고 세션별 작업을 `catch_unwind`나 tokio task로 격리해, panic 하나가 전체 세션을 죽이지 않게 한다. 재기동은 60초에 5회로 제한한다.
  - **업로드**: opt-in이다. `sentry` 0.49.3에 `minidump` 기능(프로세스 밖 수집)을 켜되 **앱에만** 적용한다. 데몬 메모리 증가를 피하기 위해서다(추정 수 MB).
  - **로그**: `tracing` + 순환 파일, 전체 상한 20MB로 둔다. `~/Library/Logs/Minikrill`에 두어 Console.app에서 볼 수 있게 한다.
  - **리댁션 규칙**: `sk-ant-`, `sk-`, `ghp_`, `github_pat_`, PEM, `.env` 형태의 `KEY=VALUE`, 홈 경로의 사용자명

#### 3.4.4 텔레메트리(옵트인)
- 첫 실행 때 묻고 기본값은 꺼짐이다. `DO_NOT_TRACK`, `KRILL_TELEMETRY_DISABLED`, CI 환경 변수를 존중한다(Orca `consent.ts:78-88`과 같은 범주).
- 이벤트는 10개 이하로 시작한다: `app_opened`, `daemon_reattached`(복구 성공 여부, 지연 시간), `agent_status_source`(L1~L4 비율), `agent_waiting_missed`(사용자가 수동으로 정정한 횟수), `crash_detected`, `update_result`, `memory_sample`(앱·데몬 RSS 구간).
- 스키마는 serde `deny_unknown_fields` 같은 엄격 구조체로 고정한다. 프롬프트, 경로, 명령 문자열, 레포 이름은 보내지 않는다.
- 전송은 `posthog-rs` 0.27.0이나 HTTPS POST 직접 호출로 한다. geoip는 끈다(Orca `client.ts:108` `disableGeoip`).

#### 3.4.5 라이선스·결제 옵션
- **Orca**: MIT(`LICENSE:1-3`, Lovecast Inc.). 앱 안에 결제 기능이 없다. 클라우드 계정(OAuth PKCE)과 capability 플래그(`src/shared/orca-profiles.ts:35`)가 앞으로 유료 기능을 걸 자리다.
- **시장 가격**: Conductor Pro $50/월, Superset Pro $20(ELv2), Warp Build $20·Max $200, Zed Pro $10.
- **결제·라이선스 서비스**:
  - Keygen: Ed25519 오프라인 라이선스 파일, Dev 티어는 활성 사용자 100명까지 무료
  - Lemon Squeezy: 5% + 50¢, License API는 온라인 검증만
  - Paddle: 5% + 50¢
- **옵션**:
  - (A) MIT 무료 + 후원
  - (B) FSL/ELv2 소스 공개 + Pro 일회성 또는 구독(Keygen 오프라인 라이선스 + LS·Paddle MoR)
  - (C) 무료 코어 + 유료 팀·클라우드 기능
- **권고**: 결정은 M5(1.0 전)에 한다. 지금은 `capabilities` 구조체 하나와 ed25519 라이선스 파일 검증 자리만 둔다.
- Orca 코드나 문구를 차용하면 `THIRD_PARTY_NOTICES`에 MIT 저작권 고지를 넣는다.

#### 3.4.6 보안
1. **소켓**: `~/Library/Application Support/Minikrill/run/`(0700) 아래에 소켓(0600)을 둔다.
   - sun_path 길이를 넘으면 `/tmp/.krill-<uid>`로 옮기되 소유자·모드를 검증하고 심링크를 거부한다(Orca `daemon-socket-endpoint-path.ts:18,50-51`).
   - 연결마다 `peer_cred().uid()`가 내 uid인지 확인한다.
   - 토큰은 `subtle::ConstantTimeEq`로 비교한다.
2. **토큰 범위**: 앱 토큰(전체 권한)과 pane 토큰(3.3)을 나눈다. 데몬 소켓은 "키 입력 주입 = 임의 코드 실행"이므로 **최고 등급 자산**으로 다룬다.
3. **자식 환경 정리**: 에이전트 자식 환경에서 앱 토큰과 내부 변수를 지운다(Orca `spawn-environment.ts:190-206` 취지). `--settings` 파일과 launchd plist는 0600으로 만들고, plist는 bootstrap 직후 삭제한다(Orca `macos-daemon-launchd.ts:146-151`).
4. **스크롤백**(T2): 디스크에 저장한다면 0600으로 하고, 보존 기간과 "세션 기록 지우기"를 제공한다. 진단 번들에는 3.4.3의 리댁션을 적용한다.
5. **비밀 저장**: `keyring` 4.2.0(+ `apple-native-keyring-store`) 또는 `security-framework` 3.7.0을 쓰고 **평문 fallback은 두지 않는다.** Orca는 평문으로 떨어질 때 경고만 한다.
6. **에이전트 권한**: 기본은 각 에이전트의 기본 권한 모드다. YOLO(`--dangerously-skip-permissions`, Codex `--dangerously-bypass-approvals-and-sandbox`)는 프로필별 opt-in으로만 켜고, 켜진 pane에는 경고 배지를 단다. setup 스크립트는 내용 해시로 신뢰를 묻는다.
7. **hook 무결성**: `krill hook`은 권한 결정 JSON을 출력하지 않는다(3.1.5-4). 전역 hook 항목은 `KRILL_PANE_ID` 가드를 둔다.
8. **업데이트 무결성**: ed25519와 Apple 서명을 둘 다 검증한다. 서명 키는 CI 비밀과 오프라인 백업으로 이원화한다.

#### 3.4.7 테스트 전략
- **Orca 규모(참고)**: `*.test.ts*` 13,248개, Playwright e2e 498개(터미널·PTY 148개), 골든 e2e `golden-quit-relaunch-session`이 있다. 업데이트·크래시 생존 e2e는 **Windows 쪽만** 있다.

| 계층 | 도구(확인 버전) | 대상 | 우선 |
|---|---|---|---|
| 단위 | `cargo-nextest` 0.9.148 | hook JSON → 상태 정규화, 프로토콜 직렬화, 브랜치 이름, 설정 파싱, porcelain v2 파서 | P0 |
| 스냅샷 | `insta` 1.49.0 | 이벤트 시퀀스 → 상태 전이, CLI `--json` 출력 | P0 |
| 데몬 통합 | 실제 소켓 + `fake-agent`(정해진 출력·제목·hook 호출) | 재연결, seq 증분 읽기, uid·토큰·pane 범위 거부, wait 모드 | P0 |
| PTY e2e | `expectrl` 0.9.0(2026-05 이후 활동 없음, 대안으로 portable-pty 직접 구동) | zsh/bash 셸 준비 감지, 입력 전달, 종료 감지 | P1 |
| 에이전트 골든 | 실제 Claude·Codex PTY 녹화 재생(Orca `config/scripts/capture-agent-pty-transcript.mjs` 개념) | 제목 글리프, 화면 규칙(L3·L4) 회귀 | P1 |
| **생존 e2e** | macOS 스크립트 | ① 앱 `kill -9` → 재실행 → 세션·출력 연속 ② vN → vN+1 업데이트 → 세션 유지 + 구 데몬 은퇴 ③ 데몬 크래시 → "재개" 제안 | **P0** |
| 메모리 회귀 | 터미널 10개 + 가짜 에이전트 1MB/s 출력 10분 → `ps -o rss`·`footprint` 합계 | 앱+데몬 상주 ≤ 200MB | P0(측정) / P1(게이트) |
| UI 스냅샷 | 오프스크린 렌더 → PNG 비교(T3) | 셀 렌더링 | P2 |
| 실제 에이전트 스모크 | `claude -p --model haiku --settings`(이번 스파이크 형태), 주 1회 | hook 스키마 변경 조기 감지 | P1 |

#### 3.4.8 CI(GitHub Actions)
- **러너**: arm64 `macos-14`·`macos-15`·`macos-26`(`macos-latest`=26), Intel `macos-15-intel`·`macos-26-intel`.
- **비용**: 공개 레포의 표준 러너는 무료다. 비공개 레포는 macOS $0.062/분, Linux 2코어 $0.006/분이다.
- **구성**:
  - PR: Linux에서 `fmt`, `clippy -D warnings`, nextest(플랫폼 독립 로직), `cargo deny`(라이선스·취약점). `macos-15`에서 데몬 통합·PTY e2e를 돌리되 경로 필터로 제한한다.
  - nightly(macOS): 생존 e2e, 메모리 게이트, 실제 에이전트 스모크(API 비용이 들므로 주 1회)
  - 릴리스: 태그 → macOS 빌드 → 안에서 바깥 순서로 서명 → 공증·staple → DMG 공증 → `latest.json` 서명 → Release 초안 → 설치 스모크(새 사용자로 spctl·실행) → 공개 → tap PR(Orca `homebrew-bump.yml` 방식)
- **비밀**: `MAC_CERT_P12`, `MAC_CERT_PASSWORD`, `ASC_API_KEY_P8`, `ASC_KEY_ID`, `ASC_ISSUER_ID`, `UPDATE_ED25519_KEY`. 릴리스 잡은 environment 보호와 승인을 거친다.

#### 3.4.9 배포 채널
- DMG(GitHub Releases)와 자체 tap `brew install --cask <owner>/krill/minikrill`로 배포한다. cask 구성은 다음과 같다.
  - `auto_updates true`
  - `binary "#{appdir}/Minikrill.app/Contents/MacOS/krill"`
  - `zap trash: ["~/Library/Application Support/Minikrill", "~/Library/Logs/Minikrill", …]`
  - `uninstall launchctl:`로 데몬 라벨을 정리한다.
- 공식 homebrew-cask는 Gatekeeper 통과가 필수다(`:fails_gatekeeper_check`, 미서명 cask는 2026-09에 비활성화됨). 인지도 기준을 채우면 등재한다.
- beta 채널은 별도 cask(`minikrill@beta`)로 한다(Orca `orca@rc.rb`와 같은 방식).

#### 3.4.10 문서·온보딩
- **첫 실행 체크리스트**
  1. git과 Xcode CLT 확인(`worktree add --no-track`, `status --porcelain=v2`를 지원하는 버전인지)
  2. 에이전트 감지(`claude`, `codex`, `opencode`, `cursor-agent`, `gemini`)와 버전
  3. 에이전트별 상태 감지 연결: Claude는 동의가 필요 없다(세션 주입). 나머지는 diff를 보여 주고 동의를 받는다.
  4. 텔레메트리 opt-in
  5. `krill` CLI 설치와 에이전트용 SKILL.md 설치 제안
- **문서**
  - "앱을 닫아도 세션이 살아 있는 이유와 완전히 끄는 법": `krill daemon stop --kill-sessions`, 메뉴의 "종료하고 세션 끄기". Orca는 설정 → Manage Sessions에만 이 기능이 있다(`ManageSessionsTable.tsx:69,96`).
  - `krill.toml` 레퍼런스
  - 에이전트별 감지 신뢰도 표
  - 보안 모델
  - 업데이트·롤백
  - 문제 해결(`krill doctor`: 소켓·권한·hook 연결·git 버전 점검)

---

## 4. 위험과 대응

| 위험 | 영향 | 대응 |
|---|---|---|
| 에이전트 CLI가 hook 스키마나 제목 글리프를 자주 바꿈. Claude는 모르는 이벤트가 있으면 설정을 거부한다 | 상태 오판, 최악에는 **에이전트 실행 실패** | 버전별 이벤트 게이트, 주 1회 실제 에이전트 스모크, 녹화 골든 테스트, 프로필·규칙을 앱 업데이트 없이 받는 경로(P2, Orca 4시간 핫 업데이트 참고) |
| Codex hook trust 게이트 / auth 분리 | Codex 상태 감지 불가 또는 로그인이 갈라짐 | S2에서 app-server RPC로 trust를 등록하는 경로를 검증한다. 실패하면 L2·L3 추정 모드로 떨어뜨리고 표시한다 |
| Cursor CLI hook이 부분적으로만 동작(포럼 보고), Cursor는 권한 대기 이벤트가 없음 | 대기 알림 누락 | L4 화면 규칙 + "추정" 배지, 정확도 수치를 문서화한다 |
| 데몬 소켓 탈취(같은 uid 악성 프로세스) | 모든 세션에 키 입력 주입 | 같은 uid 공격은 원천적으로 막을 수 없음을 문서화한다. 토큰 파일 0600, pane 범위 토큰, 감독 권한 명시 부여, 감사 로그로 완화한다 |
| hook 지연이 에이전트를 느리게 함 | 응답 지연 | 실측 p95 3.9ms. 소켓 쓰기 타임아웃 200ms, 관리 hook 타임아웃 5초(Orca 10초보다 짧게) |
| 데몬 헬퍼 서명과 TCC(폴더 접근이 데몬에 귀속) | 반복 권한 프롬프트, 업데이트 뒤 권한 거부 | 앱과 같은 identifier로 서명하고, 업데이터가 건드리지 않는 복사본에서 실행한다(T2). S4에서 검증한다 |
| 자체 업데이터 버그 | 사용자 고립 | 서명 이중 검증, 이전 번들 보존과 롤백 대화상자, 업데이트 생존 e2e, Homebrew를 대체 경로로 둔다 |
| 구 데몬 누적(Orca 실측 사례) | 메모리 예산 초과 | 세션 0이면 자동 은퇴, 생존 e2e에서 `pgrep` 개수를 확인한다 |
| 대형 레포 워크트리 생성·삭제가 수 초~수십 초 걸리고 git 자식 RSS 피크가 약 158MB | UI 멈춤, 일시 메모리 피크 | 데몬 작업 큐에서 비동기로 처리하고 진행 표시·취소를 둔다. 예산은 상주 기준으로 정의하고 피크는 따로 보고한다 |
| git 파싱이나 감시기 버그로 데몬이 크래시 | 모든 세션 손실 | 조회와 파싱은 앱에서 한다(D8). 데몬은 git 프로세스 실행·대기만 한다 |
| macOS CI 비용(비공개 레포일 때 Linux의 약 10배) | 비용 | 경로 필터, nightly 집중, 플랫폼 독립 테스트는 Linux에서 |
| 스크롤백·진단에 비밀이 노출 | 보안 사고 | 0600, 보존 기간, 삭제 UI, 리댁션 |
| 결제 결정 지연 | 상용화 지연 | capabilities 자리만 먼저 반영하고 M5에서 결정한다 |
| 의존 크레이트 정체(expectrl, portable-pty 20개월, apple-codesign 23개월) | 버그 수정 지연 | 핵심 경로에 넣지 않거나 얇게 감싼다. 문제가 생기면 포크하거나 직접 구현한다 |

---

## 5. 검증 스파이크

| ID | 목적 | 방법 | 합격 기준 |
|---|---|---|---|
| S1 | Claude 세션별 hook 주입 확정(일부 완료) | 3.1.2 실험을 확장한다. ① 사용자 전역 hook이 있을 때 `--settings` hook과 **둘 다** 실행되는지 확인 ② 모르는 이벤트를 넣었을 때 동작 ③ 대화형 모드에서 PermissionRequest·AskUserQuestion 발화 확인 | 세 경우 모두 결과 문서화. 사용자 hook 유지, 버전 게이트 표 작성 |
| S2 | Codex hook 연결 경로 결정 | (a) `~/.codex/hooks.json` + `codex app-server`의 `hooks/list`·`config/batchWrite`로 trust 등록 (b) `CODEX_HOME` 오버레이 + auth 공유 방식 | 전역 auth를 망가뜨리지 않고 PermissionRequest·Stop·Interrupt 수신 로그 확보, 토큰 갱신 후에도 로그인 유지 |
| S3 | `krill hook` 실전 지연·실패 모드 | serde_json + 토큰 + 재시도 3회를 넣은 실제 구현으로 200회 측정. 데몬 없음·느린 데몬·소켓 권한 오류 상황 | p95 < 10ms, 데몬이 없어도 50ms 안에 `{}` 출력 후 종료 0 |
| S4 | Rust 데몬 헬퍼 서명과 TCC 상속(T2 공동) | 앱과 같은 identifier+팀으로 서명한 헬퍼 `.app`을 launchd로 실행해 `~/Documents` 접근 → 앱 업데이트(번들 교체) → 다시 접근 | 업데이트 뒤 권한 프롬프트·거부 0회, `codesign --verify --strict` 통과 |
| S5 | 알림 | `objc2-user-notifications`로 앱과 헬퍼에서 각각 알림. 서명/미서명 빌드 비교 | 서명 빌드에서 알림이 표시되고 클릭하면 해당 pane으로 이동. 헬퍼에서 가능한지 결론 |
| S6 | 업데이터 방식 확정 | 자체 업데이터 시제품(ed25519 + ditto + rename + 롤백)과 Velopack으로 같은 앱(헬퍼 포함) 업데이트 | 업데이트 뒤 실행 중 세션 유지, 위조 패키지 거부, 롤백 동작. 코드량과 유지 비용 비교 문서 |
| S7 | 메모리 측정 하네스 | 10터미널 + fake-agent 1MB/s 10분, `footprint`·`ps` 수집 스크립트 | 반복 측정 편차 5% 이내, CI 아티팩트로 저장 |
| S8 | 대형 레포 git 경로 | 36k 파일 레포 10개 워크트리를 동시 감시(FSEvents) + status 폭주 시나리오 | 저장 후 2초 안에 목록 갱신, 창이 숨으면 git 호출 0회, 앱 상주 RSS 증가 < 10MB |

---

## 6. 미확인 항목

1. **Orca `orchestration check` 실행 시 89.7MB**: 메시지가 오가는 명령은 실행하지 않았다. `--version`이 약 71MiB인 것만 확인했다.
2. **Codex `-c` hook이 실행되지 않은 원인**: trust 게이트 때문인지 미지원인지 구분하지 못했다. S2에서 확인한다.
3. **Cursor CLI, OpenCode, Gemini CLI 실제 발화**: 이 맥에 설치돼 있지 않아 문서와 포럼만 확인했다.
4. **Claude가 `--settings`에 든 모르는 hook 이벤트로 설정을 거부하는지**: Orca 주석(`claude-managed-hook-events.ts:97-116`)만 근거다. 직접 시험하지 않았다(S1).
5. **사용자 전역 hook과 `--settings` hook의 병합**: 프로젝트 hook과의 병합은 실측했다. 전역(user) 범위는 문서 원문만 근거다. 실험할 때 Orca 전역 hook이 `ORCA_*` 환경 없이 no-op이라 관찰할 수 없었다.
6. **Rust 헬퍼가 같은 identifier 서명으로 TCC를 물려받는지**: Orca 근거는 Node 헬퍼 서명 스파이크(PR #25848)뿐이다(S4).
7. **UN 알림이 서명 안 된 개발 빌드나 헬퍼에서 뜨는지**(S5).
8. **velopack이 "앱 + 헬퍼 .app + 데몬 복사본" 구조에서 문제없는지**: 서명·공증 플래그가 있다는 것만 확인했다(S6).
9. **실제 `krill` CLI(clap·serde 포함)의 호출당 RSS**: std만 쓴 시제품 2.2MB만 실측했다.
10. **Orca 구 데몬 누적이 의도된 것인지**: 은퇴 경로가 데스크톱에서 호출되지 않는다는 것만 확인했다. 관련 주석이나 이슈는 찾지 못했다.
11. **Orca 메모리·성능 e2e 26개**: 파일명 기준으로 16개만 확인했다.
12. **Apple notarization 문서 원문**: 페이지 텍스트를 추출하지 못했다. 로컬 `notarytool --help`로 API 키 옵션만 확인했다.
13. **Keygen Std 요금**: 페이지에서 로드되지 않았다.
14. **sentry `minidump` 기능을 켰을 때 앱 메모리 증가량**: 추정이다.
15. **Claude `Notification`(`permission_prompt`·`elicitation_dialog`)과 PermissionRequest의 발화 순서·중복 여부**: 대화형으로 실측하지 않았다(S1).
16. **x64 universal 빌드의 크기 증가폭**: 추정이다.
17. **hardened runtime 데몬이 띄운 셸이 사용자의 `DYLD_*` 등 환경 변수를 그대로 받는지**: 확인하지 않았다(S4에 포함).

---

## 7. Minikrill 이슈 후보

마일스톤 후보는 T1 초안의 M1~M4 구분을 따르고 둘을 더했다.
- M1 코어: 데몬·세션 생존, 보안·측정 기반, 스파이크
- M2 병렬: 상태·워크트리·CLI·알림
- M3 출시 준비: 서명·공증·업데이트·크래시·배포
- M4 v1.x
- M5 상용화: 라이선스·결제
- M6 확장: orchestration 등

| # | 제목 | 마일스톤 | 우선 | 완료 조건 | 선행 |
|---|---|---|---|---|---|
| T4-00 | CLI 이름 확정(`krill` vs `mk`)과 명령 체계 문서 | M1 | P0 | 트랙 통합 문서에 이름과 명령 트리 1안 확정 | — |
| T4-01 | 스파이크 S1: Claude `--settings` 주입·전역 병합·버전 게이트 | M1 | P0 | S1 합격 기준 결과 문서, 이벤트 게이트 표 | — |
| T4-02 | 스파이크 S2: Codex hook 연결 경로(app-server trust vs CODEX_HOME) | M1 | P0 | 전역 auth 무손상 상태에서 PermissionRequest·Stop 수신 로그 | — |
| T4-03 | 스파이크 S4: 데몬 헬퍼 서명·TCC 상속(T2 공동) | M1 | P0 | 업데이트 후 권한 거부 0회 기록 | — |
| T4-04 | 스파이크 S7: 메모리 측정 하네스(10터미널 부하) | M1 | P0 | 스크립트 결과가 CI 아티팩트로 남고 반복 편차 5% 이내 | — |
| T4-05 | 소켓 보안: 0700/0600, peer uid, 상수 시간 토큰, pane 범위 토큰 | M1 | P0 | 다른 uid·잘못된 토큰·범위 밖 pane 요청 거부 통합 테스트 통과 | T2 소켓 |
| T4-06 | 생존 e2e ①: 앱 kill -9 → 재실행 → 세션·출력 연속(macOS nightly) | M1 | P0 | macos 러너에서 7일 연속 통과 | T2 재부착 |
| T4-07 | 에이전트 프로필 스키마(TOML)와 내장 5종 + 버전 감지 | M2 | P0 | `krill agent list`가 PATH 감지·버전·프로필 출력, 단위 테스트 통과 | T4-00 |
| T4-08 | `krill hook <agent>`: `{}` 선출력, 소켓 전달, 재시도 3회, fail-open | M2 | P0 | S3 기준 p95 < 10ms, 데몬이 없어도 50ms 안에 종료 0 | T4-05 |
| T4-09 | 데몬 상태 저장소 + 정규화(6상태 + 출처·신뢰도) | M2 | P0 | Claude·Codex 이벤트 시퀀스 insta 스냅샷 통과, 앱 재시작 후 `krill agent status` 동일 | T4-08 |
| T4-10 | Claude 세션별 `--settings` 주입 + `--session-id` 사전 발급 | M2 | P0 | 사용자 `~/.claude/settings.json` 바이트 무변경 상태에서 waiting·done 수신 | T4-01, T4-09 |
| T4-11 | Codex·OpenCode·Gemini·Cursor hook 설치(동의·diff·가드·원복) | M2 | P1 | 설치→제거 후 사용자 설정 파일이 원본과 바이트 동일, 가드로 Minikrill 밖에서는 no-op | T4-02, T4-09 |
| T4-12 | 보조 감지: SIGCHLD·pgid(L2), 제목 글리프(L3), 화면 규칙(L4) + "추정" 배지 | M2 | P1 | 에이전트 kill -9 후 1초 안에 exited, 녹화 재생 골든 테스트 통과 | T4-09 |
| T4-13 | 알림·Dock 배지(앱 실행 중, UNUserNotificationCenter) | M2 | P1 | 권한 요청 1초 안에 알림, 클릭하면 해당 pane 포커스 | T4-09, S5 |
| T4-14 | git 작업 큐(레포 직렬화, 동시성 제한, 환경·옵션 고정) | M2 | P0 | 동시 요청 20개에서 git 동시 실행 ≤ 4, 같은 레포 변경 작업 직렬 확인 | — |
| T4-15 | 워크트리 생성(fetch 재사용, base 결정, 이름 충돌, include 복사, setup·신뢰 해시) | M2 | P0 | 36k 파일 레포에서 생성, `-2` 충돌 처리, setup 내용이 바뀌면 재확인 프롬프트 | T4-14 |
| T4-16 | 워크트리 삭제·정리(dirty 검사, archive, remove, `branch -d`, prune·lock 검증) | M2 | P0 | 미병합 브랜치 보존, dirty면 `--force` 없이 거부, locked 거부 테스트 | T4-14 |
| T4-17 | 변경 파일·numstat 표시(FSEvents, 메타 파일 감시, 1000개 제한, 숨김 시 정지) | M2 | P1 | S8 합격 기준 | T4-14 |
| T4-18 | `krill` CLI MVP(worktree / term list·read·send·wait / agent status / daemon) | M2 | P0 | 앱 종료 상태에서 모든 명령 동작, 호출당 RSS < 10MB 실측 | T4-05, T4-09 |
| T4-19 | `krill term wait --for idle·waiting·blocked·pattern·quiet` | M2 | P1 | fake-agent로 모드별 통합 테스트, 타임아웃이면 종료 코드 2 | T4-18 |
| T4-20 | 에이전트용 SKILL.md/AGENTS 스니펫 배포 | M2 | P2 | Claude가 스킬만 보고 `krill worktree create`와 `term wait`을 성공 | T4-18 |
| T4-21 | 에이전트 자식 환경 정리 + YOLO opt-in 배지 | M2 | P1 | 자식 `env`에 앱 토큰 없음 테스트, 위험 모드 pane에 배지 | T4-07 |
| T4-22 | 세션 재개 버튼(`claude --resume`, `codex resume`) | M2 | P1 | 데몬 강제 종료 후 같은 대화 재개 수동 검증 | T4-10 |
| T4-23 | 서명·공증 파이프라인(로컬 스크립트 + CI, 안에서 바깥 순서) | M3 | P0 | `spctl -a -vv`가 `Notarized Developer ID`, `stapler validate` 성공, DMG 공증 | T4-03 |
| T4-24 | 데몬 세대 공존 + 세션 0 구 데몬 자동 은퇴(T2 공동) | M3 | P0 | vN 데몬에 vN+1 앱이 붙고, 구 데몬은 마지막 세션 종료 60초 안에 종료(ps 확인) | T2 프로토콜 협상 |
| T4-25 | 자체 업데이터(`latest.json` + ed25519 + codesign 검증 + 원자 교체 + 롤백) | M3 | P1 | 위조 패키지 거부, 재실행 실패 시 롤백 대화상자, 세션 유지 | S6, T4-23 |
| T4-26 | 생존 e2e ②③: 업데이트 vN→vN+1 / 데몬 크래시 → 재개 제안 | M3 | P0 | macos nightly 통과, 업데이트 후 데몬 프로세스 수 = 현행 세대 + 세션 있는 구 세대 | T4-24, T4-25 |
| T4-27 | 메모리 회귀 게이트(앱+데몬 상주 ≤ 200MB, 초과 시 실패) | M3 | P1 | nightly에서 게이트가 동작하고, 의도적으로 낸 회귀를 잡아내는 것 확인 | T4-04 |
| T4-28 | 크래시: panic 로그 + 다음 실행 때 감지 + 데몬 세션 격리 + 재기동 제한 | M3 | P1 | 강제 panic 후 진단 대화상자, 세션 1개의 panic이 다른 세션을 죽이지 않음 | — |
| T4-29 | 진단 번들 + 리댁션 + opt-in 업로드(sentry, 앱만) | M3 | P2 | 번들에서 `sk-ant-`, PEM, 토큰 마스킹 테스트 | T4-28 |
| T4-30 | 로그 체계(tracing, 순환, 합계 20MB, `~/Library/Logs`) | M3 | P1 | 24시간 부하 뒤 로그 합계 ≤ 20MB | — |
| T4-31 | Homebrew 자체 tap + DMG 릴리스 워크플로 + tap 자동 PR | M3 | P1 | `brew install --cask`로 설치, `krill`이 PATH에 있음, Gatekeeper 경고 없음, `zap` 후 잔여물 없음 | T4-23 |
| T4-32 | CI: Linux 단위·clippy·cargo deny + macOS 통합·PTY e2e(경로 필터) | M1 | P1 | PR마다 자동 실행, macOS 잡은 관련 경로가 바뀔 때만 | — |
| T4-33 | 첫 실행 온보딩(git·에이전트 감지, hook 동의·diff, CLI 설치, 텔레메트리) + `krill doctor` | M3 | P1 | 새 macOS 계정에서 5단계 완료 후 Claude 상태 감지 동작, doctor가 고장 시나리오 3개 탐지 | T4-10, T4-11 |
| T4-34 | 텔레메트리 opt-in(≤10 이벤트, DO_NOT_TRACK, 엄격 스키마) | M4 | P2 | 기본 꺼짐 상태에서 네트워크 전송 0회(프록시 확인), 스키마 테스트 통과 | — |
| T4-35 | PR 상태 배지 + "PR 만들기"(gh, 페이싱) | M4 | P2 | gh 로그인 상태에서 배지 표시, 요청 ≤ 20회/5분, gh가 없으면 안내만 | T4-15 |
| T4-36 | 워크트리 투기적 사전 준비(생성 창이 열려 있을 때만) | M4 | P2 | 체감 생성 시간 < 1초(36k 파일), 미사용 준비분 5분 뒤 정리 | T4-15 |
| T4-37 | 에이전트 프로필·화면 규칙 원격 갱신(서명된 규칙 파일) | M4 | P2 | 앱 업데이트 없이 규칙이 교체되고 서명이 틀린 규칙은 거부 | T4-12 |
| T4-38 | 앱이 꺼져 있을 때 알림(헬퍼 또는 메뉴바) | M4 | P2 | 앱 종료 상태에서 권한 요청 알림 표시 | S5 |
| T4-39 | 라이선스·결제 모델 결정 + capabilities 게이트 + 오프라인 ed25519 라이선스 검증 | M5 | P2 | 결정 문서 승인, 게이트 1곳, 위조 라이선스 거부 테스트 | T4-25(서명 크레이트 공유) |
| T4-40 | orchestration 유사 기능(ask/dispatch) 필요성 재평가 | M6 | P3 | 사용 사례 3개 이상이면 설계 문서, 아니면 닫음 | T4-19 |

---

### 부록: 이번 2차 조사에서 직접 실행한 것
- 1차 주장 전수 대조: 에이전트 27 / Git 20 / CLI·운영 24 / 크레이트·웹 약 50건. `.ref/orca` 소스와 crates.io API, `gh api`, 공식 문서를 사용했다.
- 실측:
  - `claude -p --settings --session-id` hook 병합 실험(`/tmp/t4spike`)
  - `codex exec -c hooks…` 실험
  - Rust `krill hook` 시제품 200회 측정
  - `rg` RSS
  - Orca CLI `--version` RSS
  - `codesign`, `spctl`, `stapler`, `du`
  - Orca 데몬 디렉터리·프로세스(`ps`, `pgrep -P`)
  - Orca 레포를 새로 클론해 git status·worktree add/remove·`--no-checkout`·`reset --hard` 시간과 RSS 측정
  - `~/.claude/settings.json`과 Orca Codex 오버레이 `hooks.json`·`config.toml` 읽기(수정 없음)
- 임시 파일은 `/tmp/t4spike`, `/tmp/t4verify`에만 두었고 정리했다. 저장소 파일은 이 보고서 외에 고치지 않았다.
