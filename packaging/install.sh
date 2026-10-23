#!/bin/bash
# Krill 원클릭 설치 스크립트 (OWE-591 / MK-154~156).
#
# 1) 기본 모드(인자 없음 또는 --release [TAG]):
#    GitHub Releases에서 서명된 Krill.app 번들(ZIP + .sha256)을 내려받아 SHA-256·codesign 검증 후
#    /Applications/Krill.app 제자리 설치 + Gatekeeper 격리 속성 정리 + ~/.local/bin/krill 연결 + 앱 실행까지 수행한다.
#    - 비공개 저장소: gh release download -R craftowen/krill -p install.sh -O - | bash
#    - 공개 저장소:   curl -fsSL https://github.com/craftowen/krill/releases/latest/download/install.sh | bash
#
# 2) Source mode installs CLI + daemon/holder only (no GUI app):
#    bash packaging/install.sh --source-dir <krill 체크아웃>
#    bash packaging/install.sh --tag main [--prefix ~/.local]
#    bash packaging/install.sh --commit <sha> [--prefix ~/.local]
set -euo pipefail

PREFIX="${HOME}/.local"
APP_DIR="/Applications"
MODE="release"
VALUE="latest"
REPO="craftowen/krill"
NO_OPEN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            MODE="release"
            if [[ $# -ge 2 && -n "${2:-}" && "${2}" != --* ]]; then
                VALUE="$2"
                shift 2
            else
                VALUE="latest"
                shift 1
            fi
            ;;
        --from-source)
            if [[ "${MODE}" == "release" ]]; then
                MODE="tag"
                VALUE="main"
            fi
            shift 1
            ;;
        --source-dir) MODE="dir"; VALUE="${2:?}"; shift 2 ;;
        --tag) MODE="tag"; VALUE="${2:?}"; shift 2 ;;
        --commit) MODE="commit"; VALUE="${2:?}"; shift 2 ;;
        --prefix) PREFIX="${2:?}"; shift 2 ;;
        --app-dir) APP_DIR="${2:?}"; shift 2 ;;
        --repo) REPO="${2:?}"; shift 2 ;;
        --no-open) NO_OPEN=1; shift 1 ;;
        -h|--help) sed -n '1,14p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[[ "$(uname -s)" == "Darwin" ]] || { echo "macOS only" >&2; exit 2; }
[[ "$(uname -m)" == "arm64" ]] || { echo "Krill supports Apple Silicon Macs only" >&2; exit 2; }
command -v shasum >/dev/null || { echo "shasum not found" >&2; exit 2; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/krill-install.XXXXXXXX")"
cleanup() { rm -rf "${WORK}"; }
trap cleanup EXIT

if [[ "${MODE}" == "release" ]]; then
    ARCH="$(uname -m)"
    case "${ARCH}" in
        arm64) ;;
        x86_64) echo "Intel Macs are not supported" >&2; exit 2 ;;
        *) echo "unsupported architecture: ${ARCH}" >&2; exit 2 ;;
    esac

    ZIP_PATTERN="Krill-*-${ARCH}-adhoc.zip"
    SHA_PATTERN="Krill-*-${ARCH}-adhoc.zip.sha256"
    DOWNLOADED=0

    # 1차: 공개 저장소일 때 curl로 latest.json 조회 후 다운로드 시도
    if command -v curl >/dev/null && [[ "${VALUE}" == "latest" ]]; then
        MANIFEST_URL="https://github.com/${REPO}/releases/latest/download/latest.json"
        if curl -fsSL "${MANIFEST_URL}" -o "${WORK}/latest.json" 2>/dev/null; then
            ZIP_URL="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["url"])' "${WORK}/latest.json" 2>/dev/null || true)"
            if [[ -n "${ZIP_URL}" ]]; then
                ZIP_NAME="$(basename "${ZIP_URL}")"
                if curl -fsSL "${ZIP_URL}" -o "${WORK}/${ZIP_NAME}" 2>/dev/null && \
                   curl -fsSL "${ZIP_URL}.sha256" -o "${WORK}/${ZIP_NAME}.sha256" 2>/dev/null; then
                    DOWNLOADED=1
                fi
            fi
        fi
    fi

    # 2차: 비공개 저장소이거나 태그 지정 시 gh CLI로 다운로드
    if [[ "${DOWNLOADED}" -eq 0 ]]; then
        for candidate in "$(command -v gh 2>/dev/null || true)" /opt/homebrew/bin/gh /usr/local/bin/gh; do
            if [[ -n "${candidate}" && -x "${candidate}" ]]; then
                GH_BIN="${candidate}"
                break
            fi
        done
        [[ -n "${GH_BIN:-}" ]] || {
            echo "error: release download failed and 'gh' CLI was not found (install via 'brew install gh && gh auth login')." >&2
            exit 1
        }
        if [[ "${VALUE}" != "latest" ]]; then
            "${GH_BIN}" release download "${VALUE}" \
                --repo "${REPO}" \
                --pattern "${ZIP_PATTERN}" \
                --pattern "${SHA_PATTERN}" \
                --dir "${WORK}" \
                --clobber
        else
            "${GH_BIN}" release download \
                --repo "${REPO}" \
                --pattern "${ZIP_PATTERN}" \
                --pattern "${SHA_PATTERN}" \
                --dir "${WORK}" \
                --clobber
        fi
    fi

    (cd "${WORK}" && shasum -a 256 -c ./*.sha256 >/dev/null)
    ZIP_FILE="$(echo "${WORK}"/Krill-*-"${ARCH}"-adhoc.zip)"
    [[ -f "${ZIP_FILE}" ]] || { echo "downloaded zip not found in ${WORK}" >&2; exit 1; }

    mkdir -p "${WORK}/unpacked"
    /usr/bin/ditto -x -k "${ZIP_FILE}" "${WORK}/unpacked"
    APP_SRC="${WORK}/unpacked/Krill.app"
    [[ -d "${APP_SRC}" ]] || { echo "Krill.app not found in archive" >&2; exit 1; }

    /usr/bin/xattr -dr com.apple.quarantine "${APP_SRC}" 2>/dev/null || true
    /usr/bin/codesign --verify --strict "${APP_SRC}"

    APP_DST="${APP_DIR}/Krill.app"
    # Apple Silicon XNU cs_blob 충돌(SIGKILL -9)을 막기 위해 디렉터리 inode는 보존하고 파일은 .tmp + os.replace로 원자 교체한다.
    python3 -c '
import os, pathlib, shutil, sys
src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
dst.mkdir(parents=True, exist_ok=True)
for root, dirs, files in os.walk(src):
    rel = pathlib.Path(root).relative_to(src)
    target_dir = dst / rel
    target_dir.mkdir(parents=True, exist_ok=True)
    for d in dirs:
        (target_dir / d).mkdir(exist_ok=True)
    for f in files:
        s = pathlib.Path(root) / f
        t = target_dir / f
        tmp = target_dir / f".{f}.tmp.{os.getpid()}"
        if tmp.exists() or tmp.is_symlink():
            tmp.unlink()
        if s.is_symlink():
            os.symlink(os.readlink(s), tmp)
        else:
            shutil.copy2(s, tmp)
        os.replace(tmp, t)
for root, dirs, files in os.walk(dst, topdown=False):
    rel = pathlib.Path(root).relative_to(dst)
    src_dir = src / rel
    for f in files:
        if not (src_dir / f).exists():
            (pathlib.Path(root) / f).unlink()
    for d in dirs:
        if not (src_dir / d).exists():
            (pathlib.Path(root) / d).rmdir()
' "${APP_SRC}" "${APP_DST}"

    /usr/bin/xattr -dr com.apple.quarantine "${APP_DST}" 2>/dev/null || true
    /usr/bin/codesign --verify --strict "${APP_DST}"

    BIN="${PREFIX}/bin"
    mkdir -p "${BIN}"
    ln -sfn "${APP_DST}/Contents/MacOS/krill" "${BIN}/krill"

    VERSION="$(/usr/bin/plutil -extract CFBundleShortVersionString raw -o - "${APP_DST}/Contents/Info.plist" 2>/dev/null || echo "latest")"
    echo "Installed Krill v${VERSION} to ${APP_DST} (CLI: ${BIN}/krill)"

    if [[ "${NO_OPEN}" -eq 0 ]]; then
        open "${APP_DST}"
    fi
    exit 0
fi

command -v cargo >/dev/null || { echo "cargo not found (need 1.98+)" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 2; }

SRC=""
if [[ "${MODE}" == "dir" ]]; then
    [[ -f "${VALUE}/Cargo.toml" && -d "${VALUE}/crates/krill" ]] || { echo "not a krill checkout: ${VALUE}" >&2; exit 2; }
    SRC="${VALUE}"
else
    command -v curl >/dev/null || { echo "curl not found" >&2; exit 2; }
    case "${MODE}" in
        tag) REF="${VALUE}" ;;
        commit) REF="${VALUE}" ;;
    esac
    TARBALL="${WORK}/source.tar.gz"
    curl -fsSL "https://codeload.github.com/${REPO}/tar.gz/${REF}" -o "${TARBALL}"
    echo "downloaded ${TARBALL} ($(wc -c < "${TARBALL}") bytes)"
    echo "sha256: $(shasum -a 256 "${TARBALL}" | cut -d' ' -f1)"
    if [[ -t 0 ]]; then
        read -r -p "continue with build? [y/N] " answer
        [[ "${answer}" == "y" || "${answer}" == "Y" ]] || exit 1
    fi
    mkdir -p "${WORK}/src"
    tar -xzf "${TARBALL}" -C "${WORK}/src" --strip-components=1
    SRC="${WORK}/src"
fi

SRC="$(cd "${SRC}" && pwd)"
if [[ -e "${SRC}/.git" ]]; then
    CHECKOUT_STATUS="$(git -C "${SRC}" status --porcelain)"
    if [[ -n "${CHECKOUT_STATUS}" ]]; then
        echo "dirty checkout, refusing" >&2
        exit 2
    fi
fi

# Run from the checkout so its Cargo config and relative target-dir apply.
# Compiler artifacts report the actual paths, including external CARGO_TARGET_DIR.
(cd "${SRC}" && perl -e 'alarm shift; exec @ARGV' 300 cargo build --release --locked \
    --manifest-path "${SRC}/Cargo.toml" -p krill -p krill-holder -p krilld \
    --message-format=json) > "${WORK}/artifacts.jsonl"

BIN="${PREFIX}/bin"
python3 - "${WORK}/artifacts.jsonl" "${BIN}" <<'PY'
import json
import os
from pathlib import Path
import shutil
import sys

names = ("krill", "krill-holder", "krilld")
artifacts = {}
with open(sys.argv[1]) as stream:
    for line in stream:
        item = json.loads(line)
        name = item.get("target", {}).get("name")
        if item.get("reason") == "compiler-artifact" and name in names and item.get("executable"):
            artifacts[name] = Path(item["executable"])
destination = Path(sys.argv[2])
# Validate every input and destination before installing any file.
for name in names:
    source = artifacts.get(name)
    if source is None or not source.is_file() or not os.access(source, os.X_OK):
        sys.exit(f"cargo did not report an executable for {name}")
    target = destination / name
    if target.exists() or target.is_symlink():
        sys.exit(f"refusing to overwrite {target}")
destination.mkdir(parents=True, exist_ok=True)
for name in names:
    target = destination / name
    with artifacts[name].open("rb") as source, target.open("xb") as output:
        shutil.copyfileobj(source, output)
    target.chmod(0o755)
    print(f"installed {target}")
PY

echo "done. export PATH=\"${BIN}:\$PATH\""
echo "CLI-only installation. For the GUI app, follow packaging/README.md (bundle.sh)."
