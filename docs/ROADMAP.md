# Krill 로드맵 v1 (2026-10-14)

트랙별 이슈 후보(T1 41개, T2 29개, T3 30개, T4 41개, 합 141개)를 중복 제거·병합해 102개로 만들었고, 교차 검증으로 선행 관계·완료 조건을 고치고 MK-103~106을 더해 106개가 되었으며, 이후 M7 Orca 패리티(MK-107~113), M8 오케스트레이션·포트·에디터(MK-114~121), M9 모바일 원격 제어(MK-122~126), M10 Orca 이관·워크벤치 확장(MK-172~187), M11 Windows 포팅(MK-188~222, 리니어 OWE-807~841과 1:1), M12 Orca 심층 패리티(MK-223~229, 리니어 OWE-918~924)를 추가했다. 출처 열의 `T2#7` 같은 값은 로컬에 보존한 내부 기획 기록의 식별자다. 우선순위 P0=긴급, P1=높음, P2=보통, P3=낮음. 목표일은 1인 + 에이전트 병렬 개발 기준 **추정**이다.

## 마일스톤

| 마일스톤 | 내용 | 목표일(추정) | 끝났다는 기준 |
|---|---|---|---|
| M1 스파이크 | 되돌리기 비싼 결정(gpui·IME, 2단 데몬, 키프레임, launchd·TCC, hook 주입)을 실험으로 확정 | 2026-11-11 | P0 스파이크 전부 합격 또는 대안 확정 |
| M2 코어 | 데몬 + 단일 창 + 세션 생존·재생 | 2026-12-23 | 앱 kill -9 1,000회 손실 0, 재생 해시 일치, 메모리 ≤ 200MB가 nightly에서 통과 |
| M3 병렬 | 워크트리·에이전트 런처·상태·알림·`krill` CLI·diff | 2027-02-10 | PRD 시나리오 1~5 수동 통과 |
| M4 출시 준비 | 서명·공증·업데이터·크래시·설정·배포·라이선스 | 2027-03-24 | ARCHITECTURE §8 차단 기준 전부 nightly 통과, Homebrew tap 설치 |
| M5 v1.x | 출시 후 1~3개월 개선 | 2027-06-30 | — |
| M6 v2 | 원격·orchestration·자동화·엔진 재평가 | 미정 | — |
| M7 Orca 패리티 | 관리자 페이지(7탭)·프로바이더 다계정/자동전환·Orca 단축키·이미지 붙여넣기·PC-to-PC SSH·실화면 QA | 2026-10-18 | MK-107~113 전부 통과 및 실화면 검증 완료 |
| M8 오케스트레이션·핵심 고도화 | 멀티 에이전트 오케스트레이션(`krill orchestration`)·스킬 레지스트리(`krill skills`)·세션 핸드오프(`krill handoff`)·워크스페이스 체크포인트/레퍼런스·리스닝 포트 감지(`krill ports`)·에디터·GUI 통합 | 2026-10-19 | MK-114~121 전부 통과 및 실화면 검증 완료 |
| M9 모바일 원격 제어 | `krilld` 모바일 기기 레지스트리·`ring` E2EE v2·지연 바인딩 게이트웨이(Tailscale/릴레이 우선·`cloudflared` 폴백)·1-스캔 QR 페어링(관리자 `Mobile` 8번째 탭 + `krill mobile`)·내장 모바일 PWA·라이브 터미널 뷰 | 2026-10-20 | MK-122~126 전부 통과, 데몬 ≤ 60MB, 실화면 검증 완료 |
| M10 Orca 이관·워크벤치 확장 | Orca 설정·다계정·원격 SSH·등록 프로젝트/워크트리 원클릭 이관(`krill import orca`)·공유 심볼릭 링크·디스크 클린업·사이드바 계보/필터·우측 파일 탐색기/전문검색·탭 고정/색상/MRU·AI Vault 검색·세션 포크·Git/PR AI 복구·Precheck 스케줄러·`krill computer` | 2026-10-25 | MK-172~187 전부 통과, 앱 ≤ 120MB·데몬 ≤ 60MB, `scripts/check.sh` 0 |
| M11 Windows 포팅 | Windows(주 타깃 `x86_64-pc-windows-msvc`) 지원: 공통 컴파일 계층(Mac에서 Windows 타깃 `cargo check` 0) → GCP Windows VM 네이티브 ConPTY 실기 스파이크(S-W1~S-W5) → 합격 시 네이티브 본구현·배포. WSL2·앱만 원격은 보류(PRD 플랫폼 절) | 미정 | MK-205 Windows check 0, 스파이크 결과표로 본구현 여부 결정, 본구현 시 MK-217 Windows 생존 e2e 손실 0, macOS `scripts/check.sh` 0 유지 |
| M12 Orca 심층 패리티 | 서브모듈·심링크·`.worktreeinclude` 글롭·Codex `SUN_LEN` 격리·훅 서브에이전트 보호·재귀 자식 워크트리 안전 삭제·스케줄 실행 상세/취소·오케스트레이션 `--question-id`/`--preamble`·AI Vault 증분 인덱싱·멀티 에이전트 Fan-Out 생성·Hunk/Line 스테이징·인앱 PR 머지 | 2026-10-19 | MK-223~229 전부 통과, `scripts/check.sh` 0 |

## 이슈 (우선순위 → 선행 순으로 진행)

| ID | 제목 | M | P | 완료 조건 | 선행 | 출처 | linear |
|---|---|---|---|---|---|---|---|
| MK-001 | Cargo 워크스페이스 골격과 크레이트 경계(krill-proto, krill-holder, krilld, krill-term, krill-app, krill) | M1 | P0 | `cargo build --workspace` 성공, 크레이트별 책임이 ARCHITECTURE §2와 일치, MIT 고지 템플릿 포함 | — | ARCH §2 | OWE-366 |
| MK-002 | [스파이크] gpui 터미널 위젯 한글 IME·렌더 품질(10개 케이스) | M1 | P0 | T3 §5 S1의 10개 케이스 결과표와 녹화 검증 후 gpui 확정 또는 AppKit+Metal 전환 결정 | MK-001, MK-003 | T3#1, T1 S5 | OWE-367 |
| MK-003 | gpui 빌드 기반: gpui-pre·모든 gpui-pre-*·gpui-component 묶음 = 고정, vendor, font-kit + runtime_shaders | M1 | P0 | Xcode 없이 `cargo build --offline --locked` 성공, hello 창이 뜨고 글자가 렌더됨(스크린샷), 업그레이드 절차 1쪽, AGENTS 명령 갱신안을 보고서에 제시 | MK-001 | T3#2 | OWE-368 |
| MK-004 | [스파이크] holder exec 핸드오프 무손실 | M1 | P0 | 세션 10개 초당 1MB 출력 중 execv 10회 연속 seq 손실 0, 종료코드 수신, 핸드오프 ≤ 50ms | MK-001 | T2#1 | OWE-369 |
| MK-005 | [스파이크] krilld kill -9 내성 + 앱 SIGKILL 1,000회 생존·바이트 재생 | M1 | P0 | krilld 100회 kill에서 SIGHUP 0·재게시 p95 ≤ 300ms(첫 크래시는 즉시 재기동, 반복 크래시만 백오프), 앱 kill 1,000회 PTY 생존 100%, 결정적 생성기 출력 해시 일치(링 안), 링 초과 시 Gap 표시 | MK-004 | T2#2, T1#2 | OWE-370 |
| MK-006 | [스파이크] alacritty grid ↔ 자체 키프레임 왕복 충실도 | M1 | P0 | 녹화본(Claude Code·Codex·vim·htop·한글·이모지) 셀 단위 100% 일치 + 임의 바이트 경계에서 스냅샷→복원→같은 접미 바이트 입력 후 상태 일치(파서 중간 상태·대체 화면·모드 포함), 퍼저 1만 건 불일치 0(제외 목록은 사람 승인) | MK-001 | T2#3 | OWE-371 |
| MK-007 | [스파이크] 서명 헬퍼 복사 + launchd 기동, TCC 상속·ProcessType·coalition·BTM | M1 | P0 | 헬퍼 고유 identifier·같은 팀 서명 + plist `AssociatedBundleIdentifiers`로 TCC 귀속 실측, 번들 교체 뒤 TCC 거부 0회, macOS 26+ 백그라운드 작업 프롬프트 발생 여부와 "허용 안 함" 시 세션 생존 기록, 리소스 엔타이틀먼트 필요 여부, Interactive 빌드 시간 차 ≤ 5%, 실패 시 SMAppService 대안 평가 | MK-001, MK-104 | T2#4, T4-03 | OWE-372 |
| MK-008 | [스파이크] 에이전트 터미널 질의 캡처와 응답 표 | M1 | P0 | Claude Code·Codex·OpenCode의 질의 목록과 응답 주체 표 확정 | MK-001 | T2#5 | OWE-373 |
| MK-009 | [스파이크] 메모리 기준선 + 측정 하네스 mk-bench | M1 | P0 | 터미널 10개 시나리오 footprint 표(120열·200열, 붙음/숨김, 창 1600×1000pt @2x, 앱+krilld+holder 합계), 앱 ≤ 120MB·데몬 ≤ 60MB 판정 또는 초과 원인·조정안, runtime_shaders 개발 빌드 기준임을 명시, 반복 편차 5% 이내(CI 연결은 MK-017 이후) | MK-002, MK-003, MK-006 | T3#3, T2#6, T4-04, T1#1, T3#14, perf-first(vte sync 버퍼 세션당 약 1.9MB 상주) | OWE-374 |
| MK-010 | [스파이크] UI 전송 방식 확정: 화면 델타 vs UI 미러, 핫/콜드 스크롤백 | M1 | P0 | 앱 재시작 후 10세션 마지막 화면 ≤ 500ms, 콜드 재생 셀 단위 일치, 결정 문서 | MK-006, MK-003 | T3#6, T2 S8, perf-first(1줄 스크롤도 전체 변경 → 행 이동 연산 필요) | OWE-375 |
| MK-011 | [스파이크] Claude --settings hook 주입·전역 병합·버전 게이트 | M1 | P0 | 사용자 전역 hook과 둘 다 실행, 모르는 이벤트 동작, PermissionRequest 발화 결과 문서 | — | T4-01 | OWE-376 |
| MK-012 | [스파이크] Codex hook 연결 경로(app-server trust vs CODEX_HOME 오버레이) | M1 | P0 | 전역 auth 무손상 상태에서 PermissionRequest·Stop 수신 로그, 토큰 갱신 후 로그인 유지 | — | T4-02 | OWE-377 |
| MK-013 | [스파이크] 대량 출력 렌더 성능 | M1 | P1 | `cat` 100MB 중 프레임 p99 ≤ 16.7ms, 입력 에코 p95 ≤ 16ms, 유휴 프레임 0 | MK-002 | T3 S3 | OWE-378 |
| MK-014 | [스파이크] 잠자기·로그아웃·사용자 전환 거동 | M1 | P1 | 깨어난 뒤 2초 안에 자동 재접속, 로그아웃 시 holder·세션 거동 기록 | MK-004 | T2 S7 | OWE-379 |
| MK-015 | [스파이크] 업데이터 방식(자체 ed25519 vs Velopack) | M1 | P1 | 두 방식으로 헬퍼 포함 앱 업데이트, 세션 유지·위조 거부·롤백, 비교 문서 | MK-007 | T4 S6 | OWE-380 |
| MK-016 | [스파이크] 알림: 앱·헬퍼, 서명·미서명 | M1 | P1 | 서명 빌드에서 알림 표시·클릭 시 pane 이동, 헬퍼 알림 가능 여부 결론 | MK-007 | T4 S5 | OWE-381 |
| MK-017 | CI: macOS 러너 clippy·nextest·PTY e2e + Linux는 krill-proto·krill-term 단위 테스트·cargo deny만 | M1 | P1 | PR마다 자동 실행, macOS 잡은 관련 경로 변경 시만 | MK-001 | T4-32 | OWE-382 |
| MK-018 | krill-holder 구현(spawn, waitpid, fd 보관, SCM_RIGHTS, 동결 내부 프로토콜 v1) | M2 | P0 | MK-004·005 기준 합격, 내부 프로토콜 계약 테스트, 줄 수 상한 macOS 빌드 1,300 + Linux 전용 300 + Windows 빌드 2,000(플랫폼별로 셈, 결정 12-2, 2026-10-19 개정 — CONVENTIONS §5) | MK-004, MK-005 | T2#7 | OWE-383 |
| MK-019 | krilld 코어: 세션별 Term(핫 1,000행), PTY 리더, 세션 단위 catch_unwind, 2026 sync 데드라인 | M2 | P0 | 패닉 주입 시 다른 세션 무영향, 종료 신호 없는 sync 스트림 150ms 뒤 반영 | MK-018, MK-006, MK-010 | T2#8, T3#4, perf-first(PTY 리더: 1024B 꽉 찬 read 뒤에만 5~20µs 재읽기) | OWE-384 |
| MK-020 | 질의 응답 권한과 재생 게이트(테마·픽셀 크기 푸시, 테마 전에는 내장 기본 테마로 즉시 응답) | M2 | P0 | 앱이 닫혀도 DA/DSR/OSC 10·11 응답(테마 미수신 시 내장 기본 테마, 마지막 테마는 디스크 저장), 재생 중 응답 0건, MK-008 응답 주체 표 16종 준수 | MK-008, MK-019 | T2#9 | OWE-385 |
| MK-021 | IPC v1: UDS 0600 + getpeereid, 프레이밍, postcard, Hello 버전·feature 협상 | M2 | P0 | 다른 UID 거부, N-1 클라이언트 호환 테스트, 1MiB 초과 프레임 거부 | MK-019 | T2#10 | OWE-386 |
| MK-022 | 소켓 게시와 단일 인스턴스(flock, 임시 bind 후 rename) | M2 | P0 | 동시 기동 100회에서 데몬 1개, 고아 소켓 0 | MK-021 | T2#11 | OWE-387 |
| MK-023 | 재접속 프로토콜 + 화면 전송(seq 이어받기, 키프레임, Resync, 지연 히스토리) | M2 | P0 | 재접속 p95 ≤ 300ms, 결정적 생성기 출력 해시 일치, 원본 바이트 seq와 화면 revision을 별도로 정의, 앱이 스크롤백을 보관하지 않음을 footprint로 확인 | MK-006, MK-010, MK-021, MK-024 | T2#12, T3#6, T1#5, T1#6 | OWE-388 |
| MK-024 | 세그먼트 저장소(zstd, append-only, 토큰 버킷, gap, 보존·GC, 0600, .noindex) + 콜드 스크롤백 페이지 재생 | M2 | P0 | 출력 폭주 10분 동안 지속 ≤ 8MB/분, 크래시 뒤 재구성 테스트, 1만 줄 너머 스크롤 원본 일치 | MK-019 | T2#13, T3#5 | OWE-389 |
| MK-025 | 앱·CLI의 데몬 자동 기동(개발 빌드: posix_spawn+setsid, launchd plist 생성은 스냅샷 테스트까지) | M2 | P0 | 첫 실행·재실행에서 연결, 기동 실패를 UI와 `krill doctor`에 표시, plist 스냅샷 테스트(AssociatedBundleIdentifiers 포함). 서명된 launchd 경로 검증은 MK-106 | MK-018, MK-021, MK-022 | T2#14 | OWE-390 |
| MK-026 | 연결 상태 배너 + 잠자기 고려 하트비트 + 5초 자동 재연결 + 끊긴 동안 입력 보류 | M2 | P0 | 데몬 SIGKILL 시 1초 안에 "재연결 중", 5초 안에 조작 없이 복구, 끊긴 동안 입력은 입력 ID·ack로 중복 실행 0·유실 0(결과가 불확실하면 화면에 표시), 새 셸 incarnation에는 자동 재전송 안 함 | MK-023, MK-030 | T2#15, T3#10, T1#7, perf-first(listen 소켓은 holder가 쥠, 재구성 끝난 세션부터 재개) | OWE-391 |
| MK-027 | 터미널 렌더러(클린룸): run 배치, 배경 병합, force_width, vsync 합치기 | M2 | P0 | MK-013 기준 합격, Zed GPL 코드 미포함 리뷰 체크 | MK-002, MK-013 | T3#7 | OWE-392 |
| MK-028 | IME 상태기계: preedit 오버레이, 확정만 PTY, 포커스 이탈 확정, 후보창 앵커 | M2 | P0 | MK-002의 10개 케이스가 회귀 체크리스트로 통과 | MK-002 | T3#8 | OWE-393 |
| MK-029 | 에이전트 TUI 호환: kitty keyboard, OSC 8, SGR 1006, DECSCUSR | M2 | P0 | Claude Code·Codex 각 30분 사용 시 깨짐 0, Shift+Enter·링크 Cmd+클릭 동작 | MK-019, MK-027 | T3#9 | OWE-394 |
| MK-030 | 앱 셸(3열) + 분할 트리 + 탭 바 + 레이아웃 복원 | M2 | P0 | 분할 4개·탭 10개, 비율 드래그, 앱 재시작 시 레이아웃 복원 | MK-003, MK-002 | T3#11, T1#9 | OWE-395 |
| MK-031 | 생존 e2e ①: 앱 kill -9 → 재실행 → 세션·출력 연속(자동 테스트 구현) | M2 | P0 | release 바이너리로 앱 kill -9 → 재실행 → 세션 생존·출력 해시 일치를 확인하는 e2e가 저장소에 있고 로컬 3회 연속 통과 | MK-023, MK-025, MK-027, MK-030 | T4-06, T1#25 | OWE-396 |
| MK-032 | 콜드 복원 뷰(재부팅·프로세스 소실 뒤 읽기 전용 마지막 화면 + 다시 시작 버튼) | M2 | P1 | 재부팅 뒤 레이아웃·마지막 스크롤백 표시, 다시 시작 버튼 동작 | MK-024, MK-030 | T2#24, T1#9 | OWE-397 |
| MK-033 | 터미널 기본 상호작용: 선택·복사·bracketed 붙여넣기·우클릭·파일 드롭·OSC 52 쓰기만 | M2 | P1 | 수동 QA 통과, 공백·따옴표 경로 드롭이 셸에서 열림, OSC 52 읽기 무시 | MK-027 | T3#12, T1#11 | OWE-398 |
| MK-034 | 셸 설정(로그인 셸·사용자 지정) + 환경 보정 | M2 | P1 | nvm/mise PATH가 Terminal.app과 같음 | MK-019 | T1#12 | OWE-399 |
| MK-035 | 숨긴 터미널 메모리 강등: krilld 비가시 세션 핫 스크롤백 축소(화면·모드는 유지), krill-app 비가시 탭 렌더 캐시 해제 | M2 | P1 | 터미널 20개 중 보이는 4개만 grid 보유, 전환 시 100ms 안 재구성 | MK-009, MK-023 | T1#10, T2#23 | OWE-400 |
| MK-036 | 세션 범위 권한(LOCAL_PEERPID 후손 판정, 감사 로그) | M3 | P0 | 세션 A 안에서 `krill send B` 기본 거부, 외부 셸은 허용, 거부는 감사 로그 | MK-021 | T2#16, T4-05 | OWE-401 |
| MK-037 | 에이전트 프로필(TOML) + 내장 5종 + 런처 + 설치 감지 preflight + 권한 기본 YOLO + YOLO 배지 + 자식 env 정리 | M3 | P0 | `krill agent list`가 감지·버전 출력, 미설치 안내, 새 세션 기본 YOLO·명시적 Manual 선택 유지, 자식 env에 내부 값 없음 | MK-034 | T4-07, T1#15, T4-21 | OWE-402 |
| MK-038 | `krill hook <agent>`: {} 선출력, 소켓 전달, 재시도, fail-open | M3 | P0 | 전체 wall-clock 50ms 예산 안에서 `{}` 즉시 출력·flush, 소켓 실패 시 spool 파일에 append 후 종료(krilld 기동 시 수거), 200회 p95 < 10ms, stdin 막힘·과대 JSON(상한) 처리 | MK-036 | T4-08 | OWE-403 |
| MK-039 | 데몬 상태 저장소 + 정규화(6상태 + 출처·신뢰도) | M3 | P0 | 이벤트 시퀀스 스냅샷 테스트, 앱 재시작 후 1초 안에 상태 동일, krilld 재기동 중 SessionStart·Stop 유실 0(spool 수거·이벤트 ID 중복 제거) | MK-038 | T4-09, T1#13 | OWE-404 |
| MK-040 | Claude 세션별 --settings 주입 + --session-id 사전 발급 | M3 | P0 | 사용자 `~/.claude/settings.json` 바이트 무변경으로 waiting·done 수신 | MK-011, MK-039 | T4-10 | OWE-405 |
| MK-041 | git 작업 큐(레포 직렬화, 동시성 제한, 환경·옵션 고정) | M3 | P0 | 동시 요청 20개에서 git 동시 실행 ≤ 4, 같은 레포 변경 직렬 | MK-019 | T4-14 | OWE-406 |
| MK-042 | 워크트리 생성(UI 대화상자·CLI, base 결정, 이름 충돌, include 복사, setup·신뢰 해시) | M3 | P0 | 36k 파일 레포에서 생성, 충돌 시 -2, 한글 이름 입력, setup 내용 해시가 바뀌면 재확인, include 경로 탈출·심링크 거부, 생성 후 사이드바 반영 | MK-041 | T4-15, T1#14, T3#22 | OWE-407 |
| MK-043 | 워크트리 삭제·정리(dirty 검사, remove, branch -d, prune·lock) | M3 | P0 | 미병합 브랜치 보존, dirty면 --force 없이 거부, locked 거부 테스트 | MK-041 | T4-16 | OWE-408 |
| MK-044 | 알림·Dock 배지·잠자기 방지(IOPMAssertion On/Agent/Off) | M3 | P0 | 입력 대기 전환 5초 안 알림, 클릭 시 pane 포커스, 작업 중 `pmset -g assertions` 표시 | MK-016, MK-039 | T1#16, T3#17, T4-13, T2#19 | OWE-409 |
| MK-045 | `krill` CLI MVP: ls/new/send/read/wait/attach/doctor, 데몬 직결 | M3 | P0 | 앱 종료 상태에서 전 명령 동작, 일반 터미널 `krill attach` 입출력, 호출당 RSS < 10MB | MK-036, MK-039, MK-025, MK-037, MK-042 | T1#17, T4-18 | OWE-410 |
| MK-046 | 사이드바: 워크트리 트리(가상 리스트) + 상태 점 + Needs You + 핀·이름 변경 | M3 | P1 | 500행 60fps, 입력 대기 세션이 맨 위, 유휴 프레임 0 | MK-030, MK-039, MK-042 | T3#13, T1#19 | OWE-411 |
| MK-047 | `krill wait --for idle·waiting·blocked·pattern·quiet` | M3 | P1 | idle은 파생 조건(상태가 working·unknown이 아니고 출력 정지 N초)으로 정의해 문서화, 모드별 통합 테스트, 타임아웃 종료 코드 2 | MK-045 | T4-19 | OWE-412 |
| MK-048 | Codex·OpenCode·Gemini·Cursor hook 설치(동의·diff·가드·원복) | M3 | P1 | 설치→제거 후 Krill이 넣은 항목만 제거(사용자가 그사이 바꾼 내용 보존, 원자 쓰기·백업), 가드로 Krill 밖에서는 no-op | MK-012, MK-039 | T4-11 | OWE-413 |
| MK-049 | 보조 감지: SIGCHLD·pgid, OSC 제목 글리프, 화면 규칙 + "추정" 배지 | M3 | P1 | 에이전트 kill -9 후 1초 안 exited, 녹화 재생 골든 테스트 | MK-039 | T4-12 | OWE-414 |
| MK-050 | 다중 클라이언트 역할과 크기 규칙(interactive/observer, latest) | M3 | P1 | 앱 + CLI interactive + observer 2개에서 크기 규칙·입력 순서·observer 쓰기 거부 | MK-023 | T2#17 | OWE-415 |
| MK-051 | 프로세스 그룹 종료 단계와 탈출 후손 스윕, 세션 상한 256 | M3 | P1 | setsid 후손까지 정리, 257번째 세션 명확한 오류 | MK-018 | T2#18 | OWE-416 |
| MK-052 | 세션 재개: resume 버튼(claude --resume, codex resume) + Restart 칩 | M3 | P1 | 데몬 강제 종료·재부팅 뒤 같은 대화 재개 | MK-032, MK-040, MK-048 | T1#18, T4-22 | OWE-417 |
| MK-053 | 변경 파일·numstat(FSEvents, 1000개 제한, 숨김 시 정지) | M3 | P1 | 저장 후 4초 안 갱신(디바운스·최소 간격 3초와 일치), 창 숨김 시 git 호출 0, 앱 상주 증가 < 10MB, 1000개 상한 도달 시 자식 정리 | MK-041 | T4-17 | OWE-418 |
| MK-054 | 읽기 전용 diff 뷰(가상 스크롤, 문법 강조) | M3 | P1 | 5,000줄 diff 60fps, 앱 +20MB 이하 | MK-030, MK-053 | T3#20, T3 S8 | OWE-419 |
| MK-055 | Open in(VS Code·Cursor·Zed·Finder) + 비-git 폴더 세션 | M3 | P1 | Open in 동작, git 아닌 폴더에서 에이전트 실행 | MK-042 | T1#20 | OWE-420 |
| MK-056 | 한글·이모지 바이트열 퍼징(데몬 파서 + 렌더 셰이핑) | M3 | P1 | 1시간 퍼징 panic 0 | MK-019, MK-027 | T3#16 | OWE-421 |
| MK-057 | Jump 팔레트(Cmd+J) 퍼지 검색 | M3 | P2 | 한글 IME 입력, 1,000개 항목 응답 ≤ 50ms | MK-028, MK-046 | T3#19, T1#19 | OWE-422 |
| MK-058 | 이모지·ZWJ·VS16 폭 보정 | M3 | P2 | 폭 불일치 목록 0 스냅샷 테스트 | MK-029 | T3#15 | OWE-423 |
| MK-059 | 터미널 검색(데몬 측, 콜드 로그 포함) | M3 | P2 | 1만 줄 이상 이력에서 다음/이전 이동·하이라이트 | MK-024 | T3#18 | OWE-424 |
| MK-060 | 에이전트용 SKILL.md/AGENTS 스니펫 배포 | M3 | P2 | Claude가 스킬만 보고 `krill new`와 `krill wait` 성공 | MK-045 | T4-20 | OWE-425 |
| MK-061 | 서명·공증 파이프라인(로컬 스크립트 + CI) | M4 | P0 | `spctl -a -vv` Notarized Developer ID, `stapler validate` 성공, DMG 공증 | MK-007 | T4-23 | OWE-426 |
| MK-062 | krilld 업그레이드 흐름(holder 재기동, N-1 호환, 다운그레이드 금지) + 고아 데몬 0 | M4 | P0 | 업그레이드 중 세션 유지·공백 ≤ 300ms, 구버전 앱 N-1 접속, 업데이트 뒤 데몬 세대 1개 | MK-018, MK-023, MK-024, MK-025 | T2#20, T4-24, T1#4, perf-first(재구성 1MiB당 3.7ms) | OWE-427 |
| MK-063 | 자체 업데이터(latest.json + ed25519 + codesign 검증 + 원자 교체 + 롤백) | M4 | P0 | 위조 패키지 거부, 재실행 실패 시 롤백, 업데이트 중 세션 유지 | MK-015, MK-061 | T4-25, T1#22 | OWE-428 |
| MK-064 | 생존 e2e ②③ + 장애 주입 CI(업데이트·krilld 크래시·출력 폭주·24시간 메모리) | M4 | P0 | macOS nightly 통과, 실패 시 릴리스 차단 | MK-031, MK-062, MK-063 | T4-26, T1#25 | OWE-429 |
| MK-065 | holder 자기 업그레이드(exec 핸드오프) | M4 | P1 | 10회 연속 무결, launchd 잡 상태 유지 | MK-018 | T2#21 | OWE-430 |
| MK-066 | 메모리 회귀 게이트(앱+데몬 ≤ 200MB, 초과 시 실패) | M4 | P1 | nightly 게이트가 의도적 회귀를 잡아냄 | MK-009, MK-031 | T4-27 | OWE-431 |
| MK-067 | 크래시 로그 + 다음 실행 감지 + 진단 번들(리댁션) + `krill doctor` 고장 진단 | M4 | P1 | 강제 panic 뒤 진단 대화상자, 번들에서 `sk-ant-`·PEM·토큰 마스킹, doctor가 고장 3종 탐지 | MK-045 | T4-28, T4-29, T1#23 | OWE-432 |
| MK-068 | krilld 자기 감시(footprint 상한 시 자기 재시작, 디스크 쓰기 지표) | M4 | P1 | 상한 초과 주입 시 세션 유지, 1분 단위 쓰기 지표 기록 | MK-024, MK-031 | T2#22 | OWE-433 |
| MK-069 | 로그 체계(tracing, 순환, 합계 20MB, ~/Library/Logs) | M4 | P1 | 24시간 부하 뒤 로그 합계 ≤ 20MB | MK-019 | T4-30 | OWE-434 |
| MK-070 | 설정(TOML) + 최소 설정 UI + 키 바인딩 파일 + 라이트/다크 + UI 줌 | M4 | P1 | 재시작 없이 반영, 키 바인딩 재정의 동작 | MK-030 | T1#24, T3#23 | OWE-435 |
| MK-071 | Resource Manager 경량판(앱·데몬·세션별 메모리, 디스크, 데몬 재시작, 세션 kill) | M4 | P1 | 표시값이 footprint ±10%, 데몬 재시작은 확인 후 동작 | MK-045 | T1#21 | OWE-436 |
| MK-072 | Homebrew 자체 tap + DMG 릴리스 워크플로 | M4 | P1 | `brew install --cask` 설치, `krill`이 PATH에, Gatekeeper 경고 없음, zap 후 잔여물 없음 | MK-061 | T4-31 | OWE-437 |
| MK-073 | 첫 실행 온보딩(git·에이전트 감지, hook 설치 안내, CLI 설치 — 동의 화면 제거됨, 2026-10-18 OWE-904) | M4 | P1 | 새 macOS 계정에서 모달 없이 완료·자동 준비 후 Claude 상태 감지 동작 | MK-040, MK-048 | T4-33 | OWE-438 |
| MK-074 | 라이선스 확정 + THIRD_PARTY_NOTICES + README 포지셔닝 + 공개 벤치(Orca 등 비교) | M4 | P1 | LICENSE·고지 파일 존재, README 첫 화면에 메모리·생존 수치와 측정 방법 | MK-064, MK-077 | T1#26, T1 S8 | OWE-439 |
| MK-075 | 개인정보: 기록 끄기, Time Machine 제외, 기록 삭제 명령 | M4 | P2 | 옵션 켜면 디스크 기록 0바이트, 삭제 뒤 파일 없음 | MK-024 | T2#25 | OWE-440 |
| MK-076 | 보안 강화: OSC 8 URL 미리보기, 제목 제어문자 필터, 여러 줄 붙여넣기 경고, Secure Keyboard Entry | M4 | P2 | 각 항목 테스트 또는 QA 통과 | MK-033 | T3#25 | OWE-441 |
| MK-077 | 라이선스·가격 모델 결정(사용자) + capabilities 자리 | M4 | P1 | 결정 문서 승인 | — | T1-D7, T4-39 | OWE-442 |
| MK-078 | 최소 접근성(VoiceOver 이름·역할) | M4 | P2 | VoiceOver로 세션 목록 탐색과 새 세션 생성 | MK-046 | T3#24, T1#27 | OWE-443 |
| MK-079 | holder 드레인 모드 | M5 | P2 | krilld 30초 SIGSTOP 중 에이전트 블록 없음, 링 범위 손실 0 | MK-018 | T2#26 | OWE-444 |
| MK-080 | .worktreeinclude(APFS clonefile) + orca.yaml setup 호환 + ORCA·CONDUCTOR 환경 변수 | M5 | P2 | Orca 설정 레포에서 .env 복사와 setup 자동 실행 | MK-042 | T1#28, T4-D9 | OWE-445 |
| MK-081 | Hibernation(Orca 안전 조건 8가지) | M5 | P2 | done 30분 지난 백그라운드 에이전트 종료·열면 자동 resume, 대기·입력 중은 절대 종료 안 함 | MK-052 | T1#29 | OWE-446 |
| MK-082 | 에이전트 세션 히스토리(요청 시 스캔) | M5 | P2 | 과거 세션 목록에서 Resume, 상주 프로세스 없음 | MK-052 | T1#30 | OWE-447 |
| MK-083 | `claude agents --json` 연동 | M5 | P2 | agent view 백그라운드 세션이 사이드바에 상태와 함께 표시 | MK-039 | T1#31 | OWE-448 |
| MK-084 | 사용량·레이트리밋 표시(Claude·Codex 로컬 파일) | M5 | P2 | 상태바에 5시간·주간 사용률, 80% 경고 | MK-039 | T1#32 | OWE-449 |
| MK-085 | diff 줄 주석 → 에이전트에 보내기 | M5 | P2 | 주석을 모아 프롬프트 1개로 주입 | MK-054 | T1#33, T3#28 | OWE-450 |
| MK-086 | 알림 웹훅(ntfy·Telegram·Discord, 본문 제외) | M5 | P2 | 완료 이벤트 POST, 재시도 1회, 출력 본문 미포함 | MK-044 | T1#34 | OWE-451 |
| MK-087 | 메뉴바 아이콘 + 앱 꺼진 상태 알림 | M5 | P2 | 창을 닫아도 working·대기 수 표시, 앱 종료 상태에서 권한 요청 알림 | MK-044 | T1#35, T4-38 | OWE-452 |
| MK-088 | Git 변경 패널·커밋 UI | M5 | P2 | 스테이징·언스테이징·커밋이 UI에서 끝남 | MK-054 | T3#21 | OWE-453 |
| MK-089 | 텔레메트리 opt-in(이벤트 10개 이하, DO_NOT_TRACK) | M5 | P2 | 기본 꺼짐에서 네트워크 전송 0회 | MK-077 | T4-34 | OWE-454 |
| MK-090 | PR 상태 배지 + PR 만들기(gh, 페이싱) | M5 | P2 | 배지 표시, 요청 ≤ 20회/5분, gh 없으면 안내 | MK-042 | T4-35 | OWE-455 |
| MK-091 | 에이전트 프로필·화면 규칙 원격 갱신(서명된 규칙 파일) | M5 | P2 | 앱 업데이트 없이 규칙 교체, 서명 틀리면 거부 | MK-049 | T4-37 | OWE-456 |
| MK-092 | Quick Commands + Copy Context | M5 | P3 | 저장 명령 클릭 주입, 최근 출력 N줄 복사 | MK-030 | T1#36 | OWE-457 |
| MK-093 | 워크트리 코멘트(`krill set --comment`) + 외부 워크트리 감지 | M5 | P3 | CLI 코멘트가 사이드바에 표시, .claude/worktrees 숨김 카드 | MK-045, MK-046 | T1#37 | OWE-458 |
| MK-094 | ko/en 다국어 | M5 | P3 | 언어 전환 시 전 문자열 변경, 하드코딩 0(lint) | MK-070 | T1#38 | OWE-459 |
| MK-095 | 워크트리 투기적 사전 준비 | M5 | P3 | 체감 생성 < 1초(36k 파일), 미사용분 5분 뒤 정리 | MK-042 | T4-36 | OWE-460 |
| MK-096 | 피어 코드서명 검증(LOCAL_PEERTOKEN + SecCode) | M5 | P3 | 미서명 바이너리의 전체 권한 접속 거부(설정) | MK-036 | T2#27 | OWE-461 |
| MK-097 | 세그먼트 암호화(키체인 키) | M5 | P3 | 평문 검색 0건, 성능 저하 ≤ 10% | MK-024 | T2#28 | OWE-462 |
| MK-098 | 상태 바·플로팅 터미널·대시보드 팝아웃 | M5 | P3 | 팝아웃 시 앱 증가 ≤ 40MB, 유휴 CPU 0% | MK-030 | T3#26, T3#27 | OWE-463 |
| MK-099 | 원격 krilld + `krill attach --host`(SSH 소켓 포워딩) | M6 | P3 | 원격 맥의 세션 조작, 네트워크 끊겼다 붙어도 세션 유지 | MK-026, MK-045 | T2#29, T1#39 | OWE-464 |
| MK-100 | 경량 orchestration(send/check/ask) 필요성 재평가 | M6 | P3 | 사용 사례 3개 이상이면 설계 문서, 아니면 닫음 | MK-047 | T1#40, T4-40 | OWE-465 |
| MK-101 | 예약 자동화(데몬 스케줄러) | M6 | P3 | cron 시각에 프롬프트 실행, 실행 기록 저장 | MK-045 | T1#41 | OWE-466 |
| MK-102 | libghostty-vt 재평가 + 이미지 프로토콜 | M6 | P3 | C API 안정·Zig 없는 빌드 여부, 메모리 비교 메모 | — | T3#29, T3#30 | OWE-467 |
| MK-103 | gpui vendor 패치 검증 후 적용(nearest 아틀라스, drawable 2장) | M1 | P1 | 패치 전후 한글 렌더 스크린샷 비교와 footprint·프레임 드롭 측정값, 패치 파일에 변경 표시 | MK-002, MK-009 | T3-D7, T3-D8, Codex 교차검증 | OWE-470 |
| MK-104 | [사람] Apple Developer Program 가입·Developer ID 인증서 설치 | M1 | P1 | `security find-identity -v -p codesigning`에 Developer ID Application 1개 이상 | — | PRD 결정 대기, Codex 교차검증 | OWE-471 |
| MK-105 | 생존 e2e 7일 연속 관찰 게이트(macOS nightly) | M4 | P1 | macOS 러너에서 MK-031 e2e가 7일 연속 통과한 기록 | MK-031, MK-017 | T4-06, Codex 교차검증 | OWE-472 |
| MK-106 | 서명 빌드의 launchd 기동 경로 전환·검증(bootout→교체→bootstrap 순서 고정) | M4 | P0 | 서명 빌드에서 launchd 잡으로 holder 기동, 업데이트 순서 e2e에서 세션 유지·고아 0 | MK-007, MK-025 | T2-D2, C1 A1~A3 | OWE-473 |
| MK-107 | 프로바이더 다계정 레지스트리(accounts.toml) + 계정별 5h/7d 사용량 측정 + FEAT_USAGE IPC·CLI 연동 | M7 | P0 | Claude·Codex·Gemini·OpenCode 다계정 등록/조회, 프로필별 5h/7d 파싱, `krill account` CLI 및 IPC 테스트 통과 | MK-084 | M7 | OWE-617 |
| MK-108 | 프로바이더 계정 원클릭 전환(관리자·상태바) + 임계치·레이트리밋 기반 자동전환(Auto-Switch) 엔진 | M7 | P0 | 원클릭 전환 시 활성 계정·에이전트 env(`CLAUDE_CONFIG_DIR`/`CODEX_HOME`) 즉시 반영, 임계치 도달 시 최저 사용률 계정으로 자동 전환 테스트 통과 | MK-107 | M7 | OWE-618 |
| MK-109 | 관리자 페이지(Settings & Admin Center) 전면 개편(7개 탭: General/Accounts/Agents/SSH/Workflows/Shortcuts/Resources) | M7 | P0 | `⌘,` 및 사이드바 설정 버튼에서 7개 탭 전환·조작 동작, 설정 변경이 재시작 없이 즉시 반영 | MK-070, MK-108 | M7 | OWE-619 |
| MK-110 | Orca 단축키 패리티(`⌘L`, `⌘⇧G`, `⌘]`/`⌘[`, `⌘⇧Enter`, `⌘K`, `⌘A`, `⌘⌥T`, `⌘⇧T`, `⌘⇧Up/Down`, `⌃1..9`, `⌘N`) | M7 | P0 | 신규 단축키 액션 바인딩 및 동작 테스트 통과, 관리자 페이지 Shortcuts 탭에 반영 | MK-030, MK-070 | M7 | OWE-620 |
| MK-111 | 클립보드 이미지 붙여넣기(`⌘V`·우클릭) 및 복사된 파일 경로 셸 이스케이프 붙여넣기 | M7 | P0 | 클립보드 PNG/TIFF/JPEG 이미지를 임시 파일(`0600`)로 저장 후 이스케이프 경로를 PTY에 주입, ExternalPaths 이스케이프 테스트 통과 | MK-033 | M7 | OWE-621 |
| MK-112 | SSH 호스트 PC-to-PC 연결(호스트 관리 UI, `~/.ssh/config` 가져오기, 연결 테스트, 원격 세션 연결) | M7 | P1 | SSH 호스트 추가/삭제/`~/.ssh/config` 파싱, 연결 테스트, 원격 호스트 세션 기동 및 터널 브리지 테스트 통과 | MK-099, MK-109 | M7 | OWE-622 |
| MK-113 | 기존 개발 기능 + M7 신규 기능 전수 실제 화면(GUI) 테스트 및 결함 수정 | M7 | P0 | 실제 `krill-app` 창 기동 후 전체 기능 화면 검증 완료, `scripts/check.sh` 0 | MK-108, MK-109, MK-110, MK-111, MK-112 | M7 | OWE-623 |
| MK-114 | 멀티 에이전트 오케스트레이션 엔진 및 `krill orchestration` CLI (Run·Task DAG·Worker-Start·Send/Check·Ask/Reply·Gate·Release) | M8 | P0 | Run/Task DAG 자동 수렴, worker-start 프리앰블 주입, send/check(--wait/--ack), ask/reply, gate-create/resolve, worker-release 단위·통합 테스트 통과 | MK-047 | M8 | OWE-624 |
| MK-115 | 에이전트 스킬 레지스트리(`krill skills list/get/install`) 및 세션 컨텍스트 핸드오프(`krill handoff`) | M8 | P0 | `krill skills list/get/install` 및 `krill handoff` CLI 동작, 테스트 통과 | MK-045, MK-114 | M8 | OWE-625 |
| MK-116 | 워크스페이스 체크포인트·상태 카드(`--workspace-status`, `--unread`, `--pr`, `--issue`) 및 `krill reference` CLI | M8 | P0 | `krill set` 확장 플래그 및 `krill reference list/add/remove/find` 동작, 테스트 통과 | MK-093 | M8 | OWE-626 |
| MK-117 | 로컬 개발 서버 포트·URL 자동 감지(`krill ports`) 및 브라우저 열기 연동 | M8 | P1 | 세션 출력 URL/포트 감지, `krill ports [--json]` 동작 및 단위 테스트 통과 | MK-045 | M8 | OWE-627 |
| MK-118 | GUI 오케스트레이션 팝오버·포트 팝오버·사이드바 체크포인트 배지·관리자 스킬 설치 통합 및 실화면 QA | M8 | P0 | 상태바 오케스트레이션/포트 팝오버, 사이드바 배지, 관리자 페이지 연동, 실화면 검증 완료, `scripts/check.sh` 0 | MK-114, MK-115, MK-116, MK-117 | M8 | OWE-628 |
| MK-119 | 가운데 에디터 탭 코어(세션 영역 파일 탭, 코드 편집·⌘S 원자적 저장·dirty·외부 변경 재로드, 변경 패널·⌘P·터미널 링크 열기) | M8 | P0 | 파일 탭 열기·편집·저장·외부 변경 상태기계 테스트 통과, 실화면 캡처, `scripts/check.sh` 0 | MK-054 | M8 | OWE-663 |
| MK-120 | 에디터 지원 포맷(파일 종류 분류, 이미지·마크다운 미리보기·CSV 표·ipynb·바이너리/미디어/PDF 자리표시자) | M8 | P0 | 분류·NUL 검사·크기 한도·CSV·ipynb 파싱 테스트 통과, `scripts/check.sh` 0 | — | M8 | OWE-664 |
| MK-121 | 에디터 포맷 뷰어 통합 + 실화면 QA | M8 | P0 | 포맷별 실화면 검증 완료, 앱 ≤ 120MB, `scripts/check.sh` 0 | MK-119, MK-120 | — | OWE-665 |
| MK-122 | 모바일 기기 레지스트리(`mobile-devices.json`)·`ring` 기반 E2EE v2 핸드셰이크·크기 비소유 `Role::Interactive` 확장 | M9 | P0 | `FEAT_MOBILE`(`1<<56`)·`0x45`/`0x65` 등록, E2EE(`CHACHA20_POLY1305`·`HKDF_SHA256`) 핸드셰이크·재전송 거부·기기 등록/회수·크기 비소유 Interactive 단위·통합 테스트 통과, `scripts/check.sh` 0 | MK-050 | M9 | OWE-678 |
| MK-123 | `krilld` 모바일 HTTP/WS 게이트웨이(Tailscale 감지·기존 LAN 설정 Relay 이행, 지연 바인딩 `Lazy Exposure`, `UnixStream::pair` 브리지) | M9 | P0 | 인터페이스 우선순위 선택·지연 바인딩 개폐·WS↔UnixStream 브리지·회수 시 즉시 연결 종료 통합 테스트 통과, 데몬 ≤ 60MB, `scripts/check.sh` 0 | MK-122 | M9 | OWE-679 |
| MK-124 | 1-스캔 QR 페어링: 관리자 페이지 `Mobile` 전용 탭(8번째 탭) + `krill mobile` CLI + E2EE 릴레이 우선·Quick Tunnel 폴백(`--tunnel`) | M9 | P0 | `Mobile` 8번째 설정 탭 UI 동작, QR 생성·만료·모드 전환 시 무효화/갱신, `krill mobile pair/devices/revoke/serve` CLI 및 `--tunnel` URL 파싱 단위·통합 테스트 통과, `scripts/check.sh` 0 | MK-123, MK-109 | M9 | OWE-680 |
| MK-125 | 내장 모바일 PWA 클라이언트 코어(`Needs You` 최상단·세션 카드·현재 터미널에서 권한 확인·프롬프트/사진 첨부·오케스트레이션·계정 스위처·Git 변경) | M9 | P0 | 모바일 PWA 정적 에셋 내장 서빙(비보안 HTTP 컨텍스트 호환), 세션 목록·과거 질문 review_in_term 및 현재 터미널 직접 입력·프롬프트/이미지 첨부·오케스트레이션/계정 전환 E2E 테스트 통과, `scripts/check.sh` 0 | MK-123, MK-124 | M9 | OWE-681 |
| MK-126 | 모바일 라이브 터미널 그리드 뷰(화면 델타·핫/콜드 스크롤백·특수키 바·`Fit to Phone`) + 딥링크 웹훅 알림 및 실화면 QA | M9 | P1 | 라이브 그리드 델타·콜드 스크롤백·보조 키바·Fit to Phone·웹훅 딥링크 테스트 통과, 실화면 검증 완료, `scripts/check.sh` 0 | MK-125, MK-086 | M9 | OWE-682 |
| MK-127 | 설치 후 온보딩 v2(상위): macOS 권한 점검·필수 요소 체크리스트(동의 화면 제거됨, 2026-10-18 OWE-904) | M4 | P1 | 새 계정 첫 실행에 모달 없이 완료·자동 준비, 필수 설치 안내는 조치 배너·빠른 점검(상태바)·점검 모달로, `scripts/check.sh` 0 | MK-073 | crates/krill-app/src/onboarding.rs | OWE-688 |
| MK-128 | 온보딩 점검 엔진(`krill doctor --json`, 공용 점검 ID·등급·수정 방법) | M4 | P1 | 판정 순수 함수 항목별 경계 테스트, 사람용 출력 회귀 없음, `scripts/check.sh` 0 | MK-067 | crates/krill/src/doctor.rs | OWE-689 |
| MK-129 | 온보딩 동의 화면 + `consent.toml` 버전 재동의(동의 화면 제거됨, 2026-10-18 OWE-904) | M4 | P1 | 첫 실행 동의 화면·`consent.toml` 제거됨(2026-10-18 OWE-904). 첫 실행은 모달 없이 `onboarding.toml`에 완료를 기록하고 자동 준비만 수행 | MK-075 | crates/krill-app/src/onboarding.rs | OWE-690 |
| MK-160 | 앱 전용 점검(설치 위치·알림 권한·폴더/전체 디스크 접근, TCC 귀속 측정 선행) | M4 | P1 | TCC 귀속 측정값, 판정 순수 함수 테스트, `scripts/check.sh` 0 | MK-044 | crates/krill-app/src/alert.rs | OWE-691 |
| MK-161 | 점검 모달 UI + `onboarding.toml` 상태기계(동의 화면 제거됨, 2026-10-18 OWE-904) | M4 | P1 | 상태기계 테스트(구 `consent` 단계 초기화·필수 fail·마이그레이션·원자 쓰기), 점검 모달·기존 온보딩 테스트 통과, `scripts/check.sh` 0 | MK-128, MK-160 | crates/krill-app/src/onboarding/welcome.rs | OWE-692 |
| MK-162 | 설정 점검 재진입(설정·메뉴·명령 팔레트) + 필수 실패 상태바 경고 | M4 | P2 | 세 진입점 같은 창, 시작 경로 비블로킹, `scripts/check.sh` 0 | MK-161 | crates/krill-app/src/settings.rs | OWE-693 |
| MK-163 | 온보딩 v2 새 계정 E2E·실화면 QA | M4 | P1 | 온보딩 시나리오 9종 결과, `scripts/check.sh` 0 | MK-128~MK-162 | — | OWE-694 |
| MK-153 | GitHub Releases 배포 연동 무설정 자동 앱 업데이트 완성(상위) | M4 | P1 | `bash packaging/publish-github.sh` 배포 후 `/Applications/Krill.app` 무설정 자동 업데이트·재기동·`krilld` 업그레이드 완결 | MK-061, MK-062, MK-063 | M4 | OWE-715 |
| MK-154 | 릴리스 Ed25519 서명키 자동 관리 + `publish-github.sh` `latest.json` 자동 생성·업로드(`--bump` 통합) | M4 | P1 | `packaging/macos/ed25519_manifest.py`↔Rust 교차 검증, `bundle.sh` `Info.plist` 키 봉인, `publish-github.sh --dry-run` `latest.json` 생성, `test_pipeline.py`·`test_release.py` 통과 | MK-061, MK-072 | M4 | OWE-716 |
| MK-155 | 업데이터 런타임 결함 4건 수정(Apple Silicon Mach-O `SIGKILL` 방지·`Info.plist` 무설정 폴백·비공개 GitHub `gh` 폴백·재기동 종료 누락) | M4 | P0 | 실행된 서명 Mach-O 번들 `swap_bundles`/`rollback` 후 재실행 종료 코드 0(`SIGKILL -9` 0회), `Config::load_for_bundle` 앱·relauncher 적용, `gh release download` 폴백 테스트 통과, `scripts/check.sh` 0 | MK-063, MK-154 | M4 | OWE-717 |
| MK-156 | 앱 내 원클릭·자동 업데이트 UI(Orca식 플로팅 `UpdateCard`·상태바 배지·설정 General 섹션·주기 폴링) 및 `krill update` CLI | M4 | P1 | 우하단 플로팅 `UpdateCard`(버전·세션 무중단 안내·`[재시작하여 업데이트]`·`[-]` 상태바 축소 토글)·상태바 배지·설정 General 업데이트 행·4시간 주기 점검·`krill update [--check] [--json]`(`krill-app --update-cli` 위임) 통합 테스트 통과, `scripts/check.sh` 0 | MK-155 | M4 | OWE-718 |
| MK-130 | [스파이크] iroh 1.3 원격 연결 실측(rustc 1.98·cargo deny·메모리·지연) | M9 | P1 | 측정값 확인, krilld 무변경으로 터널 너머 `krill ls` 동작 | MK-099 | M9 | OWE-666 |
| MK-131 | 호스트 인식 연결 계층 + 사이드바 호스트별 그룹(내 Mac+원격) + 새 세션 실행 위치 선택 | M9 | P1 | 로컬·원격 세션 동시 표시, 원격 krilld kill -9 후 로컬 무영향·자동 재연결 통합 테스트, 실화면 캡처, `scripts/check.sh` 0 | MK-099, MK-132 | M9 | OWE-667 |
| MK-132 | SSH 호스트 설정 UX(실제 입력 폼·Tailscale 피어 자동 감지·첫 연결 키 등록·포트/키/원격 HOME 소켓) | M9 | P1 | argv 빌더·입력 검증·tailscale JSON 파싱·원격 HOME 경로 테스트, `scripts/check.sh` 0 | MK-112 | — | OWE-668 |
| MK-133 | `krill-link`: iroh P2P 터널 + 기기 페어링(초대 링크·QR·근처 기기)·기기 회수, 앱 연동 | M9 | P1 | 오프라인 통합 테스트(페어링·만료 토큰·미허용 거부·revoke·krilld 터널·재연결), krill-link ≤ 20MB(별도 예산, 켤 때만), `scripts/check.sh` 0 | MK-130, MK-131 | M9 | OWE-669 |
| MK-134 | 끊김 복구: 입력 정확히 한 번·순서 보장, 연결 상태 칩, 세션 목록 캐시, 자동 재연결 | M9 | P1 | 네트워크 단절 중 입력이 재연결 후 1회·순서대로 도착, 상태 칩 전환 테스트, `scripts/check.sh` 0 | MK-131, MK-133 | — | OWE-670 |
| MK-172 | Orca 데이터 스캐너 & 원클릭 전체 이관 엔진(`krill import orca` + 다계정·원격 SSH·등록 프로젝트·워크트리·설정 이관) | M10 | P0 | `profile-state.db`/`orca-data.json`·`claude-profiles`·`codex-accounts`·`sshTargets`·`repos`·`worktreeMeta` 멱등 이관 및 이관된 계정 즉시 기동 테스트 통과, `scripts/check.sh` 0 | MK-107, MK-112 | M10 | OWE-726 |
| MK-173 | GUI Orca 마이그레이션 마법사 & Settings `Import from Orca` 카드(설정 연동; 첫 실행 온보딩 카드는 제거됨, 2026-10-18 OWE-904) | M10 | P0 | Settings에서 Orca 데이터 감지·카테고리 선택·원클릭 이관 후 재시작 없이 사이드바·계정·SSH 즉시 반영, `scripts/check.sh` 0 | MK-172 | M10 | OWE-727 |
| MK-174 | 워크트리 공유 심볼릭 링크(`Worktree Symlinks`) & 저장소별 `setup`/`archive` 훅 고도화 | M10 | P1 | 새 워크트리 생성 시 등록된 공유 경로(`.env` 등) 자동 심볼릭 링크 생성 및 `setup`/`archive` 훅 동작 테스트 통과, `scripts/check.sh` 0 | MK-080 | M10 | OWE-728 |
| MK-175 | 워크스페이스 디스크 용량 분석(`Space`) & 머지된 워크트리 일괄 클린업(`Cleanup`) | M10 | P1 | 워크트리별 용량 측정 및 PR 머지·경과일·clean 조건별 일괄 정리 CLI/GUI 테스트 통과, `scripts/check.sh` 0 | MK-043, MK-090 | M10 | OWE-729 |
| MK-176 | 부모-자식 워크트리 계보 트리(`Worktree Lineage`) & 워크트리 탐색 히스토리(`Back`/`Forward` `⌘⌥←/→`) | M10 | P1 | 부모-자식 워크트리 계층 접기/펼치기 및 `⌘⌥←`/`⌘⌥→` 탐색 히스토리 이동 테스트 통과, `scripts/check.sh` 0 | MK-046 | M10 | OWE-730 |
| MK-177 | 사이드바 그룹화·정렬·필터 메뉴 & 워크트리 카드 표시 속성 커스터마이징 | M10 | P1 | Group by·Smart Sort·수면/자동화 필터·카드 밀도 토글 영속화 및 500행 60fps 유지, `scripts/check.sh` 0 | MK-046 | M10 | OWE-731 |
| MK-178 | 우측 패널: 가상화 파일 탐색기(`File Explorer` `⌘⇧E`) & `krill file open/diff` CLI | M10 | P0 | `⌘⇧E` 파일 트리 탐색·파일 생성/이름변경/삭제·에디터 열기 및 `krill file open/diff` CLI 통합 테스트 통과, `scripts/check.sh` 0 | MK-119 | M10 | OWE-732 |
| MK-179 | 우측 패널: 워크스페이스 코드 전문 검색(`Search` `⌘⇧F`) | M10 | P1 | `⌘⇧F` 전문 검색(대소문자·정규식·글로브 필터) 및 결과 클릭 시 에디터 해당 줄 이동 테스트 통과, `scripts/check.sh` 0 | MK-119, MK-178 | M10 | OWE-733 |
| MK-180 | 탭 바 고도화: 탭 고정(`Pin Tab`) · 9색 탭 태그 · `Ctrl+Tab` MRU 최근 탭 스위처 | M10 | P1 | 탭 고정 닫기 방지, 9색 태그 영속화, `Ctrl+Tab` MRU 전환 테스트 통과, `scripts/check.sh` 0 | MK-030 | M10 | OWE-734 |
| MK-181 | AI Vault: 멀티 에이전트 대화 전문 검색(`krill search` + GUI 볼트 패널) | M10 | P0 | 다중 에이전트 트랜스크립트 본문 검색(`krill search`) 및 GUI 볼트 패널 검색·원클릭 세션 재개 테스트 통과, `scripts/check.sh` 0 | MK-052, MK-082 | M10 | OWE-735 |
| MK-182 | 에이전트 세션 포크(`Fork Agent Session`) & 새 세션에 요약 이어하기(`Continue in New Session`) | M10 | P1 | `krill fork` CLI 및 GUI 탭/터미널 우클릭에서 세션 포크·새 세션 이어하기 동작 테스트 통과, `scripts/check.sh` 0 | MK-052, MK-115 | M10 | OWE-736 |
| MK-183 | 첫 프롬프트 기반 브랜치·세션 이름 자동 변경 + Claude 프롬프트 캐시 타이머(`CacheTimer`) + 워크스페이스 신뢰 프리셋 | M10 | P1 | 첫 프롬프트 브랜치/세션 자동 리네임, 사이드바 Claude 캐시 타이머 표시, 새 워크트리 신뢰 프리셋 주입 테스트 통과, `scripts/check.sh` 0 | MK-039, MK-042 | M10 | OWE-737 |
| MK-184 | Git 패널 `Push`/`Pull`/`Sync` 액션 + AI 커밋 메시지·PR 본문 자동 생성 + Git 실패 AI 복구 배너 | M10 | P0 | Git 패널 Push/Pull/Sync 동작, AI 커밋/PR 문구 생성, Git 실패 시 에러 배너→에이전트 프롬프트 주입 테스트 통과, `scripts/check.sh` 0 | MK-088, MK-090 | M10 | OWE-738 |
| MK-185 | PR CI Checks 실패 로그 조회 & "AI로 CI 실패 / PR 리뷰 코멘트 수정" 원클릭 큐 | M10 | P1 | PR CI 체크 상태·실패 로그·리뷰 코멘트 파싱 및 선택 항목 에이전트 프롬프트 일괄 전송 테스트 통과, `scripts/check.sh` 0 | MK-085, MK-090 | M10 | OWE-739 |
| MK-186 | 스케줄 자동화 Precheck 셸 게이트 & 새 워크트리 격리 실행 · 실행 상세 로그 뷰어 | M10 | P1 | `precheck` 비-0 종료 시 에이전트 실행 생략(`skipped`), 새 워크트리 격리 실행, Workflows 탭 실행 로그 조회 및 `Run Now` 테스트 통과, `scripts/check.sh` 0 | MK-101 | M10 | OWE-740 |
| MK-187 | macOS 네이티브 Computer Use (`krill computer` CLI + `computer-use` 내장 스킬) | M10 | P1 | `krill computer` 서브커맨드(앱/창 목록, AX 스냅샷, 클릭/키 입력 합성) 및 `computer-use` 내장 스킬 설치·조회 테스트 통과, `scripts/check.sh` 0 | MK-115 | M10 | OWE-741 |
| MK-188 | Windows 포팅 기획·전략(상위) | M11 | P1 | 하위 MK-189~222 진행, Phase별 결정 기록 | — | boost-2026-10-18 | OWE-807 |
| MK-189 | 문서: PRD·ROADMAP M11·MK 행 신설, ARCHITECTURE §4.1 Windows 열(미확정) 추가 | M11 | P1 | 문서 diff 리뷰, `scripts/check.sh` 0 | — | boost WIN-D1 | OWE-808 |
| MK-190 | 하네스 문구: AGENTS 규칙 5·CONVENTIONS Windows 조항·holder 상한 해석 | M11 | P1 | 문서 diff, `scripts/check.sh` 0 | — | boost WIN-D2 | OWE-809 |
| MK-191 | linux-host 미커밋 작업 정리·병합(선행) | M11 | P1 | main에 `sys/{macos,linux}.rs` 반영, `scripts/check.sh` 0 | — | boost WIN-D3 | OWE-810 |
| MK-192 | 하네스: `scripts/check-windows.sh` + 격리 툴체인 부트스트랩 | M11 | P1 | 새 셸에서 `scripts/check-windows.sh -p krill-proto`가 기준선(lib 41건) 재현, Homebrew 툴체인 무변경 | — | boost W1-00 | OWE-811 |
| MK-193 | 의존성 선등록: windows-sys·uds_windows, target 섹션 이동, deny.toml Windows 타깃 | M11 | P1 | `cargo metadata --locked` OK, `cargo deny --locked check` ok, `scripts/check.sh` 0 | — | boost W1-01 | OWE-812 |
| MK-194 | krill-log Windows check 통과 | M11 | P1 | `WCHK(krill-log)` 0, macOS clippy·nextest | — | boost W1-02 | OWE-813 |
| MK-195 | krill-proto: `krill_proto::os` 심 + 경로·preflight·which Windows 대응 | M11 | P1 | `WCHK(krill-proto)` 0, macOS clippy·nextest, Linux CI 녹색 | MK-193 | boost W1-03 | OWE-814 |
| MK-196 | krill-term·krill-hosts Windows check 통과 | M11 | P1 | 두 크레이트 `WCHK` 0, macOS clippy·nextest | MK-195 | boost W1-04 | OWE-815 |
| MK-197 | krill-holder Windows 스텁 계약(컴파일 전용) | M11 | P1 | `WCHK(krill-holder)` 0, macOS clippy·nextest·`tests/survive.rs` 녹색, holder 줄 수 상한 준수 | MK-195 | boost W1-05 | OWE-816 |
| MK-198 | krill-link Windows check: tokio Unix·launchd cfg + 앱용 API 스텁 | M11 | P2 | `WCHK(krill-link)` 0, macOS clippy·nextest | MK-195 | boost W1-06 | OWE-817 |
| MK-199 | krilld: nonblocking pipe + `kill_process_group` 중복 헬퍼 통합(macOS 리팩터) | M11 | P2 | 동작 동일, `cargo clippy -p krilld`·`cargo nextest run -p krilld` 녹색, `scripts/check.sh` 0 | — | boost W1-07a | OWE-818 |
| MK-200 | krilld 플랫폼 계층: `sys/windows.rs` 스텁·할당기·crypto·Cargo.toml target 이동 | M11 | P1 | `WCHK(krilld, --lib)` 중 담당 파일 오류 0, macOS clippy·nextest | MK-191, MK-197 | boost W1-07b | OWE-819 |
| MK-201 | krilld fd·PTY·IPC 경로 cfg | M11 | P1 | 담당 파일 오류 0, MK-200과 합친 뒤 `WCHK(krilld)` 0, macOS nextest 녹색 | MK-197, MK-199 | boost W1-07c | OWE-820 |
| MK-202 | krill CLI Windows check | M11 | P2 | `WCHK(krill)` 0, macOS clippy·nextest | MK-196 | boost W1-08 | OWE-821 |
| MK-203 | krill-app Windows check: platform cfg + `platform/windows.rs` no-op | M11 | P1 | 1차 `WCHK(krill-app, --lib --bins)` 0, 2차 `--all-targets`(MK-200·201 뒤), macOS clippy·nextest, 메모리 게이트 회귀 없음 | MK-196, MK-198 | boost W1-09 | OWE-822 |
| MK-204 | `ci-windows.yml` + `.gitattributes`(eol=lf) | M11 | P2 | YAML 문법 검증, 처음엔 `workflow_dispatch`만, push 트리거는 MK-205 뒤 | — | boost W1-10 | OWE-823 |
| MK-205 | Phase 1 통합 검증: 워크스페이스 Windows check 0 | M11 | P1 | `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` 0, Windows 타깃 clippy, `cargo deny --target x86_64-pc-windows-msvc check` ok, macOS `scripts/check.sh` 0 | MK-194, MK-196, MK-197, MK-198, MK-200, MK-201, MK-202, MK-203 | boost W1-11 | OWE-824 |
| MK-206 | GCP Windows 테스트 VM: 켜기·끄기 스크립트 + Rust 빌드 환경 | M11 | P1 | `up.sh` → ssh로 `cargo --version`·`rustc -vV` 출력, `down.sh` 후 STATUS=TERMINATED | — | boost Phase 2 | OWE-825 |
| MK-207 | [스파이크] S-W1 ConPTY 세션 생존·핸드오프·출력 충실도 | M11 | P0 | 실측 표 + ARCHITECTURE §4.1 Windows 열 초안, VM 정지 확인 | MK-206 | boost S-W1 | OWE-826 |
| MK-208 | [스파이크] S-W2 AF_UNIX IPC·피어 인증 | M11 | P1 | 결과표(uds_windows·`SIO_AF_UNIX_GETPEERPID`·`WSADuplicateSocketW`, 안 되면 named pipe 비교), VM 정지 확인 | MK-206 | boost S-W2 | OWE-827 |
| MK-209 | [스파이크] S-W3 gpui 앱 렌더·한글 IME·메모리 지표 | M11 | P1 | 결과표 + 캡처(한글 IME 10케이스, 메모리 지표 정의), 사람 확인 항목 표시 | MK-203, MK-206 | boost S-W3 | OWE-828 |
| MK-210 | [스파이크] S-W4 데몬 수명(앱 종료·로그오프)과 기동 방식 | M11 | P2 | 기동 방식 결정안 | MK-206 | boost S-W4 | OWE-829 |
| MK-211 | [스파이크] S-W5 에이전트 실행(PATHEXT·`.cmd`)·기본 셸 | M11 | P2 | 실행 성공표 | MK-206 | boost S-W5 | OWE-830 |
| MK-212 | holder 프로토콜: 핸들 값 전달·Resize 메시지 | M11 | P2 | proto 테스트, 와이어 호환 유지 | MK-207 | boost W3-0 | OWE-831 |
| MK-213 | holder Windows 구현: ConPTY·Job Object+IOCP·DuplicateHandle·업그레이드 정책 | M11 | P2 | Windows 생존 e2e | MK-212 | boost W3-1 | OWE-832 |
| MK-214 | krilld Windows 구현: 리더 스레드·resize·DPAPI 키·셸·피어 검증·NOFOLLOW | M11 | P2 | Windows nextest 녹색 | MK-208, MK-212 | boost W3-2 | OWE-833 |
| MK-215 | 앱 Windows 구현: platform 실구현·`secondary-` 단축키·폰트·열기 | M11 | P2 | 실기 QA | MK-209 | boost W3-3 | OWE-834 |
| MK-216 | CLI Windows 구현: hook·doctor·statusline | M11 | P2 | Windows nextest | MK-205 | boost W3-4 | OWE-835 |
| MK-217 | Windows 세션 생존 e2e: 앱 `taskkill /F` 1,000회·krilld kill·출력 해시 | M11 | P2 | 손실 0, 결과 첨부 | MK-213, MK-214 | boost W3-5 | OWE-836 |
| MK-218 | 배포: 설치 형식·rename 방식 업데이터·롤백 | M11 | P3 | 설치 형식(zip·Inno·MSIX·winget) 결정, 실행 중 exe rename 교체·롤백 검증 | MK-217 | boost Phase 4 | OWE-837 |
| MK-219 | Authenticode 서명(유료 인증서 — 사람 결정) | M11 | P3 | 사람 결정(MK-104와 같은 성격) | MK-218 | boost Phase 4 | OWE-838 |
| MK-220 | release 워크플로 Windows 잡 + Windows 메모리 게이트 | M11 | P3 | release 워크플로 Windows 잡, 메모리 게이트 Windows판(지표는 MK-209에서 정의) | MK-218 | boost Phase 4 | OWE-839 |
| MK-221 | 원격·부가: krill-link·모바일 게이트웨이·krill-hosts SSH 포워딩 Windows화 | M11 | P3 | tokio Unix 대체, Windows OpenSSH 로컬 Unix 소켓 `-L` 지원 확인(안 되면 루프백 TCP + 토큰) | MK-205 | boost Phase 5 | OWE-840 |
| MK-222 | (보류) WSL2 백엔드 | M11 | P3 | 보류 — 네이티브 스파이크 실패 시 재검토 | — | boost 전략 B | OWE-841 |
| MK-223 | [M12 Epic] Orca 심층 패리티 (워크트리 엣지케이스·스케줄러·오케스트레이션·AI Vault·Fan-Out/Hunk/PR 머지) | M12 | P0 | 하위 MK-224~229 전부 통과, `scripts/check.sh` 0 | MK-172~187 | Orca 감사 | OWE-918 |
| MK-224 | 워크트리·프로바이더·훅 엣지케이스 하드닝 (서브모듈·심링크·`.worktreeinclude` 글롭·Codex `SUN_LEN`·서브에이전트 훅 보호) | M12 | P0 | 서브모듈 워크트리 삭제·심링크 자기참조 루프 방지·글롭 복사·Codex 단축 홈·서브에이전트 훅 보호 테스트 통과 | MK-042, MK-043, MK-173 | Orca 감사 | OWE-919 |
| MK-225 | 워크트리 & 세션 CLI 심층 패리티 (`--json`·`--delete-branch`·재귀 자식 워크트리 안전 삭제·`--cwd`·`--append`) | M12 | P1 | `krill worktree list/create/remove` 및 `krill new/read/send` 신규 플래그·재귀 자식 안전 삭제 테스트 통과 | MK-042, MK-043, MK-045 | Orca 감사 | OWE-920 |
| MK-226 | 스케줄 자동화 실행 이력 상세 CLI (`runs`/`show`/`cancel`/`--enabled`/`--precheck`) & 타임아웃 프로세스 그룹 정리 | M12 | P1 | `krill schedule` 서브커맨드 확장 및 타임아웃 시 음수 PGID 그룹 시그널 정리 테스트 통과 | MK-101, MK-186 | Orca 감사 | OWE-921 |
| MK-227 | 오케스트레이션 `ask --question-id` 멱등 재대기·`worker start --preamble` 3모드·스킬 공유 `--include- untracked` | M12 | P1 | `question_id` 중복 생성 방지·재대기, `preamble` 모드별 시스템 지시문 제어, untracked 스킬 동기화 테스트 통과 | MK-114, MK-115 | Orca 감사 | OWE-922 |
| MK-228 | AI History Vault 대용량 JSONL(≤ 64 MiB) Head+Tail 분할 스캔·증분 mtime 캐시·최신순 정렬 | M12 | P1 | 8 MiB 초과 JSONL Head+Tail 파싱 및 `(path, mtime_ns, size)` 증분 캐시 재사용 테스트 통과 | MK-181 | Orca 감사 | OWE-923 |
| MK-229 | 멀티 에이전트 Fan-Out 워크트리 생성 & 초기 프롬프트·Git Hunk/Line 스테이징·Unified/Split Diff·인앱 PR 머지 | M12 | P1 | 다중 에이전트 동시 워크트리 스폰·초기 프롬프트 주입, Hunk/Line stage/unstage/discard, PR Squash/Rebase/Merge 및 Draft 전환 테스트 통과 | MK-042, MK-054, MK-085, MK-185 | Orca 감사 | OWE-924 |
| MK-240 | [Epic] Krill 하이브리드 채팅 UI (`krill-holder` PTY 영속 + Zero-State 트랜스크립트 투영 + `[Chat \| Term]` 1-탭 전환) | M12 | P1 | 하위 MK-241~243 전부 완료, `scripts/check.sh` 0 | MK-125, MK-184 | M12 Hybrid Chat | OWE-943 |
| MK-241 | [Phase 1] 하이브리드 채팅 기반: 세션 바인딩(`transcript_path`·로그인 셸 자동 귀속) & 안전 입력 파이프라인(`Bracketed Paste` + 60ms 분리 `\r`) | M12 | P1 | `StoredLaunch::transcript_path` 기록, 로그인 셸 수동 실행 에이전트 첫 훅 자동 귀속, `Action::Prompt`/`Action::Photo` Bracketed Paste + 60ms 분리 `\r`, `Action::CreateTerminal { cwd_session, agent }` 단위 테스트 통과 | MK-125 | M12 Phase 1 | OWE-944 |
| MK-242 | [Phase 2] Zero-State 증분 `ChatItem` 리더(`Action::ChatPoll`) & 모바일 PWA 채팅 뷰(`screens/chat.js` + `[Chat \| Term]` 토글 + 인라인 Diff 모달) | M12 | P1 | 4대 에이전트(`claude`·`codex`·`agy`·`opencode`) 오프셋 기반 증분 `ChatItem` 정규화, 모바일 `[Chat \| Term]` 1-탭 토글, Live Activity Strip, `Action::Git` 인라인 Diff 모달 테스트 통과 | MK-241 | M12 Phase 2 | OWE-945 |
| MK-243 | [Phase 3] 인터랙티브 질문 카드(`Action::AnswerQuestion` + 60ms Paced Keystroke Injector) & 데스크톱 `[Chat \| Term]` 하이브리드 뷰 확장 | M12 | P1 | `AskUserQuestion` 역사 카드의 `review_in_term` 전환, 현재 질문 원자 ID가 없는 JSON `approve`/`answer_question` fail-closed 및 현재 터미널 직접 입력 유지, 데스크톱 `krill-app` 탭 헤더 `[Chat \| Term]` 토글 오버레이(`shell/chat.rs`) 테스트 통과 | MK-241, MK-242 | M12 Phase 3 | OWE-946 |

M11의 `WCHK(X)`는 Windows 포팅 boost 보고서(비공개) §4 정의(Mac에서 Windows 타깃 `cargo check --locked --all-targets` 오류·경고 0)다. 선행 관계는 리니어 blockedBy를 따른다.

