#!/bin/bash
# 맥에서 실행: GCP에 임시 Linux VM(x86_64·aarch64)을 만들어 현재 커밋을 빌드·스모크 테스트하고,
# tar.gz를 OUT_DIR로 가져온 뒤 VM을 지운다(성공·실패 모두). 비용이 드는 작업이라 프로젝트는 명시해야 한다.
#
#   KRILL_GCP_PROJECT=<project> bash packaging/linux/build-gcp.sh VERSION [OUT_DIR]
#
# 환경변수: KRILL_GCP_PROJECT(필수), KRILL_GCP_ZONE(기본 us-central1-a, T2A Arm 제공 영역),
#           KRILL_LINUX_ARCHES(기본 "x86_64 aarch64"), KRILL_RUST_TOOLCHAIN(기본 1.98.1)
# 빌드 VM은 Ubuntu 22.04(glibc 2.35)라 그 이상의 배포판에서 돈다.
set -euo pipefail
cd "$(dirname "$0")/../.."

[[ $# -ge 1 && $# -le 2 ]] || { echo 'usage: KRILL_GCP_PROJECT=... bash packaging/linux/build-gcp.sh VERSION [OUT_DIR]' >&2; exit 2; }
version=$1
out=${2:-packaging/dist}
project=${KRILL_GCP_PROJECT:?set KRILL_GCP_PROJECT}
zone=${KRILL_GCP_ZONE:-us-central1-a}
arches=${KRILL_LINUX_ARCHES:-x86_64 aarch64}
toolchain=${KRILL_RUST_TOOLCHAIN:-1.98.1}
command -v gcloud >/dev/null || { echo 'gcloud not found' >&2; exit 2; }
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || echo 'warning: uncommitted changes are NOT built (git archive HEAD)' >&2
mkdir -p "$out"
out=$(cd "$out" && pwd -P)
src=$(mktemp "${TMPDIR:-/tmp}/krill-src.XXXXXX")
git archive --format=tar.gz -o "$src" HEAD
# 빌드 VM(태그 krill-linux-build)에 이 맥의 현재 공인 IP에서만 SSH를 여는 임시 방화벽 규칙. 끝나면 지운다.
myip=$(curl -fsS --max-time 10 https://api.ipify.org)
rule="krill-linux-build-ssh-$(date +%s)"
gcloud compute firewall-rules create "$rule" --project "$project" --quiet \
  --allow tcp:22 --source-ranges "$myip/32" --target-tags krill-linux-build >/dev/null
trap 'gcloud compute firewall-rules delete "$rule" --project "$project" --quiet >/dev/null 2>&1 || true; rm -f "$src"' EXIT

# 한 아키텍처: VM 생성 → 빌드·스모크 → tar.gz 회수. `||` 문맥이라 set -e가 꺼지므로 단계마다 실패를 직접 돌려준다.
run_build() {
  local arch=$1 vm=$2 machine=$3 image=$4
  local g=(--project "$project" --zone "$zone" --quiet)
  gcloud compute instances create "$vm" "${g[@]}" --machine-type "$machine" \
    --image-family "$image" --image-project ubuntu-os-cloud --boot-disk-size 40GB \
    --labels purpose=krill-linux-build --tags krill-linux-build || return 1
  local ready=0
  for _ in $(seq 30); do
    if gcloud compute ssh "$vm" "${g[@]}" --command true; then ready=1; break; fi
    sleep 10
  done
  [[ $ready -eq 1 ]] || return 1
  gcloud compute scp "${g[@]}" "$src" "$vm:krill-src.tar.gz" || return 1
  gcloud compute ssh "$vm" "${g[@]}" --command "set -euo pipefail
    sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config >/dev/null
    curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain $toolchain >/dev/null
    mkdir -p src && tar -xzf krill-src.tar.gz -C src && cd src
    PATH=\$HOME/.cargo/bin:\$PATH bash packaging/linux/build.sh $version dist
    bash packaging/linux/smoke.sh dist/krill-$version-linux-$arch.tar.gz" || return 1
  gcloud compute scp "${g[@]}" "$vm:src/dist/krill-$version-linux-$arch.tar.gz" \
    "$vm:src/dist/krill-$version-linux-$arch.tar.gz.sha256" "$out/" || return 1
}

# VM 삭제는 트랩에 맡기지 않는다(비동기 함수 서브셸에서 EXIT 트랩이 돌지 않는 경우를 실측). 성공·실패 모두 지운다.
build_one() {
  local arch=$1 machine image vm log rc=0
  case "$arch" in
    x86_64) machine=e2-standard-4; image=ubuntu-2204-lts ;;
    aarch64) machine=t2a-standard-4; image=ubuntu-2204-lts-arm64 ;;
    *) echo "unsupported arch: $arch" >&2; return 2 ;;
  esac
  vm="krill-build-${arch//_/-}-$(date +%s)"
  log="$out/build-linux-$arch.log"
  run_build "$arch" "$vm" "$machine" "$image" >"$log" 2>&1 || rc=$?
  gcloud compute instances delete "$vm" --project "$project" --zone "$zone" --quiet >>"$log" 2>&1 || true
  return "$rc"
}

pids=()
for arch in $arches; do
  build_one "$arch" & pids+=($!)
done
status=0
for pid in "${pids[@]}"; do wait "$pid" || status=1; done
leftover=$(gcloud compute instances list --project "$project" --filter 'labels.purpose=krill-linux-build' --format 'value(name)')
[[ -z "$leftover" ]] || { echo "warning: build VMs still exist: $leftover" >&2; status=1; }
for arch in $arches; do
  f="$out/krill-$version-linux-$arch.tar.gz"
  if [[ -f "$f" ]]; then echo "$f"; else echo "FAILED $arch (see $out/build-linux-$arch.log)" >&2; status=1; fi
done
exit $status
