# OWE-913 — Windows holder 크래시 후 Adopt 재결합

2026-10-19. GCP Windows Server 2022 공유 VM, Rust 1.98.1 MSVC release. 상태·빌드·실험 파일은 VM `C:\spike\OWE-913\`, Mac 증거는 저장소 밖 `~/.cache/krill-win/spikes/OWE-913/`에 둔다. 같은 작업트리에서 빌드한 holder·krilld·e2e 실행 파일을 사용한다. 이 문서는 조사 기록이다.

## 구현

- holder가 보내는 v4 `Session`·`Spawned` 핸들 배열의 앞 5개(`in/out/signal/reference/conhost`)를 유지하고 뒤에 세션 프로세스·Job·원래 IOCP를 붙인다. krilld는 8개를 보존한다. 새로운 Job을 만들거나 IOCP를 다시 연결하지 않는다.
- 고아 krilld는 기존 Unix 재결합 순서대로 리더를 취소·배출하고 기록을 봉인한 뒤 새 krilld에 세션을 넘긴다. Windows 인계 소켓은 `AF_UNIX`이며 양쪽에서 피어 PID와 사용자 SID를 확인하고 그 프로세스로 `DuplicateHandle`한다.
- 새 krilld가 새 holder에 8개 핸들을 `Adopt`로 되넘기고 모든 응답을 확인한 뒤에만 이전 krilld에 `Ready`를 보낸다. Adopt 실패를 성공으로 무시하지 않는다. 이전 krilld는 인계 완료 전까지 원본 핸들을 보유한다.
- 번들 모드는 기존 `ConptyPackPseudoConsole`을 사용한다. inbox는 HPCON 구조체를 조작하지 않고 보존한 signal 파이프로 기존 크기 변경 패킷을 전송한다(OWE-826 §5에서 확인한 프로토콜). 종료는 기존 conhost 프로세스·Job 핸들로 수행한다.
- `daemon.rs`의 `ready_handshake`·`watch_for`·`wait_closed`에서 Windows Orphan 예외를 제거해 표준 앱·CLI 연결이 새 holder 기동과 재결합을 수행한다.
- 기존 holder 업그레이드 상속 핸들의 순서는 유지한다. Adopt 수신과 업그레이드 복원은 같은 세션 복원 함수를 사용한다.

## 검증 방법

`crates/krill-app/tests/win_survival_e2e.rs`의 `holder_taskkill_adopts_without_loss`는 출력 중 holder만 강제 종료하고 고아 출력이 계속되는 것을 확인한다. 표준 앱 연결로 새 holder를 띄워 Adopt한 뒤 기존 세션 PID 생존, 이전 krilld 은퇴, 새 세션 입력·출력, 기존 세션 원본 바이트 일치(SHA-256도 기록), Gap·누락 줄·검사 꼬리표 오류, 크기 변경 요청 성공, 원래 Job으로 세션 종료, 정리 후 holder·krilld 잔여 0을 검사한다. `KRILL_E2E_HOLDER_KILLS`로 반복한다.

세그먼트 기록을 포함하려면 DPAPI가 되는 비밀번호 로그온 예약 작업에서 실행해야 한다. SSH 키 로그온에서의 실행은 무손실 증거로 쓰지 않는다. 테스트는 GUI 앱 대신 기존 OWE-836 헤드리스 앱 경로를 사용한다.

```sh
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-913 cargo clippy -p krill-holder -p krill-proto -p krilld --all-targets --locked -- -D warnings
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-913 cargo build -p krill --bin krill --locked
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-913 cargo nextest run -p krill-holder -p krill-proto -p krilld --locked
CARGO_BUILD_JOBS=3 scripts/check-windows.sh -p krill-holder -p krill-proto -p krilld --clippy --target-dir /Volumes/krill-build/targets/win/OWE-913-windows
```

VM 빌드와 테스트의 재현 스크립트는 저장소 밖 `build.ps1`·`run.ps1`, 증거 로그는 같은 디렉터리의 `build.log`·`test-inbox.log`·`test-dll.log`다. 비밀번호는 보고서·소스·로그에 기록하지 않는다. VM은 다른 워커와 공유하므로 이 작업에서 정지하지 않는다.

최종 VM 소스의 변경된 Rust 파일 13개는 Mac 작업트리와 SHA-256을 대조해 모두 일치했다. 최종 release 빌드의 실행 파일 SHA-256은 다음과 같다.

| 실행 파일 | SHA-256 |
|---|---|
| `krill-holder.exe` | `8006adf10e43b13973731bdfb04d489d84376bb0aed00973fca2152d9d428cb0` |
| `krilld.exe` | `678a55915996fd9ed80ca0ee801048497df3e7b8d12bdf652919dc8694fc06e1` |
| `win_survival_e2e-657b17d2b0ce3369.exe` | `ffca4faf4159e01d68d6fa044fb60a5a01780eae7ca05bd62d4237554866fa52` |

## 결과

최종 release 바이너리로 **inbox·번들 각 5회, 총 10회** holder 강제 종료 → 표준 앱식 재접속 → Adopt를 실행했다. 두 모드 모두 테스트 종료 0이며, 전체 실행 시간은 inbox 268.24초·번들 40.22초다. 이전 바이너리의 사전 각 5회 로그는 `test-*-preliminary.log`로 분리해 아래 집계에서 제외했다.

| 모드·회차(0부터) | 생성 줄 | 재생 바이트 | 앱식 재접속 절차(ms) | 원본 SHA-256(실시간·재생 일치) |
|---|---:|---:|---:|---|
| inbox 0 | 4,850 | 93,803 | 6,298 | `89fd1558eae0a37edf9ef06811cd3fc72667a1a2efb6c38833f2e43ce02fbd4a` |
| inbox 1 | 5,900 | 113,799 | 6,866 | `af385f4bb01f9a96b1c32ce465c540a3e61e3a2cbddbb4fa044007f5bb052afa` |
| inbox 2 | 7,500 | 144,126 | 919 | `24c8869cb071a5c3292c581e42de3439529ba693a3844b866caa1e7379b387fb` |
| inbox 3 | 8,850 | 169,821 | 807 | `cdd7289e61be7984de76fc095563b4f1fd2dab0950ce22108efac583d8c1c245` |
| inbox 4 | 9,450 | 181,093 | 1,013 | `b4b9857f1cbe4a91cbc3239a9620ef098944de602aa41bc5a93348c35394152c` |
| 번들 0 | 12,200 | 233,100 | 851 | `f75d887bc51bd6842d0f1ffaadcd8c266e6a2087fe9a3fa44d958a940079453b` |
| 번들 1 | 12,250 | 234,050 | 928 | `e837c7fec26b7780250d79bfd1ee6373046e015b1ed76aa3547bf41cc9ceb15b` |
| 번들 2 | 14,600 | 278,700 | 1,117 | `93a1b51c8c3ef9799f34c27abe2ee835afa030fd874edd2293c71c84c6a78516` |
| 번들 3 | 12,100 | 231,200 | 777 | `0495aee4b3ba984b94d4a4b30d3c9644e694cb7c1b6b061cc5de05f456d0c22e` |
| 번들 4 | 9,500 | 181,806 | 911 | `b0457a6a53870fee55574b415861bbd62fd38ca8ca6b4b2083907c024e1f1dfc` |

10회 모두 Gap·누락 줄·꼬리표 오류 **0**, 기존 세션 PID 생존, 이전 krilld 은퇴, 새 세션 생성·입출력, 크기 변경 요청 응답, 기존·새 세션 종료를 통과했다. 매회 정리 후 holder·krilld 잔여 **0**이다. 재결합 전 생성 요청은 매회 `NoHolder`였고, 재결합 후 새 세션 ID는 매회 2였다.

재접속 절차 시간은 테스트의 `launch_full` 전체(헤드리스 클라이언트 기동·연결·부모 PID 조회)를 포함한다. 공유 VM에서 다른 스파이크 빌드가 동시에 진행됐고, 실행 도중 OWE-913 프로세스 우선순위를 예약 작업 기본 `BelowNormal`에서 `Normal`로 조정했다. 따라서 이 시간은 성능 기준이나 단독 Adopt 지연 측정으로 해석하지 않는다.

| 검사 | 결과 |
|---|---|
| macOS 담당 3크레이트 clippy `--all-targets --locked -- -D warnings` | 종료 0, 오류 0 |
| macOS 담당 3크레이트 nextest `--locked` | 831 passed, 11 skipped, 실패 0 |
| Windows GNU 담당 3크레이트 cross clippy | 종료 0, 오류 0 |
| Windows GNU krill-app `--all-targets` cross clippy(e2e 포함) | 종료 0, 오류 0 |
| Windows MSVC 최종 release holder·krilld 빌드 및 e2e 링크 | 종료 0, 오류 0 |
| Windows MSVC 최종 release holder Adopt e2e | inbox 5/5·번들 5/5, 실패 0, 각 모드 종료 0 |
| `cargo fmt --all --check`, `git diff --check` | 종료 0 |

Windows cross clippy의 경고는 기존 `clippy.toml`의 Unix 전용 `RawFd` 항목 1종이며 이번 변경의 코드 경고가 아니다. macOS nextest 첫 시도는 새 target에 `krill` CLI가 없어 `failed_spawn_from_session_leaves_no_creator_residue`가 실패했다. CLI를 빌드한 뒤 전체 담당 크레이트를 다시 실행해 위 결과를 얻었다. 통합 `scripts/check.sh`는 공통 작업 지시에 따라 코디네이터가 실행한다.

최종 검증 로그 사본은 Mac `~/.cache/krill-win/spikes/OWE-913/vm-logs/`에 보존한다. 재현은 비밀번호 로그온 환경에서 `powershell -NoProfile -ExecutionPolicy Bypass -File C:\spike\OWE-913\run.ps1`이며, 이번에는 기존 비밀번호 로그온 예약 작업 `OWE913`의 `smoke.ps1`을 같은 내용으로 교체해 실행했다. SSH 실행만으로 DPAPI 검증을 대신하지 않았다.

## holder 줄 수

기존 OWE-833-8 보고서와 같은 Windows 선택 물리 행 기준(공백·주석 포함, 테스트 제외)이다. `lib.rs`와 `main.rs`는 바꾸지 않았다.

| 파일 | Windows 비테스트 행 |
|---|---:|
| lib.rs | 20 |
| main.rs | 28 |
| windows.rs | 543 |
| sys/windows.rs | 986 |
| link/windows.rs | 170 |
| 합계 | **1,747** |

규범 1,700행보다 **47행 초과**한다. Adopt의 프로세스·Job·IOCP 핸들 보존과 세션 복원, inbox signal 제어, 요청 핸들 인코딩이 추가되었고 업그레이드 복원은 같은 함수를 재사용했다. 코드 압축은 하지 않았다. 코디네이터에게 사전 문의했고, 사용자에게 초과 수치를 전달한 뒤 구현·검증·커밋을 완료하고 상한 판단은 통합 단계에서 맡기라는 지시를 받았다. 하네스·규범 파일은 바꾸지 않았다. macOS 선택 holder 소스는 변하지 않았다.

Windows 10/11·GUI 앱·재부팅·로그오프는 미확인이다.
