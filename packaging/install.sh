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

# -----------------------------------------------------------------------------
# Trench Clack Pipeline UI (docs/STYLEGUIDE.md 토큰 매핑, Bash 3.2 호환)
# - stderr가 실제 터미널([[ -t 2 && $TERM != dumb ]])일 때만 트리/스피너 렌더링
# - 비-TTY 파이프·캡처 테스트 환경에서는 기존 stdout/stderr 계약을 그대로 보존
# -----------------------------------------------------------------------------
UI=0
if [[ -t 2 && "${TERM:-dumb}" != "dumb" ]]; then
    UI=1
fi
COLOR=0
if [[ "${UI}" -eq 1 && -z "${NO_COLOR:-}" ]]; then
    COLOR=1
fi
ANIM=0
if [[ "${UI}" -eq 1 && "${KRILL_INSTALL_ANIMATE:-1}" != "0" ]]; then
    ANIM=1
fi

C_TRENCH_BG=""
C_CORAL=""
C_FG=""
C_PRESERVE=""
C_SUCCESS=""
C_WARN=""
C_FAIL=""
C_MUTED=""
C_RESET=""

if [[ "${COLOR}" -eq 1 ]]; then
    case "${KRILL_COLOR_MODE:-truecolor}" in
        16|ansi)
            C_CORAL=$'\033[1;31m'
            C_FG=$'\033[1;37m'
            C_PRESERVE=$'\033[36m'
            C_SUCCESS=$'\033[32m'
            C_WARN=$'\033[33m'
            C_FAIL=$'\033[31m'
            C_MUTED=$'\033[90m'
            C_RESET=$'\033[0m'
            ;;
        *)
            # STYLEGUIDE.md: Trench #061620, Coral #ff5238, FG #dfedf2, Preserve #7dd3fc,
            # Success #86efac, Warning #eab308, Destructive #ff6568, Muted #9aa3b5
            C_TRENCH_BG=$'\033[48;2;6;22;32m'
            C_CORAL=$'\033[1;38;2;255;82;56m'
            C_FG=$'\033[38;2;223;237;242m'
            C_PRESERVE=$'\033[38;2;125;211;252m'
            C_SUCCESS=$'\033[38;2;134;239;172m'
            C_WARN=$'\033[38;2;234;179;8m'
            C_FAIL=$'\033[38;2;255;101;104m'
            C_MUTED=$'\033[38;2;154;163;181m'
            C_RESET=$'\033[0m'
            ;;
    esac
fi

SPIN_PID=""
STEP_START_CS=0
INSTALL_START_CS=0

now_cs() {
    if command -v perl >/dev/null 2>&1; then
        perl -MTime::HiRes=time -e 'printf "%d\n", int(time() * 10)' 2>/dev/null || echo "$((SECONDS * 10))"
    else
        echo "$((SECONDS * 10))"
    fi
}

fmt_elapsed_cs() {
    local diff=$(( $1 - $2 ))
    if [[ "${diff}" -lt 0 ]]; then
        diff=0
    fi
    printf '%d.%ds' "$((diff / 10))" "$((diff % 10))"
}

stop_spin() {
    if [[ -n "${SPIN_PID:-}" ]]; then
        kill "${SPIN_PID}" 2>/dev/null || true
        wait "${SPIN_PID}" 2>/dev/null || true
        SPIN_PID=""
    fi
    if [[ "${UI}" -eq 1 ]]; then
        printf '\r\033[2K\033[?25h' >&2
    fi
}

WORK="$(mktemp -d "${TMPDIR:-/tmp}/krill-install.XXXXXXXX")"
cleanup() {
    local rc=$?
    trap - EXIT
    stop_spin
    rm -rf "${WORK}"
    exit "${rc}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

ui_intro() {
    [[ "${UI}" -eq 1 ]] || return 0
    INSTALL_START_CS="$(now_cs)"
    local mode_desc="$1"
    printf '%s%s ┌  Krill%s  /  install                         %s\n' \
        "${C_TRENCH_BG}" "${C_CORAL}" "${C_FG}" "${C_RESET}" >&2
    printf '%s │  %s%s\n' "${C_MUTED}" "${mode_desc}" "${C_RESET}" >&2
    printf '%s │%s\n' "${C_MUTED}" "${C_RESET}" >&2
}

ui_step_start() {
    local step_id="$1"
    local total="$2"
    local label="$3"
    local watch_glob="${4:-}"
    STEP_START_CS="$(now_cs)"
    [[ "${UI}" -eq 1 ]] || return 0
    stop_spin
    if [[ "${ANIM}" -eq 0 ]]; then
        printf ' %s◜  [%s/%s 작업]%s %s%s%s\n' \
            "${C_WARN}" "${step_id}" "${total}" "${C_RESET}" "${C_FG}" "${label}" "${C_RESET}" >&2
        return 0
    fi
    printf '\033[?25l' >&2
    (
        trap - EXIT
        trap 'exit 0' INT TERM HUP
        local arcs=('◜' '◠' '◝' '◞' '◡' '◟')
        local tick=0
        local arc elapsed extra=""
        while :; do
            arc="${arcs[$((tick % 6))]}"
            elapsed="$((tick / 10)).$((tick % 10))s"
            if [[ -n "${watch_glob}" && $((tick % 4)) -eq 0 ]]; then
                local bytes=0 f sz
                for f in "${WORK}"/${watch_glob}; do
                    if [[ -f "${f}" ]]; then
                        sz="$(/usr/bin/stat -f%z "${f}" 2>/dev/null || echo 0)"
                        bytes=$((bytes + sz))
                    fi
                done
                if [[ "${bytes}" -gt 102400 ]]; then
                    local mib_whole=$((bytes / 1048576))
                    local mib_frac=$(( (bytes % 1048576) * 10 / 1048576 ))
                    extra=" (${mib_whole}.${mib_frac} MiB)"
                fi
            fi
            printf '\r\033[2K %s%s  [%s/%s 작업]%s %s%s%s%s  %s%s%s' \
                "${C_WARN}" "${arc}" "${step_id}" "${total}" "${C_RESET}" \
                "${C_FG}" "${label}" "${extra}" "${C_RESET}" \
                "${C_MUTED}" "${elapsed}" "${C_RESET}" >&2
            tick=$((tick + 1))
            sleep 0.1
        done
    ) </dev/null &
    SPIN_PID=$!
}

ui_step_done() {
    local step_id="$1"
    local total="$2"
    local badge="$3"
    local label="$4"
    stop_spin
    [[ "${UI}" -eq 1 ]] || return 0
    local end_cs
    end_cs="$(now_cs)"
    local elapsed
    elapsed="$(fmt_elapsed_cs "${end_cs}" "${STEP_START_CS}")"
    printf ' %s◇  [%s/%s %s]%s %s%s%s  %s%s%s\n' \
        "${C_SUCCESS}" "${step_id}" "${total}" "${badge}" "${C_RESET}" \
        "${C_FG}" "${label}" "${C_RESET}" \
        "${C_MUTED}" "${elapsed}" "${C_RESET}" >&2
}

ui_step_fail() {
    local step_id="$1"
    local total="$2"
    local label="$3"
    local detail="${4:-}"
    stop_spin
    [[ "${UI}" -eq 1 ]] || return 0
    printf ' %s×  [%s/%s 실패] %s%s\n' \
        "${C_FAIL}" "${step_id}" "${total}" "${label}" "${C_RESET}" >&2
    if [[ -n "${detail}" ]]; then
        printf '%s │  %s%s\n' "${C_MUTED}" "${detail}" "${C_RESET}" >&2
    fi
    printf ' %s└  설치를 중단했습니다%s\n' "${C_FAIL}" "${C_RESET}" >&2
}

if [[ "${MODE}" == "release" ]]; then
    ARCH="$(uname -m)"
    case "${ARCH}" in
        arm64) ;;
        x86_64) echo "Intel Macs are not supported" >&2; exit 2 ;;
        *) echo "unsupported architecture: ${ARCH}" >&2; exit 2 ;;
    esac

    ui_intro "macOS · ${ARCH} · release: ${VALUE}"

    ZIP_PATTERN="Krill-*-${ARCH}-adhoc.zip"
    SHA_PATTERN="Krill-*-${ARCH}-adhoc.zip.sha256"
    DOWNLOADED=0
    ZIP_URL=""
    ZIP_NAME=""
    GH_BIN=""

    # [01/06] 릴리스 다운로드 경로 확인
    ui_step_start "01" "06" "릴리스 확인 중"
    if command -v curl >/dev/null && [[ "${VALUE}" == "latest" ]]; then
        MANIFEST_URL="https://github.com/${REPO}/releases/latest/download/latest.json"
        if curl -fsSL --connect-timeout 15 "${MANIFEST_URL}" -o "${WORK}/latest.json" 2>/dev/null; then
            ZIP_URL="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["url"])' "${WORK}/latest.json" 2>/dev/null || true)"
            if [[ -n "${ZIP_URL}" ]]; then
                ZIP_NAME="$(basename "${ZIP_URL}")"
            fi
        fi
    fi

    if [[ -n "${ZIP_URL}" ]]; then
        ui_step_done "01" "06" "확인" "릴리스 다운로드 경로 확인 (${ZIP_NAME})"
    else
        for candidate in "$(command -v gh 2>/dev/null || true)" /opt/homebrew/bin/gh /usr/local/bin/gh; do
            if [[ -n "${candidate}" && -x "${candidate}" ]]; then
                GH_BIN="${candidate}"
                break
            fi
        done
        if [[ -z "${GH_BIN}" ]]; then
            ui_step_fail "01" "06" "릴리스 다운로드 경로를 확인하지 못했습니다" "gh CLI가 없습니다 (brew install gh && gh auth login)"
            echo "error: release download failed and 'gh' CLI was not found (install via 'brew install gh && gh auth login')." >&2
            exit 1
        fi
        ui_step_done "01" "06" "확인" "릴리스 다운로드 경로 확인 (gh CLI · ${VALUE})"
    fi

    # [02/06] 번들 다운로드
    ui_step_start "02" "06" "번들 다운로드 중" "Krill-*"
    if [[ -n "${ZIP_URL}" ]]; then
        if curl -fsSL --connect-timeout 15 "${ZIP_URL}" -o "${WORK}/${ZIP_NAME}" 2>/dev/null && \
           curl -fsSL --connect-timeout 15 "${ZIP_URL}.sha256" -o "${WORK}/${ZIP_NAME}.sha256" 2>/dev/null; then
            DOWNLOADED=1
        fi
    fi

    if [[ "${DOWNLOADED}" -eq 0 ]]; then
        if [[ -z "${GH_BIN}" ]]; then
            for candidate in "$(command -v gh 2>/dev/null || true)" /opt/homebrew/bin/gh /usr/local/bin/gh; do
                if [[ -n "${candidate}" && -x "${candidate}" ]]; then
                    GH_BIN="${candidate}"
                    break
                fi
            done
        fi
        if [[ -z "${GH_BIN}" ]]; then
            ui_step_fail "02" "06" "번들 다운로드에 실패했습니다" "gh CLI를 찾을 수 없습니다"
            echo "error: release download failed and 'gh' CLI was not found (install via 'brew install gh && gh auth login')." >&2
            exit 1
        fi
        if [[ "${VALUE}" != "latest" ]]; then
            if ! "${GH_BIN}" release download "${VALUE}" \
                --repo "${REPO}" \
                --pattern "${ZIP_PATTERN}" \
                --pattern "${SHA_PATTERN}" \
                --dir "${WORK}" \
                --clobber; then
                ui_step_fail "02" "06" "gh release download 실패 (${VALUE})"
                exit 1
            fi
        else
            if ! "${GH_BIN}" release download \
                --repo "${REPO}" \
                --pattern "${ZIP_PATTERN}" \
                --pattern "${SHA_PATTERN}" \
                --dir "${WORK}" \
                --clobber; then
                ui_step_fail "02" "06" "gh release download 실패 (latest)"
                exit 1
            fi
        fi
    fi

    ZIP_FILE="$(echo "${WORK}"/Krill-*-"${ARCH}"-adhoc.zip)"
    if [[ ! -f "${ZIP_FILE}" ]]; then
        ui_step_fail "02" "06" "내려받은 번들 파일을 찾을 수 없습니다"
        echo "downloaded zip not found in ${WORK}" >&2
        exit 1
    fi
    ZIP_BYTES="$(/usr/bin/stat -f%z "${ZIP_FILE}" 2>/dev/null || echo 0)"
    ZIP_MIB="$((ZIP_BYTES / 1048576)).$(( (ZIP_BYTES % 1048576) * 10 / 1048576 )) MiB"
    ui_step_done "02" "06" "수신" "번들 다운로드 완료 (${ZIP_MIB})"

    # [03/06] SHA-256 비교
    ui_step_start "03" "06" "SHA-256 비교 중"
    if ! (cd "${WORK}" && shasum -a 256 -c ./*.sha256 >/dev/null); then
        ui_step_fail "03" "06" "SHA-256 체크섬이 일치하지 않습니다"
        exit 1
    fi
    ui_step_done "03" "06" "일치" "SHA-256 체크섬 일치"

    # [04/06] 번들 해제·서명 검사
    ui_step_start "04" "06" "번들 해제·서명 검사 중"
    mkdir -p "${WORK}/unpacked"
    if ! /usr/bin/ditto -x -k "${ZIP_FILE}" "${WORK}/unpacked"; then
        ui_step_fail "04" "06" "번들 압축 해제(ditto)에 실패했습니다"
        exit 1
    fi
    APP_SRC="${WORK}/unpacked/Krill.app"
    if [[ ! -d "${APP_SRC}" ]]; then
        ui_step_fail "04" "06" "아카이브 내부에 Krill.app이 없습니다"
        echo "Krill.app not found in archive" >&2
        exit 1
    fi
    /usr/bin/xattr -dr com.apple.quarantine "${APP_SRC}" 2>/dev/null || true
    if ! /usr/bin/codesign --verify --strict "${APP_SRC}"; then
        ui_step_fail "04" "06" "원본 번들 코드서명 검증에 실패했습니다"
        exit 1
    fi
    ui_step_done "04" "06" "검증" "번들 해제 · 원본 서명 검사 통과"

    # [05/06] 앱 파일 설치·검사
    ui_step_start "05" "06" "앱 파일 설치·검사 중"
    APP_DST="${APP_DIR}/Krill.app"
    # Apple Silicon XNU cs_blob 충돌(SIGKILL -9)을 막기 위해 디렉터리 inode는 보존하고 파일은 .tmp + os.replace로 원자 교체한다.
    if ! python3 -c '
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
' "${APP_SRC}" "${APP_DST}"; then
        ui_step_fail "05" "06" "${APP_DST} 파일 교체에 실패했습니다"
        exit 1
    fi

    /usr/bin/xattr -dr com.apple.quarantine "${APP_DST}" 2>/dev/null || true
    if ! /usr/bin/codesign --verify --strict "${APP_DST}"; then
        ui_step_fail "05" "06" "설치된 ${APP_DST} 코드서명 검증에 실패했습니다"
        exit 1
    fi
    ui_step_done "05" "06" "설치" "${APP_DST} 설치 · 서명 확인"

    # [06/06] CLI 연결·앱 열기 요청
    ui_step_start "06" "06" "CLI 연결·앱 열기 요청 중"
    BIN="${PREFIX}/bin"
    mkdir -p "${BIN}"
    ln -sfn "${APP_DST}/Contents/MacOS/krill" "${BIN}/krill"

    VERSION="$(/usr/bin/plutil -extract CFBundleShortVersionString raw -o - "${APP_DST}/Contents/Info.plist" 2>/dev/null || echo "latest")"

    OPEN_STATUS="앱 열기 생략(--no-open)"
    if [[ "${NO_OPEN}" -eq 0 ]]; then
        if open "${APP_DST}"; then
            OPEN_STATUS="앱 열기 요청 전달"
        else
            ui_step_fail "06" "06" "CLI는 연결했으나 앱 열기 요청에 실패했습니다"
            exit 1
        fi
    fi
    ui_step_done "06" "06" "완료" "CLI 연결 · ${OPEN_STATUS}"

    if [[ "${UI}" -eq 1 ]]; then
        TOTAL_ELAPSED="$(fmt_elapsed_cs "$(now_cs)" "${INSTALL_START_CS}")"
        printf '%s │%s\n' "${C_MUTED}" "${C_RESET}" >&2
        printf ' %s└  설치했습니다%s  %s/  %s%s\n' \
            "${C_SUCCESS}" "${C_RESET}" "${C_MUTED}" "${TOTAL_ELAPSED}" "${C_RESET}" >&2
        printf '    %sKrill v%s  → %s%s\n' "${C_FG}" "${VERSION}" "${APP_DST}" "${C_RESET}" >&2
        printf '    %sCLI            → %s/krill%s\n' "${C_MUTED}" "${BIN}" "${C_RESET}" >&2
        printf '    %s시작           → open "%s"%s\n' "${C_MUTED}" "${APP_DST}" "${C_RESET}" >&2
    fi

    echo "Installed Krill v${VERSION} to ${APP_DST} (CLI: ${BIN}/krill)"
    exit 0
fi

command -v cargo >/dev/null || { echo "cargo not found (need 1.98+)" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 2; }

ui_intro "macOS · arm64 · source (${MODE}: ${VALUE})"

SRC=""
if [[ "${MODE}" == "dir" ]]; then
    ui_step_start "01" "04" "소스 체크아웃 확인 중"
    if [[ ! -f "${VALUE}/Cargo.toml" || ! -d "${VALUE}/crates/krill" ]]; then
        ui_step_fail "01" "04" "유효한 Krill 소스 체크아웃이 아닙니다" "${VALUE}"
        echo "not a krill checkout: ${VALUE}" >&2
        exit 2
    fi
    SRC="${VALUE}"
    ui_step_done "01" "04" "확인" "로컬 소스 체크아웃 확인"
else
    command -v curl >/dev/null || { echo "curl not found" >&2; exit 2; }
    case "${MODE}" in
        tag) REF="${VALUE}" ;;
        commit) REF="${VALUE}" ;;
    esac
    TARBALL="${WORK}/source.tar.gz"
    ui_step_start "01" "04" "소스 아카이브 다운로드 중 (${REF})"
    if ! curl -fsSL --connect-timeout 15 "https://codeload.github.com/${REPO}/tar.gz/${REF}" -o "${TARBALL}"; then
        ui_step_fail "01" "04" "소스 아카이브 다운로드에 실패했습니다" "${REF}"
        exit 1
    fi
    TAR_BYTES="$(wc -c < "${TARBALL}" | tr -d ' ')"
    TAR_SHA="$(shasum -a 256 "${TARBALL}" | cut -d' ' -f1)"
    ui_step_done "01" "04" "수신" "소스 아카이브 다운로드 완료 (${TAR_BYTES} bytes)"
    echo "downloaded ${TARBALL} (${TAR_BYTES} bytes)"
    echo "sha256: ${TAR_SHA}"
    if [[ -t 0 ]]; then
        read -r -p "continue with build? [y/N] " answer
        [[ "${answer}" == "y" || "${answer}" == "Y" ]] || exit 1
    fi
    mkdir -p "${WORK}/src"
    tar -xzf "${TARBALL}" -C "${WORK}/src" --strip-components=1
    SRC="${WORK}/src"
fi

ui_step_start "02" "04" "워크트리 상태 확인 중"
SRC="$(cd "${SRC}" && pwd)"
if [[ -e "${SRC}/.git" ]]; then
    if ! CHECKOUT_STATUS="$(git -C "${SRC}" status --porcelain)"; then
        ui_step_fail "02" "04" "git status 확인에 실패했습니다"
        exit 2
    fi
    if [[ -n "${CHECKOUT_STATUS}" ]]; then
        ui_step_fail "02" "04" "커밋되지 않은 변경 사항이 있어 빌드를 거부합니다"
        echo "dirty checkout, refusing" >&2
        exit 2
    fi
fi
ui_step_done "02" "04" "검사" "클린 소스 상태 확인"

# Run from the checkout so its Cargo config and relative target-dir apply.
# Compiler artifacts report the actual paths, including external CARGO_TARGET_DIR.
ui_step_start "03" "04" "릴리스 바이너리 3종 빌드 중 (krill · krill-holder · krilld)"
if [[ "${UI}" -eq 1 ]]; then
    if ! (cd "${SRC}" && perl -e 'alarm shift; exec @ARGV' 300 cargo build --release --locked \
        --manifest-path "${SRC}/Cargo.toml" -p krill -p krill-holder -p krilld \
        --message-format=json) > "${WORK}/artifacts.jsonl" 2> "${WORK}/cargo.stderr"; then
        ui_step_fail "03" "04" "Cargo 릴리스 빌드에 실패했습니다"
        cat "${WORK}/cargo.stderr" >&2 || true
        exit 1
    fi
    ui_step_done "03" "04" "빌드" "릴리스 바이너리 3종 컴파일 완료"
    if [[ -s "${WORK}/cargo.stderr" ]]; then
        cat "${WORK}/cargo.stderr" >&2
    fi
else
    (cd "${SRC}" && perl -e 'alarm shift; exec @ARGV' 300 cargo build --release --locked \
        --manifest-path "${SRC}/Cargo.toml" -p krill -p krill-holder -p krilld \
        --message-format=json) > "${WORK}/artifacts.jsonl"
fi

ui_step_start "04" "04" "바이너리 검증 및 설치 중"
BIN="${PREFIX}/bin"
if ! python3 - "${WORK}/artifacts.jsonl" "${BIN}" <<'PY'
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
then
    ui_step_fail "04" "04" "바이너리 검증 또는 설치에 실패했습니다"
    exit 1
fi
ui_step_done "04" "04" "완료" "CLI · krill-holder · krilld 설치 완료"

if [[ "${UI}" -eq 1 ]]; then
    TOTAL_ELAPSED="$(fmt_elapsed_cs "$(now_cs)" "${INSTALL_START_CS}")"
    printf '%s │%s\n' "${C_MUTED}" "${C_RESET}" >&2
    printf ' %s└  설치했습니다 (CLI-only)%s  %s/  %s%s\n' \
        "${C_SUCCESS}" "${C_RESET}" "${C_MUTED}" "${TOTAL_ELAPSED}" "${C_RESET}" >&2
    printf '    %s경로  → %s/{krill,krill-holder,krilld}%s\n' "${C_FG}" "${BIN}" "${C_RESET}" >&2
    printf '    %sPATH  → export PATH="%s:$PATH"%s\n' "${C_MUTED}" "${BIN}" "${C_RESET}" >&2
fi

echo "done. export PATH=\"${BIN}:\$PATH\""
echo "CLI-only installation. For the GUI app, follow packaging/README.md (bundle.sh)."
