#!/bin/zsh
# 완료 게이트. 하나라도 실패하면 0이 아닌 코드로 끝난다. 작업 완료 주장 전 반드시 통과.
set -euo pipefail
cd "${0:A:h}/.."
if [[ ${1-} == -h || ${1-} == --help ]]; then
  print "사용: scripts/check.sh [--memory]
  fmt · clippy · nextest · doctest · deny · NOTICES 최신 여부 · 공개 표면 검사(scripts/public-surface.sh)
  --memory          끝에 scripts/memory-gate.sh(MK-066)까지 돈다
  KRILL_TEST_SLOTS  머신 전체 동시 테스트 실행 수(기본: CPU 코어 수 / 4, 최소 1)"
  exit 0
fi
# 종료·타임아웃(perl alarm → SIGALRM)·Ctrl-C 때 이 스크립트가 띄운 cargo·테스트 프로세스 트리를 정리한다(고아 cargo가
# 공유 빌드 락을 쥔 채 남는 문제). `kill 0`은 호출자 셸까지 죽일 수 있어 자기 자식 트리만 찾아 끈다.
# zsh는 포그라운드 자식이 끝날 때까지 신호 trap을 미루므로 각 단계는 run(백그라운드 + wait)으로 돌린다.
kill_tree(){ local c; for c in $(pgrep -P $1); do kill_tree $c; done; kill -TERM $1 2>/dev/null || true; }
cleanup(){ local c; for c in $(pgrep -P $$); do kill_tree $c; done; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 142' ALRM
trap 'cleanup; exit 143' TERM HUP
run(){ "$@" & wait $!; }
step(){ print -P "%B== $1%b"; }
step "fmt";     run cargo fmt --all --check
step "clippy";  run cargo clippy --workspace --all-targets --locked -- -D warnings
# 테스트 실행 슬롯(2026-10-15): 워커 20개가 동시에 전체 테스트를 돌리면 load 150~200이 되어 지연 측정 테스트가
# 결정적으로 실패했다. 빌드는 각자 하고, 실행만 머신 전체에서 KRILL_TEST_SLOTS개까지 동시에 돈다.
# 기본값은 결정 11-①(2026-10-15)대로 CPU 코어 수 기반: 코어/4(12코어에서 예전 고정값 3과 같다).
cores=$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || print 4)
slot(){ local n=${KRILL_TEST_SLOTS:-$(( cores / 4 > 0 ? cores / 4 : 1 ))} k rc; while true; do for k in $(seq 1 $n); do
  rc=0; run lockf -s -t 0 /tmp/mk-test-slot-$k "$@" || rc=$?; [[ $rc -ne 75 ]] && return $rc; done; sleep 3; done; }
step "test";    run cargo nextest run --workspace --locked --no-tests=pass --no-run
slot cargo nextest run --workspace --locked --no-tests=pass
step "doctest"; run cargo test --workspace --doc --locked -q
step "deny";    run cargo deny --locked check --hide-inclusion-graph
step "notices"; run python3 docs/licensing/generate_notices.py --check
# 공개 저장소에 나가면 안 되는 경로·내용 검사. 파일이 없으면 건너뛰지 않고 실패한다.
step "public-surface"; run bash scripts/public-surface.sh
# --memory: MK-066 메모리 게이트까지(nightly·선택). 병렬 부하에서 흔들려 기본 경로에는 넣지 않는다.
if [[ ${1-} == --memory ]]; then step "memory"; run scripts/memory-gate.sh; fi
print -P "%B== check.sh: 전부 통과%b"
