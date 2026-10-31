# Krill 첫 실행

[English](GETTING_STARTED.md) · [README](README.ko.md)

## 경로 선택

릴리스 사용자는 Rust가 필요하지 않습니다. [설치 안내](README.ko.md#릴리스-설치)를 따라 Apple Silicon macOS 또는 Windows x64 프리뷰를 설치하고 아래 GUI·CLI 예제를 진행합니다. AI 에이전트는 별도로 설치·인증해야 합니다. 에이전트 없이도 셸 예제를 실행할 수 있습니다. 워크트리 예제에는 커밋이 하나 이상 있는 로컬 Git 저장소를 사용합니다.

## 첫 GUI 작업

1. macOS 응용 프로그램 또는 Windows 시작 메뉴에서 **Krill**을 엽니다.
2. **설정 → 모양 → 언어**에서 언어를 선택합니다. 저장한 선택은 유지됩니다.
3. 사이드바 **폴더 열기…**로 로컬 Git 저장소를 열고 해당 워크스페이스를 선택합니다.
4. **새 세션**(`+`)에서 **터미널**을 선택하고 `git status`로 작업 위치를 확인합니다. 같은 메뉴에서 설치한 에이전트를 선택해 저장소를 설명해 달라고 요청합니다.
5. 사이드바 **새 워크트리**에서 `first-review`처럼 고유한 이름을 넣고 **만들기**를 누릅니다. setup 훅은 검토 후 승인하거나 **setup 건너뛰기**를 선택합니다. 새 워크트리를 선택해 두 번째 터미널·에이전트를 열면 별도 작업 디렉터리·브랜치에서 실행됩니다.
6. 외부 터미널에서 `krill ls`로 ID를 기록합니다. macOS는 Command-Q, Windows는 **완전 종료**로 앱을 종료하고 다시 엽니다. 개별 세션 탭을 닫지 않았다면 실행 중인 같은 세션에 다시 연결됩니다. `krill ls`로 ID를 확인합니다.

세션 탭·칸을 명시적으로 닫으면 해당 세션이 종료됩니다. 앱 재시작 연속성은 세션 서비스가 살아 있을 때 적용되며 재부팅은 이전 에이전트 프로세스를 보존하지 않습니다. [장애별 보장](README.ko.md#장애별-보장)을 확인합니다.

## 첫 CLI 작업

GUI를 종료해도 명령 입력이 끊기지 않도록 외부 터미널을 사용합니다. 첫 세션 명령이 필요에 따라 로컬 서비스를 시작합니다.

### macOS 셸

저장소로 이동해 실행합니다.

```sh
krill --help
krill agent list
session_id=$(krill new --cwd "$PWD" -- /bin/sh)
krill send "$session_id" --text "printf 'hello from Krill\n'" --enter
krill wait "$session_id" --for pattern --pattern "hello from Krill" --timeout 10
krill read "$session_id" --lines 20
krill ls
```

`new`가 세션 ID를 출력하고 `read`에 인사말이 보이면 성공입니다. 셸 검사에는 에이전트 설치가 필요하지 않습니다.

### Windows PowerShell

PowerShell에서 저장소로 이동해 실행합니다.

```powershell
krill --help
krill agent list
$sessionId = krill new --cwd $PWD.Path -- powershell.exe -NoLogo -NoProfile
krill send $sessionId --text "Write-Output 'hello from Krill'" --enter
krill wait $sessionId --for pattern --pattern "hello from Krill" --timeout 10
krill read $sessionId --lines 20
krill ls
```

세션 ID와 인사말이 보이면 성공입니다. 네이티브 PowerShell 예제이므로 WSL이 필요하지 않습니다.

### 재연결·워크트리·에이전트

셸을 남겨 둔 채 GUI를 종료·재실행하고 `krill ls`와 같은 ID의 `read`로 확인합니다. `krill attach <세션_ID>`의 자리표시자를 실제 ID로 바꾸면 직접 조작할 수 있고 **Ctrl-]**로 세션 종료 없이 분리됩니다.

커밋이 있는 저장소에서 setup 실행 없이 새 워크트리를 만듭니다.

```sh
krill worktree new first-review --setup skip
krill worktree list
```

같은 이름이 이미 있으면 바꿉니다. 생성 결과의 경로·브랜치를 확인하고 출력된 경로로 이동해 설치·인증한 에이전트를 실행합니다.

```sh
krill new --agent claude --prompt "Describe this repository without changing files."
```

Codex는 `--agent codex`를 쓰고 다른 프로필은 `krill agent list`로 확인합니다. 실제 에이전트 계정으로 요청을 실행하는 명령입니다. 에이전트가 없다면 새 경로에서 셸 예제를 반복합니다.

예제 셸이 더 필요하지 않으면 해당 세션만 종료합니다.

```sh
# macOS: 위에서 만든 session_id 변수
krill kill "$session_id"
```

```powershell
# Windows: 위에서 만든 sessionId 변수
krill kill $sessionId
```

워크트리를 지울 의도가 있고 변경을 검토한 경우에만 `krill worktree rm <경로>`를 사용합니다. 재연결 확인에 워크트리 삭제는 필요하지 않습니다.

## macOS 소스 빌드

대상은 **Apple Silicon macOS**이며 Intel Mac 지원을 약속하지 않습니다. Git·Command Line Tools·Rust 1.98 이상을 설치합니다. 번들 조립에는 Python 3이 필요하고 로컬 실행에 전체 Xcode·Apple Developer 가입은 필요하지 않습니다.

공용 메인테이너 환경의 Cargo 산출물은 외장 빌드 볼륨에만 둡니다. 아래 예제 전에 `/Volumes/krill-build`가 마운트되어 쓰기 가능한지 확인하고 내장 SSD에 대체 target을 만들지 않습니다.

```sh
git clone https://github.com/craftowen/krill.git
cd krill
export CARGO_TARGET_DIR=/Volumes/krill-build/targets/krill-dev
cargo build --workspace --bins --locked
export PATH="$CARGO_TARGET_DIR/debug:$PATH"
cargo run -p krill-app --locked
```

GUI 전에 **워크스페이스 바이너리를 모두 빌드**해야 `krill`·`krill-holder`·`krilld`를 찾습니다. 위 PATH는 같은 터미널에서 CLI 예제도 실행하게 합니다. GUI 창이 뜨면 앞의 GUI 절차를 따릅니다. 이 개발 실행은 `Krill.app` 설치가 아닙니다.

로컬 릴리스 앱 번들을 만들려면 다음을 실행합니다.

```sh
packaging/bundle.sh
open packaging/dist/Krill.app
```

번들러는 locked release 빌드를 수행하며 Cargo 타깃 위치를 따릅니다. 기본 출력이 이미 존재하면 덮어쓰지 않으므로 반복 시 새 `--output <디렉터리>`를 지정합니다. 서명·출력 규칙은 [패키징](packaging/README.md)을 참고합니다.

별도 `bash packaging/install.sh --source-dir "$PWD"` 모드는 `~/.local/bin`에 **CLI·holder·데몬만** 빌드·설치하며 GUI 번들은 만들거나 설치하지 않습니다. 앱 번들을 설치하는 기본 릴리스 모드와 구분합니다. Windows 소스 패키징은 [Windows 안내](packaging/windows/README.md)에 있으며 첫 사용은 릴리스 ZIP 경로가 간단합니다.

## 문제 해결

- **CLI가 없음:** macOS 릴리스는 `~/.local/bin`, 개발 실행은 지정한 Cargo debug 경로를 PATH에 넣습니다. Windows는 설치 뒤 새 터미널을 엽니다.
- **에이전트 감지 실패:** `krill agent list`를 확인하고 별도 설치·인증합니다. PATH가 바뀌면 앱을 다시 엽니다.
- **소스 GUI 서비스 기동 실패:** 워크스페이스 바이너리를 먼저 모두 빌드하고 버전이 맞는 바이너리를 함께 둡니다.
- **설치·연결 문제:** `krill doctor`로 확인합니다. `krill doctor --bundle` 공유 전 민감정보를 직접 검토·삭제합니다. Krill 버전·커밋·OS·아키텍처·명령·실제 현상을 [버그 양식](.github/ISSUE_TEMPLATE/bug_report.md)에 적습니다.
- **보안 문제 의심:** [보안 정책](SECURITY.ko.md)을 따르고 공개 버그에 상세를 적지 않습니다.
