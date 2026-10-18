# OWE-830 — S-W5: npm .cmd 실행·인자·기본 셸

측정일: 2026-10-18. 공유 GCP `krill-win`, Windows Server 2022 Datacenter 10.0.20348, Rust 1.98.1 x86_64-pc-windows-msvc. **Windows 에이전트 탐색에 명시적인 확장자 처리가 필요하다. `Command::new("claude")`는 PATH에 `claude.cmd`만 있을 때 실패하고, `Command::new("claude.cmd")` 또는 절대 `.cmd` 경로는 동작했다.**

## 시험 방식과 한계

VM의 node·npm·winget은 PATH에서 찾지 못했다. 설치나 외부 에이전트 API 호출 없이 허용된 가짜 npm shim으로 실행 계층만 검증했다. `C:\spike\OWE-830\npm space 한글\`에 `claude.cmd`, `codex.cmd`, Rust argv 덤퍼 `argv-dump.exe`를 두었다. shim 내용은 아래와 같으며, npm처럼 `%*`로 인자를 전달한다. PATH는 각 `Command.env()`에서만 추가했다.

```batch
@echo off
"%~dp0argv-dump.exe" %*
```

Rust 덤퍼가 출력한 인자 벡터를 기대 벡터와 **문자열 전체 일치**로 비교했다. `.exe` 직접 실행을 대조군으로 사용했다. 이는 실제 npm이 생성한 모든 버전의 shim·Node.js·Claude/Codex 동작·인증·hook 이벤트 발생을 검증한 것이 아니다.

## 실측 표

| 항목 | 결과 | 관측 |
|---|---|---|
| `Command::new("claude")` | 실패 | PATH에 `claude.cmd`만 있음 → `NotFound: program not found` |
| `Command::new("codex")` | 실패 | PATH에 `codex.cmd`만 있음 → NotFound |
| `Command::new("claude.cmd")` / `codex.cmd` | 성공 | 이름+PATH 탐색으로 `hello` 한 인자 그대로 전달, exit 0 |
| PATHEXT를 `.CMD`만 설정 후 확장자 없는 `claude` | 실패 | 여전히 NotFound; Command가 PATHEXT를 대신 탐색하지 않음 |
| 공백·한글이 있는 절대 `.cmd` 경로 | 성공 | `npm space 한글\claude.cmd` 실행 |
| `.exe` 직접 인자 | 13/13 일치 | 아래 13개 각각 exit 0·exact=true |
| `.cmd` 경유 인자 | 13/13 일치 | 아래 13개 각각 exit 0·exact=true |
| `.cmd` 결합 인자 | 성공 | `codex.cmd`에 13개를 한꺼번에 전달, 순서/빈 인자 포함 전체 일치 |
| `pwsh -NoProfile -NonInteractive -Command ...` | 성공 | 7.4.20 |
| `powershell.exe` 같은 옵션 | 성공 | 5.1.20348.5756 |
| `cmd.exe /D /C ver` | 성공 | Windows 10.0.20348.5761 |
| `%COMSPEC% /D /C ver` | 성공 | `C:\Windows\system32\cmd.exe` |
| `node`, `npm`, `npm.cmd`, `winget` | 탐색 실패 | NotFound; 설치 디렉터리 전체를 조사한 것은 아님 |
| `C:\Program Files\Git\bin\sh.exe` | 존재·실행 성공 | GNU bash 5.3.15(2)-release |
| `C:\Program Files\Git\usr\bin\sh.exe` | 존재·실행 성공 | 같은 Bash 버전 |
| Git Bash hook 명령 경로 인용 | 두 sh.exe 모두 성공 | 공백·한글 경로 + `hook`, 공백, 따옴표, %, ^, 한글 인자 전체 일치 |
| 같은 hook 명령의 경로 인용 생략 | 두 sh.exe 모두 실패 | exit 127, `C:/spike/OWE-830/npm: No such file or directory` |
| 실제 npm 설치본·실제 Claude Code hook 이벤트 | 미확인 | 가짜 shim 및 sh.exe 수준 검증만 수행 |

인자 시험 집합(13개): `plain`, `has space`, `a"b`, `%KRILL_SPIKE_SENTINEL%`, `a^b`, `한글 인자`, 빈 문자열, 끝에 `\`가 있는 `C:\space dir\`, `x&y`, `x|y`, `x<y>z`, `x!y`, `a'b`. sentinel 환경변수 값은 `EXPANDED`로 설정했으나 `.cmd` 경유 결과도 `%KRILL_SPIKE_SENTINEL%` 그대로였다. `.args()` 사용 시 Rust 1.98.1이 이 shim에 필요한 escaping을 처리했다는 관측이며, 임의 cmd 문자열·중첩 `cmd /c`·delayed expansion·모든 사용자 입력에 대한 안전성 증명은 아니다.

## OWE-833 / Phase 3 설계 권고

1. `which()`에서 Windows는 Unix 실행 비트를 보지 않고 PATH 및 허용 확장자를 확인한다. `.exe`를 우선하고 `.cmd`/`.bat`은 지원하는 실행 종류로 명시한다. PATHEXT 전체를 그대로 실행 허용 목록으로 취급하지 않는다(VBS·JS·MSC 등도 들어 있음). 탐색 결과는 절대 경로로 고정하며, `.cmd`를 찾은 뒤 확장자를 제거하지 않는다. 설치 경로 공백·한글을 회귀 테스트에 넣는다.
2. 실행은 `Command::new(resolved_path).args(argv)`로 한다. 경로 문자열 자체에 따옴표를 붙이거나 `cmd /C` 문자열을 수동 조립하지 않는다. 현재 MSRV에서 확인한 batch escaping을 유지하되, 비신뢰 입력이 shell script로 바뀌는 경계를 별도로 점검한다. 네이티브 `.exe`를 찾으면 이를 선호한다.
3. 기본 대화형 셸 결정안은 **실제 실행 가능한 pwsh.exe → powershell.exe → COMSPEC(cmd.exe)** 순서다. 이번 VM은 첫 후보가 동작했다. 사용자 지정 셸을 먼저 존중하고, 비대화형 preflight의 `-NonInteractive`를 PTY 대화형 실행에 그대로 쓰지 않는다. 셸별 command 옵션과 escaping을 분리하고, spawn 실패 시 다음 후보를 검사한다. 후보가 없는 Windows 이미지의 fallback은 아직 미실측이다.
4. setup 명령이 POSIX 문법이면 Git Bash를 명시 선택하며 Windows 기본 셸에 POSIX 스크립트를 그대로 보내지 않는다. 네이티브 에이전트 프로세스 실행 자체에는 이번 하네스에서 Git Bash가 필요하지 않았다. Git Bash 경로는 설치 위치와 `CLAUDE_CODE_GIT_BASH_PATH` 등 사용자 설정을 확인하고, `bin\sh.exe`/`usr\bin\sh.exe`를 탐색한다. 설치 위치를 이번 VM의 Program Files로 고정하지 않는다.
5. Bash shell-form hook은 실행 경로/각 인자를 shell-quote한다. 검증 명령은 `'C:/spike/OWE-830/npm space 한글/argv-dump.exe' 'hook' 'space value' 'a"b' '100%' 'a^b' '한글'`이다. JSON settings에 넣을 때는 JSON 직렬화를 별도로 적용한다. stdin의 hook JSON을 명령 문자열에 끼워 넣지 않는다.
6. **현재 공식 Claude Code 문서는 Git Bash 미설치 시 PowerShell fallback과 command+args exec form도 명시한다.** 따라서 '모든 Claude Code hook에 Git Bash 필수'라는 가정을 제품 요구사항으로 고정하지 않는다. 설치된 Claude Code 버전의 지원을 확인한 뒤, Krill의 `.exe` hook은 exec form을 우선 검토한다. 이번 실기는 Claude Code 버전/exec form을 확인하지 않았으므로 기존 지원 버전 호환성을 별도로 시험해야 한다.

## 재현·검증

코드·Cargo.lock·원시 `run.log`는 저장소 밖 `~/.cache/krill-win/spikes/OWE-830/`, VM에서는 `C:\spike\OWE-830\`에 보존한다. 저장소 변경은 이 보고서뿐이다.

```sh
~/.cache/krill-win/gcp/ssh.sh 'Set-Location C:\spike\OWE-830; cargo build --locked; cargo fmt --check; & .\target\debug\launch-spike.exe | Tee-Object run.log'
```

`cargo build --locked` 및 `cargo fmt --check` 오류·경고 0, 실행 하네스 exit 0. 13개 단독 인자의 `.exe`/`.cmd` 비교와 결합 인자 모두 exact=true다. 확장자 없는 실행 NotFound와 unquoted hook exit 127은 의도한 음성 대조군이다. 의존 패키지 없는 별도 Rust 프로그램이며 저장소 크레이트를 수정하지 않았다. 전체 `scripts/check.sh`는 코디네이터 통합 검증에서 수행한다. 공유 VM은 중지하지 않는다.

참조: [Rust Command의 Windows 확장자 탐색·batch 인자 주의사항](https://doc.rust-lang.org/std/process/struct.Command.html) (접속일 문서는 1.99.1이므로 1.98.1 동작은 위 실측을 근거로 함), [Claude Code hooks의 shell/exec form·Windows PowerShell 지원](https://code.claude.com/docs/en/hooks) (2026-10-18 조회). 근거 boost 보고서 §4 Phase 2/3·§5와 대조했다.
