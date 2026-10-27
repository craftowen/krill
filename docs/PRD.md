# Krill PRD v1 (2026-10-14)

## 한 줄

**어떤 AI 코딩 에이전트든 끊기지 않게 병렬로 돌리는, 180MB짜리 네이티브 맥 앱.** 앱을 꺼도, 죽어도, 업데이트해도 에이전트 세션은 살아 있고, 다시 열면 놓친 출력까지 그대로 이어진다.

## 문제

- Orca(Electron)는 이 맥미니에서 약 1.6~1.67GB를 쓴다(실측 RSS, PTY 19~20개, 4일 가동). Krill 목표(phys_footprint, 터미널 10개)와 직접 비교할 때는 같은 세션 수·창 크기·지표로 다시 잰다(MK-074 공개 벤치). 터미널 내용을 데몬·main·렌더러에 최대 3벌 들고 있다(T2-C4).
- 사용자의 가장 큰 불만은 **연결 끊김**이다. Orca는 데몬이 죽으면 전 세션이 죽고, 업데이트 뒤 빈 구세대 데몬(v40·v41)이 남으며, 앱이 닫힌 동안은 기록과 터미널 질의 응답이 비어 있다(T1 §3.1.3, T2 §3.2).

## 포지셔닝 (T1-D2)

"앱을 꺼도 에이전트가 산다"는 이미 Claude Code(`claude agents`/`daemon`), Orca, Superset, Jean, tmux 계열이 제공한다. Krill의 차별점은 아래 넷을 **수치로 증명**하는 것이다.

1. 에이전트 종류와 무관한 **PTY 수준** 지속(Claude Code·Codex·Gemini·OpenCode·셸·dev 서버).
2. 놓친 출력을 **바이트 단위로 재생**.
3. 네이티브 **앱+데몬 ≤ 180MB**(터미널 10개).
4. 데몬 크래시·업그레이드에도 세션 유지, **고아 데몬 0**.

Claude Code 1st-party 백그라운드 세션과는 경쟁하지 않고 공존한다(`claude agents --json` 연동은 M5).

## 타깃

AI 코딩 에이전트를 동시에 여러 개(5~20개) 돌리는 macOS(Apple Silicon) 개발자. Windows x64 개발자도 지원 대상이다(아래 플랫폼 절).

## 플랫폼 (2026-10-22 사용자 결정)

- 지원·릴리스 대상은 **macOS Apple Silicon(M 시리즈, arm64)**과 **Windows x64(`x86_64-pc-windows-msvc`)**다.
- Intel Mac과 Linux는 지원하지 않는다. 해당 빌드 CI·패키징·설치·릴리스 자산은 제거한다.
- Windows 네이티브 빌드·설치·ConPTY 세션 생존 스모크를 릴리스 후보에서 검증한다. Windows 10/11 데스크톱의 수동 GUI·IME 검증과 Authenticode 인증서는 별도 미확인 항목으로 유지한다.
- 기존 Unix/Linux 호환 소스는 기존 원격 세션 보존을 위해 남아 있으며 배포 지원을 의미하지 않는다. WSL2 백엔드·Windows ARM64는 보류한다.

## v1 범위 (T1-D1)

| 포함 (Must) | 제외 (v1) |
|---|---|
| 데몬이 PTY 소유 → 앱 재시작·크래시·업데이트에도 세션 생존 | 내장 브라우저, 파일 에디터(→ "Open in" VS Code·Cursor·Zed·Finder) |
| 놓친 출력 바이트 재생, 연결 상태 배너, 자동 재연결 | 모바일 앱·클라우드 relay(→ M5 알림 웹훅) |
| 워크트리 생성·삭제, 워크트리별 에이전트 병렬 실행 | 음성(STT), computer-use |
| 에이전트 상태 6종·알림·Dock 배지·잠자기 방지 | 이슈 트래커 연동, orchestration(→ M6 재평가) |
| `krill` CLI(ls/new/send/read/wait/attach/doctor), 일반 터미널에서 `krill attach` | 원격 SSH(→ M6) |
| 변경 파일 목록 + 읽기 전용 diff | 커밋 UI·줄 주석(→ M5) |
| 한글 IME 완전 지원 | 이미지 프로토콜 |

## 핵심 사용자 시나리오 (수용 기준)

1. **앱을 죽여도 산다**: Claude Code 3개·Codex 2개·dev 서버 1개가 도는 중에 앱 `kill -9` → 재실행 → 500ms 안에 6개 세션의 마지막 화면이 뜨고, 앱이 꺼져 있던 동안의 출력이 링·세그먼트 보존 범위 안에서 생략 없이 이어진다(넘치면 "N바이트 생략" 표시). 장애별 보장 범위는 ARCHITECTURE §4.1.
2. **업데이트해도 산다**: 세션 5개 상태에서 새 버전으로 업데이트 → 세션 유지, `pgrep`으로 데몬 세대 1개.
3. **병렬 작업**: 사이드바에서 워크트리 3개 생성 → 각각 에이전트 실행 → 입력 대기가 된 세션이 알림·배지·"Needs You" 맨 위로 뜬다.
4. **CLI**: 앱이 꺼진 상태에서 `krill new --worktree feat-x --agent claude` → `krill wait --for idle` → `krill read`.
5. **한글**: Claude Code 입력창에서 "안녕하세요⏎"가 유실·중복·재정렬 없이 입력된다.

## 비기능 요구 (출시 차단 기준)

`docs/ARCHITECTURE.md` §8 표를 따른다. 핵심: 메모리 ≤ 180MB(차단 200MB), 세션 생존 1,000회 손실 0, 고아 데몬 0, 자동 재연결 5초.

## 정책 결정 (확정·보류)

| 항목 | 결정 | 상태 |
|---|---|---|
| 라이선스·가격 | `Apache-2.0` 오픈소스 무료 배포 | 확정 (MK-074·MK-077) |
| Apple Developer Program 가입(연 $99) | 미가입 동안 ad-hoc 서명·소스 빌드 우선, Developer ID 서명·공증은 별도 게이트 | 보류 (MK-104) |
| krilld panic 전략 | `unwind` 유지 + 세션 단위 `catch_unwind` 격리 | 확정 (CONVENTIONS P4) |
| GitHub 원격·CI | `craftowen/krill` + GitHub Actions(저장소 검사 + macOS ARM 전체 게이트 + Windows 네이티브 빌드·스모크) | 확정 (MK-017) |
| 텔레메트리 | 별도 동의창 없이 기본 활성. 무작위 설치 ID·최초 실행·앱 실행·일별 입력 활동·버전·OS·아키텍처 집계. 명시적 비활성화·`DO_NOT_TRACK` 존중 | 2026-10-27 사용자 지시로 변경 (MK-089), [수집·운영 안내](../services/telemetry/CLIENT.md) |
| Windows 지원 범위 | 공통 컴파일 계층 + 네이티브 ConPTY 실기 스파이크 먼저, WSL2·앱만 원격은 보류 | 확정 (2026-10-18, M11) |
| Windows 코드 서명(Authenticode, 유료 인증서) | 사람이 결정 | 보류 (MK-219) |

## 문서 지도

- 아키텍처·통합 결정: `docs/ARCHITECTURE.md`
- 로드맵·이슈 목록: `docs/ROADMAP.md`
- 내부 조사 기록은 로컬에 보존하며 공개 제품 범위는 이 문서와 ARCHITECTURE·ROADMAP을 따른다.
