#!/bin/bash
# Linux 원격 호스트용 바이너리(krill·krill-holder·krilld)를 이 Linux 머신에서 빌드해 tar.gz로 묶는다.
# GUI 앱은 없다(원격 호스트는 Mac 앱이 SSH로 붙는 대상). 산출물:
#   OUT_DIR/krill-VERSION-linux-ARCH.tar.gz (+ .sha256)  ARCH = x86_64 | aarch64
# glibc는 빌드 머신 버전 이상을 요구하므로 오래된 배포판(Ubuntu 22.04, glibc 2.35)에서 빌드한다.
set -euo pipefail
cd "$(dirname "$0")/../.."

[[ $# -ge 1 && $# -le 2 ]] || { echo 'usage: bash packaging/linux/build.sh VERSION [OUT_DIR]' >&2; exit 2; }
version=$1
out=${2:-packaging/dist}
[[ "$(uname -s)" == Linux ]] || { echo 'Linux only' >&2; exit 2; }
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) echo "unsupported arch: $arch" >&2; exit 2 ;; esac
source_version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml)
[[ "$source_version" == "$version" ]] || { echo "source version $source_version != $version" >&2; exit 1; }

cargo build --release --locked -p krill -p krill-holder -p krilld --bins
name="krill-${version}-linux-${arch}"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
for bin in krill krill-holder krilld; do
  install -m 755 "target/release/$bin" "$stage/$name/$bin"
  strip "$stage/$name/$bin"
done
mkdir -p "$out"
out=$(cd "$out" && pwd -P)
tar -C "$stage" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "$out/$name.tar.gz"
