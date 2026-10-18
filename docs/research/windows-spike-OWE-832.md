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

세션 수가 0개인 holder 교체, listener를 맡긴 뒤 holder 교체와 krilld 강제종료를 조합한 시험은 진행 중이다. 5초 뒤 강제 Job 정리로만 끝나는 분리 자식 시나리오는 아직 미확인이다.

## 재현

스파이크 프로브·소스 아카이브·로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-832/`, VM `C:\spike\OWE-832\`에 있다. 프로브는 저장소에 추가하지 않았다. VM에서만 최소 3개 크레이트를 workspace member로 설정하고 vendor patch를 제외한 빌드용 manifest를 썼으며 제품 소스는 그대로 사용했다.

```powershell
Set-Location C:\spike\OWE-832
cargo build -p krill-holder --bins --locked
cargo nextest run -p krill-holder --lib --locked
.\bin-inbox\holder-probe.exe normal C:\spike\OWE-832\final-inbox-normal
.\bin-inbox\holder-probe.exe survive C:\spike\OWE-832\final-inbox-survive
.\bin-inbox\holder-probe.exe upgrade C:\spike\OWE-832\final-inbox-upgrade
.\bin-dll\holder-probe.exe normal C:\spike\OWE-832\final-dll-normal
.\bin-dll\holder-probe.exe survive C:\spike\OWE-832\final-dll-survive
.\bin-dll\holder-probe.exe upgrade C:\spike\OWE-832\final-dll-upgrade
```

각 실행 디렉터리는 새 경로를 사용한다. `output.bin`, holder/daemon/conhost PID, `done` 파일이 증거이며 실패하면 `error`를 남긴다. 셸 입력 문자열 자체에는 완성된 출력 마커를 넣지 않고 환경변수 확장을 사용해 단순 입력 에코를 성공으로 세지 않는다.

초기 시험에서 cmd 표준 출력이 부모 SSH stdout으로 새는 문제를 확인해 ConPTY 자식의 STARTF_USESTDHANDLES를 설정했다. 초기 프로브의 이미 종료된 holder 재종료 오류도 수정한 뒤 최종 6개 시나리오가 exit 0으로 끝났다.

## 설계에 주는 함의·남은 검증

holder는 PTY 데이터를 읽지 않는다. 익명 파이프 두 개만 HANDLE_LIST로 자식 krilld에 상속하고, 세션 핸들은 그 자식 PID로 DuplicateHandle해 전달한다. 수신 핸들은 상속 플래그를 지워 krilld의 도구 실행에 새지 않게 한다. Job에는 KILL_ON_JOB_CLOSE를 설정하지 않는다.

번들 HPCON은 공개 ConptyPackPseudoConsole로 재구성한다. inbox의 sig/ref/proc 추출은 [Microsoft winconpty.h의 공유 ABI](https://github.com/microsoft/terminal/blob/main/src/winconpty/winconpty.h)에 의존하며, inbox 재구성은 하지 않는다. 이번 실측은 Server 2022만 대상으로 했고 Windows 10/11은 미확인이다. 질의 응답·출력 충실도는 OWE-826 조사 범위이며 이번 시험에서 다시 측정하지 않았다.

통합 krilld 리더 스레드 연결, 최종 플랫폼별 줄 수, 회귀 검사 결과는 후속 단계에서 이 보고서에 기록한다. 전체 scripts/check.sh는 공통 워커 지침에 따라 코디네이터 통합 단계가 담당한다.
