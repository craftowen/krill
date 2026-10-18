# OWE-833 — krilld Windows 구현 실측 (W3-2 1차: holder 링크 없는 항목)

측정일: 2026-10-18. 공유 GCP `krill-win`, Windows Server 2022 Datacenter 10.0.20348, Rust 1.98.1 x86_64-pc-windows-msvc, 관리자 계정 `krill`의 OpenSSH(키 인증) 세션. 브랜치 `craftowen/win-p3-krilld`.

**결론: holder 링크 없이 할 수 있는 krilld Windows 항목(DPAPI 키, 기본 셸·`.cmd`, 프로세스 조회, NOFOLLOW, 디렉터리 fsync, 피어 SID 검증, cwd 보고 추적)은 구현했고, 그중 Windows 전용 테스트 9개가 VM에서 통과했다. DPAPI는 SSH 키 인증 로그온에서 `ERROR_ACCESS_DENIED`(5)로 실패하고, 비밀번호 로그온(예약 작업)에서는 통과했다. 세션 리더 스레드·`Resize`·DA1 응답 경로는 OWE-832의 holder 링크 API(`Master = krill_holder::link::Pty`)가 병합된 뒤 한다.** 이 보고서는 조사 기록이며 ARCHITECTURE §4.1을 바꾸지 않는다.

## 실측 표

| 항목 | 결과 | 관측·범위 |
|---|---|---|
| DPAPI 왕복(`CryptProtectData`/`CryptUnprotectData`, 현재 사용자, `CRYPTPROTECT_UI_FORBIDDEN`) | 비밀번호 로그온 통과 / SSH 키 로그온 실패 | 예약 작업(`Register-ScheduledTask -User krill -Password`)에서 `dpapi_round_trip_and_tamper` ok. 같은 테스트를 SSH 세션에서 돌리면 `CryptProtectData` → os error 5 |
| DPAPI 변조 거부 | 통과 | blob 마지막 바이트를 바꾸면 `CryptUnprotectData` 실패 |
| 세그먼트 키 파일(`segment.key` = DPAPI blob) | 비밀번호 로그온 통과 | 데이터가 있는데 키 없음 → 거부, 새 키 생성, 파일에 평문 32바이트 없음, 재로드 동일 키, 바이트 하나 변조 → `PermissionDenied` |
| 다른 사용자 계정으로 blob 풀기 | 미확인 | 다른 사용자 primary token 프로세스를 만들지 못함(OWE-827과 같은 제약) |
| 기본 셸 탐색 | `C:\Program Files\PowerShell\7\pwsh.exe` | 물려받은 PATH에서 pwsh → powershell → `%COMSPEC%` → `cmd.exe` 순. argv는 셸 하나(`-l` 없음) |
| 셸 환경 | 통과 | `SystemRoot`·`USERPROFILE` 유지, 물려받은 `Path`(대소문자 다름)가 `PATH`로 들어감 |
| `.cmd` 해석 | 통과 | 공백·한글 포함 임시 디렉터리의 `fake-agent.cmd`를 `fake-agent`로 지정 → 절대 경로 `…\fake-agent.cmd`로 고정, 인자 `has space` 유지. 없는 이름은 그대로 둠 |
| 자기 프로세스 메모리(`GetProcessMemoryInfo` `PrivateUsage`) | 1,069,056 B | 테스트 바이너리 1회 관측. `resources()` 쓰기 수(`WriteTransferCount`)는 16 B |
| Toolhelp 프로세스 수 | 111 | 같은 순간 1회 관측 |
| 자식 트리·종료 감시 | 통과 | `cmd.exe /C ping -n 2` 자식의 `parent_pid` = 테스트 pid, `process_name` = `cmd`, `ExitWatch` 폴링이 종료를 한 번만 보고 |
| `start_us` 부모 ≤ 자식 | 통과 | `parent_pid`가 0이 아니면 부모 생성 시각 ≤ 자식 |
| NOFOLLOW(`FILE_FLAG_OPEN_REPARSE_POINT` + 이름 대리 태그 검사) | 통과 | 파일 심볼릭 링크 열기 → os error 681(`ERROR_STOPPED_ON_SYMLINK`), 링크 자리에 `create_new` → `AlreadyExists`, 원본 내용 유지. 관리자 세션이라 `symlink_file`이 실제로 만들어짐(건너뜀 없음) |
| 정션·OneDrive 비수화 자리표시자 | 미확인 | 정션은 std `is_symlink`(이름 대리 태그)로 거부되는 경로지만 실기 미시험 |
| `SIO_AF_UNIX_GETPEERPID` | 통과(주의) | pid는 맞게 채우지만 같은 테스트의 두 번째 호출에서 `lpcbBytesReturned`가 0이었다. 바이트 수 대신 pid ≠ 0으로 판정한다 |
| 피어 SID 비교(`OpenProcessToken`·`TokenUser`·`EqualSid`) | 같은 사용자 통과 | 자기 연결 → `SAME_USER`. 다른 사용자 프로세스 거부는 미확인(OWE-827과 같은 이유) |
| OSC 7·OSC 9;9 cwd 보고 | 통과 | 청크 경계를 나눠 넣은 OSC 7(`file://host/C:/Users/a%20b`) → `C:/Users/a b`, OSC 9;9 `"C:\Users\c;d"` 반영, 상대 경로·다른 OSC는 무시 |
| 디렉터리 fsync(`File::open(dir)`) | `cfg(unix)`로 제외 | OWE-835 실측 os error 5 그대로 |

## 할당기 메모리 반납 (항목 7)

krilld Windows는 시스템 할당기(Rust std → 프로세스 힙 `HeapAlloc`)를 쓴다. 강등(MK-035)처럼 200열 행(4,800B)을 세션 10개 × 2,000행 할당한 뒤 해제했다. 저장소 밖 `~/.cache/krill-win/spikes/OWE-833/alloc/`(release 빌드), 값은 `PrivateUsage`/`WorkingSetSize` 1회 관측.

| 할당 순서 | 기준선 | 전부 핫 | 절반 세션 해제 | 전부 해제 | 5초 유휴 | `HeapCompact` 뒤 |
|---|---|---|---|---|---|---|
| 세션 교차(행마다 세션 번갈아) | 0.6 MiB | 97.9 | **97.9** | 1.2 | 1.2 | 1.2 |
| 세션 교차 + 64행마다 64B 생존 객체 | 0.6 | 97.9 | 97.9 | 1.9 | 1.9 | 1.9 |
| 세션별 연속 | 0.6 | 98.4 | 52.2 | 1.8 | 1.8 | 1.7 |
| 세션별 연속 + 생존 객체 | 0.6 | 98.4 | 52.0 | 1.8 | 1.8 | 1.7 |
| 세션 교차, 1,000행 | 0.6 | 49.4 | 49.4 | 0.9 | 0.9 | 0.8 |

- 해제한 페이지는 기다림·호출 없이 바로 줄었다. jemalloc의 decay 0 설정(`alloc.rs`)에 해당하는 조정은 Windows에 필요 없다.
- 세션들이 번갈아 출력해 행이 한 페이지에 섞이면 일부 세션만 강등해도 반납되지 않는다(페이지 공유). 이것은 할당기 설정이 아니라 배치 문제다. 실제 출력 패턴(세션이 동시에 많이 출력하는 비율)에서의 반납은 미측정이다.
- 실제 krilld 세션의 강등·반납과 메모리 예산(데몬 ≤ 60MB)은 세션 경로가 생긴 뒤 다시 잰다(미확인).

## Windows nextest (`cargo nextest run -p krilld --locked`, SSH 세션)

119개 중 114개 통과, 5개 실패(마지막 전체 실행은 셸 테스트 추가 전 118개 중 113개 통과·5개 실패).

| 실패 | 원인 | 처리 |
|---|---|---|
| `status::tests::{take_spool_sorts_and_truncates, failed_save_preserves_spool_for_retry, restart_before_spool_truncation_replays_once}` | `krill_proto::hook::append_spool`이 os error 5. 같은 VM에서 krill-proto `hook::tests::append_spool_*` 3개도 실패. append 전용 핸들(GENERIC_READ·WRITE 없음)에 `try_lock`(LockFileEx)을 거는 것으로 추정 | krill-proto 소유 밖이라 고치지 않음. `.read(true)` 추가안을 코디네이터에 보고(미검증). `krill hook`의 Windows 스풀 쓰기도 같은 함수다 |
| `sys::windows::tests::dpapi_round_trip_and_tamper`, `store::crypto::tests::key_file_is_dpapi_…` | SSH 키 인증 로그온에서 DPAPI가 os error 5 | 비밀번호 로그온 예약 작업에서는 둘 다 통과(위 표). 테스트는 엄격하게 둔다 |

Windows에서 의미 없는 Unix 가정이라 이번에 `cfg(unix)`로 뺀 것: `mobile::pwa_rpc::tests::subprocess_output_and_time_are_bounded`(`/bin/sh`), `tests/mobile_pwa.rs`의 `non_secure_pwa_crypto_and_workflows`(node, Windows 모바일 게이트웨이 없음 — `sys::ipv4_interfaces` 미구현), `usage::direct::tests::antigravity_native_…`의 go-keyring base64 구간(`/usr/bin/base64`). 고친 것: `agent_history` 다중 계정 테스트의 cwd를 이 OS의 절대 경로로, 사이드바 cwd 테스트는 양쪽을 canonicalize(`\\?\` 접두). 이미 `cfg(unix)`인 기존 테스트 모듈(store·session·shell 등)은 그대로라 Windows 테스트 수가 macOS(492개)보다 훨씬 적다.

## Phase 3 설계에 주는 함의

1. **DPAPI는 로그온 종류에 기댄다.** 탐색기에서 뜬 앱 → krilld는 대화형 로그온이라 동작할 것으로 보이나(추정, 데스크톱 세션 실측은 미확인) SSH 키 인증 세션에서 띄운 krilld는 세그먼트 키를 얻지 못해 `Store::open`이 실패하고 기록이 꺼진다(데몬은 계속 돈다, `registry.rs`의 `Store::open` 실패 처리). 원격 Windows 호스트 지원을 하면 대체 키 저장(예: `CRYPTPROTECT_LOCAL_MACHINE`, 보호 수준이 낮음)을 따로 정해야 한다.
2. **피어 PID 판정은 pid 값으로.** `SIO_AF_UNIX_GETPEERPID`의 반환 바이트 수를 믿지 않는다.
3. **범위 계산(`scope`)은 고아를 0으로.** Windows는 부모가 끝나도 ppid가 남고 재사용된다. `parent_pid`가 부모 부재·생성 시각 역전을 0으로 돌려줘 macOS의 launchd 입양과 같은 범위(Full)가 된다. 이것 없이는 탐색기에서 뜬 외부 클라이언트가 늘 Restricted가 된다(코드 추론, e2e 미시험).
4. **cwd는 셸 보고에 의존한다.** 기본 pwsh·cmd는 OSC 9;9를 내지 않는다. 세션 경로를 붙일 때 기본 셸 프롬프트에 OSC 9;9를 넣을지(예: pwsh `-NoExit -Command`로 prompt 감싸기, cmd `PROMPT`에 `$e]9;9;$P$e\`) 정해야 한다(미구현·미시험). 그 전에는 세션 cwd가 시작 디렉터리에 머문다.
5. **`.cmd` 해석은 `krill_proto::agent::which`의 PATHEXT 전체를 쓴다.** OWE-830 권고(허용 확장자 제한)와 다르다. proto 소유라 이번에 바꾸지 않았다.
6. **메모리 지표는 `PrivateUsage`(커밋).** macOS `phys_footprint`와 같은 값이 아니다. 예산 판정 기준을 정할 때 작업 관리자 기본 열(사적 작업 집합)과의 차이를 따로 잰다(미측정).

## 남은 일 (OWE-832 병합 뒤)

- 세션마다 `Pty.output` 블로킹 `ReadFile` 리더 스레드, 입력은 `Pty.input` `WriteFile`, `halt` 깨우기 방식.
- `Hello.version ≥ 4`일 때만 `ToHolder::Resize`(Unix는 지금처럼 `TIOCSWINSZ`).
- OpenConsole 시작 DA1 응답: 엔진이 이미 `PtyWrite`로 답을 만들므로 리더가 그 답을 `Pty.input`에 쓰면 된다(실기 확인 필요).
- `main`의 `Holder::connect_inherited()` 진입.

## 재현

```sh
# Mac
~/.cache/krill-win/gcp/up.sh
git bundle create /tmp/owe833/src.bundle HEAD   # 이 브랜치
# tar로 C:\spike\OWE-833\ 에 보낸 뒤 VM에서 clone/fetch (README의 ssh 옵션)
~/.cache/krill-win/gcp/ssh.sh 'cd C:\spike\OWE-833\src; $env:CARGO_TARGET_DIR="C:\spike\OWE-833\target"; cargo nextest run -p krilld --locked --no-fail-fast'
# DPAPI(비밀번호 로그온): 테스트 바이너리를 Register-ScheduledTask -User krill -Password 로 실행 → C:\spike\OWE-833\dpapi-task.log
# 할당기: C:\spike\OWE-833\alloc 에서 cargo build --release; .\target\release\alloc-spike.exe 10 2000 0 [blocked]
```
