#!/bin/bash
# 맥에서 실행: 상시 Windows 빌드 VM(정지 상태로 보관)을 켜서 현재 커밋을 빌드·묶기·스모크 테스트하고,
# zip·.sha256을 OUT_DIR로 가져온 뒤 VM을 끈다(성공·실패 모두, 삭제하지 않는다). 비밀키는 VM에 보내지 않는다.
#
#   KRILL_WIN_VM_DIR=<dir> bash packaging/windows/build-vm.sh VERSION PUBLIC_KEY UPDATE_URL [OUT_DIR]
#
# KRILL_WIN_VM_DIR(필수): up.sh·ssh.sh·down.sh와 ip·id_ed25519·known_hosts가 있는 VM 접속 폴더(저장소 밖).
# KRILL_WIN_REF(기본 HEAD): 빌드할 git 리비전.
set -euo pipefail
cd "$(dirname "$0")/../.."

[[ $# -ge 3 && $# -le 4 ]] || { echo 'usage: KRILL_WIN_VM_DIR=... bash packaging/windows/build-vm.sh VERSION PUBLIC_KEY UPDATE_URL [OUT_DIR]' >&2; exit 2; }
version=$1 pubkey=$2 update_url=$3
out=${4:-packaging/dist}
vm=${KRILL_WIN_VM_DIR:?set KRILL_WIN_VM_DIR}
ref=${KRILL_WIN_REF:-HEAD}
[[ "$pubkey" =~ ^[0-9a-f]{64}$ ]] || { echo 'PUBLIC_KEY must be 64 lowercase hex' >&2; exit 2; }
[[ "$ref" != HEAD || -z "$(git status --porcelain --untracked-files=no)" ]] || echo 'warning: uncommitted changes are NOT built (git archive HEAD)' >&2
mkdir -p "$out"
out=$(cd "$out" && pwd -P)
zip="Krill-$version-windows-x64.zip"
log="$out/build-windows-x64.log"
src=$(mktemp "${TMPDIR:-/tmp}/krill-win-src.XXXXXX")
git archive --format=tar.gz -o "$src" "$ref"
trap 'rm -f "$src"; "$vm/down.sh" >>"$log" 2>&1 || echo "warning: VM stop failed; run $vm/down.sh" >&2' EXIT

# VM 쪽 C:\krill-release: src는 매번 새로, target(증분 캐시)은 유지한다. VM의 sftp 서브시스템이 실패해 scp는 -O(레거시)로 보낸다.
scp_opts=(-O -i "$vm/id_ed25519" -o IdentitiesOnly=yes -o HostKeyAlias=krill-win -o UserKnownHostsFile="$vm/known_hosts" -o ConnectTimeout=15)
: >"$log"
echo "==> Windows build on VM (log: $log)"
"$vm/up.sh" >>"$log" 2>&1
host="krill@$(cat "$vm/ip")"
"$vm/ssh.sh" 'New-Item -ItemType Directory -Force C:\krill-release | Out-Null; if (Test-Path C:\krill-release\src) { cmd /c rd /s /q C:\krill-release\src }' >>"$log" 2>&1
scp "${scp_opts[@]}" "$src" "$host:C:/krill-release/src.tar.gz" >>"$log" 2>&1
"$vm/ssh.sh" "\$ErrorActionPreference='Stop'
  New-Item -ItemType Directory C:\krill-release\src | Out-Null
  tar.exe -xzf C:\krill-release\src.tar.gz -C C:\krill-release\src; if (\$LASTEXITCODE) { exit 1 }
  Set-Location C:\krill-release\src
  \$env:CARGO_TARGET_DIR = 'C:\krill-release\target'
  & .\packaging\windows\build.ps1 -Version '$version' -PublicKey '$pubkey' -UpdateUrl '$update_url'
  & .\packaging\windows\smoke.ps1 -Zip 'packaging\dist\\$zip'" >>"$log" 2>&1 || { echo "FAILED windows-x64 (see $log)" >&2; exit 1; }
scp "${scp_opts[@]}" "$host:C:/krill-release/src/packaging/dist/$zip" "$host:C:/krill-release/src/packaging/dist/$zip.sha256" "$out/" >>"$log" 2>&1
grep -a 'smoke ok:' "$log" | tr -d '\r' || true
echo "$out/$zip"
