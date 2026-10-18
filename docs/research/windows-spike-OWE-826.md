# 스파이크 S-W1 (OWE-826 · MK-207): ConPTY 세션 생존·핸드오프·출력 충실도

**결론: ConPTY 세션의 생사는 HPCON을 만든 프로세스가 아니라 "signal 핸들 사본이 하나라도 살아 있는가"로 정해진다. 그래서 holder 교체·holder 크래시 생존·krilld 교체가 모두 된다.** 다만 OS 기본 conhost(kernel32 ConPTY)는 출력을 다시 그리고 DA1·DA2·CPR·DSR 질의에 스스로 응답한다. 그러면 Krill 불변식 "질의 응답자는 krilld 하나"와 상태 엔진의 충실도가 깨진다. 그래서 **권장안은 B(conpty.dll + OpenConsole.exe 번들)** 이고, kernel32 경로는 번들이 없을 때만 쓰는 축소 모드로 둔다.

- 실측 환경: GCP `krill-win`, Windows Server 2022 Datacenter 21H2 (빌드 20348.5761), 세션 0(OpenSSH와 WMI `Win32_Process.Create`로 띄움, 대화형 데스크톱 없음). Rust 1.98.1 msvc, windows-sys 0.61.2.
- 번들: NuGet `Microsoft.Windows.Console.ConPTY` 1.25.260930003 (nuspec 라이선스 `MIT`). x64 크기는 `conpty.dll` 119,608 B, `OpenConsole.exe` 1,089,376 B. conpty.dll은 `<dll 폴더>\x64\OpenConsole.exe`를 띄웠다(실측. 다른 배치는 미확인).
- 스파이크 코드는 저장소 밖 `~/.cache/krill-win/spikes/OWE-826/`에 있다. `src/main.rs` 한 바이너리가 holder·holder2·krilld·counter·emit·size·tree 역할을 모두 한다. 시나리오 스크립트는 `scripts/q*.ps1`, 원시 출력은 `results/`에 있다. 프로세스 사이 IPC는 세션 폴더의 파일(`req-<pid>`/`resp-<pid>`/`cmd.txt`)로 흉내 냈고, 핸들은 holder가 `DuplicateHandle`로 krilld 프로세스에 넣는다.
- 용어: **inbox**는 kernel32 `CreatePseudoConsole`로 `C:\Windows\System32\conhost.exe`를 쓰는 경로다. **dll**은 번들 `ConptyCreatePseudoConsole`로 `OpenConsole.exe`를 쓰는 경로다. **sig/ref/proc**는 HPCON 내부 핸들 세 개(signal 파이프, `\Reference`, conhost 프로세스)다.

## 1. holder(HPCON 소유자) `TerminateProcess` 시 생존

절차(`scripts/q1.ps1`): holder가 `cmd.exe /k`(또는 `pwsh -NoProfile`)를 띄운다 → krilld가 핸들을 받고 `spike.exe counter <파일> 200`을 입력한다(셸의 자식이 200ms마다 출력하고 파일도 갱신) → `taskkill /F /PID <holder>` → 3초 뒤와 5초 뒤에 프로세스·카운터 파일·krilld 출력 파일 크기를 본다.

| API | 셸 | krilld가 쥔 핸들 | holder kill 뒤 conhost | 셸 | 자식(counter) | 출력 파이프 |
|---|---|---|---|---|---|---|
| inbox | cmd | in/out만 | **죽음**(15ms 안) | 죽음 | 죽음 | `ReadFile` → ERROR_BROKEN_PIPE(109) |
| inbox | cmd | in/out + sig/ref/proc | 산다 | 산다 | 산다(12→27→37) | 계속 읽힘(322→472→502 B) |
| inbox | cmd | in/out + **sig만** | 산다 | 산다 | 산다(12→27→37) | 계속 읽힘 |
| inbox | cmd | in/out + ref만 | 죽음 | 죽음 | 죽음 | 끊김 |
| inbox | cmd | in/out + proc만 | 죽음 | 죽음 | 죽음 | 끊김 |
| dll | cmd | in/out만 | 죽음 | 죽음 | 죽음 | 109 |
| dll | cmd | in/out + sig/ref/proc | 산다 | 산다 | 산다 | 계속 읽힘(366→516→546 B) |
| dll | cmd | in/out + **sig만** | 산다 | 산다 | 산다 | 계속 읽힘 |
| dll | cmd | in/out + ref만 | **OpenConsole만 산다** | 죽음 | 죽음 | — |
| dll | cmd | in/out + proc만 | 죽음 | 죽음 | 죽음 | — |
| inbox / dll | pwsh | in/out만 | — | — | 멈춤(12→12) | — |
| inbox / dll | pwsh | in/out + sig만 | — | — | 산다(12→27→37) | — |

- 정리하면, **signal 파이프의 쓰기 핸들이 모두 닫혀야 conhost가 끝나고 셸이 따라 죽는다.** 부모(holder) 프로세스가 죽는 것 자체는 원인이 아니다. 살아남은 conhost의 ParentProcessId는 죽은 holder pid를 그대로 가리켰다.
- holder가 만든 Job(`KILL_ON_JOB_CLOSE` 없음)은 holder가 죽은 뒤에도 프로세스를 죽이지 않았다. `KILL_ON_JOB_CLOSE`를 켰을 때는 측정하지 않았다(미확인).
- inbox HPCON의 내부 배치 `{hSignal, hPtyReference, hConPtyProcess}`는 실측으로 맞는 것을 확인했다. 세 번째 핸들의 `GetProcessId`가 conhost pid와 같았고, 첫 번째 핸들에 resize 패킷을 쓰면 크기가 바뀌었다(§5). 이 배치는 **문서에 없는 구조**다.

## 2. krilld kill → 새 krilld가 holder에게서 핸들을 다시 받음

절차(`scripts/q2.ps1`): krilld1이 counter를 실행 → 3초 뒤 `taskkill /F` krilld1 → 5초 동안 krilld 없음 → krilld2가 holder에 다시 요청해 `DuplicateHandle`로 받는다 → Ctrl+C(0x03)와 `echo INPUT-OK` 입력 → krilld1·krilld2 출력 파일을 이어 붙여 `C=<n>` 연속성을 검사한다.

| API | counter 주기 | krilld 부재 | 부재 중 counter | 출력 연속성(krilld1+krilld2) | 입력(새 krilld) |
|---|---|---|---|---|---|
| inbox | 10ms | 4.4초 | 계속 증가(195→634) | 1..881, 빠진 값 0 | Ctrl+C·`INPUT-OK` 반영 |
| dll | 10ms | 4.4초 | 계속 증가(186→642) | 1..895, 빠진 값 0 | 반영 |
| inbox | 0ms(최대 속도) | 5초 | 447→**1039에서 멈춤** | 1..1508, 바이트 경계까지 이어짐(krilld1 끝 `C=460\r`, krilld2 시작 `\n`) | 반영 |
| dll | 0ms | 5초 | 468→**1058에서 멈춤** | 1..1630, 빠진 값 0 | 반영 |

- 읽는 쪽이 없으면 conhost/OpenConsole은 버리지 않고 **셸의 쓰기를 막는다(백프레셔)**. 손실은 macOS와 같은 범위로 한정된다. 즉 "krilld가 읽었지만 세그먼트에 쓰기 전 바이트"만 잃을 수 있다. 기본 파이프 크기(`CreatePipe` nSize=0)에서 krilld1이 마지막으로 읽은 줄 뒤로 약 580줄(inbox 459→1039, dll 482→1058, 줄당 약 8바이트로 약 4.6KB, 추정)을 쌓은 뒤 멈췄다.
- 스크롤 출력만 측정했다. 전체 화면 TUI를 재렌더하는 경우의 손실은 따로 측정하지 않았다(미확인. inbox는 §4대로 화면을 다시 그리므로 원본 바이트를 보장하지 않는다).

## 3. holder 교체(exec 핸드오프 대체)

절차(`scripts/q3.ps1`·`q5.ps1`·`qc.ps1`): `cmd.txt`에 `handoff`를 쓰면 holder가 in·out·sig·ref·proc·job(·IOCP)을 상속 가능한 사본으로 만들고, `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 그 핸들만 상속하게 해 `spike.exe holder2 …<핸들 값>`을 `DETACHED_PROCESS`로 띄운 뒤 `ClosePseudoConsole` 없이 종료한다. holder2는 HPCON을 다시 만든다. inbox는 구조체를 직접 만들고, dll은 `ConptyPackPseudoConsole(hProc, hRef, hSignal, &hpcon)`(conpty.h에 공개된 함수)을 쓴다.

| 항목 | inbox | dll |
|---|---|---|
| 1세대→2세대→3세대 교체 동안 conhost·셸·counter | 생존(같은 pid) | 생존(같은 pid) |
| 교체된 holder에서 HPCON 재구성 | 구조체 직접 구성(문서 밖) | `ConptyPackPseudoConsole` hr=0 |
| 교체된 holder의 `ResizePseudoConsole` 100×30, 120×40 | hr=0, 자식이 본 크기 80×25→100×30→120×40 | hr=0, 같음 |
| 교체된 holder가 새 krilld에 `DuplicateHandle` | 됨 | 됨 |
| 상속한 job 핸들로 `JobObjectBasicProcessIdList` | `assigned=2 [셸, 자식]` | 같음 |
| 새 IOCP를 같은 job에 다시 연결 | **실패**: ERROR_INVALID_PARAMETER(87) | **실패**(87) |
| IOCP 핸들 자체를 상속 | 됨. 교체 뒤 `NEW_PROCESS`·`ACTIVE_PROCESS_ZERO` 수신 | 됨 |
| 교체된 holder의 `ClosePseudoConsole` | conhost·셸 종료, krilld 읽기 109 | 같음 |

- OpenConsole을 holder가 직접 띄울 필요는 없었다. conpty.dll이 띄운 OpenConsole의 sig·ref·proc는 보통 핸들이라 상속·복제가 되고, `ConptyPackPseudoConsole`로 다시 묶인다. inbox도 같은 방식으로 되지만 구조체 배치에 기대야 한다.
- 교체 사이에 IOCP 핸들을 넘기지 않으면 그 구간의 job 메시지를 잃는다. 실제로 IOCP를 넘기지 않은 Q3 실행에서는 Ctrl+C로 끝난 counter의 `EXIT_PROCESS`를 받지 못했다.

## 4. 출력 충실도와 질의 응답

절차(`scripts/q4.ps1`): holder가 `spike.exe emit`을 직접 실행한다. 출력 모드는 `ENABLE_VIRTUAL_TERMINAL_PROCESSING`, UTF-8이다. 실행한 krilld는 두 종류다. 하나(noans)는 아무 질의에도 답하지 않는다. 다른 하나(answer)는 출력에서 `CSI c`를 보면 `CSI ?65;99c`, `CSI 6n`을 보면 `CSI 77;77R`처럼 표시용 값으로 답한다. 자식은 stdin(`ENABLE_VIRTUAL_TERMINAL_INPUT`)으로 무엇이 오는지 기록한다.

| 입력(자식이 쓴 바이트) | inbox conhost (20348) | dll OpenConsole 1.25 |
|---|---|---|
| `CSI 1;3;4;38;2;10;20;30;48;5;200m` | 낱개 SGR로 풀어 순서를 바꿔 냄 | 원본 그대로 |
| `CSI 4:3m`(물결 밑줄), `CSI 58:2::255:0:0m`(밑줄 색) | **사라짐**(속성 없이 글자만) | 그대로 |
| OSC 8 하이퍼링크 | 열기·닫기가 링크 글자보다 **먼저** 나와 링크가 빈 상태가 됨 | 그대로 |
| OSC 133 A/B/C/D | 네 개가 한꺼번에 글자 앞에 몰려 **위치 정보가 사라짐** | 글자 사이 원래 위치 그대로 |
| OSC 7, OSC 0(제목), OSC 9, BEL | 통과 | 그대로 |
| 커서 이동 `CSI 5;10H` | 통과(앞뒤에 `?25l/h` 추가) | 그대로 |
| 151자 긴 줄(폭 80) | 한 줄로 나옴(래핑 표식 없음) | 그대로 |
| 한글·가나·이모지 | 통과 | 그대로 |
| 대체 화면 `?1049h … ?1049l` | **전달 안 됨**. conhost가 직접 처리하고 화면 전체를 다시 그림(깨진 줄 포함) | 그대로 |
| `?1000h ?1006h`(마우스) | **사라짐** | 그대로 |
| `?2004h ?2026h/l CSI >1u CSI 5 q` | 통과 | 그대로 |
| 시작·종료 때 conhost가 덧붙이는 것 | 시작 `CSI 2J CSI m CSI H` + OSC 0(제목) | 시작 `CSI 1t CSI c ?1004h ?9001h`, 종료 `?1004l ?9001l` |
| **DA1 `CSI c`** | **conhost가 스스로 응답** `CSI ?1;0c`. 터미널로 넘기지 않음 | 터미널(krilld)로 넘김. answer 실행에서 자식이 krilld의 `CSI ?65;99c`를 받음 |
| DA2 `CSI >c` | 스스로 응답 `CSI >0;10;1c` | 넘김 |
| CPR `CSI 6n` | 스스로 응답 `CSI 25;32R` | 넘김. 자식이 krilld의 `CSI 77;77R`를 받음 |
| DSR `CSI 5n` | 스스로 응답 `CSI 0n` | 넘김 |
| OSC 11 `?`, DECRQM `?2026$p`, XTVERSION `CSI >0q` | 넘김(응답 없음) | 넘김 |

- `PSEUDOCONSOLE_PASSTHROUGH_MODE`는 1.25의 `inc/conpty.h`에 **없다**. 같은 값 0x8은 이제 `PSEUDOCONSOLE_GLYPH_WIDTH_GRAPHEMES`다. 1.25는 플래그 0에서 이미 바이트를 그대로 통과시켰다. inbox에 0x8을 주면 hr=0으로 받아들이지만 출력은 플래그 0과 똑같았다(효과 없음).
- OpenConsole은 시작할 때 터미널에 DA1(`CSI c`)을 보낸다. noans 실행에서도 자식 출력이 늦어지지 않았다(holder 시작부터 첫 질의까지 3.35초 vs answer 실행 3.34초, 둘 다 사례 11개×0.3초). 응답하지 않을 때 다른 영향이 있는지는 미확인이다. `?9001h`(win32-input-mode 요청)를 무시하고 일반 VT 입력(Ctrl+C, 글자)을 보내도 입력은 정상이었다(Q2).
- **함의**: inbox 경로에서는 conhost가 질의 응답자 하나를 더 차지한다. "질의 응답자는 krilld 하나"를 지키려면 dll 경로가 필요하다. inbox 축소 모드를 허용한다면 ARCHITECTURE §4에 "Windows inbox ConPTY: DA1·DA2·CPR·DSR은 conhost가 답하고 krilld는 나머지(OSC 10/11 등)에 답한다"는 예외를 적어야 한다.

## 5. 세션 묶음: Job Object + IOCP, resize 권한

절차(`scripts/q5.ps1`): 셸에서 `spike.exe tree`를 실행한다. tree는 A(2초), B(4초), C(`DETACHED_PROCESS`, 60초), D(`CREATE_BREAKAWAY_FROM_JOB`)를 띄운다 → holder 교체(IOCP 상속) → krilld2(`keep=sig`)가 `spike.exe size`를 실행하고 직접 resize한다 → `jobpids` → `TerminateJobObject`.

| 항목 | inbox | dll |
|---|---|---|
| `NEW_PROCESS`: 셸, tree, A, B, C | 모두 수신 | 모두 수신 |
| `EXIT_PROCESS`: tree, A(2초 뒤), B(4초 뒤) | 정시에 수신 | 같음 |
| D `CREATE_BREAKAWAY_FROM_JOB` | `CreateProcessW` ERROR_ACCESS_DENIED(5), job 탈출 불가 | 같음 |
| C(`DETACHED_PROCESS`)의 job 소속 | 소속(`jobpids`에 있음) | 같음 |
| `TerminateJobObject` | 셸·C·size 모두 종료. **프로세스별 `EXIT_PROCESS`는 오지 않고 `ACTIVE_PROCESS_ZERO`만 옴** | 같음 |
| `TerminateJobObject` 뒤 conhost/OpenConsole | **5초 넘게 살아 있음**(job 밖, holder가 띄운 것) → `ClosePseudoConsole`로 정리해야 함(§3 표 마지막 줄) | 같음 |
| krilld(비소유자)가 sig에 resize 패킷 `[u16 8, cols, rows]`를 직접 씀 | 성공, 자식이 본 크기 80×25→90×20 | 같음 |
| krilld가 만든 가짜 HPCON `{sig, ref, proc}`로 kernel32 `ResizePseudoConsole` | hr=0, 110×35 | hr=0, 110×35(OpenConsole도 같은 신호 형식) |

- **`ResizePseudoConsole`은 HPCON 소유 프로세스만 할 수 있는 것이 아니다.** signal 핸들을 가진 프로세스라면 누구나 할 수 있다. dll 경로라면 krilld도 `ConptyPackPseudoConsole`로 자기 HPCON을 만들어 `ConptyResizePseudoConsole`을 부를 수 있을 것이다(holder2에서만 실측했고, krilld에서 이 호출은 추정). 이렇게 만든 HPCON에 `Close`/`Release`를 부르면 세션이 닫힌다(Close는 §3에서 실측, Release는 미측정). 그러니 krilld는 그 HPCON을 버리기만 해야 한다.
- 세션당 상주 메모리(`scripts/qm.ps1`, `dir System32` 출력 뒤): conhost는 WS 5.2MB / Private 1.2MB, OpenConsole은 WS 8.0MB / Private 1.9MB, holder 스파이크는 WS 3.0~3.2MB / Private 0.6~0.7MB였다. 데몬 메모리 예산에 conhost를 넣을지는 S-W3의 지표 정의 결정을 따른다.

## 6. ARCHITECTURE §4.1 Windows 열 문구 초안 (B안 기준)

| 장애 | Windows (M11) 초안 |
|---|---|
| 앱 크래시·종료·업데이트 | 산다(앱은 ConPTY 핸들을 쥐지 않음). 출력은 macOS와 같은 범위에서 무손실. **앱 kill 자체는 이 스파이크에서 측정하지 않았다 → W3-5 e2e로 확정** |
| krilld 크래시(첫 1회) | 산다. 새 krilld가 holder에게서 in·out·sig를 `DuplicateHandle`로 다시 받는다(OWE-826 Q2). 읽는 쪽이 없는 동안 conhost가 셸 쓰기를 막아 커널 파이프·conhost 버퍼 범위에서는 손실이 없다. krilld가 읽었지만 세그먼트에 쓰기 전인 바이트는 잃을 수 있다 → Gap 표시 |
| krilld 업그레이드 | 산다, 무손실. 리더 정지·세그먼트 봉인 → 새 krilld가 holder에게 핸들을 다시 받는다(Q2와 같은 경로). 읽지 않은 출력은 파이프와 conhost에 남는다 |
| holder 업그레이드 | 산다. exec 대신 **핸들 상속 핸드오프**를 쓴다. 새 holder에 in·out·sig·ref·conhost 프로세스·job·IOCP를 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 넘기고 `ConptyPackPseudoConsole`로 HPCON을 다시 묶는다(Q3, 3세대 실측). job IOCP는 다시 연결할 수 없어(오류 87) **IOCP 핸들 자체를 넘긴다** |
| holder만 크래시 | **krilld가 sig 핸들 사본을 쥐고 있을 때만** 산다(Q1, inbox·dll·cmd·pwsh 모두). 쥐지 않으면 15ms 안에 conhost·셸·자식이 모두 죽는다. 종료 코드와 holder가 없던 동안의 job 메시지는 잃는다. 새 holder 재결합(krilld → 새 holder로 핸들 복제)은 Q2·Q3을 합친 추론이며 단독 실측은 하지 않았다 |
| holder·krilld 동시 소실, 재부팅 | 죽는다(sig 핸들이 모두 닫혀 conhost 종료 → 셸 종료, Q1의 "in/out만" 행과 같은 원리). 마지막 세그먼트까지 읽기 전용 복원 |
| 잠자기 | 미확인(이 스파이크 범위 밖) |

## 7. 권장 holder 설계: **B(OpenConsole 번들) + kernel32 축소 모드**

| 기준 | A: kernel32 + "세션 0개일 때만 holder 업그레이드" | B: conpty.dll + OpenConsole 번들 |
|---|---|---|
| holder 교체 | 실측으로는 inbox도 되지만 문서에 없는 HPCON 배치에 기댐. 그래서 "세션 0개일 때만"으로 묶는 것이 안전함 | `ConptyPackPseudoConsole`(공개 헤더)로 됨 → 세션이 있어도 교체 가능 |
| holder 크래시 생존 | sig 복제로 됨(동일) | 같음 |
| 출력 충실도 | 재렌더 때문에 SGR 확장·OSC 8·OSC 133 위치·대체 화면·마우스 모드가 사라짐 | 바이트 그대로 |
| 질의 응답자 krilld 하나 | **깨짐**(DA1·DA2·CPR·DSR은 conhost가 답함) | 지켜짐 |
| 배포 비용 | 없음 | x64 약 1.2MB, MIT 고지, OpenConsole을 `x64\`에 둬야 함, 업데이트 추적 필요 |

권장: **B를 기본으로 한다.** 번들 파일이 없거나 실행에 실패하면 kernel32로 떨어지는 축소 모드를 두되, 그 모드에서는 (1) holder 업그레이드를 "세션 0개일 때만" 하고 (2) 질의 응답 예외와 충실도 저하를 진단(`krill doctor`)에 표시한다. 즉 A는 B의 대체 경로로만 남긴다.

### Phase 3에 주는 함의

- **W3-0(OWE-831) holder 프로토콜**: 핸들 값 메시지는 in·out·sig(+ 선택으로 ref·proc)를 담는다. krilld의 pid를 확인한 뒤 holder가 `OpenProcess(PROCESS_DUP_HANDLE)` → `DuplicateHandle`로 직접 넣고 값만 보낸다. krilld가 sig를 쥐면 resize는 krilld가 직접 할 수 있다. 그래도 Resize 메시지를 둘지는 단순성으로 정한다(holder 경유가 더 단순하다).
- **W3-1(OWE-832) holder**: Job은 `KILL_ON_JOB_CLOSE` 없이 만든다(켰을 때는 미측정). 셸은 `CREATE_SUSPENDED` → `AssignProcessToJobObject` → `ResumeThread` 순서로 띄운다. 업그레이드는 핸들 상속 핸드오프로 하고 IOCP도 넘긴다. 종료 코드가 필요하면 셸 프로세스 핸들도 넘긴다(job 메시지에는 pid만 있음, 스파이크에서는 넘기지 않음). 세션을 닫을 때는 `TerminateJobObject`만으로는 conhost가 남으므로 `ClosePseudoConsole`까지 부른다. `TerminateJobObject`는 프로세스별 `EXIT_PROCESS`를 보내지 않으니 `ACTIVE_PROCESS_ZERO`를 종료 신호로 쓴다.
- **W3-2 krilld**: 세션마다 블로킹 `ReadFile` 리더 스레드를 둔다(스파이크 방식). 세션이 256개일 때 스레드 수 문제와 overlapped named pipe 대안은 미측정이다. OpenConsole이 시작할 때 보내는 DA1에 krilld가 답한다(질의 응답자 krilld). `?9001h`·`?1004h`는 무시해도 입력이 동작했다.
- **holder 1,200줄 상한**: Windows 구현에 필요한 호출은 CreatePipe·ConPTY 생성·Job/IOCP·DuplicateHandle·핸드오프·Pack이다. 스파이크 `src/main.rs`에서 이 부분(69~380행, 파일 IPC·로그 포함)은 약 310줄이다. 실제 holder 줄 수는 추정이다.

## 8. 미확인 항목

| 항목 | 이유 |
|---|---|
| Windows 10/11 데스크톱 inbox conhost의 동작 | Server 2022(20348)만 측정. 최신 inbox conhost는 재렌더·자체 응답이 다를 수 있다 |
| 앱 kill, 잠자기, 로그오프 | 앱 없음, 재부팅·로그오프 테스트는 하지 않음 |
| `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`를 켠 job | 측정 안 함 |
| OpenConsole 시작 DA1에 답하지 않을 때 영향(출력 지연 외) | 출력 지연이 없다는 것만 확인 |
| TUI 전체 화면 재렌더 때 손실 | 스크롤 출력만 측정 |
| krilld 쪽 `ConptyPackPseudoConsole` 호출, holder 크래시 뒤 새 holder 재결합 단독 시나리오 | 같은 원리를 holder2에서만 실측 |
| `ConptyReleasePseudoConsole` 동작 | 측정 안 함 |
| OpenConsole을 conpty.dll과 같은 폴더에 둘 때 | `x64\` 배치만 측정 |

## 재현

```bash
# Mac
~/.cache/krill-win/gcp/up.sh
~/.cache/krill-win/spikes/OWE-826/scripts/push.sh            # 소스 전송(tar over ssh) + VM에서 cargo build --release
# 시나리오: 변수 앞붙임 + 스크립트 본문을 ./ssh.sh로 실행 (예: Q1, kernel32, krilld가 sig만 보유)
cd ~/.cache/krill-win/gcp && ./ssh.sh "\$api='inbox'; \$name='q1-x'; \$kopt='keep=sig'; $(cat ~/.cache/krill-win/spikes/OWE-826/scripts/q1.ps1)"
~/.cache/krill-win/spikes/OWE-826/scripts/pull.sh q1-x       # 결과를 /tmp/s826out 으로
```

VM 쪽 바이너리 배치: `C:\spike\OWE-826\bin\spike.exe`, `bin\conpty.dll`, `bin\x64\OpenConsole.exe`. 결과 폴더: `C:\spike\OWE-826\data\<시나리오>\`(holder.log·job.log·krilld-*.log·out-*.bin).
