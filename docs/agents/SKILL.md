---
name: krill
description: Krill 세션 CLI `krill` 사용법. 백그라운드 세션 만들기(new)·끝날 때까지 기다리기(wait)·화면 읽기(read)·입력 보내기(send)·목록(ls)·직접 붙기(attach)·데몬 진단(doctor), 워크트리 만들기/지우기, 바뀐 파일 보기가 필요할 때 사용한다.
---

# krill 스킬 (Krill 세션 CLI)

`krill`은 세션 데몬(krilld)에 직접 붙는 CLI다. 앱이 꺼져 있어도 된다.
아래 usage·동작·종료 코드는 `krill` 0.1.0 실측(2026-10-15, 바이너리를 직접 실행해 대조)과
`crates/krill/src` 기준이다. `krill wait`의 `idle|pattern|quiet` 조건은 MK-047 병합 뒤라 들어 있다.
구현된 CLI usage를 기준으로 사용한다. 오래된 기획 단계의 옵션을 현재 기능으로 가정하지 않는다.
인자 없이 `krill`만 치면 전체 usage가 나간다(종료 2):

```text
usage: krill ls/new/send/read/wait/attach | krill doctor | krill changes [--repo <path>] | krill agent list | krill hook <agent> | krill worktree new <name> | krill worktree rm <path>
```

## 0. 가장 짧은 성공 경로

```bash
krill doctor                              # 데몬 확인. 없으면 자동 기동한다
ID=$(krill new -- /bin/sh -c 'sleep 0.3; echo done')
krill wait "$ID" --timeout 10             # 성공하면 "<id>	exited" 출력, 종료 코드 0
krill read "$ID"                          # done
```

## 1. 세션 명령

인자가 틀리면 아래 usage가 그대로 나가고 종료 코드 2다 (실측).

```text
usage: krill ls | krill new [--cwd <path>] [--agent <profile> | -- <program> <args>...] [--cols N --rows N]
       krill send <id> [--enter] [--stdin | -- <text>...]
       krill read <id> | krill wait <id> [--for idle|waiting|blocked|pattern|quiet|exited|working|done|unknown] [--pattern <regex>] [--quiet-ms N] [--timeout <seconds>]
       krill attach <id> (Ctrl-] detaches; stdin EOF detaches)
```

### `krill doctor` — 데몬 확인 (처음이면 기동)

```bash
krill doctor
# socket        /tmp/xxx/run/krilld.sock
# krill-holder  .../target/debug/krill-holder
# krilld        .../target/debug/krilld
# launch        dev build (posix_spawn + setsid; TCC attribution follows the launcher)
# daemon        ok: krilld 0.1.0 instance 24064 proto 1 (launched)   # 첫 기동 때만 (launched)
```

- 세션 명령(`new`·`send`·`read`·`wait`·`attach`·`ls`·`changes`·`worktree`)도
  붙을 때 데몬이 없으면 같은 경로로 자동 기동한다. 그러니 `doctor`는 진단용이다.
- 실패하면 이유를 한 줄로 보이고 종료 코드 1이다
  (예: `executable not found: .../krill-holder` — `krill` 옆에 `krill-holder`·`krilld`가 없을 때).

### `krill new` — 세션 만들기. 세션 id(숫자) 하나를 출력한다

```bash
ID=$(krill new -- /bin/sh -c "printf 'ready\\n'; IFS= read -r line; echo \"got:$line\"")
ID=$(krill new --cwd /private/tmp -- /bin/sh -c './build.sh')   # 시작 디렉터리 지정
ID=$(krill new --agent claude)                                  # 에이전트 프로필로 시작
ID=$(krill new --cols 120 --rows 40 -- /bin/zsh -l)             # 크기 지정 (0 불가)
```

- `--`는 **한 번만** 쓴다. `krill new -- -- prog`처럼 두 번 쓰면 프로그램 이름이
  `--`가 되어 `No such file or directory (os error 2)`(종료 1)로 실패한다(실측).
- `--cwd`는 미리 절대 경로로 푼다. `--agent` 이름이 없으면
  `unknown agent profile`(종료 1). 쓸 수 있는 이름은 `krill agent list`로 확인한다.

### `krill ls` — 세션 목록

```bash
krill ls
# ID	PID	PROCESS	STATE
# 6	22494	running	unknown
```

- `PROCESS`는 프로세스 생존(`running`·`exited`), `STATE`는 에이전트 상태다.
- 에이전트를 안 띄운 일반 셸은 `STATE`가 `unknown`이다(실측: `sleep 30` 세션이 `unknown`).

### `krill wait` — 원하는 상태·화면·정적이 될 때까지 기다리기 (100ms 간격 폴링)

```bash
krill wait "$ID" --timeout 10                            # 기본은 종료 대기. 끝나면 "<id>	exited"
krill wait "$ID" --for waiting --timeout 60
krill wait "$ID" --for pattern --pattern 'error|FAIL' --timeout 120   # 화면에 정규식이 보이면 끝
krill wait "$ID" --for 'pattern:BUILD DONE' --timeout 300            # 한 인자로도 된다
krill wait "$ID" --for idle --timeout 60                  # 출력 멈추고 에이전트가 일 중이 아니면 끝
krill wait "$ID" --for quiet --quiet-ms 1000 --timeout 60 # 상태와 무관하게 1초 무출력이면 끝
```

- `--for`는 9개 조건이다: 6개 에이전트 상태
  (`exited|working|waiting|blocked|done|unknown`) + `idle|pattern|quiet`.
- `pattern`은 `--pattern <정규식>`(또는 `--for pattern:<정규식>`)으로 주고 현재 화면의
  평문에 적용한다. 정규식이 틀리면 `invalid wait pattern: ...`(종료 2)이다(실측).
  `--for pattern`만 주고 정규식을 안 주면 usage(종료 2)이다.
- `quiet`는 상태와 무관하게 원본 PTY 출력이 멈춘 조건이다. 기본 3초,
  `--quiet-ms N` 또는 `--for quiet:<밀리초>`로 바꾼다(실측: `quiet:1500` 동작).
  `--quiet-ms`는 `quiet`·`idle`하고만 같이 쓴다(다른 조건과 쓰면 usage, 종료 2).
- `idle`은 `quiet`에 "상태가 working·unknown이 아님"을 더한 것이다.
  hook 없는 일반 셸(`sleep 30` 등)은 상태가 `unknown`이라 `idle`이 안 된다(실측: 시간 초과).
  끝난 세션은 `idle`·`quiet`이 바로 맞는다(실측: `2\tidle`, `2\tquiet`).
- `--timeout` 뒤에 `s`를 붙여도 된다(`--timeout 1s`). `0`은 한 번만 검사한다
  (맞으면 `"<id>\t<조건>"`(0), 틀리면 `wait timed out`(2), 실측).
  생략하면 무제한 대기다. `--for`·`--pattern`·`--quiet-ms`·`--timeout`은 한 번씩만 쓴다.
- 시간 초과면 `wait timed out`, 종료 코드 **2**다(실측).
- 없는 id면 `no such session`(패턴 모드) 또는
  `session unavailable or permission denied`, 종료 코드 1이다(실측).
- 성공 출력은 `"<id>\t<조건>"` 한 줄이다(예: `1\tpattern`, `2\tidle`).

### `krill read` — 현재 화면을 plain text로 보기

```bash
krill read "$ID"
```

- 끝난 세션도 읽힌다. `--tail`·`--since` 같은 옵션은 없다. 화면 전체가 나간다.
- 없는 id면 `no such session`, 종료 코드 1이다(실측).

### `krill send` — 세션에 입력 보내기

```bash
krill send "$ID" --enter -- '안녕'        # 텍스트 + Enter(\r)
printf '%s' 'long input' | krill send "$ID" --stdin
```

- 텍스트 인자가 여러 개면 공백 하나로 이어붙인다. `--`로 시작하는 내용을 보내려면
  그 앞에 `--`를 둔다 (`-- --help`처럼).
- `--enter`는 끝에 `\r` 하나를 붙인다. `--stdin`은 stdin을 통째로 보낸다.
- `--stdin`과 텍스트 인자는 같이 못 쓴다. `--enter` 포함 1~65536바이트가 아니면
  `send requires 1..=65536 bytes (including --enter)`, 종료 코드 1이다(실측).
- 세션이 입력 확인 전에 끊기면 재시도하지 않는다 (출력에 `delivery uncertain` 안내가 나간다).

### `krill attach` — 일반 터미널에서 화면째 붙기

```bash
krill attach "$ID"                     # Ctrl-](0x1d) 또는 stdin EOF로 떨어진다
```

- 호스트 터미널 크기를 세션에 전파하고, 떨어질 때 호스트 termios를 복원한다.
- 터미널 질의(DA/DSR 등)는 화면에 보이지 않게 걸러진다. 응답자는 krilld 하나다.

## 2. 에이전트 상태

6개 상태: `working`(일 중) · `waiting`(입력 대기) · `blocked`(승인·차단) ·
`done`(정상 종료) · `exited`(프로세스 종료) · `unknown`(판단 불가, hook 없는 일반 셸).

```bash
krill agent list
# profiles   .../agents.toml (none, built-in only)
# permission manual (agent default)
# claude     Claude Code  2.1.300 (Claude Code)  /Users/alice/.local/bin/claude
# codex      Codex        codex-cli 0.162.0  /Users/alice/.local/bin/codex
# cursor     Cursor CLI   not installed. install: curl https://cursor.com/install -fsS | bash
```

- 없는 에이전트도 실패가 아니다(종료 0). 설치 안내를 보여준다(실측).
- hook이 달린 에이전트(Claude 등)만 상태가 바뀐다. hook 없는 셸은 `unknown`에 머문다.

## 3. 워크트리·바뀐 파일 (데몬 git 작업 큐를 쓴다)

```bash
krill worktree new hello --repo /path/to/repo --setup skip
# /private/tmp/.../hello          # 만든 경로 (첫 줄)
# branch hello (base main)

krill worktree new '새 기능' --repo /path/to/repo --setup skip   # 한글 이름 가능
krill worktree rm /private/tmp/.../hello --repo /path/to/repo
krill changes --repo /private/tmp/.../hello
# .M staged=none unstaged=+1/-0 "f.txt"
```

- `worktree new` 전체 usage(실측):
  `usage: krill worktree new <name> [--repo <path>] [--base <ref>] [--setup run|skip] [--trust-setup <hash>]`
- 이름이 겹치면 `-2`를 붙인다(실측: 같은 이름 두 번 만들면 두 번째가 `...-2`).
- 레포에 setup 스크립트가 있으면 내용을 보여주고 해시(`SHA-256: ...`)로 승인을 받는다.
  터미널이면 `Trust and run this script? [y/N]`로 묻고, 터미널이 아니면
  `--trust-setup <해시>`로 다시 하거나 `--setup skip`으로 건너뛴다.
- `worktree` 뒤에 `new`가 없으면 `expected 'new'`, 이름이 없으면
  `missing worktree name`이다(둘 다 usage + 종료 2, 실측).
- `worktree rm` usage(실측): `usage: krill worktree rm <path> [--repo <path>] [--force]`.
  dirty면 `--force` 없이 거부한다(실측: `worktree is dirty; use --force...`, 종료 1).
  성공하면 `worktree removed` + `branch <이름> deleted|preserved`를 출력한다.
  등록되지 않은 경로면 `path is not a registered worktree...`(종료 1).
- `krill changes` 기본 레포는 현재 디렉터리다. usage(실측):
  `usage: krill changes [--repo <path>]`.
  한 줄에 `XY staged=.. unstaged=.. "경로" <- "원래경로"(rename 때만)` 형식이다.
  `staged`·`unstaged`는 `+추가/-삭제`, `binary`, `none` 중 하나다(실측).
  1000개 상한에 닿으면 stderr에 `warning: changed file list reached its limit...`이 나간다.
  실패 형식은 `changes: <사유>`(종료 1), usage 오류는 종료 2다(실측).

## 4. 종료 코드 (실측 — 기획 기록의 0/2/3/4/5안과 다르다)

| 코드 | 뜻 | 예 |
|---|---|---|
| 0 | 성공 | `wait` 조건 충족, `read`·`send`·`doctor` 정상 |
| 1 | 실패 (사유 한 줄) | `no such session`, `wait` 대상 없음, setup 미승인 |
| 2 | usage 오류 또는 `wait` 시간 초과 | 인자 오류, `--for bogus`, `wait timed out` |

## 5. 함정 (전부 실측)

1. **세션 안에서는 다른 세션을 못 건드린다.** 세션 A 안 셸에서
   `krill send B ...`를 하면 거부된다(실측: `no such session`, 종료 1). 감독·조율은
   세션 밖 일반 셸에서 한다.
2. **`krill new`의 `--`는 한 번만.** 두 번 쓰면 `No such file or directory`가 나간다.
3. **`/tmp`는 심링크다** (`/tmp` → `private/tmp`). `krill`이 돌려주는 경로는 푼 경로
   (`/private/tmp/...`)이므로, 그 뒤 명령에는 돌려받은 경로를 그대로 쓴다.
4. **`krill read`는 전체 화면이다.** 긴 출력 세션에서 토큰을 아끼려면
   세션 안에서 출력 범위를 좁히는 명령을 먼저 `send`한다.
5. **환경 격리**: `KRILL_STATE_DIR`이 있으면 그 디렉터리의 데몬을 쓴다(개발·테스트 격리용).
   평소에는 건드리지 않는다. `KRILL_WORKTREES_DIR`은 워크트리 루트 변경용이다.

## 6. 검증 근거 (2026-10-15, `krill` 0.1.0 dev 빌드, Mac mini)

| 주장 | 명령 | 결과 |
|---|---|---|
| `new`+`wait` 성공 경로 | `krill new -- /bin/echo hi` → `krill wait <id>` → `krill read <id>` | id `3`, `3\texited`(0), `hi` 출력 |
| `send`+`read` 왕복 | `krill send <id> --enter -- hello` → `wait` → `read` | `received:hello` 확인은 통합 테스트 `krill::cli commands_work_without_app...` 통과 |
| 일반 셸 상태 | `krill new -- /bin/sleep 30` 후 `krill ls`, `krill wait --for working --timeout 5` | `unknown`, `wait timed out`(2) |
| pattern 대기 | `krill wait <id> --for pattern --pattern 'marker-abc'`, `--for 'pattern:marker-abc'` | `1\\tpattern`(0) 둘 다 |
| quiet·idle | 출력 멈춘 뒤 `--for quiet`(기본 3초), `--for quiet:1500`, 끝난 세션에 `--for idle` | `1\\tquiet`, `2\\tidle`(0) |
| idle 제외 | hook 없는 `sleep` 세션에 `--for idle` | `wait timed out`(2, 상태가 `unknown`이라서) |
| timeout 0 검사 | `--for pattern --pattern ... --timeout 0` (맞음/틀림) | 맞으면 `1\\tpattern`(0), 틀리면 `wait timed out`(2) |
| 정규식 오류·인자 오류 | `--pattern '['`, `--for pattern` 단독, `--quiet-ms` 단독 조건, `--for` 두 번 | `invalid wait pattern`(2), 나머지는 usage(2) |
| 없는 세션 | `krill read 999999`, `krill send 999999 -- hi` | `no such session`(1) |
| usage 코드 | `krill`, `krill send`, `krill wait`, `krill read`, `krill changes --bogus` | usage 출력(2) |
| worktree 생성·dirty·삭제 | `worktree new hello --setup skip` → dirty 후 `rm`(거부) → `rm --force`(성공) | `branch hello (base main)`, dirty 거부(1), `worktree removed` + `branch hello deleted` |
| `changes` 형식 | dirty 후 `krill changes --repo <wt>` | `.M staged=none unstaged=+1/-0 "f.txt"`(0) |
| `doctor` 첫 기동·재사용 | 빈 `KRILL_STATE_DIR`에서 `doctor` 두 번 | `(launched)` → 재사용, 둘 다 0 |
| `agent list` 미설치 | `krill agent list` | cursor·gemini·opencode 설치 안내, 0 |
| `hook` fail-open | `printf ... \| krill hook claude` (에이전트 내부용) | `{}` 출력, 0 |
| 세션 범위 거부 | 세션 안에서 `krill send <다른 id>` (통합 테스트) | `no such session`(1), 원래 세션에 `hijack` 없음 |
