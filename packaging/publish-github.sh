#!/bin/bash
# 로컬 맥에서 Krill.app·ZIP·DMG·Homebrew tap을 조립하고 GitHub Releases에 발행한다.
# 기본은 실제 업로드 전 점검만 하려면 --dry-run을 사용한다.
set -euo pipefail
cd "$(dirname "$0")/.."

usage() {
  cat >&2 <<'USAGE'
usage: bash packaging/publish-github.sh [VERSION] [--bump patch|minor|major] [--repo OWNER/REPO] [--bin-dir DIR] [--dry-run]

Options:
  VERSION            릴리스 버전 (생략 시 Cargo.toml의 [workspace.package].version 사용)
  --bump LEVEL       Cargo.toml 버전을 patch|minor|major로 올리고 Cargo.lock과 함께 커밋한 뒤 발행
  --repo OWNER/REPO  대상 GitHub 저장소 (생략 시 git remote origin에서 자동 추출)
  --bin-dir DIR      이미 빌드된 target/release 디렉터리를 사용해 빌드 생략
  --dry-run          로컬 번들·DMG·Homebrew tap·latest.json 조립까지만 수행하고 git push / gh release 생략
USAGE
  exit 2
}

VERSION=""
BUMP=""
REPO=""
BIN_DIR=""
DRY_RUN=0

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
    --dry-run)
      DRY_RUN=1
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
  git commit -m "Release v${VERSION}" Cargo.toml Cargo.lock
  CARGO_VER="$VERSION"
fi

VERSION=${VERSION:-$CARGO_VER}
VERSION=${VERSION#v}
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] || {
  echo "invalid semantic version: $VERSION" >&2
  exit 2
}
[[ "$VERSION" == "$CARGO_VER" ]] || {
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

rm -rf "$OUT_DIR"

BUNDLE_ARGS=(--output "$OUT_DIR" --repo "$REPO")
if [[ -n "$BIN_DIR" ]]; then
  BUNDLE_ARGS+=(--bin-dir "$BIN_DIR")
fi

echo "==> [1/3] Building Krill.app bundle & ZIP..."
bash packaging/bundle.sh "${BUNDLE_ARGS[@]}"

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
KC_PATH="${KRILL_CODESIGN_KEYCHAIN:-$HOME/Library/Keychains/minikrill-codesign.keychain-db}"
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

ASSETS=(
  "$OUT_DIR/Krill-${VERSION}.dmg"
  "$OUT_DIR"/Krill-"${VERSION}"-*-adhoc.zip
  "$OUT_DIR"/Krill-"${VERSION}"-*-adhoc.zip.sha256
  "$OUT_DIR/Krill-${VERSION}-source.tar.gz"
  "$OUT_DIR/homebrew-krill/SHA256SUMS"
  "$OUT_DIR/latest.json"
  "$OUT_DIR/install.sh"
)

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "==> [3/3] Dry-run complete. Artifacts prepared in $OUT_DIR:"
  printf '  - %s\n' "${ASSETS[@]}" "$OUT_DIR/homebrew-krill/Formula/krill.rb" "$OUT_DIR/homebrew-krill/Casks/krill.rb"
  echo "To publish to GitHub ($REPO), run without --dry-run."
  exit 0
fi

echo "==> [3/3] Tagging $TAG and publishing GitHub Release to $REPO..."
if ! git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  git tag -a "$TAG" -m "Krill $TAG"
fi
git push origin HEAD "refs/tags/${TAG}"

if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  gh release upload "$TAG" "${ASSETS[@]}" --repo "$REPO" --clobber
else
  gh release create "$TAG" "${ASSETS[@]}" \
    --repo "$REPO" \
    --title "Krill $TAG" \
    --generate-notes
fi

echo "Published Krill $TAG to ${REPO_URL}/releases/tag/${TAG}"
