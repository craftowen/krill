#!/usr/bin/env bash
# Claude Code PreToolUse 훅: AGENTS.md 규칙 8(하네스 파일)·9(push·게시·원격 변경) 기계 강제.
# 입력: stdin JSON {tool_name, tool_input{file_path|notebook_path|command}}. exit 2 = 차단(stderr가 에이전트에게 간다).
# 세션 설정: KRILL_HARNESS_APPROVED=1(하네스 파일), KRILL_ALLOW_REMOTE=1(원격 쓰기 명령).
# 사용자 지시를 받은 원격 명령은 첫 줄에 `# KRILL_ALLOW_REMOTE: <사용자 지시와 작업 사유>`를 기록한다.
# 이 표시는 사용자 지시를 기록하는 것이며 자동 승인이나 배포 검증 생략을 뜻하지 않는다.
set -uo pipefail
command -v jq >/dev/null || { echo "guard.sh: jq가 없어 하네스 보호 훅을 실행할 수 없다(차단)" >&2; exit 2; }
input=$(cat)
tool=$(jq -r '.tool_name // empty' <<<"$input")

if [[ $tool == Bash ]]; then
  [[ ${KRILL_ALLOW_REMOTE-} == 1 ]] && exit 0
  cmd=$(jq -r '.tool_input.command // empty' <<<"$input")
  s='[[:space:]]'
  # 명령 시작 위치(줄 처음, ; & | ( ` $( 뒤, 환경변수 대입 허용)에서만 잡는다 — 커밋 메시지 속 글자는 통과.
  pre="(^|[;&|(\`]|\\\$\\()$s*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*$s+)*"
  why=''
  if grep -Eq "${pre}git($s+(-C|-c)$s+[^[:space:]]+|$s+--[^[:space:]]+)*$s+push($s|$)" <<<"$cmd"; then why='git push'
  elif grep -Eq "${pre}gh$s+release$s+(create|upload|delete)" <<<"$cmd"; then why='gh release create/upload/delete'
  elif grep -Eiq "${pre}gh$s+api$s.*(-X|--method)($s*|=)(PUT|POST|PATCH|DELETE)" <<<"$cmd"; then why='gh api 쓰기 요청'
  elif grep -Eq "${pre}gh$s+api$s.*$s(-f|-F|--field|--raw-field|--input)($s|=)" <<<"$cmd" \
    && ! grep -Eiq "(-X|--method)($s*|=)GET" <<<"$cmd"; then why='gh api 필드 지정(기본 POST)'
  elif grep -Eq "${pre}((ba|z)?sh$s+)?[^[:space:]]*publish-github\.sh" <<<"$cmd"; then why='publish-github.sh 게시'
  fi
  [[ -z $why ]] && exit 0
  first_line=${cmd%%$'\n'*}
  if [[ $first_line =~ ^#[[:space:]]KRILL_ALLOW_REMOTE:[[:space:]]*(.*[^[:space:]].*)$ ]]; then
    echo "허용: $why — 사용자 지시 기록: ${BASH_REMATCH[1]}" >&2
    exit 0
  fi
  echo "차단: $why — AGENTS.md 규칙 9. 사용자 배포·릴리스·push 지시는 필요한 main·v* 태그 push와 게시를 허용한다. 그 외 원격 쓰기는 별도 명시 지시가 필요하다." >&2
  echo "해당 지시를 받았으면 명령 첫 줄에 '# KRILL_ALLOW_REMOTE: <사용자 지시와 작업 사유>'를 기록한다. 재시작은 필요 없다." >&2
  exit 2
fi

[[ ${KRILL_HARNESS_APPROVED-} == 1 ]] && exit 0
path=$(jq -r '.tool_input.file_path // .tool_input.notebook_path // empty' <<<"$input")
[[ -z $path ]] && exit 0
[[ $path == /* ]] || path="$(jq -r '.cwd // empty' <<<"$input")/$path"
# 새 파일이면 디렉터리가 아직 없을 수 있어 존재하는 가장 가까운 조상에서 저장소 루트를 찾는다.
d=$(dirname "$path"); while [[ ! -d $d ]]; do d=$(dirname "$d"); done
root=$(git -C "$d" rev-parse --show-toplevel 2>/dev/null) || exit 0
root=$(cd "$root" && pwd -P)
rel="$(cd "$d" && pwd -P)${path#"$d"}"; rel=${rel#"$root"}; rel=${rel#/}
case $rel in
  AGENTS.md|CLAUDE.md|docs/CONVENTIONS.md|clippy.toml|rustfmt.toml|deny.toml|.cargo/*|.config/nextest.toml|scripts/*|.github/*|.claude/settings.json|.claude/hooks/*) ;;
  *) exit 0 ;;
esac
echo "차단: 하네스 파일 $rel — AGENTS.md 규칙 8. 메인테이너 승인 필요 — 승인받았으면 사람이 KRILL_HARNESS_APPROVED=1로 Claude를 다시 띄운다." >&2
exit 2
