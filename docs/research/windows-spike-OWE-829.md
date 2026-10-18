# OWE-829 — S-W4: Windows 데몬 수명·기동 방식

측정일: 2026-10-18. 공유 GCP `krill-win`, Windows Server 2022 Datacenter 10.0.20348, Rust 1.98.1 x86_64-pc-windows-msvc, windows-sys 0.61.2.

**결론: detached 실행은 부모 강제종료를 견디지만, Job의 breakaway 허용 여부와 핸들 상속을 별도로 통제해야 한다.** `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`를 주더라도 breakaway 금지 Job에서는 기동 자체가 실패했다. 모든 stdio를 NUL로 설정한 Rust Command도 SSH 정상 종료를 지연시켰으나 `CreateProcessW(bInheritHandles=false)` 대조군은 정상 SSH 종료와 이후 데몬 생존에 성공했다.

로그오프·재부팅은 실행하지 않았다. 코디네이터 ask 응답: “이번에는 미수행·별도 검증. 로그오프·재부팅은 미확인으로 기록하고, 보고서에 나중에 돌릴 재현 절차를 적어 두어라.” 이 보고서는 조사 기록이며 ARCHITECTURE §4.1의 보장을 변경하지 않는다.

## 측정 구조

외부 controller가 앱 역할 parent를 띄우고, parent는 파일 gate가 열릴 때까지 daemon을 만들지 않는다. controller는 parent를 지정 Job에 넣은 뒤 gate를 열어 자식이 Job 밖에서 생성되는 race를 막았다. daemon이 PID·Job 소속·콘솔 정보를 저장하고 heartbeat를 쓰면, controller가 parent에 `Child::kill()`(TerminateProcess)을 호출하고 이어서 자신의 마지막 Job HANDLE을 닫았다.

daemon 생존은 열어 둔 process HANDLE의 `WaitForSingleObject(0)`와 heartbeat 진행으로 확인했다. 파일의 PID가 존재한다는 것만으로 판정하지 않았다. daemon은 최장 120초 후 스스로 종료하며, 정상 실험에서는 controller/검증 명령이 **자기 daemon HANDLE만** 종료했다. 다른 워커의 프로세스·sshd·conhost를 일괄 종료하지 않았다.

기본 SSH 프로세스 자체가 Job 안(`IsProcessInJob=true`)이었다. 따라서 같은 10가지 조합을 SSH Job 안 controller와 `CREATE_BREAKAWAY_FROM_JOB`로 완전히 분리한 controller(`IsProcessInJob=false`)에서 각각 실행했다. 아래 표의 '추가 Job 없음'은 후자의 결과다. 두 controller에서 추가 Job 관련 결과는 같았다.

약어: D=`DETACHED_PROCESS(0x8)`, G=`CREATE_NEW_PROCESS_GROUP(0x200)`, B=`CREATE_BREAKAWAY_FROM_JOB(0x01000000)`, N=`CREATE_NO_WINDOW(0x08000000)`. K=`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, A=`JOB_OBJECT_LIMIT_BREAKAWAY_OK`.

## 실측 표

| parent의 Job | daemon 플래그 | 기동 / daemon Job 소속 | parent TerminateProcess 후 | 마지막 시험 Job HANDLE close 후 |
|---|---|---|---|---|
| 추가 Job 없음 | 0 | 성공 / 없음 | 생존 | 생존(닫을 시험 Job 없음) |
| 추가 Job 없음 | D+G+B | 성공 / 없음 | 생존 | 생존 |
| 추가 Job 없음 | N | 성공 / 없음 | 생존 | 생존 |
| 추가 Job 없음 | D+G+B+N | 성공 / 없음 | 생존 | 생존 |
| K, breakaway 금지 | 0 | 성공 / 있음 | 생존 | **종료** |
| K, breakaway 금지 | D+G | 성공 / 있음 | 생존 | **종료** |
| K, breakaway 금지 | D+G+B | **실패** / 해당 없음 | 해당 없음 | spawn 오류 5 Access denied |
| K+A | D+G (B 없음) | 성공 / 있음 | 생존 | **종료** |
| K+A | D+G+B | 성공 / 없음 | 생존 | **생존**, heartbeat 진행 |
| K+A | N+G+B | 성공 / 없음 | 생존 | **생존**, heartbeat 진행 |

parent 종료 뒤 300ms, Job close 뒤 추가 350ms의 관측이다. 종료 사례의 heartbeat는 tick 3에서 멈추고 process HANDLE이 signaled 상태가 됐다. 생존 사례는 tick 6까지 진행한 뒤 시험이 명시적으로 정리했다. 장시간 스트레스·실제 IDE별 nested Job 제한은 미확인이다.

| 콘솔·SSH 항목 | 실측 결과 | 해석·제한 |
|---|---|---|
| 기본 console EXE 실행 | console HWND 비영, CP437 | 부모 콘솔 공유 또는 새 콘솔 생성 가능; GUI 앱에서 무창 기동으로 쓰면 안 됨 |
| D+G(+B) | HWND=0, CP=0, 콘솔 프로세스 수=0 | 콘솔 연결 없는 daemon |
| N(+G+B) | HWND=0, CP437, 콘솔 프로세스 수=1 | 창 없는 콘솔과 완전 detach를 구분해야 함 |
| D+G+B+N | D+G+B와 동일 | N은 D와 함께 쓰면 무시된다는 공식 사양과 일치 |
| std Command, stdio 모두 NUL, D+G+B | SSH 정상 종료 **15초 timeout** | daemon은 Job 밖·콘솔 없음이어도 SSH 종료가 지연됨 |
| 위 SSH 전송을 강제 닫고 새 SSH 접속 | daemon 생존 | PID 1088, heartbeat tick177→183. 이전 혼합 시험에서 기본 플래그 자식도 생존했으므로 이 VM의 전송 종료를 모든 Windows 종료 상황으로 일반화하지 않음 |
| raw CreateProcessW, bInheritHandles=true, D+G+B | SSH 정상 종료 **15초 timeout** | 같은 raw 경로에서 bool만 바꾼 대조군; 전송 종료 후 PID 1888 생존·tick178→184 |
| raw CreateProcessW, bInheritHandles=false, D+G+B | SSH exit 0, 단일 관측 1271ms | 이후 새 SSH 연결에서 PID 4180 생존·heartbeat tick43→49, 검증 뒤 명시 종료 |
| 로그오프·재부팅 | **미확인 / 미수행** | 공유 VM이므로 코디네이터 승인에 따라 연기 |
| 실제 Krill·ConPTY·로그오프·보안 정책별 환경 | 미확인 | 이번 daemon은 파일 heartbeat만 쓰는 별도 console EXE |

raw true/false는 동일 STARTUPINFO·플래그에서 핸들 상속 BOOL만 바꿨다. 따라서 이번 SSH 종료 지연에는 상속 가능한 핸들의 유입이 영향을 줬다는 근거가 있다. 정확히 어떤 비stdio 핸들이었는지는 열거하지 않아 미확인이다.

`GetConsoleWindow()==0`은 API 관측이며 RDP 화면을 보며 창 깜빡임까지 확인한 것은 아니다. 1271ms는 SSH 접속·PowerShell 기동을 포함한 한 관측값이며 daemon 기동 성능 수치가 아니다.

## OWE-833 / Phase 3 설계 권고

1. 기본 daemon 부트스트랩은 콘솔 없는 D+G와 Job 분리 B를 사용하되, spawn 오류를 확인한다. B가 거부될 때 이를 빼고 조용히 재시도하면 앱을 담은 Job 종료 시 세션이 죽는다. 성공 뒤 인증된 handshake로 daemon/holder PID·기동 세대를 확인하고 Job 소속도 기록한다. 기존 Job을 강제로 변경하려 하지 않는다.
2. **플래그만으로 독립 daemon이 되지 않는다.** Rust 1.98.1 소스에서 Command는 기본 `inherit_handles=true`, CreateProcessW bInheritHandles=true이며 자동 HANDLE_LIST를 만들지 않는다. stdio=NUL만으로 다른 상속 가능한 핸들을 배제할 수 없다. 해당 버전의 `CommandExt::inherit_handles(false)`와 `spawn_with_attributes`는 unstable이므로 이를 stable/MSRV 해결책으로 쓰지 않는다. 허용 FFI 모듈의 작은 CreateProcessW wrapper에서 초기 bootstrap은 bInheritHandles=false로 시작하고, 필요한 소켓/pipe만 명시 중복화 또는 HANDLE_LIST로 넘기는 설계를 권고한다. 환경·Unicode 경로·오류·HANDLE RAII도 wrapper가 책임져야 한다.
3. D와 N을 무조건 함께 OR하지 않는다. D는 콘솔 연결을 끊고 N은 창 없는 console 실행을 의미한다. daemon/holder는 D 경로, 특정 자식 명령은 목적에 맞는 N/ConPTY 경로로 구분한다. G는 console control group이며 Job에서 분리하는 플래그가 아니다.
4. breakaway 금지 IDE/런처 환경에서는 앱이 띄운 자식에 생존을 보장할 수 없다. 이미 독립적으로 실행 중인 사용자 broker/holder에 연결하거나, 앱 Job 밖에서 기동하는 예약 작업을 검증한 뒤 사용해야 한다. 예약 작업이 모든 nested Job·정책에서 해결한다는 주장은 **추정**이다. 그 경로가 검증되기 전에는 기동 실패를 명확히 알리고 '세션 생존' 성공으로 처리하지 않는다.
5. Task Scheduler를 도입하더라도 holder의 세션 감시·재기동 정책과 충돌하지 않게 단일 기동 권한·단일 인스턴스·backoff를 정한다. daemon 재시작과 사용자 프로그램/ConPTY 생존은 별도 축이다. holder/ConPTY 보장은 OWE-826 결과와 함께 확정해야 한다.

## 자동 기동 선택안 (공식 문서 조사, 제품 실동작은 미측정)

| 선택 | 장점 | 비용·제한 | 권고 |
|---|---|---|---|
| 앱의 필요 시 bootstrap | 등록 없이 사용 시 기동 가능(설계) | restrictive Job에서 B가 거부됨(실측) | 1차 기본안: 이미 있는 daemon 연결 → 제한된 bootstrap → 명시 오류 |
| HKCU Run | 사용자별 매 로그인 자동 실행, 구조가 단순 | 즉시 실행 순서/시각 보장 없음, 명령 길이 260자, crash restart·로그오프 생존 정책은 별도 설계 | 로그인 시 autostart opt-in의 단순 후보; IDE Job 밖 시작은 추정·추가 실측 필요 |
| Task Scheduler InteractiveToken + logon trigger | 사용자 로그인 세션에서 별도 launch 경로 및 설정 가능 | 로그인된 세션이 필요; 태스크 설치/업데이트/삭제·중복 기동·정책 처리 필요 | restrictive Job용 사용자 broker 후보, 실제 설치권한·Job·세션 동작을 먼저 실측 |
| Task Scheduler Password/S4U 또는 service account | 비대화형 시작/boot trigger 등 구성 가능 | Password는 등록 자격증명 필요; S4U는 네트워크·암호화 파일 접근 제한; Batch logon 권한·DPAPI 사용자 경계·agent 인증/SSH 영향 검증 필요 | 초기 기본값에서 제외. 로그오프 후 무인 생존 요구를 확정한 뒤 별도 설계 |

Task Scheduler 기본 실행 제한은 72시간이므로 상주 프로세스를 직접 예약 작업으로 돌릴 때 `ExecutionTimeLimit=PT0S` 등 의도한 수명 설정을 명시해야 한다. 배터리/유휴/네트워크 조건·다중 인스턴스·재시작 설정도 확인한다. 이번 S-W2의 임시 사용자 예약 작업 시도는 프로그램 실행 증거를 얻지 못했으므로 제품용 Task Scheduler가 검증됐다고 간주하지 않는다.

**기동 방식 결정안:** 당장은 필요 시 독립 bootstrap을 기본으로 하고, 로그인 자동 시작은 별도 opt-in으로 설계한다. 제한 Job의 해결책과 로그오프 후 지속 실행은 별도 검증 전까지 보장하지 않는다. HKCU Run만으로 crash restart나 로그오프 생존을 해결했다고 설명하지 않는다. 재부팅 뒤에는 live process가 아니라 디스크 기록 복원·agent resume를 검증해야 한다(ARCHITECTURE §4.1).

## 재현·검증·나중에 할 시험

코드·Cargo.lock·원시 로그: 저장소 밖 `~/.cache/krill-win/spikes/OWE-829/`, VM `C:\spike\OWE-829\`. 소스는 저장소에 넣지 않았다.

```sh
~/.cache/krill-win/gcp/ssh.sh 'Set-Location C:\spike\OWE-829; cargo build --locked; if ($LASTEXITCODE -ne 0) {exit $LASTEXITCODE}; cargo fmt --check; if ($LASTEXITCODE -ne 0) {exit $LASTEXITCODE}; & .\target\debug\lifetime-spike.exe | Tee-Object matrix.log'
~/.cache/krill-win/gcp/ssh.sh 'Set-Location C:\spike\OWE-829; & .\target\debug\lifetime-spike.exe outside-matrix'
# 각 helper는 launch SSH를 닫은 뒤 새 SSH로 확인하고 자기 daemon을 종료한다.
python3 ~/.cache/krill-win/spikes/OWE-829/ssh_disconnect.py
python3 ~/.cache/krill-win/spikes/OWE-829/ssh_natural.py std
python3 ~/.cache/krill-win/spikes/OWE-829/ssh_natural.py raw
python3 ~/.cache/krill-win/spikes/OWE-829/ssh_natural.py raw-inherit
```

`cargo build --locked`·`cargo fmt --check` 오류·경고 0, 최종 matrix 두 환경 각 exit 0. Job deny의 Access denied, Job close의 자식 종료, std SSH timeout은 의도적으로 측정한 제한이다. 실행 중인 exe가 잠겨 재빌드가 실패한 중간 시도가 있었으며, 해당 spike PID 정리 후 재빌드·최종 검증은 성공했다. `matrix.log`, `outside-matrix.log`, `ssh-disconnect.log`, `ssh-natural-std.log`, `ssh-natural-raw.log`, `ssh-natural-raw-inherit.log` 및 대응 VM launch/verify 로그가 증거다.

로그오프/재부팅의 후속 재현 절차(공유 워커 종료 및 코디네이터의 별도 허가 후에만):

1. 전용 Windows RDP 사용자 세션에서 daemon/holder와 시험 agent를 기동하고 PID·세션 ID·Job·heartbeat·출력 seq를 별도 확인자에게 기록한다. 파일이 쓰이는 상태를 확인한 뒤 해당 **자기 세션 ID**를 `quser`로 확인한다.
2. 별도 관리자 접속을 유지한 채 해당 사용자 세션만 로그오프한다. 확인자는 기존 process HANDLE/PID·heartbeat·ConPTY 자식·출력 연속성을 측정한다. 재로그인 후 UI 재접속과 사용자 DPAPI 키 접근을 확인한다. 세션 disconnect/RDP 창 닫기를 로그오프로 대신 세지 않는다.
3. 별도 승인된 재부팅 실험에서는 재부팅 전 마지막 세그먼트/seq를 기록하고, 부팅 후 HKCU Run/예약 작업이 실제 시작된 시점과 사용자 로그온 의존성, 디스크 복원·agent resume를 확인한다. 기존 live PID 생존을 기대하지 않는다. 실제 autostart 등록/삭제는 그 시험에 한해 수행한다.

문서만 변경했으므로 저장소 크레이트 회귀 검사는 추가하지 않았으며 전체 `scripts/check.sh`는 코디네이터 통합 검증에서 수행한다. 최종 확인에서 자기 spike 실행 프로세스 0개, 임시 사용자·예약 작업·자격증명 파일 모두 없음(False)을 확인했다. **VM 사용 종료이며 공유 VM은 중지하지 않는다.**

참조(2026-10-18 조회): [Windows process creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags), [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects), [Rust 1.98.1 Command 구현](https://github.com/rust-lang/rust/blob/1.98.1/library/std/src/sys/process/windows.rs), [Rust 1.98.1 Windows 확장 API 안정성](https://github.com/rust-lang/rust/blob/1.98.1/library/std/src/os/windows/process.rs), [HKCU Run/RunOnce](https://learn.microsoft.com/en-us/windows/win32/setupapi/run-and-runonce-registry-keys), [Task 보안 컨텍스트](https://learn.microsoft.com/en-us/windows/win32/taskschd/security-contexts-for-running-tasks), [Task logon 종류](https://learn.microsoft.com/en-us/windows/win32/api/taskschd/ne-taskschd-task_logon_type), [Task 실행 시간 제한](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-executiontimelimit). 근거 boost 보고서 §4 Phase 2/3·§5와 대조했다.
