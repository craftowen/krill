# OWE-832 — Windows holder 구현·단독 실측

측정일: 2026-10-18. 공유 GCP `krill-win`, Windows Server 2022 10.0.20348.5761, Rust 1.98.1 MSVC. 번들은 OWE-826에서 받은 Microsoft.Windows.Console.ConPTY 1.25.260930003의 x64 DLL·OpenConsole을 사용했다.

## 실측 표

| 시나리오 | kernel32 축소 모드 | 번들 DLL 모드 | 관측 범위 |
|---|---|---|---|
| 세션 생성→출력 | PASS | PASS | 실제 holder가 띄운 자식 프로브가 `Spawned`의 in/out/sig/ref/proc를 수신, cmd의 변수 확장 결과로 실행·출력 확인 |
| Resize 80×25→100×35 | PASS | PASS | `Resize`의 Done 및 `mode con`의 Lines 35 / Columns 100 확인 |
| holder `TerminateProcess` 뒤 출력 | PASS | PASS | krilld 역할 프로브가 sig 사본을 유지, `AFTER_HOLDER_KILL_832` 실행 결과 수신 |
| Terminate→Exited | PASS | PASS | holder가 자기 conhost를 종료하고 Job의 ACTIVE_PROCESS_ZERO 뒤 Exited 전달 |
| 종료 후 conhost 잔존 | 없음 | 없음 | 프로브가 가진 conhost 프로세스 핸들이 signaled인지 확인, 최대 10초 제한 |
| 활성 세션에서 UpgradeHolder | EBUSY(16) | PASS | 축소 모드는 거절; 번들은 다른 holder PID의 Hello→Done 뒤 같은 셸 출력 확인 |
| 익명 파이프 프레임·EOF·잘린 프레임 | PASS | 동일 전송로 | MSVC `cargo nextest run -p krill-holder --lib --locked` 3/3 |
| 전송 실패 시 원격 사본 회수 | PASS | 동일 전송로 | DUPLICATE_CLOSE_SOURCE 뒤 GetHandleInformation이 invalid handle로 실패하는 단위 테스트 |
| 세션 0개에서 UpgradeHolder | PASS | PASS | 다른 holder PID의 Hello→Done |
| krilld `TerminateProcess`→새 krilld | PASS | PASS | Session 재전달과 기존 listener 재전달 후 accept 성공 |
| holder 교체→krilld 강제종료→listener 재전달 | 해당 없음(활성 세션 교체 EBUSY) | PASS | 교체된 holder가 보관한 Winsock listener를 다음 krilld 앞으로 재발급, accept 성공 |
| 분리 자식의 5초 Job 강제 종료 | PASS, 5,020ms | PASS, 5,028ms | DETACHED_PROCESS 자식을 두고 Terminate 요청부터 ACTIVE_PROCESS_ZERO/Exited까지 측정 |

Debug 13개 시나리오를 통과한 뒤, 최종 소스의 **release 바이너리에서도 13개 모두 exit 0**으로 재확인했다(CONVENTIONS P5). 위 강제 Job 정리 시간은 release 측정값이다. 셸 환경에서 holder 전용 두 변수가 제거되는 검사도 통과했다. 최종 프로브 경로의 잔존 프로세스는 **0개**다. **VM 사용 종료이며 공유 VM은 중지하지 않았다.**

## 재현

스파이크 프로브·소스 아카이브·로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-832/`, VM `C:\spike\OWE-832\`에 있다. 프로브는 저장소에 추가하지 않았다. VM에서만 최소 3개 크레이트를 workspace member로 설정하고 vendor patch를 제외한 빌드용 manifest를 썼으며 krill-holder 소스는 그대로 사용했다.

```powershell
Set-Location C:\spike\OWE-832
cargo build --release -p krill-holder --bins --locked
cargo nextest run -p krill-holder --lib --locked
New-Item -ItemType Directory -Force release-inbox,release-dll | Out-Null
Copy-Item target\release\holder-probe.exe,target\release\krill-holder.exe release-inbox -Force
Copy-Item target\release\holder-probe.exe,target\release\krill-holder.exe release-dll -Force
Push-Location release-dll
tar -xf ..\bundle.tar.gz
Pop-Location
$stamp = [DateTime]::UtcNow.ToString("yyyyMMddHHmmss")
foreach ($family in @("inbox","dll")) {
    $modes = @("normal","survive","upgrade","restart","empty","tree")
    if ($family -eq "dll") { $modes += @("handoff") }
    foreach ($mode in $modes) {
        & ".\release-$family\holder-probe.exe" $mode "C:\spike\OWE-832\repro-$stamp-$family-$mode"
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
}
```

각 실행 디렉터리는 새 경로를 사용한다. `output.bin`, holder/daemon/conhost PID, `done` 파일이 증거이며 실패하면 `error`를 남긴다. 셸 입력 문자열 자체에는 완성된 출력 마커를 넣지 않고 환경변수 확장을 사용해 단순 입력 에코를 성공으로 세지 않는다.

초기 시험에서 cmd 표준 출력이 부모 SSH stdout으로 새는 문제를 확인해 ConPTY 자식의 STARTF_USESTDHANDLES를 설정했다. 초기 프로브의 이미 종료된 holder 재종료 오류도 수정한 뒤 최종 시나리오가 exit 0으로 끝났다. Spawn.env에 holder 전용 KRILL_HOLDER_LINK/STATE를 의도적으로 넣는 프로브를 추가하고, 셸 환경에서 두 변수가 제거되는지도 확인했다.

## 설계에 주는 함의·남은 검증

holder는 PTY 데이터를 읽지 않는다. 익명 파이프 두 개만 HANDLE_LIST로 자식 krilld에 상속하고, 세션 핸들은 그 자식 PID로 DuplicateHandle해 전달한다. 수신 핸들은 상속 플래그를 지워 krilld의 도구 실행에 새지 않게 한다. Job에는 KILL_ON_JOB_CLOSE를 설정하지 않는다.

번들 HPCON은 공개 ConptyPackPseudoConsole로 재구성한다. inbox의 sig/ref/proc 추출은 [Microsoft winconpty.h의 공유 ABI](https://github.com/microsoft/terminal/blob/main/src/winconpty/winconpty.h)에 의존하며, inbox 재구성은 하지 않는다. 이번 실측은 Server 2022만 대상으로 했고 Windows 10/11은 미확인이다. 질의 응답·출력 충실도는 OWE-826 조사 범위이며 이번 시험에서 다시 측정하지 않았다.

실제 krilld의 Windows `ReadFile` 리더·Resize/DA1·main의 `connect_inherited` 연결 및 고아 재결합은 **미완료**이며 코디네이터가 OWE-833-8로 분리했다. 해당 워커가 끝낸 선행 변경을 craftowen/window에서 병합하고, 이 작업에서는 registry가 Master=Pty를 받는 경계 및 기존 Unix 소켓 기반 rejoin의 cfg만 정리했다. 리더가 연결되기 전 Windows registry/rejoin은 명시적으로 Unsupported를 반환한다. 본 보고서의 실행 검증은 holder의 실제 자식 krilld 역할 프로브를 사용한다.

## 최종 검사

| 검사 | 결과 |
|---|---|
| Mac clippy -p krill-holder -p krilld --all-targets --locked -- -D warnings | exit 0 |
| Mac nextest run -p krill-holder -p krilld --locked | 509 passed, 11 skipped, 실패 0 |
| Windows check-windows.sh -p krill-holder -p krilld --clippy | exit 0 |
| Windows check-windows.sh --clippy (전체 workspace) | exit 0 |
| MSVC cargo nextest run -p krill-holder --lib --locked | 3 passed, 0 skipped |
| cargo fmt --all --check · git diff --check | exit 0 |

Mac target dir는 /Volumes/krill-build/targets/win/OWE-832, Windows는 /Volumes/krill-build/targets/win/OWE-832-windows를 사용했다. Windows clippy에는 기존 clippy.toml의 std::os::fd::RawFd 미도달 경로 설정 경고가 크레이트마다 반복된다. 코드 오류·소스 lint 오류는 0이며, 하네스는 변경하지 않았다. 전체 scripts/check.sh는 공통 워커 지침에 따라 코디네이터 통합 단계가 담당한다.

## 줄 수 — 규범 상한 미충족, 메인테이너 승인 대기

상한 근거: [CONVENTIONS §5](../CONVENTIONS.md).

주석·공백을 포함한 물리 행 수다. Windows 선택 행은 cfg(unix) 항목과 cfg(test) 모듈을 제외한다. lib.rs는 Unix cfg 쌍 10행, main.rs는 Unix File import 2행·detach 문서/함수 14행을 뺐다. link/windows.rs와 sys/windows.rs는 첫 cfg(test) 전까지 센다.

| 파일 | wc -l 물리 행 | Windows 선택 행 |
|---|---:|---:|
| src/lib.rs | 30 | 20 |
| src/main.rs | 44 | 28 |
| src/link/windows.rs | 200 | 162 |
| src/sys/windows.rs | 928 | 901 |
| src/windows.rs | 505 | 505 |
| 합계 | 1,707 | **1,616** |

Windows 상한 1,200줄보다 **416줄 초과**한다. 규범 파일은 바꾸지 않았다. 코디네이터는 기능을 줄이지 말고 자연스러운 중복만 제거하며 이 표를 근거로 메인테이너의 결정을 받도록 지시했다.

| 기능별 선택 행 | 줄 수 |
|---|---:|
| 공통 선언·main | 48 |
| Windows Link·Packet·Pty | 162 |
| FFI 핸들 소유권·프레임·원격 복제 회수 | 180 |
| FFI DLL 선택·ConPTY 생성/Resize/Close | 218 |
| FFI 명령줄·환경·HANDLE_LIST·CreateProcess·Job/IOCP | 299 |
| FFI Winsock listener 전달 | 69 |
| FFI ACK 핸드오프·ConptyPack | 135 |
| holder 이벤트·요청·재기동·복원 루프 | 505 |
| 합계 | **1,616** |

sys/windows.rs의 분류 경계는 1–180, 181–398, 399–697, 698–766, 767–901행이다. 원시 HANDLE 값을 FFI 한 파일 안에 두는 작업 지침 때문에 이 파일은 800줄 분리 SHOULD도 초과했다. 전달·복원 실패 회수와 각 Win32 호출의 SAFETY 경계를 유지했다.

Mac은 lib.rs 30 + main.rs 44 + holder.rs 680 + link.rs 165 + sys.rs 232 + teardown.rs 91 = **1,242줄**의 보수적 물리 합계로 1,250줄 이내다. 이 합계는 공통 파일의 Windows cfg·테스트까지 포함하므로 Mac 실제 빌드 행의 상한이다.
