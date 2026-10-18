#!/bin/sh
# Krill Linux 원격 호스트 설치 스크립트. krill·krill-holder·krilld를 ~/.local/bin에 둔다(GUI 없음).
# Mac의 Krill 앱이 SSH로 이 머신에 붙으면 krilld 소켓(~/.local/share/dev.krill.app/run/krilld.sock)을 찾고,
# 데몬이 꺼져 있으면 ~/.local/bin/krill로 깨운다.
#
#   공개 저장소:   curl -fsSL https://github.com/craftowen/krill/releases/latest/download/install-linux.sh | sh
#   비공개 저장소: gh release download -R craftowen/krill -p install-linux.sh -O - | sh
#   로컬 파일:     sh install-linux.sh --tarball krill-VERSION-linux-ARCH.tar.gz
#
# 옵션: --release TAG(기본 latest) | --tarball PATH | --prefix DIR(기본 ~/.local) | --repo OWNER/REPO
set -eu

PREFIX="${HOME}/.local"
REPO="craftowen/krill"
TAG="latest"
TARBALL=""
while [ $# -gt 0 ]; do
    case "$1" in
        --release) TAG="${2:?}"; shift 2 ;;
        --tarball) TARBALL="${2:?}"; shift 2 ;;
        --prefix) PREFIX="${2:?}"; shift 2 ;;
        --repo) REPO="${2:?}"; shift 2 ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

[ "$(uname -s)" = Linux ] || { echo "Linux only (macOS는 install.sh)" >&2; exit 2; }
case "$(uname -m)" in
    x86_64|amd64) ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    *) echo "unsupported arch: $(uname -m)" >&2; exit 2 ;;
esac

WORK="$(mktemp -d "${TMPDIR:-/tmp}/krill-install.XXXXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [ -z "$TARBALL" ]; then
    pattern="krill-*-linux-${ARCH}.tar.gz"
    if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
        if [ "$TAG" = latest ]; then set -- ; else set -- "$TAG"; fi
        gh release download "$@" -R "$REPO" -D "$WORK" -p "$pattern" -p "$pattern.sha256"
    else
        base="https://github.com/${REPO}/releases"
        if [ "$TAG" = latest ]; then
            TAG=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$base/latest" | sed 's|.*/tag/||')
        fi
        version="${TAG#v}"
        name="krill-${version}-linux-${ARCH}.tar.gz"
        curl -fsSL -o "$WORK/$name" "$base/download/${TAG}/$name"
        curl -fsSL -o "$WORK/$name.sha256" "$base/download/${TAG}/$name.sha256"
    fi
    TARBALL=$(ls "$WORK"/krill-*-linux-"${ARCH}".tar.gz)
    (cd "$WORK" && sha256sum -c "$(basename "$TARBALL").sha256" >/dev/null) || {
        echo "SHA-256 mismatch" >&2; exit 1
    }
fi

tar -xzf "$TARBALL" -C "$WORK"
SRC=$(find "$WORK" -mindepth 1 -maxdepth 1 -type d -name 'krill-*-linux-*' | head -n 1)
[ -n "$SRC" ] || { echo "archive has no krill-*-linux-* directory" >&2; exit 1; }
mkdir -p "$PREFIX/bin"
for bin in krill krill-holder krilld; do
    [ -x "$SRC/$bin" ] || { echo "missing $bin in archive" >&2; exit 1; }
    # 같은 디렉터리에 먼저 복사한 뒤 rename: 돌고 있는 holder·krilld는 옛 파일을 계속 쓴다(세션 유지).
    cp "$SRC/$bin" "$PREFIX/bin/.$bin.new"
    chmod 755 "$PREFIX/bin/.$bin.new"
    mv -f "$PREFIX/bin/.$bin.new" "$PREFIX/bin/$bin"
done
echo "installed $(basename "$SRC") -> $PREFIX/bin"
# 돌고 있는 데몬이 있으면 세션을 유지한 채 새 바이너리로 교체한다(같은 버전이면 그대로 둔다).
if [ -S "${KRILL_STATE_DIR:-$HOME/.local/share/dev.krill.app}/run/krilld.sock" ]; then
    "$PREFIX/bin/krill" update || echo "warning: running daemon was not upgraded; sessions keep running on the old version" >&2
fi
# holder·krilld는 데몬을 처음 깨운 ssh 세션의 systemd scope 안에서 돈다. logind가 로그아웃 때 사용자 프로세스를
# 죽이도록 설정돼 있으면 세션이 함께 죽는다(Ubuntu·Debian 기본값은 no).
if grep -qs '^[[:space:]]*KillUserProcesses[[:space:]]*=[[:space:]]*yes' /etc/systemd/logind.conf /etc/systemd/logind.conf.d/*.conf; then
    echo "warning: systemd-logind KillUserProcesses=yes kills Krill sessions when the SSH login that started the daemon ends." >&2
    echo "         set KillUserProcesses=no (or KillExcludeUsers=$(id -un)) in /etc/systemd/logind.conf and restart systemd-logind." >&2
fi
case ":${PATH}:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: $PREFIX/bin is not on PATH for this shell (the Mac app finds it anyway)" ;;
esac
