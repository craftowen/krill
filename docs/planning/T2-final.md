# T2 최종. 세션 지속 데몬 아키텍처 (krilld) — 2차 검증 보고서

**결론 (가장 중요한 결정 3가지)**
1. **프로세스를 2단으로 나눈다.** `krill-holder`는 launchd `gui/<uid>` 비지속 잡으로 뜨며, 에이전트 프로세스의 부모이자 PTY master fd 보관자 역할만 한다. 아주 작고 거의 바꾸지 않는다. `krilld`는 holder의 자식으로, 에뮬레이터·IPC·디스크·정책을 맡는다. 이 구조에서는 앱 크래시·재시작·업데이트는 물론 **krilld 크래시와 업그레이드에도 에이전트가 살아남는다.** Orca처럼 프로토콜 버전별 데몬 세대가 공존할 필요도 없다.
2. **krilld가 유일하게 권위 있는 에뮬레이터이자 유일한 터미널 질의 응답자다.** 엔진은 `alacritty_terminal` 0.26 버전을 핀으로 고정한다. UI는 키프레임과 seq가 붙은 원본 바이트를 받아 화면을 재구성한다. 데몬 grid는 "화면 + 소량의 hot 행"으로 묶는다. 1만 행을 grid로 들면 10개에 537MB가 된다(이번 재측정).
3. **기록은 데몬이 직접 한다.** append-only zstd 세그먼트에 쓰고, 쓰기량은 토큰 버킷으로 제한한다. 느린 클라이언트나 앱 부재가 PTY 읽기를 멈추게 할 수 없다. 넘치면 Resync나 gap 표시로 처리한다. Orca는 기록 주체가 앱 main 프로세스라서 앱이 닫힌 동안 디스크 기록이 비고, 앱 main은 4일 동안 16.9GB를 썼다(이번 재측정).

- 작성일: 2026-10-14
- 검증 환경: Mac18,5 / 32GB / macOS 27.0.1(26A434) / rustc 1.98.1
- 기준 소스: Orca HEAD `a0c46b40`(`.ref/orca`), 설치 앱 v1.4.224(프로토콜 v43 데몬 실행 중)
- 이 문서의 파일:줄은 따로 적지 않으면 `.ref/orca` 기준 상대경로다.
- 외부 저장소 기준 커밋: tmux `e5b3eb0`, zellij `519a237`, abduco `8c32909`, dtach `b027c27`, shpool `3c41df9a`, mosh `decd9b7`, wezterm `e2a98e1b`, zed `c403bc6a`, ghostty `291fd050`, zmx `2d23c0d4`.
- **추정**이라고 적은 수치는 측정하지 못한 값이다.

---

## 1. 이전 조사(T2-A) 정정

1차 보고서의 인용을 하나씩 열어 확인했다. Orca 소스 줄 번호는 대부분 맞았다(아래에 없는 인용은 확인 결과 일치). 틀렸거나 보완이 필요한 것만 적는다.

| # | 1차 주장 | 실제 | 근거 |
|---|---|---|---|
| C1 | holder의 PTY 생성은 `rustix`로 openpty를 직접 구현하는 것을 권장 | **rustix 1.1.5에는 `openpty`가 없다.** `rustix::pty`에는 `openpt`/`grantpt`/`unlockpt`/`ptsname`만 있고, `not_implemented.rs`가 `rustix-openpty` 크레이트로 안내한다. `nix::pty::openpty` 또는 `rustix-openpty`를 써야 한다 | crates.io rustix 1.1.5 소스(`src/pty.rs`, `src/not_implemented.rs`) |
| C2 | rustix `sendmsg` + `ScmRights`, 피어 UID 검사 | fd 전달 enum 이름은 `SendAncillaryMessage::ScmRights`이고 macOS에서 쓸 수 있다. 하지만 **`rustix::net::sockopt::socket_peercred`는 Linux 전용**이다. macOS 피어 확인에는 `nix::unistd::getpeereid`나 sockopt `LocalPeerCred`/`LocalPeerPid`/`LocalPeerToken`(감사 토큰)을 쓴다 | rustix 1.1.5, nix 0.31.3 소스 |
| C3 | alacritty는 질의 응답을 `Event::PtyWrite`로 내보내므로 PTY에 그대로 쓰면 된다 | 일부만 맞다. **PtyWrite로 직접 응답하는 것**: DA1(`ESC[?6c`), DA2, DSR 5/6(CPR), DECRQM, CSI 18t. **OSC 4/10/11 색 질의**는 `Event::ColorRequest`로 오므로 앱이 응답 문자열을 만들어야 한다. **CSI 14t**는 `TextAreaSizeRequest`로 오며 픽셀 크기가 필요하다. **XTVERSION, DECXCPR, OSC 52 읽기, DECRQSS, XTGETTCAP에는 응답하지 않는다.** kitty 키보드 질의 `CSI ? u`는 `Config.kitty_keyboard=true`일 때만 응답하고 기본값은 false다 | `Processor::advance` 실측 프로브(`/tmp/t2verify`) |
| C4 | Orca에서 질의 응답 권한은 렌더러에 있다 | 지금 HEAD는 **3중 구조**다. 보이는 패널은 렌더러 xterm이 응답한다. 숨은 패널은 **Electron main의 런타임 헤드리스 에뮬레이터**가 응답한다(Phase 5). 데몬 에뮬레이터는 시작 장벽 동안의 DA1만 예외로 응답한다. 따라서 Orca는 터미널 하나에 에뮬레이터를 최대 3개(데몬·main·렌더러) 둔다. "앱이 닫히면 응답자가 없다"는 결론은 그대로 유효하다 | `src/main/runtime/terminal-model-query-authority.ts:1-45`, `src/main/runtime/terminal/output/headless-terminal-state.ts:13-57`, `src/main/daemon/session-output-plane.ts:53-57` |
| C5 | holder 크래시 → 마지막 master fd가 닫혀 세션 소실 | krilld도 SCM_RIGHTS로 같은 master fd를 들고 있으므로 holder만 죽으면 **마지막 close가 아니다.** krilld가 살아 있고 launchd가 프로세스 그룹을 죽이지 않으면(`AbandonProcessGroup=true`) 세션은 산다. 다만 부모가 사라져 **종료코드를 잃고**, 종료 감지는 kqueue `NOTE_EXIT`로만 할 수 있다(이 부분은 1차 실험과 같은 의미론). holder와 krilld가 둘 다 죽을 때만 소실된다 | `man launchd.plist` AbandonProcessGroup 항목. 1차 §3.4 SCM 실험 |
| C6 | launchd `gui/<uid>` 잡은 로그아웃 때 launchd가 정리하므로 Orca식 "죽은 로그인 세션 감시"가 구조적으로 불필요 | **추정으로 내린다.** Orca의 감시는 fork 경로 데몬이 로그아웃 뒤에도 살아남아 `login(1)`과 DNS를 잃는 문제 때문에 생겼다. launchd 잡의 로그아웃 거동은 이번에 실험하지 못했다 | `src/main/daemon/macos-login-session-death-watch.ts:31-45` |
| C7 | 설치 Orca 데몬: PTY 17개, footprint 172MB, 세션당 약 6.5MB | 재측정(같은 날 나중 시점): PTY 20개, footprint 171~177MB, 세션당 약 5.8MB(=(175−60)/20, 추정). 고아 v40·v41 데몬은 각각 59MB·69MB로 그대로였다. 추가로 각 PTY의 `/usr/bin/login` 래퍼가 RSS 7.3~8.2MB씩 차지한다(setuid라 footprint는 측정 불가) | `ps`·`footprint`·`proc_pid_rusage` 실측 |
| C8 | Orca main 디스크 쓰기: 4일 16.5GB, 30초 표본 4.76MB | 4일 1시간 누적 16.9GB(평균 약 2.9MB/분)로 일치한다. 이번 60초 표본은 26.6MB였다(부하에 따라 크게 변함). 데몬은 누적 78.5MB, 60초 0.15MB다. 최근 2분 안에 바뀐 main 쪽 파일 후보는 `orca-stats.json`(2.0MB), `mobile-notification-dismissals.json`(1.1MB), `profile-state.db-wal`, `orchestration.db-wal`, `logs/main.trace.ndjson`, `ai-vault/session-search.sqlite`(1.04GB)였다. 터미널 히스토리는 쓰기의 일부일 뿐이다(분해는 미확인) | `proc_pid_rusage` V2 실측, `find -mmin -2` |
| C9 | "분당 2GB" 원인은 미확인 | 미확인은 그대로다. 다만 단서가 두 가지 있다. 이슈 #27838에서 macOS 리소스 보고서는 **앱이 띄운 자식 프로세스의 쓰기를 앱의 resource coalition으로 묶어** 보고한다(Orca 아래의 `go`가 1,892초에 34.36GB를 씀). 이슈 #8118(닫힘)은 "저장할 때마다 28.5MB 상태 블롭 전체 재작성"이다. 즉 "Orca가 분당 2GB"는 에이전트 빌드의 쓰기가 Orca 이름으로 보고된 것일 수 있다(**가설**) | https://github.com/stablyai/orca/issues/27838 , https://github.com/stablyai/orca/issues/8118 |
| C10 | Orca 대기 출력 상한 2MB | 단위는 바이트가 아니라 **UTF-16 코드 단위 2M**이다. 와이어에서는 최악 약 6배가 된다 | `src/main/daemon/session-output-plane.ts:11-13` |
| C11 | keep-tail 드롭으로 "앞부분 기록을 버린다" | 버리는 것은 **숨은 패널로 가는 스트림 사본**이다. 데몬 에뮬레이터는 전체 모델을 들고 있고, 패널을 다시 보이면 스냅샷으로 복원한다. 실제 손실은 에뮬레이터 스크롤백(5,000행)을 넘는 부분에 한정된다 | `src/main/daemon/daemon-stream-keep-tail-drop.ts:1-8` |
| C12 | 크래시 루프 스로틀 "60초 창" | 60초 안에 5회를 넘으면 거부한다 | `src/main/daemon/daemon-respawn-throttle.ts:26-30` |
| C13 | shpool은 UID와 **실행 파일 경로**를 검사 | UID가 다를 때만 거부한다. 실행 파일 경로가 다르면 `warn!`만 남긴다 | shpool `libshpool/src/daemon/peer.rs` (Linux L39-42, macOS L91-94) |
| C14 | shpool `shpool_vt100` 1024열 고정 | 기본값이 1024열이고 `vt100_output_spool_width`로 바꿀 수 있다. 대체 엔진 `shpool-vterm`도 있다(`session_restore_engine = vterm`) | shpool `session_restore.rs#L24`, `Cargo.toml#L37-38` |
| C15 | mosh 프레임 20~250ms 근거가 `completeterminal.h#L45-82` | 값은 맞지만 위치가 다르다. 정확한 위치는 `src/network/transportsender.h#L49-52`(`SEND_INTERVAL_MIN 20`, `MAX 250`)다. `completeterminal.h`에는 `ECHO_TIMEOUT 50`만 있다 | mosh `decd9b7` |
| C16 | zellij daemonize가 `server_launch.rs#L70-93`에 있음 | `server_launch.rs`는 `--server <sock>`을 실행만 한다. 실제 daemonize는 `zellij-server/src/lib.rs#L3214-3228`이다. 소켓 0o1700은 `zellij-server/src/lib.rs#L3544`다. **다중 클라이언트 크기는 탭 단위 최소값**이다(`screen.rs#L2880-2925`). 1차가 미확인으로 남긴 항목을 채운다 | zellij `519a237` |
| C17 | tmux는 소켓 디렉터리 0700을 검사 | 소유 UID와 "other 비트 없음"(`TMUX_SOCK_PERM`=7)만 검사한다. group 비트는 보지 않으므로 엄격한 0700 검사가 아니다. 실제 `daemon()` 호출은 `proc.c#L388`이다. 크기 정책 기본값 `latest`는 `options-table.c#L1817-1821`에서 확인했다 | tmux `e5b3eb0` |
| C18 | Ghostty는 창·탭·cwd 복원만 하고 tmux control mode는 작업 중 | 프로세스를 유지하지 못한다는 점은 맞다. 다만 PR #14671("restore the active screen from pane state")이 **2026-10-13에 머지**되어 tmux control mode 뷰어가 실제로 진행 중이다. 또 **libghostty-vt**가 공개돼 있고, Rust 바인딩 `libghostty-vt` 0.2.2(2026-09-28, MIT OR Apache-2.0)에는 VT 직렬화 `Formatter`가 있다. 이 라이브러리 기반의 세션 유지 도구(zmx 등)도 나왔다 | https://github.com/ghostty-org/ghostty/pull/14671 , https://github.com/neurosnap/zmx , crates.io libghostty-vt |
| C19 | 9개 도구 모두 서버 업그레이드나 크래시를 견디지 못함 | 맞다. 추가로 확인한 zmx도 README "known issues"(L573)에 "IPC가 바뀐 버전으로 업그레이드하면 모든 세션이 종료된다"고 적혀 있다 | zmx `2d23c0d4` |
| C20 | 성공 기준 S-4 "krilld+holder ≤ 60MB" | 1차 §6 예산표의 기본 모드(hot 1000행) 합계 70~75MB와 **내부적으로 모순**이다. 이 보고서에서는 기준을 "화면+hot 행 정책 적용 뒤 ≤ 60MB, 최악 ≤ 90MB"로 다시 잡는다(§3.9) | 1차 §0, §6 |
| C21 | 1차 크레이트 표의 버전·날짜·라이선스 | 22개 모두 crates.io API로 다시 확인했고 **일치**한다(zstd 0.14.2 BSD-3-Clause, sendfd Apache-2.0 OR BSD-3-Clause 포함). bincode 3.0.0 = `compile_error!` 한 줄, RUSTSEC-2025-0141(informational: unmaintained)도 맞다 | crates.io API, GitHub, RustSec |
| C22 | alacritty 메모리 실측(200×50: 히스토리 10000 → 51.1MB/개, 1000 → 5.59MB, 0 → 0.53MB) | 독립 재측정으로 **일치**한다. 10개 기준 히스토리 10000은 537MB, 1000은 60MB, 0은 6.8MB(max footprint)다. `Cell`은 24B, vt100 `Cell`은 32B다 | `/tmp/t2verify` 프로브 |
| C23 | exec 핸드오프(같은 PID, 손실 없음, 종료코드 수신)와 macOS PTY 버퍼 약 1KB | 독립 재현으로 **일치**한다. gen1이 tick1~4, gen2가 tick5~8을 읽었고 PID가 같았으며 `waitpid`로 종료코드 7을 받았다. slave에서 non-blocking으로 쓰면 raw 모드 1024B, cooked 모드 1009B에서 EAGAIN이 났다 | `/tmp/t2v/spk/handoff.py`, `ptybuf.py` |

---

## 2. 확정 결정 사항

"되돌리기 비용"은 출시 뒤 이 결정을 바꿀 때 드는 비용이다.

| # | 결정 | 권고안 | 대안 | 근거 | 되돌리기 비용 |
|---|---|---|---|---|---|
| D1 | 프로세스 분리 | **2단 구조.** holder는 에이전트의 부모이자 fd 보관자이고 launchd 잡이다. krilld는 holder의 자식이다 | (a) 1단 krilld + exec 핸드오프 업그레이드. (b) 세션마다 프로세스(abduco·dtach·zmx 방식) | Orca는 데몬이 죽으면 전 세션이 죽는다(`src/main/daemon/daemon-pty-daemon-recovery.ts:58`). 조사한 10개 도구 모두 서버 크래시에 취약하다. exec와 fd 전달 의미론은 재현으로 확인했다(C23). (b)는 세션 격리가 가장 좋지만 업그레이드 대상이 N개가 되고, 앱이 N개 소켓을 관리해야 한다 | **높음.** 출시 뒤에 PTY의 부모를 바꾸면 기존 세션을 넘길 수 없다. 그래서 M2 첫 출시부터 2단이어야 한다 |
| D2 | 기동 방식 | **서명된 헬퍼 번들을 CDHash 이름의 안정 경로에 복사**하고 `launchctl bootstrap gui/<uid>`로 비지속 잡을 띄운다(`RunAtLoad=true`, `KeepAlive=false`, `AbandonProcessGroup=true`, `ProcessType=Interactive`, `AssociatedBundleIdentifiers`). 헬퍼는 **앱과 같은 identifier·팀으로 서명**한다. 개발용 ad hoc 빌드는 `posix_spawn`+`setsid`로 대체한다 | SMAppService.agent(번들 안 경로라 업데이트 중 TCC 거부 위험이 남음). 앱 fork만 사용(Orca 구 방식) | `docs/reference/macos-daemon-tcc-attribution.md:9-13`(업데이트 중 TCC 거부가 영구화됨), `:49-54`(같은 identifier로 기존 권한 상속), `:45-47`(ad hoc 빌드는 헬퍼 경로 불가), `src/main/daemon/macos-daemon-launchd.ts:62-78,120,169-172`. `man launchd.plist`에 따르면 ProcessType을 지정하지 않으면 CPU·I/O가 스로틀된다 | 중간. 기동 경로만 바꾸면 되고 세션 포맷과는 무관하다 |
| D3 | 상태 엔진 | **krilld가 세션마다 `alacritty_terminal::Term`(0.26 핀)을 둔다. 키프레임은 엔진과 무관한 Minikrill 자체 포맷이다** | libghostty-vt(직렬화 Formatter가 있음. 다만 빌드에 Zig 툴체인과 git fetch가 필요하고, `!Send`이며 pre-1.0이다). vt100 0.16(`state_formatted`가 있으나 메모리가 33% 더 크고 화면만 다룸). 셀 diff 원격 렌더(WezTerm·mosh) | 앱 없이도 상태가 최신이고 재접속 즉시 화면이 뜬다. alacritty에는 직렬화기가 없지만(C21 검증), 키프레임을 자체 포맷으로 두면 엔진을 바꿔도 디스크·와이어 포맷은 유지된다 | 중간. 키프레임 포맷이 엔진과 독립이면 엔진 교체는 데몬 내부 일이다 |
| D4 | 질의 응답 권한 | **krilld만 응답하고 UI 에뮬레이터는 응답을 끈다.** 재생·시드 중에는 절대 응답하지 않는다. 색 질의는 UI가 마지막으로 보낸 테마로 답하고, 테마를 받기 전에는 침묵한다 | Orca식 렌더러·main 권위 | Orca는 권위를 앱 쪽에 두어서 앱이 닫히면 응답자가 없다(C4). 재생 중 응답이 새 셸에 입력으로 새는 문제가 있었다(`src/renderer/src/components/terminal-pane/terminal-capability-replies.ts:156-157`, `src/main/daemon/headless-emulator.ts:74-75`). 테마 전 침묵은 OSC 11 기본 검정 버그를 막기 위한 것이다(`src/main/daemon/terminal-view-attribute-responder.ts:23-26`) | 낮음 |
| D5 | 데몬 grid 크기 | **붙은 세션은 화면 + hot 1,000행, 30분 넘게 미접속이면 화면만.** 깊은 기록은 압축 바이트로 둔다 | 1만 행 grid | 200×50 실측 기준: 1만 행 53.7MB/개, 1,000행 6.0MB/개, 화면만 0.68MB/개(C22) | 낮음(설정값) |
| D6 | IPC | **UDS 하나**(디렉터리 0700, 소켓 0600, `getpeereid`로 UID 검사). 프레임은 `[u32 LE len][u8 kind][body]`, 최대 1MiB. 제어는 postcard, 데이터는 raw. Hello에서 버전 범위와 feature 비트를 협상한다. **소켓 이름에 버전을 넣지 않는다.** 로컬에는 토큰 파일을 쓰지 않고, 세션 내부에서 온 연결은 권한을 좁힌다(D11) | Orca식 버전별 소켓과 토큰. WezTerm식 버전 불일치 하드 실패 | Orca는 v1~v45 버전 게이트(`src/main/daemon/daemon-protocol-version.ts:1-42`)와 고아 데몬(실측 v40·v41)을 남겼다. Orca 루트 AGENTS.md:118 "decoders drop unknown opcodes silently → 새 opcode는 capability 협상 필수". bincode는 유지보수 종료(C21) | 높음. 와이어 포맷은 클라이언트(앱·CLI·hook)가 많을수록 바꾸기 어렵다 |
| D7 | 재접속 | **seq 이어받기 → 키프레임 + 재생 → 지연 히스토리 로드.** 느린 클라이언트는 큐 상한을 넘으면 `Resync`로 처리하고, PTY 읽기를 멈추지 않는다 | Orca식 생산자 정지 + keep-tail 드롭 | Orca는 소비자가 멈추면 셸이 최대 60초 블록된다(`src/main/daemon/session-producer-pause.ts:7-13`). 보완 전에는 RSS가 1.38GB까지 올랐다(`docs/audits/daemon-stream-retention/README.md:17-25`) | 중간 |
| D8 | 디스크 | **append-only 세그먼트**(키프레임 + zstd-3 레코드). 토큰 버킷은 지속 8MB/분, 버스트 32MB(압축 후)다. fsync는 하지 않는다. 세션당 32MB, 전역 1GB, 닫힌 세션은 7일 보존 | Orca식 5초 전체 체크포인트 JSON + 5MB 로그. 순수 Rust `lz4_flex`(압축률 7×) | 실데이터 압축률은 zstd-3 15.5×(1차 실측). Orca의 체크포인트 상한은 200MB(`src/main/daemon/terminal-history-file-limits.ts:4`), 쿨다운은 45초(`src/main/daemon/daemon-pty-runtime-state.ts:149-152`)다 | 중간. 세그먼트 포맷에 버전 필드를 둔다 |
| D9 | 업그레이드 | **krilld는 holder가 새 바이너리로 재기동한다. holder는 exec 핸드오프로 바꾼다. holder↔krilld 내부 프로토콜은 동결한다(추가만 허용). 데몬 다운그레이드는 자동으로 하지 않는다** | 1단 exec + 전체 상태 직렬화. 세대 공존(Orca) | C23 재현. Orca는 다운그레이드 때 "newer daemon hold" 화면이 필요했다(`src/main/daemon/daemon-newer-protocol-hold.ts:21-25`) | 높음(holder 프로토콜) |
| D10 | 다중 클라이언트 | `interactive`와 `observer` 역할을 둔다. 크기는 **가장 최근에 입력하거나 포커스한 interactive 클라이언트**가 정한다 | zellij식 최소값. shpool식 배타 attach | tmux 기본값 `latest`(`options-table.c#L1817-1821`), zellij 탭 단위 최소(`screen.rs#L2880-2925`) | 낮음 |
| D11 | 세션 내부 발 연결 권한 | **연결한 프로세스가 어떤 세션의 후손인지 판정**한다(`LOCAL_PEERPID` + libproc ppid 체인). 후손이면 기본으로 그 세션 범위로 제한하고, 다른 세션 제어는 사용자 설정이나 명시 grant로만 허용한다 | 제한 없음(같은 UID면 전부 허용). 토큰 파일 | 같은 UID인 에이전트(프롬프트 인젝션된 에이전트 포함)가 `mk send`로 **다른 에이전트 세션에 키 입력을 넣을 수 있다.** 토큰 파일은 같은 UID가 읽을 수 있어 막지 못한다. 근거는 nix sockopt `LocalPeerPid`/`LocalPeerToken`(C2) | 낮음~중간(정책) |
| D12 | 전원 | krilld가 `IOPMAssertionCreateWithName`(objc2-io-kit `pwr_mgt`)을 직접 잡는다. 하트비트는 잠자기 시간을 뺀다 | Orca식 `caffeinate -i -s` 자식 | `src/main/macos-system-sleep-assertion.ts:70`. `man caffeinate`: `-s`는 AC 전원에서만 유효 | 낮음 |

---

## 3. 상세 설계와 요구사항

### 3.1 Orca가 하는 방식 (검증된 사실만)

**프로세스 구조 (설치 v1.4.224 실측, 2026-10-14)**
- main(8231, footprint 408MB), 렌더러(8400, 840~860MB), 데몬(8401, `daemon-v43.sock`, PPID=8231, 자기 PGID, footprint 171~177MB, PTY 20개)이 떠 있다.
- 고아 데몬 76415(v40, 7일, 59MB)와 93672(v41, 5일, 69MB)는 PPID 1이다.
- "Orca 메인 프로세스가 2개"로 보이는 이유: 다른 하나는 `Orca …/out/cli/index.js orchestration check`, 즉 같은 바이너리를 Node 모드로 띄운 CLI 호출이다.
- 터미널은 모두 `/usr/bin/login -flpq <user> /bin/bash … orca-tcc-login /bin/zsh -l` 래퍼를 거친다. TCC 귀속과 PAM 때문이다(`src/main/providers/macos-tcc-login-shell.ts`).

**분리 방식 (fork 경로)**
- `detached: true`, `stdio: ['ignore','ignore','pipe','ipc']`로 띄운다(`src/main/daemon/daemon-launched-child-spawn.ts:110-114`).
- 준비 메시지를 받으면 `child.disconnect(); child.unref()`로 놓아준다(`src/main/daemon/daemon-launched-child.ts:215-220`).

**HEAD의 macOS 경로 (설치본에는 아직 없음)**
- `Contents/Helpers/Orca Terminal Host.app`(약 124MiB, Node 포함)을 `userData/daemon-host/macos/terminal-host-<cdhash>/app.noindex/`에 APFS clone으로 복사한다. `codesign --verify`로 확인한 뒤 `rename`으로 게시하고, launchd 비지속 잡으로 실행한다(`docs/reference/macos-daemon-tcc-attribution.md:15-39`).
- 잡 라벨은 **프로필·프로토콜마다** 다르다(`:32-33`). 즉 세대 공존은 계속된다.
- 같은 경로에서 처음 실행할 때 약 1초가 걸린다(`:65-67`). 다른 복사본은 `lsof +D`가 비었을 때만 지운다(`:94-100`).

**상태 보관과 복원**
- 세션마다 `@xterm/headless` + `SerializeAddon` + `Unicode11Addon`을 쓰고, 기본 스크롤백은 5,000행이다(`src/main/daemon/headless-emulator.ts:59,90,101`, `src/shared/terminal-scrollback-policy.ts:1`).
- 스냅샷은 절대 커서로 복원한다(`headless-emulator.ts:254`). 이스케이프 시퀀스 중간에서 끊긴 꼬리도 따로 보관한다(`:76,280`).
- seq는 원본 바이트 누적 수다(`session-output-plane.ts:208-209`). 리사이즈와 clear도 레코드로 남긴다(`:108-121`).

**기록**
- `checkpoint.json` + `output.log`(5MB 상한)로 기록한다(`src/main/daemon/terminal-history-session-writer.ts:24-25,38-40`).
- 기록 주체는 **앱 main 쪽 `DaemonPtyAdapter`**다. `HistoryManager`는 `daemon-pty-runtime-state.ts:219`에서만 생성된다(전체 grep 확인).
- 체크포인트 타이머는 5초이고, dirty 세션만 깨운다(`daemon-pty-checkpoint-scheduler.ts:28`).

**버전 처리**
- 소켓 이름에 버전이 들어간다(`src/main/daemon/daemon-spawner.ts:133-146`).
- 새 앱은 v1~v44 소켓을 모두 탐색해 legacy adapter로 붙고, 그 adapter는 재기동하지 않는다(`src/main/daemon/daemon-legacy-adapters.ts:26-60`).
- **셸 래퍼 같은 동작 변경에도 프로토콜을 올려** 새 데몬을 강제한다(`daemon-protocol-version.ts:1-6`). 그 결과가 세대 공존과 빈 고아 데몬이다.

**자원 상한**
- 호스트 터미널은 최대 256개다. macOS 전체 PTY 한도 `kern.tty.ptmx_max`=511의 절반이다(`src/main/daemon/host-terminal-ledger.ts:22-25`). 이 맥에서도 511로 확인했다.
- 후손 프로세스 폭증 감시는 `ps`를 활성 시 5초, 유휴 시 30초마다 실행해서 한다(`src/main/daemon/daemon-process-growth-monitor.ts:18-19`).

**코드 규모**
- `src/main/daemon`의 비테스트 파일은 289개 36,174줄이고, 테스트 파일은 263개다(재확인).

### 3.2 Orca에서 끊김이 생기는 지점 (Minikrill 대응)

| # | 지점 | 근거 | Minikrill 대응 |
|---|---|---|---|
| 1 | 데몬이 죽으면 그 데몬의 모든 세션이 죽음 | `daemon-pty-daemon-recovery.ts:58` | D1 holder 분리 |
| 2 | 떠나는 데몬이 후임 데몬의 소켓을 unlink함 → "키 입력은 받는데 실행 안 되는 터미널". 23개 결함 끝에 link→생존 증명→rename 규약을 만듦 | `src/main/daemon/AGENTS.md:13-50` | 데몬이 하나뿐이고 holder가 krilld의 유일한 기동자다. flock으로 단일 인스턴스를 보장한다. bind를 임시 이름에 한 뒤 rename으로 게시하고, 종료할 때 unlink하지 않는다 |
| 3 | 업데이트 중 TCC 거부가 영구화됨 | `macos-daemon-tcc-attribution.md:9-13` | D2 |
| 4 | 로그인 세션이 죽은 뒤 데몬이 `login(1)`과 DNS를 잃음 | `macos-login-session-death-watch.ts:31-45` | launchd 잡의 로그아웃 거동을 S7에서 확인. 필요하면 같은 감시를 둔다 |
| 5 | 숨은 패널 스트림 keep-tail 드롭(세션 512K, 전역 2M 문자) | `daemon-stream-keep-tail-drop.ts:51-64` | 전송 계층과 기록 계층을 분리한다. 기록은 토큰 버킷 밖에서는 버리지 않는다 |
| 6 | 대기 출력이 2M 문자를 넘으면 레코드를 버리고 전체 스냅샷으로 대체 | `session-output-plane.ts:11-13` | 압축 링 + 키프레임 |
| 7 | 소비자가 멈추면 생산자 정지 → 셸 블록. 멈춘 소비자는 60초 뒤에야 해제 | `daemon-stream-backpressure.ts:4-7`, `session-producer-pause.ts:7-13` | D7: PTY 읽기를 절대 멈추지 않는다 |
| 8 | 연결 5초, 요청 30초 타임아웃. 60초 안에 5회 실패하면 재기동 거부 | `client.ts:28-31`, `daemon-respawn-throttle.ts:26-30` | 잠자기를 고려한 하트비트. 재기동은 holder가 지수 백오프로 한다 |
| 9 | 앱이 없으면 질의 응답자가 없음 | C4 | D4 |
| 10 | 데이터 경로가 3홉(데몬 → main → 렌더러)이고 에뮬레이터가 최대 3개 | C4 | 앱 UI가 데몬에 직접 연결하고 에뮬레이터는 2개(데몬 + 보이는 UI)만 둔다 |
| 11 | 앱이 닫힌 동안 디스크 기록 공백 | §3.1 | 데몬이 직접 기록한다(D8) |
| 12 | 앱을 다운그레이드하면 새 데몬과 말이 안 통해 "hold" 화면 | `daemon-newer-protocol-hold.ts:21-25` | 데몬은 N-1 클라이언트를 받는다. 다운그레이드한 앱은 observer와 기본 조작만 허용하고, 데몬은 낮추지 않는다 |
| 13 | 데몬 기동이 늦으면 fail-open으로 앱 내부 로컬 PTY를 띄움 → 그 터미널은 앱과 함께 죽음 | `daemon-provider-init.ts:79` 주석 | 로컬 대체 PTY를 두지 않는다. 기동 실패를 화면에 드러내고 `mk doctor`로 진단한다 |
| 14 | 재생한 스크롤백 속 옛 질의에 응답하면 새 셸에 쓰레기 입력이 들어감 | `terminal-capability-replies.ts:156-157` | 재생·시드 중에는 응답을 막는 게이트(D4) |

### 3.3 기존 해법 비교 (재확인한 값)

| 도구 | PTY 소유 | 재접속 복원 | 전송·인증 | 버전 | 다중 클라이언트 | 서버 업그레이드 / 크래시 |
|---|---|---|---|---|---|---|
| tmux | 서버 하나. 첫 클라이언트가 `daemon()` + flock으로 띄움(`server.c#L177-190`, `client.c#L78-102`). macOS는 `daemon_darwin`으로 per-user bootstrap에 다시 붙음(`compat/daemon-darwin.c#L63-76`) | 서버 grid로 다시 그림 | UDS, 소유 UID + other 비트 검사, imsg | `PROTOCOL_VERSION 8`, 불일치 시 거부(`proc.c#L133-147`) | `latest` | 둘 다 소실 |
| zellij | 세션마다 서버(`zellij-server/src/lib.rs#L3214-3228`) | grid 다시 그림. 크래시 뒤에는 레이아웃과 명령만 부활 | UDS 0o1700, u32 LE + protobuf(`ipc.rs#L533-562`) | `contract_version_1` 디렉터리 | 탭 단위 최소 | 소실 / 레이아웃만 |
| abduco | 세션마다 프로세스(이중 fork + setsid, `abduco.c#L418-422`) | 없음 | 고정 4KB 패킷 | 없음 | `-r` 읽기 전용 | 세션별 |
| dtach | 세션마다 프로세스(`master.c#L450-462`) | 없음(none/ctrl_l/winch). 클라이언트가 0개일 때 출력은 버림 | UDS 0600 | 없음 | 가능 | 세션별 |
| shpool (Rust) | 데몬 하나, 자동 daemonize(`daemonize.rs#L40-205`) | `screen`(기본)/`lines`/`simple`. vt100 1024열 기본 | UDS, UID 검사(실행 파일 불일치는 경고만). msgpack + `u8 kind/u32 len`, 500ms 하트비트 | VersionHeader 선송신, 불일치는 경고만 | 배타(Busy, `-f`) | 소실 |
| mosh | 연결마다 서버 | SSP 상태 diff, 20~250ms(`transportsender.h#L49-52`) | UDP AES-OCB | proto 2 | 원래 클라이언트만 | 소실 |
| WezTerm mux | mux 서버, GUI가 자동 기동(`client.rs#L757-776`) | 서버 모델. dirty line을 끌어옴(`GetPaneRenderChanges` L892, `GetLines` L933) | leb128(bit63 = 압축) + serial + ident | `CODEC_VERSION 46`, 하드 실패(`client.rs#L1160-1171`) | 가능 | 소실 |
| Zed | 앱 내부(alacritty 포크) | cwd·제목만 SQLite에 저장 → 새 셸 | – | – | – | 앱 종료 = 소실 |
| Ghostty | 앱 내부 | 창 복원. tmux control mode 뷰어 진행 중(#14671 머지) | – | – | – | 소실 |
| zmx (Zig) | 세션마다 데몬과 소켓 | ghostty-vt 스냅샷으로 화면 복원 | UDS, 디렉터리 0750 | – | – | IPC 변경 시 전 세션 종료 |

**배울 점**
1. 화면 복원에는 바이트 재생보다 "파싱한 상태 + 꼬리 바이트"(tmux·shpool screen·zmx·Orca)가 맞다.
2. 서버 업그레이드와 크래시를 견디는 선례는 없다. 그래서 holder와 핸드오프를 S1·S2 스파이크로 먼저 검증한다.
3. 버전 처리에서 하드 실패(WezTerm)와 세대 공존(Orca)은 둘 다 피한다. 범위와 feature 비트로 협상한다.
4. macOS에서 `daemon(3)`을 그대로 쓰면 bootstrap namespace가 바뀐다(tmux가 compat 코드로 우회, tmux-MacOSX-pasteboard README L109-130). launchd 잡을 쓰면 이 문제가 생기지 않는다.

### 3.4 프로세스 모델 (D1·D2)

```
launchd (gui/<uid>)
 └─ krill-holder        launchd 잡. 서명된 안정 경로 복사본. 목표 ≤ 800줄
     ├─ krilld          holder의 자식. 에뮬레이터·IPC·디스크·정책
     ├─ agent #1        setsid, 제어 터미널 = pty1, 부모 = holder
     └─ agent #2 …
Minikrill.app / mk CLI / hook ──UDS──> krilld
```

**holder의 책임 (이것만 한다)**
- `openpty`(nix) → `posix_spawn`(`POSIX_SPAWN_SETSID`, `POSIX_SPAWN_CLOEXEC_DEFAULT`) → 자식 쪽에서 `TIOCSCTTY`.
  - `posix_spawn`으로 TIOCSCTTY를 할 수 없으면 fork 뒤 exec 전에 처리한다(S1에서 확정).
- master fd를 **보관만** 하고 읽지 않는다.
- `waitpid`로 종료 상태를 받아 krilld에 알린다.
- krilld를 띄우고, 죽으면 지수 백오프로 다시 띄운다(1초 → 최대 30초).
- 내부 프로토콜(socketpair, 동결): `Spawn{argv, env, cwd, size} → (pid, fd via SCM_RIGHTS)`, `ListSessions → [(id, pid, fd)]`, `Exited{pid, status}`, `Kill{pid, sig}`, `Upgrade{path}`.
- 상태는 `(세션 id, pid, fd)` 목록뿐이다. exec 핸드오프 때는 이 목록을 파이프나 환경변수로 넘긴다.

**krilld의 책임**
- 시작할 때 holder에서 모든 fd를 받는다. 디스크의 마지막 키프레임과 꼬리 바이트로 에뮬레이터를 재구성하고, 읽기를 재개한 뒤 소켓을 게시한다.
- 클라이언트 소켓, 질의 응답, 기록, 전원, 정책, 메트릭을 맡는다.

**기동 경로**
1. 앱이나 CLI가 소켓 연결에 실패하면 안정 경로 복사본을 확인한다(`~/Library/Application Support/Minikrill/host/<cdhash>/Minikrill Terminal Host.app`).
2. 복사본이 없으면 `clonefile` → `codesign --verify --strict -R <앱 DR>` → `rename`으로 게시한다.
3. `launchctl bootstrap gui/<uid> <plist>`를 실행한다. plist는 0600으로 쓰고 bootstrap 직후 지운다.
4. 개발용 ad hoc 빌드는 DR이 cdhash라서 헬퍼가 일치할 수 없다(`macos-daemon-tcc-attribution.md:45-47`). 이때는 `posix_spawn` + `setsid`로 띄우고 "TCC 귀속 불안정" 경고를 남긴다.
5. Rust 헬퍼는 수 MB라서(추정) Orca의 124MiB 복사 비용 문제가 없다.

**holder 크래시 때의 동작 (C5 반영)**
- krilld가 fd를 들고 있으므로 기존 세션은 산다. 대신 종료코드를 잃고, 종료 감지는 kqueue `EVFILT_PROC NOTE_EXIT`로 한다.
- 새 세션은 새 holder 아래에서 만든다. 고아가 된 세션은 UI에 "종료코드 없음"으로 표시한다.
- krilld와 holder가 함께 죽으면 소실된다. 이것이 유일한 단일 장애점이다.

### 3.5 PTY·프로세스 그룹·시그널

- **환경 변수.** `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=Minikrill`, `MINIKRILL_SESSION_ID`, `LANG`(사용자 로케일)을 설정한다.
  - 로그인 셸과 TCC 래퍼(`login -flpq`)를 쓸지는 T4와 S6에서 정한다. 쓰면 세션당 RSS 약 7.5MB가 더 든다(C7).
- **종료 순서.**
  1. `killpg(pgid, SIGHUP)` → 2초 뒤 `SIGTERM` → 3초 뒤 `SIGKILL`.
  2. `setsid`로 빠져나간 후손은 libproc `proc_listchildpids`를 재귀로 따라가 정리한다. Orca에도 같은 문제를 다루는 `src/main/pty-descendant-termination.ts`가 있다.
- **포그라운드 프로세스.** `tcgetpgrp(master)` + `proc_pidpath`로 얻는다. "에이전트가 일하는 중" 판정은 T4가 맡는다.
- **리사이즈.** `TIOCSWINSZ`를 쓴다. 크래시 복구 뒤에는 `killpg(fg, SIGWINCH)`로 다시 그리게 한다.
- **자원 상한.** 세션은 기본 최대 256개다(`ptmx_max` 511의 절반, Orca와 같은 근거). 후손 폭증 감시는 `ps`를 띄우지 않고 libproc로 30초마다 표본을 뜬다.

### 3.6 상태 엔진과 질의 응답 (D3·D4·D5)

- **입력 경로.** PTY 바이트 → seq 증가 → `Processor::advance(&mut Term, bytes)` → 기록 링 → 클라이언트 팬아웃.
  - 세션마다 리더 태스크를 하나 둔다. 에뮬레이터 패닉은 `catch_unwind`로 그 세션에 가둔다. 그 세션만 키프레임에서 다시 만들고 계속 간다.
- **응답 표 (C3 반영).**

| 질의 | 처리 |
|---|---|
| DA1, DA2, DSR 5/6, DECRQM, CSI 18t | alacritty `PtyWrite`를 그대로 PTY에 쓴다 |
| OSC 4/10/11(색) | `ColorRequest` → UI가 마지막으로 푸시한 테마로 응답을 만든다. 푸시 전에는 침묵한다 |
| CSI 14t/16t(픽셀) | UI가 보낸 셀 픽셀 크기로 응답한다. 크기를 모르면 침묵한다 |
| kitty `CSI ? u` | `Config.kitty_keyboard = true`로 켠다. 키 인코딩은 UI가 하므로 플래그를 UI에 동기화한다(T3) |
| XTVERSION, DECXCPR, XTGETTCAP, DECRQSS | alacritty 바깥에서 별도 파서 훅으로 처리한다. 필요한 목록은 S12(에이전트 질의 캡처)로 정한다 |
| OSC 52 읽기 | 응답하지 않는다(보안). 쓰기는 UI 클립보드로 전달한다(T1 이슈 11과 일치) |

- **재생 게이트.** 키프레임 적용, 디스크 재생, 시드 중에는 모든 응답을 버린다. Orca의 `queryReplyForwardingDepth`와 같은 원리를 "이번 write가 라이브 PTY 바이트인가" 플래그 하나로 구현한다.
- **UI 에뮬레이터.** 응답 기능을 끈다. 데몬과 같은 alacritty 버전과 같은 폭 계산을 쓴다. 다만 와이어와 디스크는 자체 키프레임 포맷이라서 버전이 어긋나도 재생은 된다.
- **Grid 정책.** 붙은 세션은 hot 1,000행, 30분 넘게 미접속이면 화면만 둔다. alacritty는 히스토리를 1,000행 단위로 늘리고 줄이므로(`MAX_CACHE_SIZE`), 실제 상한은 설정값 + 1,000행 캐시다.

### 3.7 IPC와 보안 (D6·D11)

- **경로.** `~/Library/Application Support/Minikrill/run/krilld.sock`. 디렉터리 0700, 소켓 0600이다.
  - `sun_path` 104바이트를 넘으면 `/tmp/.minikrill-<uid>/`로 대체한다. Orca도 `/tmp/.orca-daemon-` 접두사를 쓴다(`src/main/daemon/daemon-socket-endpoint-path.ts:18`). 대체 디렉터리는 소유자와 권한을 검사한다.
  - `$TMPDIR` 아래는 청소 대상이다. Orca 주석 "shell rcfiles must live outside swept tmp"(`daemon-launched-child-spawn.ts:104`)가 근거다. 그래서 rcfile과 기록은 그 아래에 두지 않는다.
- **피어 검증 (accept 직후).**
  1. `getpeereid`로 UID가 같은지 본다.
  2. `LOCAL_PEERPID`로 피어 PID를 얻고 ppid 체인을 따라가 "어느 세션의 후손인가"를 판정한다(D11).
  3. 선택 사항: `LOCAL_PEERTOKEN` + security-framework `SecCode`(`GuestAttributes::set_audit_token`)로 피어 코드서명이 Minikrill 팀인지 확인한다.
- **권한 모델.**

| 피어 | 기본 권한 |
|---|---|
| 앱(서명 일치) | 전체 |
| 외부 셸에서 실행한 `mk` | 전체(사용자가 직접 실행한 것으로 간주) |
| 세션 X의 후손(에이전트, hook, 에이전트가 실행한 `mk`) | 세션 X 읽기·쓰기 + 상태 보고만. 다른 세션 제어는 설정에서 허용했을 때만. orchestration은 T4와 협의 |

- **프레이밍.**
  - 구조: `[u32 LE len][u8 kind][body]`, 최대 프레임 1MiB.
  - 제어 메시지는 postcard로 인코딩한다. enum variant는 끝에만 추가하고 삭제하지 않는다. 와이어 enum은 `#[non_exhaustive]` 규칙으로 관리한다.
  - 데이터 프레임은 `[u32 session][u64 seq][bytes]`이고 직렬화하지 않는다.
  - 큰 키프레임은 64KiB 청크로 나눠 입력 응답 프레임 사이에 끼워 보낸다. 연결은 하나만 쓴다. Orca는 control과 stream 소켓 두 개를 쓴다(`src/main/daemon/daemon-hello-protocol.ts:1-9`).
- **버전 협상.**
  - 데몬이 먼저 `ServerHello{proto_min, proto_max, features: u64, build, instance_id, boot_id}`를 보낸다.
  - 클라이언트가 `ClientHello{proto_min, proto_max, features, role, name}`로 답하면, 겹치는 최대 버전과 feature 교집합을 쓴다.
  - 규칙: 데몬은 N-1 클라이언트를 받는다. 모르는 kind는 협상한 feature로만 보낸다.
- **로컬 토큰 파일은 쓰지 않는다.** 같은 UID는 토큰을 읽을 수 있어 이득이 없다. 데몬 세대 식별은 `instance_id`로 한다.
  - T1 이슈 8의 "토큰 인증"은 원격(M6)과 페어링에만 적용하도록 조정을 제안한다.

### 3.8 재접속 프로토콜 (D7)

- **seq.** 세션별 원본 바이트 누적 오프셋(u64)이다. `incarnation_id`와 짝을 이룬다.
- **Attach.** `Attach{session, resume_from: Option<(incarnation, seq)>, history_budget_bytes, role}`
  1. `resume_from`이 메모리 링 범위 안이면 그 seq부터 바이트만 보낸다(가장 쌈).
  2. 아니면 예산을 덮는 키프레임 K + `Replay[K.seq, now)`를 보낸다. 리사이즈와 clear 레코드도 같은 순서로 재생한다.
  3. 그보다 오래된 기록은 UI가 스크롤할 때 `FetchHistory{before_seq, bytes}`로 지연 로드한다.
- **Live 데이터.** `Data{session, seq, bytes}`. 클라이언트는 seq로 중복을 버린다.
- **느린 클라이언트.** 클라이언트별 송신 큐 상한은 4MiB다. 넘치면 큐를 비우고 `Resync{from_seq}`를 보낸다. PTY 읽기는 멈추지 않는다.
- **하트비트.** 유휴 시 5초마다 Ping을 보내고, 15초 무응답이면 끊는다.
  - 판정은 `mach_continuous_time`(잠자기 포함)과 `mach_absolute_time`(잠자기 제외)의 차이로 잠자기 구간을 빼고 한다.
  - Orca의 "half-open 피어(잠든 노트북)" 문제(`session-producer-pause.ts:7-10`)는 PTY와 전송이 분리되어 생기지 않는다.
- **데몬 재시작 감지.** `instance_id`가 바뀌면 모든 세션을 경로 2로 다시 붙는다. UI에는 "다시 연결됨" 배너를 1초 동안 띄운다.

### 3.9 스크롤백 저장과 디스크 (D8)

- **세그먼트.** `sessions/<id>.noindex/seg-<start_seq>.krs`
  - 헤더: magic, 포맷 버전, 세션 id, start_seq, cols, rows, 엔진 정보.
  - 키프레임: 화면 셀, 속성, 와이드 문자, 커서와 저장 커서, 모드(대체 화면, 브래킷 붙여넣기, 마우스, kitty 플래그), 문자셋, 탭 정지, 제목, cwd, OSC 8 링크.
  - 레코드: Data/Resize/Clear/Gap을 zstd-3으로 압축한다.
  - 원본 1MiB가 쌓이거나 30초 동안 쓰기가 없으면 봉인한다. 봉인한 세그먼트는 다시 쓰지 않는다.
- **메모리 링.** 세션당 현재 세그먼트(비압축 ≤ 256KiB)와 최근 봉인본(압축 ≤ 512KiB)을 둔다. 전역 상한은 16MiB다.
- **쓰기 제한.** 토큰 버킷: 지속 8MB/분, 버스트 32MB(압축 후 기준, 원본 약 120MB/분).
  - 넘치면 메모리에 보관한다. 메모리 상한까지 넘으면 가장 오래된 미기록 세그먼트를 버리고 `Gap` 레코드를 남긴다. UI에는 "출력 폭주로 기록 N바이트 생략"을 표시한다.
- **fsync는 하지 않는다.** 데몬 크래시는 페이지 캐시가 덮는다. 메타 파일만 임시 파일 + `rename`으로 원자 교체한다.
- **보존.** 세션당 32MB(압축), 전역 1GB, 닫힌 세션은 7일이다. 디렉터리 0700, 파일 0600이다.
  - `.noindex` 접미사를 붙인다(Orca 문서 `:104-108`과 같은 관행).
  - Time Machine 제외를 선택할 수 있다. 세션별 "기록 안 함"을 지원한다.
- **스스로 감시하는 지표.** krilld는 `proc_pid_rusage`의 `ri_diskio_byteswritten`을 1분마다 기록한다. 예산을 넘으면 경고 로그를 남긴다.
  - 이것이 Orca 같은 "누가 디스크를 쓰나" 미스터리(C8·C9)에 대한 상용 운영 대응이다.
  - launchd 잡은 앱과 다른 coalition에서 돈다(**추정**). 그러면 에이전트 빌드의 쓰기가 앱 이름으로 보고되지 않는다. S6에서 확인한다.

### 3.10 업그레이드와 크래시 복구 (D9)

**krilld 업그레이드 (흔함)**
1. 새 앱이 `Upgrade{path, cdhash}`를 보낸다.
2. krilld가 링과 키프레임을 플러시하고 클라이언트에 `Restarting`을 보낸 뒤 종료한다.
3. holder가 새 바이너리를 실행한다. 새 krilld는 fd를 다시 받고, 재구성한 뒤 게시한다.
4. 목표 공백은 300ms 이하다. 공백 동안 출력이 많은 에이전트는 PTY 버퍼(약 1KB, C23)에서 블록됐다가 이어서 진행한다. 손실은 없다.

**holder 업그레이드 (드묾)**
- fd의 CLOEXEC를 해제하고 상태 목록을 넘긴 뒤 `execv`한다(C23 재현).
- launchd 잡의 PID가 그대로라서 launchd 입장에서는 같은 잡이다.

**holder 드레인 모드 (P2)**
- krilld가 5초 넘게 없으면 holder가 PTY를 대신 읽어 세션당 1MiB 링에 담는다. 넘치면 gap 처리한다.
- krilld 크래시 루프 동안에도 에이전트가 멈추지 않게 하기 위한 것이다. holder 코드가 약 200줄 늘어난다(추정).

**복구 범위**

| 사건 | 에이전트 | 화면 | 스크롤백 |
|---|---|---|---|
| 앱 크래시·재시작·업데이트 | 유지 | 완전 | 완전 |
| krilld 크래시 | 유지 | 재구성 + SIGWINCH 넛지 | 마지막 플러시 이후 최대 수 초 손실 가능 |
| krilld 업그레이드 | 유지 | 완전 | 완전 |
| holder 업그레이드 | 유지(exec) | 완전 | 완전 |
| holder만 크래시 | 유지(종료코드 상실, C5) | 완전 | 완전 |
| holder와 krilld 동시 사망, 로그아웃, 재부팅 | 소실 | 디스크 기록 읽기 전용 + 에이전트 resume(T4) | 디스크분 |
| 잠자기·깨기, 화면 잠금 | 유지 | 완전 | 완전 |

**자기 보호**
- krilld가 자기 footprint를 감시한다(예: 300MB). 넘으면 캐시를 비우고, 그래도 넘으면 자기 재시작한다. holder 덕분에 세션은 안전하다.

### 3.11 전원·잠자기 (D12)

- 잠자기 동안 PTY와 프로세스는 멈췄다가 깨어나면 이어서 돈다. 데몬이 따로 할 일은 없고, 하트비트 오판만 막으면 된다(§3.8).
- **잠자기 방지.** "작업 중" 세션이 있으면 `kIOPMAssertPreventUserIdleSystemSleep`를 잡는다. 설정은 On / Agent / Off(T1 이슈 16과 일치)다.
  - Orca의 `caffeinate -s`는 AC 전원에서만 유효하다(`man caffeinate`). 덮개를 닫을 때의 동작은 미확인이다.
- 깨어나면 `SystemResumed` 이벤트를 클라이언트에 방송한다. 네트워크가 끊긴 에이전트의 처리는 T4가 맡는다.

### 3.12 다중 클라이언트 (D10)

- 역할은 `interactive`(입력·리사이즈)와 `observer`(읽기 전용: `mk tail`, 대시보드)다.
- 크기는 가장 최근에 입력하거나 포커스한 interactive 클라이언트가 정한다. 다른 클라이언트는 그 크기로 렌더링하고 여백이나 스크롤을 쓴다(T3).
- 입력은 krilld가 도착 순서대로 직렬화한다. 배타 attach는 `--exclusive` 옵션으로만 제공한다.

### 3.13 메모리 예산 (터미널 10개, 200×50)

| 항목 | 화면만 | 기본(hot 1,000행) | 근거 |
|---|---|---|---|
| krilld 기본(tokio, 소켓, 로그) | 3~5MB | 3~5MB | 추정(1차 프로브 시작 1.2MB + tokio) |
| 에뮬레이터 | 6.8MB | 60MB(120열이면 약 36MB) | C22 실측 |
| 기록 링 | 7.7MB | 7.7MB | 설계 상한 |
| 클라이언트 큐 | 0~4MiB/클라이언트 | 같음 | 상한 |
| holder | 1~2MB | 1~2MB | 추정 |
| **합계** | **약 20~25MB** | **약 75~80MB** | 추정 |
| 비교: Orca 데몬 | – | 171~177MB(PTY 20) + 고아 2개 128MB | 실측 |

- **성공 기준(C20 정정).** 녹화한 에이전트 출력 30분을 재생했을 때, 강등 정책 적용 뒤 ≤ 60MB, 모두 붙어 있는 최악의 경우 ≤ 90MB.
- **T3 경고.** UI 에뮬레이터도 10개 × 1만 행이면 537MB다(같은 셀 크기라고 가정, 추정). 앱+데몬 100~200MB 목표는 **UI가 보이는 패널에만 grid를 두고, 깊은 스크롤백은 데몬에서 지연 로드**해야 달성된다(T1 이슈 10 parking과 일치).

### 3.14 핵심 크레이트 (2026-10-14 crates.io·GitHub 확인)

| 크레이트 | 버전(날짜) | 라이선스 | 용도와 판단 |
|---|---|---|---|
| alacritty_terminal | 0.26.0 (2026-04-06) | Apache-2.0 | **권장 엔진.** 핀 고정. 직렬화기는 직접 작성. 이슈 #9072(데몬 모드 창 반복 생성 시 1.26GB, 2026-10-13 등록)는 Alacritty 앱의 문제지만, 장기 실행 시 메모리 회귀 테스트가 필요하다는 신호다 |
| vte | 0.15.0 (2025-02-02) | Apache-2.0 OR MIT | alacritty가 재수출. 질의 훅이 필요할 때 직접 사용 |
| libghostty-vt | 0.2.2 (2026-09-28) | MIT OR Apache-2.0 | 대안. Formatter(VT 직렬화)가 있음. 다만 Zig 빌드와 git fetch가 필요하고, `!Send`이며 pre-1.0이다 → 보류 |
| vt100 | 0.16.2 (2025-07-12) | MIT | 직렬화 참고용(`state_formatted`/`contents_diff`) |
| nix | 0.31.3 (2026-05-11) | MIT | **holder 권장.** `openpty`, `killpg`, `ScmRights`, `getpeereid`, `LocalPeerPid`/`LocalPeerToken` |
| rustix | 1.1.5 (2026-09-16) | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | 보조(termios, ScmRights). openpty는 없음(C1) |
| portable-pty | 0.9.0 (2025-02-11) | MIT | holder에는 부적합(받은 fd로 `MasterPty`를 만드는 API 없음, nix 0.28 고정) |
| tokio | 1.53.3 (2026-10-13) | MIT | krilld IO. SCM_RIGHTS 미지원, `peer_cred` 있음 |
| sendfd | 0.4.5 (2026-09-26) | Apache-2.0 OR BSD-3-Clause | tokio `UnixStream` 위에서 fd 전달 |
| interprocess | 2.4.5 (2026-10-10) | 0BSD OR Apache-2.0 | 불필요(2.x에 SCM_RIGHTS 없음) |
| postcard | 1.1.3 (2025-07-24) | MIT OR Apache-2.0 | 제어 메시지와 키프레임 |
| bincode | 3.0.0 (2025-12-16) | MIT | **사용 금지**(아카이브, RUSTSEC-2025-0141) |
| zstd | 0.14.2 (2026-10-10) | BSD-3-Clause | 세그먼트 압축(C 라이브러리 링크) |
| lz4_flex | 0.14.0 (2026-07-14) | MIT | 순수 Rust 대안(7×) |
| ruzstd | 0.9.1 (2026-10-11) | MIT | 디코더는 완전. 인코더는 C zstd만 못함 → 보류 |
| libproc | 0.14.11 (2025-10-01) | MIT | pidpath, 자식 pid, cwd |
| objc2-io-kit | 0.3.2 (2025-10-04) | Zlib OR Apache-2.0 OR MIT | IOPMAssertion(`pwr_mgt`) |
| security-framework | 3.7.0 (2026-02-20) | MIT OR Apache-2.0 | 피어 코드서명 검증(`SecCode`, audit token) |
| fs4 | 1.1.0 (2026-04-28) | MIT OR Apache-2.0 | flock(단일 인스턴스). fs2는 방치됨 |
| signal-hook | 0.4.5 (2026-10-04) | MIT OR Apache-2.0 | SIGCHLD·SIGTERM |
| tracing | 0.1.44 (2025-12-18) | MIT | 로그 |
| objc2-service-management | 0.3.2 (2025-10-04) | Zlib OR Apache-2.0 OR MIT | SMAppService(대안 경로) |

라이선스는 모두 상용 배포가 가능한 허용형이다. BSD-3·Apache는 고지 의무가 있으므로 `cargo about`으로 고지 파일을 만든다(T4).

---

## 4. 위험과 대응

| # | 위험 | 영향 | 가능성 | 대응 |
|---|---|---|---|---|
| R1 | 키프레임 충실도(alacritty 직렬화를 직접 작성) | 재접속 뒤 화면이 깨짐 | 높음 | S3: 녹화 트랜스크립트와 퍼저로 왕복 검사하고 CI에 상시 등록한다. Orca도 같은 문제로 v32 등 여러 번 버전을 올렸다(`daemon-protocol-version.ts:16`) |
| R2 | holder 구조에 선례가 없음 | 미지의 시그널·제어 터미널 엣지 케이스 | 중간 | holder ≤ 800줄. S1·S2 kill 테스트, 계약 테스트 |
| R3 | macOS 정책 변화(launchd 비지속 잡, TCC, BTM 알림) | 기동 실패, 권한 프롬프트 귀속 오류 | 중간 | 메이저 OS마다 S6을 다시 돌린다. spawn 대체 경로는 남기되 경고를 띄운다 |
| R4 | PTY 버퍼가 약 1KB | krilld 공백 동안 에이전트가 즉시 블록됨 | 확정 | 재기동 300ms 목표, 드레인 모드(P2) |
| R5 | 같은 UID의 프로세스가 다른 세션을 조종(프롬프트 인젝션된 에이전트) | 보안 사고 | 중간 | D11 세션 범위 제한, 감사 로그 |
| R6 | 디스크에 비밀이 남음 | 개인정보 | 중간 | 0600, 보존 기간, 기록 안 함, 삭제 명령. 암호화는 P3 |
| R7 | 에이전트 질의 세트가 alacritty 응답 범위를 넘음(XTVERSION 등) | TUI 기능 감지 오류나 시작 지연 | 중간 | S12 캡처로 목록을 확정한 뒤 별도 훅으로 응답 |
| R8 | alacritty_terminal 0.x API 파손 | 업그레이드 비용 | 낮음~중간 | 핀 고정, 엔진 독립 키프레임 |
| R9 | UI 쪽 에뮬레이터 메모리가 목표를 깸 | 마케팅 수치 붕괴 | 중간 | §3.13 T3 경고. 보이는 패널에만 grid, 깊은 스크롤백은 지연 로드 |
| R10 | holder와 krilld 동시 사망(OOM 킬, 로그아웃) | 전 세션 소실 | 낮음 | krilld 자기 footprint 상한, holder 메모리를 작게 유지, 콜드 복원과 resume(T4) |
| R11 | 2단 구조가 과설계일 가능성 | 개발 비용 | 판단 위험 | holder는 최소 기능으로 M2에 출시하고 드레인 모드는 M5로 미룬다. 부모를 나중에 바꾸는 비용이 더 크다(D1) |

---

## 5. 검증 스파이크

| ID | 목적 | 방법 | 합격 기준 |
|---|---|---|---|
| S1 | holder exec 핸드오프 | Rust holder. 세션 10개가 번호 붙은 줄을 초당 1MB씩 출력하는 중에 자기 자신으로 `execv` | seq·번호 연속(손실 0), `waitpid` 종료코드 수신, 핸드오프 ≤ 50ms, 10회 연속 무결 |
| S2 | krilld `kill -9` 내성 | 부하 중 100회 kill. holder가 재기동하고 fd를 다시 전달 | SIGHUP 0회, 재게시 p95 ≤ 300ms, Claude Code·Codex가 SIGWINCH 뒤 1초 안에 정상 화면 |
| S3 | 키프레임 충실도 | alacritty grid → 키프레임 → 새 Term 왕복. 녹화본(Claude Code, Codex, vim, htop, less, 한글·이모지)과 퍼저 1만 건 | 녹화본 셀 단위 100% 일치(문자, 속성, 와이드, 커서, 모드, 대체 화면). 퍼저 불일치 0이거나 원인 문서화 |
| S4 | 데몬 메모리 | 녹화한 에이전트 출력 30분을 세션 10개에 재생 | 강등 뒤 ≤ 60MB, 최악 ≤ 90MB(`footprint` 기준) |
| S5 | 디스크 폭주 | 10개 세션에서 `yes`와 `cat` 대형 파일을 10분 | krilld `ri_diskio_byteswritten` 지속 ≤ 8MB/분, gap 표시 정상, 입력 지연 p95 ≤ 50ms |
| S6 | launchd·TCC·coalition | 서명한 헬퍼 복사본을 잡으로 실행. 세션에서 `~/Documents`와 `~/Desktop` 읽기 → 앱 번들 교체 → 다시 읽기. ProcessType 미지정과 Interactive로 `cargo build` 시간 비교. 리소스 보고서 귀속 확인 | TCC 프롬프트가 Minikrill로 귀속, 교체 뒤 거부 0, Interactive와 일반 터미널의 빌드 시간 차 ≤ 5%, BTM 알림 유무 기록 |
| S7 | 잠자기·로그아웃·사용자 전환 | `pmset sleepnow` 10분, 덮개 닫기, 빠른 사용자 전환, 로그아웃 | 깨어난 뒤 2초 안에 자동 재접속, 오판 0. 로그아웃 때 holder와 세션의 거동 기록 |
| S8 | 재접속 지연 | 앱 `kill -9` → 재실행, 세션당 스크롤백 5,000행 | 10개 터미널 정확한 화면까지 p95 ≤ 300ms, 지연 로드 ≤ 100ms/MB |
| S9 | 다중 클라이언트 | 앱 + CLI interactive 1개 + observer 2개 | 크기 규칙대로 동작, 입력 순서 보존, observer 쓰기 거부 |
| S10 | 느린 클라이언트 | 클라이언트가 읽기를 멈춤 | 데몬 메모리가 큐 상한 안, PTY 읽기 지속, 재개하면 Resync로 정상화 |
| S11 | 드레인 모드 | krilld를 30초 SIGSTOP | 에이전트 블록 없음, 링 범위 안 손실 0 |
| S12 | 에이전트 질의 캡처 | Claude Code·Codex·OpenCode 시작과 테마 변경 때 PTY 출력에서 질의 시퀀스를 수집. Orca의 `config/scripts/capture-agent-pty-transcript.mjs` 방식 참고 | 필요한 질의 목록 확정. 앱이 닫힌 상태에서 에이전트를 시작해도 기능 감지 지연 0 |
| S13 | 세션 범위 권한 | 세션 A 안에서 `mk send B …` 시도, 외부 셸에서 같은 명령 | A 내부 발 요청은 기본 거부, 외부는 허용, 거부는 감사 로그에 기록 |

---

## 6. 미확인 항목

| 항목 | 이유 |
|---|---|
| Orca "분당 약 2GB 디스크 쓰기"의 실제 원인 | fs_usage는 root가 필요하다. 이번 측정은 main 평균 2.9MB/분, 60초 표본 26.6MB였다. coalition 귀속 가설(C9)은 검증하지 않았다 |
| Orca main의 쓰기 대상별 분해 | 위와 같은 이유. 후보 파일 목록만 있다(C8) |
| Orca가 앱이 닫힌 동안 히스토리를 기록하지 않는다는 판단 | 코드상 `HistoryManager`가 main 쪽에만 있다는 정적 근거뿐이다. 사용자가 쓰는 Orca를 끌 수 없어 실험하지 못했다 |
| launchd `gui/<uid>` 잡의 로그아웃·빠른 사용자 전환 거동 | 로그아웃 실험을 할 수 없었다(S7) |
| launchd 잡이 별도 resource coalition을 갖는지 | 실험하지 않았다(S6) |
| 비지속 launchd 잡이 BTM "백그라운드 항목" 알림을 띄우는지 | 서명된 빌드가 필요하다(S6) |
| `ProcessType` 미지정 시 스로틀이 자식 프로세스 빌드 속도에 주는 영향 | S6 |
| 덮개를 닫을 때 `PreventUserIdleSystemSleep`의 효과 | 실험 불가(노트북 아님) |
| `posix_spawn`만으로 제어 터미널을 설정할 수 있는지(TIOCSCTTY) | S1에서 확인 |
| holder 실제 footprint(1~2MB), "화면만 + 압축 링" 모드 합계(20~25MB), 4MB 재생 20~70ms | 계산값이다. S4·S8에서 측정 |
| `login -flpq` 래퍼 프로세스의 footprint | setuid라 `footprint`로 분석할 수 없다(RSS만 측정) |
| macOS 로그아웃 때 tmux 서버가 죽는지 | 공식 근거를 찾지 못했다(Linux 사례뿐) |
| Claude Code·Codex가 시작할 때 보내는 질의 전체 목록 | S12 |

---

## 7. Minikrill 이슈 후보

마일스톤은 T1-final(`docs/planning/T1-final.md:589-594`)의 정의를 따른다: M1 스파이크, M2 코어(데몬 + 단일 창 + 세션 생존·재생), M3 병렬·CLI, M4 출시 준비, M5 v1.x, M6 v2 원격. CLI 이름은 T1의 `mk`를 따른다.

| # | 제목 | 마일스톤 | 우선순위 | 완료 조건 | 선행 이슈 |
|---|---|---|---|---|---|
| 1 | 스파이크 S1: Rust holder exec 핸드오프 무손실 | M1 | P0 | S1 합격 기준을 10회 연속 통과하고 결과 문서 작성 | — |
| 2 | 스파이크 S2: krilld `kill -9` 내성(holder가 fd 재전달) | M1 | P0 | 100회 kill에서 SIGHUP 0, 재게시 p95 ≤ 300ms | 1 |
| 3 | 스파이크 S3: alacritty grid ↔ 자체 키프레임 왕복 충실도 | M1 | P0 | 녹화본 셀 단위 100% 일치, 퍼저 1만 건 결과 문서화, CI 등록 | — |
| 4 | 스파이크 S6: 서명 헬퍼 복사 + launchd 기동, TCC·ProcessType·coalition·BTM | M1 | P0 | S6 합격 기준 측정 보고서 | — |
| 5 | 스파이크 S12: 에이전트 터미널 질의 캡처와 응답 표 확정 | M1 | P0 | Claude Code·Codex·OpenCode 질의 목록과 응답 주체 표 | — |
| 6 | 스파이크 S4: 데몬 메모리 예산(녹화 30분 재생) | M1 | P0 | 강등 뒤 ≤ 60MB, 최악 ≤ 90MB | 3 |
| 7 | krill-holder 구현(spawn, waitpid, fd 보관, SCM_RIGHTS, 동결 프로토콜 v1) | M2 | P0 | S1·S2 합격, 내부 프로토콜 계약 테스트, ≤ 800줄 | 1, 2 |
| 8 | krilld 코어: 세션별 Term, PTY 리더, 세션 단위 catch_unwind | M2 | P0 | 에뮬레이터 패닉을 주입해도 다른 세션 무영향 | 7 |
| 9 | 질의 응답 권한과 재생 게이트(테마·픽셀 크기 푸시, 테마 전 침묵) | M2 | P0 | 앱이 닫힌 상태에서도 DA/DSR/OSC 10·11에 응답. 재생 중 응답 0건 | 5, 8 |
| 10 | IPC v1: UDS 0600 + getpeereid, 프레이밍, postcard, Hello 범위·feature 협상 | M2 | P0 | 다른 UID 거부, N-1 클라이언트 호환 테스트, 1MiB 초과 프레임 거부 | 8 |
| 11 | 소켓 게시와 단일 인스턴스(flock, 임시 bind 후 rename, 종료 때 unlink 안 함) | M2 | P0 | 동시 기동 100회에서 데몬 1개, 고아 소켓 0 | 10 |
| 12 | 재접속 프로토콜(seq 이어받기, 키프레임 + 재생, Resync, 지연 히스토리) | M2 | P0 | S8 p95 ≤ 300ms, `seq 1 1000000` 해시 일치(T1 이슈 6과 같은 기준) | 3, 10 |
| 13 | 세그먼트 저장소(zstd-3, append-only, 토큰 버킷, gap, 보존·GC, 0600, .noindex) | M2 | P0 | S5 지속 ≤ 8MB/분, 크래시 뒤 재구성 테스트, 권한 검사 | 8 |
| 14 | 앱·CLI의 데몬 자동 기동(launchd, ad hoc 빌드는 spawn + 경고), 로컬 대체 PTY 금지 | M2 | P0 | 첫 실행·재실행·대체 경로 모두 연결, 기동 실패를 UI와 `mk doctor`에 표시 | 4, 7 |
| 15 | 잠자기를 고려한 하트비트와 연결 상태 배너 | M2 | P0 | S7의 잠자기 항목 합격, 소켓을 강제로 끊어도 5초 안에 복구(T1 이슈 7) | 12 |
| 16 | 세션 범위 권한(LOCAL_PEERPID 후손 판정, 감사 로그) | M3 | P0 | S13 합격 | 10 |
| 17 | 다중 클라이언트 역할과 크기 규칙(interactive/observer, latest) | M3 | P1 | S9 합격, `mk attach/tail/send` 동작 | 12 |
| 18 | 프로세스 그룹 종료 단계와 탈출한 후손 스윕, 세션 상한 256 | M3 | P1 | setsid로 빠져나간 후손까지 정리, 257번째 세션은 명확한 오류 | 7 |
| 19 | 전원: IOPMAssertion(On/Agent/Off), SystemResumed 방송 | M3 | P1 | 작업 중 `pmset -g assertions`에 표시, 깨어남 이벤트 수신 | 8 |
| 20 | krilld 업그레이드 흐름(Upgrade → 플러시 → holder 재기동), 다운그레이드 금지 | M4 | P0 | 업그레이드 중 세션 유지, 공백 ≤ 300ms, 구버전 앱은 N-1로 접속 | 7, 12 |
| 21 | holder 자기 업그레이드(exec 핸드오프) | M4 | P1 | 10회 연속 무결, launchd 잡 상태 유지 | 1, 7 |
| 22 | 자기 감시: footprint 상한 시 자기 재시작, 디스크 쓰기 지표 로그 | M4 | P1 | 상한 초과를 주입해도 세션 유지, 1분 단위 쓰기 지표 기록 | 13 |
| 23 | 메모리 강등 정책(미접속 30분 → 화면만) | M4 | P1 | S4 강등 기준 합격 | 6, 8 |
| 24 | 콜드 복원 뷰(프로세스 소실 뒤 읽기 전용 기록, T4 resume 훅) | M4 | P1 | 재부팅 뒤 마지막 화면과 스크롤백 표시, resume 버튼이 T4 API 호출(T1 이슈 9·18과 연계) | 13 |
| 25 | 개인정보: 기록 안 함, Time Machine 제외, 기록 삭제 명령 | M4 | P2 | 옵션을 켜면 디스크 기록 0바이트, 삭제 뒤 파일 없음 | 13 |
| 26 | holder 드레인 모드 | M5 | P2 | S11 합격 | 7 |
| 27 | 피어 코드서명 검증(LOCAL_PEERTOKEN + SecCode) | M5 | P3 | 서명되지 않은 바이너리의 전체 권한 접속 거부(설정) | 16 |
| 28 | 세그먼트 암호화(키체인 키) | M5 | P3 | 평문 검색 0건, 성능 저하 ≤ 10% | 13 |
| 29 | 원격 krilld(같은 프레이밍을 SSH/TLS 위에, 원격 전용 토큰) | M6 | P3 | 원격 맥이나 리눅스 krilld에 attach와 재접속 동작 | 10, 12 |
