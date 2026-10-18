#!/bin/bash
# Linux tar.gz 설치 스모크 테스트: 임시 HOME에 install-linux.sh로 설치하고 데몬 기동·세션 입출력·krilld kill -9 생존을 본다.
# 이 머신의 실제 Krill 상태(~/.local/share/dev.krill.app)는 건드리지 않는다.
set -euo pipefail
cd "$(dirname "$0")/../.."
[[ $# -eq 1 ]] || { echo 'usage: bash packaging/linux/smoke.sh krill-VERSION-linux-ARCH.tar.gz' >&2; exit 2; }
tarball=$(realpath "$1")
home=$(mktemp -d /tmp/krill-smoke.XXXXXX)
krill="$home/.local/bin/krill"
cleanup() {
  for pid in $(pgrep -f "$home/.local/bin/krill" || true); do kill -9 "$pid" 2>/dev/null || true; done
  rm -rf "$home"
}
trap cleanup EXIT
export HOME="$home"
unset KRILL_STATE_DIR

sh packaging/install-linux.sh --tarball "$tarball"
cd "$home"
"$krill" ls >/dev/null
[[ -S "$home/.local/share/dev.krill.app/run/krilld.sock" ]] || { echo 'FAIL: no krilld socket' >&2; exit 1; }
id=$("$krill" new --cwd "$home")
"$krill" send "$id" 'echo smoke-$((6*7))'
"$krill" send "$id" --enter
"$krill" wait "$id" --for pattern --pattern 'smoke-42' --timeout 10 >/dev/null
old=$(pgrep -f "^$home/.local/bin/krilld " | head -n 1)
kill -9 "$old"
for _ in $(seq 50); do
  new=$(pgrep -f "^$home/.local/bin/krilld " | head -n 1 || true)
  [[ -n "$new" && "$new" != "$old" ]] && break
  sleep 0.1
done
"$krill" send "$id" 'echo after-kill-$((6*7))'
"$krill" send "$id" --enter
"$krill" wait "$id" --for pattern --pattern 'after-kill-42' --timeout 10 >/dev/null
echo "smoke ok: $(basename "$tarball") ($(uname -m), glibc $(ldd --version | head -n 1 | awk '{print $NF}'))"
