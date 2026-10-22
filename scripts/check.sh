#!/bin/zsh
# 완료 게이트. 하나라도 실패하면 0이 아닌 코드로 끝난다. 작업 완료 주장 전 반드시 통과.
set -euo pipefail
cd "${0:A:h}/.."
if [[ ${1-} == -h || ${1-} == --help ]]; then
  print "사용: scripts/check.sh [--memory]
  fmt+deny+NOTICES+public-surface(병렬) · clippy · nextest(fast+latency) · doctest
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
# 병렬 1단계: 읽기 전용 검사 4종을 묶고, 하나라도 실패하면 전체 검사를 중단한다.
# clippy·nextest·doctest는 target/ 락을 공유하므로 순서를 유지한다.
step "fmt+deny+notices+public-surface (parallel)";
partlog=$(mktemp -d "${TMPDIR:-/tmp}/mk-check-part-XXXXXX")
typeset -A partpids
part(){ local name=$1; shift; "$@" > $partlog/$name.log 2>&1 & partpids[$!]=$name; }
part fmt cargo fmt --all --check
part deny cargo deny --locked check --hide-inclusion-graph
part notices python3 docs/licensing/generate_notices.py --check
part public-surface bash scripts/public-surface.sh
partfail=0
for pid name in ${(kv)partpids}; do
  if ! wait $pid; then print -P "%B!! $name 실패%b"; sed 's/^/  /' $partlog/$name.log; partfail=1; fi
done
rm -rf $partlog
[[ $partfail -eq 0 ]] || exit 1
step "clippy";  run cargo clippy --workspace --all-targets --locked -- -D warnings
# 테스트 실행 슬롯(2026-10-15): 워커 20개가 동시에 전체 테스트를 돌리면 load 150~200이 되어 지연 측정 테스트가
# 결정적으로 실패했다. 빌드는 각자 하고, 실행만 머신 전체에서 KRILL_TEST_SLOTS개까지 동시에 돈다.
# 기본값은 결정 11-①(2026-10-15)대로 CPU 코어 수 기반: 코어/4(12코어에서 예전 고정값 3과 같다).
cores=$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || print 4)
slot(){ local n=${KRILL_TEST_SLOTS:-$(( cores / 4 > 0 ? cores / 4 : 1 ))} k rc; while true; do for k in $(seq 1 $n); do
  rc=0; run lockf -s -t 0 /tmp/mk-test-slot-$k "$@" || rc=$?; [[ $rc -ne 75 ]] && return $rc; done; sleep 3; done; }
# 테스트 분리: 부하에 민감한 저지연 테스트를 나머지 테스트와 별도 실행한다.
# 아래 필터는 .config/nextest.toml
# latency override와 같은 집합이어야 한다(어긋나면 저지연 테스트가 고속 페이즈에서 부하를 맞아 flake 가능).
step "test build";   run cargo nextest run --workspace --locked --no-tests=pass --no-run
step "test fast";    slot cargo nextest run --workspace --locked --no-tests=pass --no-fail-fast -E 'not (binary(survive) | binary(reconnect) | binary(wait) | binary(attach) | binary(holder) | test(survive) | test(/_ms$/) | test(within_budget) | test(krilld_kill) | test(exec_handoff))'
step "test latency"; slot cargo nextest run --workspace --locked --no-tests=pass --no-fail-fast -E 'binary(survive) | binary(reconnect) | binary(wait) | binary(attach) | binary(holder) | test(survive) | test(/_ms$/) | test(within_budget) | test(krilld_kill) | test(exec_handoff)'
step "doctest"; run cargo test --workspace --doc --locked -q
# --memory: MK-066 메모리 게이트까지(nightly·선택). 병렬 부하에서 흔들려 기본 경로에는 넣지 않는다.
if [[ ${1-} == --memory ]]; then step "memory"; run scripts/memory-gate.sh; fi
print -P "%B== check.sh: 전부 통과%b"
