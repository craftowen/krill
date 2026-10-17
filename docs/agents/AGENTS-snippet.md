# Krill 세션 (`krill`) — 붙여넣기용 스니펫

이 레포에서 오래 걸리는 작업(빌드·테스트·에이전트 실행)은 Krill 세션으로 돌리고
터미널을 비워 둔다. 자세한 사용법은 `docs/agents/SKILL.md`(krill 스킬)다.
아래 usage·종료 코드는 `krill` 0.1.0 실측이다. 기획 기록의 `--json`·토큰 인증은 쓰지 않는다. `wait`의 `idle|pattern|quiet` 조건은 들어 있다(MK-047).

```bash
krill doctor                              # 데몬 확인. 없으면 자동 기동한다
ID=$(krill new -- /bin/sh -c './build.sh') # 세션 시작. id(숫자) 하나가 나온다. `--`는 한 번만
krill wait "$ID" --timeout 600            # 끝날 때까지 대기. 끝나면 "<id>\texited"(0)
krill read "$ID"                          # 화면 보기. 끝난 세션도 읽힌다
krill send "$ID" --enter -- '추가 입력'    # 입력 보내기. 세션 안에서는 다른 세션 조작 불가
krill ls                                  # ID\tPID\tPROCESS\tSTATE 목록
```

- `wait` 시간 초과는 `wait timed out`, 종료 코드 2다. `--for`는 6개 상태 +
  `idle|pattern|quiet`다. 화면 대기는 `krill wait <id> --for pattern --pattern 'DONE'`,
  무출력 대기는 `krill wait <id> --for quiet`(기본 3초)다.
- 인자가 틀리면 usage가 나가고 종료 코드 2, 없는 세션은 `no such session`(1)이다.
- `krill read`는 전체 화면을 내놓는다. `krill attach <id>`는 직접 붙기(Ctrl-]로 떨어짐).
- 워크트리: `krill worktree new <이름> --repo <경로> --setup skip`,
  삭제는 `krill worktree rm <경로> --repo <경로>`(dirty면 `--force` 필요).
  바뀐 파일: `krill changes --repo <경로>`.
