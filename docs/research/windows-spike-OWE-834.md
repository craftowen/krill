# OWE-834 — 앱 Windows 구현 검증 (platform·창 테두리·단축키·글꼴·문구·종료)

측정일: 2026-10-18. 소스는 `d682ab55`(OWE-834 커밋 7개)이다. 빌드는 공유 GCP `krill-win`(Windows Server 2022, Rust 1.98.1 `x86_64-pc-windows-msvc`), 실행·측정은 실기 TP PC(Windows 11 Pro 10.0.26200, OWE-828과 같은 PC)에서 했다.

**결론: 변경한 앱은 msvc 릴리스로 경고 없이 빌드된다. Windows 메인 창에 OS 최소화·최대화·닫기 버튼이 생겼고, 마지막 창을 닫으면 프로세스가 끝난다. 첫 실행 문구는 "이 컴퓨터에만 저장"으로 나오고 메뉴바 경고는 사라졌다.** IME 조합 취소·경고음·잠자기 방지·열기(ShellExecuteW)는 컴파일과 정적 검사까지만 했다. 실제 동작은 활성 데스크톱이 없어 확인하지 못했다(미확인). VM 창 캡처는 VM에 대화형 세션이 없어 생략했고 TP PC 실기 캡처로 대신했다(코디네이터 결정).

## 1. 결과표

| 항목 | 결과 | 근거 |
|---|---|---|
| Mac `cargo clippy -p krill-app --all-targets --locked -- -D warnings` | 0 | 로컬 |
| Mac `cargo nextest run -p krill-app --locked --no-fail-fast` | 708/708 통과(2 skipped) | 로컬. 첫 실행 때 `orca_import` 2건이 실패했다. `target/debug/krill` CLI가 빌드되지 않은 target 디렉터리라서였고, `cargo build --workspace --bins` 뒤에는 통과했다 |
| `scripts/check-windows.sh -p krill-app --clippy` | 0 | 경고는 공유 `clippy.toml`의 `std::os::fd::RawFd` 설정 경고 1건뿐이다(코드 무관) |
| VM `cargo build --release -p krill-app --locked` | 성공. 경고 0, 13분 52초(OWE-828 target 재사용), exe 42,435,584B | `C:\spike\OWE-834\build.log`. VM 사본과 Mac 사본의 SHA-256이 일치한다(`43e80466…217ab`) |
| VM 창 캡처 | **생략.** S4U 예약 작업(세션 0)으로 띄우면 gpui가 `window creation failed: cannot open windows while headless`를 낸다. 자동 로그온과 재부팅이 필요해 코디네이터 결정으로 하지 않았다 | `C:\spike\OWE-834\run\vm2.stderr.txt` |
| 실기 첫 실행 창 | 정상. OS 제목 표시줄에 제목 `Krill 시작하기`와 닫기 버튼이 있다. 크기 고정 창이라 최소화·최대화 버튼은 없다. 앱 제목 띠가 중복으로 그려지지 않는다. 문구는 "이 컴퓨터에만 저장되고" | `shots/app1-window.png`(696×659. OWE-828은 696×628) |
| 실기 메인 창 | 정상. **최소화·최대화·닫기 버튼이 있다.** 사이드바 머리 왼쪽 신호등 여백(78px)이 없다 | `shots/app2-window.png`(1216×799. OWE-828은 1216×768) |
| 마지막 창 닫기 | 메인 창에 `WM_CLOSE`(캡션 닫기와 같은 메시지, 키 입력 아님)를 보내자 10초 안에 프로세스가 끝났다 | `tp-results/close.txt` `post=True exited=True` |
| 메뉴바 경고 | 사라짐. 로그는 `app starting` 한 줄뿐이다 | `tp-results/krill-app.log` |
| 패닉 | 없음. stdout/stderr가 빈 파일이다 | `tp-results/krill-app.stdout.txt` 0B |
| 메모리(메인 창 유휴, 데몬 없음, 표본 3개) | PrivateUsage 69.6–69.7MB, Private WS 33.6–33.7MB, WS 75.5–75.7MB | `mem.ps1`. OWE-828(69.4–69.6MB)과 차이가 없다 |
| 유휴 CPU(메인 창, 데몬 없음) | 60초에 1.41초(1코어 기준 2.3%). OWE-828은 2.6% | 원인은 미확인(§3) |
| 캡션 드래그·더블클릭 최대화·스냅 레이아웃 | 미확인(합성 입력 금지). OS 기본 제목 표시줄이라 OS가 처리한다 | — |
| 제목 표시줄 색 | 밝은 OS 기본색이라 앱의 다크 테마와 어긋난다 | `app2-window.png`. 후속(§4) |
| IME 조합 취소(`ImmNotifyIME`)·`MessageBeep`·`SetThreadExecutionState`·`ShellExecuteW` 열기 | 컴파일과 Windows clippy만 했다. 실행은 미확인 | 활성 데스크톱과 키 입력이 필요하다 |
| 한글 IME 10케이스 | **사람 확인 필요**(OWE-828 §3 절차 그대로) | — |

## 2. 변경 요약(커밋)

1. `36bc7a86` platform 실구현: `platform/windows.rs`의 `cfg(windows)` `win` 모듈에만 unsafe를 두고, 모든 unsafe에 SAFETY 주석을 달았다. 공유 `shell_open`(ShellExecuteW "open")을 `permissions::open_url`, 계정 로그인 URL, `open_in`(탐색기, `vscode://file/…`·`cursor://`·`zed://`)이 함께 쓴다. 파일 "기본 앱으로 열기"·"보기"는 `explorer.exe`(`/select,"…"`)로 연다. Windows 실행 파일은 실행 비트가 없어 확장자(exe·bat·ps1·lnk 등)로 판정하고, 실행하는 대신 보기로 연다. diff 탐색기에 따로 있던 열기 구현은 `Target::open_path` 하나로 합쳤다. 작업 표시줄 배지는 후속(§4)이다.
2. `bab6bbf7` `layout::sync_parent`: 레이아웃·사이드바·편집기 저장이 모두 이 공유 함수를 거친다. Windows에서는 이 한 곳에서 디렉터리 fsync를 건너뛴다(os error 5).
3. `a7a20674` `ui::titlebar`: macOS는 기존처럼 투명 제목 표시줄과 신호등을 쓴다. 그 밖의 OS는 `appears_transparent=false`로 OS 제목 표시줄을 쓴다. 신호등 여백(78px)은 macOS에서만 둔다. 온보딩 창의 제목 띠는 macOS에서만 그린다.
4. `cb2f06fb` `FONT_MONO`: Windows는 Consolas를 쓴다. Cascadia Mono는 Windows 10에 없을 수 있는데, 없는 이름이면 gpui가 비고정폭 `fallback_font_stack`으로 떨어진다. 한글은 DirectWrite 시스템 폴백이 채운다(OWE-828 실측 렌더).
5. `bffc016a` `QuitMode::Default`: macOS는 Explicit, 그 밖의 OS는 LastWindowClosed다. 메뉴바 항목이 없으면 100ms 명령 폴링 태스크를 띄우지 않는다. 메뉴바 경고는 macOS에서만 남긴다.
6. `b9b95d46` 단축키: `keys::bind`는 macOS에서 원문 그대로 바인딩한다. 그 밖의 OS에서는 `keys::windows_keys`로 바꾼다. 규칙은 아래와 같다.
   - `cmd-글자`와 `cmd-[ ] \ /`는 `ctrl-shift-…`로 바꾼다. Ctrl+글자는 셸 제어 문자(Ctrl+C SIGINT, Ctrl+D EOF, Ctrl+W, Ctrl+[ = Esc)라서다. Windows Terminal 관례와 같고, 복사·붙여넣기는 Ctrl+Shift+C/V가 된다.
   - `cmd-shift-글자`는 `ctrl-alt-shift-…`로 바꿔 `cmd-글자`와 겹치지 않게 한다.
   - `cmd-alt-글자`는 `ctrl-alt-…`로 바꾼다.
   - 숫자·`,` `=` `-`·화살표는 `ctrl-…`로만 바꾼다.
   - 브리프는 `secondary-`(= Windows Ctrl)로 일괄 치환하라고 했지만, 그러면 터미널 포커스에서 Ctrl+D·W·K·A·L·[ 같은 셸 키를 앱이 가로챈다. gpui는 바인딩을 키 리스너보다 먼저 매칭한다(`vendor/gpui-pre/src/window.rs` `dispatch_key_event`). 그래서 위 규칙을 택했다.
7. `d682ab55` 문구: `Message::mac_or(other)`로 처리한다. macOS는 원문 그대로 쓴다. 그 밖의 OS는 카탈로그 `other_os`(이 컴퓨터, 파일 탐색기에서 보기, 배지 없는 알림 설명, OS 설정, 컴퓨터 잠자기)를 쓴다.

## 3. 유휴 CPU (§5-9)

- 데몬이 없을 때 앱이 계속 깨어 있는 주기: 메뉴바 상태 폴링 500ms(`menu_bar.rs`), 사이드바 폴링 500ms 2개(`sidebar.rs:998`·`:1492`), 셸 1초(`shell.rs:1220`), 설정 파일 감시(`settings.rs:509`), 리소스·diff 감시 등. 이번에 메뉴바 항목이 없을 때의 100ms 명령 폴링은 없앴다.
- 2.6%→2.3%로 줄었지만 측정이 각 1회뿐이라 실행 간 편차인지 구분하지 못한다. 재연결 재시도(`retry_delay` 최대 500ms) 때문인지, 폴링 뒤 `cx.notify` 재그리기 때문인지, 끊긴 세션의 present 동작 때문인지도 가르지 못했다(**미확인**). 활성 데스크톱에서 ETW(`wpr -start CPU`)나 프레임 카운터로 다시 본다.

## 4. 후속(이번 범위 밖, 근거)

- **단축키 표시 문자열**: i18n 툴팁 21개와 `shell/tabs.rs`의 `"⌘W"`가 macOS 기호로 하드코딩돼 있어 Windows에서 틀린 키를 보여준다. 바인딩에서 표시 문자열을 만드는 방식(gpui `KeyBinding` 표시)으로 바꿔야 한다. 사용자 `keys.toml`의 `cmd-` 재정의도 Windows에서는 변환하지 않는다.
- **작업 표시줄 배지**: `ITaskbarList3::SetOverlayIcon`은 COM 인터페이스(`windows` 크레이트)와 숫자를 그린 HICON 생성이 필요하다. `windows-sys`는 COM vtable을 주지 않는다.
- **트레이 아이콘**: 지금은 마지막 창을 닫으면 종료한다. 창 없이 알림을 계속 받으려면 `Shell_NotifyIconW` 트레이가 필요하다.
- **제목 표시줄 다크 모드**: `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)`로 테마에 맞출 수 있다. gpui 창 생성 뒤 HWND에 적용한다.
- **링크(P2P) 화면의 "Mac" 문구**: i18n.rs에서 "Mac"이 들어간 줄이 1,870~1,960행에만 22개다. krill-link Windows 지원 범위가 정해진 뒤 처리한다.
- **Cascadia Mono 우선**: 설치돼 있으면 쓰려면 `all_font_names()` 조회 뒤 기본값을 고르는 방식이 필요하다.
- **`draw_now`**: Windows에서는 no-op(gpui 다음 프레임)이다. MK-013 지연 기준은 Windows에서 측정하지 않았다.

## 5. 재현 명령

```sh
# Mac
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-834 cargo build --locked --workspace --bins
CARGO_TARGET_DIR=/Volumes/krill-build/targets/win/OWE-834 cargo nextest run -p krill-app --locked --no-fail-fast
scripts/check-windows.sh -p krill-app --clippy --target-dir /Volumes/krill-build/targets/win/OWE-834-windows
# VM: OWE-828 클론에서 증분 번들로 체크아웃 뒤 빌드
git bundle create ~/.cache/krill-win/spikes/OWE-834/bundle/krill.bundle 8ce49de5..HEAD
~/.cache/krill-win/spikes/OWE-828/put.sh ~/.cache/krill-win/spikes/OWE-834/bundle/krill.bundle 'C:\spike\OWE-834\krill.bundle'
~/.cache/krill-win/gcp/ssh.sh 'cd C:\spike\OWE-834; git clone -q C:\spike\OWE-828\src src; cd src; git fetch -q ..\krill.bundle HEAD; git checkout -q FETCH_HEAD'
~/.cache/krill-win/gcp/ssh.sh 'cd C:\spike\OWE-834\src; $env:CARGO_TARGET_DIR="C:\spike\OWE-828\target"; cargo build --release -p krill-app --locked'
# TP PC: OWE-828 §3과 같다(run.cmd로 설정 디렉터리 격리, schtasks /IT /RU user, capture.ps1 PrintWindow, mem.ps1, cpu.ps1, close.ps1)
```

스크립트·원시 결과·캡처는 저장소 밖 `~/.cache/krill-win/spikes/OWE-834/`(`tp/`, `tp-results/`, `shots/app1-window.png`, `shots/app2-window.png`, `vm-run.ps1`, `stage_hunks.py`)에 있다.

**TP PC 정리**: 앱 종료, 예약 작업 3개(`krill-OWE834-app`·`-cap`·`-close`) 삭제, `C:\Users\user\krill-spike` 삭제를 했다. 그 뒤 `dir C:\Users\user\krill-spike`(파일 없음), `schtasks /Query | findstr /i krill`(0건), `tasklist | findstr /i krill`(0건)으로 확인했다. 설치나 설정 변경은 없었다. **VM**: 예약 작업 `krill-OWE834-vm`은 삭제했다. `C:\spike\OWE-834\`(소스·실행 사본)는 남겼고, release 산출물은 `C:\spike\OWE-828\target`에 있다. VM은 정지하지 않았다(코디네이터 몫).
