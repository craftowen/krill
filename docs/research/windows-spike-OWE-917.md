# OWE-917 — Windows 앱 사용성 마감

측정일: 2026-10-19(KST). 브랜치 `craftowen/craftowen-owe-917-polish`. Mac은 Rust 1.98.1, Windows GNU 교차 검사는 저장소의 격리 하네스, 네이티브 실행은 공유 GCP Windows Server 2022 MSVC 환경이다. VM 작업은 `C:\spike\OWE-917\`에서만 수행했고 공유 VM은 중지하지 않았다.

## 구현

- `krill-app` release Windows 바이너리에 `windows_subsystem = "windows"`를 적용했다. 디버그 콘솔은 유지한다. 기존 `krill-log` 파일 로깅 초기화는 그대로 사용한다.
- 앱이 소유한 백그라운드 명령 생성은 `platform::background_command`로 모아 Windows에서 `CREATE_NO_WINDOW`를 설정한다. stdout/stderr 파이프와 호출자의 인자·환경·타임아웃 처리는 유지한다. holder/relauncher의 명시적 detach 플래그는 기존 정책을 유지한다.
- ko/en 카탈로그의 단축키 안내와 설정 키캡을 플랫폼별로 구분한다. macOS 문자열은 그대로이며 Windows/Linux 문구는 기존 `keys::windows_keys` 규칙과 맞춘다. 터미널 제어 문자 보존 때문에 `⌘T/F/P`는 `Ctrl+Shift+T/F/P`, `⇧⌘W`는 `Ctrl+Alt+Shift+W`, `⌘⌥T`는 `Ctrl+Alt+T`다. 링크는 `Ctrl+클릭`, 전체 화면은 F11이다. 실제 터미널 링크 클릭·경로 hover도 플랫폼 수식키를 사용한다.
- Windows `ITaskbarList3::SetOverlayIcon`으로 미확인 알림이 있을 때 시스템 정보 아이콘 오버레이를 표시하고 0건이면 제거한다. 숫자는 접근성 설명으로 전달하며 앱 안의 기존 배지에 표시한다. 숫자 그림을 별도 렌더하지 않는다. windows-sys에 없는 COM 인터페이스의 호출 부분만 SDK `shobjidl.h` ABI로 선언했고 참조를 호출 안에서 해제한다.
- 숨겨진 Win32 창과 `Shell_NotifyIconW`로 트레이 아이콘·세션 상태 tooltip·열기/종료 메뉴를 구현한다. `TaskbarCreated`에서 Explorer 재시작 후 재등록한다. 트레이 생성 성공 시에만 마지막 창을 닫아도 앱을 유지하고, 실패 시 마지막 창 닫기로 종료한다. 창이 없을 때 트레이 종료는 직접 종료하고, 창이 있으면 기존 미저장 변경 확인 경로를 사용한다. 현재 트레이 아이콘은 공유 시스템 앱 아이콘이다.
- `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)`를 현재 앱 테마에 맞춰 창 생성·테마 변경 시 적용한다. 새 창과 알림 상태는 기존 모니터 폴링에서도 동기화하며, 성공한 상태는 캐시한다. 모든 Win32/COM unsafe는 `platform/windows` 안에 있고 콜백은 unwind를 차단한다.
- `Paths::from_exe`가 canonicalize로 생긴 DOS `\\?\C:\…`·UNC `\\?\UNC\server\share\…`를 일반 DOS/UNC 경로로 바꾼다. 장치 네임스페이스는 보존하고 경로를 손실 있는 UTF-8 변환에 통과시키지 않는다. `find_exe`는 평면 설치의 `krill-holder.exe`를 우선하고, 없을 때만 정확한 패키지 버전의 `krill-holder-<version>.exe`를 찾는다. 다른 버전 holder를 임의 선택하지 않는다. 현재 zip 배포 형식은 평면 설치를 유지한다.

## 추가 승인 범위: native hook·Windows 난수

코디네이터 승인으로 `daemon::preflight`와 `agent::version`의 Windows 자식에도 `CREATE_NO_WINDOW`를 추가했다. `krill-hosts` SSH 실행은 OWE-915 담당이다. 앱은 `tracing`의 기존 회전 파일 로그를 사용하며 숨겨진 콘솔에 로그 보존을 의존하지 않는다.

Windows 전역 Claude hook 설치와 krilld 세션별 `--settings`는 `command=<krill.exe 절대 경로>, args=["hook", "claude"]`를 생성한다. 경로를 셸 문자열로 해석하지 않으며, CLI는 `MK_PANE_ID`가 없으면 `{}`와 성공 코드를 내고 즉시 끝낸다. 이 형식은 [Claude Code 공식 hook 문서의 exec form](https://code.claude.com/docs/en/hooks#command-hook-fields)을 따른다. 실제 Claude Code 버전별 설정 수용 여부와 모델 호출은 별도 확인 범위이며 유료 모델 요청은 하지 않는다.

`hook_windows.rs`는 먼저 에이전트별 fail-open JSON을 flush하고 제한된 stdin 리더, `FEAT_HOOK` 협상·이벤트 전송·ACK, 실패 시 기존 spool 포맷을 사용한다. 데몬을 새로 띄우지 않는다. Windows stdin은 std 스레드·채널 `recv_timeout`으로 첫 바이트/빈 EOF를 최대 500ms, 첫 바이트를 받은 뒤 EOF를 최대 100ms 기다린다. Claude가 payload를 쓰고 stdin을 닫는 계약을 따르며, 닫힌 파이프는 EOF까지 즉시 수거한다. payload는 256KiB까지 보존하고 초과분은 `Truncated`, EOF 상한 초과는 빈 `TimedOut`으로 구분한다. macOS stdin 10ms 예산과 구현은 변경하지 않았다. Windows 전달 목표는 별도 35ms다. 이는 코드상 예산이며 관측 지연 통과와 구분한다. 부모 PID는 기존 lock의 `sysinfo 0.31.4`에서 `ProcessesToUpdate::Some([자기 PID])`와 빈 `ProcessRefreshKind`로 자기 프로세스만 갱신한다. CLI의 `forbid(unsafe_code)`는 유지한다. workspace 버전 정의를 추가하고 lock에는 CLI 의존성 간선만 추가했으며 로컬 `cargo metadata --offline`으로 새 다운로드 없이 해석했다.

승인된 Windows 난수 결함도 수정했다. 세션 UUID와 artifacts 영구 소유자 토큰은 Windows에서 기존 `ring::rand::SystemRandom`을 쓰고 Unix `/dev/urandom` 동작은 유지한다. native exec hook 설정 생성·UUID, token 생성·재사용, 공백·한글·셸 특수문자가 있는 경로, hook ACK 및 spool payload/부모 PID를 회귀 테스트로 검사한다.

## 실측 및 검증

| 항목 | 결과 |
|---|---|
| VM GUI 세션 | `quser`: `No User exists for *`; GUI 화면 미확인 |
| 공통 실행 helper의 자식 콘솔 | 네이티브 GUI-subsystem Rust 프로브 → PowerShell `GetConsoleWindow()` = **0** |
| 같은 자식 stdout/stderr | exit 0, stdout `console=0`, stderr `stderr-ok` 회수 성공 |
| macOS 담당 크레이트 clippy/nextest | 이 워크트리에서는 미확인 — 공유 USB SSD(`/Volumes/krill-build`) I/O 포화(50MB 쓰기 87초)로 중단, 통합 브랜치 `scripts/check.sh`로 확인(코디네이터) |
| Windows clippy | **VM 네이티브 msvc** `cargo clippy --locked --all-targets -p krill-app -p krill-proto -p krill -p krilld -- -D warnings` exit **0**(커밋 트리 기준). Mac GNU 크로스는 공유 SSD 정지로 대체. 첫 실행에서 `examples/menu_bar_smoke.rs`의 불필요해진 `expect(clippy::drop_non_drop)`(Windows `StatusItem`에 이제 Drop이 있음) 1건을 찾아 Linux 전용으로 좁혔다 |
| Windows 네이티브 `cargo test -p krill --test windows` | 9/9 통과. 이전 워커 실행에서 `native_hook_sends_the_event_and_waits_for_ack_without_spooling`이 간헐 `WouldBlock`(10035)으로 1회 실패 — 테스트 서버가 accept한 소켓이 리스너의 non-blocking을 물려받은 테스트 결함이라 accept 뒤 blocking으로 되돌렸다(krilld IPC 리스너는 non-blocking을 쓰지 않아 제품 영향 없음) |
| 실제 앱 MSVC release 빌드·PE subsystem·파일 로그 | 빌드 exit 0; PE subsystem **2 (GUI)**; `--crash-test` exit **101**, stdout 0바이트, stderr 180바이트, `krill-app.log` 80바이트에 `app starting` 확인 |
| 경로·exec-form 네이티브 테스트(krill-proto, release, 이전 워커 실행) | 5/5 통과(`windows_executable_prefixes_preserve_dos_unc_and_device_paths`, `windows_paths_choose_flat_then_exact_version_holder`, `windows_claude_exec_form_keeps_executable_metacharacters_literal`, holder 핸들 2건) |
| Windows native release hook, Git Bash 없는 환경 | 30/30 Complete·원문 payload·부모 PID 일치, 실행 오류 0; 전체 지연 35ms 목표 미통과(아래 표) |
| hook 지연(프로세스 시작~종료, 30회) | 중앙값 55.02ms·p95 77.26ms·최대 404.88ms(첫 회) — **35ms 목표 미통과** |
| 부모 PID 조회(sysinfo 자기 PID만 갱신) | 0.445~1.658ms |
| 빈 hook 기동 기준선(VM 부하 시점) | 약 0.9~1.4s — 위 hook 지연과 다른 측정 시점이라 직접 비교하지 않는다 |

프로브는 제품의 `background_command` 함수 본문을 그대로 포함한 `~/.cache/krill-win/spikes/OWE-917/console.rs`를 `rustc -O`로 빌드했다. PowerShell 자식에서 Kernel32 `GetConsoleWindow`를 호출하고 stdout과 stderr를 각각 출력한다. 결과는 VM `console-result.txt`에 있다. 이 실측은 콘솔 핸들과 파이프 계약을 확인하며 데스크톱에서 깜빡임을 눈으로 확인한 결과는 아니다.

재현 명령(모든 cargo 명령 `CARGO_BUILD_JOBS=3`):

```sh
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-917 cargo clippy -p krill-app -p krill-proto -p krill -p krilld --all-targets --locked -- -D warnings
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-917 cargo nextest run -p krill-app -p krill-proto -p krill -p krilld --locked
scripts/check-windows.sh -p krill-app -p krill-proto -p krill -p krilld --clippy --target-dir /Volumes/krill-build/targets/win/OWE-917-windows  # 이번엔 SSD 정지로 VM 네이티브로 대체
# VM(C:\spike\OWE-917\src, git archive HEAD): cargo clippy --locked --all-targets -p krill-app -p krill-proto -p krill -p krilld -- -D warnings; cargo test --locked -p krill --test windows
cargo fmt --all -- --check
```

VM release 빌드는 `CARGO_TARGET_DIR=C:\spike\OWE-917\target`에서 `cargo build --release --locked -p krill-app -p krill`로 실행한다. OWE-828의 빌드 캐시를 이 작업 디렉터리로 복사해 재사용했고 다른 작업 디렉터리는 수정하지 않았다.

## stdin 결함 및 재측정

이전 10ms Windows reader는 native release hook 30회에서 부모 PID는 30/30 확인했으나 payload `Complete`는 1/30만 기록했다. 입력 리더 스레드가 스케줄되기 전에 10ms 수신 대기가 끝나는 경로를 분리했다. 첫 바이트 500ms·이후 EOF 100ms는 코드상 대기 상한이며 35ms 전체 지연 보장이 아니다. 지연 입력 회귀 테스트는 hook의 초기 응답을 읽은 뒤 25ms 기다려 첫 조각을 쓰고, 다시 25ms 뒤 나머지와 EOF를 전달해 원본 payload 보존을 확인한다.

수정 release 재측정은 **30회 모두 `Complete`·원문 payload·정확한 부모 PID 일치(30/30)**, 종료 코드 0·stdout `{}`·stderr 빈 값으로 실행 오류 **0건**이었다. 모든 자식의 `PATH`를 비우고 `CLAUDE_CODE_GIT_BASH_PATH`를 제거했으며 데몬을 띄우지 않아 30개 이벤트를 spool에서 디코딩했다. 호출 PowerShell PID 1008과 저장된 `ppid`를 직접 비교했다.

| 수정 release, 프로세스 시작부터 종료까지 | 실측(ms), 30회 |
|---|---:|
| 최소 | 48.1251 |
| 중앙값 | 55.0182 |
| p95 (정렬 후 29번째 표본) | 77.2560 |
| 최대 | 404.8835 |
| 35ms 초과 | 30/30 |

**Windows 35ms 전체 지연 목표는 미통과다.** 위 결과는 입력 정확성 통과이며 지연 목표 통과가 아니다. 재현은 VM `hook-final.ps1`(프로세스 실행·30회 원시 JSON)과 `spool-inspect-final.rs`(제품 `krill_proto::hook::read_spool`로 디코딩·정확성 검증)로 한다. 검사기는 기존 release 라이브러리에 `rustc --edition=2024 -O -C lto=thin --extern krill_proto=<rlib> -L dependency=<release/deps>`로 링크했다. `hook-final-result.txt`와 `hook-latency.json`을 VM 및 로컬 spike 디렉터리에 보존했다.

 빈 hook 기동 기준선은 이전 측정에서 약 0.9~1.4초(VM 부하 포함)였으며 이 값을 stdin 처리 시간으로 해석하지 않는다. 부모 PID 조회 프로브의 자기 PID 갱신은 이전 실측 0.445~1.658ms였고 전체 프로세스 목록 갱신으로 바꾸지 않았다. 원시 자료는 로컬 `~/.cache/krill-win/spikes/OWE-917/`, VM `C:\spike\OWE-917\` 및 `/Volumes/krill-build/targets/win/OWE-917-*.log`에 있다.

## 미확인·인계

- OWE-916: 로그인된 Windows 10/11 데스크톱에서 release 앱·자식 프로세스의 콘솔 깜빡임, 실제 트레이 아이콘/메뉴·Explorer 재시작·마지막 창 닫기/다시 열기·미저장 변경 종료, 알림 오버레이와 light/dark/system 제목 표시줄을 확인해야 한다. 이 보고서는 그 GUI 동작을 실측 통과라고 주장하지 않는다.
- Windows의 hook 관련 변경은 위 추가 승인 범위로 구현했다. native 검증 결과는 검증 표에 기록한다. 실제 Claude Code 세션/모델 호출을 자동 검증한 것으로 확대 해석하지 않는다.
- `krill-hosts` SSH 자식 콘솔 플래그는 OWE-915 담당에게 인계했다. 이 작업 보고서만으로 담당 밖 모든 실행 경로의 콘솔 동작을 보장하지 않는다.
- macOS clippy·nextest는 통합 브랜치 `scripts/check.sh`로 확인(코디네이터). 이 워크트리에서는 공유 빌드 디스크 포화로 돌리지 못했다.
- Windows 네이티브 테스트는 `krill --test windows`(9건)와 krill-proto Windows 경로 테스트(5건)만 돌렸다. krilld `windows_tests`(exec-form 세션 설정·UUID)와 artifacts 토큰 테스트는 VM에서 실행하지 않았다(미확인, clippy 컴파일만 확인).
- Windows 35ms hook 지연 목표는 미통과(중앙값 55.02ms·p95 77.26ms). 프로세스 기동 비용 축소가 후속 과제다.
