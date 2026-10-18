# OWE-828 — S-W3: gpui 앱 렌더·한글 IME·메모리 지표 (Windows 실기)

측정일: 2026-10-18. 빌드는 공유 GCP `krill-win`(Windows Server 2022, Rust 1.98.1 `x86_64-pc-windows-msvc`), 실행·측정은 실기 TP PC(Windows 11 Pro 10.0.26200, RAM 8GB, Intel UHD Graphics 730, 1920×1080, 96 DPI(100%), 입력기는 Microsoft 한국어 IME 하나)에서 했다. 소스는 `8ce49de5`(OWE-824 통합 수정 병합)이다.

**결론: `krill-app`은 코드 수정 없이 msvc 릴리스로 빌드되고, 데몬 없이도 Windows에서 창을 띄워 한글 UI를 정상 렌더한다. 메인 창 1개 유휴 시 PrivateUsage(커밋)는 69.5MB, Private Working Set은 33.5MB다. 한글 IME 10케이스는 하나도 자동 검증하지 못했다.** TP PC 사용자 세션이 '연결 끊김' 상태라 포그라운드 전환과 합성 키 입력이 불가능했고, 코디네이터 결정에 따라 세션을 콘솔에 붙이지 않았다(아래 §4). IME 10케이스는 모두 '사람 확인 필요'로 남긴다.

## 1. 결과표

| 항목 | 결과 | 근거 |
|---|---|---|
| `cargo build --release -p krill-app --locked` (VM, msvc) | 성공. 경고 0, 20분 38초(콜드), exe 42,414,080B | `C:\spike\OWE-828\build.log`. VM과 Mac 사본 SHA-256 일치(`147eaa15…5fc0a`) |
| exe 동적 의존 | 시스템 DLL 외에 **`VCRUNTIME140.dll`**과 UCRT(`api-ms-win-crt-*`). 별도 DLL을 함께 배포할 필요는 없었다 | `dumpbin /dependents`. TP PC에는 VC++ 런타임이 이미 깔려 있어 바로 실행됐다 |
| gpui 텍스트 입력 최소 예제(`ime-input`) 빌드 | 성공. 경고 0, 24분 33초(앱 빌드 뒤 같은 target 디렉터리), exe 10,498,560B | `C:\spike\OWE-828\build-ime.log` |
| 앱 실행(데몬 없음, 끊긴 세션) | 성공. 패닉 없음, stdout/stderr 빈 파일 | 로그 `app starting` → `menu bar unavailable; keeping Dock access when windows close`(WARN) |
| D3D 장치 | Intel GPU로 생성된 것으로 보임 | 격리한 `USERPROFILE` 아래 `LocalLow\Intel\ShaderCache`가 생겼다(WARP였다면 생기지 않을 것으로 추정) |
| 첫 실행 화면 렌더 | 정상. 동의 화면, 한글 폰트 폴백, 체크박스, 버튼, 스피너 | `shots/app1-window.png`(696×628, 제목 `Krill 시작하기`) |
| 메인 창 렌더(동의 기록을 미리 넣음) | 정상. 사이드바, 탭, 데몬 오류 배너, 변경 사항 패널, 상태줄 | `shots/app2-window.png`(1216×768, 제목 `Krill`) |
| 데몬 미연결 표시 | 배너 `cannot talk to krilld: … (os error 10050) — 다시 시도하는 중`, 상태줄 `데몬 오류` | `app2-window.png`. 10050 = `WSAENETDOWN`(AF_UNIX 소켓 파일이 없을 때) |
| 창 테두리 | **메인 창에 Windows 최소화·최대화·닫기 버튼이 없다**(자체 제목 표시줄). 기본 `WindowOptions`인 `ime-input`에는 OS 버튼이 있다 | `app2-window.png`와 `ime1-window.png` 비교 |
| 재그리기 | **미확인.** WM_CHAR로 넣은 `한글`이 모델에는 들어갔지만(`ime.log`) PrintWindow 캡처는 이전 프레임(placeholder)을 보였다. 끊긴 세션에서 gpui가 present를 멈춘 것인지 PrintWindow가 낡은 표면을 돌려준 것인지 구분하지 못했다 | `shots/ime2-window.png`, `tp-results/ime.log` |
| 확정 문자 경로(IME 미경유) | 통과. `PostMessage(WM_CHAR, '한')`, `'글'` → `replace_text_in_range(None, "한")`, `("글")` → 내용 `한글`, marked 없음 | `tp-results/ime.log` |
| 합성 키(`keybd_event`) | 불가. `SetForegroundWindow=False`, 포그라운드 창 0, 키 이벤트가 앱에 도달하지 않음 | `tp-results/keys.log` |
| 전체 화면 캡처(`CopyFromScreen`) | 불가. "핸들이 잘못되었습니다" | `tp-results/app*.txt`. 창 단위 `PrintWindow(PW_RENDERFULLCONTENT)`는 동작했다 |
| 한글 IME 10케이스 | **0/10 자동 검증. 전부 사람 확인 필요**(§3) | — |
| 유휴 CPU(메인 창, 데몬 없음) | 60초 동안 CPU 1.55초(1코어 기준 약 2.6%) | 원인 미확인. 재연결 재시도인지 재그리기인지 확인하지 않았다 |
| 로그 회전 파일 | 첫 시작 시 0바이트 `krill-app.log.1`~`.4`가 함께 생겼다 | 원인 미확인(Windows 전용 동작인지 보지 않았다) |

## 2. 메모리 (1회 측정, 5초 간격 표본)

측정 도구는 `Get-Process`(`WorkingSet64`·`PrivateMemorySize64`·`PeakWorkingSet64`)와 `Win32_PerfFormattedData_PerfProc_Process.WorkingSetPrivate`다. `PrivateMemorySize64`는 PROCESS_MEMORY_COUNTERS_EX의 **PrivateUsage(커밋)**와 같은 값이다. 성능 카운터 경로가 ko-KR로 지역화돼 있어 `Get-Counter`는 쓰지 않았다. `mem.ps1`의 CSV 출력은 파일로 남기지 않고 아래 표에 범위로 옮겼다.

| 상태 | WS | **Private WS** | **PrivateUsage(커밋)** | Peak WS | 핸들 | 스레드 |
|---|---|---|---|---|---|---|
| 첫 실행 동의 창 1개(에이전트 감지 중), 표본 6개 | 93.4–93.6MB | 42.4–42.7MB | 88.3–88.5MB | 122.1MB | 450–456 | 19–20 |
| 메인 창 1개 유휴(데몬 없음, 터미널 0), 표본 13개(60초) | 75.5–75.7MB | 33.4–33.6MB | 69.4–69.6MB | 81.2MB | 424–430 | 16–19 |
| 참고: `ime-input` 예제 창 1개, 표본 3개 | 59.8MB | 26.7MB | 46.7MB | 60.8MB | 366 | 9 |

- 표본 사이 편차는 0.3MB 이하였다. 측정은 각 1회 실행뿐이라 실행 간 편차는 미확인이다.
- 데몬·터미널이 없는 상태라 macOS 예산 시나리오(터미널 10개)와 직접 비교할 수 없다. 앱 ≤ 120MB 판정은 미확인이다.
- GPU 메모리(공유·전용 GPU 메모리)가 PrivateUsage에 얼마나 잡히는지는 측정하지 않았다(미확인).

### 앱 120MB 예산의 Windows 대응 기준 (제안)

**게이트 지표는 PrivateUsage(커밋)로 하고, Private Working Set은 보조 지표로 함께 기록한다.**

- macOS 기준인 `phys_footprint`는 압축되거나 스왑된 것까지 포함한 '이 프로세스 몫의 더러운 메모리'다. Windows에서 그 성격에 가장 가까운 것은 PrivateUsage다. 커밋된 사유 메모리는 작업 집합 트리밍이나 페이지 아웃이 일어나도 줄지 않는다.
- Private WS는 OS가 트리밍하면 떨어진다(이번 측정에서도 커밋의 약 48%만 상주). 그래서 게이트로 쓰면 메모리 압박이 클수록 오히려 합격하기 쉬워진다. 사용자가 작업 관리자 '메모리' 열에서 보는 값이 이것이므로 기록용으로는 남긴다.
- WS는 공유 DLL 페이지까지 포함하므로 쓰지 않는다.
- 같은 120MB를 PrivateUsage에 적용하면 이번 빈 메인 창이 69.5MB라 여유는 약 50MB다. 터미널 10개 시나리오 측정은 Windows krilld·holder(OWE-832/833) 뒤 MK-009 하네스 Windows판에서 한다. 이 수치로 예산을 바꾸지는 않는다.

## 3. 사람 확인 필요 항목 — 한글 IME 10케이스 (MK-002 S1 대응, Windows판)

자동 검증한 것은 IME를 거치지 않는 확정 문자 경로(WM_CHAR → `replace_text_in_range`) 하나뿐이다. 아래 10개는 모두 **사람 확인 필요**다. 실행기 `ime-input.exe`는 gpui-pre 0.3.9의 `examples/input.rs`(Apache-2.0)에 IME 콜백 로그를 붙인 것이다. `replace_and_mark_text_in_range`(조합)·`replace_text_in_range`(확정)·`unmark_text`·백스페이스·마우스 다운·keystroke를 exe 옆 `ime.log`에 남기므로, 사람은 키만 누르고 판정은 로그와 화면으로 한다.

| # | 케이스 | 판정 기준(로그 + 화면) | 상태 |
|---|---|---|---|
| 1 | 조합 중 표시: `ㅎ`→`하`→`한` | `mark_text`가 매 자모마다 오고 preedit 밑줄이 보임 | 사람 확인 필요 |
| 2 | 확정: "한글 " 입력 | `replace_text "한"`·`"글"` 뒤 공백 1개, 중복·유실 없음 | 사람 확인 필요 |
| 3 | 조합 중 Enter: "안녕하세요⏎" | `replace_text "요"`가 Enter keystroke보다 먼저 옴(순서 뒤바뀜 없음) | 사람 확인 필요 |
| 4 | 조합 중 백스페이스: `한` 조합 중 BS | 자모 단위로 지워짐(`mark_text "하"`), 확정 글자를 지우지 않음 | 사람 확인 필요 |
| 5 | 조합 중 텍스트 안 마우스 클릭 | `mouse_down` 전후로 조합이 확정(`unmark_text` 또는 `replace_text`)되고 유실 없음 | 사람 확인 필요 |
| 6 | 조합 중 포커스 이탈(Alt+Tab, 다른 창 클릭) 및 창 닫기 | 조합 글자 확정·유실 없음, 닫을 때 패닉 없음(stdout 파일 확인) | 사람 확인 필요 |
| 7 | 한/영 전환 직후 첫 자모(한/영 키 또는 오른쪽 Alt) | 전환 직후 첫 자모가 분리되거나 영문으로 새지 않음 | 사람 확인 필요 |
| 8 | 한글 모드에서 스페이스·첫 특수문자 | 스페이스 1회 = 공백 1개, `한`+`.`에서 `.` 유실 없음 | 사람 확인 필요 |
| 9 | 한자 키 후보창 위치 | 후보창이 커서 근처에 뜸(`bounds_for_range`). 결과만 기록 | 사람 확인 필요 |
| 10 | 이모지 패널(Win+.)·음성 입력(Win+H) | 삽입된 문자가 `replace_text`로 들어옴 | 사람 확인 필요 |

참고: 예제의 복사·붙여넣기 바인딩은 `cmd-*`라 Windows에서 Ctrl+C/V 케이스는 이 실행기로 볼 수 없다. Krill 터미널의 조합 중 Ctrl+C/V/T와 Claude Code 입력창 케이스(S1 ⑦⑧)는 Windows 데몬(OWE-832/833)이 생긴 뒤 앱으로 확인한다.

### 재현 절차(사람, RDP 또는 PC 앞)

1. exe 위치: Mac `~/.cache/krill-win/spikes/OWE-828/out/{krill-app.exe,ime-input.exe}`, VM `C:\spike\OWE-828\target\release\`. 실행기 스크립트는 `~/.cache/krill-win/spikes/OWE-828/tp/`(`run.cmd`, `capture.ps1`, `mem.ps1`, `keys.ps1`, `wmchar.ps1`)에 있다.
2. TP PC에 `C:\Users\user\krill-spike\OWE-828\`를 만들고 exe와 `run.cmd`를 복사한다(`scp out/*.exe tp/run.cmd tp-pc:C:/Users/user/krill-spike/OWE-828/`).
3. 사람이 RDP로 로그온한 데스크톱에서 `C:\Users\user\krill-spike\OWE-828\run.cmd ime-input.exe`를 실행한다. `run.cmd`는 `HOME`·`USERPROFILE`·`LOCALAPPDATA`·`APPDATA`·`KRILL_STATE_DIR`·`KRILL_LOG_DIR`·`CLAUDE_CONFIG_DIR`·`CODEX_HOME`을 모두 이 폴더 아래 `home\`으로 돌리고 업데이트 확인을 끈다.
4. 위 10케이스를 차례로 입력하며 Win+Shift+S로 캡처한다. 끝나면 `ime.log`와 캡처를 가져온다.
5. 같은 방법으로 `run.cmd krill-app.exe`를 실행해 활성 세션에서 렌더·재그리기·유휴 CPU를 다시 본다. 첫 화면을 건너뛰려면 `home\state\consent.toml`에 `version = 1`·`accepted_at = "2026-10-18T10:00:00Z"`·`app_version = "0.1.5"`를 넣는다.
6. 정리: `taskkill /IM krill-app.exe /F`, `taskkill /IM ime-input.exe /F`, `rmdir /s /q C:\Users\user\krill-spike`.

## 4. 끊긴 세션의 제약과 선택지 A

- 측정 시점 `query user`: user 세션 ID 1, 상태 **디스크(연결 끊김)**, 유휴 5시간 33분~6시간. 콘솔에 붙은 활성 세션이 없어 사람이 쓰는 중은 아니었다.
- GUI는 `schtasks /Create /IT /RU user /SC ONCE` 후 `/Run`으로 세션 1에 띄웠다. 창 생성과 첫 프레임 렌더, 메모리 측정, WM_CHAR 전달은 됐다. 포그라운드 전환, 합성 키, 전체 화면 캡처는 되지 않았다.
- 코디네이터 결정(B안): 운영 PC 물리 모니터에 잠금 해제 데스크톱을 노출하는 `tscon 1 /dest:console`(A안)은 사용자 확인 없이 하지 않았다.
- **A안이 필요한 이유**: 사람 없이 IME 10케이스를 자동화하려면 입력 데스크톱이 활성인 세션이 있어야 한다(합성 키·한/영 전환·포커스 이동). A안(SYSTEM 예약 작업으로 `tscon` 실행 → 측정 → `LockWorkStation`)이나 사람의 RDP 접속(§3 절차)이 필요하다. 어느 쪽이든 IME 케이스는 키 입력만 자동화되고, 후보창 위치·밑줄 같은 화면 판정은 캡처를 사람이 봐야 한다.

## 5. OWE-834 설계에 주는 함의

1. **gpui Windows 백엔드는 그대로 쓸 수 있다.** `platform/windows.rs` no-op 상태로도 앱 창·한글 폰트 폴백·DirectX 렌더가 동작했다. OWE-834는 렌더 계층보다 플랫폼 통합(창 테두리·단축키·열기·트레이)에 집중하면 된다.
2. **창 테두리**: 메인 창이 macOS식 투명 제목 표시줄 설정을 따라 Windows 최소화·최대화·닫기 버튼 없이 뜬다. Windows에서는 OS 테두리를 쓰거나, gpui 자체 그리기 테두리라면 캡션 버튼과 드래그·더블클릭 최대화·스냅 레이아웃(최대화 버튼 hover) 히트 테스트를 구현해야 한다.
3. **문구**: 첫 실행 화면에 "이 Mac에만 저장"이 그대로 나온다. 플랫폼 이름을 문자열에 넣지 않거나 플랫폼별 문자열로 바꾼다(i18n).
4. **메뉴 막대·Dock**: `menu bar unavailable; keeping Dock access` 경고가 난다. Windows에서는 창을 닫았을 때 앱에 다시 들어갈 경로(트레이 아이콘 또는 마지막 창 닫으면 종료)를 정해야 한다.
5. **데몬 미기동 오류 매핑**(OWE-832/833과 공동): AF_UNIX 소켓 파일이 없을 때 Windows는 `ENOENT`가 아니라 `WSAENETDOWN(10050)`을 준다. 연결 실패를 'krilld 미기동 → 기동 시도'로 분류하는 코드는 이 코드를 같은 부류로 다뤄야 한다. 사용자 배너에 원시 OS 오류 문구가 그대로 나오는 것도 정리 대상이다.
6. **CRT 배포**: exe가 `VCRUNTIME140.dll`에 동적으로 링크된다. 런타임이 없는 PC를 위해 `+crt-static`을 쓰거나 VC++ 재배포 패키지를 설치기에 넣는다(Phase 4 패키징 결정).
7. **에이전트 감지가 실제 CLI를 실행한다**: TP PC에 설치된 `claude.exe`가 첫 실행 감지 중 `CLAUDE_CONFIG_DIR` 아래에 `.claude.json`·`daemon\pipe.key`를 만들었다(격리 폴더 안). 실제 사용자 `C:\Users\user\.claude*`의 수정 시각은 측정 전 그대로였다. Windows QA를 운영 PC에서 할 때는 이번처럼 설정 디렉터리 환경변수를 격리한다.
8. **메모리 게이트**: Windows는 PrivateUsage ≤ 120MB를 게이트로, Private WS를 보조 기록으로 한다(§2). 측정 하네스는 지역화된 카운터 이름을 피해 `GetProcessMemoryInfo`나 CIM을 쓴다.
9. **유휴 CPU**: 데몬 없는 메인 창이 60초에 1.55초 CPU를 썼다. '유휴 프레임 0'(MK-013) 기준으로 재연결 재시도나 스피너 재그리기를 Windows에서 다시 확인한다(활성 세션에서, 미확인).
10. **IME**: 렌더러와 확정 경로는 동작한다. 조합(`WM_IME_COMPOSITION` → `replace_and_mark_text_in_range`)·후보창 위치·포커스 이탈 확정은 미확인이므로, OWE-834 착수 전이나 진행 중에 §3 절차로 사람 확인을 받는다.

## 6. 재현 명령

```sh
# 소스 전송(VM sftp가 동작하지 않아 ssh stdin + Git의 cat.exe로 받는다)
git bundle create ~/.cache/krill-win/spikes/OWE-828/bundle/krill.bundle HEAD
~/.cache/krill-win/spikes/OWE-828/put.sh ~/.cache/krill-win/spikes/OWE-828/bundle/krill.bundle 'C:\spike\OWE-828\krill.bundle'
~/.cache/krill-win/gcp/ssh.sh 'cd C:\spike\OWE-828; git clone -q krill.bundle src'
# 빌드(VM)
~/.cache/krill-win/gcp/ssh.sh 'cd C:\spike\OWE-828\src; $env:CARGO_TARGET_DIR="C:\spike\OWE-828\target"; cargo build --release -p krill-app --locked'
~/.cache/krill-win/gcp/ssh.sh 'cd C:\spike\OWE-828\ime-input; $env:CARGO_TARGET_DIR="C:\spike\OWE-828\target"; cargo build --release'
# 가져오기
~/.cache/krill-win/spikes/OWE-828/get.sh 'C:\spike\OWE-828\target\release\krill-app.exe' ~/.cache/krill-win/spikes/OWE-828/out/krill-app.exe
# TP PC 실행·캡처·메모리(로그온 사용자 세션)
ssh tp-pc 'schtasks /Create /TN krill-OWE828-app /TR "C:\Users\user\krill-spike\OWE-828\run.cmd krill-app.exe" /SC ONCE /ST 23:59 /IT /RU user /F && schtasks /Run /TN krill-OWE828-app'
ssh tp-pc 'powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\user\krill-spike\OWE-828\mem.ps1 -ProcName krill-app -Samples 13 -IntervalSec 5'
```

스파이크 코드·스크립트·원시 로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-828/`(`ime-input/`, `tp/`, `tp-results/`, `out/`), 캡처 이미지는 `~/.cache/krill-win/spikes/OWE-828/shots/`(`app1-window.png`, `app2-window.png`, `ime1-window.png`, `ime2-window.png`)에 있다. VM 작업 디렉터리 `C:\spike\OWE-828\`(소스·target)는 OWE-834 재사용을 위해 남겼다.

**TP PC 정리**: 앱·예제 프로세스 종료, 예약 작업 4개(`krill-OWE828-app`·`-cap`·`-ime`·`-keys`) 삭제, `C:\Users\user\krill-spike` 삭제. 이어서 `dir C:\Users\user\krill-spike`(파일 없음), `schtasks /Query | findstr /i krill`(0건), `tasklist | findstr /i "krill ime-input"`(0건)으로 확인했다. TP PC에는 설치나 시스템 설정 변경을 하지 않았다(레지스트리·GPU 정보는 읽기만 했다).
