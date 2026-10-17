#!/bin/zsh
# 완료 게이트. 하나라도 실패하면 0이 아닌 코드로 끝난다. 작업 완료 주장 전 반드시 통과.
set -euo pipefail
cd "${0:A:h}/.."
step(){ print -P "%B== $1%b"; }
step "fmt";     cargo fmt --all --check
step "clippy";  cargo clippy --workspace --all-targets --locked -- -D warnings
# 테스트 실행 슬롯(2026-10-15): 워커 20개가 동시에 전체 테스트를 돌리면 load 150~200이 되어 지연 측정 테스트가
# 결정적으로 실패했다. 빌드는 각자 하고, 실행만 머신 전체에서 KRILL_TEST_SLOTS개(기본 3)까지 동시에 돈다.
slot(){ local n=${KRILL_TEST_SLOTS:-3} k rc; while true; do for k in $(seq 1 $n); do
  rc=0; lockf -s -t 0 /tmp/mk-test-slot-$k "$@" || rc=$?; [[ $rc -ne 75 ]] && return $rc; done; sleep 3; done; }
step "test";    cargo nextest run --workspace --locked --no-tests=pass --no-run
slot cargo nextest run --workspace --locked --no-tests=pass
step "doctest"; cargo test --workspace --doc --locked -q
step "deny";    cargo deny --locked check --hide-inclusion-graph
# --memory: MK-066 메모리 게이트까지(nightly). 병렬 부하에서 흔들려 기본 경로에는 넣지 않는다.
if [[ ${1-} == --memory ]]; then step "memory"; scripts/memory-gate.sh; fi
print -P "%B== check.sh: 전부 통과%b"
