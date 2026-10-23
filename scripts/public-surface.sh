#!/usr/bin/env bash
# 공개 표면 점검(하네스 레드팀 P0-4): 공개 저장소·배포 소스 묶음에 내부 문서·개인정보·계정 ID가 섞이지 않게 막는다.
#
#   scripts/public-surface.sh          # 추적 파일(작업 트리의 수정분 포함) 점검
#   scripts/public-surface.sh <dir>    # 디렉터리 점검(예: git archive를 푼 경로)
#
# (a) docs/ 최상위 허용 목록 밖 파일, (b) gitleaks(.gitleaks.toml) 또는 내장 grep 규칙 위반이면 위치를 찍고 1로 끝난다.
set -euo pipefail

# CI는 정식 검사기 없이 축소된 grep 검사로 통과시키지 않는다.
if [[ ${CI:-} == true ]] && ! command -v gitleaks >/dev/null; then
  echo "public-surface: CI requires gitleaks; refusing the grep fallback" >&2
  exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
fail=0

# docs/ 최상위 허용 목록. 여기 없는 것은 공개하지 않는다(내부 보고서는 gitignore된 docs/reports/에 둔다).
# - 규범 문서: ARCHITECTURE·CONVENTIONS·ROADMAP·STYLEGUIDE(AGENTS.md 문서 우선순위)와 PRD.
# - 사용자 문서: ARTIFACTS(공유 서비스·CLI, 계정 정보 없음), SCHEDULING(krill schedule 사용법).
# - agents/(에이전트용 스킬·스니펫), licensing/(라이선스 고지 근거), releases/(릴리스 노트).
# - research/orca-mobile-ui-spec.md는 기존 소스의 라이선스 링크를 위한 공개 안내문만 허용.
# - 기획·조사·벤치마크·QA·마케팅 원자료는 공개하지 않는다.
allowed_docs='^docs/((ARCHITECTURE|ARTIFACTS|CONVENTIONS|PRD|ROADMAP|SCHEDULING|STYLEGUIDE|GETTING_STARTED(\.ko)?)\.md|(agents|licensing|releases)/.+|research/orca-mobile-ui-spec\.md)$'

if [[ $# -gt 0 ]]; then
  scan=$(cd "$1" && pwd)
  docs=$(cd "$scan" && { find docs -type f 2>/dev/null || true; })
else
  # 작업 트리 수정분까지 담은 커밋 객체(스태시 스택에는 넣지 않는다). 수정이 없으면 HEAD.
  tree=$(git -C "$root" stash create)
  scan=$(mktemp -d)
  trap 'rm -rf "$scan"' EXIT
  git -C "$root" archive "${tree:-HEAD}" | tar -x -C "$scan"
  # export-ignore로 묶음에서 빠지는 파일도 추적되면 공개 저장소에는 올라가므로 목록은 git에서 받는다.
  docs=$(git -C "$root" ls-files docs)
fi

bad_docs=$(printf '%s\n' "$docs" | grep -vE "$allowed_docs" | grep . || true)
if [[ -n $bad_docs ]]; then
  echo "public-surface: docs/ 허용 목록 밖 파일(내부 문서는 docs/reports/로):" >&2
  printf '%s\n' "$bad_docs" | sed 's/^/  /' >&2
  fail=1
fi

if command -v gitleaks >/dev/null; then
  if gitleaks dir --help >/dev/null 2>&1; then
    cmd=(gitleaks dir "$scan")
  else
    cmd=(gitleaks detect --no-git --source "$scan")
  fi
  if ! "${cmd[@]}" --config "$root/.gitleaks.toml" --redact --no-banner --exit-code 1; then
    echo "public-surface: gitleaks 위반(위 위치 참고)" >&2
    fail=1
  fi
else
  echo "public-surface: gitleaks 없음 — 내장 grep 규칙으로 대신 점검(vendor/ 제외)" >&2
  # check <설명> <제외 경로 ERE> <패턴 ERE>. 패턴은 GNU·BSD grep 공통 ERE만 쓴다.
  check() {
    local hits
    hits=$(cd "$scan" && grep -rnIE --exclude-dir=vendor --exclude-dir=.git -e "$3" . | awk -F: -v skip="$2" '$1 !~ skip' || true)
    if [[ -n $hits ]]; then
      echo "public-surface: $1:" >&2
      printf '%s\n' "$hits" | cut -c1-200 | sed 's/^/  /' >&2
      fail=1
    fi
  }
  check '개인 메일' '^\./(THIRD_PARTY_NOTICES\.md|docs/licensing/)' \
    '[A-Za-z0-9._%+-]+@(gmail|googlemail|naver|hanmail|daum|kakao|icloud|me|outlook|hotmail|live|proton|protonmail)\.(com|net|me)([^A-Za-z0-9]|$)'
  check '개인 홈 경로(테스트 fixture 제외)' '(tests\.rs|/tests/)' \
    '/(Users|home)/(macmini|craftowen|owen)([^A-Za-z0-9]|$)'
  check 'Cloudflare 계정/Zone ID' '^$' \
    '([Cc]loudflare|[Aa]ccount[_ -]?[Ii][Dd]|[Zz]one).{0,80}[^0-9a-f][0-9a-f]{32}([^0-9a-f]|$)'
  check 'Cloudflare D1 ID' '^$' \
    '([Dd]1|[Dd]atabase[_ -]?[Ii][Dd]).{0,80}[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
  check '토큰·개인 키(리댁션 fixture 제외)' '(crates/krill-proto/src/diag/tests\.rs|crates/krill/tests/doctor\.rs|crates/krill-app/src/crash/tests\.rs)$' \
    '(sk-ant-api[0-9]{2}-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{40,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----)'
fi

if [[ $fail -ne 0 ]]; then
  exit 1
fi
echo "public-surface: ok"
