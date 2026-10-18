# OWE-833-8 — Windows krilld 세션 경로 연결

2026-10-18, 브랜치 `craftowen/win-p3-krilld8`. 이 문서는 조사 기록이며 ARCHITECTURE §4.1의 보장을 바꾸지 않는다.

## 구현과 범위

- Windows `main`은 `Holder::connect_inherited()`로 holder의 익명 파이프 링크·보관 세션·listener를 회수한다. Unix와 공통 진입 경로에서 `ballast()`(`KRILL_TEST_BALLAST_MB`)도 호출한다.
- 세션마다 `Pty.output`의 동기 `ReadFile` 스레드와 기존 엔진 스레드를 둔다. 바이트 전달 큐는 한 슬롯(청크 최대 64KiB)이다. 엔진의 sync deadline은 채널 timeout으로 처리해 유휴 pipe polling을 하지 않는다.
- 종료·인계 시 `CancelSynchronousIo`로 자기 읽기 스레드만 깨운 뒤 join한다. stop 검사와 `ReadFile` 진입 사이의 경합은 종료 중 취소를 반복해 닫는다. holder의 핸들·ConPTY는 닫지 않는다.
- 사용자 입력은 `Pty.input`에 `WriteFile`로 쓴다. Windows 블로킹 사용자 입력 쓰기 동안 엔진 잠금을 잡지 않는다. DA1을 비롯한 질의 응답은 기존 `Engine` → `write_reply` 경로를 재사용한다.
- resize는 `Hello.version >= 4`일 때 `ToHolder::Resize`에 성공 답을 받은 뒤 엔진 크기도 바꾼다. v3 이하는 `Unsupported`, holder 단절은 연결 오류다.
- proto는 std `Command`로 짧게 실행되는 `--bootstrap` holder를 띄운다(`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, stdio null). bootstrap의 holder FFI가 `CreateProcessW(bInheritHandles=false)`로 실제 holder를 생성하고 바로 끝난다. Job에 있으면 `CREATE_BREAKAWAY_FROM_JOB`를 요구하며 금지 Job에서는 실패한다. proto에 unsafe를 추가하지 않았다.
- AF_UNIX `WSAENETDOWN(10050)`을 Windows에서만 미기동으로 분류한다.

코디네이터는 Windows holder 전용 파일의 bootstrap 변경을 이 워커에 추가 배정했다. 새 holder로의 고아 `Adopt`·ConPTY/Job 재구성은 holder가 아직 지원하지 않아 이번 범위에서 제외하고 후속 작업으로 남기기로 승인했다. holder가 죽은 뒤 기존 krilld가 signal 사본으로 계속 서빙하는 경로와 holder 생존 중 krilld 재기동 복원은 이번 검증 범위다. 고아 상태의 새 세션 생성·resize는 연결 오류를 반환한다.

## 측정 표

최종 검증 진행 중. 아직 실행하지 않았거나 성공을 관측하지 않은 항목은 **미확인**이다.

| 항목 | 결과 |
|---|---|
| Mac clippy krilld·proto | exit 0, 소스 lint 오류 0 |
| Mac nextest krilld·proto | 미확인 |
| Windows GNU workspace clippy | 미확인 |
| VM MSVC release 세션 생성·echo 입력·출력·100×35 resize·종료 | inbox PASS(실제 mode con 100×35) |
| VM MSVC krilld 강제종료 뒤 같은 셸 출력 | bundled DLL PASS(daemon 3496 → 3832, shell 2040 유지) |
| VM nextest krilld·proto(spool·DPAPI 포함) | 최종 **395 passed·1 skipped·실패 0**, 143.385초, 비밀번호 로그온 |
| SSH 정상 종료 지연 재확인 | `krill ls`로 bootstrap 후 SSH 정상 종료 exit 0, 2.743초; 별도 SSH에서 holder·krilld 생존 확인 |
| Windows 메모리 게이트·ballast 주입 | 기본 exit 1: holder+krilld 62.1 MiB > 60; Inject 80 exit 1: 142.5 MiB |

## VM nextest 1차 관측

비밀번호 로그온 예약 작업에서 Windows `da1_input_and_eof_use_the_common_engine`, `idle_reader_drop_cancels_and_joins_without_closing_holder_pipe`, DPAPI 왕복·변조, DPAPI 세그먼트 키 저장·재사용 테스트가 통과했다. spool 생성·append·상한 검사와 krilld spool 회귀도 통과했으나 deadline 후 잠금 없이 append하는 테스트는 아래와 같이 실패했다.

| 실패(6개, 전부 proto) | 관측 원인 |
|---|---|
| account_login `login_uses_env_and_literal_profile_argument` | `.gemini`에 Path::join을 쓴 실제 argv는 Windows 역슬래시인데 기대값은 Unix 슬래시 |
| diag `panic_hook_writes_crash_log` | 실제 `location: crates\\krill-proto\\...`에 Unix 슬래시 문자열을 기대 |
| hook `append_spool_waits_for_lock_then_gives_up_on_deadline` | Windows mandatory whole-file lock 중 deadline 후 write_all은 os error 33으로 거절됨. Unix fail-open append와 차이 |
| schedule 3개 | cwd `/tmp`가 Windows 절대 경로가 아니어서 기존 유효성 검사에 거절됨 |

이 파일들은 원래 소유 범위 밖이므로 코디네이터에게 최소 테스트 보정 및 spool 동작 결정을 요청했다. 승인 후 테스트 픽스처를 보정하고 Windows spool 잠금을 sidecar로 옮긴 뒤 최종 395개가 모두 통과했다. Windows 전용 분기는 공통 `hook::spool_lock` 헬퍼 한 곳이며 쓰기와 수거가 모두 사용한다. Unix는 데이터 파일의 동일 open-file description을 복제해 기존 flock·바이트 동작을 유지한다.

## 최종 release e2e

소스 최종 코드 `9e156585`를 포함한 MSVC release 바이너리다. 각각 새 디렉터리·새 holder/daemon을 사용했다. 원본 입력 문자열에는 완성된 성공 마커를 넣지 않고 cmd 환경변수 A/B를 확장한 출력으로 검증했다.

| 모드 | 결과 |
|---|---|
| kernel32 inbox, 정상 | 생성·입력 ack·변수 확장 출력·`mode con` 실제 100×35·Close→목록 제거 PASS |
| bundled DLL, krilld 강제종료 | 위 정상 흐름 뒤 daemon 3496을 taskkill, 새 daemon 3832가 listener와 shell 2040을 복원, 같은 셸 변수 확장 출력·Close PASS |
| kernel32 inbox, holder 강제종료 | holder 224를 taskkill한 뒤 daemon 5884가 shell 5892의 출력 계속 수신, 새 Create는 명시 실패, 셸 exit 후 정리 PASS |

초기 resize 프로브는 제어 연결로 크기를 바꾼 뒤 입력용 interactive 연결이 예전 뷰포트 80×25를 다시 적용했다. 기존 역할 계약대로 입력 연결에도 `roles::Request::Resize`를 보낸 뒤 실제 100×35를 확인했다. krilld restart 시험은 남아 있던 Windows `Listener::adopt` Unsupported를 발견해 실제 socket 소유권 복원과 단일 인스턴스 락 재획득으로 수정했다. 이 수정은 Windows 단위 테스트에서도 통과했다.

SSH bootstrap은 정상 종료 후에도 독립 holder·krilld가 살아 있어야 한다. 최소 `krill ls` 기동 명령의 SSH 전체 소요 2.743초·exit 0을 측정했고, 다음 별도 SSH에서 두 daemon/holder 쌍(앞선 CIM 포함 대조 실행 포함)이 살아 있음을 확인한 뒤 자기 프로세스 트리만 정리했다. OWE-829의 15초 timeout을 유발한 std 상주 자식 상속 경로는 쓰지 않았다. restrictive Job 생성 거부 자체는 이번에 새 Job 프로브로 다시 측정하지 않았으므로 그 부분 실기는 **미확인**이며 코드가 breakaway 실패를 반환한다.

### Windows 테스트 제외 범위

새 ignore/skip을 추가하지 않았다. 런타임 skipped 1개는 기존 macOS 전용 `krill-proto daemon::tests::holder_plist_lints`(plutil)다. 기존 `cfg(unix)`인 krilld 통합 테스트 `attach`, `cold`, `core`, `fault_ci`, `holder`, `holder_link`, `input`, `park`, `publish`, `rejoin`, `resources`, `roles`, `search`, `store`, `upgrade` 및 기존 Unix 전용 내부 단위 테스트는 Windows에서 컴파일 대상이 아니다. 이들은 Mac nextest로 회귀를 확인한다. Windows 경로를 검증하는 listener 복원·DA1/입력/EOF·읽기 취소·spool fail-open·DPAPI는 이번 VM 최종 395개에 포함되어 통과했다. Windows GUI·IME·1,000회 생존 스트레스·새 holder Adopt는 이번 검증 범위가 아니다.

## Windows 메모리 게이트(추가 요청)

OWE-839 `memory-gate-windows.ps1`를 코디네이터 브랜치에서 가져와 비밀번호 로그온 예약 작업으로 실행했다. 10세션·세션당 20,000줄·10초 유휴·bundled ConPTY이며 앱은 실행하지 않았다. `TEMP`/`TMP`도 `C:\spike\OWE-833-8\tmp`로 제한했다. 아래는 각 1회 관측이며 기본 예산을 통과하지 못했다.

| 모드 | holder PrivateUsage | krilld PrivateUsage | 합계 | 합계 private WS | PTY 호스트 PrivateUsage(예산 제외) | 종료 |
|---|---:|---:|---:|---:|---:|---|
| 기본 | 1.0 MiB | 61.1 MiB | **62.1 MiB** | 40.0 MiB | 19.4 MiB(10개) | 1(60 MiB 초과) |
| Inject 80 | 0.9 MiB | 141.6 MiB | **142.5 MiB** | 120.3 MiB | 19.1 MiB(10개) | 1 |

표의 반올림 관측값 차이는 80.4 MiB여서 Windows main의 ballast가 실제로 반영됨을 확인했다. 기본 예산 초과는 코디네이터에게 보고했으며 이 작업에서 최적화를 완료했다고 주장하지 않는다. 이 측정은 spool sidecar 최종 변경 직전 release 세션 코드로 실행했고 메모리 관련 코드는 동일하다. 앱 예산·Windows 10/11·다른 출력 패턴·장기 안정성은 미확인이다.

### 세션 0개 기준선과 지표 해석

동일 비밀번호 로그온·같은 바이너리의 holder+krilld를 새 상태 디렉터리에 띄우고 10초 유휴 뒤 한 번 측정했다. PTY 호스트는 0개다.

| 프로세스 | PrivateUsage(B) | Private WS(B) | 스레드 |
|---|---:|---:|---:|
| holder | 798,720 | 528,384 | 3 |
| krilld | 1,449,984 | 872,448 | 12 |
| 합계 | 2,248,704 | 1,400,832 | 15 |

10세션 게이트는 holder+krilld만 합산하며 OpenConsole/conhost 10개의 19.4 MiB는 제외했다. 따라서 호스트 프로세스가 62.1 MiB 초과분에 더해진 것은 아니다.

원인 후보는 **추정**이다: 세션마다 추가되는 블로킹 읽기·엔진 스레드의 커밋된 스택, 핫 셀·원본 링·압축/기록 컨텍스트, Windows 프로세스 힙의 페이지 공유/반납 패턴이 합계에 기여할 수 있다. 예약된 스택 주소 공간 전체를 PrivateUsage로 세는 것은 아니다. 항목별 커밋 분해·누수 판정·할당기 교체는 미측정이며 이번 범위에서 최적화하지 않았다.

지표는 [OWE-828 §2](windows-spike-OWE-828.md)에 따른 PrivateUsage(사유 커밋)이고 Private WS는 상주량 보조 기록이다. macOS phys_footprint는 압축·스왑을 포함한 프로세스 몫의 더러운 메모리이며 두 OS의 회계가 같지 않다. 같은 워크로드의 Mac 대조 측정은 하지 않았으므로 값의 직접 비교·환산은 **미확인**이다.

## holder 추가 줄 수

OWE-832 보고서와 같은 물리 행 기준(공백·주석 포함, Windows 선택 행·테스트 제외)이다. lib/main/link는 이번 작업에서 바꾸지 않았다.

| 파일 | OWE-832 | 이번 | 증가 |
|---|---:|---:|---:|
| lib.rs | 20 | 20 | 0 |
| main.rs | 28 | 28 | 0 |
| link/windows.rs | 162 | 162 | 0 |
| sys/windows.rs | 901 | 961 | 60 |
| windows.rs | 505 | 508 | 3 |
| 합계 | 1,616 | **1,679** | **63** |

Windows 상한 1,200보다 **479줄 초과**한다. 규범·하네스는 바꾸지 않았다. 메인테이너 승인 대기 항목에 합산해야 한다. macOS 선택 소스는 증가하지 않았다.

## 재현과 증거 위치

소스·스파이크 클라이언트·VM 로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-833-8/`와 VM `C:\spike\OWE-833-8\`를 사용한다. Mac target은 `/Volumes/krill-build/targets/win/OWE-833-8`, Windows GNU target은 같은 경로의 `OWE-833-8-windows`다. VM은 공유 자원이므로 중지하지 않는다.

```sh
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-833-8 cargo build -p krill-holder -p krilld --bins --locked
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-833-8 cargo clippy -p krilld -p krill-proto --all-targets --locked -- -D warnings
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-833-8 cargo nextest run -p krilld -p krill-proto --locked
scripts/check-windows.sh --clippy --target-dir /Volumes/krill-build/targets/win/OWE-833-8-windows
```

Windows 하네스 캐시와 GCP 접속 자료가 다른 작업의 디스크 이동 중 소실되어 하네스는 `--bootstrap`으로 재설치하고 GCP 자료는 코디네이터가 복구했다. 비밀번호 로그온 DPAPI 검증용 비밀번호는 재작성 README 안내대로 복구하여 보호된 로컬 파일에만 보관했다. 비밀번호는 이 문서·로그·커밋에 싣지 않는다.
