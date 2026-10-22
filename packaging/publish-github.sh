#!/bin/bash
# 로컬 맥에서 Krill.app·ZIP·DMG·Homebrew tap을 조립하고 GitHub Releases에 발행한다.
# 게시 전 가드(origin/main·CI·기존 태그/릴리스·고지문·공개 표면)를 모두 통과해야 한다(하네스 레드팀 P0-3).
# 가드만 보려면 --preflight, 업로드 없이 조립까지 하려면 --dry-run을 사용한다.
set -euo pipefail

# 동시 게시 방지(v0.1.6 사고): 이 맥의 모든 워크트리가 같은 커널 락을 쓴다. 프로세스가 죽으면 락도 풀린다.
PUBLISH_LOCK=/tmp/krill-publish.lock
if [[ -z "${KRILL_PUBLISH_LOCKED:-}" ]]; then
  status=0
  KRILL_PUBLISH_LOCKED=1 /usr/bin/lockf -k -t 0 "$PUBLISH_LOCK" /bin/bash "$0" "$@" || status=$?
  [[ "$status" -ne 75 ]] || echo "another publish-github.sh run holds $PUBLISH_LOCK; wait for it to finish" >&2
  exit "$status"
fi
cd "$(dirname "$0")/.."

usage() {
  cat >&2 <<'USAGE'
usage: bash packaging/publish-github.sh [VERSION] [--bump patch|minor|major] [--repo OWNER/REPO] [--verified-release RECEIPT] [--bin-dir DIR] [--dry-run|--preflight]

Options:
  VERSION            릴리스 버전 (생략 시 Cargo.toml의 [workspace.package].version 사용)
  --bump LEVEL       가드 통과 후 Cargo.toml 버전을 patch|minor|major로 올리고 Cargo.lock·THIRD_PARTY_NOTICES.md와
                     함께 커밋한 뒤 멈춘다(그 커밋을 main에 올리고 CI가 녹색이 된 뒤 다시 실행해 발행)
  --repo OWNER/REPO  대상 GitHub 저장소 (생략 시 git remote origin에서 자동 추출)
  --bin-dir DIR      이미 빌드된 바이너리로 빌드 생략. --dry-run 전용(게시본은 항상 이 스크립트가 직접 빌드)
  --verified-release RECEIPT  같은 HEAD의 전체 로컬·메모리·생존 검사를 증명하는 Ed25519 서명 receipt.
                             서명·툴체인·범위·고정 바이너리 해시를 확인한 경우에만 게시 빌드를 재사용한다.
  --dry-run          로컬 번들·DMG·Homebrew tap·latest.json 조립까지만 수행하고 git push / gh release 생략.
                     가드 실패는 경고로만 출력한다
  --preflight        게시 가드만 실행하고 끝낸다(하나라도 실패하면 1)

Environment:
  KRILL_WIN_VM_DIR=<dir>      Windows zip을 상시 Windows VM에서 빌드해 latest-windows.json·install.ps1과 함께 넣는다
                             (packaging/windows/build-vm.sh)
  KRILL_RELEASE_ASSET_DIR=<dir> 같은 HEAD의 Windows 네이티브 빌드·스모크 산출물(야간 배포용).
                             source-commit·체크섬·Windows 공개키 확인 후 가져온다. VM 빌드 옵션과 함께 쓰지 않는다.
  KRILL_PUBLISH_FORCE=<사유>  사람 전용 탈출구. 대화형 터미널에서 태그를 직접 입력해야 하며 사유를 출력한다.
                             이미 게시된 태그·릴리스, 로컬 태그 불일치, 공개 표면 위반은 덮어쓰지 못한다
USAGE
  exit 2
}

VERSION=""
BUMP=""
REPO=""
BIN_DIR=""
VERIFIED_RELEASE=""
DRY_RUN=0
PREFLIGHT=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bump)
      [[ $# -ge 2 ]] || usage
      BUMP="$2"
      shift 2
      ;;
    --repo)
      [[ $# -ge 2 ]] || usage
      REPO="$2"
      shift 2
      ;;
    --bin-dir)
      [[ $# -ge 2 ]] || usage
      BIN_DIR="$2"
      shift 2
      ;;
    --verified-release)
      [[ $# -ge 2 ]] || usage
      VERIFIED_RELEASE="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --preflight)
      PREFLIGHT=1
      shift
      ;;
    -h|--help)
      usage
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage
      ;;
    *)
      [[ -z "$VERSION" ]] || usage
      VERSION="$1"
      shift
      ;;
  esac
done

if [[ -n "$VERSION" && -n "$BUMP" ]]; then
  echo "cannot specify both VERSION and --bump" >&2
  exit 2
fi
if [[ "$PREFLIGHT" -eq 1 ]]; then
  DRY_RUN=0
fi
if [[ -n "$BUMP" && ( "$DRY_RUN" -eq 1 || "$PREFLIGHT" -eq 1 ) ]]; then
  echo "--bump cannot be combined with --dry-run or --preflight" >&2
  exit 2
fi
if [[ -n "$BIN_DIR" && "$DRY_RUN" -eq 0 ]]; then
  echo "--bin-dir is dry-run only; published binaries must come from this script's own build" >&2
  exit 2
fi
if [[ -n "$BIN_DIR" && -n "$VERIFIED_RELEASE" ]]; then
  echo "--bin-dir cannot be combined with --verified-release" >&2
  exit 2
fi
if [[ -n "${KRILL_RELEASE_ASSET_DIR:-}" && ( -n "${KRILL_GCP_PROJECT:-}" || -n "${KRILL_WIN_VM_DIR:-}" ) ]]; then
  echo "KRILL_RELEASE_ASSET_DIR cannot be combined with VM builds" >&2
  exit 2
fi

if [[ -z "$REPO" ]]; then
  ORIGIN_URL=$(git remote get-url origin 2>/dev/null || true)
  if [[ "$ORIGIN_URL" =~ github\.com[:/]([^/]+/[^/.]+)(\.git)?$ ]]; then
    REPO="${BASH_REMATCH[1]}"
  else
    echo "cannot infer GitHub repository from origin; pass --repo OWNER/REPO" >&2
    exit 1
  fi
fi
[[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || {
  echo "invalid --repo format (expected OWNER/REPO): $REPO" >&2
  exit 2
}

if [[ "$DRY_RUN" -eq 0 || -n "$BUMP" ]]; then
  [[ -z "$(git status --porcelain --untracked-files=normal)" ]] || {
    echo "publish requires a clean worktree; commit or stash changes first" >&2
    exit 1
  }
fi
[[ -f packaging/bundle.sh ]] || {
  echo "packaging/bundle.sh (MK-061) is required before publishing" >&2
  exit 1
}

CARGO_VER=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)
if [[ -n "$BUMP" ]]; then
  [[ "$CARGO_VER" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || {
    echo "unsupported workspace version for --bump: $CARGO_VER" >&2
    exit 1
  }
  MAJOR="${BASH_REMATCH[1]}"
  MINOR="${BASH_REMATCH[2]}"
  PATCH="${BASH_REMATCH[3]}"
  case "$BUMP" in
    patch) VERSION="${MAJOR}.${MINOR}.$((PATCH + 1))" ;;
    minor) VERSION="${MAJOR}.$((MINOR + 1)).0" ;;
    major) VERSION="$((MAJOR + 1)).0.0" ;;
    *)
      echo "--bump must be patch, minor, or major" >&2
      exit 2
      ;;
  esac
fi

VERSION=${VERSION:-$CARGO_VER}
VERSION=${VERSION#v}
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] || {
  echo "invalid semantic version: $VERSION" >&2
  exit 2
}
[[ -n "$BUMP" || "$VERSION" == "$CARGO_VER" ]] || {
  echo "version ($VERSION) does not match Cargo.toml ($CARGO_VER)" >&2
  exit 1
}

if [[ "$DRY_RUN" -eq 0 ]]; then
  command -v gh >/dev/null 2>&1 || { echo "gh CLI not found; install GitHub CLI first" >&2; exit 1; }
  gh auth status >/dev/null 2>&1 || { echo "gh CLI is not authenticated; run 'gh auth login'" >&2; exit 1; }
fi

TAG="v${VERSION}"
REPO_URL="https://github.com/${REPO}"
SOURCE_URL="${REPO_URL}/releases/download/${TAG}/Krill-${VERSION}-source.tar.gz"
DMG_URL="${REPO_URL}/releases/download/${TAG}/Krill-${VERSION}.dmg"
OUT_DIR="packaging/dist"

HEAD_SHA=$(git rev-parse HEAD)
KC_PATH="${KRILL_CODESIGN_KEYCHAIN:-$(python3 packaging/macos/identity_paths.py)}"
UPDATE_KEY_FILE="${KRILL_UPDATE_KEY_FILE:-${KC_PATH}.ed25519.key}"

# ---- 게시 가드 ----
# soft: KRILL_PUBLISH_FORCE로 사람이 넘길 수 있다. hard: 넘길 수 없다(덮어쓰기·잘못된 소스·유출은 되돌릴 수 없음).
SOFT_FAILS=0
HARD_FAILS=0
soft_fail() { echo "publish guard: $*" >&2; SOFT_FAILS=$((SOFT_FAILS + 1)); }
hard_fail() { echo "publish guard (no override): $*" >&2; HARD_FAILS=$((HARD_FAILS + 1)); }
HAVE_GH=0
command -v gh >/dev/null 2>&1 && HAVE_GH=1

if CARGO_TARGET_DIR=$(python3 packaging/release_check.py target-dir --base "${CARGO_TARGET_DIR:-}"); then
  export CARGO_TARGET_DIR
else
  hard_fail "external build target validation failed; internal target fallback is forbidden"
fi
if [[ -n "$VERIFIED_RELEASE" ]]; then
  if ! BIN_DIR=$(python3 packaging/release_check.py verify --receipt "$VERIFIED_RELEASE" --head "$HEAD_SHA" --key-file "$UPDATE_KEY_FILE"); then
    hard_fail "verified release receipt or tested binaries are invalid"
  fi
fi

# 같은 버전을 두 번 게시하지 않는다. 빌드 뒤 태그 직전에 한 번 더 확인한다.
check_unpublished() {
  local remote_tag release_out
  if ! remote_tag=$(git ls-remote --tags origin "refs/tags/${TAG}"); then
    hard_fail "cannot list tags on origin"
  elif [[ -n "$remote_tag" ]]; then
    hard_fail "tag ${TAG} already exists on origin; bump the version instead of re-publishing"
  fi
  [[ "$HAVE_GH" -eq 1 ]] || return 0
  if release_out=$(gh release view "$TAG" --repo "$REPO" --json tagName 2>&1); then
    hard_fail "GitHub release ${TAG} already exists in ${REPO}; assets are never overwritten"
  elif ! grep -qi 'not found' <<<"$release_out"; then
    hard_fail "cannot verify that release ${TAG} is absent: ${release_out}"
  fi
}

echo "==> Publish guards for ${TAG} at ${HEAD_SHA:0:12} (${REPO})"
if git fetch --quiet origin; then
  MAIN_SHA=$(git rev-parse -q --verify refs/remotes/origin/main || true)
  [[ "$HEAD_SHA" == "$MAIN_SHA" ]] ||
    soft_fail "HEAD ${HEAD_SHA:0:12} is not origin/main (${MAIN_SHA:0:12}); releases are cut from main"
else
  hard_fail "git fetch origin failed; cannot verify remote state"
fi
check_unpublished
if [[ "$HAVE_GH" -eq 1 ]]; then
  if CHECK_RUNS=$(gh api "repos/${REPO}/commits/${HEAD_SHA}/check-runs?per_page=100" --paginate \
      --jq '.check_runs[] | "\(.conclusion // .status)\t\(.name)"' 2>&1); then
    NOT_GREEN=$(grep -vE $'^(success|skipped)\t' <<<"$CHECK_RUNS" | grep . || true)
    if [[ -z "$CHECK_RUNS" ]]; then
      soft_fail "no CI check-runs on ${HEAD_SHA:0:12}; wait for CI"
    elif [[ -n "$NOT_GREEN" ]]; then
      soft_fail "CI check-runs on ${HEAD_SHA:0:12} are not all green:"$'\n'"$NOT_GREEN"
    fi
  else
    soft_fail "cannot read CI check-runs: ${CHECK_RUNS}"
  fi
else
  soft_fail "gh CLI not found; cannot verify CI or existing releases"
fi
if [[ "$HAVE_GH" -eq 1 ]] && ! python3 packaging/ci_evidence.py verify --repo "$REPO" --sha "$HEAD_SHA" --branch main; then
  hard_fail "required workflow validation proof is missing or invalid"
fi
if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  LOCAL_TAG_SHA=$(git rev-parse "refs/tags/${TAG}^{commit}")
  [[ -z "$BUMP" && "$LOCAL_TAG_SHA" == "$HEAD_SHA" ]] ||
    hard_fail "local tag ${TAG} points at ${LOCAL_TAG_SHA:0:12}, not the commit being released; inspect it and git tag -d ${TAG}"
fi
# --bump은 아래에서 고지문을 다시 만들어 같은 커밋에 넣는다.
if [[ -z "$BUMP" ]] && ! python3 docs/licensing/generate_notices.py --check >/dev/null; then
  soft_fail "THIRD_PARTY_NOTICES.md is stale; run python3 docs/licensing/generate_notices.py and commit"
fi
# 공개 표면: 소스 tarball(git archive HEAD)과 같은 내용에 내부 문서·비밀이 없어야 한다.
FORBIDDEN=$(git ls-tree -r --name-only HEAD | grep -E '^(docs/(boost-|reports/)|\.night/)' || true)
[[ -z "$FORBIDDEN" ]] || hard_fail "internal files are tracked and would ship in the source tarball:"$'\n'"$FORBIDDEN"
if [[ -f scripts/public-surface.sh ]]; then
  SCAN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/krill-publish-scan.XXXXXX")
  trap 'rm -rf "$SCAN_DIR"' EXIT
  git archive HEAD | tar -x -C "$SCAN_DIR"
  bash scripts/public-surface.sh "$SCAN_DIR" || hard_fail "scripts/public-surface.sh rejected the source tarball"
fi

if [[ $((SOFT_FAILS + HARD_FAILS)) -eq 0 ]]; then
  echo "==> All publish guards passed"
elif [[ "$PREFLIGHT" -eq 1 ]]; then
  echo "preflight: ${SOFT_FAILS} overridable and ${HARD_FAILS} hard guard failure(s)" >&2
  exit 1
elif [[ "$DRY_RUN" -eq 1 ]]; then
  echo "warning: dry-run continues, but a real publish would stop here" >&2
elif [[ "$HARD_FAILS" -gt 0 ]]; then
  echo "publish aborted: ${HARD_FAILS} guard failure(s) cannot be overridden" >&2
  exit 1
elif [[ "${KRILL_PUBLISH_FORCE:-}" =~ [^[:space:]] ]]; then
  [[ -t 0 ]] || {
    echo "KRILL_PUBLISH_FORCE needs an interactive terminal: a maintainer must type the tag to confirm" >&2
    exit 1
  }
  read -r -p "Override ${SOFT_FAILS} publish guard(s) for ${TAG}? Type ${TAG} to confirm: " CONFIRM
  [[ "$CONFIRM" == "$TAG" ]] || { echo "override not confirmed" >&2; exit 1; }
  echo "KRILL_PUBLISH_FORCE override for ${TAG} by $(git config user.name || whoami): ${KRILL_PUBLISH_FORCE}" >&2
else
  echo "publish aborted: ${SOFT_FAILS} guard failure(s). A maintainer may override with KRILL_PUBLISH_FORCE='<reason>' in an interactive terminal." >&2
  exit 1
fi
if [[ "$PREFLIGHT" -eq 1 ]]; then
  exit 0
fi

if [[ -n "$BUMP" ]]; then
  python3 -c '
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
v = sys.argv[2]
text = p.read_text()
updated, n = re.subn(r"(?ms)(^\[workspace\.package\]\n.*?^version = \")[^\"]+(\")", rf"\g<1>{v}\2", text, count=1)
assert n == 1, "failed to update [workspace.package].version"
p.write_text(updated)
' Cargo.toml "$VERSION"
  cargo update -w
  python3 docs/licensing/generate_notices.py
  git commit -m "Release v${VERSION}" Cargo.toml Cargo.lock THIRD_PARTY_NOTICES.md
  echo "Committed Release v${VERSION}. Get it onto origin/main (maintainer or user-authorized agent push), wait for CI to pass,"
  echo "then publish it with: bash packaging/publish-github.sh"
  exit 0
fi

# 위 가드에서 검증한 외장 볼륨의 게시 전용 target만 쓴다.
echo "==> Using dedicated CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
if [[ "$DRY_RUN" -eq 0 && -z "$VERIFIED_RELEASE" ]]; then
  echo "publish requires --verified-release from release_check.py validate for this HEAD" >&2
  exit 1
fi

rm -rf "$OUT_DIR"

BUNDLE_ARGS=(--output "$OUT_DIR" --repo "$REPO")
if [[ -n "$BIN_DIR" ]]; then
  BUNDLE_ARGS+=(--bin-dir "$BIN_DIR")
fi

echo "==> [1/3] Building Krill.app bundle & ZIP..."
bash packaging/bundle.sh "${BUNDLE_ARGS[@]}"
if [[ -n "$VERIFIED_RELEASE" ]]; then
  python3 packaging/release_check.py verify --receipt "$VERIFIED_RELEASE" --head "$HEAD_SHA" --key-file "$UPDATE_KEY_FILE" >/dev/null
  python3 packaging/release_check.py verify --receipt "$VERIFIED_RELEASE" --head "$HEAD_SHA" \
    --key-file "$UPDATE_KEY_FILE" --bundle "$OUT_DIR/Krill.app"
  python3 packaging/macos/smoke.py "$OUT_DIR/Krill.app"
fi

echo "==> [2/3] Building DMG, Homebrew tap & signed latest.json..."
KRILL_RELEASE_ALLOW_DIRTY="$DRY_RUN" bash packaging/release.sh \
  "$VERSION" \
  "$REPO_URL" \
  "$SOURCE_URL" \
  "$DMG_URL" \
  "$OUT_DIR/Krill.app" \
  "$OUT_DIR"

shopt -s nullglob
ZIP_MATCHES=("$OUT_DIR"/Krill-"${VERSION}"-*-adhoc.zip)
[[ "${#ZIP_MATCHES[@]}" -eq 1 ]] || {
  echo "expected exactly one zip archive in $OUT_DIR, found ${#ZIP_MATCHES[@]}" >&2
  exit 1
}
ZIP_PATH="${ZIP_MATCHES[0]}"
ZIP_NAME=$(basename "$ZIP_PATH")
ZIP_SHA256=$(awk '{print $1}' "${ZIP_PATH}.sha256")
ZIP_URL="${REPO_URL}/releases/download/${TAG}/${ZIP_NAME}"
MIN_DAEMON_PROTO=$(sed -n 's/^pub const PROTO_MIN: u16 = \([0-9][0-9]*\);/\1/p' crates/krill-proto/src/ipc.rs)
MIN_DAEMON_PROTO=${MIN_DAEMON_PROTO:-1}
KC_PATH="${KRILL_CODESIGN_KEYCHAIN:-$(python3 packaging/macos/identity_paths.py)}"
UPDATE_KEY_FILE="${KRILL_UPDATE_KEY_FILE:-${KC_PATH}.ed25519.key}"
RELEASE_CHANNEL="stable"
if [[ "$VERSION" == *-* ]]; then
  RELEASE_CHANNEL="beta"
fi
python3 packaging/macos/ed25519_manifest.py sign \
  --key-file "$UPDATE_KEY_FILE" \
  --version "$VERSION" \
  --url "$ZIP_URL" \
  --sha256 "$ZIP_SHA256" \
  --min-daemon-protocol "$MIN_DAEMON_PROTO" \
  --channel "$RELEASE_CHANNEL" \
  --rollout-pct 100 \
  --output "$OUT_DIR/latest.json"

cp packaging/install.sh "$OUT_DIR/install.sh"
chmod 755 "$OUT_DIR/install.sh"
if [[ -n "${KRILL_RELEASE_ASSET_DIR:-}" ]]; then
  ASSET_PUBLIC_KEY=$(python3 packaging/macos/ed25519_manifest.py keygen --key-file "$UPDATE_KEY_FILE")
  python3 packaging/nightly_assets.py --directory "$KRILL_RELEASE_ASSET_DIR" --output "$OUT_DIR" \
    --version "$VERSION" --commit "$HEAD_SHA" --public-key "$ASSET_PUBLIC_KEY"
fi
# Windows zip: KRILL_WIN_VM_DIR가 있으면 상시 Windows VM을 켜서 빌드·스모크 후 끈다(packaging/windows/README.md).
# 매니페스트는 macOS와 같은 키로 맥에서 서명한다. latest.json은 스키마상 항목 하나라 Windows는 latest-windows.json을 따로 둔다.
if [[ -n "${KRILL_WIN_VM_DIR:-}" ]]; then
  WIN_ZIP_NAME="Krill-${VERSION}-windows-x64.zip"
  WIN_PUB=$(python3 packaging/macos/ed25519_manifest.py keygen --key-file "$UPDATE_KEY_FILE")
  bash packaging/windows/build-vm.sh "$VERSION" "$WIN_PUB" \
    "${REPO_URL}/releases/latest/download/latest-windows.json" "$OUT_DIR"
fi
WIN_ZIP_NAME="Krill-${VERSION}-windows-x64.zip"
if [[ -f "$OUT_DIR/$WIN_ZIP_NAME" ]]; then
  python3 packaging/macos/ed25519_manifest.py sign \
    --key-file "$UPDATE_KEY_FILE" \
    --version "$VERSION" \
    --url "${REPO_URL}/releases/download/${TAG}/${WIN_ZIP_NAME}" \
    --sha256 "$(awk '{print $1}' "$OUT_DIR/${WIN_ZIP_NAME}.sha256")" \
    --min-daemon-protocol "$MIN_DAEMON_PROTO" \
    --channel "$RELEASE_CHANNEL" \
    --rollout-pct 100 \
    --output "$OUT_DIR/latest-windows.json"
  cp packaging/windows/install.ps1 "$OUT_DIR/install.ps1"
fi
WINDOWS_ASSETS=()
if [[ -f "$OUT_DIR/Krill-${VERSION}-windows-x64.zip" ]]; then
  WINDOWS_ASSETS=(
    "$OUT_DIR/Krill-${VERSION}-windows-x64.zip"
    "$OUT_DIR/Krill-${VERSION}-windows-x64.zip.sha256"
    "$OUT_DIR/latest-windows.json"
    "$OUT_DIR/install.ps1"
  )
  for asset in "${WINDOWS_ASSETS[@]}"; do
    [[ -f "$asset" ]] || { echo "missing Windows release asset: $asset" >&2; exit 1; }
  done
else
  echo "warning: no Windows zip (set KRILL_WIN_VM_DIR to build it)" >&2
fi

ASSETS=(
  "$OUT_DIR/Krill-${VERSION}.dmg"
  "$OUT_DIR"/Krill-"${VERSION}"-*-adhoc.zip
  "$OUT_DIR"/Krill-"${VERSION}"-*-adhoc.zip.sha256
  "$OUT_DIR/Krill-${VERSION}-source.tar.gz"
  "$OUT_DIR/homebrew-krill/SHA256SUMS"
  "$OUT_DIR/latest.json"
  "$OUT_DIR/install.sh"
  ${WINDOWS_ASSETS[@]+"${WINDOWS_ASSETS[@]}"}
)

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "==> [3/3] Dry-run complete. Artifacts prepared in $OUT_DIR:"
  printf '  - %s\n' "${ASSETS[@]}" "$OUT_DIR/homebrew-krill/Formula/krill.rb" "$OUT_DIR/homebrew-krill/Casks/krill.rb"
  echo "To publish to GitHub ($REPO), run without --dry-run."
  exit 0
fi

# 빌드하는 사이 다른 쪽이 같은 버전을 게시했을 수 있다. 기존 릴리스에 올리는 경로는 없다(--clobber 금지).
HARD_FAILS=0
check_unpublished
if git fetch --quiet origin; then
  FINAL_MAIN_SHA=$(git rev-parse -q --verify refs/remotes/origin/main || true)
  [[ "$HEAD_SHA" == "$FINAL_MAIN_SHA" ]] || hard_fail "origin/main changed while building; refusing to tag a stale candidate"
else
  hard_fail "cannot recheck origin/main before tagging"
fi
if [[ "$(git rev-parse HEAD)" != "$HEAD_SHA" ]] || ! git diff --quiet HEAD; then
  hard_fail "source changed while building; refusing to tag an unvalidated tree"
fi
[[ "$HARD_FAILS" -eq 0 ]] || { echo "publish aborted before tagging" >&2; exit 1; }

UPDATE_PUBKEY=$(python3 packaging/macos/ed25519_manifest.py keygen --key-file "$UPDATE_KEY_FILE")
PLAN_ARGS=(plan --directory "$OUT_DIR" --plan "$OUT_DIR/publication.json" --head "$HEAD_SHA"
  --repo "$REPO" --version "$VERSION" --key-file "$UPDATE_KEY_FILE")
if [[ -n "${KRILL_RELEASE_ASSET_DIR:-}" ]]; then PLAN_ARGS+=(--all-platforms); fi
python3 packaging/release_verify.py "${PLAN_ARGS[@]}"
AUDIT_DIR="${KRILL_PUBLICATION_AUDIT_DIR:-/Volumes/krill-build/nightly/publications/$HEAD_SHA}"
mkdir -p "$AUDIT_DIR"
cp "$OUT_DIR/publication.json" "$AUDIT_DIR/publication.json"

echo "==> [3/3] Tagging $TAG and publishing GitHub Release to $REPO..."
if ! git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  git tag -a "$TAG" -m "Krill $TAG"
fi
git push origin "refs/tags/${TAG}"
CREATE_STATUS=0
gh release create "$TAG" "${ASSETS[@]}" \
  --repo "$REPO" \
  --title "Krill $TAG" \
  --generate-notes \
  --verify-tag || CREATE_STATUS=$?

# 응답이 유실됐어도 쓰기를 재시도하지 않는다. 원격의 완전한 파일·서명 검증만 성공으로 인정한다.
python3 packaging/release_verify.py verify --plan "$OUT_DIR/publication.json" --head "$HEAD_SHA" \
  --repo "$REPO" --public-key "$UPDATE_PUBKEY" --output "$AUDIT_DIR/public-verification.json"
if [[ "$CREATE_STATUS" -ne 0 ]]; then echo "Create response failed; complete remote publication verified."; fi

echo "Published Krill $TAG to ${REPO_URL}/releases/tag/${TAG}"
