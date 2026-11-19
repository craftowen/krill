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
  # 명령 시작 위치(줄 처음, ; & | ( ` $( 뒤)에서만 잡는다 — 커밋 메시지 속 글자는 통과.
  # 그 앞의 환경변수 대입, env·command·exec 같은 래퍼(옵션 포함), 절대 경로 실행도 같은 명령으로 본다.
  pre="(^|[;&|(\`]|\\\$\\()$s*(([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*|([^[:space:]]*/)?(env|command|exec|builtin|nohup|sudo|time)|-[A-Za-z-]+($s+[A-Z_][A-Z0-9_]*)?)$s+)*([^[:space:]]*/)?"
  gh="${pre}gh($s+(-R|--repo)($s+|=)[^[:space:]]+)?$s+"
  why=''
  if grep -Eq "${pre}git($s+(-C|-c)$s+[^[:space:]]+|$s+--[^[:space:]]+)*$s+push($s|$)" <<<"$cmd"; then why='git push'
  elif grep -Eq "${gh}release$s+(create|upload|delete)" <<<"$cmd"; then why='gh release create/upload/delete'
  elif grep -Eq "${gh}pr$s+merge" <<<"$cmd"; then why='gh pr merge'
  elif grep -Eq "${gh}(workflow$s+(run|enable|disable)|run$s+rerun)" <<<"$cmd"; then why='gh workflow 실행·설정(유료 실행 포함)'
  elif grep -Eiq "${gh}api$s.*(-X|--method)($s*|=)(PUT|POST|PATCH|DELETE)" <<<"$cmd"; then why='gh api 쓰기 요청'
  elif grep -Eq "${gh}api$s.*$s(-f|-F|--field|--raw-field|--input)($s|=)" <<<"$cmd" \
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
# 보호 목록은 이 훅이 속한 저장소의 .github/CODEOWNERS 하나다('/x/'는 하위 전체, '/x'는 그 파일).
# Cargo.toml은 리뷰 대상이지만 의존성·버전 편집이 일상이라 여기서는 막지 않는다.
owners="$(dirname "${BASH_SOURCE[0]}")/../../.github/CODEOWNERS"
[[ -f $owners ]] || { echo "guard.sh: $owners가 없어 하네스 보호 목록을 읽을 수 없다(차단)" >&2; exit 2; }
hit=''
while read -r pattern _; do
  [[ $pattern == /* && $pattern != /Cargo.toml ]] || continue
  pattern=${pattern#/}
  if [[ $pattern == */ ]]; then [[ $rel == "$pattern"* ]] && hit=1; else [[ $rel == "$pattern" ]] && hit=1; fi
done < "$owners"
[[ -z $hit ]] && exit 0
echo "차단: 하네스 파일 $rel — AGENTS.md 규칙 8. 메인테이너 승인 필요 — 승인받았으면 사람이 KRILL_HARNESS_APPROVED=1로 Claude를 다시 띄운다." >&2
exit 2
